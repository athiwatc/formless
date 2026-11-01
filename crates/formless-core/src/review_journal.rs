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
const MAX_RECEIPT_BYTES: usize = 1024 * 1024;
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
}
impl Image {
    fn capture(image: &ReviewImage) -> Result<Self> {
        if image
            .bytes
            .as_ref()
            .is_some_and(|b| b.len() > MAX_REVIEW_BYTES)
        {
            bail!("Review image exceeds journal byte limit");
        }
        if image
            .security
            .as_ref()
            .is_some_and(|bytes| bytes.len() > 65536)
        {
            bail!("Security metadata exceeds review receipt limit");
        }
        Ok(Self {
            integrity: image.integrity_hash()?,
            security_complete: image.security_complete,
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
        })
    }
    fn decode(&self) -> Result<ReviewImage> {
        let bytes = self
            .base64
            .as_ref()
            .map(|encoded| {
                if encoded.len() > MAX_REVIEW_BYTES.div_ceil(3) * 4 {
                    bail!("Review journal image exceeds byte limit");
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                if bytes.len() > MAX_REVIEW_BYTES {
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
                || security.is_some())
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
        };
        if image.integrity_hash()? != self.integrity {
            bail!("Private recovery image integrity mismatch");
        }
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
        };
        receipt.refresh_integrity()?;
        Ok(receipt)
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
        if self.format_version != 1
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
        Ok((self.before.decode()?, self.after.decode()?))
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

#[cfg(windows)]
fn windows_private(file: &File) -> Result<()> {
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
        let mut length = 0;
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
        if length == 0 || length > 32768 {
            bail!("Private review DACL exceeds its inspection limit");
        }
        let text =
            String::from_utf16(unsafe { std::slice::from_raw_parts(text, length as usize - 1) })?;
        drop(allocation);
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
    windows_identity(file)?;
    let expected = crate::win_pipe::OwnerOnly::new()?;
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
}
