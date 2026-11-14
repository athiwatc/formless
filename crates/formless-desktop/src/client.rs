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
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use formless_protocol::{
    Event, Request, RequestEnvelope, ServerMessage, SessionInfo, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc as queue, watch};

use crate::forecast;
use crate::model::{Destination, Intent};
use crate::questions;
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
    Questions(questions::State),
    QuestionAnswer {
        connection: u64,
        operation: uuid::Uuid,
        question_id: String,
        outcome: questions::Outcome,
    },
    Page {
        destination: Destination,
        result: Result<Value, String>,
    },
    Action {
        session_id: String,
        result: Result<(), String>,
    },
    Prepared(Box<forecast::Prepared>),
    PreparationReady {
        id: uuid::Uuid,
        epoch: u64,
    },
    PreparationFailed {
        id: uuid::Uuid,
        epoch: u64,
        error: String,
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
    Review {
        intent: Intent,
        draft_basis: String,
        epoch: u64,
    },
    ConfirmPrepared(uuid::Uuid),
    CommitPrepared(uuid::Uuid),
    CreateSession,
    Refresh,
    AnswerQuestion {
        connection: u64,
        operation: uuid::Uuid,
        question: Box<questions::Open>,
        answers: Vec<formless_protocol::QuestionAnswer>,
    },
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
    epoch: Arc<AtomicU64>,
    connection: Arc<AtomicU64>,
    #[cfg(feature = "test-support")]
    recorded: Option<queue::Receiver<Command>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.invalidate_context();
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
        let epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = epoch.clone();
        let connection = Arc::new(AtomicU64::new(0));
        let worker_connection = connection.clone();
        std::thread::spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(worker(
                    endpoint,
                    rx,
                    tx,
                    cancelled,
                    worker_ready,
                    worker_epoch,
                    worker_connection,
                )),
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
            epoch,
            connection,
            #[cfg(feature = "test-support")]
            recorded: None,
        }
    }

    pub fn send(&self, command: Command) -> Result<(), &'static str> {
        if matches!(command, Command::Select { .. } | Command::Reconnect) {
            self.invalidate_context();
        }
        if matches!(
            command,
            Command::Intent(_)
                | Command::CreateSession
                | Command::Review { .. }
                | Command::ConfirmPrepared(_)
                | Command::CommitPrepared(_)
                | Command::AnswerQuestion { .. }
        ) && !self.is_ready()
        {
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

    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub fn connection(&self) -> u64 {
        self.connection.load(Ordering::Acquire)
    }

    /// Synchronous invalidation cannot be lost to the bounded command queue.
    pub fn invalidate_context(&self) {
        advance_epoch(&self.epoch);
    }

    pub fn review(&self, intent: Intent, draft_basis: String) -> Result<(), &'static str> {
        self.send(Command::Review {
            intent,
            draft_basis,
            epoch: self.epoch(),
        })
    }

    /// Serialized view fixture: no I/O, acknowledgements or invented results.
    #[cfg(feature = "test-support")]
    pub fn recording() -> Self {
        let (commands, recorded) = queue::channel(QUEUE);
        let (_, updates) = mpsc::sync_channel(QUEUE);
        let (stop, _) = watch::channel(false);
        Self {
            commands,
            updates,
            stop,
            ready: Arc::new(AtomicBool::new(true)),
            epoch: Arc::new(AtomicU64::new(0)),
            connection: Arc::new(AtomicU64::new(0)),
            recorded: Some(recorded),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn take_recorded(&mut self) -> Vec<Command> {
        let mut commands = Vec::new();
        if let Some(recorded) = &mut self.recorded {
            while let Ok(command) = recorded.try_recv() {
                commands.push(command);
            }
        }
        commands
    }
}

fn advance_epoch(epoch: &AtomicU64) {
    let mut old = epoch.load(Ordering::Acquire);
    loop {
        let next = old
            .checked_add(1)
            .expect("prospective action epoch exhausted");
        match epoch.compare_exchange_weak(old, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(current) => old = current,
        }
    }
}

#[derive(Debug)]
enum Pending {
    Ping,
    Subscribe,
    Sessions,
    Snapshot {
        session_id: String,
        generation: u64,
        questions_revision: u64,
    },
    Page(Destination),
    Action(Action),
    Forecast(Box<Preparation>),
    Consent(Box<Preparation>),
    Create,
    Answer(Action),
}

/// Transport request ids restart on reconnect. Each mutation needs a separate
/// identity so an ACK for Stop (or another connection) cannot resolve an
/// earlier Send whose outcome is still unknown.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Action {
    id: uuid::Uuid,
    session_id: String,
    question_id: Option<String>,
}

#[derive(Debug)]
enum PreparationStage {
    Forecast,
    NeedsConsent(String),
    Ready,
}

#[derive(Debug)]
struct Preparation {
    id: uuid::Uuid,
    epoch: u64,
    generation: u64,
    active: Option<String>,
    draft_basis: String,
    intent: Intent,
    stage: PreparationStage,
}

impl Preparation {
    fn current(&self, engine: &Engine) -> bool {
        engine.ready
            && self.epoch == engine.epoch
            && self.generation == engine.generation
            && self.active == engine.active
    }
}

/// Pure, connection-local correlation and invalidation state. The worker
/// replaces this on reconnect; selection generations belong to the window.
#[derive(Default)]
struct Engine {
    connection: u64,
    questions: questions::Directory,
    blocked_questions: HashSet<String>,
    question_recovery_limit: bool,
    epoch: u64,
    prepared: Option<Box<Preparation>>,
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
    fn emit_questions(&mut self) {
        let mut state = self
            .questions
            .state(self.connection, self.blocked_questions.clone());
        if self.question_recovery_limit {
            state
                .blocked
                .extend(state.open.iter().map(|question| question.id.clone()));
            state.notice =
                Some("Too many unacknowledged question answers; continue in the TUI.".into());
        }
        state.inflight = self
            .pending
            .values()
            .filter_map(|pending| match pending {
                Pending::Answer(action) => action.question_id.clone(),
                _ => None,
            })
            .collect();
        self.updates.push(Update::Questions(state));
    }
    fn blocked_by_unknown(&self, command: &Command, uncertain: &HashSet<Action>) -> bool {
        let intent = match command {
            Command::Intent(intent) | Command::Review { intent, .. } => Some(intent),
            Command::CommitPrepared(id) => self
                .prepared
                .as_ref()
                .filter(|work| work.id == *id)
                .map(|work| &work.intent),
            _ => None,
        };
        intent.is_some_and(|intent| {
            intent.starts_work()
                && uncertain.iter().any(|action| {
                    action.question_id.is_none() && action.session_id == intent.session_id() && !self.pending.values().any(|pending|
                matches!(pending, Pending::Action(pending) | Pending::Answer(pending) if pending == action))
                })
        })
    }
    fn sync_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
        let (generation, active) = (self.generation, self.active.as_ref());
        let current = |work: &Preparation| {
            work.epoch == epoch && work.generation == generation && work.active.as_ref() == active
        };
        if self.prepared.as_ref().is_some_and(|work| !current(work)) {
            self.prepared = None;
        }
        self.pending.retain(|_, pending| match pending {
            Pending::Forecast(work) | Pending::Consent(work) => current(work),
            _ => true,
        });
        self.outgoing
            .retain(|request| self.pending.contains_key(&request.id));
    }
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
        self.sync_epoch(self.epoch);
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
                        questions_revision: self.questions.revision(),
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
            Command::AnswerQuestion {
                connection,
                operation,
                question,
                answers,
            } if self.ready => {
                let error = if connection != self.connection || self.questions.get(&question.id) != Some(question.as_ref()) {
                    Some("This question is no longer current. Answers kept.".to_string())
                } else if self.question_recovery_limit || self.blocked_questions.contains(&question.id) {
                    Some("A previous answer's outcome is unknown. Do not retry; continue in the TUI.".to_string())
                } else if self.pending.values().any(|pending| matches!(pending, Pending::Answer(action) if action.question_id.as_deref() == Some(question.id.as_str()))) {
                    Some("An answer is already awaiting acknowledgement.".to_string())
                } else { question.validate_answers(&answers).err().map(str::to_string) };
                if let Some(error) = error {
                    self.updates.push(Update::QuestionAnswer {
                        connection: self.connection,
                        operation,
                        question_id: question.id,
                        outcome: questions::Outcome::Rejected(error),
                    });
                } else {
                    self.request(
                        Request::AnswerQuestion {
                            question_id: question.id.clone(),
                            answer: formless_protocol::QuestionAnswer::summary_of(&answers),
                            answers,
                        },
                        Pending::Answer(Action {
                            id: operation,
                            session_id: question.session_id,
                            question_id: Some(question.id),
                        }),
                    );
                    self.emit_questions();
                }
            }
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
            Command::Review {
                intent,
                draft_basis,
                epoch,
            } if self.ready => {
                if epoch != self.epoch || !intent.starts_work() {
                    self.updates.push(Update::Notice(
                        "The action changed before its forecast was requested. Draft kept.".into(),
                    ));
                    return;
                }
                if self.prepared.is_some() || self.pending.values().any(|pending| matches!(pending,
                    Pending::Forecast(_) | Pending::Consent(_)
                ) || matches!(pending, Pending::Action(action) if action.session_id == intent.session_id())) {
                    self.updates.push(Update::Notice("Finish or cancel the current action first. Draft kept.".into()));
                    return;
                }
                let input = forecast::input(&intent);
                let work = Preparation {
                    id: uuid::Uuid::new_v4(),
                    epoch,
                    generation: self.generation,
                    active: self.active.clone(),
                    draft_basis,
                    intent,
                    stage: PreparationStage::Forecast,
                };
                self.request(
                    Request::ForecastRun {
                        input,
                        for_send: true,
                    },
                    Pending::Forecast(Box::new(work)),
                );
            }
            Command::ConfirmPrepared(id) if self.ready => {
                let Some(work) = self.prepared.take() else {
                    return;
                };
                if work.id != id || !work.current(self) {
                    self.prepared = Some(work);
                    return;
                }
                match &work.stage {
                    PreparationStage::NeedsConsent(consent) => self.request(
                        Request::ConfirmForecast {
                            id: consent.clone(),
                        },
                        Pending::Consent(work),
                    ),
                    _ => self.prepared = Some(work),
                }
            }
            Command::CommitPrepared(id) if self.ready => {
                let Some(work) = self.prepared.take() else {
                    return;
                };
                if work.id != id
                    || !work.current(self)
                    || !matches!(work.stage, PreparationStage::Ready)
                {
                    self.prepared = Some(work);
                    return;
                }
                // The view revalidated its exact draft/IME state before this
                // commit. Neither a quote nor a consent ACK starts work itself.
                self.command(Command::Intent(work.intent));
            }
            Command::Intent(intent) if self.ready => {
                if intent.starts_work() && self.pending.values().any(|pending|
                    matches!(pending, Pending::Action(action) if action.session_id == intent.session_id())) {
                    self.updates.push(Update::Notice("Wait for the previous action's acknowledgement; the draft was kept.".into()));
                    return;
                }
                let action_id = uuid::Uuid::new_v4();
                let (session_id, request) = match intent {
                    Intent::SendMessage {
                        session_id,
                        content,
                        mode,
                    } => (
                        session_id.clone(),
                        Request::SendMessage {
                            session_id,
                            content,
                            mode,
                            client_msg_id: Some(action_id.to_string()),
                        },
                    ),
                    Intent::InterruptRun { session_id } => {
                        (session_id.clone(), Request::InterruptRun { session_id })
                    }
                    Intent::ResumeRun { session_id } => {
                        (session_id.clone(), Request::ResumeRun { session_id })
                    }
                    Intent::AbandonRun { session_id } => {
                        (session_id.clone(), Request::AbandonRun { session_id })
                    }
                };
                self.request(
                    request,
                    Pending::Action(Action {
                        id: action_id,
                        session_id,
                        question_id: None,
                    }),
                );
            }
            Command::Intent(_)
            | Command::CreateSession
            | Command::Review { .. }
            | Command::ConfirmPrepared(_)
            | Command::CommitPrepared(_) => self.updates.push(Update::Notice(
                "Not connected; the action was not sent. The draft was kept.".into(),
            )),
            Command::AnswerQuestion {
                operation,
                question,
                ..
            } => self.updates.push(Update::QuestionAnswer {
                connection: self.connection,
                operation,
                question_id: question.id,
                outcome: questions::Outcome::Rejected("Not connected; no answer was sent.".into()),
            }),
        }
    }

    fn receive(&mut self, message: ServerMessage) -> Result<(), String> {
        let (id, result, response_ok) = match message {
            ServerMessage::Event(event) => {
                if matches!(
                    event,
                    Event::UserQuestion { .. } | Event::UserQuestionClosed { .. }
                ) {
                    self.questions.event(&event);
                    self.emit_questions();
                }
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
                ok,
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
                self.emit_questions();
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
                questions_revision,
            } => {
                if let Ok(value) = &result {
                    if let Some(events) = value.get("pending_events").and_then(Value::as_array) {
                        if self.questions.snapshot(events, questions_revision) {
                            self.emit_questions();
                        } else if questions_revision != self.questions.revision() {
                            self.dirty_session = true;
                        }
                    }
                }
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
            Pending::Answer(action) => {
                let question_id = action.question_id.expect("question action has identity");
                let outcome = match result {
                    Ok(value) if value["answered"] == true => {
                        self.questions.close(&question_id);
                        questions::Outcome::Accepted
                    }
                    Ok(value) if value["answered"] == false => {
                        self.questions.close(&question_id);
                        questions::Outcome::Closed
                    }
                    Ok(_) => {
                        self.blocked_questions.insert(question_id.clone());
                        questions::Outcome::Unknown
                    }
                    Err(_) if response_ok => {
                        self.blocked_questions.insert(question_id.clone());
                        questions::Outcome::Unknown
                    }
                    Err(error) => questions::Outcome::Rejected(error),
                };
                self.updates.push(Update::QuestionAnswer {
                    connection: self.connection,
                    operation: action.id,
                    question_id,
                    outcome,
                });
                self.emit_questions();
                self.dirty_session = true;
            }
            Pending::Forecast(mut work) => {
                if !work.current(self) {
                    return Ok(());
                }
                let quote = result
                    .and_then(|value| {
                        serde_json::from_value::<formless_protocol::RunForecast>(value)
                            .map_err(|error| format!("Could not read the daemon forecast: {error}"))
                    })
                    .and_then(|quote| {
                        forecast::validate(work.intent.session_id(), &quote)?;
                        Ok(quote)
                    });
                match quote {
                    Ok(quote) => {
                        work.stage = if quote.confirmation_required {
                            PreparationStage::NeedsConsent(
                                quote.confirmation_id.clone().expect("validated consent id"),
                            )
                        } else {
                            PreparationStage::Ready
                        };
                        self.updates
                            .push(Update::Prepared(Box::new(forecast::Prepared {
                                id: work.id,
                                epoch: work.epoch,
                                generation: work.generation,
                                session_id: work.intent.session_id().into(),
                                label: forecast::label(&work.intent),
                                draft_basis: work.draft_basis.clone(),
                                quote,
                            })));
                        self.prepared = Some(work);
                    }
                    Err(error) => self.updates.push(Update::PreparationFailed {
                        id: work.id,
                        epoch: work.epoch,
                        error,
                    }),
                }
            }
            Pending::Consent(mut work) => {
                if !work.current(self) {
                    return Ok(());
                }
                match result.and_then(|value| if value["confirmed"] == true { Ok(()) }
                    else { Err("The daemon did not acknowledge this forecast confirmation. Nothing was started.".into()) }) {
                    Ok(()) => {
                        work.stage = PreparationStage::Ready;
                        self.updates.push(Update::PreparationReady { id: work.id, epoch: work.epoch });
                        self.prepared = Some(work);
                    }
                    Err(error) => self.updates.push(Update::PreparationFailed { id: work.id, epoch: work.epoch, error }),
                }
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

struct ConnectionState<'a> {
    ready: &'a AtomicBool,
    epoch: &'a AtomicU64,
    connection: &'a AtomicU64,
}

async fn connected<S: tokio::io::AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    commands: &mut queue::Receiver<Command>,
    tx: &SyncSender<Update>,
    stop: &mut watch::Receiver<bool>,
    retained: &mut Retained,
    state: ConnectionState<'_>,
) -> Result<(), String> {
    let ConnectionState {
        ready,
        epoch,
        connection,
    } = state;
    let Retained {
        active,
        page,
        uncertain,
    } = retained;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = Frames::new(BufReader::new(reader));
    let mut engine = Engine {
        connection: connection.load(Ordering::Acquire),
        blocked_questions: uncertain
            .iter()
            .filter_map(|action| action.question_id.clone())
            .collect(),
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
        engine.sync_epoch(epoch.load(Ordering::Acquire));
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
            if let Pending::Action(action) | Pending::Answer(action) = p {
                Some(action.clone())
            } else {
                None
            }
        }));
        let limit = uncertain
            .iter()
            .filter(|action| action.question_id.is_some())
            .count()
            >= 512;
        if engine.question_recovery_limit != limit {
            engine.question_recovery_limit = limit;
            engine.emit_questions();
        }
        write_requests(&mut writer, &mut engine)
            .await
            .map_err(|e| e.to_string())?;
        deliver(tx, &mut engine)?;
        tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            command = commands.recv() => { engine.sync_epoch(epoch.load(Ordering::Acquire)); match command {
                None => return Ok(()),
                Some(Command::Reconnect) => return Err("Reconnecting at the user's request; actions are not retried.".into()),
                Some(Command::Select { session_id, generation }) => { *active = (session_id.clone(), generation); engine.select(session_id, generation); }
                Some(Command::Page(destination)) => { *page = Some(destination); engine.load_page(destination); }
                Some(command) if engine.blocked_by_unknown(&command, uncertain) => {
                    engine.updates.push(Update::Notice("A previous action's outcome is unknown. It was not retried: deduplication does not survive reconnect. Check this Session in the TUI before starting more work.".into()));
                }
                Some(command) => engine.command(command),
            } },
            message = reader.next() => match message.map_err(|e| e.to_string())? {
                Some(message) => {
                    engine.sync_epoch(epoch.load(Ordering::Acquire));
                    let answered = match &message { ServerMessage::Response { id, ok, result, .. } => match engine.pending.get(id) {
                        Some(Pending::Action(action)) => Some(action.clone()),
                        Some(Pending::Answer(action)) if !ok || result.as_ref().and_then(|value| value["answered"].as_bool()).is_some() => Some(action.clone()),
                        _ => None }, _ => None };
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
    epoch: Arc<AtomicU64>,
    connection: Arc<AtomicU64>,
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
                Ok(Ok(stream)) => connected(stream, &mut commands, &tx, &mut stop, &mut retained, ConnectionState { ready:&ready,epoch:&epoch,connection:&connection }).await,
                Ok(Err(error)) => Err(format!("Cannot connect: {error}. Start the existing daemon with the CLI.")),
                Err(error) => Err(error.to_string()),
            }
        };
        ready.store(false, Ordering::Release);
        advance_epoch(&epoch);
        advance_epoch(&connection);
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

    fn question(id: &str) -> formless_protocol::Event {
        Event::UserQuestion {
            question_id: id.into(),
            session_id: "a".into(),
            question: "Which option ไทย?".into(),
            options: vec!["one".into(), "two".into()],
            questions: vec![],
        }
    }

    fn answer(open: &questions::Open) -> Command {
        Command::AnswerQuestion {
            connection: 0,
            operation: uuid::Uuid::new_v4(),
            question: Box::new(open.clone()),
            answers: vec![formless_protocol::QuestionAnswer {
                selected: vec!["two".into()],
                text: Some("Own answer 😀".into()),
            }],
        }
    }

    #[test]
    fn question_identity_survives_navigation_and_requires_a_specific_positive_ack() {
        for acknowledged in [false, true] {
            let mut engine = ready();
            let event = question("q");
            let open = questions::Open::from_event(&event).unwrap();
            engine.receive(ServerMessage::Event(event)).unwrap();
            engine.select(Some("different-session".into()), 9);
            engine.outgoing.clear();
            engine.command(answer(&open));
            let request = engine.outgoing.pop_back().unwrap();
            assert!(
                matches!(&request.request,Request::AnswerQuestion { question_id,answers,.. } if question_id=="q" && answers[0].selected==["two"] && answers[0].text.as_deref()==Some("Own answer 😀"))
            );
            engine.command(answer(&open));
            assert!(
                engine.outgoing.is_empty(),
                "no repeated mutation while pending"
            );
            respond(&mut engine, request.id, json!({"answered":acknowledged}));
            let outcome = engine
                .updates
                .iter()
                .rev()
                .find_map(|update| match update {
                    Update::QuestionAnswer { outcome, .. } => Some(outcome),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                (
                    matches!(outcome, questions::Outcome::Accepted),
                    matches!(outcome, questions::Outcome::Closed)
                ),
                (acknowledged, !acknowledged)
            );
            assert!(engine.questions.get("q").is_none());
            engine.command(answer(&open));
            assert!(
                engine.outgoing.is_empty(),
                "closed identity cannot be answered again"
            );
        }
    }

    #[test]
    fn malformed_answer_ack_is_unknown_and_a_retired_snapshot_cannot_revive_a_closed_question() {
        for payload in [Some(json!({"unexpected":true})), None] {
            let mut engine = ready();
            let event = question("q");
            let open = questions::Open::from_event(&event).unwrap();
            engine.receive(ServerMessage::Event(event.clone())).unwrap();
            engine.command(answer(&open));
            let request = engine.outgoing.pop_back().unwrap();
            engine
                .receive(ServerMessage::Response {
                    id: request.id,
                    ok: true,
                    result: payload,
                    error: None,
                })
                .unwrap();
            assert!(engine.blocked_questions.contains("q"));
            assert!(engine.updates.iter().any(|update| matches!(
                update,
                Update::QuestionAnswer {
                    outcome: questions::Outcome::Unknown,
                    ..
                }
            )));
            engine.command(answer(&open));
            assert!(engine.outgoing.is_empty());
        }
        let mut engine = ready();
        engine.select(Some("a".into()), 1);
        let request = snapshot_request(&mut engine);
        let event = question("q");
        engine.receive(ServerMessage::Event(event.clone())).unwrap();
        engine
            .receive(ServerMessage::Event(Event::UserQuestionClosed {
                question_id: "q".into(),
            }))
            .unwrap();
        let mut old = snapshot("a", "canonical");
        old["pending_events"] = json!([event]);
        respond(&mut engine, request, old);
        assert!(engine.questions.get("q").is_none());
        assert!(
            engine.dirty_session,
            "profile questions are refreshed after observed read overlap"
        );
    }

    fn prepare(engine: &mut Engine, required: bool) -> uuid::Uuid {
        engine.command(Command::Review {
            intent: Intent::SendMessage {
                session_id: "a".into(),
                content: "keep exact draft ไทย".into(),
                mode: SendMode::Queue,
            },
            draft_basis: "keep exact draft ไทย".into(),
            epoch: engine.epoch,
        });
        let request = engine.outgoing.pop_back().unwrap();
        assert!(matches!(
            request.request,
            Request::ForecastRun { for_send: true, .. }
        ));
        respond(
            engine,
            request.id,
            serde_json::to_value(crate::fixture::run_forecast("a", required)).unwrap(),
        );
        engine
            .updates
            .iter()
            .find_map(|update| match update {
                Update::Prepared(prepared) => Some(prepared.id),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn forecast_and_consent_replies_never_start_work_without_a_current_view_commit() {
        for required in [false, true] {
            let mut engine = ready();
            let id = prepare(&mut engine, required);
            assert!(engine.outgoing.is_empty(), "a quote must not send");
            if required {
                engine.command(Command::CommitPrepared(id));
                assert!(engine.outgoing.is_empty(), "consent cannot be skipped");
                engine.command(Command::ConfirmPrepared(id));
                let request = engine.outgoing.pop_back().unwrap();
                assert!(matches!(request.request, Request::ConfirmForecast { .. }));
                respond(&mut engine, request.id, json!({"confirmed":true}));
                assert!(engine.outgoing.is_empty(), "a consent ACK must not send");
                assert!(engine.updates.iter().any(|update| matches!(update, Update::PreparationReady { id: got, .. } if *got == id)));
            }
            engine.command(Command::CommitPrepared(uuid::Uuid::new_v4()));
            assert!(engine.outgoing.is_empty());
            engine.command(Command::CommitPrepared(id));
            engine.command(Command::CommitPrepared(id));
            assert_eq!(
                engine.outgoing.len(),
                1,
                "duplicate commits must not replay"
            );
            assert!(
                matches!(&engine.outgoing[0].request, Request::SendMessage { content, mode: SendMode::Queue, .. } if content == "keep exact draft ไทย")
            );
        }
    }

    #[test]
    fn changed_draft_selection_or_connection_retires_quotes_and_late_consent() {
        for navigation in [false, true] {
            let mut engine = ready();
            let id = prepare(&mut engine, true);
            engine.command(Command::ConfirmPrepared(id));
            let confirm = engine.outgoing.pop_back().unwrap();
            engine.updates.clear();
            if navigation {
                engine.select(Some("b".into()), 2);
                engine.select(Some("a".into()), 3);
            } else {
                engine.sync_epoch(1);
            }
            respond(&mut engine, confirm.id, json!({"confirmed":true}));
            engine.command(Command::CommitPrepared(id));
            assert!(!engine
                .outgoing
                .iter()
                .any(|request| matches!(request.request, Request::SendMessage { .. })));
            assert!(!engine
                .updates
                .iter()
                .any(|update| matches!(update, Update::PreparationReady { .. })));
        }
        let mut old = ready();
        let id = prepare(&mut old, false);
        let mut reconnected = ready();
        reconnected.command(Command::CommitPrepared(id));
        assert!(
            reconnected.outgoing.is_empty(),
            "connection-local preparation cannot survive reconnect"
        );
    }

    #[test]
    fn malformed_forecasts_and_rejected_consent_keep_the_action_uncommitted() {
        let mut engine = ready();
        engine.command(Command::Review {
            intent: Intent::ResumeRun {
                session_id: "a".into(),
            },
            draft_basis: "retained".into(),
            epoch: 0,
        });
        let request = engine.outgoing.pop_back().unwrap();
        assert!(
            matches!(request.request, Request::ForecastRun { input, .. } if input.resume && input.draft.is_empty())
        );
        respond(
            &mut engine,
            request.id,
            serde_json::to_value(crate::fixture::run_forecast("foreign", true)).unwrap(),
        );
        assert!(engine.prepared.is_none());
        assert!(engine
            .updates
            .iter()
            .any(|update| matches!(update, Update::PreparationFailed { .. })));
        engine.updates.clear();
        let id = prepare(&mut engine, true);
        engine.command(Command::ConfirmPrepared(id));
        let confirm = engine.outgoing.pop_back().unwrap();
        engine
            .receive(ServerMessage::err(confirm.id, "quote changed".into()))
            .unwrap();
        engine.command(Command::CommitPrepared(id));
        assert!(engine.outgoing.is_empty());
    }

    #[test]
    fn unknown_mutations_also_block_reviewed_work_and_prepared_commits() {
        let mut engine = ready();
        let id = prepare(&mut engine, false);
        let uncertain = HashSet::from([Action {
            id: uuid::Uuid::new_v4(),
            session_id: "a".into(),
            question_id: None,
        }]);
        assert!(engine.blocked_by_unknown(&Command::CommitPrepared(id), &uncertain));
        assert!(engine.blocked_by_unknown(
            &Command::Review {
                intent: Intent::ResumeRun {
                    session_id: "a".into()
                },
                draft_basis: String::new(),
                epoch: 0
            },
            &uncertain
        ));
        engine.sync_epoch(1);
        assert!(engine.prepared.is_none());
        assert!(engine.pending.is_empty());
    }

    #[test]
    fn cancelling_a_quote_does_not_depend_on_command_queue_capacity() {
        let (commands, _receiver) = queue::channel(QUEUE);
        let (_, updates) = mpsc::sync_channel(QUEUE);
        let (stop, _) = watch::channel(false);
        let client = Client {
            commands,
            updates,
            stop,
            ready: Arc::new(AtomicBool::new(true)),
            epoch: Arc::new(AtomicU64::new(0)),
            connection: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "test-support")]
            recorded: None,
        };
        let mut engine = ready();
        let id = prepare(&mut engine, false);
        for _ in 0..QUEUE {
            client.send(Command::Refresh).unwrap();
        }
        assert!(client.send(Command::ConfirmPrepared(id)).is_err());
        client.invalidate_context();
        engine.sync_epoch(client.epoch());
        engine.command(Command::CommitPrepared(id));
        assert!(
            engine.outgoing.is_empty(),
            "full queue cannot preserve a cancelled action"
        );
    }

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
        let epoch = AtomicU64::new(0);
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
                ConnectionState {
                    ready: &ready,
                    epoch: &epoch,
                    connection: &AtomicU64::new(0),
                },
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
    async fn unknown_question_answer_survives_reconnect_stop_and_an_unrelated_question_ack() {
        let mut retained = disconnect_after_unacknowledged_send().await;
        let original_send = retained.uncertain.clone();
        for reconnect in [false, true] {
            let before = retained.uncertain.clone();
            let (stream, server) = tokio::io::duplex(16384);
            let (command_tx, mut commands) = queue::channel(QUEUE);
            let _keep_commands = command_tx.clone();
            let (updates, rx) = mpsc::sync_channel(QUEUE);
            let (_stop, mut cancelled) = watch::channel(false);
            let ready = AtomicBool::new(false);
            let epoch = AtomicU64::new(0);
            let connection = AtomicU64::new(0);
            let peer = tokio::spawn(async move {
                let mut server = BufReader::new(server);
                loop {
                    let mut line = String::new();
                    assert_ne!(server.read_line(&mut line).await.unwrap(), 0);
                    let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                    let mut events = Vec::new();
                    let mut finish = false;
                    let result = match request.request {
                        Request::Ping => json!({"min_protocol":1,"max_protocol":1}),
                        Request::Subscribe { .. } => {
                            events.push(question("q1"));
                            if reconnect {
                                events.push(question("q2"));
                            }
                            json!({"subscribed":true})
                        }
                        Request::ListSessions => json!([]),
                        Request::GetSession { id } => snapshot(&id, "canonical"),
                        Request::AnswerQuestion { question_id, .. } => {
                            if !reconnect {
                                assert_eq!(question_id, "q1");
                                break;
                            }
                            assert_eq!(
                                question_id, "q2",
                                "the unknown q1 answer must not be retried"
                            );
                            finish = true;
                            json!({"answered":true})
                        }
                        Request::InterruptRun { .. } if reconnect => json!({"interrupted":true}),
                        Request::SendMessage { .. } => {
                            panic!("an unrelated answer ACK cleared the unknown Send")
                        }
                        _ => panic!("unexpected fixture request"),
                    };
                    let mut frame =
                        serde_json::to_vec(&ServerMessage::ok(request.id, result)).unwrap();
                    frame.push(b'\n');
                    server.get_mut().write_all(&frame).await.unwrap();
                    for event in events {
                        let mut frame = serde_json::to_vec(&ServerMessage::Event(event)).unwrap();
                        frame.push(b'\n');
                        server.get_mut().write_all(&frame).await.unwrap();
                    }
                    if finish {
                        break;
                    }
                }
            });
            let driver = tokio::spawn(async move {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    for update in rx.try_iter() {
                        if let Update::Questions(state) = update {
                            if state
                                .open
                                .iter()
                                .any(|q| q.id == if reconnect { "q2" } else { "q1" })
                            {
                                command_tx
                                    .try_send(answer(
                                        &questions::Open::from_event(&question("q1")).unwrap(),
                                    ))
                                    .unwrap();
                                if reconnect {
                                    command_tx
                                        .try_send(Command::Intent(Intent::SendMessage {
                                            session_id: "a".into(),
                                            content: "must stay blocked".into(),
                                            mode: SendMode::Send,
                                        }))
                                        .unwrap();
                                    command_tx
                                        .try_send(Command::Intent(Intent::InterruptRun {
                                            session_id: "a".into(),
                                        }))
                                        .unwrap();
                                    command_tx
                                        .try_send(answer(
                                            &questions::Open::from_event(&question("q2")).unwrap(),
                                        ))
                                        .unwrap();
                                }
                                return rx;
                            }
                        }
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "question inventory never arrived"
                    );
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                connected(
                    stream,
                    &mut commands,
                    &updates,
                    &mut cancelled,
                    &mut retained,
                    ConnectionState {
                        ready: &ready,
                        epoch: &epoch,
                        connection: &connection,
                    },
                ),
            )
            .await
            .unwrap();
            peer.await.unwrap();
            let _receiver = driver.await.unwrap();
            assert!(result.unwrap_err().contains("disconnected"));
            if reconnect {
                assert_eq!(
                    retained.uncertain, before,
                    "Stop and q2 ACKs cannot clear either unknown operation"
                );
            } else {
                assert_eq!(retained.uncertain.len(), 2);
                assert!(retained
                    .uncertain
                    .iter()
                    .any(|action| action.question_id.as_deref() == Some("q1")));
            }
            assert!(original_send.is_subset(&retained.uncertain));
        }
    }

    #[tokio::test]
    async fn reconnect_stop_acknowledgements_do_not_clear_an_unknown_send() {
        let mut retained = disconnect_after_unacknowledged_send().await;
        let original_unknown = retained.uncertain.clone();
        for (stop_succeeded, resume) in [(true, false), (false, false), (true, true), (false, true)]
        {
            let (stream, server) = tokio::io::duplex(8192);
            let (command_tx, mut commands) = queue::channel(QUEUE);
            let _keep_commands_open = command_tx.clone();
            let (updates, rx) = mpsc::sync_channel(QUEUE);
            let (_stop, mut cancelled) = watch::channel(false);
            let ready = AtomicBool::new(false);
            let epoch = AtomicU64::new(0);
            let peer = tokio::spawn(async move {
                let retry = || {
                    Command::Intent(if resume {
                        Intent::ResumeRun {
                            session_id: "a".into(),
                        }
                    } else {
                        Intent::SendMessage {
                            session_id: "a".into(),
                            content: "one action".into(),
                            mode: SendMode::Send,
                        }
                    })
                };
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
                            command_tx.send(retry()).await.unwrap();
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
                            command_tx.send(retry()).await.unwrap();
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
                    ConnectionState {
                        ready: &ready,
                        epoch: &epoch,
                        connection: &AtomicU64::new(0),
                    },
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
