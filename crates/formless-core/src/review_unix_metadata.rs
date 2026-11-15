//! Conditional Unix attribute preservation for owned review I/O.
//!
//! `user.*` values may be written with descriptor APIs on a private stage.
//! `com.apple.provenance` is captured and compared, then left untouched: this
//! module never sets, removes, ignores, or adopts it. `None` on a review image
//! is the verified-empty contract, including legacy records. It is not a
//! wildcard for a stamp the kernel adds to a new inode.
//!
//! Known unsupported scenarios stay fail-closed and are not stripped:
//! nonempty macOS ACLs, nonzero BSD flags, a Darwin list that changes under
//! `XATTR_SHOWCOMPRESSION`, resource forks, Finder info, quarantine, decmpfs,
//! and every `security.*` / `system.*` / `trusted.*` name. A private stage is
//! inspected before chown or write: an inherited ACL, nonzero flags, a hidden
//! list, an unsupported or unexpected name, or a provenance value that is not
//! the desired opaque bytes refuses before any desired byte is written.
//! Desired `user.*` names that are not already present are restored exactly
//! afterward. A preexisting `user.*` name is refused before write. Publication
//! still requires the full staged map to match. This is
//! not an adoption flow and not an in-place repair.
//! The final rename can still race an external writer. Snapshot metadata
//! stays 128KiB, after-state proofs stay 6KiB, and the per-run and journal
//! caps are unchanged. A large but valid map can therefore be editable and
//! still lack an after-state proof.
//!
//! Windows security descriptors are never stored in this extension.

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) const UNIX_METADATA_VERSION: u8 = 1;
pub(crate) const PROVENANCE_NAME: &str = "com.apple.provenance";
pub(crate) const MAX_UNIX_ATTRS: usize = 16;
pub(crate) const MAX_UNIX_NAME_BYTES: usize = 127;
pub(crate) const MAX_UNIX_VALUE_BYTES: usize = 1024;
pub(crate) const MAX_UNIX_AGGREGATE_BYTES: usize = 4096;
const MAX_LIST_BYTES: usize = MAX_UNIX_ATTRS * (MAX_UNIX_NAME_BYTES + 1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum UnixPlatform {
    Linux,
    Macos,
}

impl UnixPlatform {
    pub(crate) fn current() -> Result<Self> {
        if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Ok(Self::Macos)
        } else {
            bail!("Unix extended metadata is unsupported on this platform");
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnixMetadata {
    version: u8,
    platform: UnixPlatform,
    entries: Vec<UnixAttr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnixAttr {
    name: String,
    value_b64: String,
}

impl UnixMetadata {
    #[cfg(any(unix, test))]
    pub(crate) fn try_new(platform: UnixPlatform, pairs: Vec<(String, Vec<u8>)>) -> Result<Self> {
        if pairs.len() > MAX_UNIX_ATTRS {
            bail!("Unix metadata record is empty or exceeds its count limit");
        }
        let mut entries = Vec::with_capacity(pairs.len());
        for (name, value) in pairs {
            if name.len() > MAX_UNIX_NAME_BYTES || value.len() > MAX_UNIX_VALUE_BYTES {
                bail!("Unix attribute name or value exceeds its limit");
            }
            entries.push(UnixAttr {
                name,
                value_b64: base64::engine::general_purpose::STANDARD.encode(value),
            });
        }
        let metadata = Self {
            version: UNIX_METADATA_VERSION,
            platform,
            entries,
        };
        metadata.validate_stored()?;
        Ok(metadata)
    }

    #[cfg(test)]
    pub(crate) fn with_version_for_test(mut self, version: u8) -> Self {
        self.version = version;
        self
    }

    pub(crate) fn validate_stored(&self) -> Result<()> {
        if self.version != UNIX_METADATA_VERSION {
            bail!("Unsupported Unix metadata version");
        }
        if self.entries.is_empty() || self.entries.len() > MAX_UNIX_ATTRS {
            bail!("Unix metadata record is empty or exceeds its count limit");
        }
        let mut aggregate = 0usize;
        let mut previous: Option<&str> = None;
        for entry in &self.entries {
            if entry.name.is_empty()
                || entry.name.len() > MAX_UNIX_NAME_BYTES
                || entry.name.as_bytes().contains(&0)
            {
                bail!("Unix attribute name is malformed or exceeds its limit");
            }
            if previous.is_some_and(|prior| entry.name.as_str() <= prior) {
                bail!("Unix attribute records are not canonical");
            }
            previous = Some(entry.name.as_str());
            if !name_allowed(self.platform, &entry.name) {
                bail!("Unix attribute name is not restorable");
            }
            if entry.value_b64.len() > encoded_cap(MAX_UNIX_VALUE_BYTES) {
                bail!("Unix attribute value exceeds its limit");
            }
            let value = decode_canonical(&entry.value_b64)?;
            aggregate = aggregate
                .saturating_add(entry.name.len())
                .saturating_add(value.len());
            if aggregate > MAX_UNIX_AGGREGATE_BYTES {
                bail!("Unix attributes exceed their aggregate limit");
            }
        }
        Ok(())
    }

    pub(crate) fn validate_current(&self) -> Result<()> {
        self.validate_stored()?;
        if self.platform != UnixPlatform::current()? {
            bail!("Unix extended metadata belongs to another operating system");
        }
        Ok(())
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        let mut total = std::mem::size_of::<Self>().saturating_add(
            self.entries
                .capacity()
                .saturating_mul(std::mem::size_of::<UnixAttr>()),
        );
        for entry in &self.entries {
            total = total
                .saturating_add(entry.name.capacity())
                .saturating_add(entry.value_b64.capacity());
        }
        total
    }

    #[cfg(any(unix, test))]
    pub(crate) fn pairs(&self) -> Result<Vec<(String, Vec<u8>)>> {
        self.validate_stored()?;
        self.entries
            .iter()
            .map(|entry| Ok((entry.name.clone(), decode_canonical(&entry.value_b64)?)))
            .collect()
    }

    #[cfg(any(unix, test))]
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }
}

fn name_allowed(platform: UnixPlatform, name: &str) -> bool {
    match platform {
        UnixPlatform::Linux => is_user_name(name),
        UnixPlatform::Macos => is_user_name(name) || name == PROVENANCE_NAME,
    }
}

fn is_user_name(name: &str) -> bool {
    name.strip_prefix("user.")
        .is_some_and(|suffix| !suffix.is_empty() && !suffix.contains('\0'))
}

fn encoded_cap(limit: usize) -> usize {
    limit.div_ceil(3).saturating_mul(4)
}

fn decode_canonical(encoded: &str) -> Result<Vec<u8>> {
    if encoded.len() > encoded_cap(MAX_UNIX_VALUE_BYTES) {
        bail!("Unix attribute value exceeds its limit");
    }
    let value = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("Unix attribute value is not canonical base64")?;
    if base64::engine::general_purpose::STANDARD.encode(&value) != encoded {
        bail!("Unix attribute value is not canonical base64");
    }
    if value.len() > MAX_UNIX_VALUE_BYTES {
        bail!("Unix attribute value exceeds its limit");
    }
    Ok(value)
}

#[cfg(any(unix, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Restoration {
    Exact(Option<UnixMetadata>),
    Unsupported(&'static str),
}

#[cfg(any(unix, test))]
fn assess(
    platform: UnixPlatform,
    flags: u32,
    acl_empty: bool,
    normal: &BTreeMap<String, Vec<u8>>,
    revealed: &BTreeMap<String, Vec<u8>>,
) -> Result<Restoration> {
    if !acl_empty {
        return Ok(Restoration::Unsupported(
            "Extended ACLs cannot be preserved by interactive review; file is read-only in this view",
        ));
    }
    if flags != 0 {
        return Ok(Restoration::Unsupported(
            "BSD file flags cannot be preserved by interactive review; file is read-only in this view",
        ));
    }
    if normal != revealed {
        return Ok(Restoration::Unsupported(
            "Hidden extended attributes cannot be preserved by interactive review; file is read-only in this view",
        ));
    }
    if normal.is_empty() {
        return Ok(Restoration::Exact(None));
    }
    if normal.len() > MAX_UNIX_ATTRS {
        bail!("Extended attribute count exceeds its limit");
    }
    if normal.keys().any(|name| !name_allowed(platform, name)) {
        return Ok(Restoration::Unsupported(
            "Extended attributes or ACLs cannot be preserved by interactive review; file is read-only in this view",
        ));
    }
    let pairs = normal
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Ok(Restoration::Exact(Some(UnixMetadata::try_new(
        platform, pairs,
    )?)))
}

#[cfg(any(unix, test))]
fn bounded_len(size: isize, limit: usize) -> Result<usize> {
    if size < 0 {
        bail!("Extended attribute size is invalid");
    }
    let size = size as usize;
    if size > limit {
        bail!("Extended attribute payload exceeds its limit");
    }
    Ok(size)
}

#[cfg(any(unix, test))]
fn parse_names(buf: &[u8]) -> Result<Vec<String>> {
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    if buf.last() != Some(&0) {
        bail!("Extended attribute name list is malformed");
    }
    let mut parts: Vec<&[u8]> = buf.split(|byte| *byte == 0).collect();
    if parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    if parts.iter().any(|part| part.is_empty()) {
        bail!("Extended attribute name list is malformed");
    }
    let mut names = Vec::with_capacity(parts.len());
    for part in parts {
        if part.len() > MAX_UNIX_NAME_BYTES {
            bail!("Unix attribute name is malformed or exceeds its limit");
        }
        let name = std::str::from_utf8(part).context("Extended attribute name is malformed")?;
        names.push(name.to_owned());
    }
    Ok(names)
}

#[cfg(any(unix, test))]
fn canonical_names(buf: &[u8]) -> Result<Vec<String>> {
    let names = parse_names(buf)?;
    if names.len() > MAX_UNIX_ATTRS {
        bail!("Extended attribute count exceeds its limit");
    }
    let mut seen = std::collections::BTreeSet::new();
    for name in &names {
        if !seen.insert(name.as_str()) {
            bail!("Extended attribute names are malformed or duplicated");
        }
    }
    Ok(names)
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Descriptor {
    flags: u32,
    acl_empty: bool,
    normal: BTreeMap<String, Vec<u8>>,
    revealed: BTreeMap<String, Vec<u8>>,
}

#[cfg(unix)]
impl Descriptor {
    pub(crate) fn restoration(&self) -> Result<Restoration> {
        assess(
            UnixPlatform::current()?,
            self.flags,
            self.acl_empty,
            &self.normal,
            &self.revealed,
        )
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    fn with_extra_user_for_test(mut self) -> Self {
        let name = "user.formless_scan_race".to_owned();
        self.normal.insert(name.clone(), b"race".to_vec());
        self.revealed.insert(name, b"race".to_vec());
        self
    }

    pub(crate) fn require_exact(&self) -> Result<Option<UnixMetadata>> {
        match self.restoration()? {
            Restoration::Exact(map) => Ok(map),
            Restoration::Unsupported(message) => bail!(message),
        }
    }
}

#[cfg(any(unix, test))]
fn desired_attribute_map(
    platform: UnixPlatform,
    desired: Option<&UnixMetadata>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let Some(metadata) = desired else {
        return Ok(BTreeMap::new());
    };
    if metadata.platform != platform {
        bail!("Unix extended metadata belongs to another operating system");
    }
    Ok(metadata.pairs()?.into_iter().collect())
}

/// Attributes already on a stage. Desired `user.*` values may be absent and are
/// created later with `XATTR_CREATE`. Any preexisting `user.*` name is refused
/// before write, whether or not its value matches, because create-only restore
/// cannot succeed. Provenance that is already present must match exactly and is
/// never copied into the desired image.
#[cfg(any(unix, test))]
pub(crate) fn preexisting_eligible(
    platform: UnixPlatform,
    flags: u32,
    acl_empty: bool,
    normal: &BTreeMap<String, Vec<u8>>,
    revealed: &BTreeMap<String, Vec<u8>>,
    desired: Option<&UnixMetadata>,
) -> Result<()> {
    if !acl_empty {
        bail!("Inherited ACL on the review stage; refusing before any desired byte");
    }
    if flags != 0 {
        bail!("Inherited BSD flags on the review stage; refusing before any desired byte");
    }
    if normal != revealed {
        bail!("Inherited hidden attributes on the review stage; refusing before any desired byte");
    }
    let desired_map = desired_attribute_map(platform, desired)?;
    for (name, value) in normal {
        if name == PROVENANCE_NAME {
            match desired_map.get(name) {
                Some(expected) if expected == value => continue,
                _ => bail!(
                    "Staged provenance does not match the desired image; refusing before any desired byte"
                ),
            }
        }
        if is_user_name(name) {
            bail!(
                "Preexisting user attribute on the review stage; refusing before any desired byte"
            );
        }
        if !name_allowed(platform, name) {
            bail!("Unexpected stage attribute; refusing before any desired byte");
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn ensure_attribute_platform() -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        bail!("Extended permission metadata cannot be verified on this Unix platform");
    }
}

#[cfg(unix)]
pub(crate) fn inspect_extended(file: &std::fs::File) -> Result<Descriptor> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[cfg(test)]
        fault_inject::note_inspect();
        return inspect_supported(file);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        bail!("Extended permission metadata cannot be verified on this Unix platform");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn inspect_supported(file: &std::fs::File) -> Result<Descriptor> {
    #[cfg(target_os = "linux")]
    let flags = 0u32;
    #[cfg(target_os = "macos")]
    let flags = bsd_flags(file)?;
    #[cfg(target_os = "linux")]
    let acl_empty = true;
    #[cfg(target_os = "macos")]
    let acl_empty = macos_acl::is_empty(file)?;
    let normal = list_map(file, false)?;
    #[cfg(target_os = "macos")]
    let revealed = list_map(file, true)?;
    #[cfg(target_os = "linux")]
    let revealed = normal.clone();
    Ok(Descriptor {
        flags,
        acl_empty,
        normal,
        revealed,
    })
}

#[cfg(unix)]
pub(crate) fn require_stage_preexisting(
    file: &std::fs::File,
    desired: Option<&UnixMetadata>,
) -> Result<()> {
    let inspected = inspect_extended(file)?;
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    let inspected = fault_inject::prewrite_descriptor(file, desired, inspected)?;
    preexisting_eligible(
        UnixPlatform::current()?,
        inspected.flags,
        inspected.acl_empty,
        &inspected.normal,
        &inspected.revealed,
        desired,
    )
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureScan {
    Stable(Descriptor),
    Unverifiable,
}

/// One snapshot scan. Injected oversized and unreadable faults return before
/// any attribute buffer is allocated. Real inspect errors are unverifiable.
#[cfg(unix)]
pub(crate) fn capture_scan(file: &std::fs::File) -> CaptureScan {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    if let Some(scan) = fault_inject::injected_capture_scan(file) {
        return scan;
    }
    match inspect_extended(file) {
        Ok(descriptor) => CaptureScan::Stable(descriptor),
        Err(_) => CaptureScan::Unverifiable,
    }
}

/// `capture` preserves a byte-only diagnostic when a scan is unverifiable or
/// the stable view is unsupported. Two stable scans that differ are a race.
/// A failed scan never promotes the other scan's map to write authority.
#[cfg(unix)]
pub(crate) fn capture_authority(
    before: &CaptureScan,
    after: &CaptureScan,
    capture: bool,
) -> Result<(Option<UnixMetadata>, bool)> {
    match (before, after) {
        (CaptureScan::Stable(left), CaptureScan::Stable(right)) => {
            if left != right {
                bail!("Current file changed while reading; inspect again");
            }
            match left.restoration() {
                Ok(Restoration::Exact(map)) => Ok((map, true)),
                Ok(Restoration::Unsupported(message)) if !capture => bail!(message),
                Ok(Restoration::Unsupported(_)) => Ok((None, false)),
                Err(_) if capture => Ok((None, false)),
                Err(error) => Err(error),
            }
        }
        _ if capture => Ok((None, false)),
        _ => bail!("Extended file metadata could not be verified"),
    }
}

#[cfg(unix)]
pub(crate) fn staged_map_matches(
    file: &std::fs::File,
    desired: Option<&UnixMetadata>,
) -> Result<bool> {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    let step = fault_inject::begin_verify(file)?;
    let staged = inspect_extended(file)?.require_exact()?;
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    let staged = fault_inject::override_verify(staged, desired, step)?;
    Ok(staged.as_ref() == desired)
}

#[cfg(unix)]
pub(crate) fn restore_user_attributes(
    file: &std::fs::File,
    metadata: Option<&UnixMetadata>,
) -> Result<()> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (file, metadata);
        bail!("Extended permission metadata cannot be verified on this Unix platform");
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let Some(metadata) = metadata else {
            return Ok(());
        };
        metadata.validate_current()?;
        for entry in &metadata.entries {
            if entry.name == PROVENANCE_NAME {
                continue;
            }
            let value = decode_canonical(&entry.value_b64)?;
            set_user_create(file, &entry.name, &value)?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn set_user_create(file: &std::fs::File, name: &str, value: &[u8]) -> Result<()> {
    if name == PROVENANCE_NAME || !is_user_name(name) {
        bail!("Only user attributes can be restored");
    }
    #[cfg(test)]
    fault_inject::prepare_user_create(file, name, value)?;
    set_fd_attribute(file, name, value, libc::XATTR_CREATE)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn set_owned_attribute_for_test(
    file: &std::fs::File,
    name: &str,
    value: &[u8],
) -> Result<()> {
    if name.starts_with("com.apple.") || name.is_empty() || name.contains('\0') {
        bail!("test fixture must not set macOS system attributes");
    }
    set_fd_attribute(file, name, value, 0)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn read_attribute_for_test(file: &std::fs::File, name: &str) -> Result<Vec<u8>> {
    let c_name = std::ffi::CString::new(name).context("Extended attribute name is malformed")?;
    let fd = {
        use std::os::fd::AsRawFd;
        file.as_raw_fd()
    };
    let raw = xattr_value_len(fd, &c_name, false)?;
    let size = bounded_len(raw, MAX_UNIX_VALUE_BYTES)?;
    if size == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; size];
    let n = xattr_value_read(fd, &c_name, &mut buf, false)?;
    if n as usize != size {
        bail!("Extended attributes changed while they were being read");
    }
    Ok(buf)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn remove_user_attribute_for_test(file: &std::fs::File, name: &str) -> Result<()> {
    if !is_user_name(name) {
        bail!("test fixture removes only user attributes");
    }
    let c_name = std::ffi::CString::new(name).context("Extended attribute name is malformed")?;
    use std::os::fd::AsRawFd;
    let rc = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::fremovexattr(file.as_raw_fd(), c_name.as_ptr())
        }
        #[cfg(target_os = "macos")]
        {
            libc::fremovexattr(file.as_raw_fd(), c_name.as_ptr(), 0)
        }
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn set_fd_attribute(
    file: &std::fs::File,
    name: &str,
    value: &[u8],
    flags: libc::c_int,
) -> Result<()> {
    let c_name = std::ffi::CString::new(name).context("Extended attribute name is malformed")?;
    let ptr = if value.is_empty() {
        std::ptr::NonNull::<u8>::dangling()
            .as_ptr()
            .cast::<libc::c_void>()
    } else {
        value.as_ptr().cast::<libc::c_void>()
    };
    use std::os::fd::AsRawFd;
    // SAFETY: the fd is live, the name is NUL-terminated, and `ptr` is valid for
    // `value.len()` bytes (or dangling and unread when the length is zero).
    // CREATE refuses to replace an existing user attribute. Provenance is never
    // passed here.
    let rc = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::fsetxattr(file.as_raw_fd(), c_name.as_ptr(), ptr, value.len(), flags)
        }
        #[cfg(target_os = "macos")]
        {
            libc::fsetxattr(
                file.as_raw_fd(),
                c_name.as_ptr(),
                ptr,
                value.len(),
                0,
                flags,
            )
        }
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn list_map(file: &std::fs::File, show_compression: bool) -> Result<BTreeMap<String, Vec<u8>>> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let raw = xattr_list_len(fd, show_compression)?;
    let size = bounded_len(raw, MAX_LIST_BYTES)?;
    if size == 0 {
        return Ok(BTreeMap::new());
    }
    let mut buf = vec![0u8; size];
    let n = xattr_list_read(fd, &mut buf, show_compression)?;
    if n != size as isize {
        bail!("Extended attributes changed while they were being read");
    }
    let names = canonical_names(&buf)?;
    let mut map = BTreeMap::new();
    let mut aggregate = 0usize;
    for name in names {
        let c_name = std::ffi::CString::new(name.as_str())
            .context("Extended attribute name is malformed")?;
        let raw_value = xattr_value_len(fd, &c_name, show_compression)?;
        let value_len = bounded_len(raw_value, MAX_UNIX_VALUE_BYTES)?;
        aggregate = aggregate
            .saturating_add(name.len())
            .saturating_add(value_len);
        if aggregate > MAX_UNIX_AGGREGATE_BYTES {
            bail!("Unix attributes exceed their aggregate limit");
        }
        let value = if value_len == 0 {
            Vec::new()
        } else {
            let mut bytes = vec![0u8; value_len];
            let got = xattr_value_read(fd, &c_name, &mut bytes, show_compression)?;
            if got as usize != value_len {
                bail!("Extended attributes changed while they were being read");
            }
            bytes
        };
        map.insert(name, value);
    }
    Ok(map)
}

#[cfg(target_os = "linux")]
fn xattr_list_len(fd: std::os::fd::RawFd, _show: bool) -> Result<isize> {
    // SAFETY: a null buffer with a zero size is the documented length query.
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "macos")]
fn xattr_list_len(fd: std::os::fd::RawFd, show: bool) -> Result<isize> {
    // SAFETY: a null buffer with a zero size is the documented length query.
    // SHOWCOMPRESSION only changes which names are visible; it is not a write.
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, show_flag(show)) };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "linux")]
fn xattr_list_read(fd: std::os::fd::RawFd, buf: &mut [u8], _show: bool) -> Result<isize> {
    // SAFETY: `buf` is writable for its length and the fd is live.
    let size = unsafe { libc::flistxattr(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "macos")]
fn xattr_list_read(fd: std::os::fd::RawFd, buf: &mut [u8], show: bool) -> Result<isize> {
    // SAFETY: `buf` is writable for its length and the fd is live.
    let size = unsafe { libc::flistxattr(fd, buf.as_mut_ptr().cast(), buf.len(), show_flag(show)) };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "linux")]
fn xattr_value_len(fd: std::os::fd::RawFd, name: &std::ffi::CStr, _show: bool) -> Result<isize> {
    // SAFETY: NUL-terminated name and a null value buffer with size zero.
    let size = unsafe { libc::fgetxattr(fd, name.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "macos")]
fn xattr_value_len(fd: std::os::fd::RawFd, name: &std::ffi::CStr, show: bool) -> Result<isize> {
    // SAFETY: NUL-terminated name, position zero, and a null value buffer.
    let size = unsafe {
        libc::fgetxattr(
            fd,
            name.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            show_flag(show),
        )
    };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "linux")]
fn xattr_value_read(
    fd: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    buf: &mut [u8],
    _show: bool,
) -> Result<isize> {
    // SAFETY: `buf` is writable for its length, the name is NUL-terminated, and the fd is live.
    let size = unsafe { libc::fgetxattr(fd, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "macos")]
fn xattr_value_read(
    fd: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    buf: &mut [u8],
    show: bool,
) -> Result<isize> {
    // SAFETY: `buf` is writable for its length. Position zero does not address a resource fork.
    let size = unsafe {
        libc::fgetxattr(
            fd,
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            show_flag(show),
        )
    };
    if size < 0 {
        return Err(anyhow::Error::new(std::io::Error::last_os_error())
            .context("Extended file metadata could not be verified"));
    }
    Ok(size)
}

#[cfg(target_os = "macos")]
fn show_flag(show: bool) -> libc::c_int {
    // Apple sys/xattr.h XATTR_SHOWCOMPRESSION. Read-only; never combined with a set.
    if show {
        0x0020
    } else {
        0
    }
}

#[cfg(target_os = "macos")]
fn bsd_flags(file: &std::fs::File) -> Result<u32> {
    use std::os::fd::AsRawFd;
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live fd and a writable stat buffer. Fields are read only after success.
    let rc = unsafe { libc::fstat(file.as_raw_fd(), info.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fstat succeeded, so the kernel initialized `info`.
    Ok(unsafe { info.assume_init() }.st_flags)
}

#[cfg(target_os = "macos")]
mod macos_acl {
    use super::*;
    use std::os::fd::AsRawFd;
    // Darwin ABI (ACL_FIRST_ENTRY = 0). ENOENT means no ACL property. EINVAL
    // from acl_get_entry means the ACL has no first entry.
    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            id: libc::c_int,
            out: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }

    pub(super) fn is_empty(file: &std::fs::File) -> Result<bool> {
        unsafe {
            *libc::__error() = 0;
        }
        // SAFETY: the fd is live. A null result is an error or a documented absence.
        let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(true);
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
                // SAFETY: the pointer came from acl_get_fd and has not been freed.
                unsafe { acl_free(self.0) };
            }
        }
        let _owned = Acl(acl);
        let mut entry = std::ptr::null_mut();
        // SAFETY: `acl` is a live ACL object and `entry` is a writable out pointer.
        let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
        if result == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if result == -1 && error.raw_os_error() == Some(libc::EINVAL) {
            return Ok(true);
        }
        Err(error.into())
    }
}

#[cfg(unix)]
fn stat_entry(parent: &std::fs::File, name: &std::ffi::CStr) -> Result<Option<libc::stat>> {
    use std::os::fd::AsRawFd;
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live directory fd, NUL-terminated name, and AT_SYMLINK_NOFOLLOW so a
    // swapped symlink is described rather than followed. The buffer is read only
    // after success.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            info.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    // SAFETY: fstatat succeeded, so the kernel initialized `info`.
    Ok(Some(unsafe { info.assume_init() }))
}

#[cfg(unix)]
pub(crate) fn same_directory_inode(
    parent: &std::fs::File,
    name: &std::ffi::CStr,
    file: &std::fs::File,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let Some(info) = stat_entry(parent, name)? else {
        return Ok(false);
    };
    let live = file.metadata()?;
    Ok(info.st_dev as u64 == live.dev() && info.st_ino as u64 == live.ino())
}

#[cfg(unix)]
pub(crate) fn stage_ready_to_publish(
    parent: &std::fs::File,
    name: &std::ffi::CStr,
    file: &std::fs::File,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let Some(info) = stat_entry(parent, name)? else {
        return Ok(false);
    };
    let live = file.metadata()?;
    let regular = info.st_mode & libc::S_IFMT == libc::S_IFREG;
    Ok(regular
        && live.is_file()
        && info.st_nlink as u64 == 1
        && live.nlink() == 1
        && info.st_dev as u64 == live.dev()
        && info.st_ino as u64 == live.ino())
}

#[cfg(unix)]
pub(crate) fn unlink_stage_name(
    parent: &std::fs::File,
    name: &std::ffi::CStr,
    file: &std::fs::File,
) -> Result<()> {
    if !same_directory_inode(parent, name, file)? {
        bail!("temporary stage name no longer matches its inode");
    }
    use std::os::fd::AsRawFd;
    // SAFETY: the directory fd is live and the name was just shown to be this
    // inode. No AT_REMOVEDIR, so a directory entry that is not a file is not removed.
    let rc = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod fault_inject {
    use super::*;
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum StageFault {
        InheritedAcl,
        UnsupportedAttr,
        PreexistingUserSame,
        PreexistingUserWrong,
        FailUserSetter,
        UnexpectedAttr,
        FailFinal,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum CaptureFault {
        OversizedList,
        UnreadableValue,
        SecondScanFails,
        SecondScanDiffers,
    }

    pub(crate) struct StageFaultGuard(());
    pub(crate) struct CaptureFaultGuard(());

    thread_local! {
        static STAGE_FAULT: Cell<Option<StageFault>> = Cell::new(None);
        static STAGE_PREWRITE_LEN: Cell<Option<u64>> = Cell::new(None);
        static STAGE_HOOK_LEN: Cell<Option<u64>> = Cell::new(None);
        static STAGE_HOOK_REACHED: Cell<bool> = Cell::new(false);
        static STAGE_VERIFY_STEP: Cell<u8> = Cell::new(0);
        static CAPTURE_FAULT: Cell<Option<CaptureFault>> = Cell::new(None);
        static CAPTURE_CALLS: Cell<u8> = Cell::new(0);
        static INSPECT_CALLS: Cell<u32> = Cell::new(0);
    }

    fn stage_fault() -> Option<StageFault> {
        STAGE_FAULT.with(|cell| cell.get())
    }

    pub(super) fn note_inspect() {
        INSPECT_CALLS.with(|cell| cell.set(cell.get().saturating_add(1)));
    }

    pub(crate) fn install_stage_fault(fault: StageFault) -> StageFaultGuard {
        STAGE_FAULT.with(|cell| cell.set(Some(fault)));
        STAGE_PREWRITE_LEN.with(|cell| cell.set(None));
        STAGE_HOOK_LEN.with(|cell| cell.set(None));
        STAGE_HOOK_REACHED.with(|cell| cell.set(false));
        STAGE_VERIFY_STEP.with(|cell| cell.set(0));
        StageFaultGuard(())
    }

    impl Drop for StageFaultGuard {
        fn drop(&mut self) {
            STAGE_FAULT.with(|cell| cell.set(None));
        }
    }

    pub(crate) fn install_capture_fault(fault: CaptureFault) -> CaptureFaultGuard {
        CAPTURE_FAULT.with(|cell| cell.set(Some(fault)));
        CAPTURE_CALLS.with(|cell| cell.set(0));
        INSPECT_CALLS.with(|cell| cell.set(0));
        CaptureFaultGuard(())
    }

    impl Drop for CaptureFaultGuard {
        fn drop(&mut self) {
            CAPTURE_FAULT.with(|cell| cell.set(None));
        }
    }

    pub(crate) fn stage_prewrite_len() -> Option<u64> {
        STAGE_PREWRITE_LEN.with(|cell| cell.get())
    }

    pub(crate) fn stage_hook_len() -> Option<u64> {
        STAGE_HOOK_LEN.with(|cell| cell.get())
    }

    pub(crate) fn stage_hook_reached() -> bool {
        STAGE_HOOK_REACHED.with(|cell| cell.get())
    }

    pub(crate) fn capture_inspect_calls() -> u32 {
        INSPECT_CALLS.with(|cell| cell.get())
    }

    fn record_hook_len(file: &std::fs::File) {
        STAGE_HOOK_LEN.with(|cell| cell.set(file.metadata().ok().map(|meta| meta.len())));
    }

    pub(super) fn prewrite_descriptor(
        file: &std::fs::File,
        desired: Option<&UnixMetadata>,
        inspected: Descriptor,
    ) -> Result<Descriptor> {
        STAGE_VERIFY_STEP.with(|cell| cell.set(0));
        if stage_fault().is_none() {
            return Ok(inspected);
        }
        STAGE_PREWRITE_LEN.with(|cell| cell.set(file.metadata().ok().map(|meta| meta.len())));
        match stage_fault() {
            Some(StageFault::InheritedAcl) => Ok(Descriptor {
                flags: 0,
                acl_empty: false,
                normal: BTreeMap::new(),
                revealed: BTreeMap::new(),
            }),
            Some(StageFault::UnsupportedAttr) => {
                let mut normal = BTreeMap::new();
                normal.insert("security.selinux".into(), b"injected".to_vec());
                Ok(Descriptor {
                    flags: 0,
                    acl_empty: true,
                    revealed: normal.clone(),
                    normal,
                })
            }
            Some(StageFault::PreexistingUserSame | StageFault::PreexistingUserWrong) => {
                let value: &[u8] = if stage_fault() == Some(StageFault::PreexistingUserSame) {
                    b"one"
                } else {
                    b"other"
                };
                let mut normal = BTreeMap::new();
                normal.insert("user.formless_live".into(), value.to_vec());
                Ok(Descriptor {
                    flags: 0,
                    acl_empty: true,
                    revealed: normal.clone(),
                    normal,
                })
            }
            Some(
                StageFault::FailUserSetter | StageFault::UnexpectedAttr | StageFault::FailFinal,
            ) => {
                // Desired provenance bytes make the pre-write comparison exact.
                // The receipt is not updated from the stage.
                let mut normal = BTreeMap::new();
                if let Some(metadata) = desired {
                    if let Some(value) = metadata
                        .pairs()?
                        .into_iter()
                        .find(|(name, _)| name == PROVENANCE_NAME)
                        .map(|(_, value)| value)
                    {
                        normal.insert(PROVENANCE_NAME.to_owned(), value);
                    }
                }
                Ok(Descriptor {
                    flags: 0,
                    acl_empty: true,
                    revealed: normal.clone(),
                    normal,
                })
            }
            None => Ok(inspected),
        }
    }

    pub(super) fn prepare_user_create(
        file: &std::fs::File,
        name: &str,
        value: &[u8],
    ) -> Result<()> {
        if stage_fault() != Some(StageFault::FailUserSetter) {
            return Ok(());
        }
        record_hook_len(file);
        set_fd_attribute(file, name, value, 0)?;
        STAGE_HOOK_REACHED.with(|cell| cell.set(true));
        Ok(())
    }

    pub(super) fn begin_verify(file: &std::fs::File) -> Result<u8> {
        let step = STAGE_VERIFY_STEP.with(|cell| {
            let step = cell.get();
            cell.set(step.saturating_add(1));
            step
        });
        let plant = match (stage_fault(), step) {
            (Some(StageFault::UnexpectedAttr), 0) => Some("user.formless_unexpected"),
            (Some(StageFault::FailFinal), 1) => Some("user.formless_final"),
            _ => None,
        };
        if let Some(name) = plant {
            record_hook_len(file);
            set_owned_attribute_for_test(file, name, b"x")?;
            STAGE_HOOK_REACHED.with(|cell| cell.set(true));
        }
        Ok(step)
    }

    pub(super) fn override_verify(
        staged: Option<UnixMetadata>,
        desired: Option<&UnixMetadata>,
        step: u8,
    ) -> Result<Option<UnixMetadata>> {
        match (stage_fault(), step) {
            (Some(StageFault::FailFinal), 0) => Ok(desired.cloned()),
            (Some(StageFault::FailFinal), _) => {
                let platform = match desired {
                    Some(metadata) => metadata.platform,
                    None => UnixPlatform::current()?,
                };
                Ok(Some(UnixMetadata::try_new(
                    platform,
                    vec![("user.formless_final_mismatch".into(), b"no".to_vec())],
                )?))
            }
            _ => Ok(staged),
        }
    }

    pub(super) fn injected_capture_scan(file: &std::fs::File) -> Option<CaptureScan> {
        let fault = CAPTURE_FAULT.with(|cell| cell.get())?;
        let call = CAPTURE_CALLS.with(|cell| {
            let call = cell.get();
            cell.set(call.saturating_add(1));
            call
        });
        match fault {
            CaptureFault::OversizedList | CaptureFault::UnreadableValue => {
                Some(CaptureScan::Unverifiable)
            }
            CaptureFault::SecondScanFails if call > 0 => Some(CaptureScan::Unverifiable),
            CaptureFault::SecondScanDiffers if call > 0 => Some(match inspect_extended(file) {
                Ok(descriptor) => CaptureScan::Stable(descriptor.with_extra_user_for_test()),
                Err(_) => CaptureScan::Unverifiable,
            }),
            _ => None,
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use fault_inject::{
    capture_inspect_calls, install_capture_fault, install_stage_fault, stage_hook_len,
    stage_hook_reached, stage_prewrite_len, CaptureFault, StageFault,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_user(pairs: Vec<(String, Vec<u8>)>) -> Result<UnixMetadata> {
        UnixMetadata::try_new(UnixPlatform::Linux, pairs)
    }

    #[test]
    fn bounds_reject_before_an_oversize_buffer_is_required() {
        assert!(bounded_len(-1, 10).is_err());
        assert!(bounded_len(11, 10).is_err());
        assert_eq!(bounded_len(0, 10).unwrap(), 0);
        assert_eq!(bounded_len(10, 10).unwrap(), 10);
    }

    #[test]
    fn name_lists_reject_malformed_duplicates_and_count_overflow() {
        assert_eq!(
            parse_names(b"user.b\0user.a\0").unwrap(),
            vec!["user.b".to_string(), "user.a".to_string()]
        );
        assert!(parse_names(b"user.a").is_err());
        assert!(parse_names(b"user.a\0\0user.b\0").is_err());
        assert!(parse_names(b"\xff\0").is_err());
        assert!(canonical_names(b"user.a\0user.a\0").is_err());
        let mut packed = Vec::new();
        for index in 0..MAX_UNIX_ATTRS + 1 {
            packed.extend(format!("user.n{index:02}").as_bytes());
            packed.push(0);
        }
        assert!(canonical_names(&packed).is_err());
    }

    #[test]
    fn adversarial_records_fail_closed() {
        let meta = linux_user(vec![("user.a".into(), b"a".to_vec())]).unwrap();
        assert_eq!(
            meta.pairs().unwrap(),
            vec![("user.a".into(), b"a".to_vec())]
        );
        let mut value = serde_json::to_value(&meta).unwrap();
        value["entries"][0]["value_b64"] = serde_json::json!("YQ");
        let parsed: UnixMetadata = serde_json::from_value(value.clone()).unwrap();
        assert!(parsed.validate_stored().is_err());
        value["entries"][0]["value_b64"] = serde_json::json!("YQ==");
        value["entries"] = serde_json::json!([
            {"name": "user.b", "value_b64": ""},
            {"name": "user.a", "value_b64": ""}
        ]);
        assert!(serde_json::from_value::<UnixMetadata>(value.clone())
            .unwrap()
            .validate_stored()
            .is_err());
        value["entries"] = serde_json::json!([
            {"name": "user.a", "value_b64": ""},
            {"name": "user.a", "value_b64": ""}
        ]);
        assert!(serde_json::from_value::<UnixMetadata>(value.clone())
            .unwrap()
            .validate_stored()
            .is_err());
        value["entries"] = serde_json::json!([]);
        assert!(serde_json::from_value::<UnixMetadata>(value.clone())
            .unwrap()
            .validate_stored()
            .is_err());
        value = serde_json::to_value(&meta).unwrap();
        value["version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<UnixMetadata>(value.clone())
            .unwrap()
            .validate_stored()
            .unwrap_err()
            .to_string()
            .contains("Unsupported Unix metadata version"));
        value = serde_json::to_value(&meta).unwrap();
        value["platform"] = serde_json::json!("windows");
        assert!(serde_json::from_value::<UnixMetadata>(value.clone()).is_err());
        value = serde_json::to_value(&meta).unwrap();
        value["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<UnixMetadata>(value).is_err());
        assert!(linux_user(vec![
            ("user.b".into(), Vec::new()),
            ("user.a".into(), Vec::new()),
        ])
        .is_err());
        assert!(linux_user(vec![(PROVENANCE_NAME.into(), b"x".to_vec())]).is_err());
        let mut counted = Vec::new();
        for index in 0..MAX_UNIX_ATTRS + 1 {
            counted.push((format!("user.n{index:02}"), Vec::new()));
        }
        assert!(linux_user(counted).is_err());
        let mut counted = Vec::new();
        for index in 0..MAX_UNIX_ATTRS {
            counted.push((format!("user.n{index:02}"), Vec::new()));
        }
        assert!(linux_user(counted).is_ok());
        let ok_name = format!("user.{}", "a".repeat(MAX_UNIX_NAME_BYTES - "user.".len()));
        assert_eq!(ok_name.len(), MAX_UNIX_NAME_BYTES);
        assert!(linux_user(vec![(ok_name, Vec::new())]).is_ok());
        let bad_name = format!(
            "user.{}",
            "b".repeat(MAX_UNIX_NAME_BYTES - "user.".len() + 1)
        );
        assert!(linux_user(vec![(bad_name, Vec::new())]).is_err());
        assert!(linux_user(vec![("user.a".into(), vec![1; MAX_UNIX_VALUE_BYTES])]).is_ok());
        assert!(linux_user(vec![("user.a".into(), vec![1; MAX_UNIX_VALUE_BYTES + 1])]).is_err());
        let heavy = ["user.a", "user.b", "user.c", "user.d"]
            .into_iter()
            .map(|name| (name.to_string(), vec![1u8; 1024]))
            .collect();
        assert!(linux_user(heavy).is_err());
        assert!(linux_user(vec![
            ("user.a".into(), vec![1u8; 1024]),
            ("user.b".into(), vec![1u8; 1024]),
            ("user.c".into(), vec![1u8; 1024]),
        ])
        .is_ok());
        let empty = linux_user(vec![("user.empty".into(), Vec::new())]).unwrap();
        assert_eq!(empty.pairs().unwrap()[0].1, Vec::<u8>::new());
        let stored = empty.retained_bytes();
        assert!(stored >= "user.empty".len());
        let foreign = if cfg!(target_os = "linux") {
            UnixMetadata::try_new(
                UnixPlatform::Macos,
                vec![("user.foreign".into(), b"x".to_vec())],
            )
            .unwrap()
        } else {
            linux_user(vec![("user.foreign".into(), b"x".to_vec())]).unwrap()
        };
        assert!(foreign.validate_stored().is_ok());
        assert!(foreign.validate_current().is_err());
    }

    #[test]
    fn unsupported_views_are_not_exact_maps() {
        let empty = BTreeMap::new();
        let flags = assess(UnixPlatform::Macos, 1, true, &empty, &empty).unwrap();
        assert!(matches!(flags, Restoration::Unsupported(message) if message.contains("BSD")));
        let acl = assess(UnixPlatform::Macos, 0, false, &empty, &empty).unwrap();
        assert!(matches!(acl, Restoration::Unsupported(message) if message.contains("ACL")));
        let mut hidden = BTreeMap::new();
        hidden.insert("com.apple.decmpfs".into(), b"hidden".to_vec());
        let mismatch = assess(UnixPlatform::Macos, 0, true, &empty, &hidden).unwrap();
        assert!(
            matches!(mismatch, Restoration::Unsupported(message) if message.contains("Hidden"))
        );
        for name in [
            "com.apple.quarantine",
            "com.apple.decmpfs",
            "com.apple.ResourceFork",
            "com.apple.FinderInfo",
            "com.apple.provenance.extra",
            "security.selinux",
            "system.posix_acl_access",
            "trusted.formless",
        ] {
            let mut map = BTreeMap::new();
            map.insert(name.to_string(), b"x".to_vec());
            for platform in [UnixPlatform::Linux, UnixPlatform::Macos] {
                assert!(
                    matches!(
                        assess(platform, 0, true, &map, &map).unwrap(),
                        Restoration::Unsupported(_)
                    ),
                    "{platform:?} {name}"
                );
            }
        }
        assert!(matches!(
            assess(UnixPlatform::Linux, 0, true, &empty, &empty).unwrap(),
            Restoration::Exact(None)
        ));
        let mut user = BTreeMap::new();
        user.insert("user.a".into(), Vec::new());
        assert!(matches!(
            assess(UnixPlatform::Linux, 0, true, &user, &user).unwrap(),
            Restoration::Exact(Some(_))
        ));
        let mut provenance = BTreeMap::new();
        provenance.insert(PROVENANCE_NAME.into(), b"abcdefghijk".to_vec());
        match assess(UnixPlatform::Macos, 0, true, &provenance, &provenance).unwrap() {
            Restoration::Exact(Some(metadata)) => {
                assert_eq!(metadata.names().collect::<Vec<_>>(), vec![PROVENANCE_NAME]);
            }
            other => panic!("provenance must be exact on macos, got {other:?}"),
        }
        assert!(matches!(
            assess(UnixPlatform::Linux, 0, true, &provenance, &provenance).unwrap(),
            Restoration::Unsupported(_)
        ));
        let provenance = UnixMetadata::try_new(
            UnixPlatform::Macos,
            vec![(PROVENANCE_NAME.into(), b"abcdefghijk".to_vec())],
        )
        .unwrap();
        #[cfg(target_os = "macos")]
        assert!(provenance.validate_current().is_ok());
        #[cfg(not(target_os = "macos"))]
        assert!(provenance.validate_current().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stage_name_swap_symlink_and_hardlink_are_not_publishable() {
        let root = std::env::temp_dir().join(format!("fl-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("stage");
        std::fs::write(&path, b"original").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let parent = std::fs::File::open(&root).unwrap();
        let name = std::ffi::CString::new("stage").unwrap();
        assert!(stage_ready_to_publish(&parent, &name, &file).unwrap());
        std::fs::rename(&path, root.join("moved")).unwrap();
        std::fs::write(&path, b"foreign").unwrap();
        assert!(!stage_ready_to_publish(&parent, &name, &file).unwrap());
        assert!(!same_directory_inode(&parent, &name, &file).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign");
        assert_eq!(std::fs::read(root.join("moved")).unwrap(), b"original");
        std::os::unix::fs::symlink("moved", root.join("link")).unwrap();
        let link = std::ffi::CString::new("link").unwrap();
        assert!(!stage_ready_to_publish(&parent, &link, &file).unwrap());
        std::fs::hard_link(root.join("moved"), root.join("hard")).unwrap();
        let moved = std::ffi::CString::new("moved").unwrap();
        assert!(!stage_ready_to_publish(&parent, &moved, &file).unwrap());
        assert!(same_directory_inode(&parent, &moved, &file).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_second_user_create_fails_without_replacing_the_value() {
        let root = std::env::temp_dir().join(format!("fl-xattr-create-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("once");
        std::fs::write(&path, b"body").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let record = UnixMetadata::try_new(
            UnixPlatform::current().unwrap(),
            vec![("user.once".into(), b"keep".to_vec())],
        )
        .unwrap();
        restore_user_attributes(&file, Some(&record)).unwrap();
        assert!(restore_user_attributes(&file, Some(&record)).is_err());
        let viewed = inspect_extended(&file)
            .unwrap()
            .require_exact()
            .unwrap()
            .unwrap();
        assert!(viewed
            .pairs()
            .unwrap()
            .iter()
            .any(|(name, value)| name == "user.once" && value == b"keep"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn restore_does_not_write_a_forged_provenance_value() {
        let root = std::env::temp_dir().join(format!("fl-prov-restore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("stamped");
        std::fs::write(&path, b"body").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let before = inspect_extended(&file)
            .unwrap()
            .require_exact()
            .unwrap()
            .map(|metadata| metadata.pairs().unwrap());
        let forged = b"FORGEDPROV!".to_vec();
        let record = UnixMetadata::try_new(
            UnixPlatform::Macos,
            vec![
                (PROVENANCE_NAME.into(), forged.clone()),
                ("user.kept".into(), b"v".to_vec()),
            ],
        )
        .unwrap();
        restore_user_attributes(&file, Some(&record)).unwrap();
        let after = inspect_extended(&file)
            .unwrap()
            .require_exact()
            .unwrap()
            .unwrap()
            .pairs()
            .unwrap();
        assert!(after
            .iter()
            .any(|(name, value)| name == "user.kept" && value == b"v"));
        assert!(after
            .iter()
            .all(|(name, value)| name != PROVENANCE_NAME || value != &forged));
        if let Some(before) = before.as_ref().and_then(|pairs| {
            pairs
                .iter()
                .find(|(name, _)| name == PROVENANCE_NAME)
                .map(|(_, value)| value.clone())
        }) {
            assert!(after
                .iter()
                .any(|(name, value)| name == PROVENANCE_NAME && value == &before));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nonzero_bsd_flags_are_refused_without_being_cleared() {
        let root = std::env::temp_dir().join(format!("fl-flags-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("flagged");
        std::fs::write(&path, b"exact").unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        use std::os::fd::AsRawFd;
        // SAFETY: the fd is a regular file we created. UF_NODUMP is a user file flag.
        let rc = unsafe { libc::fchflags(file.as_raw_fd(), 0x1) };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        let descriptor = inspect_extended(&file).unwrap();
        assert!(matches!(
            descriptor.restoration().unwrap(),
            Restoration::Unsupported(message) if message.contains("BSD")
        ));
        assert_ne!(bsd_flags(&file).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn retained_bytes_counts_struct_vec_and_string_capacity() {
        assert_eq!(MAX_UNIX_ATTRS, 16);
        assert_eq!(MAX_UNIX_NAME_BYTES, 127);
        assert_eq!(MAX_UNIX_VALUE_BYTES, 1024);
        assert_eq!(MAX_UNIX_AGGREGATE_BYTES, 4096);
        let mut name = String::with_capacity(64);
        name.push_str("user.a");
        assert!(name.capacity() > name.len());
        let meta = linux_user(vec![(name, b"hello".to_vec())]).unwrap();
        let entry = &meta.entries[0];
        assert!(entry.name.capacity() > entry.name.len());
        let lens = entry.name.len() + entry.value_b64.len();
        let lower = std::mem::size_of::<UnixMetadata>()
            + meta.entries.capacity() * std::mem::size_of::<UnixAttr>()
            + entry.name.capacity()
            + entry.value_b64.capacity();
        assert!(meta.retained_bytes() > lens);
        assert!(meta.retained_bytes() >= lower);
    }

    #[test]
    fn provenance_predicates_refuse_missing_forged_and_unexpected_maps() {
        let stamp = b"opaque-stamp".to_vec();
        let desired = UnixMetadata::try_new(
            UnixPlatform::Macos,
            vec![(PROVENANCE_NAME.into(), stamp.clone())],
        )
        .unwrap();
        let empty = BTreeMap::new();
        let mut matching = BTreeMap::new();
        matching.insert(PROVENANCE_NAME.into(), stamp.clone());
        preexisting_eligible(
            UnixPlatform::Macos,
            0,
            true,
            &matching,
            &matching,
            Some(&desired),
        )
        .unwrap();
        match assess(UnixPlatform::Macos, 0, true, &matching, &matching).unwrap() {
            Restoration::Exact(map) => assert_eq!(map.as_ref(), Some(&desired)),
            other => panic!("matching provenance must be exact, got {other:?}"),
        }
        // Absence is eligible only because the stamp may appear later. It is not a match.
        preexisting_eligible(UnixPlatform::Macos, 0, true, &empty, &empty, Some(&desired)).unwrap();
        match assess(UnixPlatform::Macos, 0, true, &empty, &empty).unwrap() {
            Restoration::Exact(map) => assert_ne!(map.as_ref(), Some(&desired)),
            other => panic!("missing provenance is not the desired stamp, got {other:?}"),
        }
        let missing =
            preexisting_eligible(UnixPlatform::Macos, 0, true, &matching, &matching, None)
                .unwrap_err();
        assert!(missing.to_string().contains("provenance"), "{missing}");
        assert!(
            missing
                .to_string()
                .contains("refusing before any desired byte"),
            "{missing}"
        );
        let mut forged = matching.clone();
        forged.get_mut(PROVENANCE_NAME).unwrap().push(0xff);
        let forged_error = preexisting_eligible(
            UnixPlatform::Macos,
            0,
            true,
            &forged,
            &forged,
            Some(&desired),
        )
        .unwrap_err();
        assert!(
            forged_error.to_string().contains("provenance"),
            "{forged_error}"
        );
        match assess(UnixPlatform::Macos, 0, true, &forged, &forged).unwrap() {
            Restoration::Exact(map) => assert_ne!(map.as_ref(), Some(&desired)),
            other => panic!("forged provenance must not compare equal, got {other:?}"),
        }
        let mut unexpected = matching.clone();
        unexpected.insert("com.apple.provenance.extra".into(), b"x".to_vec());
        let unexpected_error = preexisting_eligible(
            UnixPlatform::Macos,
            0,
            true,
            &unexpected,
            &unexpected,
            Some(&desired),
        )
        .unwrap_err();
        assert!(
            unexpected_error
                .to_string()
                .contains("Unexpected stage attribute"),
            "{unexpected_error}"
        );
        let mut userish = BTreeMap::new();
        userish.insert("user.com.apple.provenance".into(), b"x".to_vec());
        assert!(preexisting_eligible(
            UnixPlatform::Macos,
            0,
            true,
            &userish,
            &userish,
            Some(&desired),
        )
        .is_err());
        let acl = preexisting_eligible(
            UnixPlatform::Macos,
            0,
            false,
            &matching,
            &matching,
            Some(&desired),
        )
        .unwrap_err();
        assert!(acl.to_string().contains("Inherited ACL"), "{acl}");
        let flags = preexisting_eligible(
            UnixPlatform::Macos,
            1,
            true,
            &matching,
            &matching,
            Some(&desired),
        )
        .unwrap_err();
        assert!(flags.to_string().contains("BSD"), "{flags}");
        let mut revealed = matching.clone();
        revealed.insert("com.apple.decmpfs".into(), b"h".to_vec());
        let hidden = preexisting_eligible(
            UnixPlatform::Macos,
            0,
            true,
            &matching,
            &revealed,
            Some(&desired),
        )
        .unwrap_err();
        assert!(hidden.to_string().contains("hidden"), "{hidden}");
        let desired_user = UnixMetadata::try_new(
            UnixPlatform::Linux,
            vec![("user.keep".into(), b"wanted".to_vec())],
        )
        .unwrap();
        preexisting_eligible(
            UnixPlatform::Linux,
            0,
            true,
            &empty,
            &empty,
            Some(&desired_user),
        )
        .unwrap();
        let mut same = BTreeMap::new();
        same.insert("user.keep".into(), b"wanted".to_vec());
        let same_error = preexisting_eligible(
            UnixPlatform::Linux,
            0,
            true,
            &same,
            &same,
            Some(&desired_user),
        )
        .unwrap_err();
        assert!(
            same_error
                .to_string()
                .contains("Preexisting user attribute"),
            "{same_error}"
        );
        assert!(
            same_error
                .to_string()
                .contains("refusing before any desired byte"),
            "{same_error}"
        );
        let mut different = BTreeMap::new();
        different.insert("user.keep".into(), b"other".to_vec());
        let different_error = preexisting_eligible(
            UnixPlatform::Linux,
            0,
            true,
            &different,
            &different,
            Some(&desired_user),
        )
        .unwrap_err();
        assert!(
            different_error
                .to_string()
                .contains("Preexisting user attribute"),
            "{different_error}"
        );
        assert!(
            different_error
                .to_string()
                .contains("refusing before any desired byte"),
            "{different_error}"
        );
        let mut extra = different.clone();
        extra.insert("user.other".into(), b"x".to_vec());
        assert!(preexisting_eligible(
            UnixPlatform::Linux,
            0,
            true,
            &extra,
            &extra,
            Some(&desired_user),
        )
        .is_err());
        preexisting_eligible(UnixPlatform::Linux, 0, true, &empty, &empty, None).unwrap();
        preexisting_eligible(UnixPlatform::Macos, 0, true, &empty, &empty, None).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unverifiable_or_disagreed_scans_do_not_grant_capture_authority() {
        let empty = BTreeMap::new();
        let stable = CaptureScan::Stable(Descriptor {
            flags: 0,
            acl_empty: true,
            normal: empty.clone(),
            revealed: empty,
        });
        let failed = CaptureScan::Unverifiable;
        for (before, after) in [(&failed, &failed), (&stable, &failed), (&failed, &stable)] {
            let (map, complete) = capture_authority(before, after, true).unwrap();
            assert!(map.is_none());
            assert!(!complete);
            assert!(capture_authority(before, after, false).is_err());
        }
        let mut raced = BTreeMap::new();
        raced.insert("user.formless_scan_race".into(), b"race".to_vec());
        let other = CaptureScan::Stable(Descriptor {
            flags: 0,
            acl_empty: true,
            normal: raced.clone(),
            revealed: raced,
        });
        let error = capture_authority(&stable, &other, true).unwrap_err();
        assert!(
            error.to_string().contains("changed while reading"),
            "{error}"
        );
        assert!(capture_authority(&stable, &other, false).is_err());
        assert!(bounded_len((MAX_LIST_BYTES as isize) + 1, MAX_LIST_BYTES).is_err());
        assert!(bounded_len((MAX_UNIX_VALUE_BYTES as isize) + 1, MAX_UNIX_VALUE_BYTES).is_err());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut user = BTreeMap::new();
            user.insert("user.a".into(), b"a".to_vec());
            let exact = CaptureScan::Stable(Descriptor {
                flags: 0,
                acl_empty: true,
                normal: user.clone(),
                revealed: user,
            });
            let (map, complete) = capture_authority(&exact, &exact, true).unwrap();
            assert!(complete);
            assert_eq!(map.unwrap().names().collect::<Vec<_>>(), vec!["user.a"]);
            let mut unsupported = BTreeMap::new();
            unsupported.insert("security.selinux".into(), b"x".to_vec());
            let denied = CaptureScan::Stable(Descriptor {
                flags: 0,
                acl_empty: true,
                normal: unsupported.clone(),
                revealed: unsupported,
            });
            let (map, complete) = capture_authority(&denied, &denied, true).unwrap();
            assert!(map.is_none() && !complete);
            assert!(capture_authority(&denied, &denied, false).is_err());
        }
    }
}
