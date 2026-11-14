//! Private bounded before/after images. Audit entries must never include this
//! type's serialization: only IDs, hashes, phase and display path labels.
use crate::{hunk_review::MAX_REVIEW_BYTES, review_file_io::ReviewImage, Paths};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
const MAX_RECEIPT_BYTES: usize = 8 * 1024 * 1024;
const MAX_WHOLE_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RECEIPTS: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum Phase {
    Prepared,
    Applied,
    Undone,
    Ambiguous,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Image {
    base64: Option<String>,
    mode: Option<u32>,
    readonly: bool,
    owner: Option<u32>,
    group: Option<u32>,
    security: Option<String>,
    integrity: String,
    security_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    windows_identity: Option<crate::review_file_io::WindowsIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unix_metadata: Option<crate::review_unix_metadata::UnixMetadata>,
}
impl Image {
    fn capture(image: &ReviewImage) -> Result<Self> {
        Self::capture_bounded(image, MAX_REVIEW_BYTES)
    }
    fn capture_bounded(image: &ReviewImage, limit: usize) -> Result<Self> {
        if image.bytes.as_ref().is_some_and(|b| b.len() > limit) {
            bail!("Review image exceeds journal byte limit");
        }
        if image
            .security
            .as_ref()
            .is_some_and(|bytes| bytes.len() > 65536)
        {
            bail!("Security metadata exceeds review receipt limit");
        }
        image.ensure_consistent()?;
        Ok(Self {
            integrity: image.integrity_hash()?,
            security_complete: image.security_complete,
            windows_identity: image.windows_identity.clone(),
            base64: image
                .bytes
                .as_ref()
                .map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
            mode: image.mode,
            readonly: image.readonly,
            owner: image.owner,
            group: image.group,
            security: image
                .security
                .as_ref()
                .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
            unix_metadata: image.unix_metadata.clone(),
        })
    }
    fn decode_bounded(&self, limit: usize) -> Result<ReviewImage> {
        let bytes = self
            .base64
            .as_ref()
            .map(|encoded| {
                if encoded.len() > limit.div_ceil(3) * 4 {
                    bail!("Review journal image exceeds byte limit");
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                if bytes.len() > limit {
                    bail!("Review journal decoded image exceeds byte limit");
                }
                Ok(bytes)
            })
            .transpose()?;
        let security = self
            .security
            .as_ref()
            .map(|encoded| {
                if encoded.len() > 65536usize.div_ceil(3) * 4 {
                    bail!("Security metadata exceeds review receipt limit");
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                if bytes.len() > 65536 {
                    bail!("Security metadata exceeds review receipt limit");
                }
                Ok::<_, anyhow::Error>(bytes)
            })
            .transpose()?;
        if bytes.is_none()
            && (self.mode.is_some()
                || self.readonly
                || self.owner.is_some()
                || self.group.is_some()
                || security.is_some()
                || self.windows_identity.is_some()
                || self.unix_metadata.is_some())
        {
            bail!("Missing review image contains invalid mode metadata");
        }
        let image = ReviewImage {
            bytes,
            mode: self.mode,
            readonly: self.readonly,
            owner: self.owner,
            group: self.group,
            security,
            security_complete: self.security_complete,
            windows_identity: self.windows_identity.clone(),
            unix_metadata: self.unix_metadata.clone(),
            ..Default::default()
        };
        if image.integrity_hash()? != self.integrity {
            bail!("Private recovery image integrity mismatch");
        }
        image.ensure_consistent()?;
        Ok(image)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    format_version: u32,
    integrity: String,
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub file_id: String,
    pub hunk_id: String,
    pub path: String,
    pub previous_decision: String,
    pub phase: Phase,
    pub created_at: String,
    sequence: u64,
    platform: String,
    before: Image,
    after: Image,
    /// Namespace recovery authority is private and independent of target
    /// existence/metadata equality. Old receipts have no retained object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained: Option<crate::review_retained::RetainedToken>,
}
impl Receipt {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: &str,
        run: &str,
        file: &str,
        hunk: &str,
        path: &str,
        previous_decision: &str,
        before: &ReviewImage,
        after: &ReviewImage,
    ) -> Result<Self> {
        let mut receipt = Self {
            format_version: 1,
            integrity: String::new(),
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session.into(),
            run_id: run.into(),
            file_id: file.into(),
            hunk_id: hunk.into(),
            path: path.into(),
            previous_decision: previous_decision.into(),
            phase: Phase::Prepared,
            created_at: chrono::Utc::now().to_rfc3339(),
            sequence: 0,
            platform: std::env::consts::OS.into(),
            before: Image::capture(before)?,
            after: Image::capture(after)?,
            retained: None,
        };
        receipt.refresh_integrity()?;
        Ok(receipt)
    }
    /// Whole-file rewind keeps the existing two-MiB snapshot limit without
    /// widening the interactive constructor's 256-KiB image allowance.
    pub fn new_whole_file(
        session: &str,
        run: &str,
        path: &str,
        before: &ReviewImage,
        after: &ReviewImage,
    ) -> Result<Self> {
        let mut receipt = Self::new(
            session,
            run,
            "whole-file",
            "whole-file",
            path,
            "pending",
            &ReviewImage::missing(),
            &ReviewImage::missing(),
        )?;
        receipt.format_version = 2;
        receipt.before = Image::capture_bounded(before, MAX_WHOLE_FILE_BYTES)?;
        receipt.after = Image::capture_bounded(after, MAX_WHOLE_FILE_BYTES)?;
        receipt.refresh_integrity()?;
        Ok(receipt)
    }
    /// Bind trusted newly-created action metadata before callers inspect its
    /// images. The initial checksum is verified first; disk corruption is never
    /// repaired by this builder, and committed receipts cannot be rebound.
    pub fn bind_action(
        mut self,
        id: &str,
        retained: Option<crate::review_retained::RetainedToken>,
    ) -> Result<Self> {
        self.images()?;
        if self.phase != Phase::Prepared || self.sequence != 0 {
            bail!("Only a new action can be bound");
        }
        validate_id(id)?;
        self.id = id.into();
        self.retained = retained;
        self.refresh_integrity()?;
        Ok(self)
    }
    fn checksum(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut unsigned = self.clone();
        unsigned.integrity.clear();
        let mut hash = Sha256::new();
        hash.update(b"formless-private-review-receipt-v1\0");
        hash.update(serde_json::to_vec(&unsigned)?);
        Ok(format!("{:x}", hash.finalize()))
    }
    fn refresh_integrity(&mut self) -> Result<()> {
        self.integrity = self.checksum()?;
        Ok(())
    }
    pub fn images(&self) -> Result<(ReviewImage, ReviewImage)> {
        validate_id(&self.id)?;
        if !matches!(self.format_version, 1 | 2)
            || self.integrity.len() != 64
            || self.checksum()? != self.integrity
        {
            bail!("Private review receipt integrity or format mismatch");
        }
        if self.platform != std::env::consts::OS {
            bail!("Review receipt belongs to another operating system; automatic recovery is unavailable");
        }
        if self.path.len() > 32768
            || self.session_id.len() > 256
            || self.run_id.len() > 256
            || self.file_id.len() > 256
            || self.hunk_id.len() > 256
        {
            bail!("Review receipt identity exceeds its limit");
        }
        if !matches!(self.previous_decision.as_str(), "pending" | "accepted") {
            bail!("Review receipt decision is invalid");
        }
        let limit = if self.format_version == 2 {
            MAX_WHOLE_FILE_BYTES
        } else {
            MAX_REVIEW_BYTES
        };
        Ok((
            self.before.decode_bounded(limit)?,
            self.after.decode_bounded(limit)?,
        ))
    }
    /// Read-only reconciliation. Even a prepared record can already correspond
    /// to the after-image if a reply or final phase update failed. Ambiguous
    /// content is never automatically rewritten from a remembered snapshot.
    pub fn observed_phase(&self, current: &ReviewImage) -> Result<Phase> {
        let (before, after) = self.images()?;
        if current == &after {
            Ok(Phase::Applied)
        } else if current == &before {
            Ok(
                if self.phase == Phase::Undone || self.phase == Phase::Applied {
                    Phase::Undone
                } else {
                    Phase::Prepared
                },
            )
        } else {
            Ok(Phase::Ambiguous)
        }
    }
}
fn validate_id(id: &str) -> Result<()> {
    if uuid::Uuid::parse_str(id)?.to_string() != id {
        bail!("Noncanonical review receipt ID");
    }
    Ok(())
}

pub(crate) struct Journal {
    root: PathBuf,
    _guard: File,
}
impl Journal {
    /// Read-only inspections do not create an absent journal directory.
    pub fn open(paths: &Paths, create: bool) -> Result<Option<Self>> {
        let root = paths.data_dir.join("review-receipts");
        match std::fs::symlink_metadata(&root) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => create_directory(&root)?,
            Err(e) => return Err(e.into()),
        }
        let metadata = std::fs::symlink_metadata(&root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!("Review journal must be an owned private directory");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
                bail!("Review journal directory is not owner-only");
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                bail!("Review journal directory uses a reparse point");
            }
        }
        let guard = open_directory(&root)?;
        #[cfg(windows)]
        windows_private(&guard)?;
        Ok(Some(Self {
            root,
            _guard: guard,
        }))
    }
    fn entries(&self) -> Result<Vec<(String, u64)>> {
        // Only the bounded canonical receipt namespace is accepted; no client
        // supplies a filename. Private temporary records count toward all caps.
        let mut entries = Vec::new();
        #[cfg(target_os = "linux")]
        let directory = {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", self._guard.as_raw_fd()))
        };
        #[cfg(not(target_os = "linux"))]
        let directory = self.root.clone();
        self.check_root()?;
        for entry in std::fs::read_dir(directory)? {
            if entries.len() >= MAX_RECEIPTS {
                bail!("Private review journal exceeds its file count limit");
            }
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Invalid review receipt filename"))?;
            let id = name
                .strip_suffix(".json")
                .or_else(|| name.strip_suffix(".tmp"))
                .context("Unexpected private review journal filename")?;
            validate_id(id)?;
            let file = self.open_named(&name, false)?;
            let metadata = file.metadata()?;
            if metadata.len() > MAX_RECEIPT_BYTES as u64 {
                bail!("Private review journal entry exceeds its byte limit");
            }
            if !metadata.is_file() {
                bail!("Private review journal contains a nonregular file");
            }
            entries.push((name, metadata.len()));
        }
        self.check_root()?;
        Ok(entries)
    }
    fn inventory(&self) -> Result<(usize, u64)> {
        let entries = self.entries()?;
        let bytes = entries.iter().try_fold(0u64, |sum, (_, size)| {
            sum.checked_add(*size)
                .context("Review journal size overflow")
        })?;
        Ok((entries.len(), bytes))
    }
    pub fn list(&self, session: &str, run: &str) -> Result<Vec<Receipt>> {
        let entries = self.entries()?;
        let total = entries.iter().try_fold(0u64, |sum, (_, size)| {
            sum.checked_add(*size)
                .context("Review journal size overflow")
        })?;
        if total > MAX_JOURNAL_BYTES {
            bail!("Private review journal exceeds its byte inspection limit");
        }
        let mut receipts = Vec::new();
        for (name, _) in entries {
            // Interrupted phase-update temporaries never supersede their
            // durable prepared receipt, and are never interpreted as actions.
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            let receipt = self.read(id)?;
            if receipt.session_id == session && receipt.run_id == run {
                receipts.push(receipt);
            }
        }
        receipts.sort_by_key(|receipt| receipt.sequence);
        Ok(receipts)
    }
    fn check_root(&self) -> Result<()> {
        let named = std::fs::symlink_metadata(&self.root)?;
        if named.file_type().is_symlink() || !named.is_dir() {
            bail!("Private review journal directory changed");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owned = self._guard.metadata()?;
            if (named.dev(), named.ino()) != (owned.dev(), owned.ino())
                || named.mode() & 0o077 != 0
                || named.uid() != unsafe { libc::geteuid() }
            {
                bail!("Private review journal directory identity or permissions changed");
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if named.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                bail!("Private review journal root uses a reparse point");
            }
            let named = open_directory(&self.root)?;
            if windows_identity(&named)? != windows_identity(&self._guard)? {
                bail!("Private review journal directory identity changed");
            }
        }
        Ok(())
    }
    fn open_named(&self, name: &str, create: bool) -> Result<File> {
        let id = name
            .strip_suffix(".json")
            .or_else(|| name.strip_suffix(".tmp"))
            .context("Invalid review receipt extension")?;
        validate_id(id)?;
        self.check_root()?;
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(name)?;
            let flags = if create {
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
            } else {
                libc::O_RDONLY
            };
            let fd = unsafe {
                libc::openat(
                    self._guard.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(unsafe { File::from_raw_fd(fd) })
        }
        #[cfg(windows)]
        {
            open_private(&self.root.join(name), create)
        }
    }
    pub fn read(&self, id: &str) -> Result<Receipt> {
        validate_id(id)?;
        let mut file = self.open_named(&format!("{id}.json"), false)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_RECEIPT_BYTES as u64 {
            bail!("Private review receipt is nonregular or oversized");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                bail!("Private review receipt permissions or ownership are unsafe");
            }
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_RECEIPT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            bail!("Private review receipt grew beyond its limit");
        }
        let receipt: Receipt = serde_json::from_slice(&bytes)?;
        if receipt.id != id {
            bail!("Review receipt identity mismatch");
        }
        receipt.images()?;
        Ok(receipt)
    }
    pub fn prepare(&self, receipt: &mut Receipt) -> Result<()> {
        if receipt.phase != Phase::Prepared {
            bail!("New review receipt is not prepared");
        }
        receipt.refresh_integrity()?;
        receipt.images()?;
        // The daemon serializes review mutations. A durable profile-local
        // sequence keeps undo ordering correct across wall-clock rollback.
        let mut sequence = 0;
        for (name, _) in self.entries()? {
            if let Some(id) = name.strip_suffix(".json") {
                sequence = sequence.max(self.read(id)?.sequence);
            }
        }
        receipt.sequence = sequence
            .checked_add(1)
            .context("Review journal sequence overflow")?;
        receipt.refresh_integrity()?;
        let bytes = serde_json::to_vec(receipt)?;
        let (count, total) = self.inventory()?;
        if bytes.len() > MAX_RECEIPT_BYTES
            || count >= MAX_RECEIPTS - 4
            || total + bytes.len() as u64 > MAX_JOURNAL_BYTES - MAX_RECEIPT_BYTES as u64
        {
            bail!("Private review journal capacity reached; old recovery records are retained and no file was changed");
        }
        let mut file = self.open_named(&format!("{}.json", receipt.id), true)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.sync_directory()?;
        Ok(())
    }
    pub fn set_phase(&self, expected: &Receipt, phase: Phase) -> Result<Receipt> {
        if self.read(&expected.id)? != *expected {
            bail!("Private review receipt changed before its phase update");
        }
        let mut updated = expected.clone();
        updated.phase = phase;
        updated.refresh_integrity()?;
        let bytes = serde_json::to_vec(&updated)?;
        let (count, total) = self.inventory()?;
        if bytes.len() > MAX_RECEIPT_BYTES
            || count >= MAX_RECEIPTS
            || total + bytes.len() as u64 > MAX_JOURNAL_BYTES
        {
            bail!("Private journal has no capacity for a durable phase update; original prepared receipt remains");
        }
        let temporary_name = format!("{}.tmp", uuid::Uuid::new_v4());
        let final_name = format!("{}.json", expected.id);
        let mut file = self.open_named(&temporary_name, true)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        self.check_root()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let source = std::ffi::CString::new(temporary_name.as_str())?;
            let target = std::ffi::CString::new(final_name.as_str())?;
            if unsafe {
                libc::renameat(
                    self._guard.as_raw_fd(),
                    source.as_ptr(),
                    self._guard.as_raw_fd(),
                    target.as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        #[cfg(windows)]
        {
            std::fs::rename(self.root.join(&temporary_name), self.root.join(&final_name))?;
        }
        self.sync_directory()
            .context("Receipt phase was replaced, but its directory durability is unconfirmed")?;
        Ok(updated)
    }
    fn sync_directory(&self) -> Result<()> {
        #[cfg(unix)]
        {
            self._guard.sync_all()?;
        }
        Ok(())
    }
}
#[cfg(unix)]
fn create_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}
#[cfg(windows)]
fn create_directory(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<_> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let security = crate::win_pipe::OwnerOnly::new()?;
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::CreateDirectoryW(
            wide.as_ptr(),
            security.as_raw().cast(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn open_directory(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    Ok(options.open(path)?)
}
#[cfg(windows)]
fn open_private(path: &Path, create: bool) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = OpenOptions::new();
        opts.read(!create)
            .write(create)
            .create_new(create)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        Ok(opts.open(path)?)
    }
    #[cfg(windows)]
    {
        use std::os::windows::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
            io::FromRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::*;
        let file = if create {
            let wide: Vec<_> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let security = crate::win_pipe::OwnerOnly::new()?;
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    windows_sys::Win32::Foundation::GENERIC_WRITE,
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
            unsafe { File::from_raw_handle(handle) }
        } else {
            OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?
        };
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("Private review receipt uses a reparse point");
        }
        windows_private(&file)?;
        Ok(file)
    }
}

/// Decode the SDDL text returned by
/// `ConvertSecurityDescriptorToStringSecurityDescriptorW`.
///
/// Microsoft documents `StringSecurityDescriptorLen` as the size of the
/// returned buffer in TCHARs. On Windows that value is the buffer capacity,
/// not the string length, and the capacity may contain more than one trailing
/// NUL. Only the units before the first NUL are decoded. Padding is ignored.
/// A null pointer or a capacity outside `1..=32768` is rejected before a
/// slice is created. A missing terminator, or invalid UTF-16 before it, fails
/// closed.
///
/// # Safety
/// For a non-null pointer with a valid capacity, the caller must provide an
/// readable allocation of at least `capacity` UTF-16 units, initialized through
/// the first terminator (or the entire capacity if no terminator exists).
#[cfg(any(windows, test))]
unsafe fn private_review_dacl_text(text: *const u16, capacity: u32) -> Result<String> {
    if text.is_null() {
        bail!("Private review DACL string is missing");
    }
    if capacity == 0 || capacity > 32768 {
        bail!("Private review DACL exceeds its inspection limit");
    }
    let mut units = Vec::new();
    for index in 0..capacity as usize {
        // SAFETY: the caller guarantees readable initialized units through the
        // first terminator; the checked capacity bounds every pointer offset.
        let unit = unsafe { text.add(index).read() };
        if unit == 0 {
            return String::from_utf16(&units)
                .context("Private review DACL string is not valid UTF-16");
        }
        units.push(unit);
    }
    bail!("Private review DACL string is missing its terminator")
}

/// Exact `(ACE)` spellings. The set comparison is unchanged: order does not
/// matter, and a missing or unclosed ACE is rejected.
#[cfg(any(windows, test))]
fn private_review_dacl_aces(text: &str) -> Result<std::collections::BTreeSet<String>> {
    let first = text
        .find('(')
        .context("Private review DACL has no explicit grants")?;
    let mut aces = std::collections::BTreeSet::new();
    let mut remaining = &text[first..];
    while !remaining.is_empty() {
        let end = remaining.find(')').context("Invalid private review DACL")?;
        aces.insert(remaining[..=end].to_string());
        remaining = &remaining[end + 1..];
    }
    Ok(aces)
}

#[cfg(windows)]
pub(crate) fn windows_private(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{Authorization::*, *},
    };
    struct Allocation(*mut std::ffi::c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    fn dacl(sd: PSECURITY_DESCRIPTOR) -> Result<std::collections::BTreeSet<String>> {
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if control & SE_DACL_PROTECTED == 0 {
            bail!("Private review storage has an inheritable DACL");
        }
        let mut text = std::ptr::null_mut();
        let mut length: u32 = 0;
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                &mut length,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let allocation = Allocation(text.cast());
        // `allocation` drops on both failures below, so LocalFree runs before
        // either error returns. The decoded string is owned and outlives it.
        // SAFETY: successful Win32 conversion owns the reported allocation
        // and returns a NUL-terminated UTF-16 string within that capacity.
        let decoded = unsafe { private_review_dacl_text(text, length) }?;
        let aces = private_review_dacl_aces(&decoded)?;
        drop(allocation);
        Ok(aces)
    }
    windows_identity(file)?;
    // Pipes keep OwnerOnly::new (GA). A file or directory stores that mask as FA.
    let expected = crate::win_pipe::OwnerOnly::persisted_file_dacl()?;
    let attrs = unsafe { &*expected.as_raw().cast::<SECURITY_ATTRIBUTES>() };
    let expected = dacl(attrs.lpSecurityDescriptor)?;
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32).into());
    }
    let allocation = Allocation(descriptor);
    let actual = dacl(descriptor)?;
    drop(allocation);
    if actual != expected {
        bail!("Private review storage grants access beyond the current owner and SYSTEM");
    }
    Ok(())
}

#[cfg(windows)]
fn windows_identity(file: &File) -> Result<(u32, u32, u32)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let info = unsafe { info.assume_init() };
    if file.metadata()?.is_file() && info.nNumberOfLinks != 1 {
        bail!("Private review receipt uses multiple hard links");
    }
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
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
    fn fixture() -> (Fixture, Paths) {
        let dir = Fixture(std::env::temp_dir().join(format!(
            "fl-private-review-journal-{}",
            uuid::Uuid::new_v4()
        )));
        std::fs::create_dir(&dir.0).unwrap();
        let paths = Paths {
            data_dir: dir.0.join("data"),
            config_dir: dir.0.join("config"),
            profile: "private-review-fixture".into(),
        };
        (dir, paths)
    }
    fn image(text: &[u8]) -> ReviewImage {
        ReviewImage {
            bytes: Some(text.to_vec()),
            mode: Some(if cfg!(unix) { 0o100600 } else { 32 }),
            readonly: false,
            unix_metadata: None,
            ..Default::default()
        }
    }
    fn receipt(before: &ReviewImage, after: &ReviewImage) -> Receipt {
        Receipt::new(
            "session",
            "run",
            "file",
            "hunk",
            "/private-fixture/not-opened.txt",
            "pending",
            before,
            after,
        )
        .unwrap()
    }
    #[test]
    fn whole_file_receipts_preserve_large_images_without_widening_interactive_limits() {
        let (_dir, paths) = fixture();
        std::fs::create_dir(&paths.data_dir).unwrap();
        let journal = Journal::open(&paths, true).unwrap().unwrap();
        let before = image(&vec![b'a'; 300 * 1024]);
        let after = image(&vec![b'b'; 300 * 1024]);
        assert!(Receipt::new(
            "s",
            "r",
            "f",
            "h",
            "/not-opened",
            "pending",
            &before,
            &after
        )
        .is_err());
        let mut whole = Receipt::new_whole_file("s", "r", "/not-opened", &before, &after).unwrap();
        journal.prepare(&mut whole).unwrap();
        let loaded = journal.read(&whole.id).unwrap();
        assert_eq!(loaded.images().unwrap(), (before.clone(), after.clone()));
        assert_eq!(loaded.observed_phase(&before).unwrap(), Phase::Prepared);
        assert_eq!(loaded.observed_phase(&after).unwrap(), Phase::Applied);
        let applied = journal.set_phase(&loaded, Phase::Applied).unwrap();
        assert_eq!(journal.read(&applied.id).unwrap().phase, Phase::Applied);
        assert!(Receipt::new_whole_file(
            "s",
            "r",
            "/not-opened",
            &image(&vec![0; 2 * 1024 * 1024 + 1]),
            &after
        )
        .is_err());
    }
    #[test]
    fn binding_new_action_preserves_valid_images_and_never_reseals_a_corrupt_receipt() {
        let before = image(b"before");
        let after = image(b"after");
        let original = receipt(&before, &after);
        let action = original
            .clone()
            .bind_action(&uuid::Uuid::new_v4().to_string(), None)
            .unwrap();
        assert_eq!(action.images().unwrap(), (before, after));
        let mut corrupt = original;
        corrupt.path.push_str("corrupted");
        assert!(corrupt
            .bind_action(&uuid::Uuid::new_v4().to_string(), None)
            .is_err());
    }
    #[test]
    fn readonly_inspection_does_not_create_absent_profile_or_journal() {
        let (_dir, paths) = fixture();
        assert!(Journal::open(&paths, false).unwrap().is_none());
        assert!(!paths.data_dir.exists());
        assert!(!paths.config_dir.exists());
    }
    #[test]
    fn private_prepared_receipt_survives_reopen_and_reconciles_lost_phase_reply() {
        let (_dir, paths) = fixture();
        std::fs::create_dir(&paths.data_dir).unwrap();
        let before = image(b"exact before\r\nEOF");
        let after = image(b"exact after\r\nEOF");
        let mut action = receipt(&before, &after);
        {
            let journal = Journal::open(&paths, true).unwrap().unwrap();
            journal.prepare(&mut action).unwrap();
            assert_eq!(journal.read(&action.id).unwrap(), action);
        }
        let journal = Journal::open(&paths, false).unwrap().unwrap();
        let prepared = journal.read(&action.id).unwrap();
        assert_eq!(
            prepared.observed_phase(&after).unwrap(),
            Phase::Applied,
            "filesystem may commit before final phase reply"
        );
        let applied = journal.set_phase(&prepared, Phase::Applied).unwrap();
        assert_eq!(
            applied.observed_phase(&before).unwrap(),
            Phase::Undone,
            "undo may finish before its final phase reply"
        );
        assert_eq!(
            applied.observed_phase(&image(b"outside edit")).unwrap(),
            Phase::Ambiguous
        );
        let undone = journal.set_phase(&applied, Phase::Undone).unwrap();
        assert_eq!(journal.read(&action.id).unwrap(), undone);
        assert_eq!(undone.images().unwrap(), (before, after));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&journal.root).unwrap().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(journal.root.join(format!("{}.json", action.id)))
                    .unwrap()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(journal.read("../../outside").is_err());
    }
    #[test]
    fn monotonic_receipt_sequence_survives_clock_rollback_and_pending_temporary_files() {
        let (_dir, paths) = fixture();
        std::fs::create_dir(&paths.data_dir).unwrap();
        let journal = Journal::open(&paths, true).unwrap().unwrap();
        let mut first = receipt(&image(b"a"), &image(b"b"));
        first.created_at = "2050-01-01T00:00:00Z".into();
        journal.prepare(&mut first).unwrap();
        let mut second = receipt(&image(b"b"), &image(b"c"));
        second.created_at = "2000-01-01T00:00:00Z".into();
        journal.prepare(&mut second).unwrap();
        let temporary = format!("{}.tmp", uuid::Uuid::new_v4());
        journal
            .open_named(&temporary, true)
            .unwrap()
            .write_all(b"incomplete staged update")
            .unwrap();
        let ordered = journal.list("session", "run").unwrap();
        assert_eq!(
            ordered.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec![first.id.as_str(), second.id.as_str()]
        );
        assert!(journal.list("different-session", "run").unwrap().is_empty());
        assert!(journal.list("session", "different-run").unwrap().is_empty());
    }
    #[test]
    fn journal_capacity_refuses_new_action_without_discarding_old_recovery_records() {
        let (_dir, paths) = fixture();
        std::fs::create_dir(&paths.data_dir).unwrap();
        let journal = Journal::open(&paths, true).unwrap().unwrap();
        for _ in 0..MAX_RECEIPTS - 4 {
            journal
                .open_named(&format!("{}.tmp", uuid::Uuid::new_v4()), true)
                .unwrap();
        }
        let mut action = receipt(&image(b"old"), &image(b"new"));
        assert!(journal.prepare(&mut action).is_err());
        assert_eq!(journal.inventory().unwrap().0, MAX_RECEIPTS - 4);
        assert!(!journal.root.join(format!("{}.json", action.id)).exists());
        assert!(Image::capture(&image(&vec![b'x'; MAX_REVIEW_BYTES + 1])).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn private_journal_refuses_symlink_hardlink_and_public_permission_receipts() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (dir, paths) = fixture();
        std::fs::create_dir(&paths.data_dir).unwrap();
        let journal = Journal::open(&paths, true).unwrap().unwrap();
        let outside = dir.0.join("outside-private-fixture");
        std::fs::write(&outside, b"never overwritten").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let target = journal.root.join(format!("{id}.json"));
        symlink(&outside, &target).unwrap();
        assert!(journal.read(&id).is_err());
        std::fs::remove_file(&target).unwrap();
        std::fs::hard_link(&outside, &target).unwrap();
        assert!(journal.read(&id).is_err());
        std::fs::remove_file(&target).unwrap();
        let mut action = receipt(&image(b"old"), &image(b"new"));
        journal.prepare(&mut action).unwrap();
        let target = journal.root.join(format!("{}.json", action.id));
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(journal.read(&action.id).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"never overwritten");
    }
    #[test]
    fn valid_base64_preimage_corruption_and_identity_corruption_are_rejected_before_restore() {
        let (dir, paths) = fixture();
        std::fs::create_dir(&paths.data_dir).unwrap();
        let journal = Journal::open(&paths, true).unwrap().unwrap();
        let target = dir.0.join("current.txt");
        std::fs::write(&target, b"post image remains exact").unwrap();
        let mut action = receipt(
            &image(b"original pre image"),
            &image(b"post image remains exact"),
        );
        action.path = target.to_str().unwrap().into();
        journal.prepare(&mut action).unwrap();
        let file = journal.root.join(format!("{}.json", action.id));
        let original = std::fs::read(&file).unwrap();
        let mut data: serde_json::Value = serde_json::from_slice(&original).unwrap();
        data["before"]["base64"] = serde_json::json!(
            base64::engine::general_purpose::STANDARD.encode(b"corrupted pre image")
        );
        std::fs::write(&file, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(journal.read(&action.id).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"post image remains exact");
        let mut altered: Receipt = serde_json::from_value(data).unwrap();
        altered.refresh_integrity().unwrap();
        assert!(altered.images().unwrap_err().to_string().contains("image integrity"),"the independent pre-image checksum remains authoritative even if the outer checksum is recalculated");
        for key in ["session_id", "run_id", "file_id", "hunk_id", "path"] {
            let mut data: serde_json::Value = serde_json::from_slice(&original).unwrap();
            data[key] = serde_json::json!("another identity");
            std::fs::write(&file, serde_json::to_vec(&data).unwrap()).unwrap();
            assert!(journal.read(&action.id).is_err(), "{key}");
            assert_eq!(std::fs::read(&target).unwrap(), b"post image remains exact");
        }
    }
    fn dacl_units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }
    fn dacl_capacity(units: &[u16]) -> u32 {
        u32::try_from(units.len()).unwrap()
    }
    fn dacl_error<T: std::fmt::Debug>(result: Result<T>) -> String {
        format!("{:#}", result.unwrap_err())
    }
    #[test]
    fn private_dacl_padded_capacity_decodes_through_the_first_nul_only() {
        let mut units = dacl_units("D:P(A;;GA;;;SY)");
        units.push(0);
        assert_eq!(
            unsafe { private_review_dacl_text(units.as_ptr(), dacl_capacity(&units)) }.unwrap(),
            "D:P(A;;GA;;;SY)"
        );
        units.push(0);
        units.push(0);
        units.extend(dacl_units("(A;;GA;;;WD)"));
        units.push(0);
        units.push(0xD800);
        let text =
            unsafe { private_review_dacl_text(units.as_ptr(), dacl_capacity(&units)) }.unwrap();
        assert_eq!(text, "D:P(A;;GA;;;SY)");
        let aces = private_review_dacl_aces(&text).unwrap();
        assert_eq!(aces.len(), 1);
        assert!(aces.contains("(A;;GA;;;SY)"));
        assert!(!aces.contains("(A;;GA;;;WD)"));
    }
    #[test]
    fn private_dacl_missing_terminator_fails_closed() {
        let units = dacl_units("D:P(A;;GA;;;SY)");
        let error =
            dacl_error(unsafe { private_review_dacl_text(units.as_ptr(), dacl_capacity(&units)) });
        assert!(error.contains("missing its terminator"), "{error}");
    }
    #[test]
    fn private_dacl_invalid_utf16_before_terminator_fails_closed() {
        let mut units = dacl_units("D:P(");
        units.push(0xD800);
        units.extend(dacl_units("A;;GA;;;SY)"));
        units.push(0);
        units.extend(dacl_units("(A;;GA;;;WD)"));
        let error =
            dacl_error(unsafe { private_review_dacl_text(units.as_ptr(), dacl_capacity(&units)) });
        assert!(error.contains("not valid UTF-16"), "{error}");
    }
    #[test]
    fn private_dacl_zero_capacity_fails_closed_before_slice() {
        let units = [0u16];
        let error = dacl_error(unsafe { private_review_dacl_text(units.as_ptr(), 0) });
        assert!(error.contains("exceeds its inspection limit"), "{error}");
    }
    #[test]
    fn private_dacl_oversize_capacity_fails_closed_before_slice() {
        let units = [0u16];
        let error = dacl_error(unsafe { private_review_dacl_text(units.as_ptr(), 32_769) });
        assert!(error.contains("exceeds its inspection limit"), "{error}");
        let bounded = vec![0u16; 32_768];
        let decoded = unsafe { private_review_dacl_text(bounded.as_ptr(), 32_768) }.unwrap();
        assert!(decoded.is_empty());
    }
    #[test]
    fn private_dacl_null_pointer_fails_closed_before_slice() {
        let error = dacl_error(unsafe { private_review_dacl_text(std::ptr::null(), 8) });
        assert!(error.contains("string is missing"), "{error}");
    }
    #[test]
    fn private_dacl_strict_aces_reject_missing_unclosed_and_interior_nul() {
        assert!(dacl_error(private_review_dacl_aces("D:P")).contains("no explicit grants"));
        let unclosed = dacl_error(private_review_dacl_aces("D:P(A;;GA;;;SY"));
        assert!(
            unclosed.contains("Invalid private review DACL"),
            "{unclosed}"
        );
        let interior = dacl_error(private_review_dacl_aces("(A;;GA;;;SY)\u{0}"));
        assert!(
            interior.contains("Invalid private review DACL"),
            "{interior}"
        );
        let trailing = dacl_error(private_review_dacl_aces("(A;;GA;;;SY) trailing"));
        assert!(
            trailing.contains("Invalid private review DACL"),
            "{trailing}"
        );
    }
    #[test]
    fn private_dacl_exact_ace_set_ignores_order_and_rejects_extra_grant() {
        let forward = private_review_dacl_aces("D:P(A;;GA;;;SY)(A;;GA;;;BA)").unwrap();
        let reverse = private_review_dacl_aces("(A;;GA;;;BA)(A;;GA;;;SY)").unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(forward.len(), 2);
        assert!(forward.contains("(A;;GA;;;SY)"));
        assert!(forward.contains("(A;;GA;;;BA)"));
        let wider = private_review_dacl_aces("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;WD)").unwrap();
        assert_ne!(forward, wider);
    }

    #[test]
    fn private_dacl_comparator_rejects_extra_mask_without_reading_protection_prefix() {
        let owner = private_review_dacl_aces("D:P(A;;FA;;;SY)(A;;FA;;;BA)").unwrap();
        let extra =
            private_review_dacl_aces("D:P(A;;0x011F01FF;;;SY)(A;;0x011F01FF;;;BA)").unwrap();
        assert_ne!(owner, extra);
        // ACE equality ignores D: versus D:P. windows_private still requires the protected bit.
        let unprotected = private_review_dacl_aces("D:(A;;FA;;;SY)(A;;FA;;;BA)").unwrap();
        assert_eq!(owner, unprotected);
    }

    #[cfg(windows)]
    fn dacl_text_of(owner: &crate::win_pipe::OwnerOnly) -> String {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES};
        let attrs = unsafe { &*owner.as_raw().cast::<SECURITY_ATTRIBUTES>() };
        let mut text = std::ptr::null_mut();
        let mut length = 0u32;
        assert_ne!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    attrs.lpSecurityDescriptor,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    &mut length,
                )
            },
            0
        );
        // Free the Win32 string if decoding panics. The owned String outlives it.
        struct FreeWide(*mut std::ffi::c_void);
        impl Drop for FreeWide {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let allocation = FreeWide(text.cast());
        let decoded = unsafe { private_review_dacl_text(text, length) }.unwrap();
        drop(allocation);
        decoded
    }

    #[cfg(windows)]
    fn ace_fields(ace: &str) -> (String, String, String, String) {
        let body = ace.trim_start_matches('(').trim_end_matches(')');
        let parts: Vec<&str> = body.split(';').collect();
        assert_eq!(parts.len(), 6);
        assert!(parts[3].is_empty() && parts[4].is_empty());
        (
            parts[0].to_string(),
            parts[1].to_string(),
            parts[2].to_string(),
            parts[5].to_string(),
        )
    }

    #[cfg(windows)]
    fn descriptor_masks(owner: &crate::win_pipe::OwnerOnly) -> Vec<(u8, u8, u32)> {
        use windows_sys::Win32::Security::{
            GetAce, GetSecurityDescriptorDacl, ACE_HEADER, SECURITY_ATTRIBUTES,
        };
        let attrs = unsafe { &*owner.as_raw().cast::<SECURITY_ATTRIBUTES>() };
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(
                    attrs.lpSecurityDescriptor,
                    &mut present,
                    &mut dacl,
                    &mut defaulted,
                )
            },
            0
        );
        assert_ne!(present, 0);
        assert!(!dacl.is_null());
        let mut masks = Vec::new();
        for index in 0..unsafe { (*dacl).AceCount } {
            let mut raw = std::ptr::null_mut();
            assert_ne!(unsafe { GetAce(dacl, u32::from(index), &mut raw) }, 0);
            let header = unsafe { raw.cast::<ACE_HEADER>().read_unaligned() };
            let mask = unsafe { raw.cast::<u8>().add(4).cast::<u32>().read_unaligned() };
            masks.push((header.AceType, header.AceFlags, mask));
        }
        masks
    }

    // Windows SID APIs require DWORD-aligned SID storage. Copies retain exact
    // bytes from validated native descriptors without relying on Vec<u8> alignment.
    #[cfg(windows)]
    fn aligned_sid(bytes: &[u8]) -> Vec<u32> {
        assert!((8..=68).contains(&bytes.len()));
        assert_eq!(bytes.len() % 4, 0);
        bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes(chunk.try_into().unwrap()))
            .collect()
    }

    #[cfg(windows)]
    fn same_sid(left: &[u8], right: &[u8]) -> bool {
        use windows_sys::Win32::Security::EqualSid;
        // Win32 models input-only PSID as mutable; use owned aligned buffers.
        let mut left = aligned_sid(left);
        let mut right = aligned_sid(right);
        unsafe { EqualSid(left.as_mut_ptr().cast(), right.as_mut_ptr().cast()) != 0 }
    }

    #[cfg(windows)]
    fn token_user_sid() -> Vec<u8> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{
            GetLengthSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token = std::ptr::null_mut();
            assert_ne!(
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
                0
            );
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            assert!(len as usize >= std::mem::size_of::<TOKEN_USER>());
            let mut buf = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            assert_ne!(ok, 0);
            let user = buf.as_ptr().cast::<TOKEN_USER>().read_unaligned();
            let sid_len = GetLengthSid(user.User.Sid) as usize;
            assert!((8..=68).contains(&sid_len));
            std::slice::from_raw_parts(user.User.Sid.cast::<u8>(), sid_len).to_vec()
        }
    }

    #[cfg(windows)]
    fn local_system_sid() -> Vec<u8> {
        use windows_sys::Win32::Security::{CreateWellKnownSid, WinLocalSystemSid};
        let mut system = [0u64; 9];
        let mut len = std::mem::size_of_val(&system) as u32;
        assert_ne!(
            unsafe {
                CreateWellKnownSid(
                    WinLocalSystemSid,
                    std::ptr::null_mut(),
                    system.as_mut_ptr().cast(),
                    &mut len,
                )
            },
            0
        );
        unsafe { std::slice::from_raw_parts(system.as_ptr().cast::<u8>(), len as usize) }.to_vec()
    }

    #[cfg(windows)]
    fn descriptor_sids(owner: &crate::win_pipe::OwnerOnly) -> Vec<Vec<u8>> {
        use windows_sys::Win32::Security::{
            GetAce, GetSecurityDescriptorDacl, ACE_HEADER, SECURITY_ATTRIBUTES,
        };
        let attrs = unsafe { &*owner.as_raw().cast::<SECURITY_ATTRIBUTES>() };
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(
                    attrs.lpSecurityDescriptor,
                    &mut present,
                    &mut dacl,
                    &mut defaulted,
                )
            },
            0
        );
        assert_ne!(present, 0);
        assert!(!dacl.is_null());
        let mut sids = Vec::new();
        for index in 0..unsafe { (*dacl).AceCount } {
            let mut raw = std::ptr::null_mut();
            assert_ne!(unsafe { GetAce(dacl, u32::from(index), &mut raw) }, 0);
            let header = unsafe { raw.cast::<ACE_HEADER>().read_unaligned() };
            assert_eq!(header.AceType, 0);
            let ace_size = usize::from(header.AceSize);
            assert!(ace_size >= 16 && ace_size % 4 == 0);
            let bytes =
                unsafe { std::slice::from_raw_parts(raw.cast::<u8>().add(8), ace_size - 8) };
            assert_eq!(bytes[0], 1);
            assert!(bytes[1] <= 15);
            let sid_len = 8 + usize::from(bytes[1]) * 4;
            assert!(sid_len <= bytes.len());
            sids.push(bytes[..sid_len].to_vec());
        }
        sids
    }

    /// Canonical account text from the token SID. SDDL aliases and numeric
    /// forms are not guessed; callers that need identity use `same_sid`.
    #[cfg(windows)]
    fn current_user_account() -> String {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        // Win32 models input-only PSID as mutable; this buffer is owned and aligned.
        let mut sid = aligned_sid(&token_user_sid());
        let mut wide = std::ptr::null_mut();
        assert_ne!(
            unsafe { ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut wide) },
            0
        );
        struct FreeWide(*mut std::ffi::c_void);
        impl Drop for FreeWide {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let allocation = FreeWide(wide.cast());
        let mut len = 0usize;
        unsafe {
            while *wide.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(wide, len));
            drop(allocation);
            text
        }
    }

    #[cfg(windows)]
    fn inspect_access() -> u32 {
        use windows_sys::Win32::Storage::FileSystem::{FILE_READ_ATTRIBUTES, SYNCHRONIZE};
        0x0002_0000 | FILE_READ_ATTRIBUTES | SYNCHRONIZE
    }

    #[cfg(windows)]
    fn create_production_object(path: &Path, directory: bool) -> File {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL,
            FILE_FLAG_OPEN_REPARSE_POINT,
        };
        let security = crate::win_pipe::OwnerOnly::new().unwrap();
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        if directory {
            assert_ne!(
                unsafe { CreateDirectoryW(wide.as_ptr(), security.as_raw().cast()) },
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            open_directory(path).unwrap()
        } else {
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    security.as_raw().cast(),
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(
                handle,
                INVALID_HANDLE_VALUE,
                "{}",
                std::io::Error::last_os_error()
            );
            unsafe { File::from_raw_handle(handle) }
        }
    }

    #[cfg(windows)]
    fn create_sddl_object(path: &Path, sddl: &str, directory: bool, access: u32) -> File {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::{LocalFree, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        let wide_sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut raw_sd = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide_sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut raw_sd,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        struct Free(*mut std::ffi::c_void);
        impl Drop for Free {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let sd = Free(raw_sd.cast());
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        if directory {
            assert_ne!(
                unsafe { CreateDirectoryW(wide.as_ptr(), &attrs) },
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            drop(attrs);
            drop(sd);
            let mut options = OpenOptions::new();
            options
                .access_mode(access)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
            options.open(path).unwrap()
        } else {
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    access,
                    0,
                    &attrs,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(
                handle,
                INVALID_HANDLE_VALUE,
                "{}",
                std::io::Error::last_os_error()
            );
            drop(attrs);
            drop(sd);
            unsafe { File::from_raw_handle(handle) }
        }
    }

    #[cfg(windows)]
    fn create_owner_object(path: &Path, directory: bool, access: u32) -> File {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        let security = crate::win_pipe::OwnerOnly::new().unwrap();
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        if directory {
            assert_ne!(
                unsafe { CreateDirectoryW(wide.as_ptr(), security.as_raw().cast()) },
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            let mut options = OpenOptions::new();
            options
                .access_mode(access)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
            options.open(path).unwrap()
        } else {
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    access,
                    0,
                    security.as_raw().cast(),
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(
                handle,
                INVALID_HANDLE_VALUE,
                "{}",
                std::io::Error::last_os_error()
            );
            unsafe { File::from_raw_handle(handle) }
        }
    }

    #[cfg(windows)]
    fn replace_protected_dacl(file: &File, sddl: &str) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{
            SetKernelObjectSecurity, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        };
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1,
                    &mut sd,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        struct FreeSd(*mut std::ffi::c_void);
        impl Drop for FreeSd {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let sd = FreeSd(sd.cast());
        let status = unsafe {
            SetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                sd.0.cast(),
            )
        };
        assert_ne!(status, 0, "{}", std::io::Error::last_os_error());
    }

    /// Puts the captured owner DACL back while `file` still has WRITE_DAC.
    /// Drop runs on assertion failure, before the handle and temp dir close.
    #[cfg(windows)]
    struct OwnerDaclRestore<'a> {
        file: &'a File,
        sd: *mut std::ffi::c_void,
    }

    #[cfg(windows)]
    impl<'a> OwnerDaclRestore<'a> {
        fn capture(file: &'a File) -> Self {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
            use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
            let mut sd = std::ptr::null_mut();
            let status = unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut sd,
                )
            };
            assert_eq!(
                status,
                0,
                "{}",
                std::io::Error::from_raw_os_error(status as i32)
            );
            assert!(!sd.is_null());
            Self { file, sd }
        }
    }

    #[cfg(windows)]
    impl OwnerDaclRestore<'_> {
        fn restore(&mut self) -> std::io::Result<()> {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::LocalFree;
            use windows_sys::Win32::Security::{
                SetKernelObjectSecurity, DACL_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION,
            };
            if self.sd.is_null() {
                return Ok(());
            }
            let restored = unsafe {
                SetKernelObjectSecurity(
                    self.file.as_raw_handle(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    self.sd.cast(),
                )
            };
            if restored == 0 {
                return Err(std::io::Error::last_os_error());
            }
            unsafe {
                LocalFree(self.sd);
            }
            self.sd = std::ptr::null_mut();
            Ok(())
        }
    }

    #[cfg(windows)]
    impl Drop for OwnerDaclRestore<'_> {
        fn drop(&mut self) {
            let _ = self.restore();
            if !self.sd.is_null() {
                unsafe {
                    windows_sys::Win32::Foundation::LocalFree(self.sd);
                }
            }
        }
    }

    #[cfg(windows)]
    fn persisted_dacl_text(file: &File) -> String {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
            SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        let mut sd = std::ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        assert_eq!(
            status,
            0,
            "{}",
            std::io::Error::from_raw_os_error(status as i32)
        );
        struct Free(*mut std::ffi::c_void);
        impl Drop for Free {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let sd_allocation = Free(sd.cast());
        let mut text = std::ptr::null_mut();
        let mut length = 0u32;
        assert_ne!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    sd,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    &mut length,
                )
            },
            0
        );
        struct FreeWide(*mut std::ffi::c_void);
        impl Drop for FreeWide {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0) };
            }
        }
        let text_allocation = FreeWide(text.cast());
        let decoded = unsafe { private_review_dacl_text(text, length) }.unwrap();
        drop(text_allocation);
        drop(sd_allocation);
        decoded
    }

    #[cfg(windows)]
    fn rights_are_mask(rights: &str, mask: u32) -> bool {
        let Some(hex) = rights
            .strip_prefix("0x")
            .or_else(|| rights.strip_prefix("0X"))
        else {
            return false;
        };
        u32::from_str_radix(hex, 16).ok() == Some(mask)
    }

    #[cfg(windows)]
    fn assert_persisted_owner_dacl(file: &File) {
        let persisted = persisted_dacl_text(file);
        let aces = private_review_dacl_aces(&persisted).expect("persisted DACL");
        assert!(
            persisted.starts_with("D:P(")
                && aces.len() == 2
                && aces.iter().all(|ace| {
                    let (kind, flags, rights, _) = ace_fields(ace);
                    kind == "A" && flags.is_empty() && rights == "FA"
                }),
            "{persisted}"
        );
    }

    #[cfg(windows)]
    const OWNER_MISMATCH: &str =
        "Private review storage grants access beyond the current owner and SYSTEM";

    #[cfg(windows)]
    fn assert_rejected(label: &str, sddl: &str, fragment: &str, adverse: impl Fn(&str) -> bool) {
        let (dir, _) = fixture();
        let access = inspect_access() | 0x0004_0000;
        for directory in [false, true] {
            let path = dir.0.join(format!(
                "{label}-{}",
                if directory { "dir" } else { "file" }
            ));
            let file = create_owner_object(&path, directory, access);
            let mut owner_dacl = OwnerDaclRestore::capture(&file);
            replace_protected_dacl(&file, sddl);
            let persisted = persisted_dacl_text(&file);
            let error = dacl_error(windows_private(&file));
            owner_dacl
                .restore()
                .expect("restore synthetic owner DACL before cleanup");
            assert_persisted_owner_dacl(&file);
            windows_private(&file).expect("restored fixture owner DACL");
            drop(owner_dacl);
            assert!(
                adverse(&persisted),
                "{label} directory={directory}: {persisted}"
            );
            assert!(
                error.contains(fragment) && !error.contains("inheritable DACL"),
                "{label} directory={directory}: {error}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_maps_only_expected_generic_all() {
        use windows_sys::Win32::Foundation::GENERIC_ALL;
        use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
        assert_eq!(GENERIC_ALL, 0x1000_0000);
        assert_eq!(FILE_ALL_ACCESS, 0x001F_01FF);
        let pipe = crate::win_pipe::OwnerOnly::new().unwrap();
        let file = crate::win_pipe::OwnerOnly::persisted_file_dacl().unwrap();
        let pipe_text = dacl_text_of(&pipe);
        let file_text = dacl_text_of(&file);
        assert!(pipe_text.starts_with("D:P(") && !pipe_text.contains("(A;;FA;;;"));
        assert!(file_text.starts_with("D:P(") && !file_text.contains("(A;;GA;;;"));
        let pipe_aces = private_review_dacl_aces(&pipe_text).unwrap();
        let file_aces = private_review_dacl_aces(&file_text).unwrap();
        assert_eq!(pipe_aces.len(), 2);
        assert_eq!(file_aces.len(), 2);
        let pipe_fields: Vec<_> = pipe_aces.iter().map(|ace| ace_fields(ace)).collect();
        let file_fields: Vec<_> = file_aces.iter().map(|ace| ace_fields(ace)).collect();
        assert!(pipe_fields
            .iter()
            .all(|(kind, flags, rights, _)| { kind == "A" && flags.is_empty() && rights == "GA" }));
        assert!(file_fields
            .iter()
            .all(|(kind, flags, rights, _)| { kind == "A" && flags.is_empty() && rights == "FA" }));
        let user = token_user_sid();
        let system = local_system_sid();
        assert!(!same_sid(&user, &system));
        let pipe_sids = descriptor_sids(&pipe);
        let file_sids = descriptor_sids(&file);
        assert_eq!(pipe_sids.len(), 2);
        assert_eq!(file_sids.len(), 2);
        assert!(pipe_sids.iter().any(|sid| same_sid(sid, &user)));
        assert!(pipe_sids.iter().any(|sid| same_sid(sid, &system)));
        assert!(pipe_sids
            .iter()
            .all(|sid| { file_sids.iter().any(|other| same_sid(sid, other)) }));
        assert!(file_sids
            .iter()
            .all(|sid| { pipe_sids.iter().any(|other| same_sid(sid, other)) }));
        let pipe_masks = descriptor_masks(&pipe);
        let file_masks = descriptor_masks(&file);
        assert!(pipe_masks
            .iter()
            .all(|(kind, flags, mask)| { *kind == 0 && *flags == 0 && *mask == GENERIC_ALL }));
        assert!(file_masks
            .iter()
            .all(|(kind, flags, mask)| { *kind == 0 && *flags == 0 && *mask == FILE_ALL_ACCESS }));
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_accepts_fresh_file_and_directory() {
        let (dir, _) = fixture();
        for (name, directory) in [("owner-file", false), ("owner-dir", true)] {
            let file = create_production_object(&dir.0.join(name), directory);
            assert_persisted_owner_dacl(&file);
            windows_private(&file).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_creation_normalizes_to_owner_dacl() {
        let sid = current_user_account();
        // 0x011F01FF and an unprotected D: are not what creation stores.
        for (label, requested) in [
            (
                "extra-rights",
                format!("D:P(A;;0x011F01FF;;;{sid})(A;;0x011F01FF;;;SY)"),
            ),
            ("unprotected", format!("D:(A;;FA;;;{sid})(A;;FA;;;SY)")),
        ] {
            let (dir, _) = fixture();
            for directory in [false, true] {
                let path = dir.0.join(format!(
                    "{label}-{}",
                    if directory { "dir" } else { "file" }
                ));
                let file = create_sddl_object(&path, &requested, directory, inspect_access());
                assert_persisted_owner_dacl(&file);
                windows_private(&file).expect("normalized creation is owner-only");
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_rejects_missing_rights() {
        let sid = current_user_account();
        // Clears FILE_READ_DATA / FILE_LIST_DIRECTORY. A subset of FA is not valid.
        assert_rejected(
            "missing-rights",
            &format!("D:P(A;;0x001F01FE;;;{sid})(A;;FA;;;SY)"),
            OWNER_MISMATCH,
            |text| {
                private_review_dacl_aces(text).map_or(false, |aces| {
                    aces.iter()
                        .any(|ace| rights_are_mask(&ace_fields(ace).2, 0x001F_01FE))
                })
            },
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_rejects_extra_principal() {
        let sid = current_user_account();
        assert_rejected(
            "extra-principal",
            &format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FR;;;WD)"),
            OWNER_MISMATCH,
            |text| {
                private_review_dacl_aces(text).map_or(false, |aces| {
                    aces.len() == 3 && aces.iter().any(|ace| ace_fields(ace).3 == "WD")
                })
            },
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_rejects_missing_principal() {
        let sid = current_user_account();
        assert_rejected(
            "missing-system",
            &format!("D:P(A;;FA;;;{sid})"),
            OWNER_MISMATCH,
            |text| {
                private_review_dacl_aces(text).map_or(false, |aces| {
                    aces.len() == 1 && aces.iter().all(|ace| ace_fields(ace).3 != "SY")
                })
            },
        );
        assert_rejected("missing-user", "D:P(A;;FA;;;SY)", OWNER_MISMATCH, |text| {
            private_review_dacl_aces(text).map_or(false, |aces| {
                aces.len() == 1 && aces.iter().any(|ace| ace_fields(ace).3 == "SY")
            })
        });
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_rejects_inheritance() {
        let sid = current_user_account();
        assert_rejected(
            "inherit-flags",
            &format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"),
            OWNER_MISMATCH,
            |text| {
                private_review_dacl_aces(text).map_or(false, |aces| {
                    aces.iter().any(|ace| {
                        let flags = ace_fields(ace).1;
                        flags.contains("OI") && flags.contains("CI")
                    })
                })
            },
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_review_persisted_dacl_rejects_unsupported_ace() {
        let sid = current_user_account();
        // Deny only FILE_WRITE_DATA (0x2) to Anonymous. The fixture open asks
        // for READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE, so this
        // unsupported deny is stored and windows_private sees the mismatch.
        assert_rejected(
            "deny-ace",
            &format!("D:P(D;;0x00000002;;;AN)(A;;FA;;;{sid})(A;;FA;;;SY)"),
            OWNER_MISMATCH,
            |text| {
                private_review_dacl_aces(text).map_or(false, |aces| {
                    aces.iter().any(|ace| ace_fields(ace).0 == "D")
                })
            },
        );
    }

    #[test]
    fn unix_metadata_unknown_version_and_missing_image_fail_closed() {
        let valid = crate::review_unix_metadata::UnixMetadata::try_new(
            crate::review_unix_metadata::UnixPlatform::Linux,
            vec![("user.a".into(), b"b".to_vec())],
        )
        .unwrap();
        let bad = valid.clone().with_version_for_test(2);
        let mut review = image(b"kept");
        review.unix_metadata = Some(bad.clone());
        assert!(Image::capture(&review).is_err());
        let integrity = review.integrity_hash().unwrap();
        let record = Image {
            base64: Some(base64::engine::general_purpose::STANDARD.encode(b"kept")),
            mode: review.mode,
            readonly: review.readonly,
            owner: review.owner,
            group: review.group,
            security: None,
            integrity,
            security_complete: review.security_complete,
            windows_identity: None,
            unix_metadata: Some(bad),
        };
        assert!(record
            .decode_bounded(MAX_REVIEW_BYTES)
            .unwrap_err()
            .to_string()
            .contains("Unsupported Unix metadata version"));
        let mut missing = ReviewImage::missing();
        missing.unix_metadata = Some(valid);
        assert!(Image::capture(&missing).is_err());
        let integrity = missing.integrity_hash().unwrap();
        let record = Image {
            base64: None,
            mode: None,
            readonly: false,
            owner: None,
            group: None,
            security: None,
            integrity,
            security_complete: true,
            windows_identity: None,
            unix_metadata: missing.unix_metadata,
        };
        assert!(record
            .decode_bounded(MAX_REVIEW_BYTES)
            .unwrap_err()
            .to_string()
            .contains("Missing review image"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_metadata_receipt_roundtrip_tamper_and_ambiguous_phase() {
        let plain = image(b"plain");
        let metadata = crate::review_unix_metadata::UnixMetadata::try_new(
            crate::review_unix_metadata::UnixPlatform::current().unwrap(),
            vec![
                ("user.formless_a".into(), b"\0\xff".to_vec()),
                ("user.formless_empty".into(), Vec::new()),
            ],
        )
        .unwrap();
        let mut with = plain.clone();
        with.unix_metadata = Some(metadata);
        let untouched = receipt(&plain, &plain);
        let json = serde_json::to_string(&untouched).unwrap();
        assert!(!json.contains("unix_metadata"));
        let loaded: Receipt = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.checksum().unwrap(), untouched.integrity);
        assert!(loaded.images().unwrap().0.unix_metadata.is_none());
        let action = receipt(&with, &with);
        assert_eq!(action.format_version, 1);
        assert_eq!(action.images().unwrap(), (with.clone(), with.clone()));
        assert!(serde_json::to_string(&action)
            .unwrap()
            .contains("unix_metadata"));
        let mut value = serde_json::to_value(&action).unwrap();
        value["before"]["unix_metadata"]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Receipt>(value).is_err());
        let mut value = serde_json::to_value(&action).unwrap();
        value["before"]["unix_metadata"]["entries"][0]["value_b64"] = serde_json::json!("Yg==");
        let mut tampered: Receipt = serde_json::from_value(value).unwrap();
        assert!(tampered.images().is_err());
        tampered.refresh_integrity().unwrap();
        assert!(tampered
            .images()
            .unwrap_err()
            .to_string()
            .contains("image integrity"));
        let mut other = with.clone();
        other.unix_metadata = Some(
            crate::review_unix_metadata::UnixMetadata::try_new(
                crate::review_unix_metadata::UnixPlatform::current().unwrap(),
                vec![("user.formless_a".into(), b"zz".to_vec())],
            )
            .unwrap(),
        );
        assert_eq!(action.observed_phase(&other).unwrap(), Phase::Ambiguous);
        assert_eq!(action.phase, Phase::Prepared);
        let mut format = action.clone();
        format.format_version = 3;
        format.refresh_integrity().unwrap();
        assert!(format.images().is_err());
    }
}
