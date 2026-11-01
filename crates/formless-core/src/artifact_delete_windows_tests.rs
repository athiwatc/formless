//! Native NTFS regression. This is intentionally not a mocked sharing test:
//! attribute-only reparse writes can bypass FILE_SHARE_WRITE restrictions.
use super::*;
use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
use windows_sys::Win32::{
    Storage::FileSystem::*,
    System::{
        Ioctl::FSCTL_SET_REPARSE_POINT, SystemServices::IO_REPARSE_TAG_MOUNT_POINT,
        IO::DeviceIoControl,
    },
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn junction(attribute_handle: &File, destination: &Path) -> io::Result<()> {
    let destination = destination.canonicalize()?;
    let destination = destination.to_str().ok_or(io::ErrorKind::InvalidInput)?;
    let destination = destination
        .strip_prefix(r"\\?\")
        .ok_or(io::ErrorKind::InvalidInput)?;
    let nt = format!(r"\??\{destination}");
    let target: Vec<u16> = std::ffi::OsStr::new(&nt).encode_wide().collect();
    let bytes = u16::try_from(target.len() * 2).map_err(|_| io::ErrorKind::InvalidInput)?;
    // REPARSE_DATA_BUFFER: 8-byte header, four u16 mount-point offsets,
    // substitute-name UTF-16 + NUL, and an empty NUL-terminated print name.
    let mut data = Vec::new();
    data.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    data.extend((8u16 + bytes + 4).to_le_bytes());
    data.extend(0u16.to_le_bytes());
    for field in [0u16, bytes, bytes + 2, 0u16] {
        data.extend(field.to_le_bytes());
    }
    for unit in target {
        data.extend(unit.to_le_bytes());
    }
    data.extend([0u8; 4]);
    let mut returned = 0;
    // SAFETY: live synchronous attribute-only directory handle; contiguous
    // valid mount-point buffer with lengths matching the owned input storage.
    // No output buffer or asynchronous OVERLAPPED is supplied.
    if unsafe {
        DeviceIoControl(
            attribute_handle.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr().cast(),
            data.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[test]
fn attribute_only_retag_after_root_capture_cannot_authorize_outside_deletion() {
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-native-ad-{}", uuid::Uuid::new_v4())));
    let owned = fixture.0.join("artifacts");
    let outside = fixture.0.join("outside");
    std::fs::create_dir_all(&owned).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let victim = outside.join("victim");
    std::fs::write(&victim, b"private native outside sentinel").unwrap();
    let root = Root::open(&owned).unwrap();
    // Root captured its ordinary physical directory before it was retagged.
    let attribute_handle = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&owned)
        .unwrap();
    junction(&attribute_handle, &outside)
        .expect("private NTFS attribute-only junction mutation must really occur");
    assert_eq!(
        std::fs::read(owned.join("victim")).unwrap(),
        b"private native outside sentinel",
        "negative must exercise an actually redirected native path"
    );
    assert!(
        super::directory_entries::entries(&root.directory)
            .unwrap()
            .is_empty(),
        "handle enumeration stays on the original empty directory after its named route retags"
    );
    let absent = root.metadata(&owned.join("missing")).unwrap_err();
    assert_eq!(
        super::rejection(&absent),
        Some(super::Rejection::Missing),
        "only original-parent handle enumeration can prove absence after retagging"
    );
    assert!(root.metadata(&owned.join("victim")).is_err());
    assert!(root.remove(&owned.join("victim")).is_err());
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"private native outside sentinel"
    );
    drop(attribute_handle);
    drop(root);
}

fn registered_alias_survives_reconciliation(short: bool) {
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-native-alias-{}", uuid::Uuid::new_v4())));
    let data = fixture.0.join("data");
    let artifacts = data.join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let long = artifacts.join("VeryLongOwnedArtifactFileName.txt");
    std::fs::write(&long, b"private registered alias").unwrap();
    let alias = if short {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_GENERIC_WRITE | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&long)
            .unwrap();
        let name: Vec<u16> = "ALIAS.TXT"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: owned private regular-file HANDLE and terminated short name.
        assert_ne!(
            unsafe { SetFileShortNameW(file.as_raw_handle(), name.as_ptr()) },
            0,
            "native short-name assignment unavailable: {}",
            io::Error::last_os_error()
        );
        drop(file);
        artifacts.join("ALIAS.TXT")
    } else {
        artifacts.join("VERYLONGOWNEDARTIFACTFILENAME.TXT")
    };
    assert_eq!(
        std::fs::read(&alias).unwrap(),
        b"private registered alias",
        "positive must really resolve an alternate native name"
    );
    let store = crate::Store::open(&data.join("formless.db")).unwrap();
    let id = store
        .save_artifact(None, alias.to_str().unwrap(), "", 24)
        .unwrap();
    assert_eq!(
        crate::artifacts::reconcile(&store, &artifacts, std::time::Duration::ZERO).unwrap(),
        crate::artifacts::Reconciled::default()
    );
    assert_eq!(std::fs::read(&long).unwrap(), b"private registered alias");
    assert_eq!(
        store.get_artifact(&id).unwrap().unwrap().1,
        alias.to_str().unwrap()
    );
    drop(store);
}

#[test]
fn case_alias_registration_protects_the_enumerated_long_file_identity() {
    registered_alias_survives_reconciliation(false);
}

#[test]
#[ignore = "requires local NTFS short-name assignment support; manual native security regression"]
fn short_alias_registration_protects_the_enumerated_long_file_identity() {
    registered_alias_survives_reconciliation(true);
}
