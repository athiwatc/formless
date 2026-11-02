//! Durable Windows object retention. A token is an index into an owned private
//! manifest, never a caller path or authority derived from SQLite metadata.
//! Immutable before-bytes live in the manifest; the retained hard link shares
//! later writes with the original object and is deliberately NOT a snapshot.
use crate::review_file_io::{
    decode_before_metadata, encode_before_metadata, ReviewImage, WindowsIdentity,
};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_RECORD_BYTES: usize = 3 * 1024 * 1024;
pub(crate) const MAX_RECORDS: usize = 64;
pub(crate) const MAX_STORE_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const RETENTION_SECONDS: u64 = 7 * 24 * 60 * 60;

/// This locator is not a confidentiality boundary. Original file readers retain
/// their original rights to the retained object; only new receipt JSON is private.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RetainedToken {
    id: String,
    /// Prevent expiry while an in-process review owner retains this locator.
    /// Decoding a receipt never recreates this lease or filesystem authority.
    #[serde(skip)]
    lease: Option<std::sync::Arc<TokenLease>>,
}
struct TokenLease {
    pin: std::sync::Arc<()>,
    // The per-store registry is weak. A live token must keep that exact store
    // (and its weak pin index) alive across reopening, not merely the pin alone.
    _store: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}
impl PartialEq for RetainedToken {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for RetainedToken {}
impl PartialOrd for RetainedToken {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RetainedToken {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}
impl std::fmt::Debug for RetainedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetainedToken([private locator])")
    }
}
impl RetainedToken {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            bail!("Invalid retained-object locator");
        }
        Ok(Self {
            id: value.to_owned(),
            lease: None,
        })
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    #[cfg(windows)]
    fn unleased(&self) -> Self {
        Self {
            id: self.id.clone(),
            lease: None,
        }
    }
    #[cfg(windows)]
    fn with_lease(
        &self,
        pin: std::sync::Arc<()>,
        store: std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) -> Self {
        Self {
            id: self.id.clone(),
            lease: Some(std::sync::Arc::new(TokenLease {
                pin,
                _store: Some(store),
            })),
        }
    }
    #[cfg(windows)]
    fn fresh() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self {
            id: bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
            lease: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedOwner {
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    /// SHA256 of the exact server-bound snapshot path, not a post-change view ID.
    pub(crate) path_digest: String,
}
impl RetainedOwner {
    fn validate(&self) -> Result<()> {
        for value in [&self.session_id, &self.run_id] {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                bail!("Invalid retained-object owner");
            }
        }
        RetainedToken::parse(&self.path_digest)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum RetentionKind {
    Link,
    Move,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    checksum: String,
    token: RetainedToken,
    owner: RetainedOwner,
    identity: WindowsIdentity,
    kind: RetentionKind,
    created_at: u64,
    expires_at: u64,
    before_metadata: String,
    before_base64: String,
    missing_capture: Option<String>,
}
impl Manifest {
    fn new(
        token: RetainedToken,
        owner: RetainedOwner,
        image: &ReviewImage,
        kind: RetentionKind,
        now: u64,
    ) -> Result<Self> {
        owner.validate()?;
        let bytes = image
            .bytes
            .as_ref()
            .context("Cannot retain a missing file object")?;
        if bytes.len() > (2 * 1024 * 1024usize) {
            bail!("Retained before-image exceeds its byte limit");
        }
        let mut record = Self {
            version: 1,
            checksum: String::new(),
            token,
            owner,
            identity: image
                .windows_identity
                .clone()
                .context("Retained file has no strong Windows identity")?,
            kind,
            created_at: now,
            expires_at: now
                .checked_add(RETENTION_SECONDS)
                .context("Retention time overflow")?,
            before_metadata: encode_before_metadata(image)?,
            before_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            missing_capture: None,
        };
        record.checksum = record.checksum()?;
        record.validate()?;
        Ok(record)
    }
    fn validate(&self) -> Result<ReviewImage> {
        if self.checksum != self.checksum()? {
            bail!("Retained manifest checksum mismatch");
        }
        if self.version != 1
            || self.expires_at.checked_sub(self.created_at) != Some(RETENTION_SECONDS)
        {
            bail!("Unsupported or corrupt retained-object manifest");
        }
        RetainedToken::parse(self.token.id())?;
        self.owner.validate()?;
        if self.before_base64.len() > (2 * 1024 * 1024usize).div_ceil(3) * 4 {
            bail!("Retained before-image exceeds its encoded limit");
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&self.before_base64)?;
        if bytes.len() > (2 * 1024 * 1024usize) {
            bail!("Retained before-image exceeds its decoded limit");
        }
        let image = decode_before_metadata(&self.before_metadata, Some(&bytes))?;
        if image.windows_identity.as_ref() != Some(&self.identity) {
            bail!("Retained manifest identity disagrees with its before-image");
        }
        Ok(image)
    }
    fn checksum(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut unsigned = self.clone();
        unsigned.checksum.clear();
        let mut hash = Sha256::new();
        hash.update(b"formless-retained-object-v1\0");
        hash.update(serde_json::to_vec(&unsigned)?);
        Ok(format!("{:x}", hash.finalize()))
    }
    fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_RECORD_BYTES {
            bail!("Retained manifest exceeds its receipt limit");
        }
        Ok(bytes)
    }
    fn decode(bytes: &[u8], token: &RetainedToken, owner: Option<&RetainedOwner>) -> Result<Self> {
        if bytes.len() > MAX_RECORD_BYTES {
            bail!("Retained manifest exceeds its receipt limit");
        }
        let record: Self = serde_json::from_slice(bytes)?;
        record.validate()?;
        if &record.token != token || owner.is_some_and(|owner| owner != &record.owner) {
            bail!("Retained object belongs to a different snapshot");
        }
        Ok(record)
    }
    /// Reserve a full bounded mutable object, plus the immutable JSON bytes.
    /// Hard-link writes by external actors cannot themselves be quota-enforced.
    fn charge(&self) -> Result<u64> {
        Ok(self.encode()?.len() as u64 + (2 * 1024 * 1024usize) as u64)
    }
}

/// A first snapshot of an absent path is trusted only when this private marker
/// was flushed before the Tool ran. SQLite absence rows alone cannot mint it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissingMarker {
    version: u8,
    checksum: String,
    token: RetainedToken,
    owner: RetainedOwner,
    before_integrity: String,
    created_at: u64,
    expires_at: u64,
}
impl MissingMarker {
    fn new(
        token: RetainedToken,
        owner: RetainedOwner,
        before: &ReviewImage,
        timestamp: u64,
    ) -> Result<Self> {
        owner.validate()?;
        if before != &ReviewImage::missing() {
            bail!("Missing capture requires the exact canonical absent image");
        }
        let mut marker = Self {
            version: 1,
            checksum: String::new(),
            token,
            owner,
            before_integrity: before.integrity_hash()?,
            created_at: timestamp,
            expires_at: timestamp
                .checked_add(RETENTION_SECONDS)
                .context("Missing marker time overflow")?,
        };
        marker.checksum = marker.checksum()?;
        marker.validate()?;
        Ok(marker)
    }
    fn checksum(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut unsigned = self.clone();
        unsigned.checksum.clear();
        let mut hash = Sha256::new();
        hash.update(b"formless-review-missing-capture-v1\0");
        hash.update(serde_json::to_vec(&unsigned)?);
        Ok(format!("{:x}", hash.finalize()))
    }
    fn validate(&self) -> Result<()> {
        self.owner.validate()?;
        RetainedToken::parse(self.token.id())?;
        if self.version != 1
            || self.checksum != self.checksum()?
            || self.expires_at.checked_sub(self.created_at) != Some(RETENTION_SECONDS)
            || self.before_integrity != ReviewImage::missing().integrity_hash()?
        {
            bail!("Invalid private missing-capture marker");
        }
        Ok(())
    }
    fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > 4096 {
            bail!("Missing-capture marker exceeds its byte bound");
        }
        Ok(bytes)
    }
    fn decode(bytes: &[u8], token: &RetainedToken, owner: Option<&RetainedOwner>) -> Result<Self> {
        if bytes.len() > 4096 {
            bail!("Missing-capture marker exceeds its byte bound");
        }
        let marker: Self = serde_json::from_slice(bytes)?;
        marker.validate()?;
        if &marker.token != token || owner.is_some_and(|owner| &marker.owner != owner) {
            bail!("Missing-capture marker belongs to another snapshot");
        }
        Ok(marker)
    }
    /// Reserve the eventual created-image receipt/object BEFORE the Tool runs.
    /// Upgrading this exact token never needs a second logical store slot.
    fn charge(&self) -> u64 {
        MAX_RECORD_BYTES as u64 + (2 * 1024 * 1024usize) as u64 + 4096
    }
}

#[cfg(windows)]
#[path = "review_retained_windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{PreparedRetention, RetainedObject, RetainedStore};

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> RetainedOwner {
        RetainedOwner {
            session_id: "session".into(),
            run_id: "run".into(),
            path_digest: "a".repeat(64),
        }
    }
    fn image() -> ReviewImage {
        ReviewImage {
            bytes: Some(b"immutable before".to_vec()),
            windows_identity: Some(WindowsIdentity {
                volume: 5,
                file: [7; 16],
            }),
            security: Some(vec![1, 2, 3]),
            ..Default::default()
        }
    }
    fn record() -> Manifest {
        Manifest::new(
            RetainedToken::parse(&"b".repeat(64)).unwrap(),
            owner(),
            &image(),
            RetentionKind::Link,
            123,
        )
        .unwrap()
    }
    #[test]
    fn locator_and_snapshot_owner_never_accept_paths_or_other_runs() {
        for invalid in ["../object", "C:\\victim", &"A".repeat(64), &"b".repeat(63)] {
            assert!(RetainedToken::parse(invalid).is_err());
        }
        let record = record();
        let bytes = record.encode().unwrap();
        assert!(Manifest::decode(&bytes, &record.token, Some(&owner())).is_ok());
        let mut other = owner();
        other.run_id = "other".into();
        assert!(Manifest::decode(&bytes, &record.token, Some(&other)).is_err());
        assert!(Manifest::decode(
            &bytes,
            &RetainedToken::parse(&"c".repeat(64)).unwrap(),
            None
        )
        .is_err());
    }
    #[test]
    fn immutable_bytes_and_metadata_are_bound_separately_from_live_object() {
        let mut record = record();
        assert_eq!(record.validate().unwrap(), image());
        record.before_base64 =
            base64::engine::general_purpose::STANDARD.encode(b"later object write");
        assert!(record.validate().is_err());
        let mut record = self::record();
        record.identity.volume += 1;
        assert!(record.validate().is_err());
    }
    #[test]
    fn manifests_bound_expiry_size_unknown_fields_and_reserved_live_bytes() {
        let record = record();
        assert_eq!(record.expires_at - record.created_at, RETENTION_SECONDS);
        assert!(record.charge().unwrap() > (2 * 1024 * 1024usize) as u64);
        assert!(record.charge().unwrap() < MAX_STORE_BYTES);
        let mut bad = record.clone();
        bad.expires_at += 1;
        assert!(bad.encode().is_err());
        let mut bad = record.clone();
        bad.created_at += 10;
        bad.expires_at += 10;
        assert!(bad.encode().is_err());
        let mut bad = record.clone();
        bad.owner.run_id = "forged".into();
        assert!(bad.encode().is_err());
        let mut bad = record.clone();
        bad.before_base64 = "a".repeat(MAX_RECORD_BYTES);
        assert!(bad.encode().is_err());
        assert!(Manifest::decode(&vec![b' '; MAX_RECORD_BYTES + 1], &record.token, None).is_err());
        let mut value = serde_json::to_value(record.clone()).unwrap();
        value["path"] = "C:\\victim".into();
        assert!(
            Manifest::decode(&serde_json::to_vec(&value).unwrap(), &record.token, None).is_err()
        );
        assert!(Manifest::new(
            record.token,
            owner(),
            &image(),
            RetentionKind::Move,
            u64::MAX
        )
        .is_err());
        assert_eq!(MAX_RECORDS, 64);
    }
    #[test]
    fn serializing_a_locator_does_not_recreate_its_runtime_expiry_lease() {
        let pin = std::sync::Arc::new(());
        let weak = std::sync::Arc::downgrade(&pin);
        let mut token = RetainedToken::parse(&"d".repeat(64)).unwrap();
        token.lease = Some(std::sync::Arc::new(TokenLease { pin, _store: None }));
        let bytes = serde_json::to_vec(&token).unwrap();
        assert_eq!(bytes, format!("\"{}\"", "d".repeat(64)).as_bytes());
        let decoded: RetainedToken = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, token);
        assert!(decoded.lease.is_none());
        drop(token);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn private_missing_capture_binds_first_absence_owner_and_reserved_future_slot() {
        let token = RetainedToken::parse(&"f".repeat(64)).unwrap();
        let marker =
            MissingMarker::new(token.clone(), owner(), &ReviewImage::missing(), 123).unwrap();
        assert!(MissingMarker::decode(&marker.encode().unwrap(), &token, Some(&owner())).is_ok());
        assert!(marker.charge() >= MAX_RECORD_BYTES as u64 + (2 * 1024 * 1024usize) as u64);
        assert!(MissingMarker::new(token.clone(), owner(), &image(), 123).is_err());
        let mut other = owner();
        other.path_digest = "c".repeat(64);
        assert!(MissingMarker::decode(&marker.encode().unwrap(), &token, Some(&other)).is_err());
        let mut forged = marker.clone();
        forged.created_at += 10;
        forged.expires_at += 10;
        assert!(forged.encode().is_err());
        let mut forged = marker.clone();
        forged.before_integrity = image().integrity_hash().unwrap();
        forged.checksum = forged.checksum().unwrap();
        assert!(forged.encode().is_err());
        assert!(MissingMarker::decode(&vec![b' '; 4097], &token, None).is_err());
    }
}
