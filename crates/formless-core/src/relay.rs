//! Hosted relay client (SPEC §17 v2): the daemon dials out to the relay
//! (`wss://formless.athiwat.xyz` by default) and browsers meet it there.
//! The relay only forwards opaque AES-256-GCM ciphertext — the pairing
//! secret (32 random bytes, base64url) *is* the key, and the room id is
//! a truncated SHA-256 of the key, so the relay cannot read traffic or
//! derive the key from the room it routes.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::config::{load_relay_secret, RelayCfg};
use crate::daemon::Daemon;
use crate::Config;

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

pub fn encrypt_line(key: &[u8; 32], plaintext: &str) -> Result<String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut iv = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut iv);
    let ct = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_bytes())
        .map_err(|_| anyhow!("relay encryption failed"))?;
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok(json!({"v": 1, "iv": b64.encode(iv), "ct": b64.encode(ct)}).to_string())
}

pub fn decrypt_line(key: &[u8; 32], frame: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(frame).context("relay frame is not JSON")?;
    if v["v"].as_u64() != Some(1) {
        return Err(anyhow!("unsupported relay frame version"));
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let iv = b64
        .decode(v["iv"].as_str().context("relay frame missing iv")?)
        .context("bad relay iv")?;
    let ct = b64
        .decode(v["ct"].as_str().context("relay frame missing ct")?)
        .context("bad relay ciphertext")?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let pt = cipher
        .decrypt(Nonce::from_slice(&iv), ct.as_ref())
        .map_err(|_| anyhow!("relay decryption failed (wrong pairing secret?)"))?;
    String::from_utf8(pt).context("relay plaintext is not UTF-8")
}

pub fn relay_ws_url(base: &str, secret: &str) -> Result<String> {
    let key = key_from_secret(secret)?;
    Ok(format!(
        "{}/relay/{}?role=daemon",
        base.trim_end_matches('/'),
        room_id(&key)
    ))
}

pub fn browser_url(cfg: &RelayCfg, secret: &str) -> String {
    format!("{}/#k={secret}", cfg.web_url())
}

/// One relay connection: browser protocol lines arrive encrypted, are
/// fed to a normal daemon connection over an in-process duplex stream,
/// and everything the daemon sends back is encrypted to the relay.
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
    let (relay_side, daemon_side) = tokio::io::duplex(64 * 1024);
    let conn = {
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(daemon_side).await })
    };
    let (read_half, mut write_half) = tokio::io::split(relay_side);
    let mut lines = BufReader::new(read_half).lines();
    let result = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break Ok(()),
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(Message::Text(frame))) => {
                        match decrypt_line(&key, &frame) {
                            Ok(line) => {
                                if write_half.write_all(line.as_bytes()).await.is_err()
                                    || write_half.write_all(b"\n").await.is_err() {
                                    break Ok(());
                                }
                            }
                            Err(e) => tracing::warn!("relay: dropping undecryptable frame: {e}"),
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = ws_write.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => break Err(anyhow!("relay websocket: {e}")),
                }
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let frame = encrypt_line(&key, &line)?;
                        if ws_write.send(Message::Text(frame)).await.is_err() {
                            break Ok(());
                        }
                    }
                    _ => break Ok(()),
                }
            }
        }
    };
    conn.abort();
    result
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
        let cfg = match Config::load(&daemon.paths) {
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
