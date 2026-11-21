//! Client-local atomic result publication. A failed Run never calls this helper.
use anyhow::{ensure, Context, Result};
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::fs::{File, Metadata};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
#[path = "pipeline_output_windows.rs"]
mod windows_access;

pub const MAX_RESULT_FILE_BYTES: usize = 8 * 1024 * 1024;

pub struct OutputCommit {
    /// The complete result was committed; only directory durability is unknown.
    pub durability_warning: Option<String>,
}
struct Temporary {
    path: PathBuf,
    file: Option<File>,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}
fn ordinary_destination(path: &Path) -> Result<Option<Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "output destination must be a regular file, not a link or directory"
            );
            ensure!(
                !metadata.permissions().readonly(),
                "output destination is read-only"
            );
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    metadata.file_attributes() & 0x400 == 0,
                    "output destination is a reparse point"
                );
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
#[cfg(not(windows))]
fn create_private(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}
#[cfg(windows)]
fn create_private(path: &Path) -> Result<File> {
    use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
    use windows_sys::Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, WRITE_DAC,
        },
    };
    let security = crate::win_pipe::OwnerOnly::new()?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            security.as_raw().cast(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        std::io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_handle(handle.cast()) })
}

fn preserve_destination_access(file: &File, destination: &Path, metadata: &Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, PermissionsExt},
        };
        let _ = destination;
        if file.metadata()?.gid() != metadata.gid() {
            // SAFETY: the destination group is data and this descriptor is
            // the still-owned private temporary file. Leave its owner alone.
            if unsafe { libc::fchown(file.as_raw_fd(), !0 as libc::uid_t, metadata.gid()) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("preserve output destination group");
            }
        }
        file.set_permissions(std::fs::Permissions::from_mode(metadata.mode() & 0o777))?;
    }
    #[cfg(windows)]
    {
        let _ = metadata;
        windows_access::copy_dacl(destination, file)?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = destination;
        file.set_permissions(metadata.permissions())?;
    }
    Ok(())
}

/// Commit exactly one complete bounded result. This refuses final symlinks and
/// special files and never truncates the old destination before atomic rename.
pub fn write_result_file(path: &Path, bytes: &[u8]) -> Result<OutputCommit> {
    write_with(path, bytes, |file, bytes| Ok(file.write_all(bytes)?))
}
fn write_with(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> Result<()>,
) -> Result<OutputCommit> {
    ensure!(
        bytes.len() <= MAX_RESULT_FILE_BYTES,
        "Run output exceeds the 8 MiB result-file limit"
    );
    let name = path.file_name().context("output path must name a file")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent
        .canonicalize()
        .context("output parent directory does not exist")?;
    ensure!(parent.is_dir(), "output parent must be a directory");
    let destination = parent.join(name);
    ordinary_destination(&destination)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = parent.join(format!(
        ".formless-output-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = create_private(&temporary).context("create private output temporary file")?;
    let mut owned = Temporary {
        path: temporary,
        file: Some(file),
    };
    let file = owned
        .file
        .as_mut()
        .context("output temporary file unavailable")?;
    write(file, bytes).context("write complete Run result")?;
    file.sync_all().context("flush complete Run result")?;
    if let Some(metadata) = ordinary_destination(&destination)? {
        preserve_destination_access(file, &destination, &metadata)
            .context("preserve existing output destination access")?;
        file.sync_all().context("flush Run output permissions")?;
    }
    owned.file.take(); // Windows rename must not retain an open writer.
    ordinary_destination(&destination)?;
    std::fs::rename(&owned.path, &destination)
        .context("atomically replace Run output destination")?;
    #[cfg(unix)]
    let durability_warning = File::open(&parent)
        .and_then(|directory| directory.sync_all())
        .err()
        .map(|_| {
            "Run output was committed completely, but its directory could not be flushed".into()
        });
    #[cfg(not(unix))]
    let durability_warning = None;
    Ok(OutputCommit { durability_warning })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> Fixture {
        let path =
            std::env::temp_dir().join(format!("fl-pipeline-output-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Fixture(path)
    }
    #[test]
    fn output_replaces_only_after_complete_flush_and_partial_write_failure_keeps_old_bytes() {
        let fixture = fixture();
        let path = fixture.0.join("answer.json");
        std::fs::write(&path, b"old output").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(write_with(&path, b"new complete output", |file, bytes| {
            file.write_all(&bytes[..3])?;
            anyhow::bail!("private injected write failure")
        })
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old output");
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
        write_result_file(&path, b"{\"ok\":true}\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"ok\":true}\n");
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn output_updates_shared_access_only_after_writing_and_keeps_new_paths_private() {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, PermissionsExt},
        };
        let fixture = fixture();
        let shared = fixture.0.join("shared");
        let existing = File::create(&shared).unwrap();
        let original_group = existing.metadata().unwrap().gid();
        // Exercise a different group where this fixture can set one.
        let alternate = if unsafe { libc::geteuid() } == 0 {
            if original_group == 65534 {
                65533
            } else {
                65534
            }
        } else {
            original_group
        };
        assert_eq!(
            unsafe { libc::fchown(existing.as_raw_fd(), !0 as libc::uid_t, alternate) },
            0
        );
        existing
            .set_permissions(std::fs::Permissions::from_mode(0o660))
            .unwrap();
        drop(existing);
        write_with(&shared, b"finished", |file, bytes| {
            assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
            Ok(file.write_all(bytes)?)
        })
        .unwrap();
        let metadata = std::fs::metadata(&shared).unwrap();
        assert_eq!(metadata.gid(), alternate);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o660);
        assert_eq!(std::fs::read(shared).unwrap(), b"finished");

        let private = fixture.0.join("new");
        write_result_file(&private, b"private").unwrap();
        assert_eq!(
            std::fs::metadata(private).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn output_limits_and_invalid_parent_leave_old_destination_untouched() {
        let fixture = fixture();
        let path = fixture.0.join("answer");
        std::fs::write(&path, b"keep").unwrap();
        assert!(write_result_file(&path, &vec![0; MAX_RESULT_FILE_BYTES + 1]).is_err());
        assert!(write_result_file(&fixture.0.join("missing/answer"), b"new").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn output_symlink_cannot_replace_or_change_its_target() {
        let fixture = fixture();
        let target = fixture.0.join("target");
        let link = fixture.0.join("link");
        std::fs::write(&target, b"private target").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_result_file(&link, b"new output").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"private target");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 2);
    }
}
