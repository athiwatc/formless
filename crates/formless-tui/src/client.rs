use anyhow::Result;
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
        let stream = std::os::unix::net::UnixStream::connect(paths.socket_path()).map_err(|e| {
            anyhow::anyhow!(formless_core::daemon::explain_connect_error(paths, &e))
        })?;
        let reader = stream.try_clone()?;
        Ok((Box::new(reader), Box::new(stream)))
    }
    #[cfg(windows)]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.pipe_name())
            .map_err(|e| {
                anyhow::anyhow!(formless_core::daemon::explain_connect_error(paths, &e))
            })?;
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

/// Compare this client with the running daemon (SPEC §9.1). `Ok(None)`
/// when they match, `Ok(Some(warning))` when the versions differ but the
/// protocol is the same, and an error (with the fix) when the daemon
/// cannot be talked to at all.
pub fn check_daemon_version(paths: &Paths) -> Result<Option<String>> {
    let status = match request_once(paths, Request::Status) {
        Ok(v) => v,
        Err(e) if e.to_string().starts_with("protocol mismatch") => {
            anyhow::bail!(
                "the running daemon speaks a different protocol than this formless \
                 ({e}), so they cannot work together. Run `formless daemon restart` \
                 to replace it with this version."
            )
        }
        Err(e) => return Err(e),
    };
    let warnings = status_warnings(&status);
    Ok((!warnings.is_empty()).then(|| warnings.join(" ")))
}

/// Everything about the running daemon that differs from what this
/// client expects: another version, an older build of the same version
/// (a rebuild since it started), or approvals off when this client asked
/// for them (`--secure` / `FORMLESS_SECURE`) (SPEC §9.3, §10.3).
pub fn status_warnings(status: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let ours = env!("CARGO_PKG_VERSION");
    if let Some(w) = version_warning(ours, status["version"].as_str()) {
        out.push(w);
    } else if let Some(w) = build_warning(&formless_core::daemon::build_id(), status) {
        out.push(w);
    }
    let wants_secure = formless_core::config::env_flag("FORMLESS_SECURE")
        .ok()
        .flatten()
        == Some(true);
    if wants_secure && status["secure"] == false {
        out.push(
            "approvals are off in the running daemon, though you asked for them (--secure / \
             FORMLESS_SECURE). Run `formless --secure daemon restart`, or set security.secure = \
             true in config.toml."
                .into(),
        );
    }
    out
}

/// The warning when the daemon runs an older build of this version: it
/// says so itself (`stale`), or its build differs from this binary's.
pub fn build_warning(ours: &str, status: &serde_json::Value) -> Option<String> {
    let theirs = status["build"].as_str().unwrap_or_default();
    let differs = !ours.is_empty() && !theirs.is_empty() && ours != theirs;
    (status["stale"] == true || differs).then(|| {
        "formless was rebuilt since the daemon started, so it still runs the old code. Run \
         `formless daemon restart` to switch to the new build."
            .to_string()
    })
}

/// The mismatch warning, if the daemon runs another version (an older
/// daemon does not report one).
pub fn version_warning(ours: &str, daemon: Option<&str>) -> Option<String> {
    if daemon == Some(ours) {
        return None;
    }
    let daemon = daemon.map_or("an older version".to_string(), |v| format!("v{v}"));
    Some(format!(
        "the daemon is {daemon} but this formless is v{ours} (left running from before an \
         upgrade?). Run `formless daemon restart` to switch to v{ours}."
    ))
}

/// `formless daemon restart`: wait for (or with `force` interrupt) the
/// running Runs, shut the daemon down, wait until the old process is gone,
/// and start a fresh one. `say` reports progress.
pub fn restart_daemon(paths: &Paths, force: bool, mut say: impl FnMut(&str)) -> Result<()> {
    use formless_core::daemon::{daemon_reachable, ensure_daemon};
    use std::time::{Duration, Instant};
    if daemon_reachable(paths) {
        match request_once(paths, Request::Status) {
            Ok(status) => {
                let running = |paths: &Paths| -> Vec<String> {
                    request_once(paths, Request::Status)
                        .ok()
                        .and_then(|v| serde_json::from_value(v["running_sessions"].clone()).ok())
                        .unwrap_or_default()
                };
                let mut sessions: Vec<String> =
                    serde_json::from_value(status["running_sessions"].clone()).unwrap_or_default();
                if !sessions.is_empty() {
                    if force {
                        say(&format!("interrupting {} running Run(s)…", sessions.len()));
                        for id in &sessions {
                            let _ = request_once(
                                paths,
                                Request::InterruptRun {
                                    session_id: id.clone(),
                                },
                            );
                        }
                    } else {
                        say(&format!(
                            "waiting for {} running Run(s) to finish… (--force interrupts them)",
                            sessions.len()
                        ));
                    }
                    let deadline = Instant::now() + Duration::from_secs(15);
                    while !sessions.is_empty() {
                        if force && Instant::now() > deadline {
                            anyhow::bail!(
                                "Runs did not stop within 15s; the daemon was left running"
                            );
                        }
                        std::thread::sleep(Duration::from_millis(200));
                        sessions = running(paths);
                    }
                }
                let _ = request_once(paths, Request::Shutdown);
            }
            // A daemon on another protocol cannot be asked to stop: signal it.
            Err(e) if e.to_string().starts_with("protocol mismatch") => terminate_daemon(paths)?,
            Err(e) => return Err(e),
        }
        // Gone means not accepting and done flushing (it removes its lock last).
        let deadline = Instant::now() + Duration::from_secs(30);
        while daemon_reachable(paths) || paths.lock_file().exists() {
            if Instant::now() > deadline {
                anyhow::bail!("the old daemon did not exit within 30s; see `formless doctor`");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        say("daemon stopped");
    }
    ensure_daemon(paths)?;
    say("daemon running");
    Ok(())
}

/// SIGTERM the daemon named in the lock file, then drop its lock and
/// socket (it may not get to clean up after itself).
fn terminate_daemon(paths: &Paths) -> Result<()> {
    let pid = std::fs::read_to_string(paths.lock_file())
        .ok()
        .and_then(|p| p.trim().parse::<u32>().ok())
        .ok_or_else(|| anyhow::anyhow!("no daemon pid in {}", paths.lock_file().display()))?;
    #[cfg(unix)]
    let status = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status()?;
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()?;
    if !status.success() {
        anyhow::bail!("could not stop the daemon (pid {pid})");
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = std::fs::remove_file(paths.lock_file());
    #[cfg(unix)]
    let _ = std::fs::remove_file(paths.socket_path());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::version_warning;

    #[test]
    fn version_warning_only_on_a_mismatch() {
        assert_eq!(version_warning("0.2.0", Some("0.2.0")), None);
        let w = version_warning("0.2.0", Some("0.1.0")).unwrap();
        assert!(
            w.starts_with("the daemon is v0.1.0 but this formless is v0.2.0"),
            "{w}"
        );
        assert!(w.contains("formless daemon restart"));
        let w = version_warning("0.2.0", None).unwrap();
        assert!(w.starts_with("the daemon is an older version"), "{w}");
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_stale_or_different_build_is_reported() {
        let ok = json!({"version": env!("CARGO_PKG_VERSION"), "build": "a", "stale": false});
        assert!(build_warning("a", &ok).is_none());
        assert!(build_warning("b", &ok).is_some(), "rebuilt client");
        let stale = json!({"build": "a", "stale": true});
        assert!(build_warning("a", &stale).is_some(), "the daemon says so");
        assert!(build_warning("", &json!({})).is_none(), "unknown: quiet");
    }
}
