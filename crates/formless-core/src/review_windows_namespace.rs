//! Namespace mutations against exact source and destination directory HANDLEs.
//! No named reopen, replacement, privilege adjustment, or security-descriptor setter.
//!
//! https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_link_information
//! https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntsetinformationfile
//! https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info
use std::{
    ffi::OsStr,
    fs::File,
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle},
    },
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
    },
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FileLinkInformation, NtCreateFile, NtSetInformationFile, FILE_CREATE,
            FILE_DIRECTORY_FILE, FILE_LINK_INFORMATION, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            CloseHandle, RtlNtStatusToDosError, HANDLE, INVALID_HANDLE_VALUE, STATUS_PENDING,
            STATUS_SUCCESS, UNICODE_STRING, WAIT_OBJECT_0,
        },
        Storage::FileSystem::{
            FileDispositionInfoEx, FileIdInfo, FileRenameInfo, GetFileInformationByHandleEx,
            SetFileInformationByHandle, DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY,
            FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            FILE_DISPOSITION_INFO_EX, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO,
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_READ_EA,
            FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
            FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, READ_CONTROL, SYNCHRONIZE,
        },
        System::{
            Threading::{WaitForSingleObject, INFINITE},
            IO::{IO_STATUS_BLOCK, IO_STATUS_BLOCK_0},
        },
    },
};

struct NativeGate {
    lock: Mutex<()>,
    poisoned: AtomicBool,
}
impl NativeGate {
    const fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            poisoned: AtomicBool::new(false),
        }
    }
    fn enter(&self) -> io::Result<MutexGuard<'_, ()>> {
        let guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("native namespace gate poisoned"))?;
        if self.poisoned.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "native namespace completion uncertain; restart and reconcile retained objects",
            ));
        }
        Ok(guard)
    }
    fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }
}
static NATIVE_GATE: NativeGate = NativeGate::new();

fn status_error(status: i32, operation: &str) -> io::Error {
    if status < 0 {
        // Negative NTSTATUS is a completed error. Keep OS error classes such
        // as name collision/absence for the rooted caller's reconciliation.
        let error = unsafe { RtlNtStatusToDosError(status) };
        if error != 0 && error != 317 {
            return io::Error::from_raw_os_error(error as i32);
        }
    }
    io::Error::other(format!("native {operation} did not report exact success (NTSTATUS {status:#010x}); verify retained-object state"))
}

fn component(leaf: &OsStr) -> io::Result<Vec<u16>> {
    let wide: Vec<u16> = leaf.encode_wide().collect();
    if wide.is_empty()
        || wide.len() > 255
        || wide == [46]
        || wide == [46, 46]
        || wide.last().is_some_and(|unit| matches!(*unit, 32 | 46))
        || wide
            .iter()
            .any(|unit| matches!(*unit, 0..=31 | 34 | 42 | 47 | 58 | 60 | 62 | 63 | 92 | 124))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one ordinary Windows filename",
        ));
    }
    Ok(wide)
}

fn volume(file: &File) -> io::Result<u64> {
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: live borrowed HANDLE and properly aligned output allocation.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.assume_init() }.VolumeSerialNumber)
}

fn check(source: &File, directory: &File) -> io::Result<()> {
    if !source.metadata()?.is_file() || !directory.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected regular source and pinned directory",
        ));
    }
    if volume(source)? != volume(directory)? {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retained object and destination must share a volume",
        ));
    }
    Ok(())
}

fn buffer_bytes(header: usize, name_offset: usize, name: &[u16]) -> Vec<u64> {
    // Include an extra NUL for defensive interoperability; FileNameLength does
    // not include it. u64 allocation also satisfies pointer-field alignment.
    let bytes = header.max(name_offset + (name.len() + 1) * 2);
    vec![0; bytes.div_ceil(size_of::<u64>())]
}

struct LinkRequest {
    source: File,
    _directory: File,
    buffer: Vec<u64>,
    status: IO_STATUS_BLOCK,
}

/// Create a no-replace link to the exact source object in the pinned directory.
/// The caller separately authorizes the source identity, retained-store root,
/// link count and lifecycle. No serialized identity grants namespace authority.
///
/// # Safety
/// `source` must be an exclusively operated synchronous file object opened with
/// SYNCHRONIZE and without FILE_FLAG_OVERLAPPED. Other clones must not perform
/// I/O concurrently until this call returns. This is proven at the caller's
/// actual CreateFile/OpenOptions factory, not inferred from an arbitrary File.
/// The source and directory handles must not be closed through raw handles.
pub(super) unsafe fn link_existing(
    source: &File,
    directory: &File,
    leaf: &OsStr,
) -> io::Result<()> {
    let _gate = NATIVE_GATE.enter()?;
    let name = component(leaf)?;
    check(source, directory)?;
    let mut request = Box::new(LinkRequest {
        source: source.try_clone()?,
        _directory: directory.try_clone()?,
        buffer: buffer_bytes(
            size_of::<FILE_LINK_INFORMATION>(),
            offset_of!(FILE_LINK_INFORMATION, FileName),
            &name,
        ),
        status: IO_STATUS_BLOCK {
            Anonymous: IO_STATUS_BLOCK_0 {
                Status: STATUS_PENDING,
            },
            Information: 0,
        },
    });
    let info = request.buffer.as_mut_ptr().cast::<FILE_LINK_INFORMATION>();
    // SAFETY: zeroed, sufficiently sized/aligned storage; only the ordinary
    // Boolean union arm is set, with no Ex/POSIX/bypass-access flags.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = 0;
        (*info).RootDirectory = request._directory.as_raw_handle();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    // SAFETY: caller proves synchronous ownership; all request storage and both
    // cloned HANDLEs remain owned until kernel completion below.
    let mut status = unsafe {
        NtSetInformationFile(
            request.source.as_raw_handle(),
            &mut request.status,
            info.cast(),
            (request.buffer.len() * 8) as u32,
            FileLinkInformation,
        )
    };
    if status == STATUS_PENDING {
        // Synchronous sources normally complete before returning. Retain the
        // request even on this unexpected path; never timeout/drop kernel-owned
        // IOSB/input memory while the operation can still be pending.
        let waited = unsafe { WaitForSingleObject(request.source.as_raw_handle(), INFINITE) };
        if waited != WAIT_OBJECT_0 {
            NATIVE_GATE.poison();
            std::mem::forget(request);
            return Err(io::Error::other(
                "native link completion unknown; pending request storage and handles retained",
            ));
        }
        status = unsafe { request.status.Anonymous.Status };
        if status == STATUS_PENDING {
            NATIVE_GATE.poison();
            std::mem::forget(request);
            return Err(io::Error::other(
                "native link still pending after wait; request storage and handles retained",
            ));
        }
    }
    if status != STATUS_SUCCESS {
        return Err(status_error(status, "link"));
    }
    Ok(())
}

/// Move the exact opened link into a pinned directory without replacement.
///
/// # Safety
/// Same synchronous, exclusive file-object contract as `link_existing` applies.
/// Additionally the source must have DELETE access. Its opened-link identity
/// must be the authorized locator: a different hardlink of the same file is not
/// interchangeable. File::try_clone preserves this file object and link name.
pub(super) unsafe fn rename_existing(
    source: &File,
    directory: &File,
    leaf: &OsStr,
) -> io::Result<()> {
    let _gate = NATIVE_GATE.enter()?;
    let name = component(leaf)?;
    check(source, directory)?;
    let source = source.try_clone()?;
    let directory = directory.try_clone()?;
    let mut buffer = buffer_bytes(
        size_of::<FILE_RENAME_INFO>(),
        offset_of!(FILE_RENAME_INFO, FileName),
        &name,
    );
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).Anonymous.ReplaceIfExists = 0;
        (*info).RootDirectory = directory.as_raw_handle();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    // SAFETY: documented public Win32 synchronous API, no OVERLAPPED argument;
    // complete aligned storage and both owned HANDLEs outlive its return.
    if unsafe {
        SetFileInformationByHandle(
            source.as_raw_handle(),
            FileRenameInfo,
            info.cast(),
            (buffer.len() * 8) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Unlink only the exact opened name, preserving all other links and handles.
/// Uses DELETE | POSIX_SEMANTICS only: no read-only override, replacement or
/// bypass access flags. Unsupported filesystems fail closed.
///
/// # Safety
/// Caller proves the same synchronous exclusive file-object contract as
/// `rename_existing`, DELETE access, and the checked exact opened-link name.
/// A source opened by `open_exclusive` prevents competing DELETE-only renames.
/// Successful return marks POSIX deletion; caller must drop EVERY clone of this
/// source file object before proving visible-name absence or committing Undo.
/// Separately opened metadata witnesses may remain alive. Preflight support on
/// a private owned same-volume probe before offering namespace review actions.
pub(super) unsafe fn unlink_existing(source: &File) -> io::Result<()> {
    let _gate = NATIVE_GATE.enter()?;
    if !source.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected regular opened unlink source",
        ));
    }
    let source = source.try_clone()?;
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: caller's source is synchronous and DELETE-capable; the complete
    // fixed-size native buffer and owned HANDLE remain alive through return.
    if unsafe {
        SetFileInformationByHandle(
            source.as_raw_handle(),
            FileDispositionInfoEx,
            (&info as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// All pointers passed to NtCreateFile point into this owned request or its
// owned allocations. A single impossible wait failure retains the whole box;
// the sticky process-wide gate then rejects every further namespace operation.
struct OpenRequest {
    directory: File,
    name: Vec<u16>,
    unicode: UNICODE_STRING,
    attributes: OBJECT_ATTRIBUTES,
    security: Option<crate::win_pipe::OwnerOnly>,
    status: IO_STATUS_BLOCK,
    output: HANDLE,
}
impl Drop for OpenRequest {
    fn drop(&mut self) {
        if !self.output.is_null() && self.output != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.output);
            }
        }
    }
}

fn open_relative(
    directory: &File,
    leaf: &OsStr,
    access: u32,
    create: bool,
    is_directory: bool,
    share: u32,
) -> io::Result<File> {
    let _gate = NATIVE_GATE.enter()?;
    let name = component(leaf)?;
    // No WRITE_DAC/WRITE_OWNER, backup-intent or generic all-access request.
    // ACCESS_SYSTEM_SECURITY may only request rights already available in the
    // caller's token for comparing a previously complete SACL capture; this
    // module never enables privileges or changes a security descriptor.
    const ALLOWED: u32 = 0x0100_0000 // ACCESS_SYSTEM_SECURITY, no privilege adjustment
        | 0x8000_0000
        | 0x4000_0000
        | FILE_READ_DATA
        | FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_TRAVERSE
        | FILE_READ_EA
        | FILE_WRITE_EA
        | FILE_READ_ATTRIBUTES
        | FILE_WRITE_ATTRIBUTES
        | READ_CONTROL
        | SYNCHRONIZE
        | DELETE;
    if access & !ALLOWED != 0 || !directory.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported native open access or parent",
        ));
    }
    let security = create.then(crate::win_pipe::OwnerOnly::new).transpose()?;
    let mut request = Box::new(OpenRequest {
        directory: directory.try_clone()?,
        name,
        unicode: UNICODE_STRING {
            Length: 0,
            MaximumLength: 0,
            Buffer: std::ptr::null_mut(),
        },
        attributes: OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: std::ptr::null_mut(),
            ObjectName: std::ptr::null(),
            Attributes: 0,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        },
        security,
        status: IO_STATUS_BLOCK {
            Anonymous: IO_STATUS_BLOCK_0 {
                Status: STATUS_PENDING,
            },
            Information: 0,
        },
        output: std::ptr::null_mut(),
    });
    request.unicode.Length = (request.name.len() * 2) as u16;
    request.unicode.MaximumLength = request.unicode.Length;
    request.unicode.Buffer = request.name.as_mut_ptr();
    request.attributes.RootDirectory = request.directory.as_raw_handle();
    request.attributes.ObjectName = &request.unicode;
    if let Some(security) = &request.security {
        // OwnerOnly owns the descriptor throughout pending completion. No
        // caller-borrowed SECURITY_ATTRIBUTES or descriptor enters the kernel.
        request.attributes.SecurityDescriptor = unsafe {
            (*(security
                .as_raw()
                .cast::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>()))
            .lpSecurityDescriptor
        };
    }
    // SAFETY: all variable-length names, attributes, SD, output HANDLE and IOSB
    // remain owned/stable; native name is one component relative to a live root.
    let mut status = unsafe {
        NtCreateFile(
            &mut request.output,
            access | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            &request.attributes,
            &mut request.status,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            share,
            if create { FILE_CREATE } else { FILE_OPEN },
            (if is_directory {
                FILE_DIRECTORY_FILE
            } else {
                FILE_NON_DIRECTORY_FILE
            }) | FILE_OPEN_REPARSE_POINT
                | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status == STATUS_PENDING {
        let waited = if request.output.is_null() || request.output == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(unsafe { WaitForSingleObject(request.output, INFINITE) })
        };
        if waited != Some(WAIT_OBJECT_0) {
            NATIVE_GATE.poison();
            std::mem::forget(request);
            return Err(io::Error::other(
                "native open completion unknown; one request retained and namespace gate poisoned",
            ));
        }
        status = unsafe { request.status.Anonymous.Status };
        if status == STATUS_PENDING {
            NATIVE_GATE.poison();
            std::mem::forget(request);
            return Err(io::Error::other(
                "native open remains pending; one request retained and namespace gate poisoned",
            ));
        }
    }
    if status != STATUS_SUCCESS {
        return Err(status_error(status, "open"));
    }
    if request.output.is_null() || request.output == INVALID_HANDLE_VALUE {
        return Err(io::Error::other("native open returned no file HANDLE"));
    }
    let output = request.output;
    request.output = std::ptr::null_mut();
    let file = unsafe { File::from_raw_handle(output) };
    let metadata = file.metadata()?;
    if (if is_directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    }) || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native open is not the requested ordinary non-reparse object",
        ));
    }
    if volume(&file)? != volume(&request.directory)? {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native open volume differs from pinned parent",
        ));
    }
    Ok(file)
}

/// Create a new owner/SYSTEM-only private metadata file without replacing an
/// existing name. Returned HANDLE is synchronous, read/write/DELETE capable.
pub(super) fn create_private(directory: &File, leaf: &OsStr) -> io::Result<File> {
    open_relative(
        directory,
        leaf,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
        true,
        false,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

/// Open an existing ordinary file relative to the exact directory HANDLE.
/// It never creates or truncates; caller chooses the restricted access mask.
/// Optional audit-security access uses only privileges already available in its token.
/// Returned HANDLE always has SYNCHRONIZE and synchronous non-alertable I/O.
pub(super) fn open_existing(directory: &File, leaf: &OsStr, access: u32) -> io::Result<File> {
    open_relative(
        directory,
        leaf,
        access,
        false,
        false,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

/// Open an existing rooted file with exclusive data AND namespace access.
/// Metadata-only attribute/SYNCHRONIZE pins remain compatible; any other open
/// requesting data read/write or DELETE access fails until this handle drops.
/// This prevents another DELETE-only handle from renaming the checked link
/// between the caller's identity/content check and its namespace operation.
/// Caller must complete private before-image restoration before public linking.
pub(super) fn open_exclusive(directory: &File, leaf: &OsStr, access: u32) -> io::Result<File> {
    open_relative(directory, leaf, access, false, false, 0)
}

/// Create an owner/SYSTEM-only private directory, never replacing a name.
pub(super) fn create_private_directory(parent: &File, leaf: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        leaf,
        FILE_LIST_DIRECTORY
            | FILE_ADD_FILE
            | FILE_ADD_SUBDIRECTORY
            | FILE_TRAVERSE
            | FILE_READ_ATTRIBUTES
            | READ_CONTROL
            | SYNCHRONIZE,
        true,
        true,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

/// Open an existing ordinary directory through its exact parent HANDLE.
/// Final reparse points are opened without traversal and then refused.
pub(super) fn open_directory(parent: &File, leaf: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        leaf,
        FILE_LIST_DIRECTORY
            | FILE_ADD_FILE
            | FILE_ADD_SUBDIRECTORY
            | FILE_TRAVERSE
            | FILE_READ_ATTRIBUTES
            | READ_CONTROL
            | SYNCHRONIZE,
        false,
        true,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

/// Open a directory for exact-handle empty removal, without security setter rights.
pub(super) fn open_directory_for_removal(parent: &File, leaf: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        leaf,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
        false,
        true,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::OpenOptions,
        os::windows::fs::OpenOptionsExt,
        path::{Path, PathBuf},
    };
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
    fn fixture() -> Fixture {
        let fixture = Fixture(std::env::temp_dir().join(format!(
            "formless-native-namespace-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        fixture
    }
    fn source(path: &Path) -> File {
        OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap()
    }
    fn directory(path: &Path) -> File {
        OpenOptions::new()
            .access_mode(
                FILE_LIST_DIRECTORY
                    | FILE_ADD_FILE
                    | FILE_ADD_SUBDIRECTORY
                    | FILE_TRAVERSE
                    | READ_CONTROL
                    | FILE_READ_ATTRIBUTES
                    | SYNCHRONIZE,
            )
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap()
    }

    // Attribute-only retagging bypasses data sharing locks; this fixture proves
    // that namespace operations use the captured directory object, not its name.
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

    fn pinned_names(parent: &File) -> Vec<std::ffi::OsString> {
        use std::os::windows::ffi::OsStringExt;
        let mut storage = vec![0u64; 8192];
        assert_ne!(
            unsafe {
                GetFileInformationByHandleEx(
                    parent.as_raw_handle(),
                    FileIdBothDirectoryRestartInfo,
                    storage.as_mut_ptr().cast(),
                    (storage.len() * 8) as u32,
                )
            },
            0
        );
        let mut offset = 0usize;
        let mut names = Vec::new();
        loop {
            assert!(offset + size_of::<FILE_ID_BOTH_DIR_INFO>() <= storage.len() * 8);
            let record = unsafe {
                &*(storage
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<FILE_ID_BOTH_DIR_INFO>())
            };
            let name_offset = offset + offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
            let length = record.FileNameLength as usize;
            assert_eq!(length % 2, 0);
            assert!(name_offset + length <= storage.len() * 8);
            let name = unsafe {
                std::slice::from_raw_parts(
                    storage.as_ptr().cast::<u8>().add(name_offset).cast::<u16>(),
                    length / 2,
                )
            };
            names.push(std::ffi::OsString::from_wide(name));
            if record.NextEntryOffset == 0 {
                break;
            }
            assert_eq!(record.NextEntryOffset % 8, 0);
            offset += record.NextEntryOffset as usize;
        }
        names
    }

    #[test]
    fn attribute_retag_cannot_redirect_rooted_namespace_operations() {
        use std::io::Write;
        let fixture = fixture();
        let root = fixture.0.join("root");
        let outside = fixture.0.join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("victim"), b"outside sentinel").unwrap();
        let parent = directory(&root);
        let attribute = OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        junction(&attribute, &outside).unwrap();
        assert_eq!(
            std::fs::read(root.join("victim")).unwrap(),
            b"outside sentinel"
        );
        let from = fixture.0.join("source");
        std::fs::write(&from, b"original source").unwrap();
        let file = source(&from);
        unsafe {
            link_existing(&file, &parent, OsStr::new("retained")).unwrap();
            rename_existing(&file, &parent, OsStr::new("moved")).unwrap();
        }
        let mut intent = create_private(&parent, OsStr::new("intent")).unwrap();
        intent.write_all(b"private intent").unwrap();
        let reopened = open_existing(&parent, OsStr::new("intent"), FILE_GENERIC_READ).unwrap();
        assert_eq!(reopened.metadata().unwrap().len(), 14);
        let child = create_private_directory(&parent, OsStr::new("child")).unwrap();
        assert!(child.metadata().unwrap().is_dir());
        assert!(open_directory(&parent, OsStr::new("child"))
            .unwrap()
            .metadata()
            .unwrap()
            .is_dir());
        let names = pinned_names(&parent);
        for name in ["retained", "moved", "intent", "child"] {
            assert!(names.contains(&name.into()));
        }
        assert_eq!(
            std::fs::read(outside.join("victim")).unwrap(),
            b"outside sentinel"
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        // Drop every borrowed HANDLE before removing the reparse entry. Never
        // recurse through a junction during fixture teardown.
        drop((attribute, parent, file, intent, reopened, child));
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn component_and_buffer_layout_reject_alternate_names() {
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "a\\b",
            "a:b",
            "x.",
            "x ",
            "a\0b",
        ] {
            assert!(component(OsStr::new(name)).is_err(), "{name:?}");
        }
        let name = component(OsStr::new("retained-雪")).unwrap();
        for (header, offset) in [
            (
                size_of::<FILE_LINK_INFORMATION>(),
                offset_of!(FILE_LINK_INFORMATION, FileName),
            ),
            (
                size_of::<FILE_RENAME_INFO>(),
                offset_of!(FILE_RENAME_INFO, FileName),
            ),
        ] {
            let storage = buffer_bytes(header, offset, &name);
            assert_eq!(
                storage.as_ptr() as usize % std::mem::align_of::<FILE_LINK_INFORMATION>(),
                0
            );
            assert!(storage.len() * 8 >= header.max(offset + (name.len() + 1) * 2));
            assert!(storage.iter().all(|value| *value == 0));
        }
    }
    #[test]
    fn impossible_completion_poison_is_sticky_without_retries() {
        let gate = NativeGate::new();
        drop(gate.enter().unwrap());
        gate.poison();
        assert!(gate.enter().is_err());
        assert!(gate.enter().is_err());
    }
    #[test]
    fn rooted_factory_creates_no_replace_and_opens_without_truncation() {
        use std::io::{Read, Write};
        let fixture = fixture();
        let parent = directory(&fixture.0);
        let mut created = create_private(&parent, OsStr::new("intent")).unwrap();
        created.write_all(b"immutable intent").unwrap();
        created.sync_all().unwrap();
        assert!(create_private(&parent, OsStr::new("intent")).is_err());
        let mut opened = open_existing(&parent, OsStr::new("intent"), FILE_GENERIC_READ).unwrap();
        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"immutable intent");
        assert!(open_existing(&parent, OsStr::new("missing"), FILE_READ_ATTRIBUTES).is_err());
        assert!(open_existing(&parent, OsStr::new("intent"), WRITE_DAC).is_err());
        assert!(!fixture.0.join("missing").exists());
        let child = create_private_directory(&parent, OsStr::new("objects")).unwrap();
        assert!(create_private_directory(&parent, OsStr::new("objects")).is_err());
        let reopened = open_directory(&parent, OsStr::new("objects")).unwrap();
        let nested = create_private(&reopened, OsStr::new("nested")).unwrap();
        assert!(nested.metadata().unwrap().is_file());
        assert!(child.metadata().unwrap().is_dir());
        assert!(open_existing(&parent, OsStr::new("objects"), FILE_READ_ATTRIBUTES).is_err());
        assert!(open_directory(&parent, OsStr::new("intent")).is_err());
    }
    #[test]
    fn exclusive_rooted_open_allows_metadata_pins_but_blocks_data_and_delete_peers() {
        use std::io::Write;
        let fixture = fixture();
        let parent = directory(&fixture.0);
        drop(create_private(&parent, OsStr::new("object")).unwrap());
        let pin = open_existing(&parent, OsStr::new("object"), FILE_READ_ATTRIBUTES).unwrap();
        let mut exclusive = open_exclusive(
            &parent,
            OsStr::new("object"),
            FILE_READ_DATA
                | FILE_WRITE_DATA
                | FILE_READ_ATTRIBUTES
                | FILE_WRITE_ATTRIBUTES
                | DELETE,
        )
        .unwrap();
        let error = open_existing(&parent, OsStr::new("object"), FILE_READ_DATA).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(32),
            "must fail with sharing violation before any data access"
        );
        let delete_error = open_existing(&parent, OsStr::new("object"), DELETE).unwrap_err();
        assert_eq!(
            delete_error.raw_os_error(),
            Some(32),
            "DELETE-only peer must not race exact-name CAS"
        );
        let other_pin = open_existing(&parent, OsStr::new("object"), FILE_READ_ATTRIBUTES).unwrap();
        exclusive.write_all(b"restored before publication").unwrap();
        exclusive.sync_all().unwrap();
        drop(exclusive);
        assert!(open_existing(&parent, OsStr::new("object"), FILE_READ_DATA).is_ok());
        drop((pin, other_pin));
    }
    #[test]
    fn exact_posix_unlink_preserves_retained_alias_and_metadata_witnesses() {
        use std::io::Write;
        let fixture = fixture();
        let parent = directory(&fixture.0);
        let mut created = create_private(&parent, OsStr::new("public")).unwrap();
        created.write_all(b"retained original").unwrap();
        created.sync_all().unwrap();
        drop(created);
        let source = open_exclusive(
            &parent,
            OsStr::new("public"),
            FILE_READ_DATA | FILE_WRITE_DATA | DELETE,
        )
        .unwrap();
        unsafe {
            link_existing(&source, &parent, OsStr::new("secret")).unwrap();
        }
        let witness = open_existing(&parent, OsStr::new("public"), FILE_READ_ATTRIBUTES).unwrap();
        let secret_pin =
            open_existing(&parent, OsStr::new("secret"), FILE_READ_ATTRIBUTES).unwrap();
        unsafe {
            unlink_existing(&source).unwrap();
        }
        drop(source); // POSIX delete-name completion requires this file object's last handle.
        assert!(!fixture.0.join("public").exists());
        assert_eq!(
            std::fs::read(fixture.0.join("secret")).unwrap(),
            b"retained original"
        );
        assert_eq!(witness.metadata().unwrap().len(), 17);
        assert_eq!(secret_pin.metadata().unwrap().len(), 17);
    }
    #[test]
    fn native_link_and_rename_are_same_object_and_never_replace() {
        let fixture = fixture();
        let from = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(&from, b"original").unwrap();
        std::fs::write(destination.join("occupied"), b"sentinel").unwrap();
        let file = source(&from);
        let parent = directory(&destination);
        // SAFETY: source factory explicitly requests SYNCHRONIZE/DELETE and
        // omits OVERLAPPED; no concurrent I/O on this private owned fixture.
        unsafe {
            assert!(link_existing(&file, &parent, OsStr::new("occupied")).is_err());
            link_existing(&file, &parent, OsStr::new("retained")).unwrap();
            assert!(rename_existing(&file, &parent, OsStr::new("occupied")).is_err());
            rename_existing(&file, &parent, OsStr::new("moved")).unwrap();
        }
        assert!(!from.exists());
        assert_eq!(
            std::fs::read(destination.join("occupied")).unwrap(),
            b"sentinel"
        );
        std::fs::write(destination.join("moved"), b"same object").unwrap();
        assert_eq!(
            std::fs::read(destination.join("retained")).unwrap(),
            b"same object"
        );
    }
    #[test]
    fn renamed_parent_handle_never_uses_its_old_path() {
        let fixture = fixture();
        let old = fixture.0.join("parent");
        let moved = fixture.0.join("moved-parent");
        std::fs::create_dir(&old).unwrap();
        let parent = directory(&old);
        let from = fixture.0.join("source");
        std::fs::write(&from, b"private").unwrap();
        let file = source(&from);
        std::fs::rename(&old, &moved).unwrap();
        std::fs::create_dir(&old).unwrap();
        unsafe {
            link_existing(&file, &parent, OsStr::new("retained")).unwrap();
        }
        assert_eq!(std::fs::read(moved.join("retained")).unwrap(), b"private");
        assert!(std::fs::read_dir(&old).unwrap().next().is_none());
    }
}
