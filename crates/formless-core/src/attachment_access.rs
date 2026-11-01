//! Internal attachment markers are requests, not filesystem capabilities.
//! Only an owned artifact record plus a rooted native file handle permits reads.
use crate::{Paths, Store};
use anyhow::Result;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

type Records = HashMap<String, (String, i64)>;

#[derive(Clone)]
enum Resolver {
    Profile(PathBuf),
    Snapshot(Arc<Records>),
}

#[derive(Clone)]
pub(crate) struct AttachmentAccess {
    root: PathBuf,
    resolver: Resolver,
}

impl AttachmentAccess {
    pub(crate) fn profile(paths: &Paths) -> Self {
        Self {
            root: paths.data_dir.join("artifacts"),
            resolver: Resolver::Profile(paths.db_file()),
        }
    }

    /// Offline forecasts must never open a pager on the original database.
    /// Resolve only referenced IDs using the caller's existing private snapshot.
    pub(crate) fn snapshot(store: &Store, messages: &[Value]) -> Result<Option<Self>> {
        let Some(root) = store.artifact_root.clone() else {
            return Ok(None);
        };
        let mut records = Records::new();
        for message in messages {
            for attachment in message["_attachments"].as_array().into_iter().flatten() {
                let Some(id) = attachment["id"].as_str() else {
                    continue;
                };
                if !records.contains_key(id) {
                    if let Some((_, path, bytes)) = store.get_artifact(id)? {
                        records.insert(id.into(), (path, bytes));
                    }
                }
            }
        }
        Ok(Some(Self {
            root,
            resolver: Resolver::Snapshot(Arc::new(records)),
        }))
    }

    fn record(&self, id: &str) -> Option<(String, i64)> {
        match &self.resolver {
            Resolver::Snapshot(records) => records.get(id).cloned(),
            Resolver::Profile(db) => {
                // No CREATE, migration or optimize. Managed live providers may
                // read the current registry, including a newly steered upload.
                let conn = rusqlite::Connection::open_with_flags(
                    db,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .ok()?;
                conn.query_row(
                    "SELECT path,bytes FROM artifacts WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok()
            }
        }
    }

    pub(crate) fn read(
        &self,
        attachment: &Value,
        max_bytes: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        let denied = || "no verified owned artifact reference".to_string();
        let id = attachment["id"].as_str().ok_or_else(denied)?;
        let claimed = attachment["path"].as_str().ok_or_else(denied)?;
        let (path, recorded_bytes) = self.record(id).ok_or_else(denied)?;
        if claimed != path || recorded_bytes < 0 {
            return Err(denied());
        }
        if attachment
            .get("bytes")
            .is_some_and(|v| !v.is_null() && v.as_u64() != Some(recorded_bytes as u64))
        {
            return Err("attachment size does not match its owned record".into());
        }
        if recorded_bytes as u64 > max_bytes as u64 {
            return Err("the stored attachment exceeds the source size limit".into());
        }
        let root = std::path::absolute(&self.root).map_err(|_| denied())?;
        let path = std::path::absolute(Path::new(&path)).map_err(|_| denied())?;
        let relative = path.strip_prefix(&root).map_err(|_| {
            "the attachment is outside this profile's artifact directory".to_string()
        })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(denied());
        }
        let root_info = std::fs::symlink_metadata(&root).map_err(|_| denied())?;
        if !root_info.is_dir() || root_info.file_type().is_symlink() {
            return Err("the artifact directory is not an owned regular directory".into());
        }
        let file = open_rooted(&root, relative, &root_info).map_err(|_| {
            "the attachment is missing, nonregular, or uses a symbolic link".to_string()
        })?;
        let metadata = file.metadata().map_err(|_| denied())?;
        if !metadata.is_file() || metadata.len() != recorded_bytes as u64 {
            return Err("the stored attachment changed or is not a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "the stored attachment could not be read".to_string())?;
        if bytes.len() != recorded_bytes as usize || bytes.len() > max_bytes {
            return Err("the stored attachment changed while reading".into());
        }
        if let Some(hash) = attachment.get("sha256").filter(|v| !v.is_null()) {
            use sha2::Digest;
            let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
            if !hash
                .as_str()
                .is_some_and(|expected| expected.eq_ignore_ascii_case(&actual))
            {
                return Err("the attachment hash does not match its recorded metadata".into());
            }
        }
        Ok(bytes)
    }
}

#[cfg(unix)]
fn open_rooted(
    root: &Path,
    relative: &Path,
    expected: &std::fs::Metadata,
) -> std::io::Result<File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };
    fn open(dir: Option<&File>, name: &std::ffi::OsStr, directory: bool) -> std::io::Result<File> {
        let name = CString::new(name.as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: NUL-terminated path, valid borrowed parent descriptor, and
        // native ABI flags. Every successful owned descriptor closes via File.
        let fd = unsafe {
            libc::openat(
                dir.map_or(libc::AT_FDCWD, |d| d.as_raw_fd()),
                name.as_ptr(),
                flags,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    let mut parent = open(None, root.as_os_str(), true)?;
    let opened = parent.metadata()?;
    if (opened.dev(), opened.ino()) != (expected.dev(), expected.ino()) {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    let mut components = relative.components().peekable();
    while let Some(Component::Normal(name)) = components.next() {
        let file = open(Some(&parent), name, components.peek().is_some())?;
        if components.peek().is_none() {
            return Ok(file);
        }
        parent = file;
    }
    Err(std::io::ErrorKind::InvalidInput.into())
}

#[cfg(windows)]
fn open_rooted(root: &Path, relative: &Path, _: &std::fs::Metadata) -> std::io::Result<File> {
    use std::{
        ffi::OsString,
        os::windows::{
            ffi::OsStringExt,
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    fn directory(path: &Path) -> std::io::Result<File> {
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if !dir.metadata()?.is_dir()
            || dir.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        Ok(dir)
    }
    fn identity(file: &File) -> std::io::Result<(u32, u32, u32)> {
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: live borrowed HANDLE and valid output storage. Read only
        // fields initialized by the successful native metadata operation.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let info = unsafe { info.assume_init() };
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }
    fn final_path(file: &File) -> std::io::Result<PathBuf> {
        let mut buffer = vec![0u16; 32768];
        // SAFETY: live owned file HANDLE and writable, correctly sized UTF-16
        // buffer. Inspect the opened target before any content is read.
        let len = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        } as usize;
        if len == 0 || len >= buffer.len() {
            return Err(std::io::Error::last_os_error());
        }
        Ok(PathBuf::from(OsString::from_wide(&buffer[..len])))
    }
    let owned_root = directory(root)?;
    let root_identity = identity(&owned_root)?;
    let root_final = final_path(&owned_root)?;
    let mut named = root_final.clone();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(std::io::ErrorKind::InvalidInput.into());
        };
        named.push(name);
        if std::fs::symlink_metadata(&named)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&named)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    let opened = final_path(&file)?;
    if !opened.starts_with(&root_final) || identity(&directory(root)?)? != root_identity {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_rooted(_: &Path, _: &Path, _: &std::fs::Metadata) -> std::io::Result<File> {
    Err(std::io::ErrorKind::Unsupported.into())
}
