//! Original-object retention never sets an existing object's security descriptor.
//! All object/receipt opens and namespace changes are relative to a pinned HANDLE.
use super::*;
use crate::{review_journal, review_windows_namespace as namespace, Paths};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::Storage::FileSystem::*;

const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
const DELETE_ACCESS: u32 = 0x0001_0000;
const READ_CONTROL_ACCESS: u32 = 0x0002_0000;
const OBJECT_ACCESS: u32 =
    FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS | READ_CONTROL_ACCESS;
const PIN_ACCESS: u32 = FILE_READ_ATTRIBUTES | SYNCHRONIZE_ACCESS;
const PROBE_RESERVE: u64 = 8192;
#[path = "artifact_delete_windows_directory.rs"]
mod pinned_entries;
type Live = BTreeMap<RetainedToken, Weak<Pin>>;
type Pin = ();
struct Inventory {
    records: BTreeMap<RetainedToken, Manifest>,
    missing: BTreeMap<RetainedToken, MissingMarker>,
    actual_bytes: BTreeMap<RetainedToken, u64>,
}
impl Inventory {
    fn len(&self) -> usize {
        self.records.len()
            + self
                .missing
                .keys()
                .filter(|token| !self.records.contains_key(*token))
                .count()
    }
    fn charge(&self) -> Result<u64> {
        let excess = |token: &RetainedToken| {
            self.actual_bytes
                .get(token)
                .copied()
                .unwrap_or(0)
                .saturating_sub((2 * 1024 * 1024u64))
        };
        let missing = self.missing.values().try_fold(0u64, |sum, marker| {
            sum.checked_add(marker.charge())
                .and_then(|sum| sum.checked_add(excess(&marker.token)))
                .context("Missing marker quota overflow")
        })?;
        self.records
            .values()
            .filter(|record| !self.missing.contains_key(&record.token))
            .try_fold(missing, |sum, record| {
                sum.checked_add(record.charge()?)
                    .and_then(|sum| sum.checked_add(excess(&record.token)))
                    .context("Retained quota overflow")
            })
    }
}
struct Inner {
    directory: File,
    _parent: File,
    serial: Mutex<Live>,
    namespace_capable: AtomicBool,
}
#[derive(Clone)]
pub(crate) struct RetainedStore(Arc<Inner>);
pub(crate) struct PreparedRetention {
    store: RetainedStore,
    manifest: Manifest,
    token: RetainedToken,
    _pin: Arc<Pin>,
}
impl PreparedRetention {
    pub(crate) fn token(&self) -> &RetainedToken {
        &self.token
    }
}
pub(crate) struct RetainedObject {
    file: File,
    before: ReviewImage,
    identity: WindowsIdentity,
    token: RetainedToken,
    _pin: Arc<Pin>,
    _store: RetainedStore,
}
impl RetainedObject {
    pub(crate) fn file(&self) -> &File {
        &self.file
    }
    pub(crate) fn before(&self) -> &ReviewImage {
        &self.before
    }
    pub(crate) fn token(&self) -> RetainedToken {
        self.token
            .with_lease(self._pin.clone(), self._store.0.clone())
    }
    /// Every internal peer must be present in the bounded owned inventory.
    /// A renamed public alias or an untracked link is never waived.
    pub(crate) fn verify_peer(&self, live: &File) -> Result<()> {
        if identity(live)? != self.identity
            || identity(&self.file)? != self.identity
            || !self._store.verify_live_peer(live)?
        {
            bail!("Retained review object has an untracked file alias");
        }
        regular(live)?;
        regular(&self.file)?;
        Ok(())
    }
    pub(crate) fn require_retained_only(&self) -> Result<()> {
        let _serial = self._store.lock()?;
        let internal = self._store.internal_links(&self.identity)?;
        if identity(&self.file)? != self.identity || internal == 0 || links(&self.file)? != internal
        {
            bail!("Deleted review object still has an untracked live name");
        }
        Ok(())
    }
    pub(crate) fn open_for_mutation(&self) -> Result<File> {
        let _serial = self._store.lock()?;
        let access = OBJECT_ACCESS | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | DELETE_ACCESS;
        let file = match namespace::open_exclusive(
            &self._store.0.directory,
            OsStr::new(&object_name(&self.token)),
            access | 0x0100_0000, // ACCESS_SYSTEM_SECURITY: query existing rights only.
        ) {
            Ok(file) => file,
            Err(error) if matches!(error.raw_os_error(), Some(5 | 1314)) => {
                namespace::open_exclusive(
                    &self._store.0.directory,
                    OsStr::new(&object_name(&self.token)),
                    access,
                )?
            }
            Err(error) => return Err(error.into()),
        };
        regular(&file)?;
        if identity(&file)? != self.identity || identity(&self.file)? != self.identity {
            bail!("Retained object changed before its exclusive mutation");
        }
        let internal = self._store.internal_links(&self.identity)?;
        if internal == 0 || links(&file)? != internal {
            bail!("Private before-image restore would modify a still-public file alias");
        }
        Ok(file)
    }
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn object_name(token: &RetainedToken) -> String {
    format!("{}.object", token.id())
}
fn receipt_name(token: &RetainedToken) -> String {
    format!("{}.json", token.id())
}
fn missing_name(token: &RetainedToken) -> String {
    format!("{}.missing", token.id())
}
fn regular(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("Retained object is not an ordinary non-reparse file");
    }
    Ok(())
}
fn identity(file: &File) -> Result<WindowsIdentity> {
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: live HANDLE and properly sized native output.
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
fn links(file: &File) -> Result<u32> {
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: live HANDLE and properly sized native output.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { info.assume_init() }.nNumberOfLinks)
}

impl RetainedStore {
    pub(crate) fn supports_identity(&self, source: &WindowsIdentity) -> Result<bool> {
        Ok(identity(&self.0.directory)?.volume == source.volume)
    }
    /// Inspecting an absent store never creates it. A separate profile-private
    /// store is used ONLY on the source volume; there is no project-local fallback.
    pub(crate) fn open(paths: &Paths, create: bool) -> Result<Option<Self>> {
        static STORES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Inner>>>> = OnceLock::new();
        let data = match paths.data_dir.canonicalize() {
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            result => result?,
        };
        let path = data.join("review-objects");
        let mut stores = STORES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("Retained store registry is poisoned"))?;
        stores.retain(|_, weak| weak.strong_count() != 0);
        if let Some(store) = stores.get(&path).and_then(Weak::upgrade) {
            return Ok(Some(Self(store)));
        }
        if stores.len() >= 32 {
            bail!("Too many live retained-object stores");
        }
        // Configured data aliases are resolved once as trusted configuration.
        // Creating the private child uses the pinned parent, not this named path.
        let parent = std::fs::OpenOptions::new()
            .access_mode(
                FILE_LIST_DIRECTORY
                    | FILE_ADD_FILE
                    | FILE_ADD_SUBDIRECTORY
                    | FILE_TRAVERSE
                    | FILE_READ_ATTRIBUTES
                    | 0x0002_0000
                    | SYNCHRONIZE_ACCESS,
            )
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path.parent().context("No profile data parent")?)?;
        let parent_meta = parent.metadata()?;
        if !parent_meta.is_dir()
            || parent_meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            bail!("Profile data parent is not an ordinary pinned directory");
        }
        let directory = match namespace::open_directory(&parent, OsStr::new("review-objects")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                namespace::create_private_directory(&parent, OsStr::new("review-objects"))?
            }
            Err(e) => return Err(e.into()),
            Ok(directory) => directory,
        };
        review_journal::windows_private(&directory)?;
        let store = Self(Arc::new(Inner {
            directory,
            _parent: parent,
            serial: Mutex::new(BTreeMap::new()),
            namespace_capable: AtomicBool::new(false),
        }));
        stores.insert(path, Arc::downgrade(&store.0));
        Ok(Some(store))
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Live>> {
        self.0
            .serial
            .lock()
            .map_err(|_| anyhow::anyhow!("Retained store state is poisoned"))
    }
    /// Establish exact-name unlink support before offering a namespace inverse.
    /// A private same-volume probe is never a substitute for opening the actual
    /// future source with DELETE and validating its ordinary attributes.
    pub(crate) fn ensure_namespace_capability(&self) -> Result<()> {
        self.ensure_namespace_capability_with(|probe| {
            // SAFETY: newly created exact-object synchronous factory HANDLE.
            unsafe {
                namespace::unlink_existing(probe)?;
            }
            Ok(())
        })
    }
    fn ensure_namespace_capability_with(
        &self,
        unlink: impl FnOnce(&File) -> Result<()>,
    ) -> Result<()> {
        let _serial = self.lock()?;
        if self.0.namespace_capable.load(Ordering::Acquire) {
            return Ok(());
        }
        let records = self.inventory()?;
        let charge = records
            .charge()?
            .checked_add(8192)
            .context("Probe quota overflow")?;
        if charge > MAX_STORE_BYTES {
            bail!("No reserved capacity for namespace capability validation");
        }
        #[derive(Serialize)]
        struct ProbeIntent {
            version: u8,
            root: WindowsIdentity,
            nonce: RetainedToken,
            created_at: u64,
            probe: Option<WindowsIdentity>,
        }
        fn bytes(intent: &ProbeIntent) -> Result<Vec<u8>> {
            use sha2::{Digest, Sha256};
            let body = serde_json::to_vec(intent)?;
            let mut hash = Sha256::new();
            hash.update(b"formless-review-namespace-probe-v1\0");
            hash.update(&body);
            let value =
                serde_json::json!({"intent": intent, "checksum": format!("{:x}", hash.finalize())});
            let bytes = serde_json::to_vec(&value)?;
            if bytes.len() > 4096 {
                bail!("Namespace probe intent exceeds its reserved bound");
            }
            Ok(bytes)
        }
        let mut intent = ProbeIntent {
            version: 1,
            root: identity(&self.0.directory)?,
            nonce: RetainedToken::fresh(),
            created_at: now()?,
            probe: None,
        };
        // Fixed probe names are reserved, not silently swept as arbitrary
        // entries. Any interrupted/uncertain probe makes inventory fail closed.
        let mut receipt = namespace::create_private(
            &self.0.directory,
            OsStr::new("namespace-capability.intent"),
        )?;
        receipt.write_all(&bytes(&intent)?)?;
        receipt.sync_all()?;
        let probe =
            namespace::create_private(&self.0.directory, OsStr::new("namespace-capability.probe"))?;
        intent.probe = Some(identity(&probe)?);
        receipt.seek(SeekFrom::Start(0))?;
        receipt.set_len(0)?;
        receipt.write_all(&bytes(&intent)?)?;
        receipt.sync_all()?;
        // Created-file Reject can use rename even on a filesystem without hard
        // links. Prove its inverse link primitive BEFORE offering that Reject.
        // SAFETY: our exact synchronous private probe and pinned target HANDLE;
        // the durable intent already binds its ID and the reserved peer names.
        unsafe {
            namespace::link_existing(
                &probe,
                &self.0.directory,
                OsStr::new("namespace-capability.peer"),
            )?;
        }
        let peer = namespace::open_existing(
            &self.0.directory,
            OsStr::new("namespace-capability.peer"),
            PIN_ACCESS,
        )?;
        if Some(identity(&peer)?) != intent.probe || links(&probe)? != 2 {
            bail!("Namespace probe link did not retain the exact owned object");
        }
        // Identity and durable ownership intent precede namespace removal.
        unlink(&probe)?;
        drop(probe);
        let writable_peer = namespace::open_exclusive(
            &self.0.directory,
            OsStr::new("namespace-capability.peer"),
            PIN_ACCESS | DELETE_ACCESS,
        )?;
        if Some(identity(&writable_peer)?) != intent.probe || links(&writable_peer)? != 1 {
            bail!("Namespace probe peer changed before final handle cleanup");
        }
        // SAFETY: exact synchronous exclusive factory handle to our linked peer.
        unsafe {
            namespace::unlink_existing(&writable_peer)?;
        }
        drop(writable_peer);
        drop(peer);
        // This exact new private receipt HANDLE, not its string name, is the
        // only cleanup authority. A failed unlink above never reaches cleanup.
        remove_handle(&receipt)?;
        drop(receipt);
        self.0.namespace_capable.store(true, Ordering::Release);
        Ok(())
    }
    fn pin(live: &mut Live, token: &RetainedToken) -> Arc<Pin> {
        if let Some(pin) = live.get(token).and_then(Weak::upgrade) {
            return pin;
        }
        let pin = Arc::new(());
        live.insert(token.unleased(), Arc::downgrade(&pin));
        pin
    }
    fn eligible(live: &Live, token: &RetainedToken, expires_at: u64) -> Result<()> {
        if now()? >= expires_at && live.get(token).and_then(Weak::upgrade).is_none() {
            bail!("Retained recovery deadline passed; an expired receipt cannot create a new live lease");
        }
        Ok(())
    }
    fn read_manifest(
        &self,
        token: &RetainedToken,
        owner: Option<&RetainedOwner>,
    ) -> Result<Manifest> {
        RetainedToken::parse(token.id())?;
        let file = namespace::open_existing(
            &self.0.directory,
            OsStr::new(&receipt_name(token)),
            OBJECT_ACCESS,
        )?;
        regular(&file)?;
        review_journal::windows_private(&file)?;
        if file.metadata()?.len() > MAX_RECORD_BYTES as u64 {
            bail!("Retained manifest is oversized");
        }
        let mut bytes = Vec::new();
        (&file)
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        Manifest::decode(&bytes, token, owner)
    }
    fn read_missing(
        &self,
        token: &RetainedToken,
        owner: Option<&RetainedOwner>,
    ) -> Result<MissingMarker> {
        RetainedToken::parse(token.id())?;
        let file = namespace::open_existing(
            &self.0.directory,
            OsStr::new(&missing_name(token)),
            OBJECT_ACCESS,
        )?;
        regular(&file)?;
        review_journal::windows_private(&file)?;
        let mut bytes = Vec::new();
        (&file).take(4097).read_to_end(&mut bytes)?;
        MissingMarker::decode(&bytes, token, owner)
    }
    fn validate_binding(&self, record: &Manifest) -> Result<()> {
        match (&record.missing_capture, record.kind) {
            (None, RetentionKind::Link) => Ok(()),
            (Some(checksum), RetentionKind::Move) => {
                let marker = self.read_missing(&record.token, Some(&record.owner))?;
                if checksum != &marker.checksum
                    || record.created_at != marker.created_at
                    || record.expires_at != marker.expires_at
                {
                    bail!("Retained move does not match its private first missing capture");
                }
                Ok(())
            }
            _ => bail!("Retained object has no valid private first-capture binding"),
        }
    }
    pub(crate) fn record_missing(
        &self,
        owner: &RetainedOwner,
        before: &ReviewImage,
    ) -> Result<RetainedToken> {
        if before != &ReviewImage::missing() {
            bail!("Only the canonical missing image can be captured");
        }
        owner.validate()?;
        let mut live = self.lock()?;
        let inventory = self.inventory()?;
        if let Some(marker) = inventory
            .missing
            .values()
            .find(|marker| &marker.owner == owner)
        {
            Self::eligible(&live, &marker.token, marker.expires_at)?;
            let pin = Self::pin(&mut live, &marker.token);
            return Ok(marker.token.with_lease(pin, self.0.clone()));
        }
        if inventory
            .records
            .values()
            .any(|record| &record.owner == owner)
        {
            bail!("A first existing snapshot cannot be replaced by missing capture");
        }
        let marker = MissingMarker::new(RetainedToken::fresh(), owner.clone(), before, now()?)?;
        if inventory.len() >= MAX_RECORDS
            || inventory
                .charge()?
                .checked_add(marker.charge())
                .context("Missing marker quota overflow")?
                > MAX_STORE_BYTES - PROBE_RESERVE
        {
            bail!("No reserved slot for a future created-file inverse; missing capture remains read-only");
        }
        let mut file =
            namespace::create_private(&self.0.directory, OsStr::new(&missing_name(&marker.token)))?;
        file.write_all(&marker.encode()?)?;
        file.sync_all()?;
        let pin = Self::pin(&mut live, &marker.token);
        Ok(marker.token.with_lease(pin, self.0.clone()))
    }
    pub(crate) fn lookup_missing(
        &self,
        owner: &RetainedOwner,
        before: &ReviewImage,
    ) -> Result<Option<RetainedToken>> {
        if before != &ReviewImage::missing() {
            bail!("Missing lookup requires the canonical absent snapshot");
        }
        owner.validate()?;
        let mut live = self.lock()?;
        let inventory = self.inventory()?;
        for marker in inventory.missing.values() {
            if &marker.owner != owner {
                continue;
            }
            Self::eligible(&live, &marker.token, marker.expires_at)?;
            let pin = Self::pin(&mut live, &marker.token);
            return Ok(Some(marker.token.with_lease(pin, self.0.clone())));
        }
        Ok(None)
    }
    fn inventory(&self) -> Result<Inventory> {
        let names = pinned_entries::entries(&self.0.directory)?;
        if names.len() > MAX_RECORDS * 3 {
            bail!("Retained store inventory exceeds its entry bound");
        }
        let mut records = BTreeMap::new();
        let mut missing = BTreeMap::new();
        let mut actual_bytes = BTreeMap::new();
        let mut objects = Vec::new();
        for name in names {
            let name = name
                .to_str()
                .context("Unexpected non-text retained store entry")?;
            if let Some(id) = name.strip_suffix(".json") {
                let token = RetainedToken::parse(id)?;
                records.insert(token.clone(), self.read_manifest(&token, None)?);
            } else if let Some(id) = name.strip_suffix(".object") {
                objects.push(RetainedToken::parse(id)?);
            } else if let Some(id) = name.strip_suffix(".missing") {
                let token = RetainedToken::parse(id)?;
                missing.insert(token.clone(), self.read_missing(&token, None)?);
            } else {
                bail!("Unknown retained store entry; no automatic deletion is authorized");
            }
        }
        for object in objects {
            let record = records
                .get(&object)
                .context("Retained object has no durable intent; manual recovery is required")?;
            let file = self.open_object(record)?;
            // Shared writes can exceed the interactive byte limit. Metadata
            // inspection and owned-alias reclamation must remain possible;
            // admission charges this actual size, never zero/a stale estimate.
            actual_bytes.insert(object, file.metadata()?.len());
        }
        for record in records.values() {
            self.validate_binding(record)?;
            match (&record.missing_capture, missing.get(&record.token)) {
                (Some(checksum), Some(marker))
                    if record.kind == RetentionKind::Move
                        && checksum == &marker.checksum
                        && record.owner == marker.owner =>
                {
                    ()
                }
                (None, None) if record.kind == RetentionKind::Link => (),
                _ => bail!("Retained move is not bound to its private first missing capture"),
            }
        }
        let inventory = Inventory {
            records,
            missing,
            actual_bytes,
        };
        if inventory.len() > MAX_RECORDS {
            bail!("Retained store entry quota exceeded");
        }
        Ok(inventory)
    }
    fn open_object(&self, record: &Manifest) -> Result<File> {
        let file = namespace::open_existing(
            &self.0.directory,
            OsStr::new(&object_name(&record.token)),
            PIN_ACCESS,
        )?;
        regular(&file)?;
        if identity(&file)? != record.identity {
            bail!("Retained object identity differs from its durable intent");
        }
        Ok(file)
    }
    fn internal_links(&self, wanted: &WindowsIdentity) -> Result<u32> {
        let mut count = 0u32;
        for record in self.inventory()?.records.into_values() {
            if &record.identity != wanted {
                continue;
            }
            match self.open_object(&record) {
                Ok(_) => {
                    count = count
                        .checked_add(1)
                        .context("Retained link count overflow")?;
                }
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    ()
                }
                Err(error) => return Err(error),
            }
        }
        Ok(count)
    }
    pub(crate) fn verify_live_peer(&self, live: &File) -> Result<bool> {
        regular(live)?;
        let _serial = self.lock()?;
        let count = self.internal_links(&identity(live)?)?;
        if count == 0 {
            return Ok(false);
        }
        if links(live)?
            != count
                .checked_add(1)
                .context("Retained link count overflow")?
        {
            bail!("Live file has an untracked public or private alias");
        }
        Ok(true)
    }
    pub(crate) fn lookup(
        &self,
        owner: &RetainedOwner,
        before: &ReviewImage,
    ) -> Result<Option<RetainedToken>> {
        owner.validate()?;
        let mut live = self.lock()?;
        for record in self.inventory()?.records.into_values() {
            if &record.owner == owner {
                if record.validate()?.integrity_hash()? != before.integrity_hash()? {
                    bail!("Original retained snapshot was replaced");
                }
                // Missing/expired object retention does not invalidate the
                // independent ordinary same-object byte path. Its private
                // immutable binding MUST still verify before that fallback.
                if now()? >= record.expires_at
                    && live.get(&record.token).and_then(Weak::upgrade).is_none()
                {
                    return Ok(None);
                }
                match self.open_object(&record) {
                    Ok(_) => (),
                    Err(error)
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                    {
                        if pinned_entries::child_missing(
                            &self.0.directory,
                            OsStr::new(&object_name(&record.token)),
                        )? {
                            return Ok(None);
                        }
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
                let pin = Self::pin(&mut live, &record.token);
                return Ok(Some(record.token.with_lease(pin, self.0.clone())));
            }
        }
        Ok(None)
    }
    fn prepare(
        &self,
        owner: &RetainedOwner,
        source: &File,
        before: &ReviewImage,
        kind: RetentionKind,
    ) -> Result<PreparedRetention> {
        owner.validate()?;
        regular(source)?;
        let actual = identity(source)?;
        if before.windows_identity.as_ref() != Some(&actual)
            || actual.volume != identity(&self.0.directory)?.volume
        {
            bail!("Retaining this exact file requires a same-volume private object store");
        }
        let mut live = self.lock()?;
        let records = self.inventory()?;
        if links(source)?
            != self
                .internal_links(&actual)?
                .checked_add(1)
                .context("Retained link count overflow")?
        {
            bail!("Cannot retain a file with untracked hard links");
        }
        if records
            .records
            .values()
            .any(|record| &record.owner == owner)
        {
            bail!("Original snapshot already has a retained-object intent");
        }
        let marker = records
            .missing
            .values()
            .find(|marker| &marker.owner == owner);
        let (mut manifest, extra, extra_slot) = match kind {
            RetentionKind::Move => {
                let marker = marker.context("Created-file inverse requires a private missing capture from before the Tool; SQLite rows alone provide no authority")?;
                Self::eligible(&live, &marker.token, marker.expires_at)?;
                let mut manifest = Manifest::new(
                    marker.token.unleased(),
                    owner.clone(),
                    before,
                    kind,
                    marker.created_at,
                )?;
                manifest.missing_capture = Some(marker.checksum.clone());
                manifest.checksum = manifest.checksum()?;
                (manifest, 0, 0)
            }
            RetentionKind::Link => {
                if marker.is_some() {
                    bail!("A captured missing snapshot cannot be replaced by an existing before-image");
                }
                let manifest =
                    Manifest::new(RetainedToken::fresh(), owner.clone(), before, kind, now()?)?;
                let charge = manifest.charge()?;
                (manifest, charge, 1)
            }
        };
        manifest.checksum = manifest.checksum()?;
        let total = records
            .charge()?
            .checked_add(extra)
            .context("Retained quota overflow")?;
        if records.len() + extra_slot > MAX_RECORDS || total > MAX_STORE_BYTES - PROBE_RESERVE {
            bail!("Retained store capacity reached; no namespace was changed");
        }
        let mut file = namespace::create_private(
            &self.0.directory,
            OsStr::new(&receipt_name(&manifest.token)),
        )?;
        file.write_all(&manifest.encode()?)?;
        file.sync_all()?;
        let pin = Self::pin(&mut live, &manifest.token);
        let token = manifest.token.with_lease(pin.clone(), self.0.clone());
        Ok(PreparedRetention {
            store: self.clone(),
            manifest,
            token,
            _pin: pin,
        })
    }
    /// Caller supplies a synchronous exact-object handle from the review I/O
    /// factory. The immutable image has already been bounded by snapshot capture.
    pub(crate) fn capture(
        &self,
        owner: &RetainedOwner,
        source: &File,
        before: &ReviewImage,
    ) -> Result<RetainedToken> {
        if let Some(token) = self.lookup(owner, before)? {
            if identity(source)? != self.read_manifest(&token, Some(owner))?.identity {
                bail!("Snapshot source object changed");
            }
            return Ok(token);
        }
        let prepared = self.prepare(owner, source, before, RetentionKind::Link)?;
        let _serial = self.lock()?;
        // SAFETY: source is the caller's synchronous exact-object review handle;
        // target is this pinned private store and a fresh single-component name.
        unsafe {
            namespace::link_existing(
                source,
                &self.0.directory,
                OsStr::new(&object_name(&prepared.manifest.token)),
            )?;
        }
        if identity(source)? != prepared.manifest.identity
            || links(source)?
                != self
                    .internal_links(&prepared.manifest.identity)?
                    .checked_add(1)
                    .context("Retained link count overflow")?
        {
            bail!("Retained link has an unexpected identity or link count");
        }
        Ok(prepared.token.clone())
    }
    pub(crate) fn prepare_move(
        &self,
        owner: &RetainedOwner,
        source: &File,
        before: &ReviewImage,
    ) -> Result<PreparedRetention> {
        self.prepare(owner, source, before, RetentionKind::Move)
    }
    pub(crate) fn commit_move(
        &self,
        prepared: PreparedRetention,
        source: &File,
    ) -> Result<RetainedToken> {
        if !Arc::ptr_eq(&self.0, &prepared.store.0) || prepared.manifest.kind != RetentionKind::Move
        {
            bail!("Prepared retention belongs to another store or operation");
        }
        let _serial = self.lock()?;
        let expected_before = self
            .internal_links(&prepared.manifest.identity)?
            .checked_add(1)
            .context("Retained link count overflow")?;
        if identity(source)? != prepared.manifest.identity || links(source)? != expected_before {
            bail!("Created file object changed before its retained move");
        }
        // SAFETY: exact synchronous source name and pinned target, no replacement.
        unsafe {
            namespace::rename_existing(
                source,
                &self.0.directory,
                OsStr::new(&object_name(&prepared.manifest.token)),
            )?;
        }
        if identity(source)? != prepared.manifest.identity
            || links(source)? != self.internal_links(&prepared.manifest.identity)?
        {
            bail!("Retained move has an unexpected identity or link count");
        }
        Ok(prepared.token.clone())
    }
    pub(crate) fn open_verified(
        &self,
        token: &RetainedToken,
        owner: &RetainedOwner,
    ) -> Result<RetainedObject> {
        let mut live = self.lock()?;
        let record = self.read_manifest(token, Some(owner))?;
        self.validate_binding(&record)?;
        Self::eligible(&live, token, record.expires_at)?;
        let before = record.validate()?;
        let file = self.open_object(&record)?;
        let pin = Self::pin(&mut live, token);
        Ok(RetainedObject {
            file,
            before,
            identity: record.identity,
            token: token.clone(),
            _pin: pin,
            _store: self.clone(),
        })
    }
    /// Expiry is suspended by live review owners. Unknown/corrupt entries abort
    /// inspection instead of supplying deletion authority. No recursive cleanup.
    pub(crate) fn expire(&self, timestamp: u64) -> Result<usize> {
        let mut live = self.lock()?;
        let inventory = self.inventory()?;
        let mut expired = 0;
        let tokens: std::collections::BTreeSet<_> = inventory
            .records
            .keys()
            .chain(inventory.missing.keys())
            .cloned()
            .collect();
        for token in tokens {
            let record = inventory.records.get(&token);
            let marker = inventory.missing.get(&token);
            let deadline = marker
                .map(|marker| marker.expires_at)
                .or_else(|| record.map(|record| record.expires_at))
                .context("Empty retained slot")?;
            let oversized = inventory
                .actual_bytes
                .get(&token)
                .is_some_and(|size| *size > (2 * 1024 * 1024u64));
            if (!oversized && deadline > timestamp)
                || live.get(&token).and_then(Weak::upgrade).is_some()
            {
                continue;
            }
            if let Some(record) = record {
                let object = namespace::open_existing(
                    &self.0.directory,
                    OsStr::new(&object_name(&token)),
                    OBJECT_ACCESS | DELETE_ACCESS,
                );
                match object {
                    Ok(file) => {
                        regular(&file)?;
                        if identity(&file)? != record.identity {
                            bail!("Expired retained object identity changed");
                        }
                        remove_handle(&file)?;
                    }
                    // Nt relative lookup is against the pinned directory, not a
                    // named path susceptible to a missing-path retagging ambiguity.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
                let file = namespace::open_existing(
                    &self.0.directory,
                    OsStr::new(&receipt_name(&record.token)),
                    OBJECT_ACCESS | DELETE_ACCESS,
                )?;
                review_journal::windows_private(&file)?;
                let mut bytes = Vec::new();
                (&file)
                    .take(MAX_RECORD_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                Manifest::decode(&bytes, &record.token, Some(&record.owner))?;
                remove_handle(&file)?;
            }
            if let Some(marker) = marker {
                let file = namespace::open_existing(
                    &self.0.directory,
                    OsStr::new(&missing_name(&token)),
                    OBJECT_ACCESS | DELETE_ACCESS,
                )?;
                regular(&file)?;
                review_journal::windows_private(&file)?;
                let mut bytes = Vec::new();
                (&file).take(4097).read_to_end(&mut bytes)?;
                let current = MissingMarker::decode(&bytes, &token, Some(&marker.owner))?;
                if current.checksum != marker.checksum {
                    bail!("Missing marker changed during expiry");
                }
                remove_handle(&file)?;
            }
            live.remove(&token);
            expired += 1;
        }
        live.retain(|_, weak| weak.strong_count() != 0);
        Ok(expired)
    }
}
fn remove_handle(file: &File) -> Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
    // SAFETY: owned verified leaf HANDLE, correctly sized native input.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};

    struct Fixture {
        root: PathBuf,
        paths: Paths,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("formless-retained-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let paths = Paths {
                data_dir: root.join("data"),
                config_dir: root.join("config"),
                profile: "private-retained-test".into(),
            };
            std::fs::create_dir(&paths.data_dir).unwrap();
            Self { root, paths }
        }
        fn store(&self) -> RetainedStore {
            RetainedStore::open(&self.paths, true).unwrap().unwrap()
        }
        fn source(&self, name: &str, bytes: &[u8]) -> (PathBuf, File, ReviewImage) {
            let path = self.root.join(name);
            std::fs::write(&path, bytes).unwrap();
            let file = std::fs::OpenOptions::new()
                .access_mode(
                    OBJECT_ACCESS | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | DELETE_ACCESS,
                )
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
                .unwrap();
            let image = ReviewImage {
                bytes: Some(bytes.to_vec()),
                mode: Some(file.metadata().unwrap().file_attributes()),
                windows_identity: Some(identity(&file).unwrap()),
                unix_metadata: None,
                ..Default::default()
            };
            (path, file, image)
        }
        fn parent(&self) -> File {
            std::fs::OpenOptions::new()
                .access_mode(
                    FILE_ADD_FILE
                        | FILE_TRAVERSE
                        | FILE_LIST_DIRECTORY
                        | FILE_READ_ATTRIBUTES
                        | SYNCHRONIZE_ACCESS,
                )
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&self.root)
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn owner(run: &str) -> RetainedOwner {
        RetainedOwner {
            session_id: "session".into(),
            run_id: run.into(),
            path_digest: "e".repeat(64),
        }
    }

    #[test]
    fn normal_capture_keeps_immutable_images_across_writes_and_later_runs() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, before) = f.source("owned.txt", b"before");
        let first = store.capture(&owner("first"), &source, &before).unwrap();
        assert_eq!(links(&source).unwrap(), 2);
        let held = store.open_verified(&first, &owner("first")).unwrap();
        held.verify_peer(&source).unwrap();
        std::fs::write(&path, b"after!").unwrap();
        assert_eq!(held.before().bytes.as_deref(), Some(&b"before"[..]));
        assert_eq!(std::fs::read(&path).unwrap(), b"after!");
        let mut second_image = before.clone();
        second_image.bytes = Some(b"after!".to_vec());
        let second = store
            .capture(&owner("second"), &source, &second_image)
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(links(&source).unwrap(), 3);
        let second_held = store.open_verified(&second, &owner("second")).unwrap();
        second_held.verify_peer(&source).unwrap();
        held.verify_peer(&source).unwrap();
        assert_eq!(second_held.before().bytes.as_deref(), Some(&b"after!"[..]));
        assert!(store.open_verified(&first, &owner("second")).is_err());
        std::fs::hard_link(&path, f.root.join("untracked.txt")).unwrap();
        assert!(held.verify_peer(&source).is_err());
        assert!(store
            .capture(&owner("third"), &source, &second_image)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"after!");
    }

    #[test]
    fn deleted_object_restores_private_before_bytes_then_links_the_same_object() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, before) = f.source("deleted.txt", b"before");
        let token = store.capture(&owner("deleted"), &source, &before).unwrap();
        std::fs::write(&path, b"changed body").unwrap();
        drop(source);
        std::fs::remove_file(&path).unwrap();
        let object = store.open_verified(&token, &owner("deleted")).unwrap();
        object.require_retained_only().unwrap();
        let mut writable = object.open_for_mutation().unwrap();
        assert_eq!(
            identity(&writable).unwrap(),
            before.windows_identity.clone().unwrap()
        );
        writable.seek(SeekFrom::Start(0)).unwrap();
        writable.set_len(0).unwrap();
        writable
            .write_all(object.before().bytes.as_ref().unwrap())
            .unwrap();
        writable.sync_all().unwrap();
        assert!(!path.exists());
        // SAFETY: exclusive synchronous exact-object factory handle, pinned
        // ordinary parent, single-component no-replace target.
        unsafe { namespace::link_existing(&writable, &f.parent(), OsStr::new("deleted.txt")) }
            .unwrap();
        drop(writable);
        assert_eq!(std::fs::read(&path).unwrap(), b"before");
        let live = std::fs::OpenOptions::new()
            .access_mode(PIN_ACCESS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&path)
            .unwrap();
        object.verify_peer(&live).unwrap();
        assert_eq!(identity(&live).unwrap(), before.windows_identity.unwrap());
    }

    #[test]
    fn prepared_created_move_and_live_expiry_preserve_exact_object_and_other_files() {
        let f = Fixture::new();
        let store = f.store();
        let marker = store
            .record_missing(&owner("created"), &ReviewImage::missing())
            .unwrap();
        let (path, source, after) = f.source("created.txt", b"created");
        let prepared = store
            .prepare_move(&owner("created"), &source, &after)
            .unwrap();
        let token = prepared.token().clone();
        assert_eq!(token, marker);
        assert!(path.exists());
        assert_eq!(links(&source).unwrap(), 1);
        assert!(store.read_manifest(&token, Some(&owner("created"))).is_ok());
        store.commit_move(prepared, &source).unwrap();
        assert!(!path.exists());
        drop(source);
        let object = store.open_verified(&token, &owner("created")).unwrap();
        object.require_retained_only().unwrap();
        assert_eq!(
            identity(object.file()).unwrap(),
            after.windows_identity.clone().unwrap()
        );
        assert_eq!(store.expire(u64::MAX).unwrap(), 0);
        std::fs::write(&path, b"occupant").unwrap();
        // No replacement even if the caller has retained-object authority.
        assert!(unsafe {
            namespace::link_existing(object.file(), &f.parent(), OsStr::new("created.txt"))
        }
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"occupant");
        let decoded = token.unleased();
        let weak = Arc::downgrade(&token.lease.as_ref().unwrap().pin);
        drop(token);
        drop(marker);
        assert!(weak.upgrade().is_some());
        drop(object);
        assert!(weak.upgrade().is_none());
        assert_eq!(store.expire(u64::MAX).unwrap(), 1);
        assert!(store.open_verified(&decoded, &owner("created")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"occupant");
    }

    #[test]
    fn unknown_store_entries_refuse_cleanup_and_inspection_does_not_create() {
        let f = Fixture::new();
        assert!(RetainedStore::open(&f.paths, false).unwrap().is_none());
        assert!(!f.paths.data_dir.join("review-objects").exists());
        let store = f.store();
        let (path, source, before) = f.source("survivor.txt", b"sentinel");
        let token = store.capture(&owner("sentinel"), &source, &before).unwrap();
        drop(source);
        std::fs::write(
            f.paths.data_dir.join("review-objects/foreign-entry"),
            b"keep",
        )
        .unwrap();
        assert!(store.expire(u64::MAX).is_err());
        assert!(store
            .read_manifest(&token, Some(&owner("sentinel")))
            .is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"sentinel");
        assert_eq!(
            std::fs::read(f.paths.data_dir.join("review-objects/foreign-entry")).unwrap(),
            b"keep"
        );
    }
    #[test]
    fn a_token_alone_preserves_the_same_store_and_pin_index_across_reopen() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, before) = f.source("retained-owner.txt", b"sentinel");
        let token = store.capture(&owner("pin"), &source, &before).unwrap();
        let weak_store = Arc::downgrade(&store.0);
        let weak_pin = Arc::downgrade(&token.lease.as_ref().unwrap().pin);
        drop(source);
        drop(store);
        assert!(weak_store.upgrade().is_some());
        let reopened = f.store();
        assert!(Arc::ptr_eq(&weak_store.upgrade().unwrap(), &reopened.0));
        assert_eq!(reopened.expire(u64::MAX).unwrap(), 0);
        let last = token.clone();
        drop(token);
        assert_eq!(reopened.expire(u64::MAX).unwrap(), 0);
        drop(last);
        assert!(weak_pin.upgrade().is_none());
        assert_eq!(reopened.expire(u64::MAX).unwrap(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"sentinel");
        drop(reopened);
        assert!(weak_store.upgrade().is_none());
    }
    #[test]
    fn namespace_probe_proves_support_before_reject_and_cleans_only_its_objects() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, before) = f.source("probe-sentinel.txt", b"keep");
        let token = store.capture(&owner("probe"), &source, &before).unwrap();
        store.ensure_namespace_capability().unwrap();
        assert!(store.0.namespace_capable.load(Ordering::Acquire));
        assert_eq!(
            pinned_entries::entries(&store.0.directory).unwrap().len(),
            2
        );
        store
            .ensure_namespace_capability_with(|_| {
                panic!("cached success must not create another probe")
            })
            .unwrap();
        assert!(store.open_verified(&token, &owner("probe")).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    }
    #[test]
    fn uncertain_namespace_probe_keeps_bounded_durable_intent_and_original_files() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, before) = f.source("probe-error-sentinel.txt", b"keep");
        let token = store
            .capture(&owner("probe-error"), &source, &before)
            .unwrap();
        let error = store
            .ensure_namespace_capability_with(|_| anyhow::bail!("controlled unsupported unlink"))
            .unwrap_err();
        assert!(error.to_string().contains("controlled unsupported unlink"));
        assert!(!store.0.namespace_capable.load(Ordering::Acquire));
        let names = pinned_entries::entries(&store.0.directory).unwrap();
        assert_eq!(names.len(), 5);
        assert!(names
            .iter()
            .any(|name| name == "namespace-capability.intent"));
        assert!(names
            .iter()
            .any(|name| name == "namespace-capability.probe"));
        assert!(names.iter().any(|name| name == "namespace-capability.peer"));
        assert!(store.ensure_namespace_capability().is_err());
        assert!(store.expire(u64::MAX).is_err());
        assert!(store
            .read_manifest(&token, Some(&owner("probe-error")))
            .is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    }
    #[test]
    fn renamed_public_alias_is_untouched_until_it_is_retained_and_undo_stays_lifo() {
        let f = Fixture::new();
        let store = f.store();
        let (a, source, before_a) = f.source("rename-a.txt", b"before A");
        let token_a = store
            .capture(&owner("rename-a"), &source, &before_a)
            .unwrap();
        let _marker_b = store
            .record_missing(&owner("rename-b"), &ReviewImage::missing())
            .unwrap();
        let b = f.root.join("rename-b.txt");
        std::fs::rename(&a, &b).unwrap();
        std::fs::write(&b, b"changed B").unwrap();
        drop(source);
        let object_a = store.open_verified(&token_a, &owner("rename-a")).unwrap();
        assert!(object_a.require_retained_only().is_err());
        assert!(object_a.open_for_mutation().is_err());
        assert_eq!(std::fs::read(&b).unwrap(), b"changed B");
        let source_b = std::fs::OpenOptions::new()
            .access_mode(OBJECT_ACCESS | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | DELETE_ACCESS)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&b)
            .unwrap();
        let mut before_b = before_a.clone();
        before_b.bytes = Some(b"changed B".to_vec());
        let prepared_b = store
            .prepare_move(&owner("rename-b"), &source_b, &before_b)
            .unwrap();
        let token_b = store.commit_move(prepared_b, &source_b).unwrap();
        drop(source_b);
        assert!(!a.exists() && !b.exists());
        let object_b = store.open_verified(&token_b, &owner("rename-b")).unwrap();
        object_a.require_retained_only().unwrap();
        let mut write_a = object_a.open_for_mutation().unwrap();
        write_a.set_len(0).unwrap();
        write_a
            .write_all(object_a.before().bytes.as_ref().unwrap())
            .unwrap();
        write_a.sync_all().unwrap();
        unsafe { namespace::link_existing(&write_a, &f.parent(), OsStr::new("rename-a.txt")) }
            .unwrap();
        drop(write_a);
        assert_eq!(std::fs::read(&a).unwrap(), b"before A");
        assert!(object_b.open_for_mutation().is_err()); // Undo B before Undo A is unsafe.
        assert_eq!(std::fs::read(&a).unwrap(), b"before A");
        std::fs::remove_file(&a).unwrap(); // Undo A first.
        object_b.require_retained_only().unwrap();
        let mut write_b = object_b.open_for_mutation().unwrap();
        write_b.set_len(0).unwrap();
        write_b
            .write_all(object_b.before().bytes.as_ref().unwrap())
            .unwrap();
        write_b.sync_all().unwrap();
        unsafe { namespace::link_existing(&write_b, &f.parent(), OsStr::new("rename-b.txt")) }
            .unwrap();
        drop(write_b);
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"changed B");
        assert_eq!(object_a.before().bytes.as_deref(), Some(&b"before A"[..]));
        assert_eq!(object_b.before().bytes.as_deref(), Some(&b"changed B"[..]));
    }
    #[test]
    fn sqlite_only_created_claim_cannot_prepare_a_move_without_private_first_capture() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, after) = f.source("foreign-created-claim.txt", b"untouched");
        assert!(store
            .lookup_missing(&owner("forged"), &ReviewImage::missing())
            .unwrap()
            .is_none());
        assert!(store
            .prepare_move(&owner("forged"), &source, &after)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"untouched");
        assert!(pinned_entries::entries(&store.0.directory)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn reserved_missing_slot_can_upgrade_when_no_new_slot_can_be_admitted() {
        let f = Fixture::new();
        let store = f.store();
        let first_owner = owner("reserved-0");
        let first = store
            .record_missing(&first_owner, &ReviewImage::missing())
            .unwrap();
        let mut owners = vec![first.clone()];
        let mut full = false;
        for index in 1..MAX_RECORDS + 1 {
            match store.record_missing(
                &owner(&format!("reserved-{index}")),
                &ReviewImage::missing(),
            ) {
                Ok(token) => owners.push(token),
                Err(_) => {
                    full = true;
                    break;
                }
            }
        }
        assert!(full);
        assert!(owners.len() > 1);
        let before_slots = store.inventory().unwrap().len();
        let (path, source, image) = f.source("reserved-created.txt", b"created");
        let prepared = store.prepare_move(&first_owner, &source, &image).unwrap();
        assert_eq!(prepared.token(), &first);
        let token = store.commit_move(prepared, &source).unwrap();
        drop(source);
        assert!(!path.exists());
        assert_eq!(store.inventory().unwrap().len(), before_slots);
        let object = store.open_verified(&token, &first_owner).unwrap();
        assert_eq!(object.before().bytes.as_deref(), Some(&b"created"[..]));
        object.require_retained_only().unwrap();
    }
    #[test]
    fn expired_receipts_cannot_mint_new_pins_but_existing_owner_keeps_its_window() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, before) = f.source("expiry-sentinel.txt", b"keep");
        let token = store.capture(&owner("expiry"), &source, &before).unwrap();
        drop(source);
        let mut record = store.read_manifest(&token, Some(&owner("expiry"))).unwrap();
        record.created_at = now().unwrap().checked_sub(RETENTION_SECONDS + 1).unwrap();
        record.expires_at = record.created_at + RETENTION_SECONDS;
        record.checksum = record.checksum().unwrap();
        let mut file = namespace::open_existing(
            &store.0.directory,
            OsStr::new(&receipt_name(&token)),
            OBJECT_ACCESS | FILE_WRITE_DATA,
        )
        .unwrap();
        file.set_len(0).unwrap();
        file.write_all(&record.encode().unwrap()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert!(store.lookup(&owner("expiry"), &before).unwrap().is_some());
        assert!(store.open_verified(&token, &owner("expiry")).is_ok());
        let decoded = token.unleased();
        drop(token);
        drop(store);
        let reopened = f.store();
        assert!(reopened
            .lookup(&owner("expiry"), &before)
            .unwrap()
            .is_none());
        let mut forged = before.clone();
        forged.bytes = Some(b"forged historical bytes".to_vec());
        assert!(reopened.lookup(&owner("expiry"), &forged).is_err());
        assert!(reopened.open_verified(&decoded, &owner("expiry")).is_err());
        assert_eq!(reopened.expire(now().unwrap()).unwrap(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    }
    #[test]
    fn oversized_owned_retainer_is_charged_honestly_and_remains_reclaimable() {
        let f = Fixture::new();
        let store = f.store();
        let (path, source, before) = f.source("grown-then-deleted.txt", b"before");
        let token = store.capture(&owner("grown"), &source, &before).unwrap();
        source.set_len(MAX_STORE_BYTES + 1).unwrap();
        drop(source);
        std::fs::remove_file(&path).unwrap();
        assert!(store.inventory().unwrap().charge().unwrap() > MAX_STORE_BYTES);
        assert_eq!(store.expire(now().unwrap()).unwrap(), 0); // Active owner remains protected.
        drop(token);
        assert_eq!(store.expire(now().unwrap()).unwrap(), 1);
        assert!(pinned_entries::entries(&store.0.directory)
            .unwrap()
            .is_empty());
        let (_new_path, new_source, new_before) = f.source("admitted-after-reclaim.txt", b"new");
        assert!(store
            .capture(&owner("new"), &new_source, &new_before)
            .is_ok());
    }
    #[test]
    fn expired_missing_marker_does_not_turn_sqlite_absence_into_fresh_authority() {
        let f = Fixture::new();
        let store = f.store();
        let token = store
            .record_missing(&owner("missing-expiry"), &ReviewImage::missing())
            .unwrap();
        let mut marker = store
            .read_missing(&token, Some(&owner("missing-expiry")))
            .unwrap();
        marker.created_at = now().unwrap().checked_sub(RETENTION_SECONDS + 1).unwrap();
        marker.expires_at = marker.created_at + RETENTION_SECONDS;
        marker.checksum = marker.checksum().unwrap();
        let mut file = namespace::open_existing(
            &store.0.directory,
            OsStr::new(&missing_name(&token)),
            OBJECT_ACCESS | FILE_WRITE_DATA,
        )
        .unwrap();
        file.set_len(0).unwrap();
        file.write_all(&marker.encode().unwrap()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert!(store
            .lookup_missing(&owner("missing-expiry"), &ReviewImage::missing())
            .unwrap()
            .is_some());
        drop(token);
        drop(store);
        let reopened = f.store();
        assert!(reopened
            .lookup_missing(&owner("missing-expiry"), &ReviewImage::missing())
            .is_err());
        let (path, source, after) = f.source("late-created.txt", b"keep");
        assert!(reopened
            .prepare_move(&owner("missing-expiry"), &source, &after)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        assert_eq!(reopened.expire(now().unwrap()).unwrap(), 1);
    }
    #[test]
    fn namespace_probe_and_marker_upgrade_fit_at_the_exact_logical_slot_limit() {
        let f = Fixture::new();
        let store = f.store();
        let reserved_owner = owner("last-reserved");
        let marker = store
            .record_missing(&reserved_owner, &ReviewImage::missing())
            .unwrap();
        let mut leases = vec![marker.clone()];
        for index in 1..MAX_RECORDS {
            let (_path, source, image) = f.source(&format!("occupied-{index}.txt"), b"small");
            match store.capture(&owner(&format!("occupied-{index}")), &source, &image) {
                Ok(token) => leases.push(token),
                Err(_) => break,
            }
        }
        assert!(store.inventory().unwrap().len() <= MAX_RECORDS);
        let admitted = store.inventory().unwrap().len();
        assert!(!store.0.namespace_capable.load(Ordering::Acquire));
        store.ensure_namespace_capability().unwrap();
        let (path, source, image) = f.source("last-created.txt", b"created");
        let prepared = store
            .prepare_move(&reserved_owner, &source, &image)
            .unwrap();
        assert_eq!(prepared.token(), &marker);
        let token = store.commit_move(prepared, &source).unwrap();
        drop(source);
        assert!(!path.exists());
        assert_eq!(store.inventory().unwrap().len(), admitted);
        let object = store.open_verified(&token, &reserved_owner).unwrap();
        object.require_retained_only().unwrap();
    }
    #[test]
    fn cross_volume_identity_is_refused_without_a_namespace_mutation() {
        let f = Fixture::new();
        let store = f.store();
        let (_path, source, image) = f.source("volume-source.txt", b"keep");
        let actual = identity(&source).unwrap();
        assert!(store.supports_identity(&actual).unwrap());
        let foreign = WindowsIdentity {
            volume: actual.volume ^ 1,
            file: actual.file,
        };
        assert!(!store.supports_identity(&foreign).unwrap());
        assert_eq!(image.windows_identity.as_ref(), Some(&actual));
        assert!(pinned_entries::entries(&store.0.directory)
            .unwrap()
            .is_empty());
    }
}
