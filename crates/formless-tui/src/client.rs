use anyhow::{Context, Result};
use formless_protocol::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use formless_core::Paths;

pub struct DaemonClient {
    write_tx: mpsc::Sender<String>,
    next_id: AtomicU64,
}

fn connect_stream(paths: &Paths) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    #[cfg(unix)]
    {
        let stream = std::os::unix::net::UnixStream::connect(paths.socket_path())
            .context("connect daemon socket")?;
        let reader = stream.try_clone()?;
        Ok((Box::new(reader), Box::new(stream)))
    }
    #[cfg(windows)]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.pipe_name())
            .context("connect daemon pipe")?;
        let reader = file.try_clone()?;
        Ok((Box::new(reader), Box::new(file)))
    }
}

impl DaemonClient {
    pub fn connect(paths: &Paths) -> Result<(Self, mpsc::Receiver<ServerMessage>)> {
        let (reader, mut writer) = connect_stream(paths)?;
        let (msg_tx, msg_rx) = mpsc::channel::<ServerMessage>();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(reader).lines();
            while let Some(Ok(line)) = lines.next() {
                if let Ok(msg) = serde_json::from_str::<ServerMessage>(&line) {
                    if msg_tx.send(msg).is_err() {
                        break;
                    }
                }
            }
        });
        let (write_tx, write_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            while let Ok(line) = write_rx.recv() {
                if writer.write_all(line.as_bytes()).is_err() {
                    break;
                }
                if writer.write_all(b"\n").is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });
        Ok((
            Self {
                write_tx,
                next_id: AtomicU64::new(1),
            },
            msg_rx,
        ))
    }

    /// Fire-and-forget; responses arrive on the shared channel with this id.
    pub fn send(&self, request: Request) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let env = RequestEnvelope {
            id,
            protocol: PROTOCOL_VERSION,
            request,
        };
        if let Ok(line) = serde_json::to_string(&env) {
            let _ = self.write_tx.send(line);
        }
        id
    }
}

/// One-shot request against the daemon (CLI/doctor use). Blocks for the
/// matching response, discarding interleaved events.
pub fn request_once(paths: &Paths, request: Request) -> Result<serde_json::Value> {
    let (client, rx) = DaemonClient::connect(paths)?;
    let id = client.send(request);
    loop {
        match rx.recv() {
            Ok(ServerMessage::Response {
                id: rid,
                ok,
                result,
                error,
            }) if rid == id => {
                return if ok {
                    Ok(result.unwrap_or(serde_json::Value::Null))
                } else {
                    Err(anyhow::anyhow!(
                        error.unwrap_or_else(|| "daemon error".into())
                    ))
                };
            }
            Ok(_) => continue,
            Err(e) => return Err(anyhow::anyhow!("daemon closed: {e}")),
        }
    }
}
