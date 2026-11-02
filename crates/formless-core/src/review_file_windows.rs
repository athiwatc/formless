//! Windows review access pins every ancestor against rename/delete and rejects
//! reparse points. ReplaceFile preserves existing ACLs; its documented partial
//! failure modes still require reconciliation with the durable prepared receipt.
use super::{ReviewImage, WindowsIdentity, MAX_REVIEW_BYTES};
use anyhow::{bail, Context, Result};
use formless_runtime::sensitive_paths::{PathDecision, PathPolicy};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
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
/// Attribute-only handles retain identity without holding a data-sharing lock.
/// Equality ignores this nonserialized capability; WindowsIdentity is compared
/// separately. Only a live active review may attach it to a decoded receipt.
#[derive(Clone, Debug, Default)]
pub(crate) struct Witness(Option<std::sync::Arc<File>>);
impl PartialEq for Witness {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for Witness {}
impl Witness {
    pub(super) fn matches(&self, expected: Option<&WindowsIdentity>) -> bool {
        self.0
            .as_ref()
            .and_then(|file| identity(file).ok())
            .as_ref()
            .zip(expected)
            .is_some_and(|(actual, expected)| actual == expected)
    }
}
fn identity(file: &File) -> Result<WindowsIdentity> {
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: live borrowed HANDLE, properly sized/aligned native output.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let info = unsafe { info.assume_init() };
    Ok(WindowsIdentity {
        volume: info.VolumeSerialNumber,
        file: info.FileId.Identifier,
    })
}
/// Live-only authority from the successful mkdir callback; never decoded from a token.
pub(crate) struct WindowsDirectoryProof {
    file: File,
    security: Vec<u8>,
    attributes: u32,
    captured: std::time::Instant,
}
impl WindowsDirectoryProof {
    pub(crate) fn fresh(&self) -> bool {
        self.captured.elapsed().as_secs() < crate::review_retained::RETENTION_SECONDS
    }
}
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
    path_by_handle(file, 0)
}
fn physical_path(file: &File) -> Result<PathBuf> {
    path_by_handle(file, VOLUME_NAME_NT)
}
fn path_by_handle(file: &File, flags: u32) -> Result<PathBuf> {
    let mut out = vec![0u16; 32768];
    // SAFETY: live borrowed HANDLE, valid writable UTF-16 output buffer.
    let size = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            out.as_mut_ptr(),
            out.len() as u32,
            flags,
        )
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
            let expected_parent = physical_path(guards.last().context("No parent")?)?;
            named = append(&named, &name);
            let child = directory(&named)?;
            if physical_path(&child)?.parent() != Some(expected_parent.as_path()) {
                bail!("Review directory lookup escaped its pinned physical parent");
            }
            guards.push(child);
        }
        let path = append(&final_path(guards.last().context("no parent")?)?, &leaf);
        Ok(Self { path, guards })
    }
    pub(super) fn recheck(&self, raw: &Path, policy: &PathPolicy) -> Result<()> {
        for (index, guard) in self.guards.iter().enumerate() {
            let metadata = guard.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                bail!("Pinned review parent was retagged");
            }
            if index > 0
                && physical_path(guard)?.parent()
                    != Some(physical_path(&self.guards[index - 1])?.as_path())
            {
                bail!("Pinned review parent chain changed physical identity");
            }
        }
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
        self.read_bounded(raw, policy, MAX_REVIEW_BYTES, false, None, None)
    }
    pub(super) fn read_bounded(
        &self,
        raw: &Path,
        policy: &PathPolicy,
        limit: usize,
        capture: bool,
        alias: Option<&Path>,
        tracked: Option<&crate::review_retained::RetainedStore>,
    ) -> Result<ReviewImage> {
        self.recheck(raw, policy)?;
        let expected_parent = physical_path(self.guards.last().context("No parent")?)?;
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
        if physical_path(&file)?.parent() != Some(expected_parent.as_path()) {
            bail!("Review file lookup escaped its pinned physical parent");
        }
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
        let logical_single_link = single_link
            || tracked.is_some_and(|store| store.verify_live_peer(&file).unwrap_or(false));
        if !logical_single_link && !capture {
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
        let windows_identity = match identity(&file) {
            Ok(identity) => Some(identity),
            // Whole-file snapshots still retain authorized bounded bytes when
            // this filesystem cannot provide a strong interactive-review ID.
            Err(_) if capture => None,
            Err(error) => return Err(error),
        };
        let witness = if !capture && logical_single_link && streams_complete {
            let pin = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&self.path)?;
            if Some(identity(&pin)?) != windows_identity {
                bail!("File object changed before review retained it");
            }
            Witness(Some(std::sync::Arc::new(pin)))
        } else {
            Witness::default()
        };
        Ok(ReviewImage {
            bytes: Some(bytes),
            windows_identity: windows_identity.clone(),
            witness,
            mode: Some(metadata.file_attributes()),
            readonly: metadata.permissions().readonly(),
            security,
            security_complete: complete
                && logical_single_link
                && streams_complete
                && windows_identity.is_some(),
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
    fn run_directory(&self, raw: &Path, policy: &PathPolicy) -> Result<File> {
        self.recheck(raw, policy)?;
        let parent = self.guards.last().context("No pinned directory parent")?;
        let file = crate::review_windows_namespace::open_directory_for_removal(
            parent,
            self.path.file_name().context("No directory leaf")?,
        )?;
        if physical_path(&file)?.parent() != Some(physical_path(parent)?.as_path()) {
            bail!("Directory escaped pinned parent");
        }
        Ok(file)
    }
    pub(super) fn capture_directory(
        &self,
        raw: &Path,
        policy: &PathPolicy,
    ) -> Result<WindowsDirectoryProof> {
        let file = self.run_directory(raw, policy)?;
        let (security, _) = snapshot_security(&file)?;
        let attributes = file.metadata()?.file_attributes();
        Ok(WindowsDirectoryProof {
            file,
            security,
            attributes,
            captured: std::time::Instant::now(),
        })
    }
    pub(super) fn directory_identity(&self, raw: &Path, policy: &PathPolicy) -> Result<(u64, u64)> {
        let file = self.run_directory(raw, policy)?;
        let id = identity(&file)?;
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(id.file);
        Ok((id.volume, u64::from_le_bytes(digest[..8].try_into()?)))
    }
    pub(super) fn verify_directory(
        &self,
        raw: &Path,
        proof: &WindowsDirectoryProof,
        policy: &PathPolicy,
        remove: bool,
    ) -> Result<()> {
        if !proof.fresh() {
            bail!("Live created-directory proof expired");
        }
        let current = self.run_directory(raw, policy)?;
        if identity(&current)? != identity(&proof.file)?
            || current.metadata()?.file_attributes() != proof.attributes
            || snapshot_security(&current)?.0 != proof.security
        {
            bail!("Created directory object or security changed since the Run");
        }
        self.recheck(raw, policy)?;
        if remove {
            // Windows rejects a nonempty directory. The same checked HANDLE is
            // used, so no replaced pathname can acquire removal authority.
            let disposition = FILE_DISPOSITION_INFO_EX {
                Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            };
            if unsafe {
                SetFileInformationByHandle(
                    current.as_raw_handle(),
                    FileDispositionInfoEx,
                    (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    }
    pub(super) fn namespace_target_matches(
        &self,
        raw: &Path,
        expected: &WindowsIdentity,
        policy: &PathPolicy,
    ) -> Result<bool> {
        self.recheck(raw, policy)?;
        Ok(
            identity(self.guards.last().context("No pinned namespace parent")?)?.volume
                == expected.volume,
        )
    }
    /// Open the exact authorized target link for a namespace operation. This
    /// rooted synchronous factory grants DELETE but no security setter rights.
    pub(super) fn namespace_source(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<File> {
        self.namespace_source_bounded(raw, expected, policy, store, MAX_REVIEW_BYTES)
    }
    pub(super) fn namespace_source_bounded(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
        limit: usize,
    ) -> Result<File> {
        use crate::review_windows_namespace as namespace;
        self.recheck(raw, policy)?;
        if let Some(reason) = expected.ordinary_write_restriction() {
            bail!(reason);
        }
        let before = expected
            .bytes
            .as_deref()
            .context("Existing namespace source required")?;
        if before.len() > limit {
            bail!("Namespace source exceeds review byte limit");
        }
        let parent = self.guards.last().context("No parent")?;
        let leaf = self.path.file_name().context("No leaf")?;
        let rights = windows_sys::Win32::Foundation::GENERIC_READ
            | windows_sys::Win32::Foundation::GENERIC_WRITE
            | 0x0010_0000
            | 0x0001_0000;
        let mut file =
            namespace::open_exclusive(parent, leaf, rights | ACCESS_SYSTEM_SECURITY_RIGHT)
                .or_else(|error| match error.raw_os_error() {
                    Some(5 | 1314) => namespace::open_exclusive(parent, leaf, rights),
                    _ => Err(error),
                })?;
        if physical_path(&file)?.parent() != Some(physical_path(parent)?.as_path())
            || Some(identity(&file)?) != expected.windows_identity
        {
            bail!("Namespace source left its exact pinned object or parent");
        }
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || Some(metadata.file_attributes()) != expected.mode
            || metadata.permissions().readonly() != expected.readonly
        {
            bail!("Namespace source metadata changed");
        }
        require_default_stream_only(&file)?;
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { info.assume_init() }.nNumberOfLinks != 1 && !store.verify_live_peer(&file)? {
            bail!("Namespace source has untracked aliases");
        }
        let (security, complete) = snapshot_security(&file)?;
        if Some(security) != expected.security || complete != expected.security_complete {
            bail!("Namespace source security metadata changed");
        }
        let mut actual = Vec::with_capacity(before.len());
        (&file).take(limit as u64 + 1).read_to_end(&mut actual)?;
        if actual != before {
            bail!("Namespace source bytes changed");
        }
        file.seek(SeekFrom::Start(0))?;
        self.recheck(raw, policy)?;
        Ok(file)
    }
    pub(super) fn restore_retained(
        &self,
        raw: &Path,
        object: &crate::review_retained::RetainedObject,
        desired: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<Vec<String>> {
        self.restore_retained_bounded(raw, object, desired, policy, store, MAX_REVIEW_BYTES)
    }
    pub(super) fn restore_retained_bounded(
        &self,
        raw: &Path,
        object: &crate::review_retained::RetainedObject,
        desired: &ReviewImage,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
        limit: usize,
    ) -> Result<Vec<String>> {
        use crate::review_windows_namespace as namespace;
        self.recheck(raw, policy)?;
        if self
            .read_bounded(raw, policy, limit, false, None, Some(store))?
            .bytes
            .is_some()
        {
            bail!("Namespace restore cannot replace an occupied target");
        }
        if let Some(reason) = desired.ordinary_write_restriction() {
            bail!(reason);
        }
        if object.before() != desired {
            bail!("Retained immutable image disagrees with desired restoration");
        }
        let bytes = desired
            .bytes
            .as_deref()
            .context("Missing desired restoration image")?;
        if bytes.len() > limit {
            bail!("Restoration image exceeds review limit");
        }
        // No mutable public alias may exist before ANY retained-body rewrite.
        object.require_retained_only()?;
        let mut file = object.open_for_mutation()?;
        object.require_retained_only()?;
        if Some(identity(&file)?) != desired.windows_identity {
            bail!("Retained restoration identity changed");
        }
        let (security, complete) = snapshot_security(&file)?;
        if Some(security) != desired.security || complete != desired.security_complete {
            bail!("Retained original security descriptor no longer matches");
        }
        require_default_stream_only(&file)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(bytes)
            .context("Retained restoration bytes may be partial; receipt remains prepared")?;
        file.set_len(bytes.len() as u64)?;
        let basic = FILE_BASIC_INFO {
            CreationTime: 0,
            LastAccessTime: 0,
            LastWriteTime: 0,
            ChangeTime: 0,
            FileAttributes: desired.mode.context("Missing restored attributes")?,
        };
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileBasicInfo,
                (&basic as *const FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        file.sync_all()?;
        file.seek(SeekFrom::Start(0))?;
        let mut actual = Vec::with_capacity(bytes.len());
        (&file).take(limit as u64 + 1).read_to_end(&mut actual)?;
        if actual != bytes {
            bail!("Retained restoration readback differs");
        }
        object.require_retained_only()?;
        self.recheck(raw, policy)?;
        let parent = self.guards.last().context("No parent")?;
        let leaf = self.path.file_name().context("No leaf")?;
        // SAFETY: private factory supplied an exclusively used synchronous file
        // object; target is the checked pinned parent and exact owned leaf.
        unsafe {
            namespace::link_existing(&file, parent, leaf)?;
        }
        let linked = namespace::open_existing(parent, leaf, FILE_READ_ATTRIBUTES | 0x0010_0000)?;
        if identity(&linked)? != identity(&file)?
            || physical_path(&linked)?.parent() != Some(physical_path(parent)?.as_path())
        {
            bail!("Linked restoration could not be verified; retain prepared receipt");
        }
        self.recheck(raw, policy)?;
        Ok(Vec::new())
    }
    pub(super) fn remove_retained(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        object: &crate::review_retained::RetainedObject,
        policy: &PathPolicy,
        store: &crate::review_retained::RetainedStore,
    ) -> Result<Vec<String>> {
        let source = self.namespace_source(raw, expected, policy, store)?;
        self.remove_prepared(raw, source, expected, object, policy)
    }
    pub(super) fn remove_prepared(
        &self,
        raw: &Path,
        source: File,
        expected: &ReviewImage,
        object: &crate::review_retained::RetainedObject,
        policy: &PathPolicy,
    ) -> Result<Vec<String>> {
        self.recheck(raw, policy)?;
        if Some(identity(&source)?) != expected.windows_identity
            || physical_path(&source)?.parent()
                != Some(physical_path(self.guards.last().context("No parent")?)?.as_path())
        {
            bail!("Prepared unlink lost its exact source object or pinned parent");
        }
        object.verify_peer(&source)?;
        // SAFETY: namespace_source opened this exact public link synchronously
        // with DELETE and exclusive sharing. Its bytes, identity and pinned
        // parent remain checked while the same file object is used by unlink.
        unsafe {
            crate::review_windows_namespace::unlink_existing(&source)?;
        }
        drop(source);
        Ok(Vec::new())
    }
    /// Byte-only mutation retains the exact object (including opaque audit/SACL
    /// state). This is recoverable via prepared images, not an atomic replace.
    pub(super) fn replace_existing(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
    ) -> Result<Vec<String>> {
        self.replace_existing_tracked(raw, expected, desired, policy, None)
    }
    pub(super) fn replace_existing_tracked(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
        tracked: Option<&crate::review_retained::RetainedStore>,
    ) -> Result<Vec<String>> {
        self.replace_existing_bounded(raw, expected, desired, policy, tracked, MAX_REVIEW_BYTES)
    }
    pub(super) fn replace_existing_bounded(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
        tracked: Option<&crate::review_retained::RetainedStore>,
        limit: usize,
    ) -> Result<Vec<String>> {
        self.recheck(raw, policy)?;
        let before = expected
            .bytes
            .as_deref()
            .context("Existing file required")?;
        let after = desired.bytes.as_deref().context("Existing file required")?;
        if before.len() > limit || after.len() > limit {
            bail!("Review byte limit exceeded");
        }
        if let Some(reason) = expected
            .existing_write_restriction()
            .or_else(|| desired.existing_write_restriction())
        {
            bail!(reason);
        }
        if !expected.witness.matches(expected.windows_identity.as_ref())
            || !desired.witness.matches(expected.windows_identity.as_ref())
        {
            bail!("Byte-only Windows mutation requires the original live review witness");
        }
        let mut metadata_only = desired.clone();
        metadata_only.bytes = expected.bytes.clone();
        if metadata_only != *expected {
            bail!("Byte-only Windows review cannot change metadata or object identity");
        }
        let open = |extra| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .access_mode(
                    windows_sys::Win32::Foundation::GENERIC_READ
                        | windows_sys::Win32::Foundation::GENERIC_WRITE
                        | extra,
                )
                .share_mode(0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&self.path)
        };
        let expected_parent = physical_path(self.guards.last().context("No parent")?)?;
        let mut file = open(ACCESS_SYSTEM_SECURITY_RIGHT).or_else(|_| open(0))?;
        if physical_path(&file)?.parent() != Some(expected_parent.as_path()) {
            bail!("Review write lookup escaped its pinned physical parent");
        }
        let verify = |file: &File, body: &[u8]| -> Result<()> {
            self.recheck(raw, policy)?;
            if physical_path(file)?.parent() != Some(expected_parent.as_path()) {
                bail!("Review object left its pinned physical parent");
            }
            let actual = final_path(file)?;
            let anchored = append(
                &final_path(self.guards.last().context("No parent")?)?,
                self.path.file_name().context("No leaf")?,
            );
            if actual != anchored
                || !matches!(
                    policy.classify(raw, &actual).map_err(anyhow::Error::msg)?,
                    PathDecision::Ordinary
                )
            {
                bail!("The opened review object no longer has the ordinary anchored path");
            }
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || Some(identity(file)?) != expected.windows_identity
            {
                bail!("The existing review file object changed");
            }
            let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if unsafe { info.assume_init() }.nNumberOfLinks != 1
                && !tracked.is_some_and(|store| store.verify_live_peer(file).unwrap_or(false))
            {
                bail!("Multiple hard links are not hunk-editable");
            }
            require_default_stream_only(file)?;
            let (security, complete) = snapshot_security(file)?;
            if Some(security) != expected.security
                || complete != expected.security_complete
                || Some(metadata.file_attributes()) != expected.mode
                || metadata.permissions().readonly() != expected.readonly
                || metadata.len() != body.len() as u64
            {
                bail!("Review bytes or metadata changed before verification");
            }
            Ok(())
        };
        verify(&file, before)?;
        let mut observed = Vec::with_capacity(before.len());
        (&file).take(limit as u64 + 1).read_to_end(&mut observed)?;
        if observed != before {
            bail!("Review bytes changed before commit");
        }
        // One exclusive HANDLE is retained through CAS, write, truncate, flush
        // and readback. No pathname replacement and no security setter occurs.
        file.seek(SeekFrom::Start(0))?;
        file.write_all(after)
            .context("In-place write may be partial; reconcile prepared receipt")?;
        file.set_len(after.len() as u64)
            .context("In-place truncate may have changed bytes; reconcile prepared receipt")?;
        file.sync_all()
            .context("In-place durability uncertain; retain prepared receipt")?;
        if Some(file.metadata()?.file_attributes()) != expected.mode {
            // Writes may set ARCHIVE. Restore only known ordinary attributes on
            // the same HANDLE, never touch any security descriptor component.
            let basic = FILE_BASIC_INFO {
                CreationTime: 0,
                LastAccessTime: 0,
                LastWriteTime: 0,
                ChangeTime: 0,
                FileAttributes: expected.mode.context("Missing ordinary attributes")?,
            };
            if unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle(),
                    FileBasicInfo,
                    (&basic as *const FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            file.sync_all()?;
        }
        file.seek(SeekFrom::Start(0))?;
        observed.clear();
        (&file).take(limit as u64 + 1).read_to_end(&mut observed)?;
        if observed != after {
            bail!("In-place readback differs; retain prepared recovery receipt");
        }
        verify(&file, after)?;
        Ok(Vec::new())
    }
    pub(super) fn replace(
        &self,
        raw: &Path,
        expected: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
    ) -> Result<Vec<String>> {
        self.recheck(raw, policy)?;
        if expected.bytes.is_none() || desired.bytes.is_none() {
            bail!("Windows creation/deletion requires retained-object recovery; this review is read-only");
        }
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
        access
            .replace_existing(&path, &before, &after, &policy)
            .unwrap();
        assert_eq!(access.read(&path, &policy).unwrap(), after);
        access
            .replace_existing(&path, &after, &before, &policy)
            .unwrap();
        assert_eq!(access.read(&path, &policy).unwrap(), before);
        assert!(access
            .replace(&path, &before, &ReviewImage::missing(), &policy)
            .is_err());
        assert!(path.exists());
        assert!(access
            .replace(&path, &ReviewImage::missing(), &before, &policy)
            .is_err());
        assert_eq!(access.read(&path, &policy).unwrap(), before);
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    }
    #[test]
    fn ordinary_existing_file_bytes_roundtrip_retains_object_and_visible_security() {
        let root = Fixture::new();
        let path = root.0.join("ordinary.txt");
        std::fs::write(&path, "before\r\n日本語\r\n").unwrap();
        let policy = PathPolicy::default();
        let access = super::super::ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        assert!(before.existing_write_restriction().is_none());
        let original_identity = before.windows_identity.clone();
        let mut after = before.clone();
        after.bytes = Some("after\r\n🦀\r\n".as_bytes().to_vec());
        access.replace(&before, &after, &policy).unwrap();
        assert_eq!(access.read(&policy).unwrap(), after);
        access.replace(&after, &before, &policy).unwrap();
        let restored = access.read(&policy).unwrap();
        assert_eq!(restored, before);
        assert_eq!(restored.windows_identity, original_identity);
        // Receipt decoding does not retain a HANDLE capability, even when its
        // durable FileID and checksummed bytes are still exactly correct.
        let decoded: ReviewImage =
            serde_json::from_slice(&serde_json::to_vec(&before).unwrap()).unwrap();
        assert_eq!(decoded, before);
        assert!(!decoded.witness.matches(decoded.windows_identity.as_ref()));
        assert!(access.replace(&decoded, &after, &policy).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before.bytes.unwrap());
    }
    #[test]
    fn retained_object_rejects_same_bytes_at_a_replaced_path_and_new_hardlinks() {
        let root = Fixture::new();
        let path = root.0.join("ordinary.txt");
        std::fs::write(&path, b"before").unwrap();
        let policy = PathPolicy::default();
        let access = super::super::ReviewPath::open(&path, &policy).unwrap();
        let before = access.read(&policy).unwrap();
        let mut after = before.clone();
        after.bytes = Some(b"after".to_vec());
        let old = root.0.join("old.txt");
        std::fs::rename(&path, &old).unwrap();
        std::fs::write(&path, b"before").unwrap();
        assert!(access.replace(&before, &after, &policy).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"before");
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&old, &path).unwrap();
        std::fs::hard_link(&path, &old).unwrap();
        assert!(access.replace(&before, &after, &policy).is_err());
        assert_eq!(std::fs::read(&old).unwrap(), b"before");
    }
    #[test]
    fn retagged_pinned_parent_cannot_mint_a_witness_for_an_outside_file() {
        use windows_sys::Win32::System::{
            Ioctl::FSCTL_SET_REPARSE_POINT, SystemServices::IO_REPARSE_TAG_MOUNT_POINT,
            IO::DeviceIoControl,
        };
        let root = Fixture::new();
        let owned = root.0.join("owned");
        let outside = root.0.join("outside");
        std::fs::create_dir(&owned).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let victim = outside.join("victim.txt");
        std::fs::write(&victim, b"outside private sentinel").unwrap();
        let policy = PathPolicy::default();
        let access = Access::open(&owned.join("victim.txt")).unwrap();
        let attribute = OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(&owned)
            .unwrap();
        let destination = outside.canonicalize().unwrap();
        let destination = destination.to_str().unwrap().strip_prefix(r"\\?\").unwrap();
        let target: Vec<u16> = std::ffi::OsStr::new(&format!(r"\??\{destination}"))
            .encode_wide()
            .collect();
        let bytes = u16::try_from(target.len() * 2).unwrap();
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
        // Private NTFS fixture: mutate only the owned empty directory through
        // its attribute HANDLE; no signals, settings, or user paths are used.
        assert_ne!(
            unsafe {
                DeviceIoControl(
                    attribute.as_raw_handle(),
                    FSCTL_SET_REPARSE_POINT,
                    data.as_ptr().cast(),
                    data.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            },
            0,
            "actual fixture retag must occur"
        );
        assert_eq!(
            std::fs::read(owned.join("victim.txt")).unwrap(),
            b"outside private sentinel"
        );
        assert!(access.read(&owned.join("victim.txt"), &policy).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"outside private sentinel");
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
