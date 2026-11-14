//! Ordinary CLI export files only. No core review/security helper is changed.
//! Native Windows ACL, sharing and failure-recovery acceptance is still required.
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};
use windows_sys::Win32::{
    Foundation::{
        LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_UNABLE_TO_MOVE_REPLACEMENT,
        ERROR_UNABLE_TO_MOVE_REPLACEMENT_2, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
        },
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation, TokenUser,
        DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, MoveFileExW, ReplaceFileW, CREATE_NEW,
        FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, MOVEFILE_WRITE_THROUGH,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: these allocations come from the documented LocalAlloc Win32
        // results below and are released once after all borrowed pointers end.
        unsafe { LocalFree(self.0) };
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "export path contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

fn private_descriptor() -> io::Result<LocalAllocation> {
    use std::os::windows::io::{AsRawHandle, OwnedHandle};
    let mut token = ptr::null_mut();
    // SAFETY: valid pseudo process handle; token is an initialized out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a new owned real handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0;
    // SAFETY: the zero-length query writes only the required buffer size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed == 0 || needed > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid token-user size",
        ));
    }
    // usize storage supplies the alignment required by TOKEN_USER and SID.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    // SAFETY: aligned buffer holds at least needed bytes and remains live until
    // the SID has been converted into its separately allocated string.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful TokenUser query initializes this aligned structure.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = ptr::null_mut();
    // SAFETY: SID points inside the live token buffer; sid is an out-pointer.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid_allocation = LocalAllocation(sid.cast());
    let mut len = 0;
    // SAFETY: this API guarantees a valid NUL-terminated allocated UTF-16 SID.
    unsafe {
        while *sid.add(len) != 0 {
            len += 1;
        }
    }
    // SAFETY: len was measured within the API's NUL-terminated allocation.
    let sid_text = String::from_utf16(unsafe { std::slice::from_raw_parts(sid, len) })
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid user SID"))?;
    drop(sid_allocation);
    // Protected DACL, current process user only. No parent ACL is changed.
    let sddl: Vec<_> = format!("D:P(A;;FA;;;{sid_text})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: input string is terminated; API allocates the returned descriptor.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalAllocation(descriptor))
}

fn descriptor_acl(descriptor: &LocalAllocation) -> io::Result<(Vec<u8>, bool)> {
    let mut present = 0;
    let mut acl = ptr::null_mut();
    let mut defaulted = 0;
    // SAFETY: initialized live Win32 descriptor and valid out-pointers.
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || acl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "export staging has no restrictive DACL",
        ));
    }
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: same live descriptor; API initializes the control/revision outputs.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the validated descriptor API returned a live ACL of AclSize bytes.
    let bytes =
        unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize) }.to_vec();
    Ok((bytes, control & SE_DACL_PROTECTED != 0))
}

fn verify_private(file: &File, expected: &LocalAllocation) -> io::Result<()> {
    let mut pointer = ptr::null_mut();
    // SAFETY: valid owned file/directory handle and initialized descriptor output.
    // This only reads security information; no parent permission is changed.
    let error = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut pointer,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let actual = LocalAllocation(pointer);
    let (acl, protected) = descriptor_acl(&actual)?;
    let (expected_acl, expected_protected) = descriptor_acl(expected)?;
    if !protected || !expected_protected || acl != expected_acl {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "filesystem did not preserve private export staging ACL",
        ));
    }
    Ok(())
}

struct Stage {
    directory: PathBuf,
    retain: bool,
}
impl Drop for Stage {
    fn drop(&mut self) {
        if !self.retain {
            let _ = std::fs::remove_file(self.directory.join("replacement"));
            // Never recursively remove this directory: a failed ReplaceFileW
            // can leave the only old copy at `previous`, which must survive.
            let _ = std::fs::remove_dir(&self.directory);
        }
    }
}

pub(super) fn write(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(destination, |file| file.write_all(bytes))
}

pub(super) fn write_with(
    destination: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = destination
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "export has no filename"))?;
    let parent = std::fs::canonicalize(parent)?;
    let destination = parent.join(name);
    let existing = match std::fs::metadata(&destination) {
        Ok(meta) if meta.is_file() => {
            // Preserve ordinary write-access failures without truncation.
            drop(OpenOptions::new().write(true).open(&destination)?);
            true
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "export destination is not a regular file",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let descriptor = private_descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut reserved = None;
    for _ in 0..64 {
        let directory = parent.join(format!(
            ".formless-export-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let directory_wide = wide(&directory)?;
        // SAFETY: live terminated path and descriptor, no inherited handle.
        if unsafe { CreateDirectoryW(directory_wide.as_ptr(), &attributes) } != 0 {
            reserved = Some(Stage {
                directory,
                retain: false,
            });
            break;
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error().map(|n| n as u32),
            Some(ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS)
        ) {
            continue;
        }
        return Err(error);
    }
    let mut stage = reserved.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot reserve private export staging",
        )
    })?;
    let directory_handle = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&stage.directory)?;
    verify_private(&directory_handle, &descriptor)?;
    drop(directory_handle);
    let replacement = stage.directory.join("replacement");
    let previous = stage.directory.join("previous");
    let replacement_wide = wide(&replacement)?;
    // SAFETY: descriptor is already private at CREATE_NEW, before any content.
    // A zero sharing mask keeps other handles out while writing.
    let handle = unsafe {
        CreateFileW(
            replacement_wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: newly created file handle, transferred exactly once to File.
    let mut file = unsafe { File::from_raw_handle(handle) };
    // Fail closed on filesystems that ignore security descriptors, before the
    // callback can write any export bytes.
    verify_private(&file, &descriptor)?;
    write(&mut file)?;
    file.sync_all()?;
    drop(file);
    let destination_wide = wide(&destination)?;
    if !existing {
        // No replace flag: another writer creating the destination wins safely.
        // Same-volume publication has no copy/delete fallback.
        if unsafe {
            MoveFileExW(
                replacement_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        return Ok(());
    }
    let previous_wide = wide(&previous)?;
    // SAFETY: all paths live on the same volume. Flags=0 deliberately refuses
    // ACL/metadata merge errors; never opt into IGNORE_ACL/MERGE_ERRORS. The
    // private backup protects the old bytes in documented partial-failure states.
    if unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            replacement_wide.as_ptr(),
            previous_wide.as_ptr(),
            0,
            ptr::null(),
            ptr::null(),
        )
    } != 0
    {
        if let Err(error) = std::fs::remove_file(&previous) {
            stage.retain = true;
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "export replaced; old copy retained at {}: {error}",
                    previous.display()
                ),
            ));
        }
        return Ok(());
    }
    let error = io::Error::last_os_error();
    let partial = matches!(
        error.raw_os_error().map(|n| n as u32),
        Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT | ERROR_UNABLE_TO_MOVE_REPLACEMENT_2)
    );
    let backup = previous.try_exists().unwrap_or(true);
    if backup || partial {
        stage.retain = true;
        // Restore only an absent destination, without replacing anything that
        // another writer has created. Otherwise retain both private recovery files.
        if backup
            && matches!(std::fs::symlink_metadata(&destination), Err(ref e) if e.kind()==io::ErrorKind::NotFound)
            && unsafe {
                MoveFileExW(
                    previous_wide.as_ptr(),
                    destination_wide.as_ptr(),
                    MOVEFILE_WRITE_THROUGH,
                )
            } != 0
        {
            stage.retain = false;
            return Err(io::Error::new(
                error.kind(),
                format!("export replacement failed; old export restored: {error}"),
            ));
        }
        return Err(io::Error::new(
            error.kind(),
            format!(
                "export replacement failed; inspect recovery files in {} before retrying: {error}",
                stage.directory.display()
            ),
        ));
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };
    use windows_sys::Win32::{
        Security::{
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            GetSecurityDescriptorControl, GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
            SE_DACL_PROTECTED,
        },
        Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "fl-export-win-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn destination(&self) -> PathBuf {
            self.0.join("export.txt")
        }
        fn entries(&self) -> Vec<String> {
            let mut entries: Vec<_> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            entries.sort();
            entries
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn acl_bytes(file: &File) -> (Vec<u8>, bool) {
        let mut acl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        // SAFETY: valid owned file handle and initialized out-pointers.
        assert_eq!(
            unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut acl,
                    ptr::null_mut(),
                    &mut descriptor,
                )
            },
            0
        );
        let descriptor = LocalAllocation(descriptor);
        assert!(!acl.is_null(), "null DACL would grant unrestricted access");
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: live API-allocated descriptor, initialized out-pointers.
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) },
            0
        );
        // SAFETY: successful GetSecurityInfo returns a valid ACL of AclSize bytes.
        let bytes =
            unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize) }
                .to_vec();
        (bytes, control & SE_DACL_PROTECTED != 0)
    }
    fn expected_private_acl() -> Vec<u8> {
        let descriptor = private_descriptor().unwrap();
        let mut present = 0;
        let mut acl = ptr::null_mut();
        let mut defaulted = 0;
        // SAFETY: descriptor is initialized by the documented SDDL API.
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted)
            },
            0
        );
        assert_ne!(present, 0);
        assert!(!acl.is_null());
        // SAFETY: this valid ACL comes from the live descriptor above.
        unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), (*acl).AclSize as usize) }.to_vec()
    }
    #[test]
    fn stage_is_private_before_first_byte_and_parent_acl_is_unchanged() {
        let f = Fixture::new();
        let target = f.destination();
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&f.0)
            .unwrap();
        let before = acl_bytes(&parent);
        let expected = expected_private_acl();
        write_with(&target, |file| {
            assert_eq!(file.metadata()?.len(), 0);
            let (acl, protected) = acl_bytes(file);
            assert!(protected);
            assert_eq!(acl, expected);
            file.write_all(b"harmless Windows export fixture")
        })
        .unwrap();
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"harmless Windows export fixture"
        );
        assert_eq!(acl_bytes(&File::open(&target).unwrap()), (expected, true));
        assert_eq!(acl_bytes(&parent), before);
        assert_eq!(f.entries(), ["export.txt"]);
    }
    #[test]
    fn replacement_preserves_dacl_and_old_reader_snapshot() {
        let f = Fixture::new();
        let target = f.destination();
        write(&target, b"old complete export").unwrap();
        let mut old = File::open(&target).unwrap();
        let before = acl_bytes(&old);
        write(&target, b"new complete export").unwrap();
        let mut bytes = Vec::new();
        old.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"old complete export");
        assert_eq!(std::fs::read(&target).unwrap(), b"new complete export");
        assert_eq!(acl_bytes(&File::open(&target).unwrap()), before);
        assert_eq!(f.entries(), ["export.txt"]);
    }
    #[test]
    fn partial_write_failure_preserves_old_bytes_and_removes_stage() {
        let f = Fixture::new();
        let target = f.destination();
        write(&target, b"old complete export").unwrap();
        let before = acl_bytes(&File::open(&target).unwrap());
        let result = write_with(&target, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected precommit write failure"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old complete export");
        assert_eq!(acl_bytes(&File::open(&target).unwrap()), before);
        assert_eq!(f.entries(), ["export.txt"]);
    }
    #[test]
    fn destination_created_by_another_writer_is_not_replaced() {
        let f = Fixture::new();
        let target = f.destination();
        let result = write_with(&target, |file| {
            std::fs::write(&target, b"other writer")?;
            file.write_all(b"staged writer")
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"other writer");
        assert_eq!(f.entries(), ["export.txt"]);
    }
    #[test]
    fn commit_sharing_failure_preserves_old_export_and_cleans_stage() {
        let f = Fixture::new();
        let target = f.destination();
        write(&target, b"old complete export").unwrap();
        let before = acl_bytes(&File::open(&target).unwrap());
        let mut blocker = None;
        let result = write_with(&target, |file| {
            blocker = Some(
                OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .open(&target)?,
            );
            file.write_all(b"staged writer")
        });
        assert!(result.is_err());
        drop(blocker);
        assert_eq!(std::fs::read(&target).unwrap(), b"old complete export");
        assert_eq!(acl_bytes(&File::open(&target).unwrap()), before);
        assert_eq!(f.entries(), ["export.txt"]);
    }
}
