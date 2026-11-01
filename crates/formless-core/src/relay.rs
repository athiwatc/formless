//! Hosted relay client (SPEC §17 v2): the daemon dials out to the relay
//! (`wss://formless.athiwat.xyz` by default) and browsers meet it there.
//! The relay only forwards opaque AES-256-GCM ciphertext — the pairing
//! secret (32 random bytes, base64url) *is* the key, and the room id is
//! a truncated SHA-256 of a domain-separated daemon claim, so the relay
//! cannot derive the pairing key or read traffic from its routing proof.
//!
//! Frames are protocol v2 (SPEC §12). Each browser connection starts with
//! a handshake: both sides send `{"v":2,"hello":<16 random bytes>}` and
//! derive a fresh session key with HKDF-SHA256 over the pairing key,
//! salted with both nonces. Every later frame carries a per-direction
//! sequence number that is bound into the AES-GCM associated data and
//! must be exactly one more than the last, so a captured frame cannot be
//! replayed (in this or any other session) or reflected back to its
//! sender, and a frame the relay withholds breaks the channel instead of
//! vanishing (#905).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::config::{load_relay_secret, RelayCfg};
use crate::daemon::Daemon;

/// The pairing key. It and every copy derived from it are wiped when
/// dropped (#908).
pub fn key_from_secret(secret: &str) -> Result<Zeroizing<[u8; 32]>> {
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(secret.trim())
            .context("relay secret is not base64url")?,
    );
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("relay secret must decode to 32 bytes"))?;
    Ok(Zeroizing::new(key))
}

/// Distinct daemon-slot capability. It never serves as an E2E traffic key.
/// The room commits to this derived value, avoiding attacker-controlled TOFU.
pub fn daemon_claim(key: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut claim = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<Sha256>::new(None, key)
        .expand(b"formless-relay/3 daemon-claim", claim.as_mut_slice())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    claim
}

pub fn room_id(key: &[u8; 32]) -> String {
    let claim = daemon_claim(key);
    let digest = Sha256::digest(claim.as_slice());
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Header-only proof, marked sensitive so request Debug output omits it.
/// Never put the derived capability or pairing secret in a relay URL.
pub fn daemon_authorization(key: &[u8; 32]) -> tokio_tungstenite::tungstenite::http::HeaderValue {
    let claim = daemon_claim(key);
    let mut value = Zeroizing::new(String::from("Bearer "));
    use std::fmt::Write as _;
    for byte in claim.iter() {
        write!(&mut *value, "{byte:02x}").expect("write to String");
    }
    let mut header = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&value)
        .expect("canonical ASCII daemon claim");
    header.set_sensitive(true);
    header
}

pub const FRAME_VERSION: u64 = 2;
const HELLO_BYTES: usize = 16;
const KDF_INFO: &[u8] = b"formless-relay/2";

/// Which end of a relay channel this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Browser,
    Daemon,
}

impl Side {
    fn direction(self) -> &'static str {
        match self {
            Side::Browser => "b2d",
            Side::Daemon => "d2b",
        }
    }

    fn peer(self) -> Side {
        match self {
            Side::Browser => Side::Daemon,
            Side::Daemon => Side::Browser,
        }
    }
}

/// A fresh handshake nonce.
pub fn new_hello() -> [u8; HELLO_BYTES] {
    let mut nonce = [0u8; HELLO_BYTES];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    nonce
}

pub fn hello_frame(nonce: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    json!({"v": FRAME_VERSION, "hello": b64.encode(nonce)}).to_string()
}

/// The value proving one side of the handshake holds the pairing key:
/// HKDF over the key, salted with both nonces (so it is bound to this
/// transcript), with the sender's direction as info (#904).
pub fn confirm_tag(
    key: &[u8; 32],
    browser_hello: &[u8],
    daemon_hello: &[u8],
    from: Side,
) -> [u8; 32] {
    let salt = [browser_hello, daemon_hello].concat();
    let mut tag = [0u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(&salt), key)
        .expand(
            format!("formless-relay/2 confirm {}", from.direction()).as_bytes(),
            &mut tag,
        )
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    tag
}

/// The daemon's handshake reply: its nonce and the proof it has the key.
pub fn daemon_hello_frame(key: &[u8; 32], browser_hello: &[u8], daemon_hello: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let tag = confirm_tag(key, browser_hello, daemon_hello, Side::Daemon);
    json!({"v": FRAME_VERSION, "hello": b64.encode(daemon_hello), "confirm": b64.encode(tag)})
        .to_string()
}

/// The browser's answer once it has checked the daemon's proof: its own.
pub fn confirm_frame(key: &[u8; 32], browser_hello: &[u8], daemon_hello: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let tag = confirm_tag(key, browser_hello, daemon_hello, Side::Browser);
    json!({"v": FRAME_VERSION, "confirm": b64.encode(tag)}).to_string()
}

/// The tag in a confirmation frame (one without a `hello`).
pub fn parse_confirm(frame: &str) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_str(frame).ok()?;
    if v["v"].as_u64() != Some(FRAME_VERSION) || v.get("hello").is_some() {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(v["confirm"].as_str()?)
        .ok()
}

/// The nonce of a handshake frame, or `None` if `frame` is not one.
pub fn parse_hello(frame: &str) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_str(frame).ok()?;
    if v["v"].as_u64() != Some(FRAME_VERSION) {
        return None;
    }
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(v["hello"].as_str()?)
        .ok()?;
    (nonce.len() == HELLO_BYTES).then_some(nonce)
}

/// One end of an established relay channel: a per-connection session key
/// and per-direction sequence numbers.
pub struct Channel {
    cipher: Aes256Gcm,
    side: Side,
    sent: u64,
    received: u64,
}

impl Channel {
    pub fn new(key: &[u8; 32], browser_hello: &[u8], daemon_hello: &[u8], side: Side) -> Self {
        let salt = [browser_hello, daemon_hello].concat();
        let mut session_key = Zeroizing::new([0u8; 32]);
        hkdf::Hkdf::<Sha256>::new(Some(&salt), key)
            .expand(KDF_INFO, session_key.as_mut_slice())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        Self {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(session_key.as_slice())),
            side,
            sent: 0,
            received: 0,
        }
    }

    fn aad(side: Side, seq: u64) -> String {
        format!("formless-relay/2 {} {seq}", side.direction())
    }

    pub fn seal(&mut self, plaintext: &str) -> Result<String> {
        self.sent += 1;
        let mut iv = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut iv);
        let aad = Self::aad(self.side, self.sent);
        let ct = self
            .cipher
            .encrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("relay encryption failed"))?;
        let b64 = base64::engine::general_purpose::STANDARD;
        Ok(json!({
            "v": FRAME_VERSION,
            "seq": self.sent,
            "iv": b64.encode(iv),
            "ct": b64.encode(ct),
        })
        .to_string())
    }

    pub fn open(&mut self, frame: &str) -> Result<String> {
        let v: serde_json::Value =
            serde_json::from_str(frame).context("relay frame is not JSON")?;
        if v["v"].as_u64() != Some(FRAME_VERSION) {
            return Err(anyhow!("unsupported relay frame version"));
        }
        let seq = v["seq"].as_u64().context("relay frame missing seq")?;
        if seq <= self.received {
            return Err(anyhow!("relay frame replayed (seq {seq})"));
        }
        if seq != self.received + 1 {
            return Err(anyhow!(
                "relay frame out of sequence (expected {}, got {seq}): a frame went missing",
                self.received + 1
            ));
        }
        let b64 = base64::engine::general_purpose::STANDARD;
        let iv = b64
            .decode(v["iv"].as_str().context("relay frame missing iv")?)
            .context("bad relay iv")?;
        if iv.len() != 12 {
            return Err(anyhow!("bad relay iv"));
        }
        let ct = b64
            .decode(v["ct"].as_str().context("relay frame missing ct")?)
            .context("bad relay ciphertext")?;
        let aad = Self::aad(self.side.peer(), seq);
        let pt = self
            .cipher
            .decrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: &ct,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("relay decryption failed (wrong pairing secret?)"))?;
        self.received = seq;
        String::from_utf8(pt).context("relay plaintext is not UTF-8")
    }
}

pub fn relay_ws_url(base: &str, secret: &str) -> Result<String> {
    let key = key_from_secret(secret)?;
    Ok(format!(
        "{}/relay/{}?role=daemon&mux=1&transport=1",
        base.trim_end_matches('/'),
        room_id(&key)
    ))
}

/// The browser URL carrying the pairing secret in its fragment. Check the
/// relay address with [`RelayCfg::check`] before showing it.
pub fn browser_url(cfg: &RelayCfg, secret: &str) -> String {
    format!("{}/#k={secret}", cfg.web_url())
}

#[path = "relay_endpoint.rs"]
mod endpoint;
#[path = "relay_link.rs"]
mod link;
#[path = "relay_transport.rs"]
mod transport;

/// A broken E2E channel requires a fresh browser generation, unlike a kick.
pub const INTEGRITY_CLOSE: u16 = 4002;

/// A bounded native relay transport. Each browser owns an independent actor,
/// IPC connection and byte leases; slow consumers cannot park socket routing.
pub async fn connect_once(
    daemon: Arc<Daemon>,
    base_url: &str,
    secret: &str,
    shutdown: &CancellationToken,
) -> Result<()> {
    link::connect(daemon, base_url, secret, shutdown).await
}

/// A link that lasted this long resets the reconnect backoff; a relay that
/// accepts and drops at once does not (#906).
const STABLE_LINK: std::time::Duration = std::time::Duration::from_secs(60);
/// Reconnect backoff bounds, in milliseconds.
const BACKOFF_MIN_MS: u64 = 1_000;
const BACKOFF_MAX_MS: u64 = 30_000;

/// The backoff ceiling after a link that lasted `lasted`.
pub fn next_backoff(ceiling_ms: u64, lasted: std::time::Duration) -> u64 {
    if lasted >= STABLE_LINK {
        BACKOFF_MIN_MS
    } else {
        (ceiling_ms * 2).clamp(BACKOFF_MIN_MS, BACKOFF_MAX_MS)
    }
}

/// How long to wait under `ceiling_ms`: full jitter, so daemons that lost
/// the relay together do not come back in waves (#906).
pub fn jittered(ceiling_ms: u64) -> std::time::Duration {
    use rand::Rng;
    std::time::Duration::from_millis(rand::thread_rng().gen_range(250..=ceiling_ms.max(250)))
}

/// Reconnect loop for `Daemon::serve`; runs only when relay is enabled
/// and a pairing secret exists.
pub async fn relay_loop(daemon: Arc<Daemon>) {
    let shutdown = daemon.shutdown.clone();
    let mut ceiling_ms = BACKOFF_MIN_MS;
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let cfg = match daemon.config() {
            Ok(c) if c.relay.enabled => c.relay,
            Ok(_) => return,
            Err(_) => return,
        };
        let Some(secret) = load_relay_secret(&daemon.paths).map(Zeroizing::new) else {
            tracing::warn!("relay enabled but no pairing secret; run `formless relay enable`");
            return;
        };
        if let Err(e) = cfg.check() {
            tracing::warn!("relay not started: {e:#}");
            return;
        }
        let started = std::time::Instant::now();
        let ended = connect_once(daemon.clone(), &cfg.url, &secret, &shutdown).await;
        ceiling_ms = next_backoff(ceiling_ms, started.elapsed());
        let wait = jittered(ceiling_ms);
        match ended {
            Ok(()) => tracing::info!(
                "relay link closed; reconnecting in {:.1}s",
                wait.as_secs_f64()
            ),
            Err(e) => tracing::warn!(
                "relay connection ended: {e}; retrying in {:.1}s",
                wait.as_secs_f64()
            ),
        }
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}
