//! Local daemon client. Domain state stays in the out-of-process daemon.
//!
//! Ordering contract: the existing protocol orders frames, but has no
//! snapshot watermark or delta offset in its legacy mode. The production
//! constructor subscribes with `deltas: false`, treats events as invalidations, and installs only
//! correlated canonical snapshots. It never replays events onto a snapshot.
//! A relevant event during a read schedules another read, including when the
//! first reply already contained it. This is deliberately conservative.
//! A separate validation-only constructor negotiates the opt-in future-attempt
//! contract, stages owned frames and emits tagged view batches. It is not a
//! production preference or CLI switch.
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

pub mod attachments;
mod delivery;
#[cfg(test)]
mod native_send_tests;
mod reader;
mod run_actions;
mod send_review;
#[cfg(test)]
mod topic_tests;
pub use reader::{StreamBatch, StreamOwner, StreamStatus};

use crate::forecast;
use crate::model::{Destination, Intent};
use crate::profile_search;
use crate::questions;
use crate::replies::{self, SessionSnapshot};
use crate::session_edit;
use crate::session_export;
use crate::session_fork;
use crate::session_usage;

const QUEUE: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH: Duration = Duration::from_millis(250);

/// Visible v1 recovery contract, including compatibility fallback.
pub const FUTURE_ATTEMPT_LIMITATION: &str = "Attaching or reconnecting mid-answer shows persisted history until the answer completes or the next eligible attempt. Older daemons use persisted history.";
pub const FUTURE_ATTEMPT_STATUS: &str =
    "Connected · future-attempt streaming · persisted-history recovery";

#[derive(Debug)]
pub enum Update {
    TopicSupport(bool),
    Topics {
        connection: u64,
        operation: Option<uuid::Uuid>,
        result: Result<crate::topics::Reply, String>,
    },
    TopicCandidates {
        generation: u64,
        session_id: String,
        messages: Vec<crate::topics::Candidate>,
    },
    AttachmentStaged(attachments::Outcome),
    DeleteUndo(crate::run_actions::DeleteUndo),
    RunActionTarget {
        owner: crate::run_actions::Owner,
        operation: uuid::Uuid,
        result: Result<crate::run_actions::Target, String>,
    },
    RunActionReview {
        owner: crate::run_actions::Owner,
        operation: uuid::Uuid,
        quote: Option<formless_protocol::RunForecast>,
        reviewed_text: Option<String>,
    },
    RunActionReady {
        owner: crate::run_actions::Owner,
        operation: uuid::Uuid,
    },
    LiveContext {
        owner: crate::run_actions::Owner,
        result: Result<crate::run_actions::Context, String>,
    },
    RunActivity {
        owner: crate::run_actions::Owner,
        event: Event,
    },
    LiveOverlay {
        client: uuid::Uuid,
        connection: u64,
        generation: u64,
        session_id: String,
        view: crate::live_overlay::View,
    },
    Transcript(StreamBatch),
    ApprovalInventory {
        client: uuid::Uuid,
        connection: u64,
        operation: uuid::Uuid,
        revision: u64,
        result: Result<crate::approvals::Inventory, String>,
    },
    ApprovalInvalidated {
        client: uuid::Uuid,
        connection: u64,
        revision: u64,
    },
    SessionStatusInvalidated {
        client: uuid::Uuid,
        connection: u64,
        generation: u64,
        session_id: String,
    },
    Invocations {
        connection: u64,
        operation: uuid::Uuid,
        result: Result<crate::invocations::Report, String>,
    },
    RunInspection {
        connection: u64,
        operation: uuid::Uuid,
        target: crate::run_inspection::Target,
        result: Result<crate::run_inspection::Report, String>,
    },
    SessionComparison {
        connection: u64,
        operation: uuid::Uuid,
        pair: crate::comparison::Pair,
        result: Result<crate::comparison::Report, String>,
    },
    SessionUsage {
        target: session_usage::Target,
        operation: uuid::Uuid,
        result: Result<session_usage::Report, String>,
    },
    SessionExport {
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
        format: session_export::Format,
        result: Result<session_export::Document, String>,
    },
    ProfileSearch {
        connection: u64,
        operation: uuid::Uuid,
        query: String,
        result: Result<Vec<formless_protocol::SearchHit>, String>,
    },
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
    SessionEdited {
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
        outcome: session_edit::Outcome,
    },
    PendingAttachments {
        client: uuid::Uuid,
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
        result: Result<crate::pending_files::Report, String>,
    },
    ModelCatalog {
        instance: uuid::Uuid,
        connection: u64,
        operation: uuid::Uuid,
        result: Result<crate::model_catalog::Catalog, String>,
    },
    SessionDetails {
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
        result: Result<session_edit::Metadata, String>,
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
    ResourceAdded {
        destination: Destination,
        text: String,
        schedule: String,
    },
    ManagementNotice {
        destination: Destination,
        notice: String,
    },
}

pub enum Command {
    Topic {
        connection: u64,
        operation: Option<uuid::Uuid>,
        action: crate::topics::Action,
    },
    Manage {
        destination: Destination,
        request: Request,
        label: String,
    },
    StageAttachments(attachments::Operation),
    UndoDelete(uuid::Uuid),
    ReadRunAction {
        owner: crate::run_actions::Owner,
        operation: uuid::Uuid,
    },
    PrepareRunAction(crate::run_actions::Dispatch),
    ConfirmRunAction(uuid::Uuid),
    CommitRunAction(uuid::Uuid),
    DetachReference(crate::reference_detach::Dispatch),
    ReadPendingAttachments {
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
    },
    ReadModelCatalog {
        connection: u64,
        operation: uuid::Uuid,
    },
    DecideStep(crate::step_decision::Dispatch),
    EnableStepMode(crate::step_mode::Dispatch),
    DisableStepMode(crate::step_mode::Dispatch),
    ReadApprovals {
        connection: u64,
        operation: uuid::Uuid,
    },
    DecideApproval(crate::approvals::Dispatch),
    ReadInvocations {
        connection: u64,
        operation: uuid::Uuid,
    },
    ForkSession(session_fork::Action),
    InspectRun {
        connection: u64,
        operation: uuid::Uuid,
        target: crate::run_inspection::Target,
    },
    CompareSessions {
        connection: u64,
        operation: uuid::Uuid,
        pair: crate::comparison::Pair,
    },
    ReadSessionUsage {
        target: session_usage::Target,
        operation: uuid::Uuid,
    },
    ExportSession {
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
        format: session_export::Format,
    },
    SearchProfile {
        connection: u64,
        operation: uuid::Uuid,
        query: String,
    },
    Reconnect,
    Select {
        session_id: Option<String>,
        generation: u64,
    },
    Page(Destination),
    Intent(Intent),
    Review {
        intent: Intent,
        expected_run_id: Option<String>,
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
    EditSession {
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
        change: session_edit::Change,
    },
    ReadSessionDetails {
        connection: u64,
        operation: uuid::Uuid,
        session_id: String,
    },
}

/// An explicit local socket/pipe path avoids duplicating Profile discovery
/// or opening configuration/credentials in the presentation process.
#[derive(Clone, Debug)]
pub struct Endpoint(pub PathBuf);

pub struct Client {
    reference_ledger: Arc<crate::reference_detach::Ledger>,
    step_ledger: Arc<crate::step_decision::Ledger>,
    mode_ledger: Arc<crate::step_mode::Ledger>,
    approval_ledger: Arc<crate::approvals::Ledger>,
    commands: queue::Sender<Command>,
    updates: Receiver<Update>,
    stop: watch::Sender<bool>,
    ready: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    connection: Arc<AtomicU64>,
    forks: Arc<session_fork::Ledger>,
    stream_epoch: Arc<AtomicU64>,
    instance: uuid::Uuid,
    future_attempts: bool,
    #[cfg(feature = "test-support")]
    recorded: Option<queue::Receiver<Command>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.invalidate_context();
        advance_epoch(&self.stream_epoch);
        self.mode_ledger
            .cancel_unsent("Client closed before writing; no mode setting was sent.");
        let _ = self.stop.send(true);
    }
}

impl Client {
    pub fn start(endpoint: Endpoint) -> Self {
        Self::start_policy(endpoint, false)
    }
    /// Validation only. No CLI, preference or production constructor enables it.
    #[cfg(any(test, feature = "daemon-tests"))]
    pub fn start_future_attempts_for_validation(endpoint: Endpoint) -> Self {
        Self::start_policy(endpoint, true)
    }
    fn start_policy(endpoint: Endpoint, future_attempts: bool) -> Self {
        let (commands, rx) = queue::channel(QUEUE);
        let (tx, updates) = mpsc::sync_channel(QUEUE);
        let (stop, cancelled) = watch::channel(false);
        let ready = Arc::new(AtomicBool::new(false));
        let worker_ready = ready.clone();
        let epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = epoch.clone();
        let connection = Arc::new(AtomicU64::new(0));
        let worker_connection = connection.clone();
        let stream_epoch = Arc::new(AtomicU64::new(0));
        let worker_stream_epoch = stream_epoch.clone();
        let instance = uuid::Uuid::new_v4();
        let forks = Arc::new(session_fork::Ledger::default());
        let worker_forks = forks.clone();
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
                    WorkerState {
                        ready: worker_ready,
                        epoch: worker_epoch,
                        connection: worker_connection,
                        forks: worker_forks,
                        stream_epoch: worker_stream_epoch,
                        future_attempts,
                        instance,
                    },
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
            reference_ledger: Arc::new(crate::reference_detach::Ledger::default()),
            step_ledger: Arc::new(crate::step_decision::Ledger::default()),
            mode_ledger: Arc::new(crate::step_mode::Ledger::default()),
            approval_ledger: Arc::new(crate::approvals::Ledger::default()),
            commands,
            updates,
            stop,
            ready,
            epoch,
            connection,
            forks,
            stream_epoch,
            future_attempts,
            instance,
            #[cfg(feature = "test-support")]
            recorded: None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn record_step_write(&self, dispatch: &crate::step_decision::Dispatch) -> bool {
        assert!(self.recorded.is_some());
        assert!(dispatch.owned_by(&self.step_ledger));
        dispatch.before_write()
    }
    #[cfg(feature = "test-support")]
    pub fn record_step_outcome(
        &self,
        dispatch: &crate::step_decision::Dispatch,
        value: Value,
        response_ok: bool,
    ) {
        assert!(self.recorded.is_some());
        assert!(dispatch.owned_by(&self.step_ledger));
        dispatch.finish(crate::step_decision::decode(
            dispatch.action(),
            Ok(value),
            response_ok,
        ));
    }
    pub fn step_status(&self) -> crate::step_decision::Status {
        self.step_ledger.status()
    }
    pub fn cancel_unsent_step(&self) {
        self.step_ledger
            .cancel_unsent("Step review closed before writing; no decision was sent.");
    }
    pub fn decide_step(&self, action: crate::step_decision::Action) -> Result<(), &'static str> {
        if !self.is_ready()
            || action.client != self.instance()
            || action.connection != self.connection()
            || action.epoch != self.epoch()
        {
            return Err("Connection or review changed; no decision was queued.");
        }
        self.send(Command::DecideStep(self.step_ledger.reserve(action)?))
    }
    #[cfg(feature = "test-support")]
    pub fn finish_recorded_mode(
        &self,
        dispatch: &crate::step_mode::Dispatch,
        outcome: crate::step_mode::Outcome,
    ) {
        assert!(self.recorded.is_some() && dispatch.owned_by(&self.mode_ledger));
        assert!(dispatch.before_write());
        dispatch.finish(outcome);
    }
    pub fn step_mode_status(&self) -> crate::step_mode::Status {
        self.mode_ledger.status()
    }
    pub fn enable_step_mode(&self, action: crate::step_mode::Action) -> Result<(), &'static str> {
        self.queue_mode_change(action, true)
    }
    pub fn disable_step_mode(&self, action: crate::step_mode::Action) -> Result<(), &'static str> {
        self.queue_mode_change(action, false)
    }
    fn queue_mode_change(
        &self,
        action: crate::step_mode::Action,
        on: bool,
    ) -> Result<(), &'static str> {
        if action.on != on
            || !self.is_ready()
            || action.client != self.instance()
            || action.connection != self.connection()
            || action.view_epoch != self.stream_epoch()
        {
            return Err(
                "Session view, mode target or connection changed; no mode change was queued.",
            );
        }
        let dispatch = self.mode_ledger.reserve(action)?;
        self.send(if on {
            Command::EnableStepMode(dispatch)
        } else {
            Command::DisableStepMode(dispatch)
        })
    }
    #[cfg(feature = "test-support")]
    pub fn finish_recorded_approval(&self, d: &crate::approvals::Dispatch, value: Value) {
        assert!(self.recorded.is_some() && d.owned_by(&self.approval_ledger));
        assert!(d.before_write());
        d.finish(crate::approvals::decode(Ok(value), true));
    }
    pub fn approval_status(&self) -> crate::approvals::Status {
        self.approval_ledger.status()
    }
    pub fn cancel_unsent_approval(&self) {
        self.approval_ledger
            .cancel_unsent("Approval review closed before writing; no decision was sent.");
    }
    pub fn decide_approval(&self, action: crate::approvals::Action) -> Result<(), &'static str> {
        if !self.is_ready()
            || action.client != self.instance()
            || action.connection != self.connection()
            || action.epoch != self.epoch()
        {
            return Err("Profile connection or review changed; no approval decision was queued.");
        }
        self.send(Command::DecideApproval(
            self.approval_ledger.reserve(action)?,
        ))
    }
    pub fn reference_status(&self) -> crate::reference_detach::Status {
        self.reference_ledger.status()
    }
    pub fn cancel_unsent_reference(&self) {
        self.reference_ledger
            .cancel_unsent("Reference review changed before writing; nothing was removed.");
    }
    pub fn detach_reference(
        &self,
        action: crate::reference_detach::Action,
    ) -> Result<(), &'static str> {
        if !self.is_ready()
            || action.client != self.instance()
            || action.connection != self.connection()
            || action.epoch != self.epoch()
        {
            return Err("Reference review or connection changed; no removal was queued.");
        }
        self.send(Command::DetachReference(
            self.reference_ledger.reserve(action)?,
        ))
    }
    #[cfg(feature = "test-support")]
    pub fn finish_recorded_reference(
        &self,
        d: &crate::reference_detach::Dispatch,
        result: Result<Value, String>,
        ok: bool,
    ) {
        assert!(self.recorded.is_some() && d.owned_by(&self.reference_ledger));
        assert!(d.before_write());
        d.finish(crate::reference_detach::decode(d.action(), result, ok));
    }
    pub fn send(&self, command: Command) -> Result<(), &'static str> {
        if let Command::PrepareRunAction(d) = &command {
            let owner = &d.action.owner;
            if !self.is_ready()
                || owner.client != self.instance()
                || owner.connection != self.connection()
                || owner.epoch != self.epoch()
                || !d.reserved()
            {
                d.reject("Connection or review changed; nothing was queued.");
                return Err("Connection or review changed; nothing was queued.");
            }
        }
        let reference = self.reference_status();
        if reference.blocked()
            && matches!(&command,Command::Review{intent,..}|Command::Intent(intent) if intent.starts_work() && reference.action().is_some_and(|a|a.session_id==intent.session_id()))
        {
            return Err("Reference removal is pending or unknown; no new work was queued. Continue in Formless TUI if its outcome is unknown.");
        }
        if let Command::DetachReference(d) = &command {
            if !d.owned_by(&self.reference_ledger)
                || !self.is_ready()
                || d.action().client != self.instance()
                || d.action().connection != self.connection()
                || d.action().epoch != self.epoch()
            {
                d.reject("Reference review or connection changed; no removal was queued.");
                return Err("Reference review or connection changed; no removal was queued.");
            }
        }
        if matches!(&command,Command::EditSession{change,..} if change.is_selection()) {
            self.invalidate_context();
        }
        if let Command::DecideApproval(d) = &command {
            if !d.owned_by(&self.approval_ledger)
                || !self.is_ready()
                || d.action().client != self.instance()
                || d.action().connection != self.connection()
                || d.action().epoch != self.epoch()
            {
                d.reject("Profile connection or review changed; no decision was queued.");
                return Err("Profile connection or review changed; no decision was queued.");
            }
        }

        if let Command::EnableStepMode(d) | Command::DisableStepMode(d) = &command {
            if d.action().on != matches!(&command, Command::EnableStepMode(_))
                || !d.owned_by(&self.mode_ledger)
                || !self.is_ready()
                || d.action().client != self.instance()
                || d.action().connection != self.connection()
                || d.action().view_epoch != self.stream_epoch()
            {
                d.reject("Session view or connection changed; no mode setting was queued.");
                return Err("Session view or connection changed; no mode setting was queued.");
            }
        }
        if let Command::DecideStep(dispatch) = &command {
            if !dispatch.owned_by(&self.step_ledger)
                || !self.is_ready()
                || dispatch.action().client != self.instance()
                || dispatch.action().connection != self.connection()
                || dispatch.action().epoch != self.epoch()
            {
                dispatch.reject("Step review attachment changed; no decision was queued.");
                return Err("Step review attachment changed; no decision was queued.");
            }
        }
        if matches!(command, Command::Select { .. } | Command::Reconnect) {
            self.invalidate_context();
            advance_epoch(&self.stream_epoch);
            self.mode_ledger.cancel_unsent(
                "Selection or connection changed before writing; no mode setting was sent.",
            );
        }
        if matches!(
            command,
            Command::Topic { .. }
                | Command::Intent(_)
                | Command::ReadRunAction { .. }
                | Command::PrepareRunAction(_)
                | Command::ConfirmRunAction(_)
                | Command::CommitRunAction(_)
                | Command::CreateSession
                | Command::Review { .. }
                | Command::ConfirmPrepared(_)
                | Command::CommitPrepared(_)
                | Command::AnswerQuestion { .. }
                | Command::EditSession { .. }
                | Command::ReadSessionDetails { .. }
                | Command::ReadModelCatalog { .. }
                | Command::ReadPendingAttachments { .. }
                | Command::StageAttachments(_)
                | Command::SearchProfile { .. }
                | Command::ExportSession { .. }
                | Command::ReadSessionUsage { .. }
                | Command::Manage { .. }
        ) && !self.is_ready()
        {
            return Err("Not connected; the action was not queued.");
        }
        let fork = if let Command::ForkSession(action) = &command {
            if !self.is_ready() || self.connection() != action.connection {
                return Err("Connection changed; no fork was queued. Review again.");
            }
            self.forks.reserve(action.clone())?;
            Some(action.clone())
        } else {
            None
        };
        if self.commands.try_send(command).is_err() {
            if let Some(action) = fork {
                self.forks
                    .reject_unsent(&action, "Command queue unavailable; no fork was queued.");
            }
            return Err("The daemon command queue is unavailable; nothing was queued.");
        }
        Ok(())
    }
    pub fn fork_status(&self) -> session_fork::Status {
        self.forks.status()
    }
    #[cfg(feature = "test-support")]
    pub fn record_fork_write(&self, action: &session_fork::Action) {
        assert!(self.recorded.is_some());
        assert!(self.forks.before_write(action));
    }
    #[cfg(feature = "test-support")]
    pub fn record_fork_outcome(
        &self,
        action: &session_fork::Action,
        outcome: session_fork::Outcome,
    ) {
        assert!(
            self.recorded.is_some(),
            "only a recording boundary can simulate a fork receipt"
        );
        self.forks.before_write(action);
        self.forks.finish(action, outcome);
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
    pub fn instance(&self) -> uuid::Uuid {
        self.instance
    }
    pub fn stream_epoch(&self) -> u64 {
        self.stream_epoch.load(Ordering::Acquire)
    }
    pub fn future_attempt_validation(&self) -> bool {
        self.future_attempts
    }

    /// Synchronous invalidation cannot be lost to the bounded command queue.
    pub fn invalidate_context(&self) {
        advance_epoch(&self.epoch);
        self.cancel_unsent_reference();
        self.step_ledger
            .cancel_unsent("Review context changed before writing; no Step decision was sent.");
        self.approval_ledger
            .cancel_unsent("Review context changed before writing; no approval decision was sent.");
    }

    pub fn review(&self, intent: Intent, draft_basis: String) -> Result<(), &'static str> {
        self.review_for_run(intent, draft_basis, None)
    }

    pub(crate) fn review_for_run(
        &self,
        intent: Intent,
        draft_basis: String,
        expected_run_id: Option<String>,
    ) -> Result<(), &'static str> {
        self.send(Command::Review {
            intent,
            expected_run_id,
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
            reference_ledger: Arc::new(crate::reference_detach::Ledger::default()),
            step_ledger: Arc::new(crate::step_decision::Ledger::default()),
            mode_ledger: Arc::new(crate::step_mode::Ledger::default()),
            approval_ledger: Arc::new(crate::approvals::Ledger::default()),
            commands,
            updates,
            stop,
            ready: Arc::new(AtomicBool::new(true)),
            epoch: Arc::new(AtomicU64::new(0)),
            connection: Arc::new(AtomicU64::new(0)),
            forks: Arc::new(session_fork::Ledger::default()),
            stream_epoch: Arc::new(AtomicU64::new(0)),
            instance: uuid::Uuid::new_v4(),
            future_attempts: false,
            recorded: Some(recorded),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn recording_future_attempts() -> Self {
        let mut client = Self::recording();
        client.future_attempts = true;
        client
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
    #[cfg(feature = "test-support")]
    pub fn retire_recording_connection(&self) {
        assert!(
            self.recorded.is_some(),
            "only a serialized view fixture can retire its connection without I/O"
        );
        drop(session_fork::ConnectionGuard::new(
            self.forks.clone(),
            self.connection(),
        ));
        advance_epoch(&self.connection);
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
    Topic {
        operation: Option<uuid::Uuid>,
        action: crate::topics::Action,
        revision: u64,
    },
    Manage {
        destination: Destination,
        label: String,
        undo: bool,
        run_job: bool,
        add: Option<(String, String)>,
    },
    Attachment(Box<attachments::Work>),
    RunReconcile(Box<run_actions::Recovery>),
    RestoreSession(crate::run_actions::DeleteUndo),
    RunTarget {
        owner: crate::run_actions::Owner,
        operation: uuid::Uuid,
    },
    RunInspect(Box<run_actions::Work>),
    QueueInventory(Box<run_actions::Work>, bool),
    RunForecast(Box<run_actions::Work>),
    RunConsent(Box<run_actions::Work>),
    RunCommitInspect(Box<run_actions::Work>),
    RunWrite(Box<run_actions::Work>),
    LiveContext(crate::run_actions::Owner),
    DetachReference(crate::reference_detach::Dispatch),
    SendInventory(Box<Preparation>),
    ReviewedAction {
        action: Action,
        work: Box<Preparation>,
        sent: bool,
    },
    AttachmentInventory {
        operation: uuid::Uuid,
        session_id: String,
    },
    ModelCatalog {
        operation: uuid::Uuid,
    },
    ApprovalRead {
        operation: uuid::Uuid,
        revision: u64,
    },
    ApprovalInspect {
        dispatch: crate::approvals::Dispatch,
        revision: u64,
    },
    ApprovalCommit {
        dispatch: crate::approvals::Dispatch,
        revision: u64,
    },
    ModeInspect {
        dispatch: crate::step_mode::Dispatch,
        revision: u64,
    },
    ModeCommit {
        dispatch: crate::step_mode::Dispatch,
        revision: u64,
    },
    StepInspect(crate::step_decision::Dispatch),
    StepCommit(crate::step_decision::Dispatch),
    StreamSubscribe(StreamOwner),
    StreamBarrier {
        owner: StreamOwner,
        questions_revision: u64,
        status_revision: u64,
    },
    StreamStatus {
        owner: StreamOwner,
        questions_revision: u64,
        status_revision: u64,
    },
    Invocations {
        operation: uuid::Uuid,
    },
    ForkSession(session_fork::Action),
    RunInspection {
        operation: uuid::Uuid,
        target: crate::run_inspection::Target,
    },
    SessionComparison {
        operation: uuid::Uuid,
        pair: crate::comparison::Pair,
    },
    SessionUsage {
        target: session_usage::Target,
        operation: uuid::Uuid,
    },
    SessionExport {
        operation: uuid::Uuid,
        session_id: String,
        format: session_export::Format,
    },
    ProfileSearch {
        operation: uuid::Uuid,
        query: String,
    },
    Ping,
    Subscribe,
    Sessions,
    Snapshot {
        session_id: String,
        generation: u64,
        questions_revision: u64,
        status_revision: u64,
    },
    Page(Destination),
    Action(Action),
    Forecast(Box<Preparation>),
    Consent(Box<Preparation>),
    Create,
    Answer(Action),
    EditSession {
        operation: uuid::Uuid,
        session_id: String,
        change: session_edit::Change,
    },
    InspectModelSelection {
        operation: uuid::Uuid,
        session_id: String,
        change: session_edit::Change,
    },
    ReadSessionDetails {
        operation: uuid::Uuid,
        session_id: String,
    },
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
    NeedsReview,
    NeedsConsent(String),
    Ready,
}

#[derive(Debug)]
struct Preparation {
    client: uuid::Uuid,
    connection: u64,
    attachments: Option<crate::pending_files::Report>,
    expected_run_id: Option<String>,
    id: uuid::Uuid,
    epoch: u64,
    generation: u64,
    active: Option<String>,
    draft_basis: String,
    status_revision: u64,
    intent: Intent,
    stage: PreparationStage,
}

impl Preparation {
    fn current(&self, engine: &Engine) -> bool {
        engine.ready
            && self.client == engine.instance
            && self.connection == engine.connection
            && self.epoch == engine.epoch
            && self.generation == engine.generation
            && self.active == engine.active
            && self.status_revision == engine.status_revision
    }
}

/// Pure, connection-local correlation and invalidation state. The worker
/// replaces this on reconnect; selection generations belong to the window.
#[derive(Default)]
struct Engine {
    topics_revision: u64,
    topics_available: bool,
    topics_polled: Option<std::time::Instant>,
    attachment_conversions: VecDeque<attachments::ClipboardJob>,
    run_recoveries: Arc<std::sync::Mutex<HashMap<uuid::Uuid, run_actions::Recovery>>>,
    reconciled_actions: Vec<Action>,
    delete_undo: Option<crate::run_actions::DeleteUndo>,
    compacting: Arc<std::sync::Mutex<HashSet<String>>>,
    action_work: Option<Box<run_actions::Work>>,
    live_overlay: crate::live_overlay::Overlay,
    live_available: bool,
    reference_detach_available: bool,
    session_model_cas_available: bool,
    reviewed_send_available: bool,
    idle_step_off_available: bool,
    reviewed_steer_run_available: bool,
    instance: uuid::Uuid,
    reader: reader::Reader,
    stream_epoch: u64,
    status_revision: u64,
    approval_revision: u64,
    forks: Arc<session_fork::Ledger>,
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
    fn topic_command(&mut self, operation: Option<uuid::Uuid>, action: crate::topics::Action) {
        if operation.is_some() {
            self.topics_revision = self.topics_revision.saturating_add(1);
        }
        self.request(
            action.request(),
            Pending::Topic {
                operation,
                action,
                revision: self.topics_revision,
            },
        );
    }
    fn emit_live_overlay(&mut self) {
        if let Some(session_id) = self.active.clone() {
            if let Some(view) = self.live_overlay.take() {
                self.updates.push(Update::LiveOverlay {
                    client: self.instance,
                    connection: self.connection,
                    generation: self.generation,
                    session_id,
                    view,
                });
            }
        }
    }
    fn reference_current(&self, d: &crate::reference_detach::Dispatch) -> bool {
        let a = d.action();
        self.ready
            && a.valid()
            && a.client == self.instance
            && a.connection == self.connection
            && a.epoch == self.epoch
    }
    fn retire_reference_writes(&mut self) {
        let ids = self
            .pending
            .iter()
            .filter_map(|(id, p)| match p {
                Pending::DetachReference(d)
                    if !d.sent() && (!d.reserved() || !self.reference_current(d)) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in ids {
            self.outgoing.retain(|r| r.id != id);
            if let Some(Pending::DetachReference(d)) = self.pending.remove(&id) {
                d.reject("Reference context changed before writing; nothing was removed.");
            }
        }
    }
    fn approval_current(&self, d: &crate::approvals::Dispatch) -> bool {
        let a = d.action();
        self.ready
            && a.valid()
            && a.client == self.instance
            && a.connection == self.connection
            && a.epoch == self.epoch
    }
    fn retire_approval_work(&mut self) {
        let ids = self
            .pending
            .iter()
            .filter_map(|(id, p)| match p {
                Pending::ApprovalRead { revision, .. } if *revision != self.approval_revision => {
                    Some(*id)
                }
                Pending::ApprovalInspect {
                    dispatch: d,
                    revision,
                } if !d.reserved()
                    || !self.approval_current(d)
                    || *revision != self.approval_revision =>
                {
                    Some(*id)
                }
                Pending::ApprovalCommit {
                    dispatch: d,
                    revision,
                } if !d.sent()
                    && (!d.reserved()
                        || !self.approval_current(d)
                        || *revision != self.approval_revision) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in ids {
            self.outgoing.retain(|r| r.id != id);
            match self.pending.remove(&id) {
                Some(
                    Pending::ApprovalInspect { dispatch, .. }
                    | Pending::ApprovalCommit { dispatch, .. },
                ) => dispatch.reject(
                    "Approval inventory or review changed before writing; no decision was sent.",
                ),
                Some(Pending::ApprovalRead { operation, .. }) => {
                    self.updates.push(Update::ApprovalInventory {
                        client: self.instance,
                        connection: self.connection,
                        operation,
                        revision: self.approval_revision,
                        result: Err(
                            "Approvals changed while reading; reload the current inventory.".into(),
                        ),
                    })
                }
                _ => {}
            }
        }
    }
    fn invalidate_approvals(&mut self) -> Result<(), String> {
        self.approval_revision = self
            .approval_revision
            .checked_add(1)
            .ok_or("Approval revision exhausted")?;
        self.retire_approval_work();
        self.updates.push(Update::ApprovalInvalidated {
            client: self.instance,
            connection: self.connection,
            revision: self.approval_revision,
        });
        Ok(())
    }
    fn before_approval_writes(&mut self) -> Result<(), String> {
        self.retire_approval_work();
        let work = self
            .pending
            .values()
            .filter_map(|p| match p {
                Pending::ApprovalCommit { dispatch, .. } if !dispatch.sent() => {
                    Some(dispatch.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for d in work {
            if self.approval_revision == u64::MAX || self.status_revision == u64::MAX {
                return Err("Approval status revision exhausted".into());
            }
            if !d.before_write() {
                return Err(
                    "Approval reservation retired before writing; nothing was retried.".into(),
                );
            }
            self.invalidate_approvals()?;
            self.invalidate_mode_metadata()?;
        }
        Ok(())
    }

    fn begin_mode_change(&mut self, dispatch: crate::step_mode::Dispatch, on: bool) {
        if self.pending.values().any(|p|matches!(p,Pending::ModeInspect{dispatch:d,..}|Pending::ModeCommit{dispatch:d,..} if d.action().operation==dispatch.action().operation)){return;}
        if dispatch.action().on != on || !self.mode_current(&dispatch) || !dispatch.reserved() {
            dispatch.reject("Session view or mode target changed; no setting was sent.");
        } else if !on && !self.idle_step_off_available {
            dispatch.reject("This daemon cannot safely disable Step mode only while idle. Update the daemon or use Formless TUI.");
        } else {
            self.request(
                Request::GetSession {
                    id: dispatch.action().session_id.clone(),
                },
                Pending::ModeInspect {
                    dispatch,
                    revision: self.status_revision,
                },
            );
        }
    }
    fn mode_current(&self, d: &crate::step_mode::Dispatch) -> bool {
        let a = d.action();
        self.ready
            && a.valid()
            && a.client == self.instance
            && a.connection == self.connection
            && a.generation == self.generation
            && a.view_epoch == self.stream_epoch
            && self.active.as_deref() == Some(a.session_id.as_str())
    }
    fn retire_mode_changes(&mut self) {
        let ids = self
            .pending
            .iter()
            .filter_map(|(id, p)| match p {
                Pending::ModeInspect {
                    dispatch: d,
                    revision,
                } if !d.reserved()
                    || !self.mode_current(d)
                    || *revision != self.status_revision =>
                {
                    Some(*id)
                }
                Pending::ModeCommit {
                    dispatch: d,
                    revision,
                } if !d.sent()
                    && (!d.reserved()
                        || !self.mode_current(d)
                        || *revision != self.status_revision) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in ids {
            self.outgoing.retain(|r| r.id != id);
            if let Some(
                Pending::ModeInspect { dispatch, .. } | Pending::ModeCommit { dispatch, .. },
            ) = self.pending.remove(&id)
            {
                dispatch
                    .reject("Session/Run/Step changed before writing; no mode setting was sent.");
            }
        }
    }
    fn invalidate_mode_metadata(&mut self) -> Result<(), String> {
        self.status_revision = self
            .status_revision
            .checked_add(1)
            .ok_or("Session status revision exhausted")?;
        self.sync_epoch(self.epoch);
        self.retire_step_reviews(true);
        self.retire_mode_changes();
        self.dirty_session = true;
        if let Some(session_id) = self.active.clone() {
            self.updates.push(Update::SessionStatusInvalidated {
                client: self.instance,
                connection: self.connection,
                generation: self.generation,
                session_id,
            });
        }
        Ok(())
    }
    fn before_mode_writes(&mut self) -> Result<(), String> {
        let work = self
            .pending
            .values()
            .filter_map(|p| match p {
                Pending::ModeCommit { dispatch, .. } if !dispatch.sent() => Some(dispatch.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for dispatch in work {
            if self.status_revision == u64::MAX {
                return Err("Session status revision exhausted".into());
            }
            if !dispatch.before_write() {
                return Err(
                    "Step-mode reservation retired before writing; no retry was sent.".into(),
                );
            }
            self.invalidate_mode_metadata()?;
        }
        Ok(())
    }
    fn step_current(&self, dispatch: &crate::step_decision::Dispatch) -> bool {
        let a = dispatch.action();
        self.ready
            && a.valid()
            && self.instance == a.client
            && self.connection == a.connection
            && self.epoch == a.epoch
            && self.generation == a.generation
            && self.active.as_deref() == Some(a.prompt.session_id.as_str())
    }
    fn retire_step_reviews(&mut self, status_changed: bool) {
        let retired: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(id, p)| match p {
                Pending::StepInspect(d)
                    if status_changed || !d.reserved() || !self.step_current(d) =>
                {
                    Some(*id)
                }
                Pending::StepCommit(d)
                    if !d.sent() && (status_changed || !d.reserved() || !self.step_current(d)) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
        for id in retired {
            self.outgoing.retain(|r| r.id != id);
            if let Some(Pending::StepInspect(d) | Pending::StepCommit(d)) = self.pending.remove(&id)
            {
                d.reject("Step review changed before writing; no decision was sent.");
            }
        }
    }
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
        let run_session = match command {
            Command::PrepareRunAction(d) => Some(d.action.owner.session.as_str()),
            Command::ConfirmRunAction(_) | Command::CommitRunAction(_) => self
                .action_work
                .as_ref()
                .map(|w| w.dispatch.action.owner.session.as_str()),
            _ => None,
        };
        if let Some(session) = run_session {
            return uncertain
                .iter()
                .any(|a| a.question_id.is_none() && a.session_id == session);
        }
        if let Command::DetachReference(d) = command {
            return uncertain
                .iter()
                .any(|a| a.question_id.is_none() && a.session_id == d.action().session_id);
        }
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
                matches!(pending, Pending::Action(pending) | Pending::Answer(pending) | Pending::ReviewedAction{action:pending,..} if pending == action) || matches!(pending,Pending::DetachReference(d) if d.action().operation==action.id && d.action().session_id==action.session_id))
                })
        })
    }
    fn sync_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.retire_run_review();
        let (generation, active, status_revision, client, connection) = (
            self.generation,
            self.active.as_ref(),
            self.status_revision,
            self.instance,
            self.connection,
        );
        let ready = self.ready;
        let current = |work: &Preparation| {
            ready
                && work.epoch == epoch
                && work.client == client
                && work.connection == connection
                && work.generation == generation
                && work.active.as_ref() == active
                && work.status_revision == status_revision
        };
        if self.prepared.as_ref().is_some_and(|work| !current(work)) {
            self.prepared = None;
        }
        let cancelled: Vec<_> = self
            .pending
            .values()
            .filter_map(|p| match p {
                Pending::ReviewedAction {
                    action,
                    work,
                    sent: false,
                } if !current(work) => Some(action.clone()),
                _ => None,
            })
            .collect();
        self.pending.retain(|_, pending| match pending {
            Pending::Forecast(work) | Pending::Consent(work) | Pending::SendInventory(work) => {
                current(work)
            }
            Pending::ReviewedAction { work, sent, .. } => *sent || current(work),
            _ => true,
        });
        self.outgoing
            .retain(|request| self.pending.contains_key(&request.id));
        for action in cancelled {
            self.updates.push(Update::Action{session_id:action.session_id,result:Err("Review changed before writing; no reviewed message was sent. Review again explicitly".into())});
        }
    }
    fn request(&mut self, request: Request, pending: Pending) {
        let session = match &request {
            Request::SendMessage { session_id, .. }
            | Request::SendReviewedMessage { session_id, .. }
            | Request::StartPipelineRun { session_id, .. }
            | Request::ResumeRun { session_id }
            | Request::EditMessage { session_id, .. }
            | Request::RetryRun { session_id, .. } => Some(session_id),
            _ => None,
        };
        if session.is_some_and(|id| self.compaction_pending(id)) {
            let error = "Compaction is still pending. Wait for its completion before starting work or rewriting history.";
            match &pending {
                Pending::RunWrite(w) => w.dispatch.reject(error),
                Pending::Action(a) | Pending::ReviewedAction { action: a, .. } => {
                    self.updates.push(Update::Action {
                        session_id: a.session_id.clone(),
                        result: Err(error.into()),
                    });
                }
                _ => {}
            }
            self.updates.push(Update::Notice(error.into()));
            return;
        }
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
        if self.stream_subscribe() {
            return;
        }
        let sessions = self
            .active
            .iter()
            .cloned()
            .chain(
                self.compacting
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .cloned(),
            )
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        self.request(
            Request::Subscribe {
                sessions: Some(sessions),
                deltas: self.live_available && !self.reader.requested,
                client: Some("Desktop".into()),
            },
            Pending::Subscribe,
        );
    }

    fn select(&mut self, session_id: Option<String>, generation: u64) {
        self.live_overlay = Default::default();
        self.reader.clear();
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
        if self.topics_available
            && self
                .topics_polled
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
            && !self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Topic { .. }))
        {
            self.topics_polled = Some(std::time::Instant::now());
            self.topic_command(None, crate::topics::Action::List);
        }
        self.emit_live_overlay();
        if self.dirty_list
            && !self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Sessions))
        {
            self.dirty_list = false;
            self.request(Request::ListSessions, Pending::Sessions);
        }
        if self.stream_refresh() {
            return;
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
                        status_revision: self.status_revision,
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
        let request = match crate::manage::list_request(destination) {
            Some(request) => request,
            None => match destination {
                Destination::Metrics => Request::RunMetrics {
                    limit: Some(100),
                    session_id: None,
                    since: None,
                },
                Destination::Proposals => Request::ListProposals,
                Destination::Settings => Request::GetSettings,
                _ => return,
            },
        };
        self.request(request, Pending::Page(destination));
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Topic {
                connection,
                operation,
                action,
            } => {
                if operation.is_some()
                    && self.pending.values().any(|p| {
                        matches!(
                            p,
                            Pending::Topic {
                                operation: Some(_),
                                ..
                            }
                        )
                    })
                {
                    // A single consent may have an approval/start continuation.
                    // Duplicate clicks must not create a second workflow.
                    return;
                }
                if self.ready && connection == self.connection && self.topics_available {
                    self.topic_command(operation, action);
                } else {
                    self.updates.push(Update::Topics {
                        connection: self.connection, operation,
                        result: Err("Topics require a current connection to a topic-enabled daemon. Nothing was sent.".into()),
                    });
                }
            }
            Command::StageAttachments(operation) => self.stage_attachments(operation),
            Command::UndoDelete(operation) => {
                if let Some(ticket) = self
                    .delete_undo
                    .take()
                    .filter(|t| t.operation == operation && t.available() && self.ready)
                {
                    self.request(
                        Request::RestoreSession {
                            id: ticket.owner.session.clone(),
                        },
                        Pending::RestoreSession(ticket),
                    );
                }
            }
            Command::ReadRunAction { owner, operation } => {
                if self.ready && self.action_owner().as_ref() == Some(&owner) {
                    let retired = self
                        .pending
                        .iter()
                        .filter_map(|(id, p)| matches!(p, Pending::RunTarget { .. }).then_some(*id))
                        .collect::<Vec<_>>();
                    for id in retired {
                        self.pending.remove(&id);
                        self.outgoing.retain(|r| r.id != id);
                    }
                    self.request(
                        Request::GetSession {
                            id: owner.session.clone(),
                        },
                        Pending::RunTarget { owner, operation },
                    );
                }
            }
            Command::PrepareRunAction(d) => {
                self.action_prepare(d);
            }
            Command::ConfirmRunAction(id) => {
                self.action_confirm(id);
            }
            Command::CommitRunAction(id) => {
                self.action_commit(id);
            }
            command => self.command_existing(command),
        }
    }

    fn command_existing(&mut self, command: Command) {
        let run_busy = self
            .action_work
            .as_ref()
            .is_some_and(|w| w.dispatch.reserved())
            || self.pending.values().any(|p| {
                matches!(
                    p,
                    Pending::RunInspect(_)
                        | Pending::QueueInventory(_, _)
                        | Pending::RunForecast(_)
                        | Pending::RunConsent(_)
                        | Pending::RunCommitInspect(_)
                        | Pending::RunWrite(_)
                )
            });
        if run_busy
            && matches!(
                &command,
                Command::Review { .. }
                    | Command::ConfirmPrepared(_)
                    | Command::CommitPrepared(_)
                    | Command::Intent(_)
                    | Command::EditSession { .. }
                    | Command::DetachReference(_)
                    | Command::EnableStepMode(_)
                    | Command::DisableStepMode(_)
                    | Command::DecideStep(_)
                    | Command::DecideApproval(_)
            )
        {
            self.updates.push(Update::Notice("Wait for the reviewed action receipt or cancel its review before another mutation. Nothing was sent.".into()));
            return;
        }
        let work = match &command {
            Command::Review { intent, .. } | Command::Intent(intent) => Some(intent),
            Command::ConfirmPrepared(id) | Command::CommitPrepared(id) => self
                .prepared
                .as_ref()
                .filter(|p| p.id == *id)
                .map(|p| &p.intent),
            _ => None,
        };
        if work.is_some_and(|intent| intent.starts_work() && self.pending.values().any(|p|matches!(p,Pending::DetachReference(d)if d.action().session_id==intent.session_id()))) {
            self.updates.push(Update::Notice(
                "Wait for the reference-removal receipt before starting work. Draft kept.".into(),
            ));
            return;
        }
        let model_pending = self.pending.values().any(|p| {
            matches!(p, Pending::InspectModelSelection { .. })
                || matches!(p,Pending::EditSession{change,..} if change.is_selection())
        });
        let starts_work = matches!(
            &command,
            Command::Review { .. } | Command::ConfirmPrepared(_) | Command::CommitPrepared(_)
        ) || matches!(&command,Command::Intent(intent) if intent.starts_work());
        if model_pending && starts_work {
            let epoch = match &command {
                Command::Review { epoch, .. } => Some(*epoch),
                Command::ConfirmPrepared(id) | Command::CommitPrepared(id) => self
                    .prepared
                    .as_ref()
                    .filter(|p| p.id == *id)
                    .map(|p| p.epoch),
                _ => None,
            };
            self.prepared = None;
            let error =
                "Wait for the Session model/effort save receipt before starting work. Draft kept."
                    .to_string();
            if let Some(epoch) = epoch {
                self.updates.push(Update::PreparationFailed {
                    id: match &command {
                        Command::ConfirmPrepared(id) | Command::CommitPrepared(id) => *id,
                        _ => uuid::Uuid::new_v4(),
                    },
                    epoch,
                    error,
                });
            } else {
                self.updates.push(Update::Notice(error));
            }
            return;
        }
        match command {
            Command::Topic { .. } => unreachable!("handled by topic dispatcher"),
            Command::StageAttachments(_) => unreachable!("handled by attachment dispatcher"),
            Command::UndoDelete(_) => unreachable!("handled by action dispatcher"),
            Command::ReadRunAction { .. }
            | Command::PrepareRunAction(_)
            | Command::ConfirmRunAction(_)
            | Command::CommitRunAction(_) => unreachable!("handled by action dispatcher"),
            Command::DetachReference(d) => {
                if !self.reference_detach_available || !self.reference_current(&d) || !d.reserved() {
                    d.reject("Atomic reference removal is unavailable or this review changed. Update the daemon or use Formless TUI; nothing was removed.");
                } else if self.pending.values().any(|p|matches!(p,Pending::Action(a)|Pending::ReviewedAction{action:a,..} if a.session_id==d.action().session_id)) {
                    d.reject("Wait for the Session's action receipt before removing a reference. Nothing was removed.");
                } else if !self.pending.values().any(|p|matches!(p,Pending::DetachReference(old) if old.action().operation==d.action().operation)) {
                    self.request(d.action().request(),Pending::DetachReference(d));
                }
            }
            Command::ReadPendingAttachments {
                connection,
                operation,
                session_id,
            } => {
                let error = if !self.ready
                    || connection != self.connection
                    || operation.is_nil()
                    || session_id.is_empty()
                    || session_id.len() > 256
                    || session_id.chars().any(char::is_control)
                {
                    Some("Connection or Session changed; read pending files again explicitly.")
                } else if self
                    .pending
                    .values()
                    .filter(|p| matches!(p, Pending::AttachmentInventory { .. }))
                    .count()
                    >= 2
                {
                    Some("Two pending-file reads remain outstanding. Wait or reconnect.")
                } else {
                    None
                };
                if let Some(error) = error {
                    self.updates.push(Update::PendingAttachments {
                        client: self.instance,
                        connection: self.connection,
                        operation,
                        session_id,
                        result: Err(error.into()),
                    });
                } else {
                    self.request(
                        Request::ListPendingAttachments {
                            session_id: session_id.clone(),
                        },
                        Pending::AttachmentInventory {
                            operation,
                            session_id,
                        },
                    );
                }
            }
            Command::ReadModelCatalog {
                connection,
                operation,
            } => {
                let error = if !self.ready || connection != self.connection || operation.is_nil() {
                    Some("Connection changed; read the catalog again explicitly.")
                } else if self
                    .pending
                    .values()
                    .any(|p| matches!(p, Pending::ModelCatalog { .. }))
                {
                    Some("A catalog read is still outstanding. Wait for its reply.")
                } else {
                    None
                };
                if let Some(error) = error {
                    self.updates.push(Update::ModelCatalog {
                        instance: self.instance,
                        connection: self.connection,
                        operation,
                        result: Err(error.into()),
                    });
                } else {
                    self.request(Request::ListModels, Pending::ModelCatalog { operation });
                }
            }
            Command::ReadInvocations {
                connection,
                operation,
            } => {
                let error = if !self.ready || connection != self.connection {
                    Some("Connection changed. Read Invocations again explicitly.")
                } else if self
                    .pending
                    .values()
                    .filter(|pending| matches!(pending, Pending::Invocations { .. }))
                    .count()
                    >= 2
                {
                    Some("Two Invocation reads remain outstanding. Wait for a reply.")
                } else {
                    None
                };
                if let Some(error) = error {
                    self.updates.push(Update::Invocations {
                        connection,
                        operation,
                        result: Err(error.into()),
                    });
                } else {
                    self.request(Request::ListInvocations, Pending::Invocations { operation });
                }
            }
            Command::ReadApprovals {
                connection,
                operation,
            } => {
                if self.ready
                    && connection == self.connection
                    && !operation.is_nil()
                    && self
                        .pending
                        .values()
                        .filter(|p| matches!(p, Pending::ApprovalRead { .. }))
                        .count()
                        < 2
                {
                    self.request(
                        Request::PendingApprovals,
                        Pending::ApprovalRead {
                            operation,
                            revision: self.approval_revision,
                        },
                    );
                } else {
                    self.updates.push(Update::ApprovalInventory {
                        client: self.instance,
                        connection: self.connection,
                        operation,
                        revision: self.approval_revision,
                        result: Err(
                            "Approval reads unavailable or already pending; nothing was requested."
                                .into(),
                        ),
                    });
                }
            }
            Command::DecideApproval(dispatch) => {
                if self.pending.values().any(|p|matches!(p,Pending::ApprovalInspect{dispatch:d,..}|Pending::ApprovalCommit{dispatch:d,..} if d.action().operation==dispatch.action().operation)){return}
                if !self.approval_current(&dispatch) || !dispatch.reserved() {
                    dispatch.reject("Approval review is stale; no decision was sent.");
                } else {
                    self.request(
                        Request::PendingApprovals,
                        Pending::ApprovalInspect {
                            dispatch,
                            revision: self.approval_revision,
                        },
                    );
                }
            }
            Command::EnableStepMode(dispatch) => self.begin_mode_change(dispatch, true),
            Command::DisableStepMode(dispatch) => self.begin_mode_change(dispatch, false),
            Command::DecideStep(dispatch) => {
                if self.pending.values().any(|p|matches!(p,Pending::StepInspect(d)|Pending::StepCommit(d) if d.action().operation==dispatch.action().operation)){return;}
                if !self.step_current(&dispatch) || !dispatch.reserved() {
                    dispatch.reject("Step review context changed; no decision was sent.");
                } else {
                    self.request(
                        Request::GetSession {
                            id: dispatch.action().prompt.session_id.clone(),
                        },
                        Pending::StepInspect(dispatch),
                    );
                }
            }
            Command::ForkSession(action) => {
                if self.pending.values().any(
                    |pending| matches!(pending,Pending::ForkSession(current) if current==&action),
                ) {
                    return;
                }
                if !self.ready
                    || self.connection != action.connection
                    || !action.valid()
                    || !self.forks.reserved(&action)
                {
                    self.forks.reject_unsent(
                        &action,
                        "Fork was not sent: connection or reservation changed. Review again.",
                    );
                } else {
                    self.request(
                        Request::ForkSession {
                            id: action.source_id.clone(),
                            name: None,
                            up_to_seq: None,
                        },
                        Pending::ForkSession(action),
                    );
                }
            }
            Command::InspectRun {
                connection,
                operation,
                target,
            } => {
                let error = if !self.ready || connection != self.connection || !target.valid() {
                    Some("Connection or Run identity changed. Select the Run again.")
                } else if self
                    .pending
                    .values()
                    .filter(|pending| matches!(pending, Pending::RunInspection { .. }))
                    .count()
                    >= 2
                {
                    Some("Two Run inspections remain outstanding. Wait for a reply.")
                } else {
                    None
                };
                if let Some(error) = error {
                    self.updates.push(Update::RunInspection {
                        connection: self.connection,
                        operation,
                        target,
                        result: Err(error.into()),
                    });
                } else {
                    self.request(
                        Request::GetRun {
                            run_id: target.run_id.clone(),
                        },
                        Pending::RunInspection { operation, target },
                    );
                }
            }
            Command::CompareSessions {
                connection,
                operation,
                pair,
            } => {
                let error = if !self.ready || connection != self.connection || !pair.valid() {
                    Some("Connection or Session pair changed. Choose the pair again.".to_string())
                } else if self
                    .pending
                    .values()
                    .filter(|p| matches!(p, Pending::SessionComparison { .. }))
                    .count()
                    >= 2
                {
                    Some("Two comparisons remain outstanding. Wait for a reply before reading again.".to_string())
                } else {
                    None
                };
                if let Some(error) = error {
                    self.updates.push(Update::SessionComparison {
                        connection: self.connection,
                        operation,
                        pair,
                        result: Err(error),
                    });
                } else {
                    self.request(
                        Request::CompareSessions {
                            left: pair.left.clone(),
                            right: pair.right.clone(),
                        },
                        Pending::SessionComparison { operation, pair },
                    );
                }
            }
            Command::ReadSessionUsage { target, operation } => {
                let error = if !self.ready
                    || self.connection != target.connection
                    || self.generation != target.generation
                    || self.active.as_deref() != Some(target.session_id.as_str())
                {
                    Some("Session or connection changed. Read usage again for the open Session.")
                } else if self
                    .pending
                    .values()
                    .filter(|p| matches!(p, Pending::SessionUsage { .. }))
                    .count()
                    >= 2
                {
                    Some("Two usage reads remain outstanding. Wait for their replies or reconnect.")
                } else {
                    None
                };
                if let Some(error) = error {
                    self.updates.push(Update::SessionUsage {
                        target,
                        operation,
                        result: Err(error.into()),
                    });
                } else {
                    self.request(
                        Request::SessionUsage {
                            session_id: target.session_id.clone(),
                        },
                        Pending::SessionUsage { target, operation },
                    );
                }
            }
            Command::ExportSession {
                connection,
                operation,
                session_id,
                format,
            } => {
                let error = if !self.ready || connection != self.connection {
                    Some("Connection changed; request a new export after connecting.")
                } else if session_id.is_empty() || session_id.len() > 256 {
                    Some("Unsupported Session identity. Use Formless TUI.")
                } else if self
                    .pending
                    .values()
                    .filter(|p| matches!(p, Pending::SessionExport { .. }))
                    .count()
                    >= 2
                {
                    Some("Two exports are still outstanding. Wait for their replies or reconnect before exporting again.")
                } else {
                    None
                };
                if let Some(error) = error {
                    self.updates.push(Update::SessionExport {
                        connection: self.connection,
                        operation,
                        session_id,
                        format,
                        result: Err(error.into()),
                    });
                } else {
                    self.request(
                        Request::ExportSession {
                            id: session_id.clone(),
                            format: Some(format.wire().into()),
                        },
                        Pending::SessionExport {
                            operation,
                            session_id,
                            format,
                        },
                    );
                }
            }
            Command::SearchProfile {
                connection,
                operation,
                query,
            } => {
                if !self.ready
                    || connection != self.connection
                    || profile_search::query(&query).is_err()
                {
                    self.updates.push(Update::ProfileSearch {
                        connection: self.connection,
                        operation,
                        query,
                        result: Err(
                            "Connection or query is invalid; search again after connecting.".into(),
                        ),
                    });
                } else if self
                    .pending
                    .values()
                    .filter(|p| matches!(p, Pending::ProfileSearch { .. }))
                    .count()
                    >= 2
                {
                    self.updates.push(Update::ProfileSearch{connection:self.connection,operation,query,result:Err("Two searches are still outstanding. Wait for their reply or reconnect before searching again.".into())});
                } else {
                    let query = profile_search::query(&query)
                        .expect("query checked above")
                        .to_string();
                    self.request(
                        Request::SearchSessions {
                            query: query.clone(),
                            limit: Some(profile_search::LIMIT),
                        },
                        Pending::ProfileSearch { operation, query },
                    );
                }
            }
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
            Command::Manage {
                destination,
                request,
                label,
            } => {
                if self.ready {
                    let undo = matches!(request, Request::UndoRegistryChange);
                    let run_job = matches!(request, Request::RunJobNow { .. });
                    let add = match &request {
                        Request::AddSkill { source, .. } => Some((source.clone(), String::new())),
                        Request::AddJob { prompt, cron, .. } => {
                            Some((prompt.clone(), cron.clone().unwrap_or_default()))
                        }
                        Request::UpsertMemory {
                            id: None,
                            text: Some(text),
                            ..
                        } => Some((text.clone(), String::new())),
                        _ => None,
                    };
                    self.request(
                        request,
                        Pending::Manage {
                            destination,
                            label,
                            undo,
                            run_job,
                            add,
                        },
                    );
                } else {
                    self.updates.push(Update::ManagementNotice {
                        destination,
                        notice: format!("Could not {label}: Not connected; nothing was sent."),
                    });
                }
            }
            Command::Refresh => {
                self.recover_run_actions();
                self.stream_reload();
                self.dirty_list = true;
                self.dirty_session = true;
                if let Some(page) = self.page {
                    self.load_page(page);
                }
            }
            Command::CreateSession if self.ready => {
                self.request(Request::CreateSession { name: None }, Pending::Create)
            }
            Command::ReadSessionDetails {
                connection,
                operation,
                session_id,
            } if self.ready => {
                if connection != self.connection || session_id.is_empty() || session_id.len() > 256
                {
                    self.updates.push(Update::SessionDetails {
                        connection: self.connection,
                        operation,
                        session_id,
                        result: Err(
                            "Connection or Session identity changed; reload details.".into()
                        ),
                    });
                } else {
                    self.request(
                        Request::ListSessions,
                        Pending::ReadSessionDetails {
                            operation,
                            session_id,
                        },
                    );
                }
            }
            Command::EditSession {
                connection,
                operation,
                session_id,
                change,
            } if self.ready => {
                if change.is_selection() && self.pending.values().any(|p|matches!(p,Pending::EditSession{operation:op,session_id:id,..}|Pending::InspectModelSelection{operation:op,session_id:id,..} if *op==operation && id==&session_id)) {return;}
                let error = if change.is_selection() && !self.session_model_cas_available {
                    Some("Update the daemon for atomic model/effort changes. No setting was sent.".into())
                } else if connection != self.connection {
                    Some("Connection changed; reload Session details before editing.".into())
                } else if session_id.is_empty() || session_id.len() > 256 {
                    Some("Invalid Session identity; no change was sent.".into())
                } else if change.is_selection() && self.pending.values().any(|p|matches!(p,Pending::Action(a)|Pending::ReviewedAction{action:a,..} if a.session_id==session_id)) {
                    Some("Wait for the Session's previous action receipt before changing its model. No setting was sent.".into())
                } else if self.pending.values().any(|pending| matches!(pending, Pending::EditSession { session_id: id, .. } | Pending::InspectModelSelection {session_id:id,..} if id == &session_id)) {
                    Some("A metadata change for this Session is already waiting.".into())
                } else { change.validate().err().map(str::to_string) };
                if let Some(error) = error {
                    self.updates.push(Update::SessionEdited {
                        connection: self.connection,
                        operation,
                        session_id,
                        outcome: session_edit::Outcome::Rejected(error),
                    });
                } else if change.is_selection() {
                    if let Err(error) = self.invalidate_mode_metadata() {
                        self.updates.push(Update::SessionEdited {
                            connection: self.connection,
                            operation,
                            session_id,
                            outcome: session_edit::Outcome::Rejected(error),
                        });
                    } else {
                        self.request(
                            Request::ListSessions,
                            Pending::InspectModelSelection {
                                operation,
                                session_id,
                                change,
                            },
                        );
                    }
                } else {
                    self.request(
                        change.request(&session_id),
                        Pending::EditSession {
                            operation,
                            session_id,
                            change,
                        },
                    );
                }
            }
            Command::Review {
                intent,
                expected_run_id,
                draft_basis,
                epoch,
            } if self.ready => {
                if epoch != self.epoch
                    || !intent.starts_work()
                    || self.active.as_deref() != Some(intent.session_id())
                {
                    self.updates.push(Update::Notice(
                        "The action changed before its forecast was requested. Draft kept.".into(),
                    ));
                    return;
                }
                if self.prepared.is_some() || self.pending.values().any(|pending| matches!(pending,
                    Pending::Forecast(_) | Pending::Consent(_) | Pending::SendInventory(_)
                ) || matches!(pending, Pending::Action(action) | Pending::ReviewedAction{action,..} if action.session_id == intent.session_id())) {
                    self.updates.push(Update::Notice("Finish or cancel the current action first. Draft kept.".into()));
                    return;
                }
                if let Intent::SendMessage { content, mode, .. } = &intent {
                    if !self.reviewed_send_available {
                        self.updates.push(Update::PreparationFailed{id:uuid::Uuid::new_v4(),epoch,error:"This daemon does not support reviewed pending files and Send/Steer mode checks. Update the daemon or use Formless TUI".into()});
                        return;
                    }
                    if *mode == formless_protocol::SendMode::Steer
                        && (!self.reviewed_steer_run_available
                            || !expected_run_id
                                .as_deref()
                                .is_some_and(formless_protocol::valid_reviewed_run_id))
                    {
                        self.updates.push(Update::PreparationFailed {
                            id: uuid::Uuid::new_v4(),
                            epoch,
                            error: "Steer needs a current Run identity and a matching daemon. Refresh the Session or use Formless TUI. Draft kept.".into(),
                        });
                        return;
                    }
                    if *mode == formless_protocol::SendMode::Queue
                        || content != &draft_basis
                        || content.len() > crate::forecast::MAX_REVIEW_DRAFT_BYTES
                    {
                        self.updates.push(Update::PreparationFailed{id:uuid::Uuid::new_v4(),epoch,error:"Queue attachment ownership is unavailable; Send/Steer require the exact draft within the native 64 KiB review limit. Use Formless TUI for Queue or larger messages".into()});
                        return;
                    }
                }
                let work = Preparation {
                    client: self.instance,
                    connection: self.connection,
                    attachments: None,
                    expected_run_id: expected_run_id.filter(|_| {
                        matches!(
                            intent,
                            Intent::SendMessage {
                                mode: formless_protocol::SendMode::Steer,
                                ..
                            }
                        )
                    }),
                    id: uuid::Uuid::new_v4(),
                    epoch,
                    generation: self.generation,
                    active: self.active.clone(),
                    draft_basis,
                    status_revision: self.status_revision,
                    intent,
                    stage: PreparationStage::Forecast,
                };
                self.start_send_review(Box::new(work));
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
                    PreparationStage::NeedsReview => {
                        let mut work = work;
                        work.stage = PreparationStage::Ready;
                        self.updates.push(Update::PreparationReady {
                            id: work.id,
                            epoch: work.epoch,
                        });
                        self.prepared = Some(work);
                    }
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
                if matches!(work.intent, Intent::SendMessage { .. }) {
                    self.commit_reviewed_send(work);
                } else {
                    self.command(Command::Intent(work.intent));
                }
            }
            Command::Intent(intent) if self.ready => {
                if intent.starts_work() && self.pending.values().any(|pending|
                    matches!(pending, Pending::Action(action) | Pending::ReviewedAction{action,..} if action.session_id == intent.session_id())) {
                    self.updates.push(Update::Notice("Wait for the previous action's acknowledgement; the draft was kept.".into()));
                    return;
                }
                let action_id = uuid::Uuid::new_v4();
                let (session_id, request) = match intent {
                    Intent::SendMessage { .. } => {
                        self.updates.push(Update::Notice("No message sent. Read and confirm pending-file metadata through the composer review first.".into()));
                        return;
                    }
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
            Command::EditSession {
                operation,
                session_id,
                ..
            } => self.updates.push(Update::SessionEdited {
                connection: self.connection,
                operation,
                session_id,
                outcome: session_edit::Outcome::Rejected(
                    "Not connected; no change was sent.".into(),
                ),
            }),
            Command::ReadSessionDetails {
                operation,
                session_id,
                ..
            } => self.updates.push(Update::SessionDetails {
                connection: self.connection,
                operation,
                session_id,
                result: Err("Not connected; no details were requested.".into()),
            }),
        }
    }

    fn receive(&mut self, message: ServerMessage) -> Result<(), String> {
        let definite_rejection = run_actions::definite_rejection(&message);
        let (id, result, response_ok) = match message {
            ServerMessage::Event(event) => {
                if let Some(destination) = self.page {
                    let refresh = matches!(
                        (destination, &event),
                        (
                            Destination::Tools,
                            Event::RegistryChanged { .. } | Event::ToolUpdated { .. }
                        ) | (Destination::Mcp, Event::McpServersChanged)
                            | (Destination::Skills, Event::SkillsChanged)
                            | (Destination::Memory, Event::MemoryUpdated)
                            | (Destination::Jobs, Event::JobsChanged)
                    );
                    if refresh {
                        self.load_page(destination);
                    }
                }
                if let Event::CompactionNotice { session_id, .. } = &event {
                    self.pending.retain(|_, p| !matches!(p, Pending::LiveContext(owner) if owner.session == *session_id));
                    self.outgoing.retain(|r| self.pending.contains_key(&r.id));
                }
                if matches!(&event, Event::CompactionNotice { .. })
                    || matches!(&event, Event::Note { text, .. } if text.starts_with("compaction failed:"))
                {
                    if let Some(session) = event.session_id() {
                        self.compacting
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(session);
                    }
                }
                if matches!(
                    event,
                    Event::ProviderStalled { .. }
                        | Event::ProviderRetrying { .. }
                        | Event::RunUsage { .. }
                        | Event::CompactionNotice { .. }
                        | Event::RunStarted { .. }
                        | Event::RunFinished { .. }
                        | Event::RunPaused { .. }
                        | Event::MessageComplete { .. }
                        | Event::StreamRestarted { .. }
                        | Event::Note { .. }
                ) {
                    if let Some(owner) = self
                        .action_owner()
                        .filter(|o| event.session_id() == Some(o.session.as_str()))
                    {
                        self.updates.push(Update::RunActivity {
                            owner,
                            event: event.clone(),
                        });
                        if matches!(
                            event,
                            Event::RunUsage { .. }
                                | Event::CompactionNotice { .. }
                                | Event::RunFinished { .. }
                        ) {
                            self.context_read();
                        }
                    }
                }
                if self.live_available && !self.reader.requested {
                    if let Some(session) = self.active.as_deref() {
                        let reload = self.live_overlay.event(session, &event);
                        self.dirty_session |= reload;
                        if reload || matches!(event, Event::RunStarted { .. }) {
                            self.emit_live_overlay();
                        }
                    }
                }
                let changed = match &event {
                    Event::SessionUpdated { session } => Some((
                        session.id.as_str(),
                        Some((
                            session.model.as_deref(),
                            session.reasoning_effort.as_deref(),
                        )),
                    )),
                    Event::SessionDeleted { id } => Some((id.as_str(), None)),
                    _ => None,
                };
                if let Some((changed, observed)) = changed {
                    let retired=self.pending.iter().filter_map(|(id,p)| {
                        let unsent=self.outgoing.iter().any(|r|r.id==*id);
                        let pair_changed=|change:&session_edit::Change|matches!(change,session_edit::Change::Selection{expected,..} if observed!=Some((expected.model.as_deref(),expected.reasoning_effort.as_deref())));
                        match p {Pending::InspectModelSelection{session_id,change,..} if session_id==changed && pair_changed(change)=>Some(*id),Pending::EditSession{session_id,change,..} if session_id==changed && change.is_selection() && unsent && pair_changed(change)=>Some(*id),_=>None}
                    }).collect::<Vec<_>>();
                    for id in retired {
                        self.outgoing.retain(|r| r.id != id);
                        if let Some(
                            Pending::InspectModelSelection {
                                operation,
                                session_id,
                                ..
                            }
                            | Pending::EditSession {
                                operation,
                                session_id,
                                ..
                            },
                        ) = self.pending.remove(&id)
                        {
                            self.updates.push(Update::SessionEdited{connection:self.connection,operation,session_id,outcome:session_edit::Outcome::Rejected("Session model/effort changed before the setting was written. Reload current values; nothing was sent.".into())});
                        }
                    }
                }
                if matches!(
                    &event,
                    Event::ApprovalRequested { .. }
                        | Event::ApprovalClosed { .. }
                        | Event::ApprovalAnswered { .. }
                        | Event::SessionDeleted { .. }
                ) {
                    self.invalidate_approvals()?;
                }

                if matches!(
                    event,
                    Event::RunStarted { .. }
                        | Event::RunFinished { .. }
                        | Event::RunPaused { .. }
                        | Event::QueueChanged { .. }
                        | Event::StepPending { .. }
                        | Event::StepResolved { .. }
                        | Event::SessionDeleted { .. }
                ) && event
                    .session_id()
                    .is_some_and(|id| Some(id) == self.active.as_deref())
                {
                    self.status_revision = self
                        .status_revision
                        .checked_add(1)
                        .ok_or("Session status revision exhausted")?;
                    // Retire forecasts synchronously, before any queued UI callback
                    // can commit one based on the old Run or Step inventory.
                    self.sync_epoch(self.epoch);
                    self.retire_step_reviews(true);
                    self.retire_mode_changes();
                    self.dirty_session = true;
                    self.updates.push(Update::SessionStatusInvalidated {
                        client: self.instance,
                        connection: self.connection,
                        generation: self.generation,
                        session_id: self.active.clone().expect("selected Session"),
                    });
                }
                if let Event::StepPending { session_id, .. } = &event {
                    // Historical event notice only. GetSession separately
                    // restores bounded read-only identities after reconnect;
                    // neither a preview nor an event authorizes a decision.
                    self.updates.push(Update::Notice(format!("Step review requested for Session {}. Review the complete arguments using Review Step; oversized or unsupported prompts require Formless TUI.",questions::preview(session_id,200))));
                }
                if matches!(
                    event,
                    Event::UserQuestion { .. } | Event::UserQuestionClosed { .. }
                ) {
                    self.questions.event(&event);
                    self.emit_questions();
                }
                let stream_handled = self.stream_event(&event);
                // No event mutates canonical text or asserts a Run state.
                // Unscoped gap notices conservatively invalidate everything.
                if !stream_handled
                    && event
                        .session_id()
                        .is_none_or(|id| Some(id) == self.active.as_deref())
                    && !matches!(
                        event,
                        Event::MessageDelta { .. }
                            | Event::ReasoningDelta { .. }
                            | Event::ToolProgress { .. }
                            | Event::TranscriptAttempt { .. }
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
            Pending::Attachment(work) => self.attachment_reply(work, result),
            Pending::RunReconcile(recovery) => self.run_reconciled(*recovery, result),
            Pending::RestoreSession(ticket) => {
                let acknowledged = response_ok
                    && result
                        .as_ref()
                        .is_ok_and(|v| v["id"] == ticket.owner.session);
                self.updates.push(Update::Notice(if acknowledged {
                    "Session restored.".into()
                } else {
                    "Restore outcome unknown or rejected. Reload Sessions; nothing was replayed."
                        .into()
                }));
                self.dirty_list = true;
                self.dirty_session = true;
            }
            Pending::RunTarget { owner, operation } => {
                if self.action_owner().as_ref() == Some(&owner) {
                    let result = result.and_then(|v| self.action_target(&owner.session, &v));
                    self.updates.push(Update::RunActionTarget {
                        owner,
                        operation,
                        result,
                    });
                }
            }
            Pending::RunInspect(w) => self.action_inspected(w, result, false),
            Pending::QueueInventory(w, commit) => self.queue_inventory(w, result, commit),
            Pending::RunCommitInspect(w) => self.action_inspected(w, result, true),
            Pending::RunForecast(w) => self.action_forecast(w, result),
            Pending::RunConsent(w) => self.action_consented(w, result),
            Pending::RunWrite(w) => {
                let acknowledged =
                    response_ok && result.as_ref().is_ok_and(|v| w.dispatch.acknowledged(v));
                if definite_rejection && w.dispatch.action.kind == crate::run_actions::Kind::Compact
                {
                    self.compacting
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&w.dispatch.action.owner.session);
                }
                if response_ok
                    && w.dispatch.action.kind == crate::run_actions::Kind::Delete
                    && result.as_ref().is_ok_and(|v| w.dispatch.acknowledged(v))
                {
                    if let Some(deadline) = result
                        .as_ref()
                        .ok()
                        .and_then(|v| v["undo_secs"].as_u64())
                        .and_then(|secs| w.sent_at?.checked_add(Duration::from_secs(secs)))
                    {
                        let ticket = crate::run_actions::DeleteUndo {
                            owner: w.dispatch.action.owner.clone(),
                            operation: uuid::Uuid::new_v4(),
                            deadline,
                        };
                        if ticket.available() {
                            self.delete_undo = Some(ticket.clone());
                            self.updates.push(Update::DeleteUndo(ticket));
                        }
                    }
                }
                if acknowledged || definite_rejection {
                    self.run_recoveries
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&w.dispatch.action.operation);
                } else if let Some(recovery) = self
                    .run_recoveries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(&w.dispatch.action.operation)
                {
                    recovery.replied = true;
                    recovery.error = result.as_ref().err().cloned();
                }
                w.dispatch.finish(result, definite_rejection);
                self.recover_run_actions();
                self.dirty_session = true;
                self.dirty_list = true;
            }
            Pending::LiveContext(owner) => self.context_received(owner, result),
            Pending::StreamSubscribe(owner) => self.stream_subscribed(owner, result),
            Pending::StreamBarrier {
                owner,
                questions_revision,
                status_revision,
            } => self.stream_loaded(owner, questions_revision, status_revision, result),
            Pending::StreamStatus {
                owner,
                questions_revision,
                status_revision,
            } => self.stream_status(owner, questions_revision, status_revision, result),
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
                self.topics_available = result["features"]
                    .as_array()
                    .is_some_and(|features| features.iter().any(|f| f == "topic_orchestration_v1"));
                self.updates
                    .push(Update::TopicSupport(self.topics_available));
                self.session_model_cas_available =
                    result["features"].as_array().is_some_and(|features| {
                        features.iter().any(|f| f == "session_model_compare_set_v1")
                    });
                self.live_available = result["features"].as_array().is_some_and(|features| {
                    features.iter().any(|feature| feature == "subscribe_filter")
                });
                self.reference_detach_available =
                    result["features"].as_array().is_some_and(|features| {
                        features.iter().any(|f| f == "reviewed_reference_detach_v1")
                    });
                self.reviewed_send_available =
                    result["features"].as_array().is_some_and(|features| {
                        [
                            "pending_attachment_inventory_v1",
                            "reviewed_attachment_send_v1",
                            formless_protocol::REVIEWED_SEND_MODE_FEATURE,
                        ]
                        .iter()
                        .all(|required| features.iter().any(|feature| feature == required))
                    });
                self.idle_step_off_available =
                    result["features"].as_array().is_some_and(|features| {
                        features
                            .iter()
                            .any(|feature| feature == formless_protocol::IDLE_STEP_OFF_FEATURE)
                    });
                self.reviewed_steer_run_available =
                    result["features"].as_array().is_some_and(|features| {
                        features
                            .iter()
                            .any(|feature| feature == formless_protocol::REVIEWED_STEER_RUN_FEATURE)
                    });
                self.reader.available = result["features"].as_array().is_some_and(|features| {
                    features
                        .iter()
                        .any(|feature| feature == "future_transcript_attempts_v1")
                });
                self.subscribe();
            }
            Pending::Subscribe => {
                result?;
                self.ready = true;
                self.recover_run_actions();
                self.dirty_list = true;
                self.dirty_session = true;
                self.updates.push(Update::Connection {
                    ready: true,
                    notice: if self.live_available && !self.reader.requested {
                        "Connected · provisional live text · canonical history on refresh"
                    } else {
                        "Connected · canonical messages · live token streaming unavailable"
                    }
                    .into(),
                });
                self.emit_questions();
                if self.topics_available {
                    self.topic_command(None, crate::topics::Action::Main);
                }
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
            Pending::Topic {
                operation,
                action,
                revision,
            } => {
                if operation.is_none() && revision != self.topics_revision {
                    return Ok(());
                }
                let result = result.and_then(|v| action.decode(v));
                if matches!(&result, Ok(crate::topics::Reply::Inventory(_))) {
                    // A fresh inventory can introduce a topic/harness transcript
                    // before its SessionUpdated event reaches this connection.
                    self.dirty_list = true;
                }
                if let crate::topics::Action::MoveAndStart(id) = &action {
                    if matches!(&result, Ok(crate::topics::Reply::Topic(t)) if t.id == *id && t.status == formless_protocol::TopicStatus::Approved)
                    {
                        // This continuation exists only for this explicit consent,
                        // never for a polled approval or a move-only operation.
                        self.topic_command(operation, crate::topics::Action::Start(id.clone()));
                        return Ok(());
                    }
                }
                if operation.is_some() {
                    self.dirty_list = true;
                    self.dirty_session = true;
                    self.topics_polled = None;
                }
                self.updates.push(Update::Topics {
                    connection: self.connection,
                    operation,
                    result,
                });
            }
            Pending::Snapshot {
                session_id,
                generation,
                questions_revision,
                status_revision,
            } => {
                if status_revision != self.status_revision {
                    self.dirty_session = true;
                    return Ok(());
                }
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
                    if let Ok(value) = &result {
                        self.updates.push(Update::TopicCandidates {
                            generation,
                            session_id: session_id.clone(),
                            messages: crate::topics::candidates(value),
                        });
                    }
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
                            self.context_read();
                            self.live_overlay
                                .snapshot(snapshot.steerable_run_id.as_deref(), snapshot.running);
                            self.emit_live_overlay();
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
            Pending::AttachmentInventory {
                operation,
                session_id,
            } => {
                let result=result.map_err(|_|"Pending-file inventory unavailable. Use Formless TUI or a daemon supporting pending_attachment_inventory_v1.".into()).and_then(|value|crate::pending_files::Report::decode(&session_id,value));
                self.updates.push(Update::PendingAttachments {
                    client: self.instance,
                    connection: self.connection,
                    operation,
                    session_id,
                    result,
                });
            }
            Pending::ModelCatalog { operation } => {
                self.updates.push(Update::ModelCatalog{instance:self.instance,connection:self.connection,operation,result:result.map_err(|_|"Catalog read failed. Provider details are not displayed; check configuration in Formless TUI.".into()).and_then(crate::model_catalog::Catalog::decode)});
            }
            Pending::Invocations { operation } => {
                self.updates.push(Update::Invocations {
                    connection: self.connection,
                    operation,
                    result: result.and_then(crate::invocations::Report::decode),
                });
            }
            Pending::ModeInspect { dispatch, revision } => {
                if !self.mode_current(&dispatch)
                    || !dispatch.reserved()
                    || revision != self.status_revision
                {
                    dispatch.reject("Session/Run/Step changed; no mode setting was sent.");
                } else {
                    let value = result.as_ref().ok().filter(|v| {
                        v["session"]["id"].as_str() == Some(&dispatch.action().session_id)
                    });
                    let sampled = value
                        .and_then(|v| v.get("step_mode"))
                        .and_then(Value::as_bool);
                    match sampled {
                        Some(on) if on == dispatch.action().on => {
                            dispatch.observed_target();
                            self.invalidate_mode_metadata()?;
                        }
                        Some(_) => {
                            let idle = value.is_some_and(|v| v["running"].as_bool() == Some(false) && v.get("unfinished_run").is_some_and(Value::is_null) && matches!(crate::steps::Inventory::decode(&dispatch.action().session_id,v.get("pending_steps")),crate::steps::Inventory::Snapshot(p) if p.prompts.is_empty() && !p.more));
                            if !dispatch.action().on && !idle {
                                dispatch.reject("Disable requires a fresh idle Session and complete empty Step inventory. No setting was sent.");
                                self.invalidate_mode_metadata()?;
                            } else {
                                let session_id = dispatch.action().session_id.clone();
                                let request = if dispatch.action().on {
                                    Request::SetStepMode {
                                        session_id,
                                        on: true,
                                    }
                                } else {
                                    Request::DisableStepModeIfIdle { session_id }
                                };
                                self.request(request, Pending::ModeCommit { dispatch, revision });
                            }
                        }
                        None => {
                            dispatch.reject("Current Step mode is unavailable or unsupported; no setting was sent.");
                            self.invalidate_mode_metadata()?;
                        }
                    }
                }
            }
            Pending::ModeCommit { dispatch, .. } => {
                dispatch.finish(crate::step_mode::decode(
                    dispatch.action(),
                    result,
                    response_ok,
                ));
                if self.active.as_deref() == Some(&dispatch.action().session_id) {
                    self.invalidate_mode_metadata()?;
                }
                self.dirty_session = true;
            }
            Pending::ApprovalRead {
                operation,
                revision,
            } => {
                let result = if revision == self.approval_revision {
                    result.and_then(|v| {
                        crate::approvals::Inventory::from_list(&v).map_err(str::to_string)
                    })
                } else {
                    Err("Approvals changed while reading; reload the current inventory.".into())
                };
                self.updates.push(Update::ApprovalInventory {
                    client: self.instance,
                    connection: self.connection,
                    operation,
                    revision: self.approval_revision,
                    result,
                });
            }
            Pending::ApprovalInspect { dispatch, revision } => {
                let exact = result
                    .as_ref()
                    .ok()
                    .and_then(|v| crate::approvals::Inventory::from_list(v).ok())
                    .and_then(|inventory| inventory.find(dispatch.action().prompt.id()).cloned())
                    .is_some_and(|p| p == dispatch.action().prompt);
                if !self.approval_current(&dispatch)
                    || !dispatch.reserved()
                    || revision != self.approval_revision
                    || !exact
                {
                    dispatch.reject("The exact complete approval could not be revalidated. No decision was sent; use a fresh review or Formless TUI.");
                } else {
                    self.request(
                        dispatch.action().request(),
                        Pending::ApprovalCommit { dispatch, revision },
                    );
                }
            }
            Pending::ApprovalCommit { dispatch, .. } => {
                dispatch.finish(crate::approvals::decode(result, response_ok));
                self.invalidate_approvals()?;
                self.invalidate_mode_metadata()?;
                self.dirty_list = true;
            }
            Pending::StepInspect(dispatch) => {
                let action = dispatch.action();
                let valid=result.as_ref().is_ok_and(|value| {
                    value["session"]["id"].as_str()==Some(action.prompt.session_id.as_str()) &&
                    matches!(crate::steps::Inventory::decode(&action.prompt.session_id,value.get("pending_steps")),crate::steps::Inventory::Snapshot(snapshot) if !snapshot.more && snapshot.prompts.len()==1 && snapshot.prompts.first()==Some(&action.prompt))
                });
                if !self.step_current(&dispatch) || !dispatch.reserved() || !valid {
                    dispatch.reject("The exact complete Step prompt could not be revalidated. No decision was sent; inspect Formless TUI or review again.");
                } else {
                    self.request(dispatch.action().request(), Pending::StepCommit(dispatch));
                }
            }
            Pending::StepCommit(dispatch) => {
                dispatch.finish(crate::step_decision::decode(
                    dispatch.action(),
                    result,
                    response_ok,
                ));
                self.dirty_session = true;
            }
            Pending::ForkSession(action) => {
                let outcome = session_fork::decode(&action, result);
                let applied = matches!(outcome, session_fork::Outcome::Applied(_));
                self.forks.finish(&action, outcome);
                if applied {
                    self.dirty_list = true;
                }
            }
            Pending::RunInspection { operation, target } => {
                let result =
                    result.and_then(|value| crate::run_inspection::Report::decode(&target, value));
                self.updates.push(Update::RunInspection {
                    connection: self.connection,
                    operation,
                    target,
                    result,
                });
            }
            Pending::SessionComparison { operation, pair } => {
                let result =
                    result.and_then(|value| crate::comparison::Report::decode(&pair, value));
                self.updates.push(Update::SessionComparison {
                    connection: self.connection,
                    operation,
                    pair,
                    result,
                });
            }
            Pending::SessionUsage { target, operation } => {
                self.updates.push(Update::SessionUsage {
                    target,
                    operation,
                    result: result.and_then(session_usage::Report::decode),
                });
            }
            Pending::SessionExport {
                operation,
                session_id,
                format,
            } => {
                self.updates.push(Update::SessionExport {
                    connection: self.connection,
                    operation,
                    session_id,
                    format,
                    result: result.and_then(|value| session_export::decode(format, value)),
                });
            }
            Pending::ProfileSearch { operation, query } => {
                self.updates.push(Update::ProfileSearch {
                    connection: self.connection,
                    operation,
                    query,
                    result: result.and_then(profile_search::decode),
                })
            }
            Pending::Manage {
                destination,
                label,
                undo,
                run_job,
                add,
            } => {
                let succeeded = result.is_ok();
                let notice = match result {
                    Ok(value)
                        if value
                            .get("error")
                            .and_then(Value::as_str)
                            .is_some_and(|error| !error.is_empty()) =>
                    {
                        format!("Could not {label}: {}", value["error"].as_str().unwrap())
                    }
                    Ok(value) if run_job => match value
                        .get("status")
                        .and_then(Value::as_str)
                        .filter(|status| !status.is_empty())
                    {
                        Some(status) => format!("{label}: {status}"),
                        None => format!("Could not {label}: daemon returned no execution status."),
                    },
                    Ok(value) if undo => {
                        let summary = value
                            .get("undone")
                            .and_then(Value::as_str)
                            .unwrap_or("registry change");
                        format!("Undone: {summary}")
                    }
                    Ok(value) => match value
                        .get("warning")
                        .and_then(Value::as_str)
                        .filter(|warning| !warning.is_empty())
                    {
                        Some(warning) => format!("Done: {label} · Warning: {warning}"),
                        None => format!("Done: {label}"),
                    },
                    Err(error) => format!("Could not {label}: {error}"),
                };
                self.updates.push(Update::ManagementNotice {
                    destination,
                    notice,
                });
                if succeeded {
                    if let Some((text, schedule)) = add {
                        self.updates.push(Update::ResourceAdded {
                            destination,
                            text,
                            schedule,
                        });
                    }
                }
                if self.page == Some(destination) {
                    self.load_page(destination);
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
            Pending::InspectModelSelection {
                operation,
                session_id,
                change,
            } => {
                let fresh =
                    result.and_then(|v| session_edit::Metadata::from_list_reply(&session_id, &v));
                match fresh {
                    Ok(metadata) if change.matches(&metadata) => {
                        self.invalidate_mode_metadata()?;
                        self.dirty_list=true;
                        self.updates.push(Update::SessionEdited{connection:self.connection,operation,session_id,outcome:session_edit::Outcome::ObservedSelection(metadata)});
                    }
                    Ok(metadata) if matches!(&change,session_edit::Change::Selection{expected,..} if metadata.selection.as_ref()==Some(expected)) => {
                        self.invalidate_mode_metadata()?;
                        self.request(change.request(&session_id),Pending::EditSession{operation,session_id,change});
                    }
                    _ => self.updates.push(Update::SessionEdited{connection:self.connection,operation,session_id,outcome:session_edit::Outcome::Rejected("The paired model/effort selection changed or could not be revalidated. Reload current details; no setting was sent.".into())}),
                }
            }
            Pending::EditSession {
                operation,
                session_id,
                change,
            } => {
                let model_selection = change.is_selection();
                let outcome = match result {
                    Ok(value) => {
                        let exposes_instructions = value.get("instructions").is_some();
                        let exposes_selection =
                            value.get("model").is_some() && value.get("reasoning_effort").is_some();
                        let field = match &change {
                            session_edit::Change::Name(_) => "name",
                            session_edit::Change::Tags(_) => "tags",
                            session_edit::Change::Pin(_) => "pinned",
                            session_edit::Change::Archive(_) => "archived",
                            session_edit::Change::Instructions(_) => "instructions",
                            session_edit::Change::Selection { .. } => "model",
                        };
                        let exposes_changed_field =
                            value.get(field).is_some() && (!model_selection || exposes_selection);
                        serde_json::from_value::<SessionInfo>(value)
                            .ok()
                            .filter(|session| exposes_changed_field && session.id == session_id)
                            .and_then(|session| {
                                session_edit::Metadata::from_session_reply(
                                    &session,
                                    exposes_instructions,
                                    exposes_selection,
                                )
                                .ok()
                            })
                            .filter(|metadata| change.matches(metadata))
                            .map_or(
                                session_edit::Outcome::Unknown,
                                session_edit::Outcome::Applied,
                            )
                    }
                    Err(_) if response_ok => session_edit::Outcome::Unknown,
                    Err(error) => session_edit::Outcome::Rejected(error),
                };
                if model_selection {
                    self.invalidate_mode_metadata()?;
                    if matches!(outcome, session_edit::Outcome::Unknown) {
                        self.updates.push(Update::Notice(format!("Session {session_id} model/effort save outcome unknown. Nothing was retried; reload its current selection before saving again.")));
                    }
                }
                self.updates.push(Update::SessionEdited {
                    connection: self.connection,
                    operation,
                    session_id,
                    outcome,
                });
                self.dirty_list = true;
            }
            Pending::ReadSessionDetails {
                operation,
                session_id,
            } => {
                let result = result
                    .and_then(|value| session_edit::Metadata::from_list_reply(&session_id, &value));
                self.updates.push(Update::SessionDetails {
                    connection: self.connection,
                    operation,
                    session_id,
                    result,
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
            Pending::DetachReference(d) => {
                d.finish(crate::reference_detach::decode(
                    d.action(),
                    result,
                    response_ok,
                ));
            }
            Pending::SendInventory(work) => self.send_inventory_loaded(work, result),
            Pending::ReviewedAction { action, work, .. } => {
                self.dirty_session = true;
                self.dirty_list = true;
                self.updates.push(Update::Action {session_id: action.session_id, result: if result.as_ref().is_ok_and(|value| work.acknowledges_reviewed_send(value)) {Ok(())} else {Err("Reviewed message outcome is unknown. It may be persisted. It was not retried; check this Session in Formless TUI before starting more work".into())}});
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
                        } else if work.attachments.is_some() {
                            PreparationStage::NeedsReview
                        } else {
                            PreparationStage::Ready
                        };
                        self.updates
                            .push(Update::Prepared(Box::new(forecast::Prepared {
                                client: work.client,
                                connection: work.connection,
                                attachments: work.attachments.clone(),
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
    uncertain: &mut HashSet<Action>,
) -> io::Result<()> {
    while let Some(request) = engine.outgoing.pop_front() {
        if !engine.action_before_write(request.id) {
            continue;
        }
        if let Some(Pending::RunWrite(w)) = engine.pending.get(&request.id) {
            uncertain.insert(Action {
                id: w.dispatch.action.operation,
                session_id: w.dispatch.action.owner.session.clone(),
                question_id: None,
            });
        }
        if let Some(Pending::RestoreSession(ticket)) = engine.pending.get(&request.id) {
            uncertain.insert(Action {
                id: ticket.operation,
                session_id: ticket.owner.session.clone(),
                question_id: None,
            });
        }
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
    run_recoveries: Arc<std::sync::Mutex<HashMap<uuid::Uuid, run_actions::Recovery>>>,
    compacting: Arc<std::sync::Mutex<HashSet<String>>>,
    instance: uuid::Uuid,
    stream_epoch: Arc<AtomicU64>,
    future_attempts: bool,
    active: (Option<String>, u64),
    page: Option<Destination>,
    uncertain: HashSet<Action>,
    forks: Arc<session_fork::Ledger>,
}
impl Retained {
    fn retire_connection(&mut self) {
        self.compacting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
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
        run_recoveries,
        compacting,
        active,
        page,
        uncertain,
        forks,
        stream_epoch,
        future_attempts,
        instance,
    } = retained;
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = Frames::new(BufReader::new(reader));
    let _fork_connection_guard =
        session_fork::ConnectionGuard::new(forks.clone(), connection.load(Ordering::Acquire));
    let mut engine = Engine {
        run_recoveries: run_recoveries.clone(),
        compacting: compacting.clone(),
        instance: *instance,
        reader: reader::Reader::new(*future_attempts),
        stream_epoch: stream_epoch.load(Ordering::Acquire),
        forks: forks.clone(),
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
    let mut delivery = delivery::Delivery::default();
    let flush_timer = tokio::time::sleep(Duration::from_secs(86400));
    tokio::pin!(flush_timer);
    let mut armed_deadline = None;
    let mut attachment_conversions = tokio::task::JoinSet::new();
    loop {
        engine.sync_stream_epoch(stream_epoch.load(Ordering::Acquire));
        engine.sync_epoch(epoch.load(Ordering::Acquire));
        if attachment_conversions.is_empty() {
            if let Some(job) = engine.attachment_conversions.pop_front() {
                attachment_conversions.spawn_blocking(move || job.convert());
            }
        }
        if *future_attempts {
            delivery.before_io(
                tx,
                tokio::time::Instant::now(),
                !engine.outgoing.is_empty(),
                |owner| engine.owns_stream(owner),
            )?;
        }
        for id in engine.pending.keys() {
            deadlines.entry(*id).or_insert_with(std::time::Instant::now);
        }
        deadlines.retain(|id, _| engine.pending.contains_key(id));
        if deadlines.values().any(|at| at.elapsed() >= REQUEST_TIMEOUT) {
            return Err("A daemon request timed out; its action outcome may be unknown.".into());
        }
        // Mutations are never replayed. Retain Run/question identities before
        // writes, which may partially succeed. The metadata editor instead
        // freezes on connection change and requires an explicit fresh read.
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
        engine.retire_reference_writes();
        engine.before_approval_writes()?;
        engine.retire_step_reviews(false);
        engine.retire_mode_changes();
        engine.before_mode_writes()?;
        for pending in engine.pending.values() {
            if let Pending::StepCommit(dispatch) = pending {
                if !dispatch.before_write() {
                    return Err(
                        "Step reservation retired before writing; no retry was sent.".into(),
                    );
                }
            }
            if let Pending::ForkSession(action) = pending {
                if !forks.before_write(action) {
                    return Err(
                        "Fork reservation changed before writing; nothing was retried.".into(),
                    );
                }
            }
        }
        engine.retire_reference_writes();
        for pending in engine.pending.values() {
            if let Pending::DetachReference(d) = pending {
                if !d.sent() && !d.before_write() {
                    return Err(
                        "Reference reservation retired before writing; nothing was retried.".into(),
                    );
                }
                uncertain.insert(Action {
                    id: d.action().operation,
                    session_id: d.action().session_id.clone(),
                    question_id: None,
                });
            }
        }
        for pending in engine.pending.values_mut() {
            if let Pending::ReviewedAction { action, sent, .. } = pending {
                uncertain.insert(action.clone());
                *sent = true;
            }
        }
        write_requests(&mut writer, &mut engine, uncertain)
            .await
            .map_err(|e| e.to_string())?;
        if *future_attempts {
            delivery.drain(tx, &mut engine, tokio::time::Instant::now())?;
        } else {
            deliver(tx, &mut engine)?;
        }
        let deadline = delivery.deadline();
        if deadline != armed_deadline {
            if let Some(at) = deadline {
                flush_timer.as_mut().reset(at);
            }
            armed_deadline = deadline;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            command = commands.recv() => { engine.sync_stream_epoch(stream_epoch.load(Ordering::Acquire)); engine.sync_epoch(epoch.load(Ordering::Acquire)); match command {
                None => return Ok(()),
                Some(Command::Reconnect) => return Err("Reconnecting at the user's request; actions are not retried.".into()),
                Some(Command::Select { session_id, generation }) => { *active = (session_id.clone(), generation); engine.select(session_id, generation); }
                Some(Command::Page(destination)) => { *page = Some(destination); engine.load_page(destination); }
                Some(command) if engine.blocked_by_unknown(&command, uncertain) => {
                    if let Command::PrepareRunAction(d) = &command { d.reject("A previous Session action is unknown. Nothing was sent; inspect Formless TUI."); }
                    if let Command::DetachReference(d)=&command {d.reject("A previous Session action is unknown. No reference removal was sent; continue in Formless TUI.");}

                    let target=match &command {Command::Intent(intent)|Command::Review{intent,..}=>Some(intent.session_id().to_string()),Command::CommitPrepared(id)=>engine.prepared.as_ref().filter(|p|p.id==*id).map(|p|p.intent.session_id().to_string()),_=>None};
                    if let Some(session_id)=target {engine.updates.push(Update::Action{session_id,result:Err("A previous action's outcome is unknown. No new action was sent; check the Session in Formless TUI before retrying".into())});}
                    engine.updates.push(Update::Notice("A previous action's outcome is unknown. It was not retried: deduplication does not survive reconnect. Check this Session in the TUI before starting more work.".into()));
                }
                Some(command) => engine.command(command),
            } },
            _ = &mut flush_timer, if deadline.is_some() => {},
            message = reader.next() => match message.map_err(|e| e.to_string())? {
                Some(message) => {
                    engine.sync_stream_epoch(stream_epoch.load(Ordering::Acquire));
                    engine.sync_epoch(epoch.load(Ordering::Acquire));
                    let answered = match &message { ServerMessage::Response { id, ok, result, .. } => match engine.pending.get(id) {
                        Some(Pending::RestoreSession(t)) if *ok && result.as_ref().is_some_and(|v| v["id"] == t.owner.session) => Some(Action { id: t.operation, session_id: t.owner.session.clone(), question_id: None }),
                        Some(Pending::RunWrite(w)) if run_actions::definite_rejection(&message) || (*ok && result.as_ref().is_some_and(|v| w.dispatch.acknowledged(v))) => Some(Action { id: w.dispatch.action.operation, session_id: w.dispatch.action.owner.session.clone(), question_id: None }),
                        Some(Pending::DetachReference(d)) if *ok && result.as_ref().is_some_and(|v|crate::reference_detach::acknowledged(d.action(),v))=>Some(Action{id:d.action().operation,session_id:d.action().session_id.clone(),question_id:None}),
                        Some(Pending::Action(action)) => Some(action.clone()),
                        Some(Pending::ReviewedAction {action,work,..}) if *ok && result.as_ref().is_some_and(|value| work.acknowledges_reviewed_send(value)) => Some(action.clone()),
                        Some(Pending::Answer(action)) if !ok || result.as_ref().and_then(|value| value["answered"].as_bool()).is_some() => Some(action.clone()),
                        _ => None }, _ => None };
                    engine.receive(message)?;
                    for action in engine.reconciled_actions.drain(..) { uncertain.remove(&action); }
                    ready.store(engine.ready, Ordering::Release);
                    if let Some(id) = answered { uncertain.remove(&id); }
                }
                None => return Err("Daemon disconnected. Run state is unknown; reconnecting.".into()),
            },
            Some(converted) = attachment_conversions.join_next(), if !attachment_conversions.is_empty() => {
                engine.complete_clipboard_conversion(converted.map_err(|error| format!("Clipboard image conversion worker failed: {error}"))?);
            }
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

struct WorkerState {
    instance: uuid::Uuid,
    stream_epoch: Arc<AtomicU64>,
    future_attempts: bool,
    ready: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    connection: Arc<AtomicU64>,
    forks: Arc<session_fork::Ledger>,
}

fn disconnected_update(command: Command, fallback: &str) -> Update {
    match command {
        Command::Topic { connection, operation, .. } => Update::Topics {
            connection, operation, result: Err("Connection lost. Topic action was not replayed; refresh to inspect its outcome.".into()),
        },
        Command::ReadInvocations {
            connection,
            operation,
        } => Update::Invocations {
            // Match the unsent command, even if the worker already advanced
            // its connection. The caller decides whether this owner is stale.
            connection,
            operation,
            result: Err("Not connected; the Invocation read was not sent. Read Invocations again explicitly after reconnecting.".into()),
        },
        _ => Update::Notice(fallback.into()),
    }
}

async fn worker(
    endpoint: Endpoint,
    mut commands: queue::Receiver<Command>,
    tx: SyncSender<Update>,
    mut stop: watch::Receiver<bool>,
    state: WorkerState,
) {
    let WorkerState {
        ready,
        epoch,
        connection,
        forks,
        stream_epoch,
        future_attempts,
        instance,
    } = state;
    let mut retained = Retained {
        forks,
        stream_epoch: stream_epoch.clone(),
        future_attempts,
        instance,
        ..Default::default()
    };
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
                Command::ForkSession(action) => retained.forks.reject_unsent(
                    &action,
                    "Fork was not written before disconnect. Review again explicitly.",
                ),
                command => {
                    let _ = tx.try_send(disconnected_update(
                        command,
                        "A queued action was not sent after disconnect. Draft kept.",
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
        retained.retire_connection();
        advance_epoch(&epoch);
        advance_epoch(&connection);
        advance_epoch(&stream_epoch);
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
                    Some(command) => { let _ = tx.try_send(disconnected_update(command, "Not connected; the action was not sent. Draft kept.")); }
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

    #[test]
    fn metadata_reads_reject_missing_duplicate_or_malformed_target_without_loading_history() {
        let session = crate::fixture::sessions(1).remove(0);
        for value in [
            json!([]),
            json!([session.clone(), session.clone()]),
            json!({"session":session.clone()}),
            json!([{"id":session.id}]),
        ] {
            let mut engine = ready();
            let operation = uuid::Uuid::new_v4();
            engine.command(Command::ReadSessionDetails {
                connection: 0,
                operation,
                session_id: session.id.clone(),
            });
            let request = engine.outgoing.pop_back().unwrap();
            assert!(matches!(request.request, Request::ListSessions));
            respond(&mut engine, request.id, value);
            assert!(engine.updates.iter().any(|update|matches!(update,Update::SessionDetails {operation:op,result:Err(_),..} if *op==operation)));
        }
    }

    #[test]
    fn session_metadata_uses_frozen_target_and_validates_operation_specific_replies() {
        for valid in [true, false] {
            let mut engine = ready();
            engine.active = Some("another-session".into());
            let mut session = crate::fixture::sessions(1).remove(0);
            let target = session.id.clone();
            session.name = "Renamed ไทย 😀".into();
            let operation = uuid::Uuid::new_v4();
            engine.command(Command::EditSession {
                connection: 0,
                operation,
                session_id: target.clone(),
                change: session_edit::Change::Name(session.name.clone()),
            });
            let request = engine.outgoing.pop_back().unwrap();
            assert!(
                matches!(&request.request,Request::RenameSession {id,name} if id==&target && name=="Renamed ไทย 😀")
            );
            engine.command(Command::EditSession {
                connection: 0,
                operation: uuid::Uuid::new_v4(),
                session_id: target.clone(),
                change: session_edit::Change::Pin(true),
            });
            assert!(
                engine.outgoing.is_empty(),
                "no duplicate while this Session's save is pending"
            );
            if !valid {
                session.id = "foreign".into();
            }
            respond(
                &mut engine,
                request.id,
                serde_json::to_value(&session).unwrap(),
            );
            assert!(engine.updates.iter().any(|update|matches!(update,Update::SessionEdited {operation:id,outcome,..} if *id==operation && if valid {matches!(outcome,session_edit::Outcome::Applied(_))} else {matches!(outcome,session_edit::Outcome::Unknown)})));
            engine.command(Command::EditSession {
                connection: 1,
                operation: uuid::Uuid::new_v4(),
                session_id: target.clone(),
                change: session_edit::Change::Pin(true),
            });
            assert!(
                engine.outgoing.is_empty(),
                "retired connection cannot dispatch metadata changes"
            );
            let read = uuid::Uuid::new_v4();
            engine.command(Command::ReadSessionDetails {
                connection: 0,
                operation: read,
                session_id: target.clone(),
            });
            let request = engine.outgoing.pop_back().unwrap();
            assert!(matches!(request.request, Request::ListSessions));
            respond(&mut engine, request.id, json!([session]));
            assert!(engine.updates.iter().any(|update|matches!(update,Update::SessionDetails {operation,result,..} if *operation==read && result.is_ok()==valid)));
        }
    }

    #[test]
    fn metadata_missing_or_mismatched_success_is_unknown() {
        for reply in [
            Value::Null,
            json!({"pinned":true}),
            serde_json::to_value(crate::fixture::sessions(1).remove(0)).unwrap(),
        ] {
            let mut engine = ready();
            let session = crate::fixture::sessions(1).remove(0);
            let id = session.id;
            engine.command(Command::EditSession {
                connection: 0,
                operation: uuid::Uuid::new_v4(),
                session_id: id,
                change: session_edit::Change::Name("Desired different name".into()),
            });
            let request = engine.outgoing.pop_back().unwrap();
            respond(&mut engine, request.id, reply);
            assert!(engine.updates.iter().any(|update| matches!(
                update,
                Update::SessionEdited {
                    outcome: session_edit::Outcome::Unknown,
                    ..
                }
            )));
        }
    }

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

    #[test]
    fn lifecycle_retires_forecast_and_late_callbacks_without_changing_stream_epoch() {
        for ready_stage in [false, true] {
            let mut engine = ready();
            engine.active = Some("a".into());
            let id = prepare(&mut engine, false);
            if ready_stage {
                engine.command(Command::ConfirmPrepared(id));
            }
            let stream_epoch = engine.stream_epoch;
            engine
                .receive(ServerMessage::Event(Event::RunStarted {
                    session_id: "a".into(),
                    run_id: "run".into(),
                }))
                .unwrap();
            assert!(engine.prepared.is_none());
            engine.command(Command::ConfirmPrepared(id));
            engine.command(Command::CommitPrepared(id));
            assert!(!engine.outgoing.iter().any(|r| matches!(
                r.request,
                Request::SendMessage { .. }
                    | Request::SendReviewedMessage { .. }
                    | Request::SendReviewedSteer { .. }
            )));
            assert_eq!(engine.stream_epoch, stream_epoch);
        }
        let mut engine = ready();
        engine.active = Some("a".into());
        engine.command(Command::Review {
            intent: Intent::SendMessage {
                session_id: "a".into(),
                content: "draft".into(),
                mode: SendMode::Steer,
            },
            expected_run_id: Some("run".into()),
            draft_basis: "draft".into(),
            epoch: engine.epoch,
        });
        let pending = engine.outgoing.pop_back().unwrap();
        engine
            .receive(ServerMessage::Event(Event::StepResolved {
                session_id: "a".into(),
                step_id: "step".into(),
            }))
            .unwrap();
        respond(
            &mut engine,
            pending.id,
            serde_json::to_value(crate::fixture::run_forecast("a", false)).unwrap(),
        );
        assert!(engine.prepared.is_none());
    }

    #[test]
    fn attachment_only_send_reviews_exact_inventory_and_rejects_empty_or_failed_reads() {
        let attachment = json!({"id":"artifact","name":"image.png","path":"/artifacts/image","mime":"image/png","bytes":3,"future":{"kept":true}});
        for inventory in [
            Ok(json!({"session_id":"a","attachments":[attachment.clone()]})),
            Ok(json!({"session_id":"a","attachments":[]})),
            Err("inventory unavailable".to_string()),
        ] {
            let accepted = inventory
                .as_ref()
                .is_ok_and(|value| value["attachments"].as_array().unwrap().len() == 1);
            let mut engine = ready();
            engine.active = Some("a".into());
            engine.command(Command::Review {
                intent: Intent::SendMessage {
                    session_id: "a".into(),
                    content: String::new(),
                    mode: SendMode::Send,
                },
                expected_run_id: None,
                draft_basis: String::new(),
                epoch: engine.epoch,
            });
            let request = engine.outgoing.pop_back().unwrap();
            assert!(matches!(
                request.request,
                Request::ListPendingAttachments { .. }
            ));
            engine
                .receive(match inventory {
                    Ok(value) => ServerMessage::ok(request.id, value),
                    Err(error) => ServerMessage::err(request.id, error),
                })
                .unwrap();
            if accepted {
                let request = engine.outgoing.pop_back().unwrap();
                assert!(matches!(request.request, Request::ForecastRun { .. }));
                respond(
                    &mut engine,
                    request.id,
                    serde_json::to_value(crate::fixture::run_forecast("a", false)).unwrap(),
                );
                let prepared = engine
                    .updates
                    .iter()
                    .find_map(|update| match update {
                        Update::Prepared(prepared) => Some(prepared),
                        _ => None,
                    })
                    .unwrap();
                let id = prepared.id;
                assert_eq!(
                    prepared.attachments.as_ref().unwrap().expected(),
                    vec![attachment.clone()]
                );
                engine.command(Command::ConfirmPrepared(id));
                engine.command(Command::CommitPrepared(id));
                assert!(engine.outgoing.iter().any(|request| matches!(&request.request, Request::SendReviewedMessage { content, expected_attachments, .. } if content.is_empty() && expected_attachments == &vec![attachment.clone()])));
            } else {
                assert!(engine.outgoing.is_empty());
                assert!(engine
                    .updates
                    .iter()
                    .any(|update| matches!(update, Update::PreparationFailed { .. })));
            }
        }
    }

    fn prepare(engine: &mut Engine, required: bool) -> uuid::Uuid {
        engine.active = Some("a".into());
        engine.command(Command::Review {
            intent: Intent::SendMessage {
                session_id: "a".into(),
                content: "keep exact draft ไทย".into(),
                mode: SendMode::Steer,
            },
            expected_run_id: Some("run".into()),
            draft_basis: "keep exact draft ไทย".into(),
            epoch: engine.epoch,
        });
        let inventory = engine.outgoing.pop_back().unwrap();
        assert!(matches!(inventory.request, Request::ReadSteerReview { .. }));
        respond(
            engine,
            inventory.id,
            json!({"session_id":"a","run_id":"run","attachments":[]}),
        );
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
            engine.command(Command::CommitPrepared(id));
            assert!(
                engine.outgoing.is_empty(),
                "review cannot be skipped, even when no price consent is needed"
            );
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
            if !required {
                engine.command(Command::ConfirmPrepared(id));
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
                matches!(&engine.outgoing[0].request, Request::SendReviewedSteer { content, run_id, .. } if content == "keep exact draft ไทย" && run_id == "run")
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
            assert!(!engine.outgoing.iter().any(|request| matches!(
                request.request,
                Request::SendMessage { .. }
                    | Request::SendReviewedMessage { .. }
                    | Request::SendReviewedSteer { .. }
            )));
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
        engine.active = Some("a".into());
        engine.command(Command::Review {
            intent: Intent::ResumeRun {
                session_id: "a".into(),
            },
            expected_run_id: None,
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
                expected_run_id: None,
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
            reference_ledger: Arc::new(crate::reference_detach::Ledger::default()),
            step_ledger: Arc::new(crate::step_decision::Ledger::default()),
            mode_ledger: Arc::new(crate::step_mode::Ledger::default()),
            approval_ledger: Arc::new(crate::approvals::Ledger::default()),
            commands,
            updates,
            stop,
            ready: Arc::new(AtomicBool::new(true)),
            epoch: Arc::new(AtomicU64::new(0)),
            connection: Arc::new(AtomicU64::new(0)),
            forks: Arc::new(session_fork::Ledger::default()),
            stream_epoch: Arc::new(AtomicU64::new(0)),
            instance: uuid::Uuid::new_v4(),
            future_attempts: false,
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

    #[test]
    fn topic_inventory_discovers_new_transcripts_without_starting_work() {
        let mut engine = ready();
        engine.topic_command(None, crate::topics::Action::List);
        let request = engine.outgoing.pop_back().unwrap();
        assert!(matches!(request.request, Request::ListTopics));
        let (_, topic) = crate::fixture::topic_workspace();
        respond(&mut engine, request.id, json!([topic]));
        assert!(engine.dirty_list, "new transcript rows must be discovered");
        assert!(engine.outgoing.is_empty(), "inventory never starts work");
    }

    fn ready() -> Engine {
        let mut engine = Engine::default();
        engine.begin();
        let ping = engine.outgoing.pop_front().unwrap();
        assert!(matches!(ping.request, Request::Ping));
        respond(
            &mut engine,
            ping.id,
            json!({"min_protocol":1,"max_protocol":1,"features":["pending_attachment_inventory_v1","reviewed_attachment_send_v1","reviewed_send_mode_v1","reviewed_steer_run_v1"]}),
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

    #[test]
    fn production_deltas_require_subscription_capability_and_do_not_enable_future_reader() {
        for supported in [false, true] {
            let mut engine = Engine::default();
            engine.begin();
            let ping = engine.outgoing.pop_front().unwrap();
            respond(
                &mut engine,
                ping.id,
                json!({"min_protocol":1,"max_protocol":1,"features":if supported {vec!["subscribe_filter"]} else {vec![]}}),
            );
            let subscribe = engine.outgoing.pop_front().unwrap();
            assert!(matches!(subscribe.request,Request::Subscribe{deltas,..} if deltas==supported));
            respond(&mut engine, subscribe.id, json!({"subscribed":true}));
            assert!(engine.updates.iter().any(|u| matches!(u,
                Update::Connection { ready: true, notice } if notice == if supported {
                    "Connected · provisional live text · canonical history on refresh"
                } else {
                    "Connected · canonical messages · live token streaming unavailable"
                }
            )));
            assert!(!engine.reader.requested);
            engine.active = Some("s".into());
            engine
                .receive(ServerMessage::Event(Event::MessageDelta {
                    session_id: "s".into(),
                    run_id: "r".into(),
                    delta: "live".into(),
                }))
                .unwrap();
            engine.emit_live_overlay();
            assert_eq!(
                engine
                    .updates
                    .iter()
                    .any(|u| matches!(u,Update::LiveOverlay{view,..} if view.answer=="live")),
                supported
            );
            engine.updates.clear();
            engine.select(Some("other".into()), 2);
            engine.emit_live_overlay();
            assert!(!engine
                .updates
                .iter()
                .any(|u| matches!(u, Update::LiveOverlay { .. })));
        }
    }

    #[test]
    fn clearing_instructions_requires_an_explicit_matching_acknowledgement_field() {
        for mode in [
            "missing",
            "null",
            "wrong",
            "oversized",
            "foreign",
            "malformed",
        ] {
            let mut engine = ready();
            let session = crate::fixture::sessions(1).remove(0);
            let id = session.id.clone();
            let operation = uuid::Uuid::new_v4();
            engine.command(Command::EditSession {
                connection: 0,
                operation,
                session_id: id.clone(),
                change: session_edit::Change::Instructions(None),
            });
            let request = engine.outgoing.pop_front().unwrap();
            assert!(
                matches!(request.request,Request::SetSessionInstructions{id:target,instructions:None} if target==id)
            );
            let mut value = serde_json::to_value(session).unwrap();
            match mode {
                "missing" => {
                    value.as_object_mut().unwrap().remove("instructions");
                }
                "null" => value["instructions"] = Value::Null,
                "wrong" => value["instructions"] = json!("different"),
                "oversized" => {
                    value["instructions"] = json!("x".repeat(session_edit::MAX_INPUT + 1))
                }
                "foreign" => value["id"] = json!("other"),
                _ => value["instructions"] = json!(false),
            }
            respond(&mut engine, request.id, value);
            assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionEdited{operation:ack,outcome,..} if *ack==operation && match outcome {session_edit::Outcome::Applied(_)=>mode=="null",session_edit::Outcome::Unknown=>mode!="null",_=>false})),"{mode}");
        }
    }

    #[test]
    fn default_valued_setters_require_their_field_but_accept_explicit_defaults() {
        for (change, key, value) in [
            (session_edit::Change::Pin(false), "pinned", json!(false)),
            (
                session_edit::Change::Archive(false),
                "archived",
                json!(false),
            ),
            (session_edit::Change::Tags(Vec::new()), "tags", json!([])),
            (
                session_edit::Change::Instructions(None),
                "instructions",
                Value::Null,
            ),
        ] {
            for exposed in [false, true] {
                let mut engine = ready();
                let session = crate::fixture::sessions(1).remove(0);
                let id = session.id.clone();
                let operation = uuid::Uuid::new_v4();
                engine.command(Command::EditSession {
                    connection: 0,
                    operation,
                    session_id: id,
                    change: change.clone(),
                });
                let request = engine.outgoing.pop_front().unwrap();
                let mut reply = serde_json::to_value(session).unwrap();
                reply[key] = value.clone();
                if !exposed {
                    reply.as_object_mut().unwrap().remove(key);
                }
                respond(&mut engine, request.id, reply);
                assert!(engine.updates.iter().any(|update|matches!(update,Update::SessionEdited{operation:ack,outcome,..} if *ack==operation && match outcome {session_edit::Outcome::Applied(_)=>exposed,session_edit::Outcome::Unknown=>!exposed,_=>false})),"{key} exposed={exposed}");
            }
        }
    }

    #[test]
    fn a_missing_instructions_read_disables_only_that_field_and_is_not_an_empty_value() {
        for exposed in [false, true] {
            let mut engine = ready();
            let session = crate::fixture::sessions(1).remove(0);
            let id = session.id.clone();
            let operation = uuid::Uuid::new_v4();
            engine.command(Command::ReadSessionDetails {
                connection: 0,
                operation,
                session_id: id,
            });
            let request = engine.outgoing.pop_front().unwrap();
            let mut value = serde_json::to_value(session).unwrap();
            value["instructions"] = Value::Null;
            if !exposed {
                value.as_object_mut().unwrap().remove("instructions");
            }
            respond(&mut engine, request.id, json!([value]));
            let metadata = engine
                .updates
                .iter()
                .find_map(|update| match update {
                    Update::SessionDetails {
                        operation: ack,
                        result: Ok(metadata),
                        ..
                    } if *ack == operation => Some(metadata),
                    _ => None,
                })
                .unwrap();
            assert_eq!(metadata.instructions.text(), exposed.then_some(""));
            assert!(session_edit::Change::from_input(
                session_edit::Field::Name,
                "Name remains editable",
                metadata
            )
            .is_ok());
        }
    }

    #[test]
    fn usage_reads_validate_current_target_but_replies_keep_their_original_target() {
        let mut engine = ready();
        engine.select(Some("a".into()), 7);
        engine.outgoing.clear();
        let target = session_usage::Target {
            session_id: "a".into(),
            connection: 0,
            generation: 7,
        };
        let operations = [
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        ];
        for operation in operations {
            engine.command(Command::ReadSessionUsage {
                target: target.clone(),
                operation,
            });
        }
        assert_eq!(engine.outgoing.len(), 2);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionUsage{operation,result:Err(_),..} if *operation==operations[2])));
        let a = engine.outgoing.pop_front().unwrap();
        let b = engine.outgoing.pop_front().unwrap();
        assert!(matches!(&a.request,Request::SessionUsage{session_id} if session_id=="a"));
        engine.select(Some("b".into()), 8);
        engine.outgoing.clear();
        let counters = json!({"steps":1,"prompt_tokens":30,"completion_tokens":4,"cached_tokens":10,"cost":null});
        respond(
            &mut engine,
            b.id,
            json!({"total":counters,"last_run":null,"runs":1}),
        );
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionUsage{target:actual,operation,result:Ok(_)} if *actual==target&&*operation==operations[1])));
        respond(&mut engine, a.id, Value::Null);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionUsage{operation,result:Err(_),..} if *operation==operations[0])));
        let count = engine.updates.len();
        respond(&mut engine, b.id, Value::Null);
        assert_eq!(count, engine.updates.len());
        for wrong in [
            target.clone(),
            session_usage::Target {
                session_id: "b".into(),
                connection: 1,
                generation: 8,
            },
            session_usage::Target {
                session_id: "b".into(),
                connection: 0,
                generation: 9,
            },
        ] {
            engine.command(Command::ReadSessionUsage {
                target: wrong,
                operation: uuid::Uuid::new_v4(),
            });
            assert!(engine.outgoing.is_empty());
        }
    }

    #[test]
    fn invocations_are_bounded_explicit_reads_with_owned_out_of_order_replies() {
        let mut engine = ready();
        let operations = [
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        ];
        for operation in operations {
            engine.command(Command::ReadInvocations {
                connection: 0,
                operation,
            });
        }
        assert_eq!(engine.outgoing.len(), 2);
        assert!(engine.updates.iter().any(|update|matches!(update,Update::Invocations{operation,result:Err(_),..} if *operation==operations[2])));
        let a = engine.outgoing.pop_front().unwrap();
        let b = engine.outgoing.pop_front().unwrap();
        assert!(matches!(a.request, Request::ListInvocations));
        assert!(matches!(b.request, Request::ListInvocations));
        engine.select(Some("another-session".into()), 99);
        respond(&mut engine, b.id, crate::invocations::test_value());
        assert!(engine.updates.iter().any(|update|matches!(update,Update::Invocations{connection:0,operation,result:Ok(report)} if *operation==operations[1]&&report.keys[0].session_id=="session-a")));
        let count = engine.updates.len();
        respond(&mut engine, b.id, crate::invocations::test_value());
        assert_eq!(engine.updates.len(), count);
        respond(&mut engine, a.id, Value::Null);
        assert!(engine.updates.iter().any(|update|matches!(update,Update::Invocations{operation,result:Err(_),..} if *operation==operations[0])));
        engine.outgoing.clear();
        let stale = uuid::Uuid::new_v4();
        engine.command(Command::ReadInvocations {
            connection: 1,
            operation: stale,
        });
        assert!(engine.outgoing.is_empty());
        assert!(matches!(engine.updates.last(),
            Some(Update::Invocations { connection: 1, operation, result: Err(_) }) if *operation == stale));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn offline_invocation_reads_keep_their_owner_and_are_not_replayed() {
        struct SocketDir(PathBuf);
        impl Drop for SocketDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        async fn next_update(rx: &Receiver<Update>, deadline: tokio::time::Instant) -> Update {
            tokio::time::timeout_at(deadline, async {
                loop {
                    match rx.try_recv() {
                        Ok(update) => return update,
                        Err(mpsc::TryRecvError::Empty) => {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Err(error) => panic!("private worker stopped: {error}"),
                    }
                }
            })
            .await
            .expect("private worker update deadline")
        }
        fn assert_offline(update: Update, generation: u64, id: uuid::Uuid) {
            assert!(
                matches!(&update, Update::Invocations { connection, operation, result: Err(error) }
                    if *connection == generation && *operation == id && error.contains("not sent")),
                "{update:?}"
            );
        }

        let root =
            std::env::temp_dir().join(format!("fl-di-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        std::fs::create_dir(&root).unwrap();
        let root = SocketDir(root);
        let endpoint = Endpoint(root.0.join("s"));
        let (commands, incoming) = queue::channel(QUEUE);
        let (updates, rx) = mpsc::sync_channel(QUEUE);
        let (stop, cancelled) = watch::channel(false);
        let ready = Arc::new(AtomicBool::new(false));
        let connection = Arc::new(AtomicU64::new(7));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let queued = uuid::Uuid::new_v4();
        commands
            .try_send(Command::ReadInvocations {
                connection: 6,
                operation: queued,
            })
            .unwrap();
        let task = tokio::spawn(worker(
            endpoint.clone(),
            incoming,
            updates,
            cancelled,
            WorkerState {
                instance: uuid::Uuid::new_v4(),
                stream_epoch: Arc::default(),
                future_attempts: false,
                ready: ready.clone(),
                epoch: Arc::default(),
                connection: connection.clone(),
                forks: Arc::default(),
            },
        ));
        assert_offline(next_update(&rx, deadline).await, 6, queued);
        assert!(
            matches!(next_update(&rx, deadline).await, Update::Connection { ready: false, notice } if notice.starts_with("Connecting"))
        );
        assert!(
            matches!(next_update(&rx, deadline).await, Update::Connection { ready: false, notice } if notice.starts_with("Cannot connect"))
        );
        let offline_generation = connection.load(Ordering::Acquire);
        let during_backoff = uuid::Uuid::new_v4();
        commands
            .try_send(Command::ReadInvocations {
                connection: offline_generation,
                operation: during_backoff,
            })
            .unwrap();
        loop {
            if let update @ Update::Invocations { .. } = next_update(&rx, deadline).await {
                assert_offline(update, offline_generation, during_backoff);
                break;
            }
        }
        assert!(!ready.load(Ordering::Acquire));

        // The endpoint exists only after both failures. Its wire log must
        // contain the one explicit read issued after the new handshake.
        let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut server = BufReader::new(socket);
            let mut count = 0;
            let mut line = String::new();
            while server.read_line(&mut line).await.unwrap() != 0 {
                let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let result = match request.request {
                    Request::Ping => json!({"min_protocol":1,"max_protocol":1}),
                    Request::Subscribe { .. } => json!({"subscribed":true}),
                    Request::ListSessions => json!([]),
                    Request::ListInvocations => {
                        count += 1;
                        crate::invocations::test_value()
                    }
                    other => panic!("unexpected private fixture request: {other:?}"),
                };
                let mut reply = serde_json::to_vec(&ServerMessage::ok(request.id, result)).unwrap();
                reply.push(b'\n');
                server.write_all(&reply).await.unwrap();
                line.clear();
            }
            count
        });
        let fresh_generation = loop {
            if matches!(
                next_update(&rx, deadline).await,
                Update::Connection { ready: true, .. }
            ) {
                break connection.load(Ordering::Acquire);
            }
        };
        assert!(fresh_generation >= offline_generation);
        assert!(ready.load(Ordering::Acquire));
        let fresh = uuid::Uuid::new_v4();
        commands
            .try_send(Command::ReadInvocations {
                connection: fresh_generation,
                operation: fresh,
            })
            .unwrap();
        loop {
            if let Update::Invocations {
                connection,
                operation,
                result,
            } = next_update(&rx, deadline).await
            {
                assert_eq!((connection, operation), (fresh_generation, fresh));
                assert_eq!(result.unwrap().keys[0].session_id, "session-a");
                break;
            }
        }
        stop.send(true).unwrap();
        let invocation_requests = tokio::time::timeout_at(deadline, async {
            task.await.unwrap();
            peer.await.unwrap()
        })
        .await
        .expect("private worker shutdown deadline");
        assert_eq!(invocation_requests, 1);
        assert!(!rx
            .try_iter()
            .any(|update| matches!(update, Update::Invocations { .. })));
    }

    #[test]
    fn session_fork_engine_requires_a_reservation_and_exact_lineage_receipt() {
        use session_fork::{Action as Fork, Outcome, Status};
        let mut engine = ready();
        let action = Fork {
            connection: 0,
            operation: uuid::Uuid::new_v4(),
            source_id: "source".into(),
        };
        engine.command(Command::ForkSession(action.clone()));
        assert!(engine.outgoing.is_empty());
        engine.forks.reserve(action.clone()).unwrap();
        engine.command(Command::ForkSession(action.clone()));
        engine.command(Command::ForkSession(action.clone()));
        assert_eq!(
            engine.outgoing.len(),
            1,
            "a duplicate reserved command must not create a second request"
        );
        let request = engine.outgoing.pop_front().unwrap();
        assert!(
            matches!(&request.request,Request::ForkSession{id,name:None,up_to_seq:None} if id=="source")
        );
        assert!(engine.forks.before_write(&action));
        respond(
            &mut engine,
            request.id,
            json!({"id":"child","forked_from":"source","fork_point_seq":7}),
        );
        assert!(matches!(
            engine.forks.status(),
            Status::Finished {
                outcome: Outcome::Applied(session_fork::Receipt {
                    fork_point_seq: 7,
                    ..
                }),
                ..
            }
        ));
        assert!(
            engine.forks.reserve(action.clone()).is_err(),
            "a completed operation UUID cannot be reused"
        );
        let second = Fork {
            operation: uuid::Uuid::new_v4(),
            ..action
        };
        engine.forks.reserve(second.clone()).unwrap();
        engine.command(Command::ForkSession(second.clone()));
        let request = engine.outgoing.pop_front().unwrap();
        assert!(engine.forks.before_write(&second));
        respond(
            &mut engine,
            request.id,
            json!({"id":"foreign","forked_from":"other","fork_point_seq":1}),
        );
        assert!(matches!(
            engine.forks.status(),
            Status::Finished {
                outcome: Outcome::Unknown(_),
                ..
            }
        ));
        assert!(engine
            .forks
            .reserve(Fork {
                operation: uuid::Uuid::new_v4(),
                ..second
            })
            .is_err());
    }
    #[test]
    fn session_fork_full_queue_is_known_unsent_and_never_clears_a_sent_reservation() {
        let (commands, mut receiver) = queue::channel(QUEUE);
        let (_, updates) = mpsc::sync_channel(QUEUE);
        let (stop, _) = watch::channel(false);
        let client = Client {
            reference_ledger: Arc::new(crate::reference_detach::Ledger::default()),
            step_ledger: Arc::new(crate::step_decision::Ledger::default()),
            mode_ledger: Arc::new(crate::step_mode::Ledger::default()),
            approval_ledger: Arc::new(crate::approvals::Ledger::default()),
            commands,
            updates,
            stop,
            ready: Arc::new(AtomicBool::new(true)),
            epoch: Arc::new(AtomicU64::new(0)),
            connection: Arc::new(AtomicU64::new(0)),
            forks: Arc::new(session_fork::Ledger::default()),
            stream_epoch: Arc::new(AtomicU64::new(0)),
            instance: uuid::Uuid::new_v4(),
            future_attempts: false,
            #[cfg(feature = "test-support")]
            recorded: None,
        };
        for _ in 0..QUEUE {
            client.send(Command::Refresh).unwrap();
        }
        let action = session_fork::Action {
            connection: 0,
            operation: uuid::Uuid::new_v4(),
            source_id: "source".into(),
        };
        assert!(client.send(Command::ForkSession(action.clone())).is_err());
        assert!(matches!(
            client.fork_status(),
            session_fork::Status::Finished {
                outcome: session_fork::Outcome::Rejected(_),
                ..
            }
        ));
        while let Ok(command) = receiver.try_recv() {
            assert!(!matches!(command, Command::ForkSession(_)));
        }
        let next = session_fork::Action {
            operation: uuid::Uuid::new_v4(),
            ..action
        };
        client.send(Command::ForkSession(next.clone())).unwrap();
        assert!(client.send(Command::ForkSession(next.clone())).is_err());
        assert!(client.forks.before_write(&next));
        client.forks.reject_unsent(&next, "late error");
        assert!(matches!(
            client.fork_status(),
            session_fork::Status::Sent(_)
        ));
    }

    #[tokio::test]
    async fn session_fork_unknown_write_survives_reconnect_and_unrelated_acknowledgements() {
        let mut retained = Retained::default();
        let frames = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let action = session_fork::Action {
            connection: 0,
            operation: uuid::Uuid::new_v4(),
            source_id: "source".into(),
        };
        for reconnect in [false, true] {
            let (stream, server) = tokio::io::duplex(32768);
            let (commands_tx, mut commands) = queue::channel(QUEUE);
            let (updates, _receiver) = mpsc::sync_channel(QUEUE);
            let (_stop, mut cancelled) = watch::channel(false);
            let ready = Arc::new(AtomicBool::new(false));
            let epoch = AtomicU64::new(0);
            let connection = AtomicU64::new(u64::from(reconnect));
            let fork_ledger = retained.forks.clone();
            let driver_ready = ready.clone();
            let driver_action = action.clone();
            let driver = tokio::spawn(async move {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !driver_ready.load(Ordering::Acquire) {
                    assert!(std::time::Instant::now() < deadline);
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                if !reconnect {
                    fork_ledger.reserve(driver_action.clone()).unwrap();
                    commands_tx
                        .send(Command::ForkSession(driver_action))
                        .await
                        .unwrap();
                } else {
                    assert!(fork_ledger
                        .reserve(session_fork::Action {
                            connection: 1,
                            operation: uuid::Uuid::new_v4(),
                            source_id: "different-source".into()
                        })
                        .is_err());
                    // Even a command bypassing the normal Client reservation
                    // cannot replay an old fork into a new Engine.
                    commands_tx
                        .send(Command::ForkSession(driver_action))
                        .await
                        .unwrap();
                    commands_tx
                        .send(Command::Intent(Intent::InterruptRun {
                            session_id: "source".into(),
                        }))
                        .await
                        .unwrap();
                    commands_tx
                        .send(Command::InspectRun {
                            connection: 1,
                            operation: uuid::Uuid::new_v4(),
                            target: crate::run_inspection::Target {
                                run_id: "run".into(),
                                session_id: "source".into(),
                            },
                        })
                        .await
                        .unwrap();
                }
                commands_tx
            });
            let counted = frames.clone();
            let peer = tokio::spawn(async move {
                let mut server = BufReader::new(server);
                let mut stopped = false;
                let mut inspected = false;
                loop {
                    let mut line = String::new();
                    assert_ne!(server.read_line(&mut line).await.unwrap(), 0);
                    let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                    let result = match request.request {
                        Request::Ping => json!({"min_protocol":1,"max_protocol":1}),
                        Request::Subscribe { deltas: false, .. } => json!({"subscribed":true}),
                        Request::ListSessions => json!([]),
                        Request::ForkSession {
                            id,
                            name: None,
                            up_to_seq: None,
                        } => {
                            assert!(!reconnect, "fork replayed after reconnect");
                            assert_eq!(id, "source");
                            counted.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                        Request::InterruptRun { .. } if reconnect => {
                            stopped = true;
                            json!({"interrupted":true})
                        }
                        Request::GetRun { run_id } if reconnect => {
                            inspected = true;
                            crate::run_inspection::test_value(&crate::run_inspection::Target {
                                run_id,
                                session_id: "source".into(),
                            })
                        }
                        other => panic!("unexpected fork fixture request {other:?}"),
                    };
                    let mut frame =
                        serde_json::to_vec(&ServerMessage::ok(request.id, result)).unwrap();
                    frame.push(b'\n');
                    server.get_mut().write_all(&frame).await.unwrap();
                    if stopped && inspected {
                        break;
                    }
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
            assert!(result.is_err());
            peer.await.unwrap();
            let _keep_sender = driver.await.unwrap();
            assert!(matches!(
                retained.forks.status(),
                session_fork::Status::Finished {
                    outcome: session_fork::Outcome::Unknown(_),
                    ..
                }
            ));
        }
        assert_eq!(frames.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn run_inspections_bind_exact_run_and_session_with_bounded_outstanding_reads() {
        use crate::run_inspection::{test_value, Target};
        let target = Target {
            run_id: "run-a".into(),
            session_id: "session-a".into(),
        };
        let mut engine = ready();
        let operations = [
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        ];
        for operation in operations {
            engine.command(Command::InspectRun {
                connection: 0,
                operation,
                target: target.clone(),
            });
        }
        assert_eq!(engine.outgoing.len(), 2);
        assert!(engine.updates.iter().any(|update|matches!(update,Update::RunInspection{operation,result:Err(_),..} if *operation==operations[2])));
        let first = engine.outgoing.pop_front().unwrap();
        let second = engine.outgoing.pop_front().unwrap();
        assert!(matches!(&first.request,Request::GetRun{run_id} if run_id=="run-a"));
        engine.select(Some("unrelated".into()), 99);
        respond(&mut engine, second.id, test_value(&target));
        assert!(engine.updates.iter().any(|update|matches!(update,Update::RunInspection{connection:0,operation,target:actual,result:Ok(_)} if *operation==operations[1]&&actual==&target)));
        let count = engine.updates.len();
        respond(&mut engine, second.id, test_value(&target));
        assert_eq!(engine.updates.len(), count);
        let mut foreign = test_value(&target);
        foreign["session_id"] = "foreign".into();
        respond(&mut engine, first.id, foreign);
        assert!(engine.updates.iter().any(|update|matches!(update,Update::RunInspection{operation,result:Err(_),..} if *operation==operations[0])));
        engine.outgoing.clear();
        engine.command(Command::InspectRun {
            connection: 1,
            operation: uuid::Uuid::new_v4(),
            target: target.clone(),
        });
        engine.command(Command::InspectRun {
            connection: 0,
            operation: uuid::Uuid::new_v4(),
            target: Target {
                run_id: "".into(),
                ..target
            },
        });
        assert!(engine.outgoing.is_empty());
    }

    #[test]
    fn comparisons_are_explicit_bounded_reads_with_request_owned_replies() {
        use crate::comparison::{test_value, Pair};
        let mut engine = ready();
        let pair = Pair {
            left: "left".into(),
            right: "right".into(),
        };
        let operations = [
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        ];
        for operation in operations {
            engine.command(Command::CompareSessions {
                connection: 0,
                operation,
                pair: pair.clone(),
            });
        }
        assert_eq!(engine.outgoing.len(), 2);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionComparison{operation,result:Err(_),..} if *operation==operations[2])));
        let a = engine.outgoing.pop_front().unwrap();
        let b = engine.outgoing.pop_front().unwrap();
        assert!(
            matches!(&a.request,Request::CompareSessions{left,right} if left=="left"&&right=="right")
        );
        engine.select(Some("unrelated".into()), 99);
        respond(&mut engine, b.id, test_value(&pair));
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionComparison{connection:0,operation,pair:actual,result:Ok(_)} if *operation==operations[1]&&actual==&pair)));
        let count = engine.updates.len();
        respond(&mut engine, b.id, test_value(&pair));
        assert_eq!(engine.updates.len(), count);
        let mut wrong = test_value(&pair);
        wrong["right"]["session"]["id"] = "foreign".into();
        respond(&mut engine, a.id, wrong);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionComparison{operation,result:Err(_),..} if *operation==operations[0])));
        engine.outgoing.clear();
        engine.command(Command::CompareSessions {
            connection: 1,
            operation: uuid::Uuid::new_v4(),
            pair: pair.clone(),
        });
        engine.command(Command::CompareSessions {
            connection: 0,
            operation: uuid::Uuid::new_v4(),
            pair: Pair {
                left: pair.left.clone(),
                right: pair.left,
            },
        });
        assert!(engine.outgoing.is_empty());
    }

    #[test]
    fn exports_use_request_identity_not_selected_session_and_bound_outstanding_reads() {
        use session_export::Format;
        let mut engine = ready();
        let operations = [
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        ];
        for (index, operation) in operations.into_iter().enumerate() {
            engine.command(Command::ExportSession {
                connection: 0,
                operation,
                session_id: format!("target-{index}"),
                format: Format::Markdown,
            });
        }
        assert_eq!(engine.outgoing.len(), 2);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionExport{operation,result:Err(_),..} if *operation==operations[2])));
        let a = engine.outgoing.pop_front().unwrap();
        let b = engine.outgoing.pop_front().unwrap();
        assert!(
            matches!(&a.request,Request::ExportSession{id,format:Some(format)} if id=="target-0"&&format=="markdown")
        );
        engine.select(Some("unrelated".into()), 99);
        respond(
            &mut engine,
            b.id,
            json!({"name":"B","format":"markdown","content":"exact ไทย 😀"}),
        );
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionExport{connection:0,operation,session_id,format:Format::Markdown,result:Ok(document)} if *operation==operations[1]&&session_id=="target-1"&&document.text=="exact ไทย 😀")));
        respond(&mut engine, a.id, Value::Null);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::SessionExport{operation,session_id,result:Err(_),..} if *operation==operations[0]&&session_id=="target-0")));
        let count = engine.updates.len();
        respond(
            &mut engine,
            b.id,
            json!({"name":"B","format":"markdown","content":"duplicate"}),
        );
        assert_eq!(count, engine.updates.len());
        engine.outgoing.clear();
        for (connection, id) in [(1, "valid"), (0, "")] {
            engine.command(Command::ExportSession {
                connection,
                operation: uuid::Uuid::new_v4(),
                session_id: id.into(),
                format: Format::Json,
            });
            assert!(engine.outgoing.is_empty());
        }
    }

    #[test]
    fn profile_search_is_bounded_read_only_and_its_replies_keep_the_original_query() {
        let mut engine = ready();
        let operations = [
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        ];
        for (i, operation) in operations.into_iter().enumerate() {
            engine.command(Command::SearchProfile {
                connection: 0,
                operation,
                query: format!(" query-{i} "),
            });
        }
        assert_eq!(engine.outgoing.len(), 2);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::ProfileSearch{operation,result:Err(_),..} if operation==&operations[2])));
        let first = engine.outgoing.pop_front().unwrap();
        let second = engine.outgoing.pop_front().unwrap();
        assert!(
            matches!(&first.request,Request::SearchSessions{query,limit:Some(50)} if query=="query-0")
        );
        assert!(
            matches!(&second.request,Request::SearchSessions{query,limit:Some(50)} if query=="query-1")
        );
        engine.select(Some("unrelated-session".into()), 99);
        respond(
            &mut engine,
            second.id,
            json!([{"session_id":"s","session_name":"ไทย 😀","message_id":"m","seq":1,"role":"assistant","snippet":"found","created_at":"now"}]),
        );
        assert!(engine.updates.iter().any(|u|matches!(u,Update::ProfileSearch{connection:0,operation,query,result:Ok(hits)} if *operation==operations[1] && query=="query-1" && hits[0].session_id=="s")));
        respond(&mut engine, first.id, Value::Null);
        assert!(engine.updates.iter().any(|u|matches!(u,Update::ProfileSearch{operation,query,result:Err(_),..} if *operation==operations[0] && query=="query-0")));
        engine.outgoing.clear();
        for (connection, query) in [
            (1, "valid".to_string()),
            (0, " ".into()),
            (0, "x".repeat(257)),
        ] {
            engine.command(Command::SearchProfile {
                connection,
                operation: uuid::Uuid::new_v4(),
                query,
            });
            assert!(engine.outgoing.is_empty());
        }
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
    fn delayed_delta_and_duplicate_completion_after_snapshot_never_create_a_phantom_turn() {
        let mut engine = ready();
        engine.select(Some("a".into()), 1);
        let id = snapshot_request(&mut engine);
        let canonical = snapshot("a", "already persisted ไทย 😀");
        respond(&mut engine, id, canonical.clone());
        // Persistence and publication have separate producer boundaries.
        // Neither the delta nor this duplicate completion supplies a cutoff.
        engine
            .receive(ServerMessage::Event(Event::MessageDelta {
                session_id: "a".into(),
                run_id: "same-run".into(),
                delta: "already persisted".into(),
            }))
            .unwrap();
        engine
            .receive(ServerMessage::Event(Event::MessageComplete {
                session_id: "a".into(),
                run_id: "same-run".into(),
                message: serde_json::from_value(canonical["messages"][0].clone()).unwrap(),
            }))
            .unwrap();
        assert!(engine.dirty_session);
        let id = snapshot_request(&mut engine);
        respond(&mut engine, id, canonical);
        let snapshots = engine
            .updates
            .iter()
            .filter_map(|update| match update {
                Update::Snapshot { snapshot, .. } => Some(snapshot),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(snapshots.len(), 2);
        for snapshot in snapshots {
            assert_eq!(snapshot.blocks.len(), 1);
            assert_eq!(snapshot.blocks[0].markdown(), "already persisted ไทย 😀");
        }
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
        assert!(
            engine.outgoing.is_empty(),
            "raw Send cannot bypass native review"
        );
        let id = prepare(&mut engine, false);
        engine.command(Command::CommitPrepared(id));
        assert!(engine.outgoing.is_empty());
        engine.command(Command::ConfirmPrepared(id));
        engine.command(Command::CommitPrepared(id));
        engine.command(Command::CommitPrepared(id));
        assert_eq!(engine.outgoing.len(), 1);
        let request = engine.outgoing.pop_front().unwrap();
        assert!(matches!(request.request, Request::SendReviewedSteer { .. }));
        engine.command(Command::Intent(Intent::InterruptRun {
            session_id: "a".into(),
        }));
        assert!(matches!(
            engine.outgoing.back().unwrap().request,
            Request::InterruptRun { .. }
        ));
        respond(
            &mut engine,
            request.id,
            json!({"steered":true,"run_id":"run"}),
        );
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

    #[test]
    fn managed_pages_use_their_list_requests_and_deduplicate_pending_reads() {
        for destination in [
            Destination::Tools,
            Destination::Mcp,
            Destination::Skills,
            Destination::Memory,
            Destination::Jobs,
        ] {
            let mut engine = ready();
            engine.load_page(destination);
            engine.load_page(destination);
            assert_eq!(engine.outgoing.len(), 1);
            let request = engine.outgoing.pop_front().unwrap();
            assert_eq!(
                serde_json::to_value(request.request).unwrap(),
                serde_json::to_value(crate::manage::list_request(destination).unwrap()).unwrap()
            );
            respond(&mut engine, request.id, json!([]));
            assert!(
                matches!(engine.updates.last(), Some(Update::Page { destination: d, result: Ok(_), .. }) if *d == destination)
            );
        }
    }

    #[test]
    fn management_reports_receipts_and_refreshes_only_the_current_destination() {
        for (success, current) in [
            (true, Destination::Skills),
            (false, Destination::Skills),
            (true, Destination::Jobs),
        ] {
            let mut engine = ready();
            engine.page = Some(current);
            engine.command(Command::Manage {
                destination: Destination::Skills,
                request: Request::RemoveSkill { name: "foo".into() },
                label: "remove skill foo".into(),
            });
            let request = engine.outgoing.pop_front().unwrap();
            assert!(matches!(request.request, Request::RemoveSkill { name } if name == "foo"));
            if success {
                respond(&mut engine, request.id, json!({}));
            } else {
                engine
                    .receive(ServerMessage::err(request.id, "permission denied".into()))
                    .unwrap();
            }
            let expected = if success {
                "Done: remove skill foo"
            } else {
                "Could not remove skill foo: permission denied"
            };
            assert!(engine
                .updates
                .iter()
                .any(|u| matches!(u, Update::ManagementNotice { destination: Destination::Skills, notice } if notice == expected)));
            assert_eq!(
                engine.outgoing.len(),
                usize::from(current == Destination::Skills)
            );
            if current == Destination::Skills {
                assert!(matches!(engine.outgoing[0].request, Request::ListSkills));
            }
            assert_eq!(engine.page, Some(current));
        }
    }

    #[test]
    fn management_reports_payload_errors_job_statuses_and_warnings() {
        let cases = [
            (
                Request::RestartMcpServer {
                    name: "demo".into(),
                },
                json!({"restarted":"demo", "tools":null, "error":"startup failed"}),
                "Could not action: startup failed",
            ),
            (
                Request::RestartMcpServer {
                    name: "demo".into(),
                },
                json!({"restarted":"demo", "tools":0, "error":null}),
                "Done: action",
            ),
            (
                Request::AddMcpServer {
                    name: "demo".into(),
                    server: json!({}),
                },
                json!({"added":"demo", "tools":null, "error":"startup failed"}),
                "Could not action: startup failed",
            ),
            (
                Request::RunJobNow { id: "j".into() },
                json!({"id":"j", "status":"skipped: the previous firing is still running"}),
                "action: skipped: the previous firing is still running",
            ),
            (
                Request::RunJobNow { id: "j".into() },
                json!({"id":"j", "status":"prompt delivery failed: provider unavailable"}),
                "action: prompt delivery failed: provider unavailable",
            ),
            (
                Request::RunJobNow { id: "j".into() },
                json!({"id":"j", "status":"started in session s"}),
                "action: started in session s",
            ),
            (
                Request::RunJobNow { id: "j".into() },
                json!({}),
                "Could not action: daemon returned no execution status.",
            ),
            (
                Request::RunJobNow { id: "j".into() },
                json!({"status":""}),
                "Could not action: daemon returned no execution status.",
            ),
            (
                Request::SetJobEnabled {
                    id: "j".into(),
                    enabled: true,
                },
                json!({"id":"j", "enabled":true, "warning":"Keep the daemon running"}),
                "Done: action · Warning: Keep the daemon running",
            ),
        ];
        for (request, payload, expected) in cases {
            let mut engine = ready();
            engine.command(Command::Manage {
                destination: Destination::Jobs,
                request,
                label: "action".into(),
            });
            let outgoing = engine.outgoing.pop_front().unwrap();
            respond(&mut engine, outgoing.id, payload);
            assert!(engine.updates.iter().any(|update| matches!(update, Update::ManagementNotice { notice, .. } if notice == expected)), "{:?}", engine.updates);
        }
    }

    #[test]
    fn add_receipts_only_clear_drafts_after_success() {
        for success in [false, true] {
            let mut engine = ready();
            engine.command(Command::Manage {
                destination: Destination::Jobs,
                request: crate::manage::add_job("Review", "bad schedule").unwrap(),
                label: "add job".into(),
            });
            let outgoing = engine.outgoing.pop_front().unwrap();
            if success {
                respond(&mut engine, outgoing.id, json!({"id":"j"}));
            } else {
                engine
                    .receive(ServerMessage::err(
                        outgoing.id,
                        "unparseable schedule".into(),
                    ))
                    .unwrap();
            }
            assert_eq!(engine.updates.iter().any(|update| matches!(update, Update::ResourceAdded { destination: Destination::Jobs, text, schedule } if text == "Review" && schedule == "bad schedule")), success);
        }
    }

    #[test]
    fn management_requires_ready_and_undo_names_the_reverted_change() {
        let mut engine = Engine::default();
        engine.command(Command::Manage {
            destination: Destination::Tools,
            request: Request::UndoRegistryChange,
            label: "undo last change".into(),
        });
        assert!(engine.outgoing.is_empty());
        assert!(
            matches!(engine.updates.last(), Some(Update::ManagementNotice { destination: Destination::Tools, notice }) if notice.contains("Could not undo last change") && notice.contains("Not connected"))
        );
        let mut engine = ready();
        engine.page = Some(Destination::Tools);
        engine.command(Command::Manage {
            destination: Destination::Tools,
            request: Request::UndoRegistryChange,
            label: "undo last change".into(),
        });
        let request = engine.outgoing.pop_front().unwrap();
        assert!(matches!(request.request, Request::UndoRegistryChange));
        respond(
            &mut engine,
            request.id,
            json!({"undone":"user retired fetch_page", "tool":{"name":"fetch_page"}}),
        );
        assert!(
            matches!(engine.updates.last(), Some(Update::ManagementNotice { destination: Destination::Tools, notice }) if notice == "Undone: user retired fetch_page")
        );
        assert!(matches!(engine.outgoing[0].request, Request::ListTools));
    }

    #[test]
    fn resource_events_reload_only_the_matching_page_and_coalesce_reads() {
        let registry: Event = serde_json::from_value(json!({"type":"registry_changed", "change":{"tool_id":"t","name":"tool","action":"retired","from_version":1,"to_version":1,"author":"user","at":"now"}})).unwrap();
        let tool: Event = serde_json::from_value(json!({"type":"tool_updated", "tool":{"id":"t","name":"tool","description":"","retired":false,"current_version":1,"capabilities":[],"memory_mb":1,"invocations":0,"success_rate":null,"needs_attention":false,"created_at":"","updated_at":""}})).unwrap();
        let cases = [
            (Destination::Tools, registry),
            (Destination::Tools, tool),
            (Destination::Mcp, Event::McpServersChanged),
            (Destination::Skills, Event::SkillsChanged),
            (Destination::Memory, Event::MemoryUpdated),
            (Destination::Jobs, Event::JobsChanged),
        ];
        for destination in [
            Destination::Tools,
            Destination::Mcp,
            Destination::Skills,
            Destination::Memory,
            Destination::Jobs,
            Destination::Settings,
        ] {
            for (matching, event) in &cases {
                let mut engine = ready();
                engine.page = Some(destination);
                engine.receive(ServerMessage::Event(event.clone())).unwrap();
                engine.receive(ServerMessage::Event(event.clone())).unwrap();
                assert_eq!(
                    engine.outgoing.len(),
                    usize::from(destination == *matching),
                    "{destination:?} / {event:?}"
                );
                assert_eq!(engine.page, Some(destination));
            }
        }
        let mut engine = ready();
        engine
            .receive(ServerMessage::Event(Event::JobsChanged))
            .unwrap();
        assert!(engine.outgoing.is_empty());
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
        let retained = scripted_reviewed_send_response(SendMode::Send, None).await;
        assert!(retained
            .uncertain
            .iter()
            .any(|action| action.session_id == "a"));
        retained
    }
    async fn scripted_reviewed_send_response(
        mode: SendMode,
        ack: Option<Result<Value, String>>,
    ) -> Retained {
        let (stream, server) = tokio::io::duplex(8192);
        let (command_tx, mut commands) = queue::channel(QUEUE);
        let _keep_commands_open = command_tx.clone();
        let (updates, rx) = mpsc::sync_channel(QUEUE);
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
                loop {
                    tokio::select! {
                        count=server.read_line(&mut line)=>{assert_ne!(count.unwrap(),0);break;},
                        _=tokio::time::sleep(Duration::from_millis(1))=>{
                            for update in rx.try_iter() {match update {
                                Update::Prepared(p)=>command_tx.try_send(Command::ConfirmPrepared(p.id)).unwrap(),
                                Update::PreparationReady{id,..}=>command_tx.try_send(Command::CommitPrepared(id)).unwrap(),
                                _=>{},
                            }}
                        }
                    }
                }
                let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let result = match request.request {
                    Request::Ping => {
                        json!({"min_protocol":1,"max_protocol":1,"features":["pending_attachment_inventory_v1","reviewed_attachment_send_v1","reviewed_send_mode_v1","reviewed_steer_run_v1"]})
                    }
                    Request::Subscribe { deltas, .. } => {
                        assert!(!deltas);
                        json!({"subscribed":true})
                    }
                    Request::ListSessions => json!([]),
                    Request::GetSession { .. } => {
                        command_tx
                            .send(Command::Review {
                                intent: Intent::SendMessage {
                                    session_id: "a".into(),
                                    content: "one action".into(),
                                    mode,
                                },
                                expected_run_id: (mode == SendMode::Steer).then(|| "run-a".into()),
                                draft_basis: "one action".into(),
                                epoch: 0,
                            })
                            .await
                            .unwrap();
                        snapshot("a", "known")
                    }
                    Request::ContextUsage { .. } => {
                        json!({"used_tokens":0,"context_tokens":1000,"measured":false})
                    }
                    Request::ListPendingAttachments { session_id } => {
                        serde_json::json!({"session_id":session_id,"attachments":[]})
                    }
                    Request::ReadSteerReview { session_id } => {
                        assert_eq!(mode, SendMode::Steer);
                        json!({"session_id":session_id,"run_id":"run-a","attachments":[]})
                    }
                    Request::ForecastRun { .. } => {
                        serde_json::to_value(crate::fixture::run_forecast("a", false)).unwrap()
                    }
                    wire_request @ (Request::SendReviewedMessage { .. }
                    | Request::SendReviewedSteer { .. }) => {
                        match wire_request {
                            Request::SendReviewedMessage {
                                client_msg_id,
                                mode: SendMode::Send,
                                ..
                            } if mode == SendMode::Send => assert!(!client_msg_id.is_empty()),
                            Request::SendReviewedSteer {
                                client_msg_id,
                                run_id,
                                ..
                            } if mode == SendMode::Steer => {
                                assert_eq!(run_id, "run-a");
                                assert!(!client_msg_id.is_empty());
                            }
                            other => panic!("incorrect reviewed write {other:?}"),
                        }
                        if let Some(ack) = ack {
                            let reply = match ack {
                                Ok(value) => ServerMessage::ok(request.id, value),
                                Err(error) => ServerMessage::err(request.id, error),
                            };
                            let mut frame = serde_json::to_vec(&reply).unwrap();
                            frame.push(b'\n');
                            server.get_mut().write_all(&frame).await.unwrap();
                        }
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
            // Keep the update consumer alive in this task's result until EOF.
            rx
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
        retained
    }

    #[tokio::test]
    async fn reviewed_error_malformed_or_lost_receipts_remain_unknown_but_exact_success_retires_it()
    {
        for (response, accepted) in [
            (None, false),
            (Some(Err("pending attachments changed".into())), false),
            (Some(Err("postcommit launch failed".into())), false),
            (Some(Ok(json!({"started":true}))), false),
            (Some(Ok(json!({"queued":true}))), false),
            (Some(Ok(json!({"steered":true,"run_id":"r"}))), false),
            (Some(Ok(json!({"started":true,"run_id":"r"}))), true),
        ] {
            let retained = scripted_reviewed_send_response(SendMode::Send, response).await;
            assert_eq!(retained.uncertain.len(), usize::from(!accepted));
            let mut engine = ready();
            engine.active = Some("a".into());
            assert_eq!(
                engine.blocked_by_unknown(
                    &Command::Review {
                        intent: Intent::SendMessage {
                            session_id: "a".into(),
                            content: "one action".into(),
                            mode: SendMode::Send
                        },
                        expected_run_id: None,
                        draft_basis: "one action".into(),
                        epoch: engine.epoch
                    },
                    &retained.uncertain
                ),
                !accepted
            );
        }
    }

    #[tokio::test]
    async fn reviewed_steer_requires_the_matching_run_receipt_to_clear_unknown() {
        for (run_id, accepted) in [("replacement", false), ("run-a", true)] {
            let ack = Ok(json!({"steered":true,"run_id":run_id}));
            let retained = scripted_reviewed_send_response(SendMode::Steer, Some(ack)).await;
            assert_eq!(retained.uncertain.len(), usize::from(!accepted));
        }
    }

    #[tokio::test]
    async fn eof_after_a_send_preserves_unknown_outcome_without_replay() {
        disconnect_after_unacknowledged_send().await;
    }

    #[tokio::test]
    async fn unknown_send_and_question_survive_unrelated_reads_mutations_and_another_question_ack()
    {
        let mut retained = disconnect_after_unacknowledged_send().await;
        let original_send = retained.uncertain.clone();
        for reconnect in [false, true] {
            let before = retained.uncertain.clone();
            let (stream, server) = tokio::io::duplex(16384);
            let (command_tx, mut commands) = queue::channel(QUEUE);
            let _keep_commands = command_tx.clone();
            let (updates, rx) = mpsc::sync_channel(QUEUE * 2);
            let (_stop, mut cancelled) = watch::channel(false);
            let ready = AtomicBool::new(false);
            let epoch = AtomicU64::new(0);
            let connection = AtomicU64::new(0);
            let metadata_operation = uuid::Uuid::new_v4();
            let export_operation = uuid::Uuid::new_v4();
            let usage_operation = uuid::Uuid::new_v4();
            let comparison_operation = uuid::Uuid::new_v4();
            let inspection_operation = uuid::Uuid::new_v4();
            let invocation_operation = uuid::Uuid::new_v4();
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
                        Request::GetRun { run_id } if reconnect => {
                            assert_eq!(run_id, "run-a");
                            crate::run_inspection::test_value(&crate::run_inspection::Target {
                                run_id,
                                session_id: "a".into(),
                            })
                        }
                        Request::ListInvocations if reconnect => crate::invocations::test_value(),
                        Request::ListSessions => json!([]),
                        Request::ContextUsage { .. } => {
                            json!({"used_tokens":0,"context_tokens":1000,"measured":false})
                        }
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
                        Request::SessionUsage { session_id } if reconnect => {
                            assert_eq!(session_id, "a");
                            let counters =
                                serde_json::to_value(formless_protocol::UsageInfo::default())
                                    .unwrap();
                            json!({"total":counters,"last_run":null,"runs":0})
                        }
                        Request::CompareSessions { left, right } if reconnect => {
                            assert_eq!(left, "a");
                            assert_eq!(right, "b");
                            crate::comparison::test_value(&crate::comparison::Pair { left, right })
                        }
                        Request::ExportSession { id, format } if reconnect => {
                            assert_eq!(id, "a");
                            assert_eq!(format.as_deref(), Some("markdown"));
                            json!({"name":"a","format":"markdown","content":"Persisted export"})
                        }
                        Request::SetSessionPinned { id, pinned } if reconnect => {
                            assert_eq!(id, "a");
                            assert!(pinned);
                            let mut session = crate::fixture::sessions(1).remove(0);
                            session.id = id;
                            session.pinned = pinned;
                            serde_json::to_value(session).unwrap()
                        }
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
                                        .try_send(Command::EditSession {
                                            connection: 0,
                                            operation: metadata_operation,
                                            session_id: "a".into(),
                                            change: session_edit::Change::Pin(true),
                                        })
                                        .unwrap();
                                    command_tx
                                        .try_send(Command::ExportSession {
                                            connection: 0,
                                            operation: export_operation,
                                            session_id: "a".into(),
                                            format: session_export::Format::Markdown,
                                        })
                                        .unwrap();
                                    command_tx
                                        .try_send(Command::ReadSessionUsage {
                                            target: session_usage::Target {
                                                session_id: "a".into(),
                                                connection: 0,
                                                generation: 1,
                                            },
                                            operation: usage_operation,
                                        })
                                        .unwrap();
                                    command_tx
                                        .try_send(Command::CompareSessions {
                                            connection: 0,
                                            operation: comparison_operation,
                                            pair: crate::comparison::Pair {
                                                left: "a".into(),
                                                right: "b".into(),
                                            },
                                        })
                                        .unwrap();
                                    command_tx
                                        .try_send(Command::InspectRun {
                                            connection: 0,
                                            operation: inspection_operation,
                                            target: crate::run_inspection::Target {
                                                run_id: "run-a".into(),
                                                session_id: "a".into(),
                                            },
                                        })
                                        .unwrap();
                                    command_tx
                                        .try_send(Command::ReadInvocations {
                                            connection: 0,
                                            operation: invocation_operation,
                                        })
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
            let receiver = driver.await.unwrap();
            let error = result.unwrap_err();
            assert!(
                error.contains("disconnected") || error.contains("broken pipe"),
                "{error}"
            );
            if reconnect {
                let final_updates: Vec<_> = receiver.try_iter().collect();
                assert!(final_updates.iter().any(|update|matches!(update,Update::SessionEdited {operation,outcome:session_edit::Outcome::Applied(_),..} if *operation==metadata_operation)),"the metadata ACK must actually be processed");
                assert!(final_updates.iter().any(|update|matches!(update,Update::SessionExport {operation,result:Ok(_),..} if *operation==export_operation)),"the export reply must actually be processed");
                assert!(final_updates.iter().any(|update|matches!(update,Update::SessionUsage{operation,result:Ok(_),..} if *operation==usage_operation)),"the usage reply must actually be processed");
                assert!(final_updates.iter().any(|update|matches!(update,Update::SessionComparison{operation,result:Ok(_),..} if *operation==comparison_operation)),"comparison reply must actually be processed");
                assert!(final_updates.iter().any(|update|matches!(update,Update::RunInspection{operation,result:Ok(_),..} if *operation==inspection_operation)),"Run inspection reply must actually be processed");
                assert!(final_updates.iter().any(|update|matches!(update,Update::Invocations{operation,result:Ok(_),..} if *operation==invocation_operation)),"Invocation snapshot reply must actually be processed");
                assert_eq!(
                    retained.uncertain, before,
                    "Stop, metadata, export, usage, comparison, Run inspection, Invocation read and q2 ACKs cannot clear either unknown operation"
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
                        Request::ContextUsage { .. } => {
                            json!({"used_tokens":0,"context_tokens":1000,"measured":false})
                        }
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
                                    .send(Command::Intent(Intent::ResumeRun {
                                        session_id: "b".into(),
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
                        Request::ResumeRun { session_id } => {
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
                let error = result.unwrap_err();
                assert!(
                    error.contains("disconnected") || error.contains("broken pipe"),
                    "{error}"
                );
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

#[cfg(test)]
#[path = "client/native_steps_tests.rs"]
mod native_steps_tests;

#[cfg(test)]
#[path = "client/step_mode_tests.rs"]
mod step_mode_tests;

#[cfg(test)]
#[path = "client/approval_tests.rs"]
mod approval_tests;

#[cfg(test)]
#[path = "client/session_model_tests.rs"]
mod session_model_tests;

#[cfg(test)]
#[path = "client/pending_files_tests.rs"]
mod pending_files_tests;

#[cfg(test)]
mod reference_tests;

#[cfg(all(test, feature = "daemon-tests"))]
#[path = "client/preview_schema67_tests.rs"]
mod preview_schema67_tests;

// Both in-memory Core bridge fixtures share one environment-isolation lock.
#[cfg(all(test, feature = "daemon-tests"))]
#[path = "../../formless-core/tests/support/env.rs"]
mod credential_env;
