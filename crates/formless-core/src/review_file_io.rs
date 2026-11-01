//! Owned-path, no-follow review I/O. These operations are bounded and use an
//! anchored parent directory. They do not claim a filesystem transaction against
//! a noncooperating external writer racing the final content check.
use crate::hunk_review::{image_hash, MAX_REVIEW_BYTES};
use anyhow::{bail, Context, Result};
use formless_runtime::sensitive_paths::{PathDecision, PathPolicy};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Durable identity is a comparison value, never authority to reopen an object.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowsIdentity {
    pub volume: u64,
    pub file: [u8; 16],
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct ReviewImage {
    pub bytes: Option<Vec<u8>>,
    pub mode: Option<u32>,
    pub readonly: bool,
    pub owner: Option<u32>,
    pub group: Option<u32>,
    pub security: Option<Vec<u8>>,
    pub security_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_identity: Option<WindowsIdentity>,
    // Runtime capability is intentionally absent from receipt equality/checksums.
    // A decoded identity alone can never authorize byte-only Windows writes.
    #[cfg(windows)]
    #[serde(skip)]
    pub witness: windows::Witness,
}
impl ReviewImage {
    pub fn write_restriction(&self) -> Option<&'static str> {
        if self.bytes.is_none() {
            return None;
        }
        if !self.security_complete {
            return Some("Full security metadata (including Windows audit/SACL state) cannot be verified without changing privileges; file writes are refused");
        }
        self.ordinary_write_restriction()
    }
    fn ordinary_write_restriction(&self) -> Option<&'static str> {
        if self.readonly {
            return Some("Read-only files are not hunk-editable");
        }
        #[cfg(unix)]
        if self.mode.is_some_and(|mode| mode & 0o6000 != 0) {
            return Some("Set-ID files are not hunk-editable");
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Storage::FileSystem::*;
            let supported = FILE_ATTRIBUTE_ARCHIVE
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_NORMAL
                | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
                | FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_READONLY
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_TEMPORARY;
            if self.mode.is_none_or(|mode| mode & !supported != 0) {
                return Some(
                    "Special Windows file attributes cannot be preserved by interactive review",
                );
            }
        }
        None
    }
    /// Existing-object byte mutation has no security-metadata restoration step.
    /// Opaque SACL state remains on the same object, guarded by a live witness.
    pub fn existing_write_restriction(&self) -> Option<&'static str> {
        #[cfg(windows)]
        if self.bytes.is_some() && self.witness.matches(self.windows_identity.as_ref()) {
            return self.ordinary_write_restriction();
        }
        self.write_restriction()
    }
    #[cfg(windows)]
    pub fn attach_witness(&mut self, live: &Self) -> Result<()> {
        if self.windows_identity.is_none()
            || self.windows_identity != live.windows_identity
            || !live.witness.matches(live.windows_identity.as_ref())
        {
            bail!("Windows review no longer owns the exact retained file object");
        }
        self.witness = live.witness.clone();
        Ok(())
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes.as_ref().map_or(0, Vec::len) + self.security.as_ref().map_or(0, Vec::len)
    }
    pub fn hash(&self) -> String {
        image_hash(self.bytes.as_deref())
    }
    pub fn integrity_hash(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self)?;
        let mut hash = Sha256::new();
        hash.update(b"formless-review-image-v1\0");
        hash.update(bytes);
        Ok(format!("{:x}", hash.finalize()))
    }
    pub fn missing() -> Self {
        Self {
            bytes: None,
            mode: None,
            readonly: false,
            security_complete: true,
            ..Default::default()
        }
    }
}

/// Versioned before-metadata contains no duplicate body. Its integrity binds
/// the original bytes AND all metadata; corruption is not a source of authority.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BeforeMetadata {
    version: u8,
    image: ReviewImage,
    security_b64: Option<String>,
    integrity: String,
}
pub(crate) fn encode_before_metadata(image: &ReviewImage) -> Result<String> {
    use base64::Engine;
    if image
        .security
        .as_ref()
        .is_some_and(|security| security.len() > 64 * 1024)
    {
        bail!("Security metadata exceeds its snapshot limit");
    }
    let integrity = image.integrity_hash()?;
    let mut metadata = image.clone();
    metadata.bytes = None;
    let security_b64 = metadata
        .security
        .take()
        .map(|security| base64::engine::general_purpose::STANDARD.encode(security));
    Ok(serde_json::to_string(&BeforeMetadata {
        version: 1,
        image: metadata,
        security_b64,
        integrity,
    })?)
}
pub(crate) fn decode_before_metadata(encoded: &str, bytes: Option<&[u8]>) -> Result<ReviewImage> {
    if encoded.len() > 128 * 1024 {
        bail!("Recorded before-metadata exceeds its bound");
    }
    let record: BeforeMetadata = serde_json::from_str(encoded)?;
    if record.version != 1 || record.image.bytes.is_some() || record.image.security.is_some() {
        bail!("Invalid before-metadata version or body");
    }
    use base64::Engine;
    let mut image = record.image;
    image.security = record
        .security_b64
        .map(|security| base64::engine::general_purpose::STANDARD.decode(security))
        .transpose()?;
    if image
        .security
        .as_ref()
        .is_some_and(|security| security.len() > 64 * 1024)
    {
        bail!("Security metadata exceeds its snapshot limit");
    }
    image.bytes = bytes.map(<[u8]>::to_vec);
    if image.integrity_hash()? != record.integrity {
        bail!("Recorded before-metadata does not match its original image");
    }
    Ok(image)
}

/// Anchors are opened before any body is read. Callers must still prove the
/// path came from an owned Run snapshot; this type provides no client-path API.
pub(crate) struct ReviewPath {
    path: PathBuf,
    #[cfg(unix)]
    parent: std::fs::File,
    #[cfg(unix)]
    leaf: std::ffi::CString,
    #[cfg(windows)]
    native: windows::Access,
}
impl ReviewPath {
    pub(crate) fn open(path: &Path, policy: &PathPolicy) -> Result<Self> {
        if !path.is_absolute() || path.to_str().is_none_or(|p| p.contains('\u{fffd}')) {
            bail!("Review requires an absolute lossless recorded path");
        }
        if !matches!(
            policy.classify(path, path).map_err(anyhow::Error::msg)?,
            PathDecision::Ordinary
        ) {
            bail!("Protected profile or credential path is not reviewable");
        }
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            use std::os::unix::ffi::OsStrExt;
            let mut parts = path.components();
            if !matches!(parts.next(), Some(Component::RootDir)) {
                bail!("Review requires a native absolute path");
            }
            let mut names = Vec::new();
            for part in parts {
                let Component::Normal(name) = part else {
                    bail!("Unresolved path components are not reviewable");
                };
                names.push(std::ffi::CString::new(name.as_bytes())?);
            }
            let leaf = names.pop().context("A filesystem root is not reviewable")?;
            let root = c"/";
            // SAFETY: valid NUL-terminated native name and read-only/no-follow
            // directory flags. Successful descriptor ownership transfers to File.
            let fd = unsafe {
                libc::open(
                    root.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut parent = unsafe { std::fs::File::from_raw_fd(fd) };
            for name in names {
                let fd = unsafe {
                    libc::openat(
                        parent.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                parent = unsafe { std::fs::File::from_raw_fd(fd) };
            }
            Ok(Self {
                path: path.into(),
                parent,
                leaf,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                path: path.into(),
                native: windows::Access::open(path)?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = policy;
            bail!("Anchored review filesystem operations are unsupported on this platform");
        }
    }
    fn recheck_policy(&self, policy: &PathPolicy) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            #[cfg(target_os = "linux")]
            let parent_path =
                std::fs::read_link(format!("/proc/self/fd/{}", self.parent.as_raw_fd()))?;
            #[cfg(target_os = "macos")]
            let parent_path = {
                use std::os::unix::ffi::OsStrExt;
                let mut bytes = vec![0u8; libc::PATH_MAX as usize];
                // SAFETY: live borrowed descriptor and a PATH_MAX-sized writable
                // native F_GETPATH buffer. Read only a successful NUL-terminated result.
                if unsafe {
                    libc::fcntl(self.parent.as_raw_fd(), libc::F_GETPATH, bytes.as_mut_ptr())
                } < 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                let length = bytes
                    .iter()
                    .position(|&b| b == 0)
                    .context("Unterminated directory identity")?;
                PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..length]))
            };
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let parent_path = {
                use std::os::unix::fs::MetadataExt;
                let named = self.path.parent().context("no parent")?.canonicalize()?;
                let a = std::fs::metadata(&named)?;
                let b = self.parent.metadata()?;
                if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                    bail!("Review parent directory changed");
                }
                named
            };
            let actual = parent_path.join(self.path.file_name().context("no recorded filename")?);
            if !matches!(
                policy
                    .classify(&self.path, &actual)
                    .map_err(anyhow::Error::msg)?,
                PathDecision::Ordinary
            ) {
                bail!("Review destination is now a protected profile or credential path");
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            self.native.recheck(&self.path, policy)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = policy;
            bail!("Anchored review is unsupported on this platform");
        }
    }
    pub(crate) fn read(&self, policy: &PathPolicy) -> Result<ReviewImage> {
        self.read_bounded(policy, MAX_REVIEW_BYTES, false, None)
    }
    /// Snapshot bytes and metadata from the SAME opened no-follow descriptor.
    /// Unsupported metadata is recorded as incomplete, never invented.
    #[cfg(test)]
    pub(crate) fn snapshot(&self, policy: &PathPolicy) -> Result<ReviewImage> {
        self.read_bounded(policy, 2 * 1024 * 1024, true, None)
    }
    /// Compatibility byte capture for an authorized regular path through a
    /// canonical alias. Only the canonical target's descriptor supplies bytes;
    /// the requested route must still name that identity before and after read.
    /// This is not an atomic transaction with the later Tool operation.
    pub(crate) fn capture_snapshot(path: &Path, policy: &PathPolicy) -> Result<ReviewImage> {
        if !path.is_absolute() {
            bail!("Snapshot requires an absolute recorded path");
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            bail!("Snapshot requires a regular final entry, not a final symlink");
        }
        let actual = path.canonicalize()?;
        if !matches!(
            policy.classify(path, &actual).map_err(anyhow::Error::msg)?,
            PathDecision::Ordinary
        ) {
            bail!("Protected snapshot alias or target");
        }
        let access = Self::open(&actual, policy)?;
        let mut image = access.read_bounded(policy, 2 * 1024 * 1024, true, Some(path))?;
        if path != actual {
            image.security_complete = false;
        }
        Ok(image)
    }
    fn read_bounded(
        &self,
        policy: &PathPolicy,
        limit: usize,
        capture: bool,
        alias: Option<&Path>,
    ) -> Result<ReviewImage> {
        self.recheck_policy(policy)?;
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let fd = unsafe {
                libc::openat(
                    self.parent.as_raw_fd(),
                    self.leaf.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound && alias.is_none() {
                    return Ok(ReviewImage::missing());
                }
                return Err(error.into());
            }
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            let metadata = file.metadata()?;
            if !metadata.is_file() || (!capture && metadata.nlink() != 1) {
                bail!("Only regular files with a single hard link are hunk-editable");
            }
            if let Some(alias) = alias {
                self.verify_snapshot_alias(&file, alias, policy)?;
            }
            if metadata.len() > limit as u64 {
                bail!("File exceeds its {limit}-byte snapshot/review limit");
            }
            let extended_complete = match reject_extended_metadata(&file) {
                Ok(()) => true,
                Err(_) if capture => false,
                Err(error) => return Err(error),
            };
            self.recheck_policy(policy)?;
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            (&file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                bail!("Current file grew beyond the review limit");
            }
            let after = file.metadata()?;
            if metadata.len() != after.len()
                || metadata.modified()? != after.modified()?
                || metadata.mode() != after.mode()
                || metadata.uid() != after.uid()
                || metadata.gid() != after.gid()
                || metadata.ctime() != after.ctime()
                || metadata.ctime_nsec() != after.ctime_nsec()
                || metadata.nlink() != after.nlink()
                || extended_complete != reject_extended_metadata(&file).is_ok()
                || bytes.len() as u64 != after.len()
            {
                bail!("Current file changed while reading; inspect again");
            }
            if let Some(alias) = alias {
                self.verify_snapshot_alias(&file, alias, policy)?;
            }
            let security_complete = extended_complete && metadata.nlink() == 1;
            Ok(ReviewImage {
                bytes: Some(bytes),
                mode: Some(metadata.permissions().mode()),
                readonly: metadata.permissions().readonly(),
                owner: Some(metadata.uid()),
                group: Some(metadata.gid()),
                security: None,
                security_complete,
                windows_identity: None,
            })
        }
        #[cfg(windows)]
        {
            self.native
                .read_bounded(&self.path, policy, limit, capture, alias)
        }
        #[cfg(not(any(unix, windows)))]
        {
            bail!("Anchored review is unsupported on this platform");
        }
    }
    #[cfg(unix)]
    fn verify_snapshot_alias(
        &self,
        file: &std::fs::File,
        alias: &Path,
        policy: &PathPolicy,
    ) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let expected = file.metadata()?;
        let requested = std::fs::symlink_metadata(alias)?;
        let actual = alias.canonicalize()?;
        if !requested.is_file()
            || (requested.dev(), requested.ino()) != (expected.dev(), expected.ino())
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
    /// A durable pending receipt must exist BEFORE calling this function. Errors
    /// before rename leave the target untouched. Once the atomic namespace change
    /// succeeds, later fsync failure is a warning, never a false unchanged claim.
    pub(crate) fn replace(
        &self,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
    ) -> Result<Vec<String>> {
        self.recheck_policy(policy)?;
        if desired
            .bytes
            .as_ref()
            .is_some_and(|b| b.len() > MAX_REVIEW_BYTES)
        {
            bail!("Review replacement exceeds its byte limit");
        }
        let current = self.read(policy)?;
        if current != *expected {
            bail!("File content, existence or mode changed; inspect again");
        }
        #[cfg(windows)]
        if expected.bytes.is_none() || desired.bytes.is_none() {
            bail!("Windows existence-changing review requires retained-object recovery");
        }
        #[cfg(windows)]
        if expected.bytes.is_some() && desired.bytes.is_some() {
            return self
                .native
                .replace_existing(&self.path, expected, desired, policy);
        }
        if let Some(reason) = current
            .write_restriction()
            .or_else(|| desired.write_restriction())
        {
            bail!(reason);
        }
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            struct Temp<'a> {
                parent: &'a std::fs::File,
                name: std::ffi::CString,
                exists: bool,
            }
            impl Drop for Temp<'_> {
                fn drop(&mut self) {
                    if self.exists {
                        unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) };
                    }
                }
            }
            let mut temp = Temp {
                parent: &self.parent,
                name: std::ffi::CString::new(format!(".formless-review-{}", uuid::Uuid::new_v4()))?,
                exists: false,
            };
            if let Some(bytes) = &desired.bytes {
                let mode = desired
                    .mode
                    .context("Exact prior file mode was not recorded")?;
                let fd = unsafe {
                    libc::openat(
                        self.parent.as_raw_fd(),
                        temp.name.as_ptr(),
                        libc::O_WRONLY
                            | libc::O_CREAT
                            | libc::O_EXCL
                            | libc::O_NOFOLLOW
                            | libc::O_CLOEXEC,
                        0o600,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                temp.exists = true;
                let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
                reject_extended_metadata(&file)?;
                let owner = desired
                    .owner
                    .context("Exact prior file owner was not recorded")?;
                let group = desired
                    .group
                    .context("Exact prior file group was not recorded")?;
                if unsafe {
                    libc::fchown(file.as_raw_fd(), owner as libc::uid_t, group as libc::gid_t)
                } != 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                file.write_all(bytes)?;
                // Do not create privileged executable replacement content.
                if mode & 0o6000 != 0 {
                    bail!("Set-ID files are not hunk-editable");
                }
                if unsafe { libc::fchmod(file.as_raw_fd(), (mode & 0o7777) as libc::mode_t) } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                file.sync_all()?;
            }
            self.recheck_policy(policy)?;
            if self.read(policy)? != *expected {
                bail!("File changed before the review commit; inspect again");
            }
            if expected.mode.is_some_and(|mode| mode & 0o6000 != 0) {
                bail!("Set-ID files are not hunk-editable");
            }
            let create_only = desired.bytes.is_some() && expected.bytes.is_none();
            let outcome = if create_only {
                // Publish an already-flushed regular inode without replacing a
                // file another process created after our final missing check.
                unsafe {
                    libc::linkat(
                        self.parent.as_raw_fd(),
                        temp.name.as_ptr(),
                        self.parent.as_raw_fd(),
                        self.leaf.as_ptr(),
                        0,
                    )
                }
            } else if desired.bytes.is_some() {
                unsafe {
                    libc::renameat(
                        self.parent.as_raw_fd(),
                        temp.name.as_ptr(),
                        self.parent.as_raw_fd(),
                        self.leaf.as_ptr(),
                    )
                }
            } else if expected.bytes.is_some() {
                unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.leaf.as_ptr(), 0) }
            } else {
                0
            };
            if outcome != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut warnings = Vec::new();
            if create_only {
                if unsafe { libc::unlinkat(self.parent.as_raw_fd(), temp.name.as_ptr(), 0) } != 0 {
                    warnings.push("File creation completed, but temporary-link cleanup could not be confirmed.".into());
                } else {
                    temp.exists = false;
                }
            } else {
                temp.exists = false;
            }
            if let Err(error) = self.parent.sync_all() {
                warnings.push(format!("File replacement completed, but parent-directory durability could not be confirmed: {error}"));
            }
            Ok(warnings)
        }
        #[cfg(windows)]
        {
            self.native.replace(&self.path, expected, desired, policy)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = desired;
            bail!("Anchored review is unsupported on this platform");
        }
    }
}

#[cfg(unix)]
fn reject_extended_metadata(file: &std::fs::File) -> Result<()> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "linux")]
    let size = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0) };
    #[cfg(target_os = "macos")]
    let size = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0, 0) };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        bail!("Extended permission metadata cannot be verified on this Unix platform");
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if size < 0 {
            return Err(anyhow::Error::new(std::io::Error::last_os_error())
                .context("Extended file metadata could not be verified"));
        }
        if size > 0 {
            bail!("Extended attributes or ACLs cannot be preserved by interactive review; file is read-only in this view");
        }
    }
    #[cfg(target_os = "macos")]
    macos_acl::require_empty(file)?;
    Ok(())
}
#[cfg(target_os = "macos")]
mod macos_acl {
    use super::*;
    use std::os::fd::AsRawFd;
    // Darwin ABI/constants (ACL_FIRST_ENTRY=0 and success=0 differ from
    // Linux libacl). Apple's implementations return ENOENT for absent ACL
    // property, and EINVAL when a valid empty ACL has no first entry:
    // https://github.com/apple-oss-distributions/Libc/blob/main/include/sys/acl.h
    // https://github.com/apple-oss-distributions/Libc/blob/main/posix1e/acl_file.c
    // https://github.com/apple-oss-distributions/Libc/blob/main/posix1e/acl_entry.c
    // https://github.com/apple-oss-distributions/Libc/blob/main/gen/filesec.c
    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            id: libc::c_int,
            out: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    pub(super) fn require_empty(file: &std::fs::File) -> Result<()> {
        unsafe {
            *libc::__error() = 0;
        }
        let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(());
            }
            return Err(
                anyhow::Error::new(error).context("Extended ACL state could not be verified")
            );
        }
        if (acl as usize) < 4096 || (acl as usize) > usize::MAX - 4096 {
            bail!("Unexpected native ACL sentinel; automatic review write is unavailable");
        }
        struct Acl(*mut libc::c_void);
        impl Drop for Acl {
            fn drop(&mut self) {
                unsafe { acl_free(self.0) };
            }
        }
        let _owned = Acl(acl);
        let mut entry = std::ptr::null_mut();
        let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
        if result == 0 {
            bail!("Extended ACLs cannot be preserved by interactive review; file is read-only in this view");
        }
        let error = std::io::Error::last_os_error();
        if result == -1 && error.raw_os_error() == Some(libc::EINVAL) {
            return Ok(());
        }
        Err(error.into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{symlink, PermissionsExt},
    };
    pub(super) struct Fixture(pub(super) PathBuf);
    impl Fixture {
        pub(super) fn new() -> Self {
            let raw = std::env::temp_dir().join(format!("fl-review-io-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&raw).unwrap();
            Self(raw.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn atomic_reject_and_exact_undo_preserve_bytes_existence_and_mode() {
        let dir = Fixture::new();
        let path = dir.0.join("owned.txt");
        std::fs::write(&path, b"before\r\nEOF").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        let mut after = before.clone();
        after.bytes = Some(b"rejected inverse\r\nEOF".to_vec());
        assert!(access.replace(&before, &after, &policy).unwrap().is_empty());
        assert_eq!(access.read(&policy).unwrap(), after);
        assert!(access.replace(&after, &before, &policy).unwrap().is_empty());
        assert_eq!(access.read(&policy).unwrap(), before);
        assert!(access
            .replace(&before, &ReviewImage::missing(), &policy)
            .unwrap()
            .is_empty());
        assert!(!path.exists());
        assert!(access
            .replace(&ReviewImage::missing(), &before, &policy)
            .unwrap()
            .is_empty());
        assert_eq!(access.read(&policy).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(&dir.0).unwrap().count(),
            1,
            "temporary files removed after every transition"
        );
    }
    #[test]
    fn stale_content_or_mode_and_unknown_creation_mode_never_replace_target() {
        let dir = Fixture::new();
        let path = dir.0.join("owned.txt");
        std::fs::write(&path, b"current").unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let original = access.read(&policy).unwrap();
        let mut desired = original.clone();
        desired.bytes = Some(b"replacement".to_vec());
        std::fs::write(&path, b"outside edit").unwrap();
        assert!(access.replace(&original, &desired, &policy).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"outside edit");
        std::fs::write(&path, b"current").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(access.replace(&original, &desired, &policy).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"current");
        let missing = ReviewPath::open(&dir.0.join("missing.txt"), &policy).unwrap();
        assert!(missing
            .replace(
                &ReviewImage::missing(),
                &ReviewImage {
                    bytes: Some(b"must not create".to_vec()),
                    mode: None,
                    readonly: false,
                    ..Default::default()
                },
                &policy
            )
            .is_err());
        assert!(!dir.0.join("missing.txt").exists());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }
    #[test]
    fn protected_paths_symlink_parents_leaves_hardlinks_and_nonregular_files_are_refused() {
        let dir = Fixture::new();
        let protected = dir.0.join("profile");
        std::fs::create_dir(&protected).unwrap();
        let file = protected.join("private.txt");
        std::fs::write(&file, b"private fixture only").unwrap();
        let policy = PathPolicy::new(vec![protected.clone()], vec![]).unwrap();
        assert!(ReviewPath::open(&file, &policy)
            .err()
            .unwrap()
            .to_string()
            .contains("Protected profile"));
        let leaf = dir.0.join("alias.txt");
        symlink(&file, &leaf).unwrap();
        assert!(ReviewPath::open(&leaf, &policy)
            .unwrap()
            .read(&policy)
            .is_err());
        let parent = dir.0.join("alias-dir");
        symlink(&protected, &parent).unwrap();
        assert!(ReviewPath::open(&parent.join("private.txt"), &policy).is_err());
        let hard = dir.0.join("hard.txt");
        std::fs::hard_link(&file, &hard).unwrap();
        assert!(ReviewPath::open(&hard, &policy)
            .unwrap()
            .read(&policy)
            .is_err());
        let fifo = dir.0.join("owned-fifo");
        let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
        assert!(
            ReviewPath::open(&fifo, &policy)
                .unwrap()
                .read(&policy)
                .is_err(),
            "FIFO never waits for a writer"
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"private fixture only");
    }
    #[test]
    fn parent_move_into_protected_namespace_is_rechecked_before_body_or_write() {
        let dir = Fixture::new();
        let source = dir.0.join("ordinary");
        let protected = dir.0.join("profile");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&protected).unwrap();
        let path = source.join("owned.txt");
        std::fs::write(&path, b"owned bytes").unwrap();
        let policy = PathPolicy::new(vec![protected.clone()], vec![]).unwrap();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        std::fs::rename(&source, protected.join("moved")).unwrap();
        assert!(access.read(&policy).is_err());
        assert!(access
            .replace(&before, &ReviewImage::missing(), &policy)
            .is_err());
        assert_eq!(
            std::fs::read(protected.join("moved/owned.txt")).unwrap(),
            b"owned bytes"
        );
    }
    #[test]
    fn readonly_setid_and_oversize_files_leave_exact_original_content() {
        let dir = Fixture::new();
        let path = dir.0.join("owned.txt");
        std::fs::write(&path, b"exact").unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        for mode in [0o444, 0o4755] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let before = access.read(&policy).unwrap();
            assert!(access
                .replace(&before, &ReviewImage::missing(), &policy)
                .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"exact");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, vec![b'x'; MAX_REVIEW_BYTES + 1]).unwrap();
        assert!(access.read(&policy).is_err());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            MAX_REVIEW_BYTES as u64 + 1
        );
    }
}

#[cfg(all(test, unix))]
mod metadata_tests {
    use super::*;
    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
    #[test]
    fn exact_uid_gid_and_mode_survive_replacement_and_undo() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("owner.txt");
        std::fs::write(&path, b"original\r\n").unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(before.owner, Some(metadata.uid()));
        assert_eq!(before.group, Some(metadata.gid()));
        let mut after = before.clone();
        after.bytes = Some(b"changed\r\n".to_vec());
        access.replace(&before, &after, &policy).unwrap();
        let changed = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (changed.uid(), changed.gid(), changed.mode()),
            (metadata.uid(), metadata.gid(), metadata.mode())
        );
        access.replace(&after, &before, &policy).unwrap();
        assert_eq!(access.read(&policy).unwrap(), before);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn actual_extended_attribute_refuses_review_without_changing_content_or_metadata() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("tagged.txt");
        std::fs::write(&path, b"exact tagged bytes").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let name = c"user.formless_review_fixture";
        let value = b"private metadata";
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(
            result,
            0,
            "owned test filesystem must support the explicit xattr fixture: {}",
            std::io::Error::last_os_error()
        );
        let policy = PathPolicy::default();
        let error = ReviewPath::open(&path, &policy)
            .unwrap()
            .read(&policy)
            .unwrap_err();
        assert!(error.to_string().contains("Extended attributes"));
        let snapshot = ReviewPath::open(&path, &policy)
            .unwrap()
            .snapshot(&policy)
            .unwrap();
        assert_eq!(
            snapshot.bytes.as_deref(),
            Some(b"exact tagged bytes".as_slice())
        );
        assert!(
            !snapshot.security_complete,
            "capture must retain an explicit unsupported-metadata flag"
        );
        assert!(snapshot.write_restriction().is_some());
        let encoded = encode_before_metadata(&snapshot).unwrap();
        let decoded = decode_before_metadata(&encoded, snapshot.bytes.as_deref()).unwrap();
        assert_eq!(decoded, snapshot);
        assert!(decode_before_metadata(&encoded, Some(b"different bytes")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"exact tagged bytes");
        let mut actual = vec![0u8; value.len()];
        #[cfg(target_os = "linux")]
        let size = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
            )
        };
        #[cfg(target_os = "macos")]
        let size = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
                0,
                0,
            )
        };
        assert_eq!(size, value.len() as isize);
        assert_eq!(actual, value);
    }
}

#[cfg(windows)]
#[path = "review_file_windows.rs"]
mod windows;

#[cfg(all(test, unix))]
mod snapshot_tests {
    use super::*;
    #[test]
    fn whole_run_snapshot_keeps_existing_two_mib_cap_without_expanding_interactive_cap() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("large.txt");
        let body = vec![b'x'; MAX_REVIEW_BYTES + 1];
        std::fs::write(&path, &body).unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        assert!(access.read(&policy).is_err());
        let snapshot = access.snapshot(&policy).unwrap();
        assert_eq!(snapshot.bytes.as_deref(), Some(body.as_slice()));
        let encoded = encode_before_metadata(&snapshot).unwrap();
        assert!(encoded.len() < 1024, "metadata must not duplicate the body");
        assert_eq!(
            decode_before_metadata(&encoded, Some(&body)).unwrap(),
            snapshot
        );
        std::fs::write(&path, vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();
        assert!(access.snapshot(&policy).is_err());
    }
}

#[cfg(all(test, unix))]
mod compatibility_capture_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn parent_alias_retarget_after_anchor_fails_identity_before_copying_bytes() {
        let dir = super::tests::Fixture::new();
        let original = dir.0.join("original");
        let replacement = dir.0.join("replacement");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(original.join("body.txt"), b"old exact\r\n").unwrap();
        std::fs::write(replacement.join("body.txt"), b"replacement private fixture").unwrap();
        let alias = dir.0.join("alias");
        symlink(&original, &alias).unwrap();
        let requested = alias.join("body.txt");
        let policy = PathPolicy::default();
        let actual = requested.canonicalize().unwrap();
        let access = ReviewPath::open(&actual, &policy).unwrap();
        std::fs::remove_file(&alias).unwrap();
        symlink(&replacement, &alias).unwrap();
        assert!(access
            .read_bounded(&policy, 2 * 1024 * 1024, true, Some(&requested))
            .is_err());
        assert_eq!(
            std::fs::read(original.join("body.txt")).unwrap(),
            b"old exact\r\n"
        );
        assert_eq!(
            std::fs::read(replacement.join("body.txt")).unwrap(),
            b"replacement private fixture"
        );
    }
    #[test]
    fn final_symlink_is_not_new_snapshot_read_authority_and_missing_alias_is_unknown() {
        let dir = super::tests::Fixture::new();
        let target = dir.0.join("body.txt");
        std::fs::write(&target, b"exact").unwrap();
        let final_link = dir.0.join("link.txt");
        symlink(&target, &final_link).unwrap();
        let policy = PathPolicy::default();
        assert!(ReviewPath::capture_snapshot(&final_link, &policy).is_err());
        let access = ReviewPath::open(&target, &policy).unwrap();
        std::fs::remove_file(&target).unwrap();
        assert!(
            access
                .read_bounded(&policy, 2 * 1024 * 1024, true, Some(&target))
                .is_err(),
            "a failed known-existing alias read cannot turn into known absence"
        );
        assert!(!target.exists());
        assert!(std::fs::symlink_metadata(final_link)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[cfg(test)]
mod windows_identity_compatibility_tests {
    use super::*;
    #[test]
    fn legacy_image_serialization_omits_new_identity_and_binds_new_identity() {
        let legacy = r#"{"bytes":[97],"mode":420,"readonly":false,"owner":null,"group":null,"security":null,"security_complete":true}"#;
        let image: ReviewImage = serde_json::from_str(legacy).unwrap();
        assert!(image.windows_identity.is_none());
        assert_eq!(serde_json::to_string(&image).unwrap(), legacy);
        let old_hash = image.integrity_hash().unwrap();
        let mut identified = image;
        identified.windows_identity = Some(WindowsIdentity {
            volume: 17,
            file: [3; 16],
        });
        assert_ne!(identified.integrity_hash().unwrap(), old_hash);
        let metadata = encode_before_metadata(&identified).unwrap();
        assert_eq!(
            decode_before_metadata(&metadata, Some(b"a")).unwrap(),
            identified
        );
        let tampered = metadata.replace("\"volume\":17", "\"volume\":18");
        assert_ne!(tampered, metadata);
        assert!(decode_before_metadata(&tampered, Some(b"a")).is_err());
    }
}
