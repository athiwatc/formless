pub mod client;

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use formless_core::config::{load_api_key, save_api_key, Config};
use formless_core::daemon::ensure_daemon;
use formless_core::provider::{list_models_blocking, probe_blocking, ProbeResult};
use formless_core::Paths;
use formless_protocol::*;
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::collections::HashMap;
use std::io::stdout;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use client::DaemonClient;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn colors_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

fn fg(enabled: bool, color: Color) -> Style {
    if enabled {
        Style::default().fg(color)
    } else {
        Style::default()
    }
}

fn bold_fg(enabled: bool, color: Color) -> Style {
    fg(enabled, color).add_modifier(Modifier::BOLD)
}

fn role_style(enabled: bool, role: &str) -> Style {
    let color = match role {
        "user" => Color::Cyan,
        "tool" => Color::Yellow,
        "system-note" => Color::Blue,
        _ => Color::Green,
    };
    bold_fg(enabled, color)
}

fn thinking_style(enabled: bool) -> Style {
    fg(enabled, Color::Magenta).add_modifier(Modifier::ITALIC | Modifier::DIM)
}

fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

#[derive(PartialEq)]
enum View {
    Chat,
    Sessions,
    Settings,
    Tools,
    Metrics,
    Search,
}

#[derive(Clone, Copy, PartialEq)]
enum Pending {
    ListSessions,
    GetSession,
    CreateSession,
    ForkSession,
    SearchSessions,
    ListTools,
    GetTool,
    ExportTool,
    ExportSession,
}

#[derive(Clone, Copy, PartialEq)]
enum ImportKind {
    Tool,
    Session,
}

#[derive(Clone)]
struct ApprovalReq {
    approval_id: String,
    tool_name: String,
    version: i64,
    capabilities: Vec<String>,
    memory_mb: u32,
}

enum WizardJob {
    Models(Result<Vec<String>, String>),
    Probe(Result<ProbeResult, String>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum WizardStep {
    BaseUrl,
    ApiKey,
    FetchModels,
    PickModel,
    PickEffort,
    Probing,
    Failed,
}

struct Wizard {
    step: WizardStep,
    base_url: String,
    api_key: String,
    models: Vec<String>,
    model_idx: usize,
    manual_model: bool,
    effort_idx: usize,
    status: String,
    /// Previously saved model, pre-selected when the list arrives (§17).
    saved_model: String,
    jobs: mpsc::Receiver<WizardJob>,
    job_tx: mpsc::Sender<WizardJob>,
}

const EFFORTS: [Option<&str>; 4] = [None, Some("low"), Some("medium"), Some("high")];

impl Wizard {
    fn new(cfg: &Config) -> Self {
        let (job_tx, jobs) = mpsc::channel();
        // Remember previous startup options: a saved config starts at the
        // API-key step (Esc returns to the URL), with model/effort restored.
        let has_saved = !cfg.provider.base_url.trim().is_empty();
        let (step, status) = if has_saved {
            (
                WizardStep::ApiKey,
                "Saved setup found. Re-enter your API key to restore it (Esc to edit the base URL).".into(),
            )
        } else {
            (
                WizardStep::BaseUrl,
                "Welcome to Formless. Enter your provider base URL.".into(),
            )
        };
        let effort_idx = cfg
            .provider
            .reasoning_effort
            .as_deref()
            .and_then(|e| EFFORTS.iter().position(|x| *x == Some(e)))
            .unwrap_or(0);
        Self {
            step,
            base_url: cfg.provider.base_url.clone(),
            api_key: String::new(),
            models: Vec::new(),
            model_idx: 0,
            manual_model: false,
            effort_idx,
            status,
            saved_model: cfg.provider.model.clone(),
            jobs,
            job_tx,
        }
    }
    fn model(&self) -> String {
        self.models.get(self.model_idx).cloned().unwrap_or_default()
    }
}

struct Activity {
    label: String,
    since: Instant,
}

#[derive(Clone)]
struct LiveToolCall {
    name: String,
    version: i64,
    arguments: String,
    status: String,
    started: Instant,
    elapsed_ms: Option<i64>,
}

fn truncate_chars(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// Render persisted assistant tool-call metadata as display lines.
fn tool_call_lines(meta: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(meta) else {
        return Vec::new();
    };
    let Some(calls) = v["tool_calls"].as_array() else {
        return Vec::new();
    };
    calls
        .iter()
        .map(|c| {
            let name = c["function"]["name"].as_str().unwrap_or("tool");
            let args = c["function"]["arguments"].as_str().unwrap_or_default();
            format!("⚙ {name}({})", truncate_chars(args, 220))
        })
        .collect()
}

struct App {
    paths: Paths,
    view: View,
    wizard: Option<Wizard>,
    client: Option<DaemonClient>,
    inbox: Option<mpsc::Receiver<ServerMessage>>,
    sessions: Vec<SessionInfo>,
    session_idx: usize,
    current: Option<String>,
    messages: Vec<MessageInfo>,
    streaming: String,
    /// Live reasoning for the current Run (ephemeral; never persisted).
    thinking: String,
    show_thinking: bool,
    color: bool,
    tick: u64,
    activity: Option<Activity>,
    live_tools: Vec<LiveToolCall>,
    /// Absolute chat scroll top; `None` means follow the live bottom.
    chat_scroll_top: Option<usize>,
    chat_view_height: usize,
    chat_line_count: usize,
    input: String,
    running: bool,
    notes: Vec<String>,
    pending: HashMap<u64, Pending>,
    tools: Vec<ToolInfo>,
    tool_idx: usize,
    tool_detail: Option<ToolDetail>,
    approvals: std::collections::VecDeque<ApprovalReq>,
    promotions: std::collections::VecDeque<ApprovalReq>,
    purge_arm: Option<String>,
    import_mode: Option<ImportKind>,
    renaming: bool,
    search_hits: Vec<SearchHit>,
    search_idx: usize,
    search_mode: bool,
    editing_id: Option<String>,
    should_quit: bool,
}

impl App {
    fn new(paths: Paths) -> Result<Self> {
        let cfg = Config::load(&paths)?;
        let needs_wizard = !cfg.provider.is_configured() || load_api_key(&paths).is_none();
        let mut app = Self {
            wizard: if needs_wizard {
                Some(Wizard::new(&cfg))
            } else {
                None
            },
            paths,
            view: View::Chat,
            client: None,
            inbox: None,
            sessions: Vec::new(),
            session_idx: 0,
            current: None,
            messages: Vec::new(),
            streaming: String::new(),
            thinking: String::new(),
            show_thinking: true,
            color: colors_enabled(),
            tick: 0,
            activity: None,
            live_tools: Vec::new(),
            chat_scroll_top: None,
            chat_view_height: 10,
            chat_line_count: 0,
            input: String::new(),
            running: false,
            notes: Vec::new(),
            pending: HashMap::new(),
            tools: Vec::new(),
            tool_idx: 0,
            tool_detail: None,
            approvals: std::collections::VecDeque::new(),
            promotions: std::collections::VecDeque::new(),
            purge_arm: None,
            import_mode: None,
            renaming: false,
            search_hits: Vec::new(),
            search_idx: 0,
            search_mode: false,
            editing_id: None,
            should_quit: false,
        };
        if app.wizard.is_none() {
            app.connect_daemon()?;
        }
        Ok(app)
    }

    fn connect_daemon(&mut self) -> Result<()> {
        ensure_daemon(&self.paths)?;
        let (client, inbox) = DaemonClient::connect(&self.paths)?;
        self.client = Some(client);
        self.inbox = Some(inbox);
        self.request(Pending::ListSessions, Request::ListSessions);
        Ok(())
    }

    fn request(&mut self, kind: Pending, req: Request) {
        if let Some(c) = &self.client {
            let id = c.send(req);
            self.pending.insert(id, kind);
        }
    }
    fn fire(&mut self, req: Request) {
        if let Some(c) = &self.client {
            c.send(req);
        }
    }

    fn drain_daemon(&mut self) {
        let Some(inbox) = self.inbox.take() else {
            return;
        };
        let mut msgs = Vec::new();
        while let Ok(msg) = inbox.try_recv() {
            msgs.push(msg);
        }
        self.inbox = Some(inbox);
        for msg in msgs {
            match msg {
                ServerMessage::Response {
                    id,
                    ok,
                    result,
                    error,
                } => {
                    let Some(kind) = self.pending.remove(&id) else {
                        continue;
                    };
                    if !ok {
                        self.notes
                            .push(format!("daemon: {}", error.unwrap_or_default()));
                        continue;
                    }
                    let Some(result) = result else { continue };
                    match kind {
                        Pending::ListSessions => {
                            if let Ok(v) = serde_json::from_value::<Vec<SessionInfo>>(result) {
                                self.sessions = v;
                                if self.current.is_none() {
                                    if let Some(first) = self.sessions.first().cloned() {
                                        self.current = Some(first.id.clone());
                                        self.request(
                                            Pending::GetSession,
                                            Request::GetSession { id: first.id },
                                        );
                                    } else {
                                        self.request(
                                            Pending::CreateSession,
                                            Request::CreateSession { name: None },
                                        );
                                    }
                                }
                            }
                        }
                        Pending::CreateSession => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.current = Some(s.id.clone());
                                self.sessions.insert(0, s);
                                self.messages.clear();
                                self.live_tools.clear();
                                self.follow_chat_bottom();
                            }
                        }
                        Pending::ForkSession => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.current = Some(s.id.clone());
                                self.sessions.insert(0, s.clone());
                                self.request(Pending::GetSession, Request::GetSession { id: s.id });
                                self.view = View::Chat;
                                self.notes.push("session forked".into());
                            }
                        }
                        Pending::SearchSessions => {
                            if let Ok(v) = serde_json::from_value::<Vec<SearchHit>>(result) {
                                self.notes.push(format!("{} search hit(s)", v.len()));
                                self.search_hits = v;
                                self.search_idx = 0;
                                self.view = View::Search;
                            }
                        }
                        Pending::ListTools => {
                            if let Ok(v) = serde_json::from_value::<Vec<ToolInfo>>(result) {
                                self.tools = v;
                            }
                        }
                        Pending::GetTool => {
                            if let Ok(d) = serde_json::from_value::<ToolDetail>(result) {
                                self.tool_detail = Some(d);
                            }
                        }
                        Pending::ExportTool | Pending::ExportSession => {
                            let dir = self.paths.data_dir.join("exports");
                            let _ = std::fs::create_dir_all(&dir);
                            let label = result["name"].as_str().unwrap_or("export").to_string();
                            let file = dir.join(format!("{label}.json"));
                            match std::fs::write(
                                &file,
                                serde_json::to_string_pretty(&result).unwrap_or_default(),
                            ) {
                                Ok(()) => {
                                    self.notes.push(format!("exported to {}", file.display()))
                                }
                                Err(e) => self.notes.push(format!("export failed: {e}")),
                            }
                        }
                        Pending::GetSession => {
                            if let Some(msgs) = result.get("messages").cloned() {
                                if let Ok(v) = serde_json::from_value::<Vec<MessageInfo>>(msgs) {
                                    self.messages = v;
                                    self.live_tools.clear();
                                    self.follow_chat_bottom();
                                }
                            }
                        }
                    }
                }
                ServerMessage::Event(ev) => self.on_event(ev),
            }
        }
    }

    fn set_activity(&mut self, label: impl Into<String>) {
        let label = label.into();
        match &mut self.activity {
            Some(a) if a.label == label => {}
            slot => {
                *slot = Some(Activity {
                    label,
                    since: Instant::now(),
                })
            }
        }
    }

    fn spinner(&self) -> &'static str {
        SPINNER[(self.tick / 2) as usize % SPINNER.len()]
    }

    /// Always-visible working state, e.g. `⠙ thinking… (12s)`.
    fn activity_text(&self) -> Option<String> {
        self.activity.as_ref().map(|a| {
            format!(
                "{} {}… ({})",
                self.spinner(),
                a.label,
                format_elapsed(a.since.elapsed())
            )
        })
    }

    fn chat_max_top(&self) -> usize {
        self.chat_line_count.saturating_sub(self.chat_view_height)
    }

    fn scroll_chat_up(&mut self, lines: usize) {
        let max_top = self.chat_max_top();
        let cur = self.chat_scroll_top.unwrap_or(max_top);
        self.chat_scroll_top = Some(cur.saturating_sub(lines));
    }

    fn scroll_chat_down(&mut self, lines: usize) {
        if let Some(top) = self.chat_scroll_top {
            let next = top + lines;
            self.chat_scroll_top = if next >= self.chat_max_top() {
                None // back at the bottom: follow live output again
            } else {
                Some(next)
            };
        }
    }

    fn follow_chat_bottom(&mut self) {
        self.chat_scroll_top = None;
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::MessageAppended {
                session_id,
                message,
            } => {
                if self.current.as_deref() == Some(&session_id)
                    && !self.messages.iter().any(|m| m.id == message.id)
                {
                    self.messages.push(message);
                }
            }
            Event::MessageDelta { delta, .. } => {
                self.set_activity("responding");
                self.streaming.push_str(&delta);
            }
            Event::ReasoningDelta { delta, .. } => {
                self.set_activity("thinking");
                self.thinking.push_str(&delta);
            }
            Event::MessageComplete {
                session_id,
                message,
                ..
            } => {
                self.streaming.clear();
                self.thinking.clear();
                if self.current.as_deref() == Some(&session_id)
                    && !self.messages.iter().any(|m| m.id == message.id)
                {
                    self.messages.push(message);
                }
            }
            Event::RunStarted { .. } => {
                self.running = true;
                self.thinking.clear();
                self.streaming.clear();
                self.live_tools.clear();
                self.set_activity("waiting for model");
            }
            Event::RunFinished { status, .. } => {
                self.running = false;
                self.activity = None;
                self.streaming.clear();
                self.thinking.clear();
                if status != "completed" {
                    self.notes.push(format!("run {status}"));
                }
                self.request(Pending::ListSessions, Request::ListSessions);
            }
            Event::RunPaused { error, .. } => {
                self.running = false;
                self.activity = None;
                self.notes.push(format!("run paused: {error}"));
            }
            Event::CompactionNotice {
                summary_preview, ..
            } => {
                self.notes.push(format!("compacted: {summary_preview}"));
            }
            Event::Note { text, .. } => self.notes.push(text),
            Event::ToolCallStarted {
                tool_name,
                version,
                arguments,
                ..
            } => {
                self.set_activity(format!("running {tool_name}"));
                self.live_tools.push(LiveToolCall {
                    name: tool_name.clone(),
                    version,
                    arguments,
                    status: "running".into(),
                    started: Instant::now(),
                    elapsed_ms: None,
                });
                self.notes.push(format!("running {tool_name}…"));
            }
            Event::ToolCallFinished {
                tool_name,
                ok,
                elapsed_ms,
                ..
            } => {
                if let Some(call) = self
                    .live_tools
                    .iter_mut()
                    .rev()
                    .find(|c| c.name == tool_name && c.status == "running")
                {
                    call.status = if ok { "ok".into() } else { "failed".into() };
                    call.elapsed_ms = Some(if elapsed_ms > 0 {
                        elapsed_ms
                    } else {
                        call.started.elapsed().as_millis() as i64
                    });
                }
                if self.running {
                    self.set_activity("waiting for model");
                }
                self.notes
                    .push(format!("{tool_name}: {}", if ok { "ok" } else { "failed" }));
            }
            Event::ApprovalRequested {
                approval_id,
                tool_name,
                version,
                capabilities,
                memory_mb,
                ..
            } => {
                self.set_activity("awaiting your approval");
                self.approvals.push_back(ApprovalReq {
                    approval_id,
                    tool_name,
                    version,
                    capabilities,
                    memory_mb,
                });
            }
            Event::ToolUpdated { tool } => {
                if let Some(existing) = self.tools.iter_mut().find(|x| x.id == tool.id) {
                    *existing = tool;
                } else {
                    self.tools.push(tool);
                }
                self.request(Pending::ListTools, Request::ListTools);
            }
            Event::ToolDeleted { id } => self.tools.retain(|x| x.id != id),
            Event::OptimiserActivity { tool_name, text } => {
                self.notes.push(format!("optimiser {tool_name}: {text}"));
                self.request(Pending::ListTools, Request::ListTools);
            }
            Event::PromotionApprovalRequested {
                promotion_id,
                tool_name,
                reason,
                capabilities,
                memory_mb,
            } => {
                self.set_activity("awaiting your approval");
                self.promotions.push_back(ApprovalReq {
                    approval_id: promotion_id,
                    tool_name: format!("{tool_name} (promotion: {reason})"),
                    version: 0,
                    capabilities,
                    memory_mb,
                });
            }
            Event::InvocationPending { tool_name, .. } => {
                self.notes
                    .push(format!("{tool_name} still running (pending)"));
            }
            Event::InvocationCompleted { tool_name, ok, .. } => {
                self.notes.push(format!(
                    "pending {tool_name} {}",
                    if ok { "completed" } else { "failed" }
                ));
            }
            Event::SessionUpdated { session } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session.id) {
                    *s = session;
                } else {
                    self.sessions.insert(0, session);
                }
            }
            Event::MessagesEdited { session_id } => {
                if self.current.as_deref() == Some(&session_id) {
                    self.request(Pending::GetSession, Request::GetSession { id: session_id });
                }
            }
            Event::SessionDeleted { id } => {
                self.sessions.retain(|s| s.id != id);
                if self.current.as_deref() == Some(&id) {
                    self.current = None;
                    self.messages.clear();
                }
            }
        }
    }

    fn submit_chat(&mut self, queued: bool) {
        let text = std::mem::take(&mut self.input);
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(kind) = self.import_mode.take() {
            match std::fs::read_to_string(&text)
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string())
                }) {
                Ok(bundle) => {
                    self.fire(match kind {
                        ImportKind::Tool => Request::ImportTool { bundle },
                        ImportKind::Session => Request::ImportSession { bundle },
                    });
                    self.notes.push("import sent".into());
                    self.request(Pending::ListTools, Request::ListTools);
                    self.request(Pending::ListSessions, Request::ListSessions);
                }
                Err(e) => self.notes.push(format!("import failed: {e}")),
            }
            return;
        }
        if self.renaming {
            self.renaming = false;
            if let Some(id) = self.current.clone() {
                self.fire(Request::RenameSession { id, name: text });
            }
            return;
        }
        if self.search_mode {
            self.search_mode = false;
            self.request(
                Pending::SearchSessions,
                Request::SearchSessions {
                    query: text,
                    limit: Some(50),
                },
            );
            return;
        }
        if let Some(message_id) = self.editing_id.take() {
            if let Some(session_id) = self.current.clone() {
                self.follow_chat_bottom();
                self.fire(Request::EditMessage {
                    session_id,
                    message_id,
                    content: text,
                });
                self.notes.push("editing and retrying…".into());
            }
            return;
        }
        if let Some(cmd) = text.strip_prefix('/') {
            self.slash(cmd.trim());
            return;
        }
        let Some(session_id) = self.current.clone() else {
            return;
        };
        let mode = if queued {
            SendMode::Queue
        } else if self.running {
            SendMode::Steer
        } else {
            SendMode::Send
        };
        self.follow_chat_bottom();
        self.fire(Request::SendMessage {
            session_id,
            content: text,
            mode,
        });
    }

    fn slash(&mut self, cmd: &str) {
        match cmd {
            "new" => self.request(
                Pending::CreateSession,
                Request::CreateSession { name: None },
            ),
            "sessions" => self.view = View::Sessions,
            "fork" => {
                if let Some(id) = self.current.clone() {
                    self.request(
                        Pending::ForkSession,
                        Request::ForkSession {
                            id,
                            name: None,
                            up_to_seq: None,
                        },
                    );
                }
            }
            "search" => {
                self.search_mode = true;
                self.notes
                    .push("Type a search query, then Enter (searches all sessions).".into());
            }
            "edit" | "retry" => match self.messages.iter().rev().find(|m| m.role == "user") {
                Some(m) => {
                    self.editing_id = Some(m.id.clone());
                    self.input = m.content.clone();
                    self.notes
                        .push("Editing your last message — Enter retries from there.".into());
                }
                None => self.notes.push("no user message to edit".into()),
            },
            "metrics" => {
                self.view = View::Metrics;
                self.request(Pending::ListTools, Request::ListTools);
            }
            "tools" => {
                self.view = View::Tools;
                self.request(Pending::ListTools, Request::ListTools);
            }
            "settings" => self.view = View::Settings,
            "model" => {
                let cfg = Config::load(&self.paths).ok();
                let (m, e) = cfg
                    .map(|c| {
                        (
                            c.provider.model,
                            c.provider
                                .reasoning_effort
                                .unwrap_or_else(|| "default".into()),
                        )
                    })
                    .unwrap_or_default();
                self.notes
                    .push(format!("model: {m} (effort {e}); change in Settings view"));
            }
            "stop" => {
                if let Some(id) = self.current.clone() {
                    self.fire(Request::InterruptRun { session_id: id });
                }
            }
            "approve" => self.notes.push("Approvals arrive with Tools in M2.".into()),
            "thinking" => {
                self.show_thinking = !self.show_thinking;
                self.notes.push(format!(
                    "thinking display {}",
                    if self.show_thinking { "on" } else { "off" }
                ));
            }
            "help" => self.notes.push(
                "Enter send/steer · Ctrl+Enter queue · Esc stop · Tab views · PgUp/PgDn scroll · /new /sessions /fork /search /edit /tools /metrics /settings /model /thinking /stop /quit".into(),
            ),
            "quit" => self.should_quit = true,
            other => self.notes.push(format!("unknown command: /{other}")),
        }
    }

    fn wizard_poll(&mut self) {
        let mut finish = false;
        if let Some(w) = &mut self.wizard {
            while let Ok(job) = w.jobs.try_recv() {
                match job {
                    WizardJob::Models(Ok(models)) if !models.is_empty() => {
                        w.model_idx = models.iter().position(|m| *m == w.saved_model).unwrap_or(0);
                        w.models = models;
                        w.step = WizardStep::PickModel;
                        w.status = "Pick a model (Up/Down, Enter).".into();
                    }
                    WizardJob::Models(_) => {
                        w.manual_model = true;
                        w.models = vec![String::new()];
                        w.step = WizardStep::PickModel;
                        w.status = "Could not list models; type a model name, then Enter.".into();
                    }
                    WizardJob::Probe(Ok(r)) if r.tool_calling => {
                        finish = true;
                    }
                    WizardJob::Probe(Ok(_)) => {
                        w.step = WizardStep::Failed;
                        w.status = "This endpoint/model does not support tool-calling, which Formless requires. Press Enter to pick another model.".into();
                    }
                    WizardJob::Probe(Err(e)) => {
                        w.step = WizardStep::Failed;
                        w.status = format!("Probe failed: {e}. Press Enter to retry model pick.");
                    }
                }
            }
        }
        if finish {
            if let Err(e) = self.wizard_finish() {
                if let Some(w) = &mut self.wizard {
                    w.step = WizardStep::Failed;
                    w.status = format!("Saving config failed: {e}");
                }
            }
        }
    }

    fn wizard_finish(&mut self) -> Result<()> {
        let w = self.wizard.take().expect("wizard");
        let mut cfg = Config::load(&self.paths)?;
        cfg.provider.base_url = w.base_url.trim().to_string();
        cfg.provider.model = w.model();
        cfg.provider.reasoning_effort = EFFORTS[w.effort_idx].map(str::to_string);
        cfg.save(&self.paths)?;
        save_api_key(&self.paths, &w.api_key)?;
        self.connect_daemon()
    }

    fn wizard_key(&mut self, key: KeyEvent) {
        let Some(w) = &mut self.wizard else { return };
        match w.step {
            WizardStep::BaseUrl => match key.code {
                KeyCode::Enter if !w.base_url.trim().is_empty() => {
                    w.step = WizardStep::ApiKey;
                    w.status = "Enter your API key (stored in the OS keychain).".into();
                }
                KeyCode::Char(c) => w.base_url.push(c),
                KeyCode::Backspace => {
                    w.base_url.pop();
                }
                _ => {}
            },
            WizardStep::ApiKey => match key.code {
                KeyCode::Esc => {
                    w.step = WizardStep::BaseUrl;
                    w.status = "Enter your provider base URL.".into();
                }
                KeyCode::Enter => {
                    w.step = WizardStep::FetchModels;
                    w.status = "Fetching models…".into();
                    let (tx, mut cfg) = (w.job_tx.clone(), Config::default());
                    cfg.provider.base_url = w.base_url.trim().to_string();
                    let (pcfg, key2) = (cfg.provider.clone(), w.api_key.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(WizardJob::Models(
                            list_models_blocking(&pcfg, &key2).map_err(|e| e.to_string()),
                        ));
                    });
                }
                KeyCode::Char(c) => w.api_key.push(c),
                KeyCode::Backspace => {
                    w.api_key.pop();
                }
                _ => {}
            },
            WizardStep::PickModel => {
                if w.manual_model {
                    match key.code {
                        KeyCode::Enter if !w.model().trim().is_empty() => {
                            w.step = WizardStep::PickEffort;
                            w.status = "Pick reasoning effort.".into();
                        }
                        KeyCode::Char(c) => w.models[0].push(c),
                        KeyCode::Backspace => {
                            w.models[0].pop();
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Up => w.model_idx = w.model_idx.saturating_sub(1),
                        KeyCode::Down => {
                            w.model_idx = (w.model_idx + 1).min(w.models.len().saturating_sub(1))
                        }
                        KeyCode::Enter => {
                            w.step = WizardStep::PickEffort;
                            w.status = "Pick reasoning effort.".into();
                        }
                        _ => {}
                    }
                }
            }
            WizardStep::PickEffort => match key.code {
                KeyCode::Up => w.effort_idx = w.effort_idx.saturating_sub(1),
                KeyCode::Down => w.effort_idx = (w.effort_idx + 1).min(EFFORTS.len() - 1),
                KeyCode::Enter => {
                    w.step = WizardStep::Probing;
                    w.status = "Probing endpoint (connectivity + streaming + tool-calling)…".into();
                    let tx = w.job_tx.clone();
                    let pcfg = formless_core::config::ProviderCfg {
                        base_url: w.base_url.trim().to_string(),
                        ..Default::default()
                    };
                    let (key2, model) = (w.api_key.clone(), w.model());
                    std::thread::spawn(move || {
                        let _ = tx.send(WizardJob::Probe(
                            probe_blocking(&pcfg, &key2, &model).map_err(|e| e.to_string()),
                        ));
                    });
                }
                _ => {}
            },
            WizardStep::Failed if key.code == KeyCode::Enter => {
                w.step = WizardStep::PickModel;
                w.status = "Pick a model.".into();
            }
            _ => {}
        }
    }

    fn mouse(&mut self, kind: crossterm::event::MouseEventKind) {
        use crossterm::event::MouseEventKind;
        if self.wizard.is_some() || self.view != View::Chat {
            return;
        }
        match kind {
            MouseEventKind::ScrollUp => self.scroll_chat_up(3),
            MouseEventKind::ScrollDown => self.scroll_chat_down(3),
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        if self.wizard.is_some() {
            self.wizard_key(key);
            return;
        }
        if let KeyCode::Char(c) = key.code {
            if (c == 'y' || c == 'n') && self.input.is_empty() {
                if !self.approvals.is_empty() {
                    let req = self.approvals.pop_front().unwrap();
                    self.fire(Request::ApprovalDecision {
                        approval_id: req.approval_id,
                        approved: c == 'y',
                    });
                    self.notes.push(format!(
                        "{} {}",
                        if c == 'y' { "approved" } else { "denied" },
                        req.tool_name
                    ));
                    return;
                }
                if !self.promotions.is_empty() {
                    let req = self.promotions.pop_front().unwrap();
                    self.fire(Request::PromotionDecision {
                        promotion_id: req.approval_id,
                        approved: c == 'y',
                    });
                    self.notes.push(format!(
                        "promotion {} {}",
                        if c == 'y' { "approved" } else { "denied" },
                        req.tool_name
                    ));
                    return;
                }
            }
        }
        match self.view {
            View::Search => match key.code {
                KeyCode::Up => self.search_idx = self.search_idx.saturating_sub(1),
                KeyCode::Down => {
                    self.search_idx =
                        (self.search_idx + 1).min(self.search_hits.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(hit) = self.search_hits.get(self.search_idx).cloned() {
                        self.current = Some(hit.session_id.clone());
                        self.request(
                            Pending::GetSession,
                            Request::GetSession { id: hit.session_id },
                        );
                        self.view = View::Chat;
                    }
                }
                KeyCode::Esc | KeyCode::Tab => self.view = View::Chat,
                _ => {}
            },
            View::Sessions => match key.code {
                KeyCode::Up => self.session_idx = self.session_idx.saturating_sub(1),
                KeyCode::Down => {
                    self.session_idx =
                        (self.session_idx + 1).min(self.sessions.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(s) = self.sessions.get(self.session_idx).cloned() {
                        self.current = Some(s.id.clone());
                        self.request(Pending::GetSession, Request::GetSession { id: s.id });
                        self.view = View::Chat;
                    }
                }
                KeyCode::Char('n') => self.request(
                    Pending::CreateSession,
                    Request::CreateSession { name: None },
                ),
                KeyCode::Char('f') => {
                    if let Some(s) = self.sessions.get(self.session_idx).cloned() {
                        self.request(
                            Pending::ForkSession,
                            Request::ForkSession {
                                id: s.id,
                                name: None,
                                up_to_seq: None,
                            },
                        );
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(s) = self.sessions.get(self.session_idx).cloned() {
                        self.fire(Request::DeleteSession { id: s.id });
                        self.request(Pending::ListSessions, Request::ListSessions);
                    }
                }
                KeyCode::Char('e') => {
                    if let Some(s) = self.sessions.get(self.session_idx).cloned() {
                        self.request(Pending::ExportSession, Request::ExportSession { id: s.id });
                    }
                }
                KeyCode::Char('i') => {
                    self.import_mode = Some(ImportKind::Session);
                    self.view = View::Chat;
                    self.notes
                        .push("Type a session bundle file path, then Enter.".into());
                }
                KeyCode::Char('r') => {
                    self.renaming = true;
                    self.view = View::Chat;
                    self.notes
                        .push("Type the new session name, then Enter.".into());
                }
                KeyCode::Esc | KeyCode::Tab => self.view = View::Chat,
                _ => {}
            },
            View::Tools => match key.code {
                KeyCode::Up => self.tool_idx = self.tool_idx.saturating_sub(1),
                KeyCode::Down => {
                    self.tool_idx = (self.tool_idx + 1).min(self.tools.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(tool) = self.tools.get(self.tool_idx).cloned() {
                        self.request(Pending::GetTool, Request::GetTool { id: tool.id });
                    }
                }
                KeyCode::Char('e') => {
                    if let Some(tool) = self.tools.get(self.tool_idx).cloned() {
                        self.request(Pending::ExportTool, Request::ExportTool { id: tool.id });
                    }
                }
                KeyCode::Char('i') => {
                    self.import_mode = Some(ImportKind::Tool);
                    self.view = View::Chat;
                    self.notes
                        .push("Type a tool bundle file path, then Enter.".into());
                }
                KeyCode::Char('o') => {
                    let tool_id = self.tools.get(self.tool_idx).map(|t| t.id.clone());
                    self.fire(Request::RunOptimiser { tool_id });
                    self.notes.push("optimiser started".into());
                }
                KeyCode::Char('x') => {
                    if let Some(tool) = self.tools.get(self.tool_idx).cloned() {
                        if tool.retired {
                            self.fire(Request::RestoreTool { id: tool.id });
                        } else {
                            self.fire(Request::RetireTool { id: tool.id });
                        }
                    }
                }
                KeyCode::Char('b') => {
                    if let Some(tool) = self.tools.get(self.tool_idx).cloned() {
                        if tool.current_version > 1 {
                            self.fire(Request::RollbackTool {
                                id: tool.id,
                                version: tool.current_version - 1,
                            });
                        }
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(tool) = self.tools.get(self.tool_idx).cloned() {
                        if self.purge_arm.as_deref() == Some(&tool.id) {
                            self.purge_arm = None;
                            self.fire(Request::PurgeTool { id: tool.id });
                        } else {
                            self.notes
                                .push(format!("press p again to PURGE {} permanently", tool.name));
                            self.purge_arm = Some(tool.id);
                        }
                    }
                }
                KeyCode::Esc => self.view = View::Chat,
                KeyCode::Tab => self.view = View::Chat,
                _ => {}
            },
            _ => match key.code {
                KeyCode::PageUp => {
                    let page = self.chat_view_height.saturating_sub(1).max(1);
                    self.scroll_chat_up(page);
                }
                KeyCode::PageDown => {
                    let page = self.chat_view_height.saturating_sub(1).max(1);
                    self.scroll_chat_down(page);
                }
                KeyCode::Up => self.scroll_chat_up(3),
                KeyCode::Down => self.scroll_chat_down(3),
                KeyCode::End => self.follow_chat_bottom(),
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.submit_chat(true)
                }
                KeyCode::Enter => self.submit_chat(false),
                KeyCode::Esc => {
                    if self.running {
                        if let Some(id) = self.current.clone() {
                            self.fire(Request::InterruptRun { session_id: id });
                        }
                    } else {
                        self.view = View::Chat;
                    }
                }
                KeyCode::Tab => {
                    self.view = match self.view {
                        View::Chat => View::Sessions,
                        View::Sessions => View::Settings,
                        View::Settings => View::Tools,
                        View::Tools => View::Metrics,
                        View::Metrics | View::Search => View::Chat,
                    };
                    if self.view == View::Metrics {
                        self.request(Pending::ListTools, Request::ListTools);
                    }
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c) => self.input.push(c),
                _ => {}
            },
        }
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    if let Some(w) = &app.wizard {
        draw_wizard(f, w, app.color);
        return;
    }
    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(f.area());
    match app.view {
        View::Chat => draw_chat(f, chunks[0], app),
        View::Sessions => {
            let items: Vec<ListItem> = app
                .sessions
                .iter()
                .map(|s| ListItem::new(format!("{}  ({})", s.name, s.updated_at)))
                .collect();
            let mut state = ListState::default();
            if !app.sessions.is_empty() {
                state.select(Some(app.session_idx.min(app.sessions.len() - 1)));
            }
            f.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(fg(app.color, Color::Cyan))
                            .title("Sessions (Enter open, n new, f fork, d delete, r rename)"),
                    )
                    .highlight_style(bold_fg(app.color, Color::Cyan))
                    .highlight_symbol("> "),
                chunks[0],
                &mut state,
            );
        }
        View::Settings => {
            let text = Config::load(&app.paths)
                .map(|c| format!(
                    "base_url: {}\nmodel: {}\nreasoning_effort: {}\ncontext_tokens: {}\nconfig: {}\ndata: {}\n\nEdit config.toml or delete it to re-run the wizard (restart TUI). Hot-reloaded per Run.",
                    c.provider.base_url, c.provider.model, c.provider.reasoning_effort.as_deref().unwrap_or("default"),
                    c.provider.context_tokens, app.paths.config_file().display(), app.paths.data_dir.display()
                ))
                .unwrap_or_else(|e| e.to_string());
            f.render_widget(
                Paragraph::new(text)
                    .block(Block::default().borders(Borders::ALL).title("Settings")),
                chunks[0],
            );
        }
        View::Tools => draw_tools(f, chunks[0], app),
        View::Metrics => draw_metrics(f, chunks[0], app),
        View::Search => draw_search(f, chunks[0], app),
    }
    let prompt = if app.renaming {
        "Rename> "
    } else if app.search_mode {
        "Search> "
    } else if app.editing_id.is_some() {
        "Edit> "
    } else if app.running {
        "Steer> "
    } else {
        "> "
    };
    f.render_widget(
        Paragraph::new(format!("{prompt}{}", app.input)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(fg(
                    app.color,
                    if app.running {
                        Color::Yellow
                    } else {
                        Color::Cyan
                    },
                ))
                .title("Message (Enter send/steer, Ctrl+Enter queue, /help)"),
        ),
        chunks[1],
    );
    let note = app.notes.last().cloned().unwrap_or_default();
    let note_style =
        if note.contains("paused") || note.contains("failed") || note.contains("denied") {
            bold_fg(app.color, Color::Red)
        } else {
            fg(app.color, Color::Gray)
        };
    let mut status: Vec<Span> = Vec::new();
    if let Some(activity) = app.activity_text() {
        status.push(Span::styled(activity, bold_fg(app.color, Color::Yellow)));
        status.push(Span::raw("  "));
    } else if app.running {
        status.push(Span::styled(
            "● running  ",
            bold_fg(app.color, Color::Yellow),
        ));
    } else {
        status.push(Span::styled("● ready  ", fg(app.color, Color::Green)));
    }
    status.push(Span::styled(
        app.sessions
            .iter()
            .find(|s| Some(&s.id) == app.current.as_ref())
            .map(|s| s.name.as_str())
            .unwrap_or("no session")
            .to_string(),
        bold_fg(app.color, Color::Cyan),
    ));
    status.push(Span::styled(
        "  Tab views · Esc stop/back · Ctrl+C detach  ",
        fg(app.color, Color::DarkGray),
    ));
    status.push(Span::styled(note, note_style));
    f.render_widget(Paragraph::new(Line::from(status)), chunks[2]);
    if let Some(req) = app.approvals.front().or(app.promotions.front()) {
        draw_approval(f, req, app.color);
    }
}

fn draw_search(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = if app.search_hits.is_empty() {
        vec![ListItem::new("No hits. /search to try again.")]
    } else {
        app.search_hits
            .iter()
            .map(|h| {
                ListItem::new(format!(
                    "{} #{} {}: {}",
                    h.session_name, h.seq, h.role, h.snippet
                ))
            })
            .collect()
    };
    let mut state = ListState::default();
    if !app.search_hits.is_empty() {
        state.select(Some(app.search_idx.min(app.search_hits.len() - 1)));
    }
    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(app.color, Color::Magenta))
                    .title("Search (Enter open session, Esc back)"),
            )
            .highlight_style(bold_fg(app.color, Color::Magenta))
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn metrics_lines(tools: &[ToolInfo]) -> Vec<String> {
    let total_calls: i64 = tools.iter().map(|t| t.invocations).sum();
    let attention = tools.iter().filter(|t| t.needs_attention).count();
    let retired = tools.iter().filter(|t| t.retired).count();
    let mut lines = vec![
        format!(
            "Tools: {} ({} retired, {} need attention) · Total invocations: {}",
            tools.len(),
            retired,
            attention,
            total_calls
        ),
        String::new(),
        format!(
            "{:<24} {:>5} {:>7} {:>9}",
            "TOOL", "VER", "CALLS", "SUCCESS"
        ),
    ];
    let mut sorted: Vec<&ToolInfo> = tools.iter().collect();
    sorted.sort_by_key(|t| std::cmp::Reverse(t.invocations));
    for t in sorted {
        let rate = t
            .success_rate
            .map(|r| format!("{:.0}%", r * 100.0))
            .unwrap_or_else(|| "-".into());
        lines.push(format!(
            "{:<24} {:>5} {:>7} {:>9}{}",
            truncate_chars(&t.name, 24),
            t.current_version,
            t.invocations,
            rate,
            if t.needs_attention {
                "  (!needs attention)"
            } else if t.retired {
                "  (retired)"
            } else {
                ""
            }
        ));
    }
    if tools.is_empty() {
        lines.push("(no Tools yet — the agent builds them via create_tool)".into());
    }
    lines
}

fn draw_metrics(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = metrics_lines(&app.tools)
        .into_iter()
        .map(Line::raw)
        .collect();
    if let Some(d) = &app.tool_detail {
        let m = &d.metrics;
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("Selected: {}", d.tool.name),
            bold_fg(app.color, Color::Yellow),
        ));
        lines.push(Line::raw(format!(
            "total={} success={} failure={} denied={} cancelled={} validation={}",
            m.total, m.success, m.failure, m.denied, m.cancelled, m.validation_failures
        )));
        lines.push(Line::raw(format!(
            "rate={} retry={} p50={} p95={} (select in Tools view for detail)",
            m.success_rate
                .map(|r| format!("{:.1}%", r * 100.0))
                .unwrap_or_else(|| "-".into()),
            m.retry_rate
                .map(|r| format!("{:.1}%", r * 100.0))
                .unwrap_or_else(|| "-".into()),
            m.p50_ms
                .map(|v| format!("{v:.0}ms"))
                .unwrap_or_else(|| "-".into()),
            m.p95_ms
                .map(|v| format!("{v:.0}ms"))
                .unwrap_or_else(|| "-".into()),
        )));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(app.color, Color::Green))
                    .title("Metrics"),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_tools(f: &mut Frame, area: Rect, app: &App) {
    let parts =
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
    let items: Vec<ListItem> = app
        .tools
        .iter()
        .map(|t| {
            let rate = t
                .success_rate
                .map(|r| format!("{:.0}%", r * 100.0))
                .unwrap_or_else(|| "-".into());
            ListItem::new(format!(
                "{}{} v{}  calls={} success={}  [{}]",
                t.name,
                if t.needs_attention {
                    " (!needs attention)"
                } else if t.retired {
                    " (retired)"
                } else {
                    ""
                },
                t.current_version,
                t.invocations,
                rate,
                t.capabilities.join(",")
            ))
        })
        .collect();
    let mut state = ListState::default();
    if !app.tools.is_empty() {
        state.select(Some(app.tool_idx.min(app.tools.len() - 1)));
    }
    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(app.color, Color::Yellow))
                    .title("Tools (Enter detail, x retire/restore, b rollback, p purge x2)"),
            )
            .highlight_style(bold_fg(app.color, Color::Yellow))
            .highlight_symbol("> "),
        parts[0],
        &mut state,
    );
    let detail = match &app.tool_detail {
        Some(d) => {
            let m = &d.metrics;
            let cur = d.versions.iter().find(|v| v.version == d.tool.current_version);
            format!(
                "{}: {}\nversions: {} (current v{})\ncaps: [{}] memory={}MB\nmetrics: total={} success={} failure={} denied={} validation={} rate={} retry={} p50={} p95={}\n\n--- code (v{}, read-only) ---\n{}",
                d.tool.name,
                d.tool.description,
                d.versions.iter().map(|v| format!("v{}({})", v.version, v.author)).collect::<Vec<_>>().join(" "),
                d.tool.current_version,
                d.tool.capabilities.join(", "),
                d.tool.memory_mb,
                m.total, m.success, m.failure, m.denied, m.validation_failures,
                m.success_rate.map(|r| format!("{:.1}%", r * 100.0)).unwrap_or_else(|| "-".into()),
                m.retry_rate.map(|r| format!("{:.1}%", r * 100.0)).unwrap_or_else(|| "-".into()),
                m.p50_ms.map(|v| format!("{v:.0}ms")).unwrap_or_else(|| "-".into()),
                m.p95_ms.map(|v| format!("{v:.0}ms")).unwrap_or_else(|| "-".into()),
                d.tool.current_version,
                cur.map(|v| v.code.lines().take(24).collect::<Vec<_>>().join("\n")).unwrap_or_default(),
            ) + &d.optimiser_attempts.iter().take(5).map(|a| format!(
                "\noptimiser [{}] {} {} (cand {:?} vs base {:?}, {} replays)",
                a.created_at, a.status, a.detail, a.candidate_score, a.baseline_score, a.replay_total
            )).collect::<String>()
        }
        None => "Select a Tool and press Enter for detail. Tools are built by the agent via create_tool.".into(),
    };
    f.render_widget(
        Paragraph::new(detail)
            .block(Block::default().borders(Borders::ALL).title("Detail"))
            .wrap(Wrap { trim: false }),
        parts[1],
    );
}

fn draw_approval(f: &mut Frame, req: &ApprovalReq, color: bool) {
    let area = f.area();
    let w = area.width.saturating_mul(70) / 100;
    let h = 10u16.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let broad: Vec<&String> = req
        .capabilities
        .iter()
        .filter(|c| *c == "net" || *c == "process")
        .collect();
    let text = format!(
        "Tool '{}' v{} wants sensitive capabilities:\n\n  {}\n  memory: {} MB{}\n\nApproved per-Tool until capabilities widen.\n\n[y] approve   [n] deny",
        req.tool_name,
        req.version,
        if req.capabilities.is_empty() { "(none)".into() } else { req.capabilities.join(", ") },
        req.memory_mb,
        if broad.is_empty() { String::new() } else { format!("\n  WARNING broad/unscoped: {}", broad.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")) },
    );
    f.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(bold_fg(color, Color::Red))
                    .title("Approval required"),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn draw_chat(f: &mut Frame, area: Rect, app: &mut App) {
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.messages {
        // Assistant tool-call turns: show the call(s) inline (§17 v2).
        if m.role == "assistant" {
            if let Some(meta) = &m.meta {
                let calls = tool_call_lines(meta);
                if !calls.is_empty() {
                    if !m.content.is_empty() {
                        lines.push(Line::styled(
                            "Formless:",
                            role_style(app.color, "assistant"),
                        ));
                        for l in m.content.lines() {
                            lines.push(Line::raw(l.to_string()));
                        }
                    }
                    for call in calls {
                        lines.push(Line::styled(call, bold_fg(app.color, Color::Yellow)));
                    }
                    lines.push(Line::raw(""));
                    continue;
                }
            }
        }
        let who = match m.role.as_str() {
            "user" => "You",
            "tool" => "Tool result",
            "system-note" => "System",
            _ => "Formless",
        };
        lines.push(Line::styled(
            format!("{who}:"),
            role_style(app.color, m.role.as_str()),
        ));
        for l in m.content.lines() {
            lines.push(Line::raw(truncate_chars(l, 500)));
        }
        lines.push(Line::raw(""));
    }
    // Currently-running Tool calls (persisted lines above keep the history).
    for call in app.live_tools.iter().filter(|c| c.status == "running") {
        let version = if call.version > 0 {
            format!(" v{}", call.version)
        } else {
            String::new()
        };
        lines.push(Line::styled(
            format!(
                "⚙ running {}{} ({}) {}",
                call.name,
                version,
                format_elapsed(call.started.elapsed()),
                truncate_chars(&call.arguments, 160)
            ),
            bold_fg(app.color, Color::Yellow),
        ));
    }
    if app.live_tools.iter().any(|c| c.status == "running") {
        lines.push(Line::raw(""));
    }
    if app.messages.is_empty() && !app.running {
        lines.push(Line::styled(
            "Welcome to Formless.",
            bold_fg(app.color, Color::Cyan),
        ));
        lines.push(Line::styled(
            "Type a task below and press Enter — Formless builds the Tools it needs on demand.",
            fg(app.color, Color::Gray),
        ));
        lines.push(Line::styled(
            "Try /help for shortcuts, Tab to browse Sessions and Tools.",
            fg(app.color, Color::DarkGray),
        ));
        lines.push(Line::raw(""));
    }
    // Live reasoning: dimmed and separate from the answer (/thinking hides it).
    if app.show_thinking && !app.thinking.is_empty() {
        lines.push(Line::styled("Thinking:", thinking_style(app.color)));
        for l in app.thinking.lines() {
            lines.push(Line::styled(l.to_string(), thinking_style(app.color)));
        }
        lines.push(Line::raw(""));
    }
    if !app.streaming.is_empty() || app.running {
        lines.push(Line::styled(
            "Formless:",
            role_style(app.color, "assistant"),
        ));
        for l in app.streaming.lines() {
            lines.push(Line::raw(l.to_string()));
        }
        if app.streaming.is_empty() {
            lines.push(Line::styled(
                app.activity_text().unwrap_or_else(|| "…".into()),
                bold_fg(app.color, Color::Yellow),
            ));
        }
    }
    let height = (area.height.saturating_sub(2) as usize).max(1);
    app.chat_view_height = height;
    app.chat_line_count = lines.len();
    let max_top = lines.len().saturating_sub(height);
    let scroll_top = match app.chat_scroll_top {
        Some(top) => top.min(max_top),
        None => max_top,
    };
    if app.chat_scroll_top == Some(scroll_top) && scroll_top >= max_top {
        app.chat_scroll_top = None;
    }
    let title = if app.chat_scroll_top.is_some() {
        "Chat (scrolled — End/PageDown to follow live)"
    } else {
        "Chat"
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(app.color, Color::DarkGray))
                    .title(title),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll_top as u16, 0)),
        area,
    );
}

fn draw_wizard(f: &mut Frame, w: &Wizard, color: bool) {
    let area = f.area();
    let status_style = if w.step == WizardStep::Failed {
        bold_fg(color, Color::Red)
    } else {
        fg(color, Color::Gray)
    };
    let mut text = vec![
        Line::styled("Formless setup", bold_fg(color, Color::Cyan)),
        Line::raw(""),
        Line::styled(w.status.clone(), status_style),
        Line::raw(""),
    ];
    match w.step {
        WizardStep::BaseUrl => text.push(Line::raw(format!("Base URL: {}", w.base_url))),
        WizardStep::ApiKey | WizardStep::FetchModels => {
            text.push(Line::raw(format!("Base URL: {}", w.base_url)));
            text.push(Line::raw(format!(
                "API key: {}",
                "•".repeat(w.api_key.len())
            )));
        }
        WizardStep::PickModel => {
            if w.manual_model {
                text.push(Line::raw(format!("Model: {}", w.model())));
            } else {
                for (i, m) in w.models.iter().enumerate() {
                    let mark = if i == w.model_idx { "> " } else { "  " };
                    let style = if i == w.model_idx {
                        bold_fg(color, Color::Green)
                    } else {
                        Style::default()
                    };
                    text.push(Line::styled(format!("{mark}{m}"), style));
                }
            }
        }
        WizardStep::PickEffort => {
            text.push(Line::raw(format!("Model: {}", w.model())));
            for (i, e) in EFFORTS.iter().enumerate() {
                let mark = if i == w.effort_idx { "> " } else { "  " };
                let style = if i == w.effort_idx {
                    bold_fg(color, Color::Green)
                } else {
                    Style::default()
                };
                text.push(Line::styled(
                    format!("{mark}{}", e.unwrap_or("default (provider)")),
                    style,
                ));
            }
        }
        WizardStep::Probing | WizardStep::Failed => {
            text.push(Line::raw(format!("Model: {}", w.model())));
        }
    }
    text.push(Line::raw(""));
    text.push(Line::raw("Ctrl+C to quit."));
    f.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(color, Color::Cyan))
                    .title("Onboarding (SPEC §10.1)"),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

pub fn run(paths: Paths) -> Result<()> {
    let mut app = App::new(paths)?;
    enable_raw_mode()?;
    let mut out = stdout();
    out.execute(EnterAlternateScreen)?;
    out.execute(crossterm::event::EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let result = (|| -> Result<()> {
        while !app.should_quit {
            app.tick = app.tick.wrapping_add(1);
            app.drain_daemon();
            app.wizard_poll();
            terminal.draw(|f| draw(f, &mut app))?;
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    CtEvent::Key(k) => app.key(k),
                    CtEvent::Mouse(m) => app.mouse(m.kind),
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    disable_raw_mode()?;
    let _ = stdout().execute(crossterm::event::DisableMouseCapture);
    stdout().execute(LeaveAlternateScreen)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let short = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::path::PathBuf::from(format!("/tmp/fl-tui-{short}"));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        App {
            paths,
            view: View::Chat,
            wizard: None,
            client: None,
            inbox: None,
            sessions: Vec::new(),
            session_idx: 0,
            current: Some("s1".into()),
            messages: Vec::new(),
            streaming: String::new(),
            thinking: String::new(),
            show_thinking: true,
            color: true,
            tick: 0,
            activity: None,
            live_tools: Vec::new(),
            chat_scroll_top: None,
            chat_view_height: 10,
            chat_line_count: 100,
            input: String::new(),
            running: false,
            notes: Vec::new(),
            pending: HashMap::new(),
            tools: Vec::new(),
            tool_idx: 0,
            tool_detail: None,
            approvals: std::collections::VecDeque::new(),
            promotions: std::collections::VecDeque::new(),
            purge_arm: None,
            import_mode: None,
            renaming: false,
            search_hits: Vec::new(),
            search_idx: 0,
            search_mode: false,
            editing_id: None,
            should_quit: false,
        }
    }

    #[test]
    fn activity_and_thinking_follow_run_events() {
        let mut app = test_app();
        app.on_event(Event::RunStarted {
            session_id: "s1".into(),
            run_id: "r1".into(),
        });
        assert!(app.running);
        assert!(app.activity_text().unwrap().contains("waiting for model"));

        app.on_event(Event::ReasoningDelta {
            session_id: "s1".into(),
            run_id: "r1".into(),
            delta: "think ".into(),
        });
        app.on_event(Event::ReasoningDelta {
            session_id: "s1".into(),
            run_id: "r1".into(),
            delta: "more".into(),
        });
        assert_eq!(app.thinking, "think more");
        assert!(app.activity_text().unwrap().contains("thinking"));

        app.on_event(Event::ToolCallStarted {
            session_id: "s1".into(),
            run_id: "r1".into(),
            tool_name: "create_tool".into(),
            version: 1,
            arguments: "{\"name\":\"writer\"}".into(),
        });
        assert!(app.activity_text().unwrap().contains("running create_tool"));
        assert_eq!(app.live_tools.len(), 1);
        assert_eq!(app.live_tools[0].status, "running");
        app.on_event(Event::ToolCallFinished {
            session_id: "s1".into(),
            run_id: "r1".into(),
            tool_name: "create_tool".into(),
            ok: true,
            elapsed_ms: 42,
        });
        assert_eq!(app.live_tools[0].status, "ok");
        assert_eq!(app.live_tools[0].elapsed_ms, Some(42));

        app.on_event(Event::ApprovalRequested {
            approval_id: "a1".into(),
            session_id: "s1".into(),
            tool_name: "create_tool".into(),
            version: 1,
            capabilities: vec![],
            memory_mb: 256,
        });
        assert!(app
            .activity_text()
            .unwrap()
            .contains("awaiting your approval"));

        app.on_event(Event::RunFinished {
            session_id: "s1".into(),
            run_id: "r1".into(),
            status: "completed".into(),
        });
        assert!(!app.running);
        assert!(app.activity_text().is_none());
        assert!(app.thinking.is_empty(), "thinking is ephemeral per Run");
    }

    #[test]
    fn thinking_toggle_and_elapsed_and_spinner() {
        let mut app = test_app();
        app.slash("thinking");
        assert!(!app.show_thinking);
        app.slash("thinking");
        assert!(app.show_thinking);
        assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1m05s");
        let first = app.spinner();
        app.tick += 2;
        assert_ne!(first, app.spinner(), "spinner must animate with ticks");
    }

    #[test]
    fn chat_scrolling_pins_and_refollows_at_bottom() {
        let mut app = test_app(); // 100 lines, view height 10 -> max top 90
        assert_eq!(app.chat_max_top(), 90);
        app.scroll_chat_up(30);
        assert_eq!(app.chat_scroll_top, Some(60));
        app.scroll_chat_up(100);
        assert_eq!(app.chat_scroll_top, Some(0));
        app.scroll_chat_down(50);
        assert_eq!(app.chat_scroll_top, Some(50));
        app.scroll_chat_down(50);
        assert_eq!(app.chat_scroll_top, None, "reaching bottom refollows live");
        app.scroll_chat_up(5);
        app.follow_chat_bottom();
        assert_eq!(app.chat_scroll_top, None);
    }

    #[test]
    fn persisted_tool_calls_render_from_meta() {
        let meta = serde_json::json!({"tool_calls": [
            {"id": "1", "type": "function", "function": {"name": "create_tool", "arguments": "{\"name\":\"w\"}"}},
            {"id": "2", "type": "function", "function": {"name": "writer", "arguments": "{}"}}
        ]});
        let lines = tool_call_lines(&meta.to_string());
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("create_tool"), "{lines:?}");
        assert!(tool_call_lines("not json").is_empty());
        assert!(tool_call_lines("{}").is_empty());
    }

    #[test]
    fn wizard_remembers_saved_startup_options() {
        let mut cfg = Config::default();
        cfg.provider.base_url = "https://api.deepseek.com".into();
        cfg.provider.model = "deepseek-flash".into();
        cfg.provider.reasoning_effort = Some("high".into());
        let w = Wizard::new(&cfg);
        assert_eq!(w.step, WizardStep::ApiKey);
        assert_eq!(w.base_url, "https://api.deepseek.com");
        assert_eq!(w.saved_model, "deepseek-flash");
        assert_eq!(EFFORTS[w.effort_idx], Some("high"));

        let fresh = Wizard::new(&Config::default());
        assert_eq!(fresh.step, WizardStep::BaseUrl);
    }

    #[test]
    fn metrics_lines_summarise_tools() {
        let tools = vec![
            ToolInfo {
                id: "1".into(),
                name: "writer".into(),
                description: String::new(),
                retired: false,
                current_version: 2,
                capabilities: vec![],
                memory_mb: 256,
                invocations: 10,
                success_rate: Some(0.9),
                needs_attention: false,
                created_at: String::new(),
                updated_at: String::new(),
            },
            ToolInfo {
                id: "2".into(),
                name: "old".into(),
                description: String::new(),
                retired: true,
                current_version: 1,
                capabilities: vec![],
                memory_mb: 256,
                invocations: 2,
                success_rate: None,
                needs_attention: true,
                created_at: String::new(),
                updated_at: String::new(),
            },
        ];
        let lines = metrics_lines(&tools);
        assert!(lines[0].contains("Total invocations: 12"), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("90%")));
        assert!(metrics_lines(&[])
            .iter()
            .any(|l| l.contains("no Tools yet")));
    }

    #[test]
    fn edit_slash_prefills_last_user_message() {
        let mut app = test_app();
        let mk = |id: &str, role: &str, content: &str| MessageInfo {
            id: id.into(),
            session_id: "s1".into(),
            seq: 1,
            role: role.into(),
            content: content.into(),
            created_at: String::new(),
            meta: None,
        };
        app.messages.push(mk("u1", "user", "first"));
        app.messages.push(mk("a1", "assistant", "reply"));
        app.messages.push(mk("u2", "user", "second"));
        app.slash("edit");
        assert_eq!(app.editing_id.as_deref(), Some("u2"));
        assert_eq!(app.input, "second");
    }

    #[test]
    fn no_color_styles_carry_no_foreground() {
        assert_eq!(fg(false, Color::Cyan).fg, None);
        assert_eq!(role_style(false, "user").fg, None);
        assert_eq!(fg(true, Color::Cyan).fg, Some(Color::Cyan));
        assert_eq!(role_style(true, "user").fg, Some(Color::Cyan));
        assert_eq!(role_style(true, "assistant").fg, Some(Color::Green));
    }
}
