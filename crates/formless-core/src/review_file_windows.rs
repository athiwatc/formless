//! Windows review access pins every ancestor against rename/delete and rejects
//! reparse points. ReplaceFile preserves existing ACLs; its documented partial
//! failure modes still require reconciliation with the durable prepared receipt.
use super::{ReviewImage, MAX_REVIEW_BYTES};
use anyhow::{bail, Context, Result};
use formless_runtime::sensitive_paths::{PathDecision, PathPolicy};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::Win32::Storage::FileSystem::*;

// Standard access right from WinNT.h; requesting it never enables a privilege.
const ACCESS_SYSTEM_SECURITY_RIGHT: u32 = 0x0100_0000;
pub(super) struct Access {
    path: PathBuf,
    guards: Vec<File>,
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
fn append(path: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut raw = path.as_os_str().to_owned();
    if !raw.encode_wide().last().is_some_and(|c| c == b'\\' as u16) {
        raw.push("\\");
    }
    raw.push(name);
    raw.into()
}
fn final_path(file: &File) -> Result<PathBuf> {
    let mut out = vec![0u16; 32768];
    // SAFETY: live borrowed HANDLE, valid writable UTF-16 output buffer.
    let size = unsafe {
        GetFinalPathNameByHandleW(file.as_raw_handle(), out.as_mut_ptr(), out.len() as u32, 0)
    } as usize;
    if size == 0 || size >= out.len() {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(std::ffi::OsString::from_wide(&out[..size]).into())
}
fn directory(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if !file.metadata()?.is_dir()
        || file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        bail!("Review directory is not an ordinary non-reparse directory");
    }
    Ok(file)
}
impl Access {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let mut parts = path.components();
        let Some(Component::Prefix(prefix)) = parts.next() else {
            bail!("Review requires a native absolute Windows path");
        };
        if !matches!(
            prefix.kind(),
            Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(..) | Prefix::VerbatimUNC(..)
        ) {
            bail!("Unknown Windows path namespace is not reviewable");
        }
        if !matches!(parts.next(), Some(Component::RootDir)) {
            bail!("Drive-relative paths are not reviewable");
        }
        let mut root = prefix.as_os_str().to_owned();
        root.push("\\");
        let mut named = PathBuf::from(root);
        let mut guards = vec![directory(&named)?];
        let mut names = Vec::new();
        for part in parts {
            let Component::Normal(name) = part else {
                bail!("Unresolved Windows path components are not reviewable");
            };
            if name
                .encode_wide()
                .any(|c| matches!(c, 0 | 34 | 42 | 47 | 58 | 60 | 62 | 63 | 92 | 124))
            {
                bail!("Alternate streams and malformed Windows path components are not reviewable");
            }
            names.push(name.to_owned());
        }
        let leaf = names.pop().context("A filesystem root is not reviewable")?;
        for name in names {
            named = append(&named, &name);
            guards.push(directory(&named)?);
        }
        let path = append(&final_path(guards.last().context("no parent")?)?, &leaf);
        Ok(Self { path, guards })
    }
    pub(super) fn recheck(&self, raw: &Path, policy: &PathPolicy) -> Result<()> {
        let parent = final_path(self.guards.last().context("no anchored parent")?)?;
        let actual = append(
            &parent,
            self.path.file_name().context("no recorded filename")?,
        );
        if !matches!(
            policy.classify(raw, &actual).map_err(anyhow::Error::msg)?,
            PathDecision::Ordinary
        ) {
            bail!("Review destination is now a protected profile or credential path");
        }
        Ok(())
    }
    pub(super) fn read(&self, raw: &Path, policy: &PathPolicy) -> Result<ReviewImage> {
        self.read_bounded(raw, policy, MAX_REVIEW_BYTES, false, None)
    }
    pub(super) fn read_bounded(
        &self,
        raw: &Path,
        policy: &PathPolicy,
        limit: usize,
        capture: bool,
        alias: Option<&Path>,
    ) -> Result<ReviewImage> {
        self.recheck(raw, policy)?;
        let privileged = OpenOptions::new()
            .read(true)
            .access_mode(
                windows_sys::Win32::Foundation::GENERIC_READ | ACCESS_SYSTEM_SECURITY_RIGHT,
            )
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&self.path);
        let file = match privileged.or_else(|_| {
            OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&self.path)
        }) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && alias.is_none() => {
                return Ok(ReviewImage::missing())
            }
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("Only regular non-reparse files are hunk-editable");
        }
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: live HANDLE and writable output. Read native metadata only
        // after the successful call initialized all fields.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let single_link = unsafe { info.assume_init() }.nNumberOfLinks == 1;
        if !single_link && !capture {
            bail!("Files with multiple hard links are not hunk-editable");
        }
        let streams_complete = match require_default_stream_only(&file) {
            Ok(()) => true,
            Err(_) if capture => false,
            Err(error) => return Err(error),
        };
        if let Some(alias) = alias {
            self.verify_snapshot_alias(&file, alias, policy)?;
        }
        let actual = final_path(&file)?;
        if !matches!(
            policy.classify(raw, &actual).map_err(anyhow::Error::msg)?,
            PathDecision::Ordinary
        ) {
            bail!("Protected profile or credential path is not reviewable");
        }
        if metadata.len() > limit as u64 {
            bail!("File exceeds its {limit}-byte snapshot/review limit");
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != metadata.len() || bytes.len() > limit {
            bail!("Current file changed while reading");
        }
        let (security, complete) = match snapshot_security(&file) {
            Ok((security, complete)) => (Some(security), complete),
            Err(_) if capture => (None, false),
            Err(error) => return Err(error),
        };
        if let Some(alias) = alias {
            self.verify_snapshot_alias(&file, alias, policy)?;
        }
        Ok(ReviewImage {
            bytes: Some(bytes),
            mode: Some(metadata.file_attributes()),
            readonly: metadata.permissions().readonly(),
            security,
            security_complete: complete && single_link && streams_complete,
            ..Default::default()
        })
    }
    fn verify_snapshot_alias(&self, file: &File, alias: &Path, policy: &PathPolicy) -> Result<()> {
        // Metadata-only access to the requested final entry never supplies body
        // bytes. OPEN_REPARSE_POINT prevents following a final symlink; parent
        // aliases are permitted only while they still name the anchored identity.
        let entry = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(alias)?;
        if !entry.metadata()?.is_file()
            || entry.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            bail!("Snapshot alias no longer names a regular final entry");
        }
        fn identity(file: &File) -> Result<(u32, u32, u32)> {
            let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
            // SAFETY: live handle, writable native output, read only on success.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let info = unsafe { info.assume_init() };
            Ok((
                info.dwVolumeSerialNumber,
                info.nFileIndexHigh,
                info.nFileIndexLow,
            ))
        }
        let actual = alias.canonicalize()?;
        if identity(file)? != identity(&entry)?
            || actual != self.path
            || !matches!(
                policy
                    .classify(alias, &actual)
                    .map_err(anyhow::Error::msg)?,
                PathDecision::Ordinary
            )
        {
            bail!("Snapshot alias retargeted or no longer names the anchored regular file");
        }
        Ok(())
    }
    pub(super) fn replace(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
    ) -> Result<Vec<String>> {
        self.recheck(raw, policy)?;
        if (expected.bytes.is_some() && !expected.security_complete)
            || (desired.bytes.is_some() && !desired.security_complete)
        {
            bail!("Full Windows audit/SACL metadata is unavailable; no file write is allowed without changing privileges");
        }

        let parent = self.path.parent().context("no anchored parent")?;
        let temporary = append(
            parent,
            std::ffi::OsStr::new(&format!(".formless-review-{}", uuid::Uuid::new_v4())),
        );
        struct Temp(PathBuf, bool);
        impl Drop for Temp {
            fn drop(&mut self) {
                if self.1 {
                    unsafe { SetFileAttributesW(wide(&self.0).as_ptr(), FILE_ATTRIBUTE_NORMAL) };
                    let _ = std::fs::remove_file(&self.0);
                }
            }
        }
        let mut temp = Temp(temporary, false);
        if let Some(bytes) = &desired.bytes {
            let mode = desired
                .mode
                .context("Exact prior file attributes were not recorded")?;
            let supported = FILE_ATTRIBUTE_ARCHIVE
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_NORMAL
                | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
                | FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_READONLY
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_TEMPORARY;
            if mode & !supported != 0 {
                bail!("Special Windows file attributes cannot be preserved by interactive review");
            }
            // Do not expose an original file's restricted content through a
            // broadly inherited parent ACL while preparing its replacement.
            let security = crate::win_pipe::OwnerOnly::new()?;
            let handle = unsafe {
                CreateFileW(
                    wide(&temp.0).as_ptr(),
                    windows_sys::Win32::Foundation::GENERIC_WRITE
                        | WRITE_DAC
                        | WRITE_OWNER
                        | ACCESS_SYSTEM_SECURITY_RIGHT,
                    0,
                    security.as_raw().cast(),
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut file = unsafe { File::from_raw_handle(handle) };
            temp.1 = true;
            let security = desired
                .security
                .as_deref()
                .context("Exact prior Windows security metadata was not recorded")?;
            // Parse and bound metadata before publishing any new name. The
            // temporary stays owner-only while content is being prepared.
            let permissions: Permissions = serde_json::from_slice(security)?;
            permissions.validate()?;
            file.write_all(bytes)?;
            apply_security(&file, &permissions)?;
            file.sync_all()?;
            drop(file);
            // SAFETY: valid NUL-terminated native path and a validated set of
            // ordinary Windows file attributes. Temporary is not yet visible
            // under the target name.
            if unsafe { SetFileAttributesW(wide(&temp.0).as_ptr(), mode) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        self.recheck(raw, policy)?;
        if self.read(raw, policy)? != *expected {
            bail!("File content, existence or attributes changed before review commit");
        }
        // A prepared private receipt already contains both images. Unlike Unix
        // renameat, ReplaceFile errors 1176/1177 can have changed names; all
        // errors must be reconciled by the caller, never labelled unchanged.
        let result = if desired.bytes.is_some() {
            if expected.bytes.is_some() {
                unsafe {
                    ReplaceFileW(
                        wide(&self.path).as_ptr(),
                        wide(&temp.0).as_ptr(),
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                }
            } else {
                unsafe {
                    MoveFileExW(
                        wide(&temp.0).as_ptr(),
                        wide(&self.path).as_ptr(),
                        MOVEFILE_WRITE_THROUGH,
                    )
                }
            }
        } else if expected.bytes.is_some() {
            unsafe { DeleteFileW(wide(&self.path).as_ptr()) }
        } else {
            1
        };
        if result == 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error()).context("Windows replacement failed and may have changed file names; reconcile the prepared private receipt before any retry"));
        }
        temp.1 = false;
        Ok(vec!["File replacement completed; parent-directory durability could not be confirmed on Windows.".into()])
    }
}

fn require_default_stream_only(file: &File) -> Result<()> {
    // Aligned bounded native output; unknown filesystems and extra streams are
    // read-only rather than losing unrecorded data during deletion and Undo.
    let mut buffer = vec![0u64; 8192];
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStreamInfo,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
        )
    } == 0
    {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Named stream state could not be verified"));
    }
    let info = unsafe { &*buffer.as_ptr().cast::<FILE_STREAM_INFO>() };
    let offset = std::mem::offset_of!(FILE_STREAM_INFO, StreamName);
    let size = info.StreamNameLength as usize;
    if info.NextEntryOffset != 0 || size % 2 != 0 || size > buffer.len() * 8 - offset {
        bail!("Named streams or oversized stream metadata are not hunk-editable");
    }
    let name = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
            size / 2,
        )
    };
    if !name.iter().copied().eq("::$DATA".encode_utf16()) {
        bail!("Files with nondefault streams are not hunk-editable");
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Permissions {
    owner: Vec<u8>,
    group: Vec<u8>,
    dacl: Option<Vec<u8>>,
    protected: bool,
    sacl: Option<Vec<u8>>,
    sacl_protected: bool,
    full_sacl: bool,
}
fn sid_length(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < 8 || bytes[0] != 1 || bytes[1] > 15 {
        bail!("Invalid Windows security SID metadata");
    }
    let size = 8 + bytes[1] as usize * 4;
    if size != bytes.len() {
        bail!("Truncated or oversized Windows SID metadata");
    }
    Ok(size)
}
impl Permissions {
    fn validate(&self) -> Result<()> {
        sid_length(&self.owner)?;
        sid_length(&self.group)?;
        for (is_sacl, acl) in [(false, &self.dacl), (true, &self.sacl)] {
            if let Some(acl) = acl {
                if acl.len() < 8
                    || acl.len() > 65535
                    || !matches!(acl[0], 2 | 4)
                    || u16::from_le_bytes([acl[2], acl[3]]) as usize != acl.len()
                {
                    bail!("Invalid Windows DACL metadata");
                }
                let count = u16::from_le_bytes([acl[4], acl[5]]) as usize;
                let mut offset = 8;
                for _ in 0..count {
                    if offset + 8 > acl.len()
                        || !(if is_sacl {
                            matches!(acl[offset], 2 | 3 | 17)
                        } else {
                            matches!(acl[offset], 0 | 1)
                        })
                    {
                        bail!("Special Windows access-control entries are not supported by interactive review");
                    }
                    let size = u16::from_le_bytes([acl[offset + 2], acl[offset + 3]]) as usize;
                    if size < 16 || size % 4 != 0 || offset + size > acl.len() {
                        bail!("Invalid Windows access-control entry size");
                    }
                    sid_length(&acl[offset + 8..offset + size])?;
                    offset += size;
                }
                if offset != acl.len() {
                    bail!("Unexpected trailing Windows ACL data");
                }
            }
        }
        Ok(())
    }
}
fn snapshot_security(file: &File) -> Result<(Vec<u8>, bool)> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{Authorization::*, *},
    };
    let mut owner = std::ptr::null_mut();
    let mut group = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let mut sacl = std::ptr::null_mut();
    let mut full = true;
    let mut status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            BACKUP_SECURITY_INFORMATION
                | OWNER_SECURITY_INFORMATION
                | GROUP_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | SACL_SECURITY_INFORMATION
                | LABEL_SECURITY_INFORMATION
                | ATTRIBUTE_SECURITY_INFORMATION
                | SCOPE_SECURITY_INFORMATION,
            &mut owner,
            &mut group,
            &mut dacl,
            &mut sacl,
            &mut descriptor,
        )
    };
    if status != 0 {
        // No AdjustTokenPrivileges or elevation. READ_CONTROL can still expose
        // known label/resource metadata, but absence of full audit SACL proof
        // makes both replacement and deletion/recreation explicitly read-only.
        full = false;
        owner = std::ptr::null_mut();
        group = std::ptr::null_mut();
        dacl = std::ptr::null_mut();
        sacl = std::ptr::null_mut();
        descriptor = std::ptr::null_mut();
        status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | GROUP_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | LABEL_SECURITY_INFORMATION
                    | ATTRIBUTE_SECURITY_INFORMATION
                    | SCOPE_SECURITY_INFORMATION,
                &mut owner,
                &mut group,
                &mut dacl,
                &mut sacl,
                &mut descriptor,
            )
        };
    }
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32).into());
    }
    struct Allocation(*mut std::ffi::c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let _owned = Allocation(descriptor);
    fn copy_sid(sid: PSID) -> Result<Vec<u8>> {
        if sid.is_null() {
            bail!("Windows file has no recorded owner/group");
        }
        let len = unsafe { GetLengthSid(sid) } as usize;
        if !(8..=68).contains(&len) {
            bail!("Windows security SID length exceeds its bound");
        }
        Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) }.to_vec())
    }
    fn copy_acl(acl: *mut ACL) -> Result<Option<Vec<u8>>> {
        if acl.is_null() {
            Ok(None)
        } else {
            let size = unsafe { (*acl).AclSize } as usize;
            if !(8..=16384).contains(&size) {
                bail!("Windows ACL exceeds the interactive metadata limit");
            }
            let bytes = unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), size) };
            let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
            let mut used = 8;
            for _ in 0..count {
                if used + 4 > bytes.len() {
                    bail!("Truncated native ACL");
                }
                let len = u16::from_le_bytes([bytes[used + 2], bytes[used + 3]]) as usize;
                if len < 4 || used + len > bytes.len() {
                    bail!("Invalid native ACL size");
                }
                used += len;
            }
            let mut minimal = bytes[..used].to_vec();
            minimal[2..4].copy_from_slice(&(used as u16).to_le_bytes());
            Ok(Some(minimal))
        }
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if control & SE_DACL_PRESENT == 0 {
        bail!("Absent Windows DACL metadata cannot be preserved exactly");
    }
    let permissions = Permissions {
        owner: copy_sid(owner)?,
        group: copy_sid(group)?,
        dacl: copy_acl(dacl)?,
        sacl: copy_acl(sacl)?,
        sacl_protected: control & SE_SACL_PROTECTED != 0,
        full_sacl: full,
        protected: control & SE_DACL_PROTECTED != 0,
    };
    permissions.validate()?;
    let bytes = serde_json::to_vec(&permissions)?;
    if bytes.len() > 65536 {
        bail!("Windows security metadata exceeds review limit");
    }
    Ok((bytes, full))
}
fn apply_security(file: &File, permissions: &Permissions) -> Result<()> {
    use windows_sys::Win32::Security::{Authorization::*, *};
    permissions.validate()?;
    if !permissions.full_sacl {
        bail!("Full Windows audit/SACL metadata was not captured");
    }
    fn aligned(bytes: &[u8]) -> Vec<u32> {
        let mut words = vec![0u32; bytes.len().div_ceil(4)];
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                words.as_mut_ptr().cast::<u8>(),
                bytes.len(),
            );
        }
        words
    }
    let owner = aligned(&permissions.owner);
    let group = aligned(&permissions.group);
    let acl = permissions.dacl.as_ref().map(|bytes| aligned(bytes));
    let sacl = permissions.sacl.as_ref().map(|bytes| aligned(bytes));
    let flags = SACL_SECURITY_INFORMATION
        | LABEL_SECURITY_INFORMATION
        | ATTRIBUTE_SECURITY_INFORMATION
        | SCOPE_SECURITY_INFORMATION
        | if permissions.sacl_protected {
            PROTECTED_SACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_SACL_SECURITY_INFORMATION
        }
        | OWNER_SECURITY_INFORMATION
        | GROUP_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | if permissions.protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
    // SID/ACL component sizes and alignment are verified above. All backing
    // buffers remain live for this synchronous native metadata operation.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            flags,
            owner.as_ptr().cast_mut().cast(),
            group.as_ptr().cast_mut().cast(),
            acl.as_ref().map_or(std::ptr::null_mut(), |words| {
                words.as_ptr().cast_mut().cast()
            }),
            sacl.as_ref()
                .map_or(std::ptr::null(), |words| words.as_ptr().cast()),
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("fl-review-win-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn native_windows_metadata_is_preserved_or_unverifiable_state_refuses_all_writes() {
        let root = Fixture::new();
        let path = root.0.join("owned.txt");
        std::fs::write(&path, b"current\r\nEOF").unwrap();
        let policy = PathPolicy::default();
        let access = Access::open(&path).unwrap();
        let before = access.read(&path, &policy).unwrap();
        if !before.security_complete {
            let mut desired = before.clone();
            desired.bytes = Some(b"must-not-write".to_vec());
            assert!(access.replace(&path, &before, &desired, &policy).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"current\r\nEOF");
            return;
        }
        let mut after = before.clone();
        after.bytes = Some(b"review inverse\r\nEOF".to_vec());
        access.replace(&path, &before, &after, &policy).unwrap();
        assert_eq!(access.read(&path, &policy).unwrap(), after);
        access.replace(&path, &after, &before, &policy).unwrap();
        assert_eq!(access.read(&path, &policy).unwrap(), before);
        access
            .replace(&path, &before, &ReviewImage::missing(), &policy)
            .unwrap();
        assert!(!path.exists());
        access
            .replace(&path, &ReviewImage::missing(), &before, &policy)
            .unwrap();
        assert_eq!(access.read(&path, &policy).unwrap(), before);
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    }
    #[test]
    fn malformed_windows_components_never_normalize_to_another_target() {
        let root = Fixture::new();
        let policy = PathPolicy::default();
        let path = root.0.join("owned.txt");
        std::fs::write(&path, b"unchanged").unwrap();
        let malformed = [
            r"C:relative.txt".to_string(),
            format!("{}\\missing/../owned.txt", root.0.display()),
            format!("{}\\C:stream", root.0.display()),
        ];
        for raw in malformed {
            assert!(Access::open(Path::new(&raw)).is_err(), "{raw}");
        }
        assert_eq!(
            Access::open(&path)
                .unwrap()
                .read(&path, &policy)
                .unwrap()
                .bytes
                .unwrap(),
            b"unchanged"
        );
    }
}
