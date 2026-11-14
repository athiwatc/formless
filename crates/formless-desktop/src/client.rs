//! Local daemon client. Domain state stays in the out-of-process daemon.
//!
//! Ordering contract: the existing protocol orders frames, but has no
//! snapshot watermark or delta offset. Consequently this client subscribes
//! with `deltas: false`, treats events as invalidations, and installs only
//! correlated canonical snapshots. It never replays events onto a snapshot.
//! A relevant event during a read schedules another read, including when the
//! first reply already contained it. This is deliberately conservative.
//!
//! A worker owns all I/O and a bounded command/update queue. Reconnect starts
//! a fresh engine and never retries mutations: daemon send deduplication is
//! connection-scoped, so retrying across connections is unsafe.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use formless_protocol::{
    Event, Request, RequestEnvelope, ServerMessage, SessionInfo, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc as queue, watch};

use crate::model::{Destination, Intent};
use crate::replies::{self, SessionSnapshot};

const QUEUE: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub enum Update {
    Connection {
        ready: bool,
        notice: String,
    },
    Sessions(Vec<SessionInfo>),
    Snapshot {
        generation: u64,
        snapshot: SessionSnapshot,
    },
    SnapshotError {
        generation: u64,
        session_id: String,
        error: String,
    },
    Attention {
        generation: u64,
        pending: bool,
    },
    Page {
        destination: Destination,
        result: Result<Value, String>,
    },
    Action {
        session_id: String,
        result: Result<(), String>,
    },
    Notice(String),
}

pub enum Command {
    Reconnect,
    Select {
        session_id: Option<String>,
        generation: u64,
    },
    Page(Destination),
    Intent(Intent),
    CreateSession,
    Refresh,
}

/// An explicit local socket/pipe path avoids duplicating Profile discovery
/// or opening configuration/credentials in the presentation process.
#[derive(Clone, Debug)]
pub struct Endpoint(pub PathBuf);

pub struct Client {
    commands: queue::Sender<Command>,
    updates: Receiver<Update>,
    stop: watch::Sender<bool>,
    ready: Arc<AtomicBool>,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl Client {
    pub fn start(endpoint: Endpoint) -> Self {
        let (commands, rx) = queue::channel(QUEUE);
        let (tx, updates) = mpsc::sync_channel(QUEUE);
        let (stop, cancelled) = watch::channel(false);
        let ready = Arc::new(AtomicBool::new(false));
        let worker_ready = ready.clone();
        std::thread::spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(worker(endpoint, rx, tx, cancelled, worker_ready)),
                Err(error) => {
                    let _ = tx.try_send(Update::Connection {
                        ready: false,
                        notice: error.to_string(),
                    });
                }
            }
        });
        Self {
            commands,
            updates,
            stop,
            ready,
        }
    }

    pub fn send(&self, command: Command) -> Result<(), &'static str> {
        if matches!(command, Command::Intent(_) | Command::CreateSession) && !self.is_ready() {
            return Err("Not connected; the action was not queued.");
        }
        self.commands
            .try_send(command)
            .map_err(|_| "The daemon command queue is unavailable; nothing was queued.")
    }

    pub fn drain(&self) -> impl Iterator<Item = Update> + '_ {
        self.updates.try_iter().take(QUEUE)
    }
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
enum Pending {
    Ping,
    Subscribe,
    Sessions,
    Snapshot { session_id: String, generation: u64 },
    Page(Destination),
    Action(Action),
    Create,
}

/// Transport request ids restart on reconnect. Each mutation needs a separate
/// identity so an ACK for Stop (or another connection) cannot resolve an
/// earlier Send whose outcome is still unknown.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Action {
    id: uuid::Uuid,
    session_id: String,
}

/// Pure, connection-local correlation and invalidation state. The worker
/// replaces this on reconnect; selection generations belong to the window.
#[derive(Default)]
struct Engine {
    next_id: u64,
    ready: bool,
    active: Option<String>,
    generation: u64,
    page: Option<Destination>,
    pending: HashMap<u64, Pending>,
    outgoing: VecDeque<RequestEnvelope>,
    updates: Vec<Update>,
    dirty_session: bool,
    dirty_list: bool,
}

impl Engine {
    fn request(&mut self, request: Request, pending: Pending) {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("connection request id exhausted");
        self.pending.insert(self.next_id, pending);
        self.outgoing.push_back(RequestEnvelope {
            id: self.next_id,
            protocol: PROTOCOL_VERSION,
            request,
        });
    }

    fn begin(&mut self) {
        self.request(Request::Ping, Pending::Ping);
    }

    fn subscribe(&mut self) {
        self.request(
            Request::Subscribe {
                sessions: Some(self.active.iter().cloned().collect()),
                deltas: false,
                client: Some("Desktop".into()),
            },
            Pending::Subscribe,
        );
    }

    fn select(&mut self, session_id: Option<String>, generation: u64) {
        self.active = session_id;
        self.generation = generation;
        self.dirty_session = true;
        if self.ready {
            self.subscribe();
        }
    }

    fn refresh(&mut self) {
        if !self.ready {
            return;
        }
        if self.dirty_list
            && !self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Sessions))
        {
            self.dirty_list = false;
            self.request(Request::ListSessions, Pending::Sessions);
        }
        if self.dirty_session
            && !self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Snapshot { .. }))
        {
            self.dirty_session = false;
            if let Some(session_id) = self.active.clone() {
                self.request(
                    Request::GetSession {
                        id: session_id.clone(),
                    },
                    Pending::Snapshot {
                        session_id,
                        generation: self.generation,
                    },
                );
            }
        }
    }

    fn load_page(&mut self, destination: Destination) {
        self.page = Some(destination);
        if !self.ready
            || self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Page(d) if *d == destination))
        {
            return;
        }
        let request = match destination {
            Destination::Tools => Request::ListTools,
            Destination::Metrics => Request::RunMetrics {
                limit: Some(100),
                session_id: None,
                since: None,
            },
            Destination::Proposals => Request::ListProposals,
            Destination::Settings => Request::GetSettings,
            _ => return,
        };
        self.request(request, Pending::Page(destination));
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Reconnect => {} // The I/O owner handles this command.
            Command::Select {
                session_id,
                generation,
            } => self.select(session_id, generation),
            Command::Page(page) => self.load_page(page),
            Command::Refresh => {
                self.dirty_list = true;
                self.dirty_session = true;
                if let Some(page) = self.page {
                    self.load_page(page);
                }
            }
            Command::CreateSession if self.ready => {
                self.request(Request::CreateSession { name: None }, Pending::Create)
            }
            Command::Intent(intent) if self.ready => {
                let action_id = uuid::Uuid::new_v4();
                let (session_id, request) = match intent {
                    Intent::SendMessage {
                        session_id,
                        content,
                        mode,
                    } => {
                        if self
                            .pending
                            .values()
                            .any(|p| matches!(p, Pending::Action(action) if action.session_id == session_id))
                        {
                            self.updates.push(Update::Notice("Wait for the previous action's acknowledgement; the draft was kept.".into()));
                            return;
                        }
                        (
                            session_id.clone(),
                            Request::SendMessage {
                                session_id,
                                content,
                                mode,
                                client_msg_id: Some(action_id.to_string()),
                            },
                        )
                    }
                    Intent::InterruptRun { session_id } => {
                        (session_id.clone(), Request::InterruptRun { session_id })
                    }
                };
                self.request(
                    request,
                    Pending::Action(Action {
                        id: action_id,
                        session_id,
                    }),
                );
            }
            Command::Intent(_) | Command::CreateSession => self.updates.push(Update::Notice(
                "Not connected; the action was not sent. The draft was kept.".into(),
            )),
        }
    }

    fn receive(&mut self, message: ServerMessage) -> Result<(), String> {
        let (id, result) = match message {
            ServerMessage::Event(event) => {
                // No event mutates canonical text or asserts a Run state.
                // Unscoped gap notices conservatively invalidate everything.
                if event
                    .session_id()
                    .is_none_or(|id| Some(id) == self.active.as_deref())
                    && !matches!(
                        event,
                        Event::MessageDelta { .. }
                            | Event::ReasoningDelta { .. }
                            | Event::ToolProgress { .. }
                    )
                {
                    self.dirty_session = true;
                }
                if matches!(
                    event,
                    Event::SessionUpdated { .. }
                        | Event::SessionDeleted { .. }
                        | Event::RunStarted { .. }
                        | Event::RunFinished { .. }
                ) {
                    self.dirty_list = true;
                }
                if matches!(
                    event,
                    Event::ApprovalRequested { .. } | Event::UserQuestion { .. }
                ) {
                    self.updates.push(Update::Attention {
                        generation: self.generation,
                        pending: true,
                    });
                }
                return Ok(());
            }
            ServerMessage::Response {
                id,
                ok,
                result,
                error,
            } => (
                id,
                if ok {
                    result.ok_or_else(|| "The daemon returned no result.".into())
                } else {
                    Err(error.unwrap_or_else(|| "The daemon rejected this request.".into()))
                },
            ),
        };
        let Some(pending) = self.pending.remove(&id) else {
            return Ok(());
        };
        match pending {
            Pending::Ping => {
                let result = result?;
                let min = result["min_protocol"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or("The daemon did not advertise its minimum protocol.")?;
                let max = result["max_protocol"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or("The daemon did not advertise its maximum protocol.")?;
                if formless_protocol::negotiate(min, max).is_none() {
                    return Err(
                        "Incompatible daemon protocol. Update the desktop and daemon together."
                            .into(),
                    );
                }
                self.subscribe();
            }
            Pending::Subscribe => {
                result?;
                self.ready = true;
                self.dirty_list = true;
                self.dirty_session = true;
                self.updates.push(Update::Connection {
                    ready: true,
                    notice: "Connected · canonical messages · live token streaming unavailable"
                        .into(),
                });
                if let Some(page) = self.page {
                    self.load_page(page);
                }
            }
            Pending::Sessions => match result
                .and_then(|value| replies::session_list(&value).map_err(|e| e.to_string()))
            {
                Ok(sessions) => self.updates.push(Update::Sessions(sessions)),
                Err(error) => self
                    .updates
                    .push(Update::Notice(format!("Could not load Sessions: {error}"))),
            },
            Pending::Snapshot {
                session_id,
                generation,
            } => {
                // Refuse retired A→B→A requests before decoding/allocating.
                if self.active.as_deref() == Some(&session_id) && self.generation == generation {
                    match result.and_then(|value| {
                        let attention = value
                            .get("pending_events")
                            .and_then(Value::as_array)
                            .map(|events| !events.is_empty());
                        replies::session_snapshot(&session_id, &value)
                            .map(|snapshot| (snapshot, attention))
                            .map_err(|e| e.to_string())
                    }) {
                        Ok((snapshot, attention)) => {
                            self.updates.push(Update::Snapshot {
                                generation,
                                snapshot,
                            });
                            if let Some(pending) = attention {
                                self.updates.push(Update::Attention {
                                    generation,
                                    pending,
                                });
                            }
                        }
                        Err(error) => self.updates.push(Update::SnapshotError {
                            generation,
                            session_id,
                            error,
                        }),
                    }
                }
            }
            Pending::Page(destination) => self.updates.push(Update::Page {
                destination,
                result,
            }),
            Pending::Action(action) => {
                self.dirty_session = true;
                self.dirty_list = true;
                self.updates.push(Update::Action {
                    session_id: action.session_id,
                    result: result.map(|_| ()),
                });
            }
            Pending::Create => {
                self.dirty_list = true;
                self.updates.push(Update::Notice(match result {
                    Ok(_) => "Session created. Open it from Sessions.".into(),
                    Err(error) => format!("Could not create Session: {error}"),
                }));
            }
        }
        Ok(())
    }
}

struct Frames<R> {
    reader: R,
    partial: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> Frames<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            partial: Vec::new(),
        }
    }

    // Cancellation-safe: a command/timer can win the select between any
    // two fragments. Consumed bytes remain in `partial`, outside the future.
    async fn next(&mut self) -> io::Result<Option<ServerMessage>> {
        loop {
            let bytes = self.reader.fill_buf().await?;
            if bytes.is_empty() {
                return if self.partial.is_empty() {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Incomplete daemon frame",
                    ))
                };
            }
            let end = bytes.iter().position(|b| *b == b'\n').map(|p| p + 1);
            let len = end.unwrap_or(bytes.len());
            if self.partial.len().saturating_add(len) > formless_protocol::MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Oversized daemon frame",
                ));
            }
            self.partial.extend_from_slice(&bytes[..len]);
            self.reader.consume(len);
            if end.is_some() {
                let message = serde_json::from_slice(&self.partial).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Unrecognized daemon frame: {e}"),
                    )
                });
                self.partial.clear();
                return message.map(Some);
            }
        }
    }
}

async fn write_requests(
    writer: &mut (impl AsyncWrite + Unpin),
    engine: &mut Engine,
) -> io::Result<()> {
    while let Some(request) = engine.outgoing.pop_front() {
        let mut line = serde_json::to_vec(&request)?;
        if line.len() >= formless_protocol::MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Request exceeds daemon frame limit",
            ));
        }
        line.push(b'\n');
        tokio::time::timeout(Duration::from_secs(5), writer.write_all(&line)).await??;
    }
    Ok(())
}

fn deliver(tx: &SyncSender<Update>, engine: &mut Engine) -> Result<(), String> {
    for update in engine.updates.drain(..) {
        tx.try_send(update).map_err(|_| {
            "Desktop update queue unavailable; reconnecting to reload canonical state.".to_string()
        })?;
    }
    Ok(())
}

#[derive(Default)]
struct Retained {
    active: (Option<String>, u64),
    page: Option<Destination>,
    uncertain: HashSet<Action>,
}

async fn connected<S: tokio::io::AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    commands: &mut queue::Receiver<Command>,
    tx: &SyncSender<Update>,
    stop: &mut watch::Receiver<bool>,
    retained: &mut Retained,
    ready: &AtomicBool,
) -> Result<(), String> {
    let Retained {
        active,
        page,
        uncertain,
    } = retained;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = Frames::new(BufReader::new(reader));
    let mut engine = Engine {
        active: active.0.clone(),
        generation: active.1,
        page: *page,
        ..Engine::default()
    };
    engine.begin();
    let mut tick = tokio::time::interval(REFRESH);
    let mut deadlines = HashMap::new();
    let mut last_poll = std::time::Instant::now();
    loop {
        for id in engine.pending.keys() {
            deadlines.entry(*id).or_insert_with(std::time::Instant::now);
        }
        deadlines.retain(|id, _| engine.pending.contains_key(id));
        if deadlines.values().any(|at| at.elapsed() >= REQUEST_TIMEOUT) {
            return Err("A daemon request timed out; its action outcome may be unknown.".into());
        }
        // Mutations with no definite response are never replayed. Record
        // them before attempting a write, which may partially succeed.
        uncertain.extend(engine.pending.values().filter_map(|p| {
            if let Pending::Action(action) = p {
                Some(action.clone())
            } else {
                None
            }
        }));
        write_requests(&mut writer, &mut engine)
            .await
            .map_err(|e| e.to_string())?;
        deliver(tx, &mut engine)?;
        tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            command = commands.recv() => match command {
                None => return Ok(()),
                Some(Command::Reconnect) => return Err("Reconnecting at the user's request; actions are not retried.".into()),
                Some(Command::Select { session_id, generation }) => { *active = (session_id.clone(), generation); engine.select(session_id, generation); }
                Some(Command::Page(destination)) => { *page = Some(destination); engine.load_page(destination); }
                Some(Command::Intent(Intent::SendMessage { session_id, .. })) if uncertain.iter().any(|action| action.session_id == session_id && !engine.pending.values().any(|p| matches!(p, Pending::Action(pending) if pending == action))) => {
                    engine.updates.push(Update::Notice("A previous action's outcome is unknown. It was not retried: deduplication does not survive reconnect. Check this Session in the TUI before sending again.".into()));
                }
                Some(command) => engine.command(command),
            },
            message = reader.next() => match message.map_err(|e| e.to_string())? {
                Some(message) => {
                    let answered = match &message { ServerMessage::Response { id, .. } => match engine.pending.get(id) { Some(Pending::Action(action)) => Some(action.clone()), _ => None }, _ => None };
                    engine.receive(message)?;
                    ready.store(engine.ready, Ordering::Release);
                    if let Some(id) = answered { uncertain.remove(&id); }
                }
                None => return Err("Daemon disconnected. Run state is unknown; reconnecting.".into()),
            },
            _ = tick.tick() => {
                if last_poll.elapsed() >= Duration::from_secs(2) {
                    engine.dirty_session = true;
                    last_poll = std::time::Instant::now();
                }
                engine.refresh();
            }
        }
    }
}

async fn worker(
    endpoint: Endpoint,
    mut commands: queue::Receiver<Command>,
    tx: SyncSender<Update>,
    mut stop: watch::Receiver<bool>,
    ready: Arc<AtomicBool>,
) {
    let mut retained = Retained::default();
    let mut delay = Duration::from_millis(250);
    loop {
        ready.store(false, Ordering::Release);
        // Retain view intent, discard unexecuted mutations from a retired
        // connection before a new handshake can make them runnable.
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Select {
                    session_id,
                    generation,
                } => retained.active = (session_id, generation),
                Command::Page(destination) => retained.page = Some(destination),
                _ => {
                    let _ = tx.try_send(Update::Notice(
                        "A queued action was not sent after disconnect. Draft kept.".into(),
                    ));
                }
            }
        }
        let _ = tx.try_send(Update::Connection {
            ready: false,
            notice: "Connecting to the local Daemon Core…".into(),
        });
        #[cfg(unix)]
        let opening = tokio::net::UnixStream::connect(&endpoint.0);
        #[cfg(windows)]
        let opening =
            async { tokio::net::windows::named_pipe::ClientOptions::new().open(&endpoint.0) };
        let result = tokio::select! {
            _ = stop.changed() => return,
            result = tokio::time::timeout(Duration::from_secs(2), opening) => match result {
                Ok(Ok(stream)) => connected(stream, &mut commands, &tx, &mut stop, &mut retained, &ready).await,
                Ok(Err(error)) => Err(format!("Cannot connect: {error}. Start the existing daemon with the CLI.")),
                Err(error) => Err(error.to_string()),
            }
        };
        ready.store(false, Ordering::Release);
        if *stop.borrow() || commands.is_closed() {
            return;
        }
        let notice = result
            .err()
            .unwrap_or_else(|| "Disconnected; reconnecting.".into());
        let _ = tx.try_send(Update::Connection {
            ready: false,
            notice,
        });
        // Commands arriving while disconnected are drained, never silently
        // saved as mutations to execute after a later connection.
        let timer = tokio::time::sleep(delay);
        tokio::pin!(timer);
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                _ = &mut timer => break,
                command = commands.recv() => match command {
                    Some(Command::Select { session_id, generation }) => retained.active = (session_id, generation),
                    Some(Command::Page(destination)) => retained.page = Some(destination),
                    Some(_) => { let _ = tx.try_send(Update::Notice("Not connected; the action was not sent. Draft kept.".into())); }
                    None => return,
                }
            }
        }
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_protocol::{MessageInfo, SendMode};
    use serde_json::json;

    fn respond(engine: &mut Engine, id: u64, value: Value) {
        engine.receive(ServerMessage::ok(id, value)).unwrap();
    }

    fn ready() -> Engine {
        let mut engine = Engine::default();
        engine.begin();
        let ping = engine.outgoing.pop_front().unwrap();
        assert!(matches!(ping.request, Request::Ping));
        respond(
            &mut engine,
            ping.id,
            json!({"min_protocol":1,"max_protocol":1}),
        );
        let subscribe = engine.outgoing.pop_front().unwrap();
        assert!(matches!(
            subscribe.request,
            Request::Subscribe { deltas: false, .. }
        ));
        respond(
            &mut engine,
            subscribe.id,
            json!({"subscribed":true,"conn":1}),
        );
        engine.dirty_list = false;
        engine.updates.clear();
        engine
    }

    fn snapshot(id: &str, text: &str) -> Value {
        let message = MessageInfo {
            id: "m1".into(),
            session_id: id.into(),
            seq: 1,
            role: "assistant".into(),
            content: text.into(),
            created_at: "now".into(),
            meta: None,
        };
        json!({"session":{"id":id},"messages":[message],"running":false})
    }

    fn snapshot_request(engine: &mut Engine) -> u64 {
        engine.refresh();
        engine
            .outgoing
            .iter()
            .rev()
            .find(|e| matches!(e.request, Request::GetSession { .. }))
            .unwrap()
            .id
    }

    #[test]
    fn rejects_incompatible_or_malformed_protocol_before_subscribing_or_mutating() {
        for value in [
            json!({"min_protocol":2,"max_protocol":2}),
            json!({"min_protocol":0,"max_protocol":u64::MAX}),
            json!({}),
        ] {
            let mut engine = Engine::default();
            engine.begin();
            assert!(engine.receive(ServerMessage::ok(1, value)).is_err());
            assert!(!engine.ready);
            assert_eq!(engine.outgoing.len(), 1);
        }
    }

    #[test]
    fn snapshot_overlap_and_delayed_events_only_invalidate_never_replay_text() {
        let mut engine = ready();
        engine.select(Some("a".into()), 1);
        let id = snapshot_request(&mut engine);
        let delta = Event::MessageDelta {
            session_id: "a".into(),
            run_id: "r".into(),
            delta: "already persisted".into(),
        };
        engine.receive(ServerMessage::Event(delta.clone())).unwrap();
        engine
            .receive(ServerMessage::Event(Event::MessagesEdited {
                session_id: "a".into(),
            }))
            .unwrap();
        respond(&mut engine, id, snapshot("a", "canonical"));
        assert!(
            engine.dirty_session,
            "an overlapping invalidation survives the reply"
        );
        let next = snapshot_request(&mut engine);
        assert_ne!(next, id);
        respond(&mut engine, next, snapshot("a", "canonical"));
        engine.receive(ServerMessage::Event(delta)).unwrap();
        let text: Vec<_> = engine
            .updates
            .iter()
            .filter_map(|u| match u {
                Update::Snapshot { snapshot, .. } => Some(snapshot.blocks[0].markdown()),
                _ => None,
            })
            .collect();
        assert_eq!(text, ["canonical", "canonical"]);
    }

    #[test]
    fn a_b_a_and_same_id_reattachment_refuse_retired_replies_before_decoding() {
        let mut engine = ready();
        engine.select(Some("a".into()), 1);
        let old = snapshot_request(&mut engine);
        engine.select(Some("b".into()), 2);
        engine.select(Some("a".into()), 3);
        respond(&mut engine, old, Value::Null);
        assert!(engine.updates.is_empty());
        let current = snapshot_request(&mut engine);
        respond(&mut engine, current, snapshot("a", "new"));
        assert!(matches!(
            engine.updates.last(),
            Some(Update::Snapshot { generation: 3, .. })
        ));
        engine.updates.clear();
        respond(&mut engine, current, snapshot("a", "duplicate response"));
        assert!(engine.updates.is_empty());
    }

    #[test]
    fn disconnected_commands_never_queue_mutations_and_duplicate_pending_send_is_blocked() {
        let intent = || Intent::SendMessage {
            session_id: "a".into(),
            content: "hello ไทย".into(),
            mode: SendMode::Send,
        };
        let mut engine = Engine::default();
        engine.command(Command::Intent(intent()));
        engine.command(Command::CreateSession);
        assert!(engine.outgoing.is_empty());
        engine = ready();
        engine.command(Command::Intent(intent()));
        engine.command(Command::Intent(intent()));
        assert_eq!(engine.outgoing.len(), 1);
        let request = engine.outgoing.pop_front().unwrap();
        assert!(matches!(
            request.request,
            Request::SendMessage {
                client_msg_id: Some(_),
                ..
            }
        ));
        engine.command(Command::Intent(Intent::InterruptRun {
            session_id: "a".into(),
        }));
        assert!(matches!(
            engine.outgoing.back().unwrap().request,
            Request::InterruptRun { .. }
        ));
        respond(&mut engine, request.id, json!({"sent":true}));
        assert!(engine
            .updates
            .iter()
            .any(|u| matches!(u, Update::Action { result: Ok(()), .. })));
    }

    #[test]
    fn pending_decisions_are_restored_from_snapshots_without_treating_a_missing_field_as_clear() {
        let mut engine = ready();
        engine.select(Some("a".into()), 1);
        let request = snapshot_request(&mut engine);
        let mut value = snapshot("a", "known");
        value["pending_events"] = json!([{"type":"user_question"}]);
        respond(&mut engine, request, value);
        assert!(engine.updates.iter().any(|update| matches!(
            update,
            Update::Attention {
                generation: 1,
                pending: true
            }
        )));
        engine.updates.clear();
        engine.dirty_session = true;
        let request = snapshot_request(&mut engine);
        respond(&mut engine, request, snapshot("a", "known"));
        assert!(!engine
            .updates
            .iter()
            .any(|update| matches!(update, Update::Attention { .. })));
        engine.updates.clear();
        engine.dirty_session = true;
        let request = snapshot_request(&mut engine);
        let mut value = snapshot("a", "known");
        value["pending_events"] = json!([]);
        respond(&mut engine, request, value);
        assert!(engine.updates.iter().any(|update| matches!(
            update,
            Update::Attention {
                generation: 1,
                pending: false
            }
        )));
    }

    #[test]
    fn malformed_or_foreign_snapshot_is_an_explicit_error_and_resource_errors_stay_scoped() {
        let mut engine = ready();
        engine.select(Some("a".into()), 1);
        let id = snapshot_request(&mut engine);
        respond(&mut engine, id, snapshot("b", "wrong Session"));
        assert!(matches!(
            engine.updates.last(),
            Some(Update::SnapshotError { .. })
        ));
        engine.load_page(Destination::Tools);
        let request = engine.outgoing.back().unwrap().id;
        engine
            .receive(ServerMessage::err(request, "offline tools".into()))
            .unwrap();
        assert!(matches!(
            engine.updates.last(),
            Some(Update::Page {
                destination: Destination::Tools,
                result: Err(_),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn fragmented_unicode_frame_survives_cancelled_read_and_preserves_following_frame() {
        let (reader, mut writer) = tokio::io::duplex(1024);
        let mut frames = Frames::new(BufReader::new(reader));
        let mut first = serde_json::to_vec(&ServerMessage::ok(3, json!("ไทย😀"))).unwrap();
        first.push(b'\n');
        let split = first.iter().position(|b| *b >= 128).unwrap() + 1;
        writer.write_all(&first[..split]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), frames.next())
                .await
                .is_err()
        );
        writer.write_all(&first[split..]).await.unwrap();
        writer
            .write_all(b"{\"kind\":\"response\",\"id\":4,\"ok\":true,\"result\":{}}\n")
            .await
            .unwrap();
        assert!(matches!(
            frames.next().await.unwrap(),
            Some(ServerMessage::Response { id: 3, .. })
        ));
        assert!(matches!(
            frames.next().await.unwrap(),
            Some(ServerMessage::Response { id: 4, .. })
        ));
        drop(writer);
        assert!(frames.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn malformed_utf8_json_and_incomplete_frames_close_the_connection() {
        for bytes in [b"\xff\n".as_slice(), b"{no}\n", b"{\"kind\":\"event\"}"] {
            let mut frames = Frames::new(BufReader::new(bytes));
            assert!(frames.next().await.is_err());
        }
    }

    async fn disconnect_after_unacknowledged_send() -> Retained {
        let (stream, server) = tokio::io::duplex(8192);
        let (command_tx, mut commands) = queue::channel(QUEUE);
        let _keep_commands_open = command_tx.clone();
        let (updates, _rx) = mpsc::sync_channel(QUEUE);
        let (_stop, mut cancelled) = watch::channel(false);
        let ready = AtomicBool::new(false);
        let mut retained = Retained {
            active: (Some("a".into()), 1),
            ..Default::default()
        };
        let peer = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            loop {
                let mut line = String::new();
                server.read_line(&mut line).await.unwrap();
                let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let result = match request.request {
                    Request::Ping => json!({"min_protocol":1,"max_protocol":1}),
                    Request::Subscribe { deltas, .. } => {
                        assert!(!deltas);
                        json!({"subscribed":true})
                    }
                    Request::ListSessions => json!([]),
                    Request::GetSession { .. } => {
                        command_tx
                            .send(Command::Intent(Intent::SendMessage {
                                session_id: "a".into(),
                                content: "one action".into(),
                                mode: SendMode::Send,
                            }))
                            .await
                            .unwrap();
                        snapshot("a", "known")
                    }
                    Request::SendMessage { client_msg_id, .. } => {
                        assert!(client_msg_id.is_some());
                        break;
                    }
                    other => panic!("unexpected request {other:?}"),
                };
                let line = format!(
                    "{}\n",
                    serde_json::to_string(&ServerMessage::ok(request.id, result)).unwrap()
                );
                server.get_mut().write_all(line.as_bytes()).await.unwrap();
            }
            // Drop the peer without acknowledging the send.
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            connected(
                stream,
                &mut commands,
                &updates,
                &mut cancelled,
                &mut retained,
                &ready,
            ),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("disconnected"));
        peer.await.unwrap();
        assert!(retained
            .uncertain
            .iter()
            .any(|action| action.session_id == "a"));
        retained
    }

    #[tokio::test]
    async fn eof_after_a_send_preserves_unknown_outcome_without_replay() {
        disconnect_after_unacknowledged_send().await;
    }

    #[tokio::test]
    async fn reconnect_stop_acknowledgements_do_not_clear_an_unknown_send() {
        let mut retained = disconnect_after_unacknowledged_send().await;
        let original_unknown = retained.uncertain.clone();
        for stop_succeeded in [true, false] {
            let (stream, server) = tokio::io::duplex(8192);
            let (command_tx, mut commands) = queue::channel(QUEUE);
            let _keep_commands_open = command_tx.clone();
            let (updates, rx) = mpsc::sync_channel(QUEUE);
            let (_stop, mut cancelled) = watch::channel(false);
            let ready = AtomicBool::new(false);
            let peer = tokio::spawn(async move {
                let mut server = BufReader::new(server);
                let mut snapshots = 0;
                let mut other_session_sent = false;
                let mut other_session_acknowledged = false;
                loop {
                    let mut line = String::new();
                    server.read_line(&mut line).await.unwrap();
                    let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                    let result = match request.request {
                        Request::Ping => json!({"min_protocol":1,"max_protocol":1}),
                        Request::Subscribe { .. } => json!({"subscribed":true}),
                        Request::ListSessions => json!([]),
                        Request::GetSession { .. } => {
                            snapshots += 1;
                            if snapshots == 1 {
                                command_tx
                                    .send(Command::Intent(Intent::InterruptRun {
                                        session_id: "a".into(),
                                    }))
                                    .await
                                    .unwrap();
                            } else if rx.try_iter().any(|update| {
                                matches!(update,
                                    Update::Notice(notice) if notice.contains("outcome is unknown")
                                )
                            }) {
                                if other_session_acknowledged {
                                    // Observe the retry's rejection after both the
                                    // Stop ACK and another Session's Send ACK.
                                    // A poll alone does not prove it was processed.
                                    let line = format!(
                                        "{}\n",
                                        serde_json::to_string(&ServerMessage::ok(
                                            request.id,
                                            snapshot("a", "confirmed")
                                        ))
                                        .unwrap()
                                    );
                                    server.get_mut().write_all(line.as_bytes()).await.unwrap();
                                    break;
                                }
                                assert!(!other_session_sent);
                                other_session_sent = true;
                                command_tx
                                    .send(Command::Intent(Intent::SendMessage {
                                        session_id: "b".into(),
                                        content: "new action in another Session".into(),
                                        mode: SendMode::Send,
                                    }))
                                    .await
                                    .unwrap();
                            }
                            snapshot("a", "confirmed")
                        }
                        Request::InterruptRun { session_id } => {
                            assert_eq!(session_id, "a");
                            let response = if stop_succeeded {
                                ServerMessage::ok(request.id, json!({"interrupted":true}))
                            } else {
                                ServerMessage::err(request.id, "already stopped".into())
                            };
                            let line = format!("{}\n", serde_json::to_string(&response).unwrap());
                            server.get_mut().write_all(line.as_bytes()).await.unwrap();
                            // Observe processing of this exact Stop reply before
                            // submitting another Send; socket write order alone
                            // would not establish that the ACK was applied.
                            loop {
                                if rx
                                    .try_iter()
                                    .any(|update| matches!(update, Update::Action { .. }))
                                {
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                            command_tx
                                .send(Command::Intent(Intent::SendMessage {
                                    session_id: "a".into(),
                                    content: "one action".into(),
                                    mode: SendMode::Send,
                                }))
                                .await
                                .unwrap();
                            command_tx.send(Command::Refresh).await.unwrap();
                            continue;
                        }
                        Request::SendMessage { session_id, .. } => {
                            assert_eq!(session_id, "b",
                                "an unrelated response allowed retrying an unacknowledged Send across connections");
                            assert!(other_session_sent && !other_session_acknowledged);
                            let line = format!(
                                "{}\n",
                                serde_json::to_string(&ServerMessage::ok(
                                    request.id,
                                    json!({"sent":true})
                                ))
                                .unwrap()
                            );
                            server.get_mut().write_all(line.as_bytes()).await.unwrap();
                            loop {
                                if rx.try_iter().any(|update| matches!(update,
                                    Update::Action { session_id, result: Ok(()) } if session_id == "b"
                                )) { break; }
                                tokio::task::yield_now().await;
                            }
                            other_session_acknowledged = true;
                            command_tx
                                .send(Command::Intent(Intent::SendMessage {
                                    session_id: "a".into(),
                                    content: "one action".into(),
                                    mode: SendMode::Send,
                                }))
                                .await
                                .unwrap();
                            command_tx.send(Command::Refresh).await.unwrap();
                            continue;
                        }
                        other => panic!("unexpected request {other:?}"),
                    };
                    let line = format!(
                        "{}\n",
                        serde_json::to_string(&ServerMessage::ok(request.id, result)).unwrap()
                    );
                    server.get_mut().write_all(line.as_bytes()).await.unwrap();
                }
                // Keep the update consumer alive until connected() observes
                // EOF; dropping it first would test queue failure instead.
                rx
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                let result = connected(
                    stream,
                    &mut commands,
                    &updates,
                    &mut cancelled,
                    &mut retained,
                    &ready,
                )
                .await;
                peer.await.unwrap();
                assert!(result.unwrap_err().contains("disconnected"));
            })
            .await
            .expect("reconnect/Stop/retry regression timed out");
            assert_eq!(retained.uncertain, original_unknown);
        }
    }

    #[tokio::test]
    async fn oversized_frame_never_extends_the_partial_buffer_past_protocol_limit() {
        let bytes = vec![b' '; formless_protocol::MAX_FRAME_BYTES + 1];
        let mut frames = Frames::new(BufReader::new(bytes.as_slice()));
        assert_eq!(
            frames.next().await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(frames.partial.len() <= formless_protocol::MAX_FRAME_BYTES);
    }
}
