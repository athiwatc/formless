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
//! must be exactly one more than the last, so a captured frame cannot be
//! replayed (in this or any other session) or reflected back to its
//! sender, and a frame the relay withholds breaks the channel instead of
//! vanishing (#905).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use formless_protocol::RelayClientInfo;
use formless_runtime::LockExt as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
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
        "{}/relay/{}?role=daemon&mux=1",
        base.trim_end_matches('/'),
        room_id(&key)
    ))
}

/// The browser URL carrying the pairing secret in its fragment. Check the
/// relay address with [`RelayCfg::check`] before showing it.
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
    /// The handshake this channel came from, so a repeated hello for it
    /// gets the same answer instead of a new key (#904).
    browser_hello: Vec<u8>,
    reply: String,
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
            // Bounded per line, like every NDJSON reader (#888).
            let mut reader = BufReader::new(read_half);
            while let Ok(Some(line)) =
                crate::daemon::read_frame(&mut reader, formless_protocol::MAX_FRAME_BYTES).await
            {
                if out.send((cid.clone(), generation, line)).is_err() {
                    break;
                }
            }
        });
        Self {
            channel,
            browser_hello: Vec::new(),
            reply: String::new(),
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
    /// The browser's User-Agent, on `client_open`.
    #[serde(default)]
    ua: Option<String>,
}

/// Clears the relay's browser list and kick channel when a link ends.
struct Forget(Arc<Daemon>);

impl Drop for Forget {
    fn drop(&mut self) {
        self.0.relay_clients.locked().clear();
        *self.0.relay_kick.locked() = None;
    }
}

/// A browser that has sent nothing (its heartbeat included) for this long
/// is dropped, in case the relay never said it closed (#910).
const CLIENT_IDLE: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// How long an unconfirmed handshake is kept.
const HANDSHAKE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// A handshake waiting for the browser's key confirmation.
struct Pending {
    browser_hello: Vec<u8>,
    daemon_hello: Vec<u8>,
    reply: String,
    at: std::time::Instant,
}

/// Close code the relay passes to a browser whose channel broke (#905):
/// it reconnects and handshakes again, unlike 4001 (kicked).
pub const INTEGRITY_CLOSE: u16 = 4002;

/// Pushes for waiting approvals and questions are at least this far apart.
const PUSH_GAP: std::time::Duration = std::time::Duration::from_secs(30);

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
    // Approvals and questions become Web Pushes to the browsers that opted
    // in, at most one per PUSH_GAP (the relay holds the subscriptions).
    let mut attention = daemon.attention.subscribe();
    let mut last_push: Option<std::time::Instant> = None;
    // The browsers on this link (`ListRelayClients`) and requests to close
    // one; both forgotten when the link ends.
    let (kick_tx, mut kick_rx) = mpsc::unbounded_channel::<String>();
    *daemon.relay_kick.locked() = Some(kick_tx);
    let _forget = Forget(daemon.clone());
    let mut seen: HashMap<String, std::time::Instant> = HashMap::new();
    // Handshakes that haven't been confirmed yet, per browser: a browser
    // becomes a subscriber (and appears in the device list) only once it
    // has proved it holds the pairing key (#904).
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut agents: HashMap<String, String> = HashMap::new();
    let mut reap = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = reap.tick() => {
                let stale: Vec<String> = seen
                    .iter()
                    .filter(|(_, t)| t.elapsed() > CLIENT_IDLE)
                    .map(|(cid, _)| cid.clone())
                    .collect();
                pending.retain(|_, p| p.at.elapsed() < HANDSHAKE_TTL);
                for cid in stale {
                    tracing::info!("relay: dropping browser {cid}: silent for {}s", CLIENT_IDLE.as_secs());
                    seen.remove(&cid);
                    clients.remove(&cid);
                    daemon.relay_clients.locked().remove(&cid);
                    let frame = json!({"relay": "disconnect", "cid": cid, "code": INTEGRITY_CLOSE}).to_string();
                    if ws_write.send(Message::Text(frame)).await.is_err() {
                        return Ok(());
                    }
                }
            }
            Some(cid) = kick_rx.recv() => {
                clients.remove(&cid);
                daemon.relay_clients.locked().remove(&cid);
                let frame = json!({"relay": "disconnect", "cid": cid}).to_string();
                if ws_write.send(Message::Text(frame)).await.is_err() {
                    return Ok(());
                }
            }
            Ok(()) = attention.recv() => {
                if last_push.is_some_and(|t| t.elapsed() < PUSH_GAP) {
                    continue;
                }
                last_push = Some(std::time::Instant::now());
                if ws_write.send(Message::Text(r#"{"relay":"push"}"#.into())).await.is_err() {
                    return Ok(());
                }
            }
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
                let Ok(MuxFrame { relay, cid, f, ua }) = serde_json::from_str::<MuxFrame>(&frame) else {
                    tracing::warn!("relay: dropping frame without a client id (relay too old?)");
                    continue;
                };
                let now = chrono::Utc::now().to_rfc3339();
                match relay.as_deref() {
                    Some("client_close") => {
                        seen.remove(&cid);
                        pending.remove(&cid);
                        agents.remove(&cid);
                        clients.remove(&cid);
                        daemon.relay_clients.locked().remove(&cid);
                        continue;
                    }
                    Some("client_open") => {
                        // Listed once its handshake is confirmed.
                        agents.insert(cid.clone(), ua.unwrap_or_default().chars().take(200).collect());
                        continue;
                    }
                    _ => {}
                }
                if let Some(c) = daemon.relay_clients.locked().get_mut(&cid) {
                    c.last_seen = now.clone();
                }
                seen.insert(cid.clone(), std::time::Instant::now());
                let Some(f) = f else { continue };
                if let Some(browser_hello) = parse_hello(&f) {
                    // The same hello again (a resend, or the relay replaying
                    // it) gets the same answer: no new key on one side only.
                    let known = clients
                        .get(&cid)
                        .filter(|c| c.browser_hello == browser_hello)
                        .map(|c| c.reply.clone())
                        .or_else(|| {
                            pending.get(&cid).filter(|p| p.browser_hello == browser_hello).map(|p| p.reply.clone())
                        });
                    let reply = match known {
                        Some(reply) => reply,
                        None => {
                            // A new handshake waits for the browser's proof;
                            // the established channel stays until then.
                            let daemon_hello = new_hello();
                            let reply = json!({"cid": cid, "f": daemon_hello_frame(&key, &browser_hello, &daemon_hello)}).to_string();
                            pending.insert(
                                cid.clone(),
                                Pending { browser_hello, daemon_hello: daemon_hello.to_vec(), reply: reply.clone(), at: std::time::Instant::now() },
                            );
                            reply
                        }
                    };
                    if ws_write.send(Message::Text(reply)).await.is_err() {
                        return Ok(());
                    }
                    continue;
                }
                if let Some(tag) = parse_confirm(&f) {
                    let Some(p) = pending.get(&cid) else { continue };
                    let want = confirm_tag(&key, &p.browser_hello, &p.daemon_hello, Side::Browser);
                    if tag != want {
                        tracing::warn!("relay: browser {cid} failed key confirmation; ignoring its handshake");
                        pending.remove(&cid);
                        continue;
                    }
                    let p = pending.remove(&cid).expect("checked above");
                    let channel = Channel::new(&key, &p.browser_hello, &p.daemon_hello, Side::Daemon);
                    generation += 1;
                    let mut client = RelayClient::open(&daemon, cid.clone(), generation, channel, out_tx.clone());
                    client.browser_hello = p.browser_hello;
                    client.reply = p.reply;
                    clients.insert(cid.clone(), client);
                    let user_agent = agents.get(&cid).cloned().unwrap_or_default();
                    daemon.relay_clients.locked().entry(cid.clone()).or_insert(RelayClientInfo {
                        cid: cid.clone(),
                        user_agent,
                        first_seen: now.clone(),
                        last_seen: now,
                    });
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
                    // A frame that does not open (replayed, out of sequence,
                    // forged) breaks this browser's channel: close it so the
                    // browser handshakes again and reloads its state (#905).
                    Err(e) => {
                        tracing::warn!("relay: link integrity failure for browser {cid}: {e}; closing its channel");
                        clients.remove(&cid);
                        let frame = json!({"relay": "disconnect", "cid": cid, "code": INTEGRITY_CLOSE}).to_string();
                        if ws_write.send(Message::Text(frame)).await.is_err() {
                            return Ok(());
                        }
                    }
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
