//! Owned-path, no-follow review I/O. These operations are bounded and use an
//! anchored parent directory. They do not claim a filesystem transaction against
//! a noncooperating external writer racing the final content check.
//! Unix `user.*` attributes are restored only on a private stage.
//! `com.apple.provenance` is compared and never written or removed.
//! The stage is checked for an empty ACL, zero BSD flags, and no unexpected
//! attributes before chown or write. Provenance is compared, never adopted.
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
#[serde(deny_unknown_fields)]
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
    /// Exact Unix attribute map. `None` is verified-empty, including legacy
    /// images. It does not mean the kernel may add a stamp and still match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_metadata: Option<crate::review_unix_metadata::UnixMetadata>,
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
    pub(crate) fn ordinary_write_restriction(&self) -> Option<&'static str> {
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
    pub(crate) fn ensure_consistent(&self) -> Result<()> {
        if self.bytes.is_none()
            && (self.mode.is_some()
                || self.readonly
                || self.owner.is_some()
                || self.group.is_some()
                || self.security.is_some()
                || self.windows_identity.is_some()
                || self.unix_metadata.is_some())
        {
            bail!("Missing review image contains invalid mode metadata");
        }
        if self.security.is_some() && self.unix_metadata.is_some() {
            bail!("Unix extended metadata cannot reuse the Windows security blob");
        }
        if let Some(metadata) = &self.unix_metadata {
            metadata.validate_current()?;
        }
        Ok(())
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes.as_ref().map_or(0, Vec::len)
            + self.security.as_ref().map_or(0, Vec::len)
            + self
                .unix_metadata
                .as_ref()
                .map_or(0, |metadata| metadata.retained_bytes())
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
            unix_metadata: None,
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
    image.ensure_consistent()?;
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
    let encoded = serde_json::to_string(&BeforeMetadata {
        version: 1,
        image: metadata,
        security_b64,
        integrity,
    })?;
    if encoded.len() > 128 * 1024 {
        bail!("Recorded before-metadata exceeds its bound");
    }
    Ok(encoded)
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
    image.ensure_consistent()?;
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
    pub(crate) fn read_whole_file(&self, policy: &PathPolicy) -> Result<ReviewImage> {
        self.read_bounded(policy, 2 * 1024 * 1024, false, None)
    }
    pub(crate) fn read(&self, policy: &PathPolicy) -> Result<ReviewImage> {
        self.read_bounded(policy, MAX_REVIEW_BYTES, false, None)
    }
    #[cfg(windows)]
    pub(crate) fn read_tracked(
        &self,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<ReviewImage> {
        self.native.read_bounded(
            &self.path,
            policy,
            MAX_REVIEW_BYTES,
            false,
            None,
            Some(store),
        )
    }
    #[cfg(windows)]
    pub(crate) fn replace_tracked(
        &self,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<Vec<String>> {
        if self.read_tracked(policy, store)? != *expected {
            bail!("Tracked review image changed");
        }
        self.native
            .replace_existing_tracked(&self.path, expected, desired, policy, Some(store))
    }
    #[cfg(windows)]
    pub(crate) fn namespace_target_matches(
        &self,
        expected: &WindowsIdentity,
        policy: &PathPolicy,
    ) -> Result<bool> {
        self.native
            .namespace_target_matches(&self.path, expected, policy)
    }
    #[cfg(windows)]
    pub(crate) fn namespace_source(
        &self,
        expected: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<std::fs::File> {
        self.native
            .namespace_source(&self.path, expected, policy, store)
    }
    #[cfg(windows)]
    pub(crate) fn restore_retained(
        &self,
        object: &crate::review_retained::RetainedObject,
        desired: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<Vec<String>> {
        self.native
            .restore_retained(&self.path, object, desired, policy, store)
    }
    #[cfg(windows)]
    pub(crate) fn replace_whole_file_tracked(
        &self,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<Vec<String>> {
        if self.read_whole_file_tracked(policy, store)? != *expected {
            bail!("Changed since the Run");
        }
        self.native.replace_existing_bounded(
            &self.path,
            expected,
            desired,
            policy,
            Some(store),
            2 * 1024 * 1024,
        )
    }
    #[cfg(windows)]
    pub(crate) fn namespace_source_whole(
        &self,
        expected: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<std::fs::File> {
        self.native
            .namespace_source_bounded(&self.path, expected, policy, store, 2 * 1024 * 1024)
    }
    #[cfg(windows)]
    pub(crate) fn restore_retained_whole(
        &self,
        object: &crate::review_retained::RetainedObject,
        desired: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<Vec<String>> {
        self.native.restore_retained_bounded(
            &self.path,
            object,
            desired,
            policy,
            store,
            2 * 1024 * 1024,
        )
    }
    #[cfg(windows)]
    pub(crate) fn read_whole_file_tracked(
        &self,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<ReviewImage> {
        self.native.read_bounded(
            &self.path,
            policy,
            2 * 1024 * 1024,
            false,
            None,
            Some(store),
        )
    }
    #[cfg(windows)]
    pub(crate) fn remove_retained(
        &self,
        expected: &ReviewImage,
        object: &crate::review_retained::RetainedObject,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<Vec<String>> {
        self.native
            .remove_retained(&self.path, expected, object, policy, store)
    }
    #[cfg(windows)]
    pub(crate) fn remove_prepared(
        &self,
        source: std::fs::File,
        expected: &ReviewImage,
        object: &crate::review_retained::RetainedObject,
        policy: &PathPolicy,
    ) -> Result<Vec<String>> {
        self.native
            .remove_prepared(&self.path, source, expected, object, policy)
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
        // Windows canonicalization changes an ordinary drive/UNC spelling
        // into a verbatim prefix. Preserve metadata completeness when the
        // original route itself passes no-reparse ancestor anchoring.
        #[cfg(windows)]
        let direct = Self::open(path, policy).is_ok();
        #[cfg(not(windows))]
        let direct = path == actual;
        if !direct {
            image.security_complete = false;
        }
        Ok(image)
    }
    #[cfg(windows)]
    pub(crate) fn capture_snapshot_tracked(
        path: &Path,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<ReviewImage> {
        if !path.is_absolute() || !std::fs::symlink_metadata(path)?.is_file() {
            bail!("Snapshot requires an absolute regular final entry");
        }
        let actual = path.canonicalize()?;
        if !matches!(
            policy.classify(path, &actual).map_err(anyhow::Error::msg)?,
            PathDecision::Ordinary
        ) {
            bail!("Protected snapshot alias or target");
        }
        let access = Self::open(&actual, policy)?;
        let mut image = access.native.read_bounded(
            &access.path,
            policy,
            2 * 1024 * 1024,
            true,
            Some(path),
            Some(store),
        )?;
        // Windows canonicalization changes an ordinary drive/UNC spelling
        // into a verbatim prefix. Preserve metadata completeness when the
        // original route itself passes no-reparse ancestor anchoring.
        #[cfg(windows)]
        let direct = Self::open(path, policy).is_ok();
        #[cfg(not(windows))]
        let direct = path == actual;
        if !direct {
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
            // Interactive reads still fail closed. Snapshot capture keeps bytes when
            // metadata is unverifiable and never treats that map as complete.
            let before_attrs = if capture {
                crate::review_unix_metadata::capture_scan(&file)
            } else {
                crate::review_unix_metadata::CaptureScan::Stable(
                    crate::review_unix_metadata::inspect_extended(&file)?,
                )
            };
            self.recheck_policy(policy)?;
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            (&file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                bail!("Current file grew beyond the review limit");
            }
            let after = file.metadata()?;
            let after_attrs = if capture {
                crate::review_unix_metadata::capture_scan(&file)
            } else {
                crate::review_unix_metadata::CaptureScan::Stable(
                    crate::review_unix_metadata::inspect_extended(&file)?,
                )
            };
            if metadata.len() != after.len()
                || metadata.modified()? != after.modified()?
                || metadata.mode() != after.mode()
                || metadata.uid() != after.uid()
                || metadata.gid() != after.gid()
                || metadata.ctime() != after.ctime()
                || metadata.ctime_nsec() != after.ctime_nsec()
                || metadata.nlink() != after.nlink()
                || bytes.len() as u64 != after.len()
            {
                bail!("Current file changed while reading; inspect again");
            }
            if let Some(alias) = alias {
                self.verify_snapshot_alias(&file, alias, policy)?;
            }
            let (unix_metadata, extended_complete) =
                crate::review_unix_metadata::capture_authority(
                    &before_attrs,
                    &after_attrs,
                    capture,
                )?;
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
                unix_metadata,
            })
        }
        #[cfg(windows)]
        {
            self.native
                .read_bounded(&self.path, policy, limit, capture, alias, None)
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
    #[cfg(windows)]
    pub(crate) fn capture_windows_directory(
        &self,
        policy: &PathPolicy,
    ) -> Result<WindowsDirectoryProof> {
        self.native.capture_directory(&self.path, policy)
    }
    #[cfg(windows)]
    pub(crate) fn verify_windows_directory(
        &self,
        proof: &WindowsDirectoryProof,
        policy: &PathPolicy,
        remove: bool,
    ) -> Result<()> {
        self.native
            .verify_directory(&self.path, proof, policy, remove)
    }
    /// Identity of an ordinary directory, with no symlink traversal at the leaf.
    pub(crate) fn directory_identity(&self, policy: &PathPolicy) -> Result<(u64, u64)> {
        self.recheck_policy(policy)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    self.parent.as_raw_fd(),
                    self.leaf.as_ptr(),
                    info.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let info = unsafe { info.assume_init() };
            if info.st_mode & libc::S_IFMT != libc::S_IFDIR {
                bail!("Directory changed type since the Run");
            }
            Ok((info.st_dev as u64, info.st_ino as u64))
        }
        #[cfg(windows)]
        {
            self.native.directory_identity(&self.path, policy)
        }
        #[cfg(not(any(unix, windows)))]
        {
            bail!("Anchored directory rewind is unavailable on this platform")
        }
    }
    pub(crate) fn remove_created_directory(
        &self,
        expected: (u64, u64),
        policy: &PathPolicy,
    ) -> Result<()> {
        if self.directory_identity(policy)? != expected {
            bail!("Directory changed since the Run");
        }
        self.recheck_policy(policy)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // AT_REMOVEDIR fails for a nonempty directory; unknown children are never deleted.
            if unsafe {
                libc::unlinkat(
                    self.parent.as_raw_fd(),
                    self.leaf.as_ptr(),
                    libc::AT_REMOVEDIR,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.parent.sync_all()?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            bail!("Anchored directory rewind is unavailable on this platform")
        }
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
        self.replace_bounded(expected, desired, policy, MAX_REVIEW_BYTES)
    }
    pub(crate) fn replace_whole_file(
        &self,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
    ) -> Result<Vec<String>> {
        self.replace_bounded(expected, desired, policy, 2 * 1024 * 1024)
    }
    fn replace_bounded(
        &self,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
        limit: usize,
    ) -> Result<Vec<String>> {
        self.recheck_policy(policy)?;
        if desired.bytes.as_ref().is_some_and(|b| b.len() > limit) {
            bail!("Review replacement exceeds its byte limit");
        }
        let current = self.read_bounded(policy, limit, false, None)?;
        if current != *expected {
            bail!("File content, existence or mode changed; inspect again");
        }
        expected.ensure_consistent()?;
        desired.ensure_consistent()?;
        if expected.bytes.is_some()
            && desired.bytes.is_some()
            && expected.unix_metadata != desired.unix_metadata
        {
            bail!("Desired extended metadata does not match the captured file; review does not rewrite extended attributes");
        }
        #[cfg(windows)]
        if expected.bytes.is_none() || desired.bytes.is_none() {
            bail!("Windows existence-changing review requires retained-object recovery");
        }
        #[cfg(windows)]
        if expected.bytes.is_some() && desired.bytes.is_some() {
            return if limit == MAX_REVIEW_BYTES {
                self.native
                    .replace_existing(&self.path, expected, desired, policy)
            } else {
                self.native
                    .replace_existing_bounded(&self.path, expected, desired, policy, None, limit)
            };
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
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            struct Temp<'a> {
                parent: &'a std::fs::File,
                name: std::ffi::CString,
                file: Option<std::fs::File>,
            }
            impl Drop for Temp<'_> {
                fn drop(&mut self) {
                    if let Some(file) = self.file.take() {
                        let _ = crate::review_unix_metadata::unlink_stage_name(
                            self.parent,
                            &self.name,
                            &file,
                        );
                    }
                }
            }
            let mut temp = Temp {
                parent: &self.parent,
                name: std::ffi::CString::new(format!(".formless-review-{}", uuid::Uuid::new_v4()))?,
                file: None,
            };
            if let Some(bytes) = &desired.bytes {
                let mode = desired
                    .mode
                    .context("Exact prior file mode was not recorded")?;
                let owner = desired
                    .owner
                    .context("Exact prior file owner was not recorded")?;
                let group = desired
                    .group
                    .context("Exact prior file group was not recorded")?;
                if mode & 0o6000 != 0 {
                    bail!("Set-ID files are not hunk-editable");
                }
                crate::review_unix_metadata::ensure_attribute_platform()?;
                // SAFETY: the parent fd is live, the name is NUL-terminated, and
                // these flags create a private regular file without following a symlink.
                let fd = unsafe {
                    libc::openat(
                        self.parent.as_raw_fd(),
                        temp.name.as_ptr(),
                        libc::O_RDWR
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
                // SAFETY: `fd` is open and uniquely owned. `File` takes that ownership.
                temp.file = Some(unsafe { std::fs::File::from_raw_fd(fd) });
                {
                    let Some(file) = temp.file.as_mut() else {
                        bail!("Private review stage was not created");
                    };
                    // ACL, flags, unexpected names, and any preexisting user.* name
                    // are refused before any desired byte. Absent user.* names are restored later.
                    crate::review_unix_metadata::require_stage_preexisting(
                        file,
                        desired.unix_metadata.as_ref(),
                    )?;
                    // SAFETY: live stage descriptor and the recorded owner and group.
                    if unsafe {
                        libc::fchown(file.as_raw_fd(), owner as libc::uid_t, group as libc::gid_t)
                    } != 0
                    {
                        return Err(std::io::Error::last_os_error().into());
                    }
                    file.write_all(bytes)?;
                    crate::review_unix_metadata::restore_user_attributes(
                        file,
                        desired.unix_metadata.as_ref(),
                    )?;
                    // SAFETY: live stage descriptor. Only the recorded permission bits are applied.
                    if unsafe { libc::fchmod(file.as_raw_fd(), (mode & 0o7777) as libc::mode_t) }
                        != 0
                    {
                        return Err(std::io::Error::last_os_error().into());
                    }
                    file.sync_all()?;
                }
                let Some(file) = temp.file.as_ref() else {
                    bail!("Private review stage was not created");
                };
                if !crate::review_unix_metadata::stage_ready_to_publish(
                    &self.parent,
                    &temp.name,
                    file,
                )? {
                    bail!("Private review stage no longer matches its directory entry; the target was not changed");
                }
                let staged = file.metadata()?;
                if staged.uid() != owner
                    || staged.gid() != group
                    || staged.permissions().mode() & 0o7777 != mode & 0o7777
                {
                    bail!("Private review stage ownership or mode does not match the desired image; the target was not changed");
                }
                if !crate::review_unix_metadata::staged_map_matches(
                    file,
                    desired.unix_metadata.as_ref(),
                )? {
                    bail!("Staged extended metadata does not match the desired image; the target was not changed");
                }
            }
            self.recheck_policy(policy)?;
            if self.read_bounded(policy, limit, false, None)? != *expected {
                bail!("File changed before the review commit; inspect again");
            }
            if expected.mode.is_some_and(|mode| mode & 0o6000 != 0) {
                bail!("Set-ID files are not hunk-editable");
            }
            if let Some(file) = temp.file.as_ref() {
                if !crate::review_unix_metadata::stage_ready_to_publish(
                    &self.parent,
                    &temp.name,
                    file,
                )? {
                    bail!("Private review stage was replaced before publication; the target was not changed");
                }
                if !crate::review_unix_metadata::staged_map_matches(
                    file,
                    desired.unix_metadata.as_ref(),
                )? {
                    bail!("Private review stage metadata changed before publication; the target was not changed");
                }
            }
            let create_only = desired.bytes.is_some() && expected.bytes.is_none();
            // A mismatch observed after this namespace change is ambiguous for the
            // caller. This function does not retry it or rewrite the file in place.
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
                if let Some(file) = temp.file.as_ref() {
                    if let Err(error) = crate::review_unix_metadata::unlink_stage_name(
                        &self.parent,
                        &temp.name,
                        file,
                    ) {
                        warnings.push(format!("File creation completed, but temporary-link cleanup could not be confirmed: {error}"));
                    } else {
                        temp.file.take();
                    }
                }
            } else {
                temp.file.take();
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
                    unix_metadata: None,
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
    fn user_attributes_edit_undo_and_recreation_preserve_the_exact_map() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("tagged.txt");
        std::fs::write(&path, b"exact tagged bytes\r\n").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        for (name, value) in [
            ("user.formless_z", &b"z"[..]),
            ("user.formless_a", &b"private metadata"[..]),
            ("user.formless_empty", &b""[..]),
            ("user.formless_bin", &b"\0\xff"[..]),
        ] {
            crate::review_unix_metadata::set_owned_attribute_for_test(&file, name, value).unwrap();
        }
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        let metadata = before.unix_metadata.as_ref().unwrap();
        let names: Vec<_> = metadata
            .names()
            .filter(|name| name.starts_with("user."))
            .collect();
        assert_eq!(
            names,
            vec![
                "user.formless_a",
                "user.formless_bin",
                "user.formless_empty",
                "user.formless_z"
            ]
        );
        let pairs = metadata.pairs().unwrap();
        assert!(pairs
            .iter()
            .any(|(name, value)| name == "user.formless_empty" && value.is_empty()));
        assert!(pairs
            .iter()
            .any(|(name, value)| name == "user.formless_bin" && value == b"\0\xff"));
        assert!(before.security.is_none());
        assert!(before.security_complete);
        assert!(before.write_restriction().is_none());
        assert_eq!(access.read(&policy).unwrap(), before);
        let encoded = encode_before_metadata(&before).unwrap();
        assert_eq!(
            decode_before_metadata(&encoded, before.bytes.as_deref()).unwrap(),
            before
        );
        let mut after = before.clone();
        after.bytes = Some(b"changed tagged bytes\r\n".to_vec());
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
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stale_attribute_changes_do_not_publish() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("stale.txt");
        std::fs::write(&path, b"exact").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        crate::review_unix_metadata::set_owned_attribute_for_test(
            &file,
            "user.formless_live",
            b"one",
        )
        .unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        crate::review_unix_metadata::set_owned_attribute_for_test(
            &file,
            "user.formless_live",
            b"two",
        )
        .unwrap();
        let mut desired = before.clone();
        desired.bytes = Some(b"nope".to_vec());
        assert!(access.replace(&before, &desired, &policy).is_err());
        let live = access.read(&policy).unwrap();
        assert_eq!(live.bytes.as_deref(), Some(b"exact".as_slice()));
        assert_ne!(live.unix_metadata, before.unix_metadata);
        crate::review_unix_metadata::set_owned_attribute_for_test(
            &file,
            "user.formless_added",
            b"extra",
        )
        .unwrap();
        let added = access.read(&policy).unwrap();
        assert!(access.replace(&live, &desired, &policy).is_err());
        assert_eq!(
            access.read(&policy).unwrap().bytes.as_deref(),
            Some(b"exact".as_slice())
        );
        assert_eq!(
            crate::review_unix_metadata::read_attribute_for_test(&file, "user.formless_added")
                .unwrap(),
            b"extra"
        );
        crate::review_unix_metadata::remove_user_attribute_for_test(&file, "user.formless_added")
            .unwrap();
        assert!(access.replace(&added, &desired, &policy).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"exact");
        assert!(
            crate::review_unix_metadata::read_attribute_for_test(&file, "user.formless_added")
                .is_err()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn injected_snapshot_mismatch_and_failed_verify_keep_the_receipt_prepared() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("inject.txt");
        std::fs::write(&path, b"exact").unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        let platform = crate::review_unix_metadata::UnixPlatform::current().unwrap();
        let mut expected = before.clone();
        expected.unix_metadata = Some(
            crate::review_unix_metadata::UnixMetadata::try_new(
                platform,
                vec![("user.not_on_file".into(), b"x".to_vec())],
            )
            .unwrap(),
        );
        let mut desired = expected.clone();
        desired.bytes = Some(b"nope".to_vec());
        assert!(access.replace(&expected, &desired, &policy).is_err());
        assert_eq!(access.read(&policy).unwrap(), before);
        let mut rewritten = before.clone();
        rewritten.bytes = Some(b"should-not-publish".to_vec());
        let mut pairs = before
            .unix_metadata
            .as_ref()
            .map(|metadata| metadata.pairs().unwrap())
            .unwrap_or_default();
        pairs.retain(|(name, _)| name.starts_with("user."));
        pairs.push(("user.formless_mismatch".into(), b"nope".to_vec()));
        pairs.sort();
        rewritten.unix_metadata =
            Some(crate::review_unix_metadata::UnixMetadata::try_new(platform, pairs).unwrap());
        let paths = crate::Paths {
            data_dir: dir.0.join("data"),
            config_dir: dir.0.join("config"),
            profile: "private-review-fixture".into(),
        };
        std::fs::create_dir(&paths.data_dir).unwrap();
        let mut receipt = crate::review_journal::Receipt::new(
            "session",
            "run",
            "file",
            "hunk",
            path.to_str().unwrap(),
            "pending",
            &before,
            &rewritten,
        )
        .unwrap();
        let journal = crate::review_journal::Journal::open(&paths, true)
            .unwrap()
            .unwrap();
        journal.prepare(&mut receipt).unwrap();
        assert!(access.replace(&before, &rewritten, &policy).is_err());
        assert_eq!(access.read(&policy).unwrap(), before);
        assert_eq!(
            journal.read(&receipt.id).unwrap().phase,
            crate::review_journal::Phase::Prepared
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"exact");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stage_hook_refuses_inherited_metadata_before_bytes_and_keeps_prepared() {
        use crate::review_unix_metadata::{
            install_stage_fault, stage_hook_len, stage_hook_reached, stage_prewrite_len, StageFault,
        };
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("stage-hook.txt");
        std::fs::write(&path, b"exact").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        crate::review_unix_metadata::set_owned_attribute_for_test(
            &file,
            "user.formless_live",
            b"one",
        )
        .unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        assert!(before.security_complete);
        assert!(before.unix_metadata.is_some());
        let mut desired = before.clone();
        desired.bytes = Some(b"should-not-publish".to_vec());
        let paths = crate::Paths {
            data_dir: dir.0.join("data"),
            config_dir: dir.0.join("config"),
            profile: "private-review-fixture".into(),
        };
        std::fs::create_dir(&paths.data_dir).unwrap();
        let mut receipt = crate::review_journal::Receipt::new(
            "session",
            "run",
            "file",
            "hunk",
            path.to_str().unwrap(),
            "pending",
            &before,
            &desired,
        )
        .unwrap();
        let journal = crate::review_journal::Journal::open(&paths, true)
            .unwrap()
            .unwrap();
        journal.prepare(&mut receipt).unwrap();
        let expected_len = desired.bytes.as_ref().unwrap().len() as u64;
        for fault in [
            StageFault::InheritedAcl,
            StageFault::UnsupportedAttr,
            StageFault::PreexistingUserSame,
            StageFault::PreexistingUserWrong,
            StageFault::FailUserSetter,
            StageFault::UnexpectedAttr,
            StageFault::FailFinal,
        ] {
            let _guard = install_stage_fault(fault);
            let error = access.replace(&before, &desired, &policy).unwrap_err();
            assert_eq!(std::fs::read(&path).unwrap(), b"exact", "{fault:?}");
            assert_eq!(access.read(&policy).unwrap(), before, "{fault:?}");
            assert_eq!(
                journal.read(&receipt.id).unwrap().phase,
                crate::review_journal::Phase::Prepared,
                "{fault:?}"
            );
            assert_eq!(stage_prewrite_len(), Some(0), "{fault:?}");
            let message = error.to_string();
            match fault {
                StageFault::InheritedAcl => {
                    assert!(message.contains("Inherited ACL"), "{message}");
                    assert!(
                        message.contains("refusing before any desired byte"),
                        "{message}"
                    );
                    assert!(!stage_hook_reached());
                    assert!(stage_hook_len().is_none());
                }
                StageFault::UnsupportedAttr => {
                    assert!(message.contains("Unexpected stage attribute"), "{message}");
                    assert!(
                        message.contains("refusing before any desired byte"),
                        "{message}"
                    );
                    assert!(!stage_hook_reached());
                    assert!(stage_hook_len().is_none());
                }
                StageFault::PreexistingUserSame | StageFault::PreexistingUserWrong => {
                    assert!(message.contains("Preexisting user attribute"), "{message}");
                    assert!(
                        message.contains("refusing before any desired byte"),
                        "{message}"
                    );
                    assert!(!stage_hook_reached());
                    assert!(stage_hook_len().is_none());
                }
                StageFault::FailUserSetter => {
                    assert!(stage_hook_reached(), "{message}");
                    assert_eq!(stage_hook_len(), Some(expected_len));
                }
                StageFault::UnexpectedAttr => {
                    assert!(stage_hook_reached(), "{message}");
                    assert_eq!(stage_hook_len(), Some(expected_len));
                    assert!(
                        message.contains("Staged extended metadata does not match"),
                        "{message}"
                    );
                }
                StageFault::FailFinal => {
                    assert!(stage_hook_reached(), "{message}");
                    assert_eq!(stage_hook_len(), Some(expected_len));
                    assert!(message.contains("changed before publication"), "{message}");
                }
            }
            assert!(std::fs::read_dir(&dir.0).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".formless-review-")
            }));
        }
        assert_eq!(
            crate::review_unix_metadata::read_attribute_for_test(&file, "user.formless_live")
                .unwrap(),
            b"one"
        );
    }

    #[cfg(target_os = "macos")]
    fn stage_metadata_mismatch(message: &str) -> bool {
        message.contains(
            "Staged provenance does not match the desired image; refusing before any desired byte",
        ) || message.contains(
            "Staged extended metadata does not match the desired image; the target was not changed",
        ) || message.contains(
            "Private review stage metadata changed before publication; the target was not changed",
        )
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn empty_creation_preserves_a_naturally_empty_attribute_map() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("created.txt");
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        // SAFETY: geteuid and getegid have no inputs and cannot fail.
        let desired = ReviewImage {
            bytes: Some(b"created".to_vec()),
            mode: Some(0o100644),
            readonly: false,
            owner: Some(unsafe { libc::geteuid() } as u32),
            group: Some(unsafe { libc::getegid() } as u32),
            security: None,
            security_complete: true,
            windows_identity: None,
            unix_metadata: None,
        };
        let encoded = encode_before_metadata(&desired).unwrap();
        assert!(!encoded.contains("unix_metadata"));
        let desired = decode_before_metadata(&encoded, desired.bytes.as_deref()).unwrap();
        assert!(access
            .replace(&ReviewImage::missing(), &desired, &policy)
            .unwrap()
            .is_empty());
        assert_eq!(access.read(&policy).unwrap(), desired);
        assert!(desired.unix_metadata.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_does_not_adopt_provenance_for_an_empty_or_legacy_desired_map() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("created.txt");
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        // SAFETY: geteuid and getegid have no inputs and cannot fail.
        let desired = ReviewImage {
            bytes: Some(b"created".to_vec()),
            mode: Some(0o100644),
            readonly: false,
            owner: Some(unsafe { libc::geteuid() } as u32),
            group: Some(unsafe { libc::getegid() } as u32),
            security: None,
            security_complete: true,
            windows_identity: None,
            unix_metadata: None,
        };
        let encoded = encode_before_metadata(&desired).unwrap();
        assert!(!encoded.contains("unix_metadata"));
        let desired = decode_before_metadata(&encoded, desired.bytes.as_deref()).unwrap();
        assert!(desired.unix_metadata.is_none());
        match access.replace(&ReviewImage::missing(), &desired, &policy) {
            Ok(warnings) => {
                assert!(warnings.is_empty());
                let published = access.read(&policy).unwrap();
                assert_eq!(published, desired);
                assert!(published.unix_metadata.is_none());
                assert_eq!(std::fs::read(&path).unwrap(), b"created");
            }
            Err(error) => {
                assert!(stage_metadata_mismatch(&error.to_string()), "{error}");
                assert!(!path.exists());
            }
        }
        assert!(std::fs::read_dir(&dir.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".formless-review-")
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn provenance_lifecycle_is_exact_for_the_actual_descriptor_or_fails_closed() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("provenance.txt");
        let original = b"before\r\n";
        std::fs::write(&path, original).unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        match access.read(&policy) {
            Err(error) => {
                let snapshot = access.snapshot(&policy).unwrap();
                assert_eq!(snapshot.bytes.as_deref(), Some(original.as_slice()));
                assert!(snapshot.unix_metadata.is_none());
                assert!(!snapshot.security_complete);
                assert!(snapshot.write_restriction().is_some());
                assert!(
                    error
                        .to_string()
                        .contains("cannot be preserved by interactive review"),
                    "{error}"
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
            Ok(before) => {
                let pairs = before
                    .unix_metadata
                    .as_ref()
                    .map(|metadata| metadata.pairs().unwrap())
                    .unwrap_or_default();
                assert!(pairs.iter().all(|(name, _)| {
                    name.starts_with("user.") || name == "com.apple.provenance"
                }));
                assert!(before.security_complete);
                let mut forged_pairs = pairs.clone();
                if let Some(entry) = forged_pairs
                    .iter_mut()
                    .find(|(name, _)| name == "com.apple.provenance")
                {
                    entry.1.push(0xff);
                } else {
                    forged_pairs.push(("com.apple.provenance".into(), b"forged-stamp".to_vec()));
                }
                forged_pairs.sort();
                let mut forged = before.clone();
                forged.unix_metadata = Some(
                    crate::review_unix_metadata::UnixMetadata::try_new(
                        crate::review_unix_metadata::UnixPlatform::Macos,
                        forged_pairs,
                    )
                    .unwrap(),
                );
                forged.bytes = Some(b"forged\r\n".to_vec());
                let forged_error = access.replace(&before, &forged, &policy).unwrap_err();
                assert!(
                    forged_error.to_string().contains(
                        "Desired extended metadata does not match the captured file; review does not rewrite extended attributes"
                    ),
                    "{forged_error}"
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(access.read(&policy).unwrap(), before);
                if before.unix_metadata.is_some() {
                    let mut stripped = before.clone();
                    stripped.unix_metadata = None;
                    stripped.bytes = Some(b"stripped\r\n".to_vec());
                    let stripped_error = access.replace(&before, &stripped, &policy).unwrap_err();
                    assert!(
                        stripped_error.to_string().contains(
                            "Desired extended metadata does not match the captured file; review does not rewrite extended attributes"
                        ),
                        "{stripped_error}"
                    );
                    assert_eq!(std::fs::read(&path).unwrap(), original);
                    assert_eq!(access.read(&policy).unwrap(), before);
                } else {
                    assert!(before.unix_metadata.is_none());
                }
                let mut after = before.clone();
                after.bytes = Some(b"after\r\n".to_vec());
                match access.replace(&before, &after, &policy) {
                    Ok(warnings) => {
                        assert!(warnings.is_empty());
                        assert_eq!(access.read(&policy).unwrap(), after);
                    }
                    Err(error) => {
                        assert!(stage_metadata_mismatch(&error.to_string()), "{error}");
                        assert_eq!(std::fs::read(&path).unwrap(), original);
                        assert_eq!(access.read(&policy).unwrap(), before);
                    }
                }
            }
        }
        assert!(std::fs::read_dir(&dir.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".formless-review-")
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_attribute_stays_on_the_file_and_blocks_review_writes() {
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("unknown.txt");
        std::fs::write(&path, b"exact tagged bytes").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        crate::review_unix_metadata::set_owned_attribute_for_test(
            &file,
            "com.formless.unsupported",
            b"keep",
        )
        .unwrap();
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
        assert!(snapshot.unix_metadata.is_none());
        assert!(!snapshot.security_complete);
        assert!(snapshot.write_restriction().is_some());
        assert!(snapshot.security.is_none());
        let encoded = encode_before_metadata(&snapshot).unwrap();
        assert_eq!(
            decode_before_metadata(&encoded, snapshot.bytes.as_deref()).unwrap(),
            snapshot
        );
        assert_eq!(
            crate::review_unix_metadata::read_attribute_for_test(&file, "com.formless.unsupported")
                .unwrap(),
            b"keep"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"exact tagged bytes");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn oversized_or_unreadable_capture_keeps_bytes_without_write_authority() {
        use crate::review_unix_metadata::{
            capture_inspect_calls, install_capture_fault, CaptureFault,
        };
        let dir = super::tests::Fixture::new();
        let path = dir.0.join("bounded.txt");
        let body = b"diagnostic-bytes";
        std::fs::write(&path, body).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        crate::review_unix_metadata::set_owned_attribute_for_test(
            &file,
            "user.formless_keep",
            b"secret",
        )
        .unwrap();
        let policy = PathPolicy::default();
        let access = ReviewPath::open(&path, &policy).unwrap();
        let complete = access.read(&policy).unwrap();
        assert!(complete.security_complete);
        assert!(complete.unix_metadata.is_some());
        for fault in [
            CaptureFault::OversizedList,
            CaptureFault::UnreadableValue,
            CaptureFault::SecondScanFails,
        ] {
            let _guard = install_capture_fault(fault);
            let image = access.snapshot(&policy).unwrap();
            assert_eq!(image.bytes.as_deref(), Some(body.as_slice()), "{fault:?}");
            assert!(image.unix_metadata.is_none(), "{fault:?}");
            assert!(!image.security_complete, "{fault:?}");
            assert!(image.write_restriction().is_some(), "{fault:?}");
            assert!(image.security.is_none(), "{fault:?}");
            let inspections = capture_inspect_calls();
            match fault {
                CaptureFault::OversizedList | CaptureFault::UnreadableValue => {
                    assert_eq!(inspections, 0, "{fault:?}");
                }
                CaptureFault::SecondScanFails => assert_eq!(inspections, 1),
                CaptureFault::SecondScanDiffers => unreachable!(),
            }
            assert!(
                access.replace(&image, &image, &policy).is_err(),
                "{fault:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), body);
        }
        {
            let _guard = install_capture_fault(CaptureFault::SecondScanDiffers);
            let error = access.snapshot(&policy).unwrap_err();
            assert!(
                error.to_string().contains("changed while reading"),
                "{error}"
            );
            assert_eq!(capture_inspect_calls(), 2);
        }
        assert_eq!(access.read(&policy).unwrap(), complete);
        assert_eq!(std::fs::read(&path).unwrap(), body);
    }
}

#[cfg(windows)]
#[path = "review_file_windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::WindowsDirectoryProof;

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
        assert!(image.unix_metadata.is_none());
        assert_eq!(serde_json::to_string(&image).unwrap(), legacy);
        let old_hash = image.integrity_hash().unwrap();
        let mut identified = image;
        identified.windows_identity = Some(WindowsIdentity {
            volume: 17,
            file: [3; 16],
        });
        assert_ne!(identified.integrity_hash().unwrap(), old_hash);
        assert!(!serde_json::to_string(&identified)
            .unwrap()
            .contains("unix_metadata"));
        let metadata = encode_before_metadata(&identified).unwrap();
        assert!(!metadata.contains("unix_metadata"));
        assert_eq!(
            decode_before_metadata(&metadata, Some(b"a")).unwrap(),
            identified
        );
        let tampered = metadata.replace("\"volume\":17", "\"volume\":18");
        assert_ne!(tampered, metadata);
        assert!(decode_before_metadata(&tampered, Some(b"a")).is_err());
        let mut unknown = serde_json::to_value(ReviewImage::missing()).unwrap();
        unknown["not_a_field"] = serde_json::json!(1);
        assert!(serde_json::from_value::<ReviewImage>(unknown).is_err());
    }

    #[test]
    fn before_metadata_bound_stays_128_kib_and_missing_extensions_are_rejected() {
        let oversized = "x".repeat(128 * 1024 + 1);
        assert!(decode_before_metadata(&oversized, Some(b"a")).is_err());
        let encoded = encode_before_metadata(&ReviewImage::missing()).unwrap();
        assert!(encoded.len() < 128 * 1024);
        assert!(!encoded.contains("unix_metadata"));
        let meta = crate::review_unix_metadata::UnixMetadata::try_new(
            crate::review_unix_metadata::UnixPlatform::Linux,
            vec![("user.a".into(), b"b".to_vec())],
        )
        .unwrap();
        let bare = ReviewImage {
            bytes: Some(b"xyz".to_vec()),
            security_complete: true,
            unix_metadata: None,
            ..Default::default()
        };
        let extended = ReviewImage {
            unix_metadata: Some(meta.clone()),
            ..bare.clone()
        };
        assert!(extended.security.is_none());
        let value_b64_len = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(b"b").len()
        };
        assert!(
            meta.retained_bytes() > "user.a".len() + value_b64_len,
            "retained_bytes must include struct, vec, and string overhead"
        );
        assert!(extended.retained_bytes() > 3 + "user.a".len() + value_b64_len);
        assert_eq!(extended.retained_bytes(), 3 + meta.retained_bytes());
        assert_ne!(
            bare.integrity_hash().unwrap(),
            extended.integrity_hash().unwrap()
        );
        assert!(!serde_json::to_string(&bare)
            .unwrap()
            .contains("unix_metadata"));
        let mut missing = ReviewImage::missing();
        missing.unix_metadata = Some(meta);
        assert!(missing.ensure_consistent().is_err());
        assert!(encode_before_metadata(&missing).is_err());
        let integrity = missing.integrity_hash().unwrap();
        let payload = serde_json::to_string(&BeforeMetadata {
            version: 1,
            image: missing,
            security_b64: None,
            integrity,
        })
        .unwrap();
        let error = decode_before_metadata(&payload, None).unwrap_err();
        assert!(error.to_string().contains("Missing review image"));
    }

    #[test]
    fn extension_tamper_changes_the_integrity_hash() {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let meta = crate::review_unix_metadata::UnixMetadata::try_new(
                crate::review_unix_metadata::UnixPlatform::current().unwrap(),
                vec![("user.a".into(), b"a".to_vec())],
            )
            .unwrap();
            let mut image = ReviewImage::missing();
            image.bytes = Some(b"a".to_vec());
            image.unix_metadata = Some(meta);
            image.security_complete = true;
            let encoded = encode_before_metadata(&image).unwrap();
            let tampered = encoded.replace("YQ==", "Yg==");
            assert_ne!(tampered, encoded);
            assert!(decode_before_metadata(&tampered, Some(b"a")).is_err());
        }
    }
}
