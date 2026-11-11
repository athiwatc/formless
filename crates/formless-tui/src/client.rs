use anyhow::Result;
use formless_protocol::*;
#[cfg(any(unix, test))]
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
#[cfg(any(windows, test))]
use std::time::Duration;
use std::time::Instant;

use formless_core::Paths;

#[cfg(any(windows, test))]
mod windows_duplex;

pub struct DaemonClient {
    write_tx: Outbox,
    next_id: AtomicU64,
}

/// Where [`DaemonClient::send`] puts a line. Both queues are unbounded and
/// non-blocking; a line sent after the transport ended is dropped.
enum Outbox {
    /// Unix threads and [`DaemonClient::recording`].
    Thread(mpsc::Sender<String>),
    /// Windows worker. Dropping it stops the worker before any further
    /// queued line is taken; see `windows_duplex` for what is retained.
    #[cfg(windows)]
    Duplex(windows_duplex::Duplex),
}

impl Outbox {
    fn send(&self, line: String) -> std::result::Result<(), String> {
        match self {
            Outbox::Thread(sender) => sender.send(line).map_err(|error| error.0),
            #[cfg(windows)]
            Outbox::Duplex(duplex) => duplex.enqueue(line),
        }
    }
}

/// User sends whose daemon acknowledgement has not arrived. Transport request
/// ids can change on retry/reconnect; the action UUID and original mode cannot.
#[derive(Clone)]
struct SendAction {
    session_id: String,
    content: String,
    mode: SendMode,
    key: String,
}

#[derive(Default)]
pub struct SendTracker {
    actions: Vec<SendAction>,
    requests: std::collections::HashMap<u64, String>,
}

impl SendTracker {
    pub fn send(
        &mut self,
        client: &DaemonClient,
        session_id: String,
        content: String,
        mode: SendMode,
    ) -> u64 {
        let action = match self
            .actions
            .iter()
            .find(|a| a.session_id == session_id && a.content == content)
        {
            Some(action) => action.clone(),
            None => {
                let action = SendAction {
                    session_id,
                    content,
                    mode,
                    key: uuid::Uuid::new_v4().to_string(),
                };
                self.actions.push(action.clone());
                action
            }
        };
        let id = client.send(Request::SendMessage {
            session_id: action.session_id,
            content: action.content,
            mode: action.mode,
            client_msg_id: Some(action.key.clone()),
        });
        self.requests.insert(id, action.key);
        id
    }

    /// Any answer (including a definite rejection) ends this send action.
    pub fn answered(&mut self, id: u64) -> Vec<u64> {
        if let Some(key) = self.requests.remove(&id) {
            let mut answered = vec![id];
            self.actions.retain(|a| a.key != key);
            self.requests.retain(|request, action| {
                if action == &key {
                    answered.push(*request);
                    false
                } else {
                    true
                }
            });
            answered
        } else {
            Vec::new()
        }
    }

    pub fn unsent(&self, session_id: &str) -> Option<&str> {
        self.actions
            .iter()
            .rev()
            .find(|a| a.session_id == session_id)
            .map(|a| a.content.as_str())
    }

    pub fn message(&self, request: u64) -> Option<(&str, &str)> {
        let key = self.requests.get(&request)?;
        self.actions
            .iter()
            .find(|a| &a.key == key)
            .map(|a| (a.session_id.as_str(), a.content.as_str()))
    }

    pub fn disconnected(&mut self) {
        self.requests.clear();
    }
}

/// Windows recreates the listening pipe after accept, so an immediate
/// second open can return raw `ERROR_PIPE_BUSY` (231). This short retry
/// budget is a UX bound, not a guarantee about the server recreation time.
#[cfg(any(windows, test))]
const PIPE_BUSY_BUDGET: Duration = Duration::from_millis(150);

/// Longest sleep between busy opens. A shorter remainder is slept instead.
#[cfg(any(windows, test))]
const PIPE_BUSY_SLICE: Duration = Duration::from_millis(10);

/// `ERROR_PIPE_BUSY`. Matched as a raw OS code so no other error is retried.
#[cfg(any(windows, test))]
const RAW_ERROR_PIPE_BUSY: i32 = 231;

/// Bounds when a new open attempt may start, not an already executing OS open.
#[cfg(any(windows, test))]
fn connect_deadline(total: Instant, now: Instant, busy_budget: Duration) -> Instant {
    total.min(now + busy_budget)
}

/// Synchronous retry for raw `ERROR_PIPE_BUSY` (231) only.
///
/// The deadline is checked before every attempt. An open or a sleep that
/// has already started runs to completion; this function has no cancel
/// flag. A deadline that is already due performs no open. A busy failure
/// with no time left returns that same 231 error. Any other error is
/// returned on the attempt that produced it.
#[cfg(any(windows, test))]
fn open_retrying_pipe_busy<T>(
    deadline: Instant,
    mut open: impl FnMut() -> std::io::Result<T>,
    now: impl Fn() -> Instant,
    mut wait: impl FnMut(Duration),
) -> std::io::Result<T> {
    let mut last_busy: Option<std::io::Error> = None;
    loop {
        if now() >= deadline {
            return Err(match last_busy {
                Some(error) => error,
                None => std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "named pipe open deadline expired before an attempt",
                ),
            });
        }
        match open() {
            Ok(stream) => return Ok(stream),
            Err(error) if error.raw_os_error() == Some(RAW_ERROR_PIPE_BUSY) => {
                let left = deadline.saturating_duration_since(now());
                if left.is_zero() {
                    return Err(error);
                }
                last_busy = Some(error);
                wait(left.min(PIPE_BUSY_SLICE));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn connect_stream_before(
    paths: &Paths,
    total: Option<Instant>,
) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    // One blocking Unix connect. It is not interrupted at `total`, so
    // this call is not a hard wall-clock slice of the request deadline.
    // The caller drops the streams before spawning threads when `total`
    // has already passed, and enqueues the request at most once.
    let _ = total;
    let stream = std::os::unix::net::UnixStream::connect(paths.socket_path())
        .map_err(|e| anyhow::anyhow!(formless_core::daemon::explain_connect_error(paths, &e)))?;
    let reader = stream.try_clone()?;
    Ok((Box::new(reader), Box::new(stream)))
}

/// Windows client on a worker-owned overlapped pipe (issue 1789). Starting
/// open attempts, the raw-231 retry and the handshake all end at the
/// shorter of `total` and [`PIPE_BUSY_BUDGET`] from now.
#[cfg(windows)]
fn connect_windows(
    paths: &Paths,
    total: Option<Instant>,
) -> Result<(DaemonClient, mpsc::Receiver<ServerMessage>)> {
    let now = Instant::now();
    let deadline = match total {
        Some(total) => connect_deadline(total, now, PIPE_BUSY_BUDGET),
        None => now + PIPE_BUSY_BUDGET,
    };
    let (duplex, events) = windows_duplex::connect(paths.pipe_name(), deadline)
        .map_err(|e| anyhow::anyhow!(formless_core::daemon::explain_connect_error(paths, &e)))?;
    Ok((
        DaemonClient {
            write_tx: Outbox::Duplex(duplex),
            next_id: AtomicU64::new(1),
        },
        events,
    ))
}

/// Counts `spawn_client` calls on this thread. Parallel tests do not share it.
#[cfg(test)]
thread_local! {
    static CLIENT_SPAWN_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(any(unix, test))]
struct StreamsExpired;

#[cfg(any(unix, test))]
fn spawn_client(
    reader: Box<dyn Read + Send>,
    mut writer: Box<dyn Write + Send>,
) -> (DaemonClient, mpsc::Receiver<ServerMessage>) {
    #[cfg(test)]
    CLIENT_SPAWN_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    let (msg_tx, msg_rx) = mpsc::channel::<ServerMessage>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        // Bounded per line (#888); an oversized one ends the connection.
        while let Ok(Some(line)) = read_line_capped(&mut reader, MAX_FRAME_BYTES) {
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
    (
        DaemonClient {
            write_tx: Outbox::Thread(write_tx),
            next_id: AtomicU64::new(1),
        },
        msg_rx,
    )
}

/// Drop raw streams that outlived `deadline` before any client thread exists.
#[cfg(any(unix, test))]
fn adopt_streams(
    streams: (Box<dyn Read + Send>, Box<dyn Write + Send>),
    deadline: Option<Instant>,
) -> std::result::Result<(DaemonClient, mpsc::Receiver<ServerMessage>), StreamsExpired> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        drop(streams);
        return Err(StreamsExpired);
    }
    Ok(spawn_client(streams.0, streams.1))
}

impl DaemonClient {
    #[cfg(test)]
    pub(crate) fn recording() -> (Self, mpsc::Receiver<String>) {
        let (write_tx, rx) = mpsc::channel();
        (
            Self {
                write_tx: Outbox::Thread(write_tx),
                next_id: AtomicU64::new(1),
            },
            rx,
        )
    }

    pub fn connect(paths: &Paths) -> Result<(Self, mpsc::Receiver<ServerMessage>)> {
        #[cfg(unix)]
        {
            let streams = connect_stream_before(paths, None)?;
            Ok(spawn_client(streams.0, streams.1))
        }
        #[cfg(windows)]
        {
            connect_windows(paths, None)
        }
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

/// A failed daemon response: its text, and its machine-readable code
/// when it has one (`formless_protocol::codes`, #885).
#[derive(Debug)]
pub struct DaemonError {
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonError {}

/// Whether `e` is a failed response with `code` (or, from a daemon that
/// predates codes, text starting with `fallback`).
pub fn has_code(e: &anyhow::Error, code: &str, fallback: &str) -> bool {
    match e.downcast_ref::<DaemonError>() {
        Some(d) => d.code.as_deref() == Some(code) || d.message.starts_with(fallback),
        None => e.to_string().starts_with(fallback),
    }
}

/// How long [`request_once`] waits for its answer: long enough for an
/// export or a smoke-tested save, never forever (#884). Counted from
/// before connect, then checked again before enqueue and before each
/// receive. Not a hard stop for a blocking Unix connect.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// One-shot request against the daemon (CLI/doctor use). Blocks for the
/// matching response, discarding interleaved events, up to
/// [`REQUEST_TIMEOUT`]. The deadline is taken before connect; see
/// [`request_once_within`].
pub fn request_once(paths: &Paths, request: Request) -> Result<serde_json::Value> {
    request_once_within(paths, request, REQUEST_TIMEOUT)
}

/// [`request_once`] with its own deadline.
///
/// The deadline is recorded before connect. Owned raw streams that have
/// already expired are dropped before reader or writer threads are spawned.
/// Expiry is checked before thread startup; the request is then enqueued
/// at most once (no replay). Scheduling may pass the deadline between that
/// check and enqueue. This check is not a hard wall-clock bound
/// and does not promise that no bytes are written after the deadline: a
/// blocking connect or busy sleep runs to completion, and a writer thread
/// can still flush a line after the deadline.
///
/// Unix keeps one blocking `UnixStream::connect`, which is not interrupted.
/// The receive side uses the original deadline, so a slow connect leaves
/// less time to wait. Windows retries only raw `ERROR_PIPE_BUSY` (231)
/// until the shorter of 150ms and this deadline. Every other open error is
/// unchanged. On Windows the open runs inside a worker-owned runtime; the
/// worker keeps the pipe, and a handshake that misses that shorter deadline
/// is discarded, not used. A Windows write already handed to the OS is not
/// canceled when the client is dropped. The receive loop checks the deadline
/// before each `recv_timeout`, including when an unrelated message is
/// already queued.
pub fn request_once_within(
    paths: &Paths,
    request: Request,
    timeout: std::time::Duration,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    if Instant::now() >= deadline {
        return Err(unanswered(timeout));
    }
    #[cfg(unix)]
    {
        let streams = connect_stream_before(paths, Some(deadline))?;
        let (client, rx) = match adopt_streams(streams, Some(deadline)) {
            Ok(ready) => ready,
            Err(StreamsExpired) => return Err(unanswered(timeout)),
        };
        // Adoption checks expiry before spawning threads. Do not add a second
        // abandonment path after spawn: the request is queued once, never replayed.
        let id = client.send(request);
        recv_response(&rx, id, deadline, timeout)
    }
    #[cfg(windows)]
    {
        let (client, rx) = connect_windows(paths, Some(deadline))?;
        // Same rule as Unix adoption: expired before enqueue means the
        // worker is stopped and nothing is sent. Enqueued at most once.
        if Instant::now() >= deadline {
            drop(client);
            return Err(unanswered(timeout));
        }
        let id = client.send(request);
        recv_response(&rx, id, deadline, timeout)
    }
}

fn recv_response(
    rx: &mpsc::Receiver<ServerMessage>,
    id: u64,
    deadline: Instant,
    timeout: std::time::Duration,
) -> Result<serde_json::Value> {
    loop {
        if Instant::now() >= deadline {
            return Err(unanswered(timeout));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(msg @ ServerMessage::Response { id: rid, .. }) if rid == id => {
                let code = msg.error_code().map(str::to_string);
                let ServerMessage::Response {
                    ok, result, error, ..
                } = msg
                else {
                    unreachable!("matched a response")
                };
                return if ok {
                    Ok(result.unwrap_or(serde_json::Value::Null))
                } else {
                    Err(DaemonError {
                        code,
                        message: error.unwrap_or_else(|| "daemon error".into()),
                    }
                    .into())
                };
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(unanswered(timeout)),
            Err(e) => return Err(anyhow::anyhow!("daemon closed: {e}")),
        }
    }
}

fn unanswered(timeout: std::time::Duration) -> anyhow::Error {
    anyhow::anyhow!("the daemon did not answer within {}s", timeout.as_secs())
}

/// Compare this client with the running daemon (SPEC §9.1). `Ok(None)`
/// when they match, `Ok(Some(warning))` when the versions differ but the
/// protocol is the same, and an error (with the fix) when the daemon
/// cannot be talked to at all.
pub fn check_daemon_version(paths: &Paths) -> Result<Option<String>> {
    let status = match request_once(paths, Request::Status) {
        Ok(v) => v,
        Err(e) if has_code(&e, codes::PROTOCOL_MISMATCH, "protocol mismatch") => {
            anyhow::bail!(
                "the running daemon speaks a different protocol than this formless \
                 ({e}), so they cannot work together. Run `formless daemon restart` \
                 to replace it with this version."
            )
        }
        Err(e) => return Err(e),
    };
    let warnings = status_warnings(&status)?;
    Ok((!warnings.is_empty()).then(|| warnings.join(" ")))
}

/// Everything about the running daemon that differs from what this
/// client expects: another version, an older build of the same version
/// (a rebuild since it started), or approvals off when this client asked
/// for them (`--secure` / `FORMLESS_SECURE`) (SPEC §9.3, §10.3).
///
/// An invalid `FORMLESS_SECURE` is an error here, never "didn't ask" (#846).
pub fn status_warnings(status: &serde_json::Value) -> Result<Vec<String>> {
    let wants = formless_core::config::env_flag("FORMLESS_SECURE")?;
    Ok(status_warnings_for(status, wants))
}

/// [`status_warnings`] with what this client asked for passed in.
pub fn status_warnings_for(status: &serde_json::Value, wants_secure: Option<bool>) -> Vec<String> {
    let mut out = Vec::new();
    let ours = env!("CARGO_PKG_VERSION");
    if let Some(w) = version_warning(ours, status["version"].as_str()) {
        out.push(w);
    } else if let Some(w) = build_warning(&formless_core::daemon::build_id(), status) {
        out.push(w);
    }
    if insecure_daemon(status, wants_secure) {
        out.push(INSECURE_DAEMON.into());
    }
    out
}

const INSECURE_DAEMON: &str = "approvals are off in the running daemon, though you asked for them \
    (--secure / FORMLESS_SECURE). Run `formless --secure daemon restart`, or set \
    security.secure = true in config.toml.";

/// The client asked for approvals and the daemon doesn't enforce them.
pub fn insecure_daemon(status: &serde_json::Value, wants_secure: Option<bool>) -> bool {
    wants_secure == Some(true) && status["secure"] == false
}

/// For commands that run Tools without a person watching (`run`, `tools
/// run`): fail before sending anything when this client asked for
/// approvals and the daemon doesn't enforce them (#846).
pub fn require_secure_daemon(paths: &Paths) -> Result<()> {
    if formless_core::config::env_flag("FORMLESS_SECURE")? != Some(true) {
        return Ok(());
    }
    let status = request_once(paths, Request::Status)?;
    if insecure_daemon(&status, Some(true)) {
        anyhow::bail!("{INSECURE_DAEMON} Nothing was run.");
    }
    Ok(())
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
            // Another protocol cannot authenticate shutdown through this client.
            Err(e) if has_code(&e, codes::PROTOCOL_MISMATCH, "protocol mismatch") => {
                terminate_daemon(paths)?
            }
            Err(e) => return Err(e),
        }
        // Gone means not accepting and all ownership-bearing state has dropped.
        let deadline = Instant::now() + Duration::from_secs(30);
        while daemon_reachable(paths) || formless_core::profile_lock::owner_held(paths)? {
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

/// PID markers can be stale or name a reused PID. Without an authenticated
/// protocol shutdown, refusing is safer than signaling an unrelated process.
fn terminate_daemon(paths: &Paths) -> Result<()> {
    anyhow::bail!("cannot authenticate the older daemon for shutdown; stop it through its service manager or original binary before retrying (PID marker: {})", paths.lock_file().display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_actions_keep_keys_until_answered_and_scope_them_to_the_session() {
        let (client, sent) = DaemonClient::recording();
        let mut sends = SendTracker::default();
        let mut send = |session: &str, mode| {
            let id = sends.send(&client, session.into(), "same text".into(), mode);
            let envelope: RequestEnvelope =
                serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
            (id, envelope.request)
        };
        let (first, original) = send("s1", SendMode::Send);
        let (retry_id, retry) = send("s1", SendMode::Steer);
        assert_eq!(
            serde_json::to_value(&original).unwrap(),
            serde_json::to_value(&retry).unwrap()
        );
        let (_, other_session) = send("s2", SendMode::Send);
        let key = |request: &Request| match request {
            Request::SendMessage { client_msg_id, .. } => client_msg_id.clone().unwrap(),
            _ => panic!("not a send"),
        };
        assert_ne!(key(&original), key(&other_session));
        let cleared = sends.answered(first);
        assert!(cleared.contains(&first) && cleared.contains(&retry_id));
        sends.send(&client, "s1".into(), "same text".into(), SendMode::Send);
        let fresh: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        assert_ne!(key(&original), key(&fresh.request));
        // Reconnecting resets transport ids, but preserves the user action.
        sends.disconnected();
        let (replacement, sent) = DaemonClient::recording();
        let id = sends.send(
            &replacement,
            "s1".into(),
            "same text".into(),
            SendMode::Steer,
        );
        let resent: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        assert_eq!(key(&fresh.request), key(&resent.request));
        sends.answered(id);
        assert_eq!(sends.unsent("s1"), None);
        assert_eq!(sends.unsent("s2"), Some("same text"));
    }

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
    fn asking_for_approvals_from_a_daemon_without_them_warns() {
        let off = json!({"version": env!("CARGO_PKG_VERSION"), "secure": false});
        let on = json!({"version": env!("CARGO_PKG_VERSION"), "secure": true});
        let w = status_warnings_for(&off, Some(true));
        assert!(w.iter().any(|w| w.contains("approvals are off")), "{w:?}");
        assert!(status_warnings_for(&off, None)
            .iter()
            .all(|w| !w.contains("approvals")));
        assert!(status_warnings_for(&on, Some(true))
            .iter()
            .all(|w| !w.contains("approvals")));
        assert!(insecure_daemon(&off, Some(true)) && !insecure_daemon(&on, Some(true)));
    }

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

#[cfg(test)]
mod request_tests {
    use super::*;

    /// A daemon that never answers can't hang a caller (#884), and errors
    /// are told apart by code, not wording (#885).
    #[cfg(unix)]
    #[test]
    fn an_unanswered_request_times_out_and_codes_beat_wording() {
        let dir = std::env::temp_dir().join(format!("fl-rq-{}", std::process::id()));
        let paths = Paths {
            data_dir: dir.join("d"),
            config_dir: dir.join("c"),
            profile: "rq".into(),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(paths.socket_path()).unwrap();
        // Accepts, reads, never replies.
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(1).flatten().collect();
            std::thread::sleep(std::time::Duration::from_secs(5));
            drop(held);
        });
        let started = std::time::Instant::now();
        let e = request_once_within(&paths, Request::Ping, std::time::Duration::from_millis(300))
            .unwrap_err();
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(dir);

        let reworded: anyhow::Error = DaemonError {
            code: Some(codes::PROTOCOL_MISMATCH.into()),
            message: "versions differ".into(),
        }
        .into();
        assert!(has_code(
            &reworded,
            codes::PROTOCOL_MISMATCH,
            "protocol mismatch"
        ));
        let old_daemon: anyhow::Error = DaemonError {
            code: None,
            message: "protocol mismatch: client 2 daemon 1".into(),
        }
        .into();
        assert!(has_code(
            &old_daemon,
            codes::PROTOCOL_MISMATCH,
            "protocol mismatch"
        ));
    }
}

#[cfg(test)]
mod pipe_busy_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::io::ErrorKind;
    use std::io::{Read, Write};
    use std::time::Duration;

    struct Attempt<T> {
        result: std::io::Result<T>,
        waits: Vec<Duration>,
        opens: u32,
    }

    fn attempt<T>(budget: Duration, mut open: impl FnMut(u32) -> std::io::Result<T>) -> Attempt<T> {
        let start = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let waits = RefCell::new(Vec::new());
        let opens = Cell::new(0u32);
        let result = super::open_retrying_pipe_busy(
            start + budget,
            || {
                let n = opens.get();
                opens.set(n + 1);
                open(n)
            },
            || start + elapsed.get(),
            |delay| {
                assert!(delay > Duration::ZERO);
                assert!(delay <= super::PIPE_BUSY_SLICE);
                waits.borrow_mut().push(delay);
                elapsed.set(elapsed.get() + delay);
            },
        );
        assert!(elapsed.get() <= budget);
        Attempt {
            result,
            waits: waits.into_inner(),
            opens: opens.get(),
        }
    }

    fn busy() -> std::io::Error {
        std::io::Error::from_raw_os_error(super::RAW_ERROR_PIPE_BUSY)
    }

    #[test]
    fn busy_cap_is_one_hundred_fifty_milliseconds_sliced_at_ten() {
        assert_eq!(super::RAW_ERROR_PIPE_BUSY, 231);
        assert_eq!(super::PIPE_BUSY_BUDGET, Duration::from_millis(150));
        assert_eq!(super::PIPE_BUSY_SLICE, Duration::from_millis(10));
    }

    #[test]
    fn connect_deadline_is_the_shorter_bound() {
        let now = Instant::now();
        let shorter = super::PIPE_BUSY_BUDGET / 4;
        assert!(shorter < super::PIPE_BUSY_BUDGET);
        assert_eq!(
            super::connect_deadline(now + Duration::from_secs(300), now, super::PIPE_BUSY_BUDGET),
            now + super::PIPE_BUSY_BUDGET
        );
        assert_eq!(
            super::connect_deadline(now + shorter, now, super::PIPE_BUSY_BUDGET),
            now + shorter
        );
        assert_eq!(
            super::connect_deadline(now, now, super::PIPE_BUSY_BUDGET),
            now
        );
    }

    #[test]
    fn a_zero_budget_does_not_open() {
        let done = attempt(Duration::ZERO, |_| -> std::io::Result<()> {
            panic!("open at an already due deadline");
        });
        assert_eq!(done.opens, 0);
        assert!(done.waits.is_empty());
        let error = done.result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(error.raw_os_error(), None);
        assert!(error.to_string().contains("before an attempt"));
    }

    #[test]
    fn a_transient_231_opens_once_more_and_returns_that_stream() {
        let done = attempt(super::PIPE_BUSY_BUDGET, |n| match n {
            0 => Err(busy()),
            _ => Ok("stream"),
        });
        assert_eq!(done.opens, 2);
        assert_eq!(done.waits, vec![super::PIPE_BUSY_SLICE]);
        assert_eq!(done.result.unwrap(), "stream");
    }

    #[test]
    fn an_immediate_open_does_not_sleep() {
        let done = attempt(super::PIPE_BUSY_BUDGET, |_| Ok(()));
        assert_eq!(done.opens, 1);
        assert!(done.waits.is_empty());
        done.result.unwrap();
    }

    #[test]
    fn the_deadline_returns_the_last_231_without_another_open() {
        let budget = super::PIPE_BUSY_SLICE + super::PIPE_BUSY_SLICE / 2;
        let done = attempt(budget, |_| Err::<(), _>(busy()));
        assert_eq!(done.opens, 2);
        assert_eq!(
            done.waits,
            vec![super::PIPE_BUSY_SLICE, super::PIPE_BUSY_SLICE / 2]
        );
        let error = done.result.unwrap_err();
        assert_eq!(error.raw_os_error(), Some(231));
        assert!(!error.to_string().contains("before an attempt"));
    }

    #[test]
    fn a_busy_open_that_consumes_the_budget_returns_that_231() {
        let start = Instant::now();
        let budget = Duration::from_millis(5);
        let elapsed = Cell::new(Duration::ZERO);
        let opens = Cell::new(0u32);
        let waits = Cell::new(0u32);
        let result = super::open_retrying_pipe_busy(
            start + budget,
            || {
                opens.set(opens.get() + 1);
                elapsed.set(budget);
                Err::<(), _>(busy())
            },
            || start + elapsed.get(),
            |_| waits.set(waits.get() + 1),
        );
        assert_eq!(opens.get(), 1);
        assert_eq!(waits.get(), 0);
        let error = result.unwrap_err();
        assert_eq!(error.raw_os_error(), Some(231));
        assert!(!error.to_string().contains("before an attempt"));
    }

    #[test]
    fn other_errors_are_not_retried() {
        let raw = attempt(super::PIPE_BUSY_BUDGET, |_| {
            Err::<(), _>(std::io::Error::from_raw_os_error(5))
        });
        assert_eq!(raw.opens, 1);
        assert!(raw.waits.is_empty());
        assert_eq!(raw.result.unwrap_err().raw_os_error(), Some(5));

        let kind = attempt(super::PIPE_BUSY_BUDGET, |_| {
            Err::<(), _>(std::io::Error::new(ErrorKind::NotFound, "missing pipe"))
        });
        assert_eq!(kind.opens, 1);
        assert!(kind.waits.is_empty());
        let error = kind.result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound);
        assert_eq!(error.to_string(), "missing pipe");
    }

    #[test]
    fn a_non_busy_error_after_231_is_returned_unchanged() {
        let done: Attempt<()> = attempt(super::PIPE_BUSY_BUDGET, |n| {
            if n == 0 {
                Err(busy())
            } else {
                Err(std::io::Error::new(ErrorKind::PermissionDenied, "denied"))
            }
        });
        assert_eq!(done.opens, 2);
        assert_eq!(done.waits.len(), 1);
        let error = done.result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "denied");
        assert_eq!(error.raw_os_error(), None);
    }

    #[test]
    fn a_zero_timeout_does_not_open_or_send() {
        let paths = Paths {
            data_dir: std::env::temp_dir().join(format!("fl-zero-{}", std::process::id())),
            config_dir: std::env::temp_dir().join(format!("fl-zero-cfg-{}", std::process::id())),
            profile: "zero".into(),
        };
        let error = request_once_within(&paths, Request::Ping, Duration::ZERO).unwrap_err();
        assert!(
            error.to_string().contains("did not answer within 0s"),
            "{error}"
        );
    }

    #[test]
    fn expired_streams_from_a_fake_connector_drop_without_spawn() {
        struct Counted {
            drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Drop for Counted {
            fn drop(&mut self) {
                self.drops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        impl Read for Counted {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl Write for Counted {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let before = super::CLIENT_SPAWN_CALLS.with(Cell::get);
        // Fake connector: it owns no OS pipe and returns both ends already expired.
        let connect = || {
            (
                Box::new(Counted {
                    drops: drops.clone(),
                }) as Box<dyn Read + Send>,
                Box::new(Counted {
                    drops: drops.clone(),
                }) as Box<dyn Write + Send>,
            )
        };
        let expired = super::adopt_streams(connect(), Some(Instant::now()));
        assert!(expired.is_err());
        assert_eq!(super::CLIENT_SPAWN_CALLS.with(Cell::get), before);
        assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn an_expired_receive_leaves_a_queued_unrelated_event() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(ServerMessage::Event(formless_protocol::Event::RunStarted {
            session_id: "s".into(),
            run_id: "r".into(),
        }))
        .unwrap();
        let error =
            super::recv_response(&rx, 1, Instant::now(), Duration::from_secs(5)).unwrap_err();
        assert!(error.to_string().contains("did not answer"), "{error}");
        match rx.try_recv() {
            Ok(ServerMessage::Event(formless_protocol::Event::RunStarted { .. })) => {}
            other => panic!("queued unrelated event was not left in place: {other:?}"),
        }
    }

    /// Synthetic Windows daemon for six probe/status pairs.
    ///
    /// `FORMLESS_PIPE_BUSY_CHILD=1` is the test-only hook. The parent sets it
    /// only on the child command (never `set_var` here). The child also
    /// receives `FORMLESS_PIPE_BUSY_DATA`, `FORMLESS_PIPE_BUSY_CONFIG`, and
    /// `FORMLESS_PIPE_BUSY_PROFILE` for its private temp profile.
    /// `FORMLESS_API_KEY` is a dummy passed only in that child environment so
    /// `load_api_key` stops before the keychain. The parent does not call
    /// `save_api_key`. Paths are a random profile under the temp dir, with
    /// naming off and a loopback provider URL.
    #[cfg(windows)]
    #[test]
    fn fresh_daemon_double_open_status_succeeds() {
        if std::env::var_os("FORMLESS_PIPE_BUSY_CHILD").is_some() {
            pipe_busy_child_serve();
            return;
        }
        let short = &uuid::Uuid::new_v4().to_string()[..8];
        let base = std::env::temp_dir().join(format!("fl-busy-{short}"));
        let paths = Paths {
            data_dir: base.join("d"),
            config_dir: base.join("c"),
            profile: format!("busy-{short}"),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut config = formless_core::Config::default();
        config.naming.enabled = false;
        config.provider.base_url = "http://127.0.0.1:9".into();
        config.save(&paths).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
        for (key, _) in std::env::vars_os() {
            let upper = key.to_string_lossy().to_ascii_uppercase();
            if [
                "FORMLESS_",
                "OPENAI_",
                "ANTHROPIC_",
                "GROK_",
                "GOOGLE_",
                "GEMINI_",
            ]
            .iter()
            .any(|prefix| upper.starts_with(prefix))
                || upper.contains("API_KEY")
                || upper.contains("OAUTH")
                || upper == "FL_TEST_REFRESH_CHILD"
            {
                command.env_remove(key);
            }
        }
        let child = command
            .arg("client::pipe_busy_tests::fresh_daemon_double_open_status_succeeds")
            .arg("--exact")
            .env("FORMLESS_PIPE_BUSY_CHILD", "1")
            .env("FORMLESS_PIPE_BUSY_DATA", &paths.data_dir)
            .env("FORMLESS_PIPE_BUSY_CONFIG", &paths.config_dir)
            .env("FORMLESS_PIPE_BUSY_PROFILE", &paths.profile)
            .env("FORMLESS_API_KEY", "test-dummy-not-a-user-key")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("spawn synthetic daemon child");
        let mut owned = OwnedPipeDaemon {
            child,
            paths: paths.clone(),
            base: base.clone(),
            released: false,
        };
        for _ in 0..6 {
            let give_up = Instant::now() + Duration::from_secs(10);
            loop {
                if formless_core::daemon::daemon_reachable(&owned.paths) {
                    // The successful probe consumed one pipe instance.
                    // Status is the immediate second open.
                    expect_status(&owned.paths);
                    break;
                }
                assert!(
                    Instant::now() < give_up,
                    "fresh daemon did not accept a connection"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let ping = request_once_within(&owned.paths, Request::Ping, Duration::from_secs(10))
            .expect("daemon stayed up");
        assert_eq!(
            ping["min_protocol"].as_u64(),
            Some(formless_protocol::MIN_PROTOCOL_VERSION as u64),
            "{ping}"
        );
        // Issue 1789 acceptance: one request larger than the pipe buffer,
        // written while the duplex reader is pending, into the synthetic
        // temp profile only.
        let imported = request_once_within(
            &owned.paths,
            Request::ImportSession {
                bundle: serde_json::json!({
                    "format": "formless.session/1",
                    "name": "duplex import",
                    "messages": [{"role": "user", "content": "x".repeat(256 * 1024)}],
                }),
            },
            Duration::from_secs(10),
        )
        .expect("import over the duplex pipe");
        assert!(
            imported["id"].as_str().is_some_and(|id| !id.is_empty()),
            "{imported}"
        );
        expect_status(&owned.paths);
        owned
            .finish()
            .expect("shutdown, reap and clean synthetic daemon");
    }

    /// Kills and reaps the child on drop, including panic unwind.
    #[cfg(windows)]
    struct OwnedPipeDaemon {
        child: std::process::Child,
        paths: Paths,
        base: std::path::PathBuf,
        released: bool,
    }

    #[cfg(windows)]
    impl OwnedPipeDaemon {
        fn finish(&mut self) -> Result<()> {
            let shutdown =
                request_once_within(&self.paths, Request::Shutdown, Duration::from_secs(5));
            let bound = Instant::now() + Duration::from_secs(5);
            loop {
                match self.child.try_wait()? {
                    Some(status) => {
                        self.released = true;
                        std::fs::remove_dir_all(&self.base)?;
                        shutdown?;
                        anyhow::ensure!(status.success(), "daemon child: {status}");
                        return Ok(());
                    }
                    None if Instant::now() >= bound => {
                        anyhow::bail!("daemon child exceeded join bound")
                    }
                    None => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        }
    }

    #[cfg(windows)]
    impl Drop for OwnedPipeDaemon {
        fn drop(&mut self) {
            if !self.released {
                // No network request on unwind. Reap the owned process before
                // deleting its files, including after any normal-path error.
                let _ = self.child.kill();
                match self.child.wait() {
                    Ok(_) => self.released = true,
                    Err(error) => eprintln!("could not reap synthetic daemon: {error}"),
                }
            }
            if self.released && self.base.exists() {
                if let Err(error) = std::fs::remove_dir_all(&self.base) {
                    eprintln!("could not clean synthetic daemon directory: {error}");
                }
            }
        }
    }

    /// Serves until `Request::Shutdown` or the join bound. The `serve`
    /// [`JoinHandle`] is kept, joined within 60s, and the runtime is dropped
    /// before asserting this process holds the only `Daemon` arc.
    #[cfg(windows)]
    fn pipe_busy_child_serve() {
        let paths = Paths {
            data_dir: std::env::var("FORMLESS_PIPE_BUSY_DATA")
                .expect("data dir")
                .into(),
            config_dir: std::env::var("FORMLESS_PIPE_BUSY_CONFIG")
                .expect("config dir")
                .into(),
            profile: std::env::var("FORMLESS_PIPE_BUSY_PROFILE").expect("profile"),
        };
        let daemon =
            std::sync::Arc::new(formless_core::daemon::Daemon::new(paths).expect("fresh daemon"));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("child runtime");
        let serve_daemon = std::sync::Arc::clone(&daemon);
        let serve = runtime.spawn(async move { serve_daemon.serve().await });
        runtime.block_on(async {
            let joined = tokio::time::timeout(Duration::from_secs(60), serve).await;
            joined
                .expect("bounded serve join")
                .expect("serve task")
                .expect("serve result");
        });
        drop(runtime);
        assert_eq!(
            std::sync::Arc::strong_count(&daemon),
            1,
            "sole daemon drop before the child exits"
        );
        drop(daemon);
    }

    #[cfg(windows)]
    fn expect_status(paths: &Paths) {
        let status = request_once_within(paths, Request::Status, Duration::from_secs(10))
            .expect("status after the reachable probe");
        assert!(status["version"].is_string(), "{status}");
        assert!(status["pid"].as_u64().is_some(), "{status}");
    }
}
