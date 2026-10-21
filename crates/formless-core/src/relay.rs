//! Hosted relay client (SPEC §17 v2): the daemon dials out to the relay
//! (`wss://formless.athiwat.xyz` by default) and browsers meet it there.
//! The relay only forwards opaque AES-256-GCM ciphertext — the pairing
//! secret (32 random bytes, base64url) *is* the key, and the room id is
//! a truncated SHA-256 of the key, so the relay cannot read traffic or
//! derive the key from the room it routes.
//!
//! Frames are protocol v2 (SPEC §12). Each browser connection starts with
//! a handshake: both sides send `{"v":2,"hello":<16 random bytes>}` and
//! derive a fresh session key with HKDF-SHA256 over the pairing key,
//! salted with both nonces. Every later frame carries a per-direction
//! sequence number that is bound into the AES-GCM associated data and
//! must increase, so a captured frame cannot be replayed (in this or any
//! other session) or reflected back to its sender.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::config::{load_relay_secret, RelayCfg};
use crate::daemon::Daemon;

pub fn key_from_secret(secret: &str) -> Result<[u8; 32]> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(secret.trim())
        .context("relay secret is not base64url")?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("relay secret must decode to 32 bytes"))
}

pub fn room_id(key: &[u8; 32]) -> String {
    let digest = Sha256::digest(key);
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
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
        let mut session_key = [0u8; 32];
        hkdf::Hkdf::<Sha256>::new(Some(&salt), key)
            .expand(KDF_INFO, &mut session_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        Self {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&session_key)),
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
        "{}/relay/{}?role=daemon&mux=1",
        base.trim_end_matches('/'),
        room_id(&key)
    ))
}

pub fn browser_url(cfg: &RelayCfg, secret: &str) -> String {
    format!("{}/#k={secret}", cfg.web_url())
}

/// One daemon connection per attached browser. The relay tags frames with
/// a client id (`cid`) and announces `client_open`/`client_close`; the
/// browser's handshake opens a daemon connection with its own [`Channel`],
/// so only browsers that are actually attached count as subscribers
/// (approvals fail closed and the daemon can idle-close when none are).
struct RelayClient {
    channel: Channel,
    /// Distinguishes this connection's output from an earlier one for
    /// the same `cid` that is still draining.
    generation: u64,
    write: Option<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
}

type Outbound = (String, u64, String);

impl RelayClient {
    /// Start a daemon connection; its output lines go to `out`.
    fn open(
        daemon: &Arc<Daemon>,
        cid: String,
        generation: u64,
        channel: Channel,
        out: mpsc::UnboundedSender<Outbound>,
    ) -> Self {
        let (relay_side, daemon_side) = tokio::io::duplex(64 * 1024);
        tokio::spawn(daemon.clone().handle_conn(daemon_side));
        let (read_half, write) = tokio::io::split(relay_side);
        tokio::spawn(async move {
            let mut lines = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if out.send((cid.clone(), generation, line)).is_err() {
                    break;
                }
            }
        });
        Self {
            channel,
            generation,
            write: Some(write),
        }
    }

    async fn send_line(&mut self, line: &str) -> bool {
        let Some(write) = self.write.as_mut() else {
            return false;
        };
        write.write_all(line.as_bytes()).await.is_ok() && write.write_all(b"\n").await.is_ok()
    }
}

impl Drop for RelayClient {
    /// The read half lives in the pump task, so the duplex stays open
    /// until its write side is shut down; that EOF ends `handle_conn`.
    fn drop(&mut self) {
        if let Some(mut write) = self.write.take() {
            tokio::spawn(async move {
                let _ = write.shutdown().await;
            });
        }
    }
}

/// A relay control or routed frame (`?mux=1` protocol).
#[derive(serde::Deserialize)]
struct MuxFrame {
    #[serde(default)]
    relay: Option<String>,
    cid: String,
    #[serde(default)]
    f: Option<String>,
}

/// One relay connection. Each browser handshakes, then its encrypted
/// protocol lines are fed to its own daemon connection over an in-process
/// duplex stream, and everything the daemon sends back is encrypted on
/// that browser's channel and addressed to it. Dropping a [`RelayClient`]
/// closes its stream, which ends `handle_conn` and unsubscribes it.
pub async fn connect_once(
    daemon: Arc<Daemon>,
    base_url: &str,
    secret: &str,
    shutdown: &CancellationToken,
) -> Result<()> {
    // The workspace enables both ring and aws-lc-rs via other deps, so
    // rustls cannot pick a process provider itself (it panics on wss://).
    static CRYPTO: std::sync::Once = std::sync::Once::new();
    CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let key = key_from_secret(secret)?;
    let url = relay_ws_url(base_url, secret)?;
    let (ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("connect relay {base_url}"))?;
    let (mut ws_write, mut ws_read) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Outbound>();
    let mut clients: HashMap<String, RelayClient> = HashMap::new();
    let mut generation = 0u64;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            msg = ws_read.next() => {
                let frame = match msg {
                    Some(Ok(Message::Text(frame))) => frame,
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = ws_write.send(Message::Pong(payload)).await;
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(anyhow!("relay websocket: {e}")),
                };
                let Ok(MuxFrame { relay, cid, f }) = serde_json::from_str::<MuxFrame>(&frame) else {
                    tracing::warn!("relay: dropping frame without a client id (relay too old?)");
                    continue;
                };
                if relay.as_deref() == Some("client_close") {
                    clients.remove(&cid);
                    continue;
                }
                let Some(f) = f else { continue };
                if let Some(browser_hello) = parse_hello(&f) {
                    // A (re)handshake starts a fresh connection and key.
                    let daemon_hello = new_hello();
                    let channel = Channel::new(&key, &browser_hello, &daemon_hello, Side::Daemon);
                    generation += 1;
                    clients.insert(
                        cid.clone(),
                        RelayClient::open(&daemon, cid.clone(), generation, channel, out_tx.clone()),
                    );
                    let reply = json!({"cid": cid, "f": hello_frame(&daemon_hello)}).to_string();
                    if ws_write.send(Message::Text(reply)).await.is_err() {
                        return Ok(());
                    }
                    continue;
                }
                // Frames before a handshake, or for a closed browser, are dropped.
                let Some(client) = clients.get_mut(&cid) else { continue };
                match client.channel.open(&f) {
                    Ok(line) => {
                        if !client.send_line(&line).await {
                            clients.remove(&cid);
                        }
                    }
                    Err(e) => tracing::warn!("relay: dropping frame: {e}"),
                }
            }
            Some((cid, generation, line)) = out_rx.recv() => {
                let Some(client) = clients.get_mut(&cid) else { continue };
                if client.generation != generation {
                    continue;
                }
                let sealed = client.channel.seal(&line)?;
                let frame = json!({"cid": cid, "f": sealed}).to_string();
                if ws_write.send(Message::Text(frame)).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

/// Reconnect loop for `Daemon::serve`; runs only when relay is enabled
/// and a pairing secret exists.
pub async fn relay_loop(daemon: Arc<Daemon>) {
    let shutdown = daemon.shutdown.clone();
    let mut backoff_secs = 1u64;
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let cfg = match daemon.config() {
            Ok(c) if c.relay.enabled => c.relay,
            Ok(_) => return,
            Err(_) => return,
        };
        let Some(secret) = load_relay_secret(&daemon.paths) else {
            tracing::warn!("relay enabled but no pairing secret; run `formless relay enable`");
            return;
        };
        match connect_once(daemon.clone(), &cfg.url, &secret, &shutdown).await {
            Ok(()) => backoff_secs = 1,
            Err(e) => tracing::warn!("relay connection ended: {e}; retrying in {backoff_secs}s"),
        }
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
        }
        backoff_secs = (backoff_secs * 2).min(30);
    }
}
