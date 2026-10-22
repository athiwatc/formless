pub mod client;
mod commands;
mod highlight;
mod markdown;
mod mermaid;
mod picker;
mod ui;

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use formless_core::auth::has_provider_credential;
use formless_core::config::{save_api_key, Config};
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

/// Multi-pane layout is active only when persisted on and wide enough.
pub(crate) fn pane_layout(multi_pane: bool, width: u16) -> bool {
    multi_pane && width >= 110
}

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

#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    Chat,
    Sessions,
    Settings,
    Tools,
    Metrics,
    Proposals,
    Search,
}

/// Tab / Shift+Tab order (Search is transient and only entered via /search).
const VIEW_ORDER: [View; 6] = [
    View::Chat,
    View::Sessions,
    View::Tools,
    View::Metrics,
    View::Proposals,
    View::Settings,
];

impl View {
    fn label(self) -> &'static str {
        match self {
            View::Chat => "Chat",
            View::Sessions => "Sessions",
            View::Settings => "Settings",
            View::Tools => "Tools",
            View::Metrics => "Metrics",
            View::Proposals => "Proposals",
            View::Search => "Search",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pending {
    ListSessions,
    GetSession,
    CreateSession,
    ForkSession,
    SearchSessions,
    ListTools,
    ListModules,
    ListModelStats,
    ListProposals,
    UpdateTool,
    GetTool,
    ExportTool,
    ExportSession,
    /// Resume/abandon: the answer is a note (errors already are).
    RunControl,
    ListSubagents,
    GetSubagent,
    ListMcpServers,
    ListMcpPrompts,
    GetMcpPrompt,
    ListSkills,
    ListMemories,
    RunMetrics,
    ListBuiltinStats,
    ListJobs,
    ListAudit,
    SessionUsage,
    ListInvocations,
    GetArtifact,
    GenerateProposal,
    ListProviders,
    ListModels,
    SetModel,
    ListFiles,
    MentionAttach,
    HarnessChanges,
}

#[derive(Clone, Copy, PartialEq)]
enum ImportKind {
    Tool,
    Session,
}

/// Choosing a version to roll a Tool back to; `armed` after the first
/// Enter, so a second one confirms.
struct RollbackPick {
    tool_id: String,
    idx: usize,
    armed: bool,
}

/// `/model`: pick a model and reasoning effort for the current Session,
/// or (Ctrl+Enter / Alt+Enter / Ctrl+D) as the global default.
struct ModelOverlay {
    picker: picker::ModelPicker,
    effort_idx: usize,
}

/// A row of the `/` palette: a command, or an argument for one.
struct PaletteItem {
    /// The input after accepting this row.
    insert: String,
    label: String,
    desc: String,
    /// Enter runs it as is (no required argument still missing).
    runnable: bool,
}

/// Completions for the `@path` being typed at the end of the input.
struct MentionPopup {
    query: String,
    files: Vec<formless_core::files::FileEntry>,
    sel: usize,
    /// Esc hides the popup until the query changes.
    dismissed: bool,
}

struct PendingSend {
    session_id: String,
    content: String,
    mode: SendMode,
    waiting: usize,
}

/// An open `ask_user` call (SPEC §5.5); `answers` fills as the user
/// answers its questions in order.
struct OpenQuestion {
    id: String,
    session_id: String,
    questions: Vec<Question>,
    answers: Vec<QuestionAnswer>,
}

impl OpenQuestion {
    /// The chat note for the next unanswered question.
    fn prompt(&self) -> String {
        let n = self.answers.len();
        let q = &self.questions[n];
        let mut text = String::from("question");
        if self.questions.len() > 1 {
            text.push_str(&format!(" {}/{}", n + 1, self.questions.len()));
        }
        if let Some(h) = &q.header {
            text.push_str(&format!(" [{h}]"));
        }
        text.push_str(&format!(": {}", q.question));
        for (i, o) in q.options.iter().enumerate() {
            text.push_str(&format!("\n  {}) {}", i + 1, o.label));
            if let Some(d) = &o.description {
                text.push_str(&format!(" — {d}"));
            }
        }
        text.push_str(match (q.options.is_empty(), q.multi_select) {
            (true, _) => "\n  type your answer and press Enter",
            (false, false) => "\n  type an option number or your own answer and press Enter",
            (false, true) => {
                "\n  type option numbers (e.g. 1,3) or your own answer and press Enter"
            }
        });
        text
    }
}

#[derive(Clone)]
struct ApprovalReq {
    approval_id: String,
    tool_name: String,
    version: i64,
    capabilities: Vec<String>,
    memory_mb: u32,
    /// What exactly is being approved, for non-Tool approvals.
    detail: Option<String>,
    /// Capabilities already granted (the rest are new).
    already_allowed: Vec<String>,
    code_preview: Option<String>,
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
    picker: picker::ModelPicker,
    effort_idx: usize,
    status: String,
    /// Previously saved model, pre-selected when the list arrives (§17).
    saved_model: String,
    prompt_fallback: bool,
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
            picker: picker::ModelPicker::loading(),
            effort_idx,
            status,
            saved_model: cfg.provider.model.clone(),
            prompt_fallback: false,
            jobs,
            job_tx,
        }
    }
    fn model(&self) -> String {
        self.picker.choice().unwrap_or_default()
    }
}

struct Activity {
    label: String,
    since: Instant,
}

struct EditorState {
    tool_id: String,
    name: String,
    chars: Vec<char>,
    cursor: usize,
}

impl EditorState {
    fn text(&self) -> String {
        self.chars.iter().collect()
    }
    fn move_vertical(&mut self, down: bool) {
        // Column = chars since previous newline; target line start found by scans.
        let mut col = 0usize;
        let mut i = self.cursor;
        while i > 0 && self.chars[i - 1] != '\n' {
            i -= 1;
            col += 1;
        }
        if down {
            let mut j = self.cursor;
            while j < self.chars.len() && self.chars[j] != '\n' {
                j += 1;
            }
            if j >= self.chars.len() {
                return;
            }
            let start = j + 1;
            let mut end = start;
            while end < self.chars.len() && self.chars[end] != '\n' {
                end += 1;
            }
            self.cursor = (start + col).min(end);
        } else {
            if i == 0 {
                return;
            }
            let prev_end = i - 1;
            let mut start = prev_end;
            while start > 0 && self.chars[start - 1] != '\n' {
                start -= 1;
            }
            self.cursor = (start + col).min(prev_end);
        }
    }
}

#[derive(Clone)]
/// A full-screen text viewer (artifact outputs, SPEC §9.6).
struct Pager {
    title: String,
    lines: Vec<String>,
    top: usize,
}

struct LiveToolCall {
    name: String,
    /// Tree depth: nested `tools.call`, or a sub-agent under its Tool.
    depth: usize,
    /// The sub-agent this node stands for.
    subagent: Option<String>,
    version: i64,
    arguments: String,
    status: String,
    started: Instant,
    elapsed_ms: Option<i64>,
    /// Invocation handle once the daemon names it (pending or stalled).
    handle: Option<String>,
    /// Seconds without activity, while the daemon flags it stalled.
    stalled_secs: Option<u64>,
}

/// The live calls to draw, with how many descendants each folds away. A
/// node shows while it or an ancestor is running; under a node that
/// finished successfully its descendants fold into a count.
fn live_tree(calls: &[LiveToolCall]) -> Vec<(&LiveToolCall, usize)> {
    let mut out: Vec<(&LiveToolCall, usize)> = Vec::new();
    // Stack of (depth, running, open) for the current ancestors.
    let mut stack: Vec<(usize, bool, bool)> = Vec::new();
    for c in calls {
        while stack.last().is_some_and(|(d, _, _)| *d >= c.depth) {
            stack.pop();
        }
        let parent_open = stack.last().is_none_or(|(_, _, open)| *open);
        let ancestor_running = stack.iter().any(|(_, running, _)| *running);
        let running = c.status == "running";
        let visible = parent_open && (running || ancestor_running);
        // Children show under running or failed nodes; ok ones fold.
        let open = visible && c.status != "ok";
        if visible {
            out.push((c, 0));
        } else if let Some(idx) = out
            .iter()
            .rposition(|(n, _)| n.depth < c.depth && n.status == "ok")
            .filter(|_| ancestor_running && !parent_open)
        {
            out[idx].1 += 1;
        }
        stack.push((c.depth, running, open));
    }
    out
}

/// Share of a model's prompt tokens served from the provider's cache.
fn cache_hit_text(st: &ModelStatInfo) -> String {
    if st.prompt_tokens > 0 {
        format!(
            "{:.0}%",
            100.0 * st.cached_tokens as f64 / st.prompt_tokens as f64
        )
    } else {
        "-".into()
    }
}

/// One line of a sub-agent transcript (provider-shaped message).
fn subagent_line(m: &serde_json::Value) -> String {
    let role = m["role"].as_str().unwrap_or("?");
    let text = match m["tool_calls"].as_array() {
        Some(calls) => format!(
            "calls {}",
            calls
                .iter()
                .filter_map(|c| c["function"]["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => m["content"].as_str().unwrap_or_default().replace('\n', " "),
    };
    format!("  [{role}] {}", truncate_chars(&text, 120))
}

/// Identifies a rendered streaming prefix without hashing all of it: the
/// stream only grows (or restarts from empty), so its length plus both
/// ends tell one prefix from another.
#[derive(PartialEq)]
struct StreamKey {
    len: usize,
    head: u64,
    tail: u64,
    width: usize,
}

impl StreamKey {
    fn new(prefix: &str, width: usize) -> Self {
        use std::hash::{Hash, Hasher};
        let hash = |bytes: &[u8]| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            h.finish()
        };
        let b = prefix.as_bytes();
        Self {
            len: b.len(),
            head: hash(&b[..b.len().min(256)]),
            tail: hash(&b[b.len().saturating_sub(256)..]),
            width,
        }
    }
}

/// Byte length of the part of a streaming Markdown answer that is complete:
/// up to and including its last blank line outside a code fence. Blocks
/// before it render the same however much more text arrives.
fn stable_prefix_len(text: &str) -> usize {
    let mut in_fence = false;
    let mut stable = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if trimmed.is_empty() && !in_fence && line.ends_with('\n') {
            stable = offset;
        }
    }
    stable
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
            format!("⚙ {name}  {}", ui::summarize_args(args, 220))
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
    multi_pane: bool,
    /// Configured model, shown in the status bar.
    model_label: String,
    tick: u64,
    activity: Option<Activity>,
    live_tools: Vec<LiveToolCall>,
    /// Absolute chat scroll top; `None` means follow the live bottom.
    chat_scroll_top: Option<usize>,
    chat_view_height: usize,
    chat_line_count: usize,
    input: String,
    /// When Ctrl+C was last pressed: a second press soon after detaches.
    ctrl_c_at: Option<Instant>,
    /// The terminal reports Ctrl+Enter apart from Enter (kitty keyboard
    /// protocol); otherwise queueing uses Ctrl+J / Alt+Enter.
    enhanced_keys: bool,
    /// Mouse capture (wheel scrolling); off lets the terminal select text.
    mouse_capture: bool,
    /// A prompt proposal is being generated (Proposals view shows it).
    proposal_generating: bool,
    /// Sessions with a Run in flight; run state is per session, not global.
    running_sessions: std::collections::HashSet<String>,
    notes: Vec<String>,
    pending: HashMap<u64, Pending>,
    tools: Vec<ToolInfo>,
    tool_idx: usize,
    tool_detail: Option<ToolDetail>,
    /// Open rollback version picker (Tools view, `b`).
    rollback_pick: Option<RollbackPick>,
    /// The `/model` picker (SPEC §17).
    model_overlay: Option<ModelOverlay>,
    /// The `@` file-completion popup (SPEC §5.6).
    mention: Option<MentionPopup>,
    /// A message waiting for its `@` attachments before it is sent.
    pending_send: Option<PendingSend>,
    /// The `/` palette's selected row.
    palette_sel: usize,
    /// The input the palette was dismissed (Esc) for.
    palette_hidden: Option<String>,
    approvals: std::collections::VecDeque<ApprovalReq>,
    /// Open `ask_user` calls. The next typed line answers the current
    /// Session's oldest one, one question at a time.
    questions: Vec<OpenQuestion>,
    /// Latest Run usage per Session, for the status line (SPEC §10.6).
    run_usage: HashMap<String, formless_protocol::UsageInfo>,
    /// The current Session's unfinished last Run: (status, reason).
    run_banner: Option<(String, String)>,
    /// Last listed running Invocations (for /kill and /drop by prefix).
    invocations: Vec<formless_protocol::InvocationInfo>,
    promotions: std::collections::VecDeque<ApprovalReq>,
    purge_arm: Option<String>,
    /// The last deleted Session, restorable with /undo while its window lasts.
    last_deleted: Option<(String, String)>,
    /// Open artifact viewer (/open).
    pager: Option<Pager>,
    /// Session id armed for deletion by a first `d` press.
    delete_arm: Option<String>,
    /// The current Session's sub-agents, as last listed by `/agents`.
    subagents: Vec<SubagentInfo>,
    import_mode: Option<ImportKind>,
    renaming: bool,
    modules: Vec<ModuleInfo>,
    model_stats: Vec<ModelStatInfo>,
    /// Run outcomes for the Metrics title (SPEC §8.7).
    run_summary: Option<formless_protocol::RunSummary>,
    /// Built-in call stats (SPEC §8.8).
    builtin_stats: Vec<formless_protocol::BuiltinStatInfo>,
    proposals: Vec<PromptProposalInfo>,
    proposal_idx: usize,
    editor: Option<EditorState>,
    search_hits: Vec<SearchHit>,
    search_idx: usize,
    search_mode: bool,
    editing_id: Option<String>,
    should_quit: bool,
    /// Rendered Markdown of settled messages, keyed by content and width.
    /// Rendered settled messages by (id, content length, width).
    md_cache: std::cell::RefCell<HashMap<(String, usize, usize), Vec<Line<'static>>>>,
    /// The streaming answer's settled prefix: (key, rendered lines).
    stream_cache: std::cell::RefCell<Option<(StreamKey, Vec<Line<'static>>)>>,
}

impl App {
    /// Markdown for a settled message, rendered once per message and width
    /// (assistant messages never change; reloads clear the cache).
    fn markdown_cached(&self, m: &MessageInfo, width: usize) -> Vec<Line<'static>> {
        let content = &m.content;
        let key = (m.id.clone(), content.len(), width);
        let mut cache = self.md_cache.borrow_mut();
        if let Some(lines) = cache.get(&key) {
            return lines.clone();
        }
        if cache.len() > 1024 {
            cache.clear();
        }
        let lines = markdown::render(content, self.color, "  ", width);
        cache.insert(key, lines.clone());
        lines
    }

    /// The live answer, rendered incrementally: everything up to its last
    /// block boundary is rendered once and reused while tokens arrive, so
    /// each frame only renders the unfinished tail block.
    fn streaming_markdown(&self, width: usize) -> Vec<Line<'static>> {
        let text = &self.streaming;
        let split = stable_prefix_len(text);
        let (prefix, tail) = text.split_at(split);
        let key = StreamKey::new(prefix, width);
        let mut cache = self.stream_cache.borrow_mut();
        let mut lines = match cache.as_ref() {
            Some((k, lines)) if *k == key => lines.clone(),
            _ => {
                let lines = markdown::render(prefix, self.color, "  ", width);
                *cache = Some((key, lines.clone()));
                lines
            }
        };
        if !lines.is_empty() && !tail.is_empty() {
            // The block gap a single render would put between them.
            lines.push(Line::default());
        }
        lines.extend(markdown::render(tail, self.color, "  ", width));
        lines
    }

    fn new(paths: Paths) -> Result<Self> {
        let cfg = Config::load(&paths)?;
        let has_credential = has_provider_credential(&paths, &cfg.provider);
        // OAuth onboarding is CLI-led. Do not send an OAuth-configured user
        // through the API-key field; daemon auth errors point to `auth login`.
        let needs_wizard =
            !cfg.provider.is_configured() || (!cfg.provider.oauth() && !has_credential);
        let oauth_login_required =
            cfg.provider.is_configured() && cfg.provider.oauth() && !has_credential;
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
            multi_pane: cfg.tui.multi_pane,
            model_label: cfg.provider.model.clone(),
            tick: 0,
            activity: None,
            live_tools: Vec::new(),
            chat_scroll_top: None,
            chat_view_height: 10,
            chat_line_count: 0,
            input: String::new(),
            ctrl_c_at: None,
            enhanced_keys: false,
            mouse_capture: true,
            proposal_generating: false,
            running_sessions: Default::default(),
            notes: Vec::new(),
            pending: HashMap::new(),
            tools: Vec::new(),
            tool_idx: 0,
            tool_detail: None,
            rollback_pick: None,
            model_overlay: None,
            mention: None,
            pending_send: None,
            palette_sel: 0,
            palette_hidden: None,
            approvals: std::collections::VecDeque::new(),
            questions: Vec::new(),
            run_usage: HashMap::new(),
            run_banner: None,
            invocations: Vec::new(),
            promotions: std::collections::VecDeque::new(),
            purge_arm: None,
            last_deleted: None,
            pager: None,
            delete_arm: None,
            subagents: Vec::new(),
            import_mode: None,
            renaming: false,
            modules: Vec::new(),
            model_stats: Vec::new(),
            run_summary: None,
            builtin_stats: Vec::new(),
            proposals: Vec::new(),
            proposal_idx: 0,
            editor: None,
            search_hits: Vec::new(),
            search_idx: 0,
            search_mode: false,
            editing_id: None,
            should_quit: false,
            md_cache: Default::default(),
            stream_cache: Default::default(),
        };
        if oauth_login_required {
            app.notes
                .push("OAuth login required: run `formless auth login`.".into());
        }
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
                        if matches!(kind, Pending::MentionAttach) {
                            self.mention_attached();
                        }
                        continue;
                    }
                    let Some(result) = result else { continue };
                    match kind {
                        Pending::ListSessions => {
                            if let Ok(v) = serde_json::from_value::<Vec<SessionInfo>>(result) {
                                self.sessions = v;
                                if self.current.is_none() {
                                    if let Some(first) = self.sessions.first().cloned() {
                                        self.switch_session(first.id.clone());
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
                                self.switch_session(s.id.clone());
                                self.sessions.insert(0, s);
                                self.messages.clear();
                                self.view = View::Chat;
                            }
                        }
                        Pending::ForkSession => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.switch_session(s.id.clone());
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
                        Pending::ListBuiltinStats => {
                            self.builtin_stats = serde_json::from_value(result).unwrap_or_default();
                        }
                        Pending::RunMetrics => {
                            self.run_summary =
                                serde_json::from_value(result["summary"].clone()).ok();
                        }
                        Pending::ListModelStats => {
                            if let Ok(v) = serde_json::from_value::<Vec<ModelStatInfo>>(result) {
                                self.model_stats = v;
                            }
                        }
                        Pending::ListProposals => {
                            if let Ok(v) = serde_json::from_value::<Vec<PromptProposalInfo>>(result)
                            {
                                self.proposals = v;
                                if self.proposal_idx >= self.proposals.len() {
                                    self.proposal_idx = self.proposals.len().saturating_sub(1);
                                }
                            }
                        }
                        Pending::ListModules => {
                            if let Ok(v) = serde_json::from_value::<Vec<ModuleInfo>>(result) {
                                self.modules = v;
                            }
                        }
                        Pending::UpdateTool => {
                            let name = result["tool"]["name"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string();
                            let version = result["version"].as_i64().unwrap_or_default();
                            self.notes.push(format!("saved {name} v{version}"));
                            self.editor = None;
                            self.request(Pending::ListTools, Request::ListTools);
                            if let Some(t) = self.tools.get(self.tool_idx).cloned() {
                                self.request(Pending::GetTool, Request::GetTool { id: t.id });
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
                        Pending::ListSubagents => {
                            self.subagents = serde_json::from_value(result).unwrap_or_default();
                            if self.subagents.is_empty() {
                                self.notes.push("no sub-agents in this session".into());
                            }
                            let lines: Vec<String> = self
                                .subagents
                                .iter()
                                .enumerate()
                                .map(|(i, a)| {
                                    format!(
                                        "#{} {} · {} step(s) · {}",
                                        i + 1,
                                        a.status,
                                        a.steps,
                                        truncate_chars(&a.task, 70)
                                    )
                                })
                                .collect();
                            self.notes.extend(lines);
                            if !self.subagents.is_empty() {
                                self.notes
                                    .push("/agents <n> shows that sub-agent's transcript".into());
                            }
                        }
                        Pending::ListMcpServers => {
                            let list: Vec<McpServerInfo> =
                                serde_json::from_value(result).unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push(
                                    "no MCP servers (add one with `formless mcp add`)".into(),
                                );
                            }
                            for s in list {
                                let state = if s.disabled {
                                    "disabled"
                                } else if s.running {
                                    "running"
                                } else {
                                    "idle"
                                };
                                self.notes.push(format!(
                                    "mcp {} [{}] {} · {} tools · {}{}",
                                    s.name,
                                    s.transport,
                                    state,
                                    s.tool_count,
                                    truncate_chars(&s.target, 50),
                                    s.last_error
                                        .map(|e| format!(" · error: {}", truncate_chars(&e, 80)))
                                        .unwrap_or_default()
                                ));
                            }
                        }
                        Pending::ListInvocations => {
                            self.invocations = serde_json::from_value(result).unwrap_or_default();
                            if self.invocations.is_empty() {
                                self.notes.push("no running invocations".into());
                            }
                            for i in &self.invocations {
                                let session = self
                                    .sessions
                                    .iter()
                                    .find(|s| s.id == i.session_id)
                                    .map(|s| s.name.clone())
                                    .unwrap_or_else(|| truncate_chars(&i.session_id, 8));
                                self.notes.push(format!(
                                    "{} {} · {}s · {}{}{} · /kill or /drop {}",
                                    if i.stalled { "⚠" } else { "•" },
                                    i.tool_name,
                                    i.elapsed_ms / 1000,
                                    session,
                                    if i.stalled {
                                        format!(" · silent {}s", i.silent_ms / 1000)
                                    } else {
                                        String::new()
                                    },
                                    if i.abandoned { " · abandoned" } else { "" },
                                    truncate_chars(&i.handle, 8)
                                ));
                            }
                        }
                        Pending::HarnessChanges => {
                            let list = result.as_array().cloned().unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push("no agent changes to the harness".into());
                            }
                            for c in list.iter().take(20).rev() {
                                self.notes.push(format!(
                                    "{} {} {} · by {} · {}{}",
                                    c["id"].as_str().unwrap_or_default(),
                                    c["op"].as_str().unwrap_or_default(),
                                    c["target"].as_str().unwrap_or_default(),
                                    c["tool"].as_str().unwrap_or_default(),
                                    truncate_chars(c["reason"].as_str().unwrap_or_default(), 60),
                                    if c["undone"] == true {
                                        " · undone"
                                    } else {
                                        ""
                                    }
                                ));
                            }
                        }
                        Pending::ListModels => {
                            if let Some(o) = &mut self.model_overlay {
                                let current = self
                                    .current
                                    .as_ref()
                                    .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
                                    .and_then(|s| s.model.clone())
                                    .or_else(|| {
                                        result["current"]["model"].as_str().map(str::to_string)
                                    })
                                    .unwrap_or_default();
                                let items = picker::items_from_list(&result);
                                o.picker.set_items(items, &current);
                                o.picker.error = result["error"].as_str().map(str::to_string);
                            }
                        }
                        Pending::ListFiles => {
                            let query = result["query"].as_str().unwrap_or_default();
                            if let Some(m) = self.mention.as_mut().filter(|m| m.query == query) {
                                m.files = serde_json::from_value(result["files"].clone())
                                    .unwrap_or_default();
                                m.sel = 0;
                            }
                        }
                        Pending::MentionAttach => {
                            let name = result["name"].as_str().unwrap_or_default();
                            self.notes.push(if result["reference"] == true {
                                format!("referenced {name} (the agent reads it with its Tools)")
                            } else {
                                format!(
                                    "attached {name} ({} KB)",
                                    result["bytes"].as_u64().unwrap_or(0) / 1024
                                )
                            });
                            self.mention_attached();
                        }
                        Pending::SetModel => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.notes.push(format!(
                                    "session model: {} (effort {})",
                                    s.model.as_deref().unwrap_or("default"),
                                    s.reasoning_effort.as_deref().unwrap_or("default")
                                ));
                                if let Some(old) = self.sessions.iter_mut().find(|x| x.id == s.id) {
                                    *old = s;
                                }
                            }
                        }
                        Pending::ListProviders => {
                            for p in result["providers"].as_array().into_iter().flatten() {
                                self.notes.push(format!(
                                    "provider {} · {} · model {}{}",
                                    p["name"].as_str().unwrap_or_default(),
                                    p["base_url"].as_str().unwrap_or_default(),
                                    p["model"].as_str().filter(|m| !m.is_empty()).unwrap_or("-"),
                                    if p["has_key"] == true {
                                        ""
                                    } else {
                                        " · no key"
                                    }
                                ));
                            }
                            let roles = result["roles"].as_object().cloned().unwrap_or_default();
                            if roles.is_empty() {
                                self.notes.push(
                                    "no roles · /role add NAME PROVIDER MODEL description…".into(),
                                );
                            }
                            for (name, r) in roles {
                                self.notes.push(format!(
                                    "role {name} · {} · {} · {}",
                                    r["provider"].as_str().unwrap_or("default"),
                                    r["model"]
                                        .as_str()
                                        .filter(|m| !m.is_empty())
                                        .unwrap_or("provider default"),
                                    r["description"].as_str().unwrap_or_default()
                                ));
                            }
                        }
                        Pending::GenerateProposal => {
                            if result["started"] == true {
                                self.proposal_generating = true;
                                self.notes.push(
                                    "generating a proposal: one model call plus the eval suite (costs tokens)"
                                        .into(),
                                );
                            } else {
                                self.notes.push(
                                    result["reason"]
                                        .as_str()
                                        .unwrap_or("not started")
                                        .to_string(),
                                );
                            }
                        }
                        Pending::GetArtifact => {
                            let text = result["text"].as_str().unwrap_or_default();
                            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
                            if result["end"].as_u64() < result["total_chars"].as_u64() {
                                lines.push(format!(
                                    "… showing {} of {} characters",
                                    result["end"], result["total_chars"]
                                ));
                            }
                            self.pager = Some(Pager {
                                title: format!(
                                    "artifact {} · {} bytes",
                                    truncate_chars(result["id"].as_str().unwrap_or_default(), 8),
                                    result["bytes"]
                                ),
                                lines,
                                top: 0,
                            });
                        }
                        Pending::SessionUsage => {
                            let get = |k: &str| {
                                serde_json::from_value::<formless_protocol::UsageInfo>(
                                    result[k].clone(),
                                )
                                .ok()
                            };
                            if let Some(last) = get("last_run") {
                                self.notes.push(format!("last Run: {}", usage_text(&last)));
                            }
                            if let Some(total) = get("total") {
                                self.notes.push(format!(
                                    "session ({} Runs): {}",
                                    result["runs"],
                                    usage_text(&total)
                                ));
                            }
                        }
                        Pending::ListAudit => {
                            let list: Vec<formless_protocol::AuditEntry> =
                                serde_json::from_value(result).unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push("no audit entries match".into());
                            }
                            for e in list.iter().rev() {
                                self.notes.push(format!(
                                    "audit {} {} {} {}",
                                    truncate_chars(&e.created_at, 19),
                                    e.actor,
                                    e.action,
                                    truncate_chars(&e.detail, 80)
                                ));
                            }
                        }
                        Pending::ListJobs => {
                            let list: Vec<formless_protocol::JobInfo> =
                                serde_json::from_value(result).unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push("no scheduled jobs".into());
                            }
                            for j in list {
                                self.notes.push(format!(
                                    "job {} [{}] {} · next {} · last {} · {}",
                                    j.id,
                                    if !j.approved {
                                        "awaiting approval"
                                    } else if j.enabled {
                                        "on"
                                    } else {
                                        "off"
                                    },
                                    j.cron.as_deref().unwrap_or("once"),
                                    j.next_run
                                        .as_deref()
                                        .map(|t| truncate_chars(t, 16))
                                        .unwrap_or_else(|| "-".into()),
                                    if j.last_status.is_empty() {
                                        "-"
                                    } else {
                                        &j.last_status
                                    },
                                    truncate_chars(&j.prompt, 60)
                                ));
                            }
                        }
                        Pending::ListMemories => {
                            let list: Vec<formless_protocol::MemoryInfo> =
                                serde_json::from_value(result).unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push(
                                    "no memories (the agent saves preferences with remember)"
                                        .into(),
                                );
                            }
                            for m in list {
                                self.notes.push(format!(
                                    "memory {} [{}{}] {} · {}",
                                    m.id,
                                    m.scope,
                                    if m.enabled { "" } else { ", off" },
                                    m.text,
                                    m.session_id
                                        .map(|s| format!("from session {}", truncate_chars(&s, 8)))
                                        .unwrap_or(m.author)
                                ));
                            }
                        }
                        Pending::ListSkills => {
                            let list: Vec<SkillInfo> =
                                serde_json::from_value(result).unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push(
                                    "no skills (install one with `formless skills add`)".into(),
                                );
                            }
                            for s in list {
                                self.notes.push(format!(
                                    "skill {} v{} {} · {} loads{} · {}{}{}",
                                    s.name,
                                    s.version,
                                    if s.retired {
                                        "retired"
                                    } else if s.enabled {
                                        "enabled"
                                    } else {
                                        "disabled"
                                    },
                                    s.loads,
                                    skill_success(&s),
                                    truncate_chars(&s.description, 70),
                                    if s.has_scripts { " · scripts" } else { "" },
                                    if s.author == "auto" { " · auto" } else { "" }
                                ));
                            }
                        }
                        Pending::ListMcpPrompts => {
                            let list = result.as_array().cloned().unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push("no MCP prompts available".into());
                            }
                            for p in list {
                                let args: Vec<String> = p["arguments"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|a| a["name"].as_str().map(|n| format!("{n}=…")))
                                    .collect();
                                self.notes.push(format!(
                                    "/prompt {}:{} {} — {}",
                                    p["server"].as_str().unwrap_or_default(),
                                    p["name"].as_str().unwrap_or_default(),
                                    args.join(" "),
                                    p["description"].as_str().unwrap_or_default()
                                ));
                            }
                        }
                        Pending::GetMcpPrompt => {
                            self.input = result["text"].as_str().unwrap_or_default().to_string();
                            self.notes
                                .push("prompt loaded into the input; edit it, then Enter".into());
                        }
                        Pending::GetSubagent => {
                            let lines: Vec<String> = result["messages"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(subagent_line)
                                .collect();
                            self.notes.extend(lines);
                        }
                        Pending::RunControl => match result["abandoned"].as_bool() {
                            Some(true) => self.notes.push("abandoned the interrupted run".into()),
                            Some(false) => self.notes.push("nothing to abandon".into()),
                            None => {}
                        },
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
                                    // Edits arrive as reloads: drop renders of old content.
                                    self.md_cache.borrow_mut().clear();
                                    self.live_tools.clear();
                                    self.follow_chat_bottom();
                                }
                            }
                            self.run_banner = result["unfinished_run"].as_object().map(|r| {
                                (
                                    r["status"].as_str().unwrap_or_default().to_string(),
                                    r["error"].as_str().unwrap_or_default().to_string(),
                                )
                            });
                            let queued = result["queued"].as_array().map_or(0, Vec::len);
                            if queued > 0 {
                                self.notes.push(format!(
                                    "{queued} queued message(s) start after the current run"
                                ));
                            }
                        }
                    }
                }
                ServerMessage::Event(ev) => self.on_event(ev),
            }
        }
    }

    fn rollback_key(&mut self, key: KeyEvent) {
        let Some(pick) = self.rollback_pick.take() else {
            return;
        };
        let Some(d) = self
            .tool_detail
            .as_ref()
            .filter(|d| d.tool.id == pick.tool_id)
        else {
            return;
        };
        let (last, name, current) = (
            d.versions.len().saturating_sub(1),
            d.tool.name.clone(),
            d.tool.current_version,
        );
        let version = d.versions[pick.idx].version;
        let next = match key.code {
            KeyCode::Up => Some(RollbackPick {
                idx: pick.idx.saturating_sub(1),
                armed: false,
                ..pick
            }),
            KeyCode::Down => Some(RollbackPick {
                idx: (pick.idx + 1).min(last),
                armed: false,
                ..pick
            }),
            KeyCode::Enter if version == current => {
                self.notes.push(format!("v{version} is already current"));
                Some(pick)
            }
            KeyCode::Enter if pick.armed => {
                self.fire(Request::RollbackTool {
                    id: pick.tool_id,
                    version,
                });
                self.notes.push(format!("rolled {name} back to v{version}"));
                None
            }
            KeyCode::Enter => {
                self.notes.push(format!(
                    "press Enter again to roll {name} back to v{version}"
                ));
                Some(RollbackPick {
                    armed: true,
                    ..pick
                })
            }
            _ => {
                self.notes.push("rollback cancelled".into());
                None
            }
        };
        self.rollback_pick = next;
    }

    /// The current Session's newest running call of `tool_name`.
    fn running_call(&mut self, session_id: &str, tool_name: &str) -> Option<&mut LiveToolCall> {
        if !self.is_current(session_id) {
            return None;
        }
        self.live_tools
            .iter_mut()
            .rev()
            .find(|c| c.name == tool_name && c.status == "running")
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

    fn is_current(&self, session_id: &str) -> bool {
        self.current.as_deref() == Some(session_id)
    }

    /// Whether the session on screen has a Run in flight.
    fn running(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|id| self.running_sessions.contains(id))
    }

    /// Make `id` the session on screen, dropping the previous one's live
    /// Run state so it cannot bleed into this one.
    fn switch_session(&mut self, id: String) {
        // Live traffic only for the Session on screen (lists stay global).
        self.fire(Request::Subscribe {
            sessions: Some(vec![id.clone()]),
            deltas: true,
        });
        self.current = Some(id);
        self.streaming.clear();
        self.thinking.clear();
        self.live_tools.clear();
        self.editing_id = None;
        self.activity = None;
        if self.running() {
            self.set_activity("working");
        }
        self.follow_chat_bottom();
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::MessageAppended {
                session_id,
                message,
            } => {
                if self.current.as_deref() == Some(&session_id) {
                    // A persisted assistant turn (e.g. text before a Tool
                    // call) replaces the live stream; the next step starts
                    // fresh instead of appending to it.
                    if message.role == "assistant" {
                        self.streaming.clear();
                        self.thinking.clear();
                    }
                    if !self.messages.iter().any(|m| m.id == message.id) {
                        self.messages.push(message);
                    }
                }
            }
            Event::MessageDelta {
                session_id, delta, ..
            } => {
                if !self.is_current(&session_id) {
                    return;
                }
                self.set_activity("responding");
                self.streaming.push_str(&delta);
            }
            Event::ProviderStalled {
                session_id,
                idle_secs,
                ..
            } => {
                if self.is_current(&session_id) {
                    self.set_activity("provider stalled");
                    self.notes.push(format!(
                        "no data from the provider for {idle_secs}s; /resend re-sends the request"
                    ));
                }
            }
            Event::StreamRestarted { session_id, .. } => {
                if self.is_current(&session_id) {
                    self.streaming.clear();
                    self.thinking.clear();
                    self.notes.push("provider stream dropped; retrying".into());
                }
            }
            Event::ReasoningDelta {
                session_id, delta, ..
            } => {
                if !self.is_current(&session_id) {
                    return;
                }
                self.set_activity("thinking");
                self.thinking.push_str(&delta);
            }
            Event::MessageComplete {
                session_id,
                message,
                ..
            } => {
                if !self.is_current(&session_id) {
                    return;
                }
                self.streaming.clear();
                self.thinking.clear();
                if !self.messages.iter().any(|m| m.id == message.id) {
                    self.messages.push(message);
                }
            }
            Event::RunStarted { session_id, .. } => {
                self.running_sessions.insert(session_id.clone());
                if !self.is_current(&session_id) {
                    return;
                }
                self.run_banner = None;
                self.thinking.clear();
                self.streaming.clear();
                self.live_tools.clear();
                self.set_activity("waiting for model");
            }
            Event::RunFinished {
                session_id, status, ..
            } => {
                self.running_sessions.remove(&session_id);
                // Refresh names/order even when the Run was elsewhere.
                self.request(Pending::ListSessions, Request::ListSessions);
                if !self.is_current(&session_id) {
                    return;
                }
                self.activity = None;
                self.streaming.clear();
                self.thinking.clear();
                // Nothing can still be running once the run is over.
                self.live_tools.clear();
                if status != "completed" {
                    self.notes.push(format!("run {status}"));
                }
            }
            Event::RunPaused {
                session_id, error, ..
            } => {
                self.running_sessions.remove(&session_id);
                if !self.is_current(&session_id) {
                    return;
                }
                self.activity = None;
                self.notes.push(format!("run paused: {error}"));
                self.run_banner = Some(("paused".into(), error));
            }
            Event::CompactionNotice {
                summary_preview, ..
            } => {
                self.notes.push(format!("compacted: {summary_preview}"));
            }
            Event::Note { text, .. } => self.notes.push(text),
            Event::McpServersChanged
            | Event::SkillsChanged
            | Event::MemoryUpdated
            | Event::JobsChanged => {}
            Event::SettingsChanged => {
                if let Ok(cfg) = Config::load(&self.paths) {
                    self.model_label = cfg.provider.model;
                }
            }
            Event::ToolCallStarted {
                session_id,
                tool_name,
                version,
                arguments,
                depth,
                ..
            } => {
                if !self.is_current(&session_id) {
                    return;
                }
                self.set_activity(format!("running {tool_name}"));
                self.live_tools.push(LiveToolCall {
                    name: tool_name.clone(),
                    depth,
                    subagent: None,
                    version,
                    arguments,
                    status: "running".into(),
                    started: Instant::now(),
                    elapsed_ms: None,
                    handle: None,
                    stalled_secs: None,
                });
                self.notes.push(format!("running {tool_name}…"));
            }
            Event::ToolCallFinished {
                session_id,
                tool_name,
                ok,
                elapsed_ms,
                ..
            } => {
                if !self.is_current(&session_id) {
                    return;
                }
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
                if self.running() {
                    self.set_activity("waiting for model");
                }
                self.notes
                    .push(format!("{tool_name}: {}", if ok { "ok" } else { "failed" }));
            }
            Event::UserQuestion {
                question_id,
                session_id,
                question,
                options,
                mut questions,
            } => {
                // An older daemon sends only the single-question form.
                if questions.is_empty() {
                    questions.push(Question::single(question, options));
                }
                let open = OpenQuestion {
                    id: question_id,
                    session_id,
                    questions,
                    answers: Vec::new(),
                };
                self.notes.push(open.prompt());
                if self.is_current(&open.session_id) {
                    self.set_activity("awaiting your answer");
                }
                self.questions.push(open);
            }
            Event::RunUsage {
                session_id, usage, ..
            } => {
                self.run_usage.insert(session_id, usage);
            }
            Event::UserQuestionClosed { question_id } => {
                self.questions.retain(|q| q.id != question_id);
            }
            Event::ApprovalRequested {
                session_id,
                approval_id,
                tool_name,
                version,
                capabilities,
                memory_mb,
                detail,
                already_allowed,
                code_preview,
            } => {
                // Approvals stay global: a backgrounded Run still needs one.
                if self.is_current(&session_id) {
                    self.set_activity("awaiting your approval");
                }
                self.approvals.push_back(ApprovalReq {
                    approval_id,
                    tool_name,
                    version,
                    capabilities,
                    memory_mb,
                    detail,
                    already_allowed,
                    code_preview,
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
            Event::ProposalUpdated { proposal } => {
                self.notes.push(format!(
                    "proposal [{}] {}: {}",
                    proposal.kind, proposal.status, proposal.content
                ));
                if let Some(existing) = self.proposals.iter_mut().find(|x| x.id == proposal.id) {
                    *existing = proposal;
                } else {
                    self.proposals.insert(0, proposal);
                }
            }
            Event::ModuleUpdated { module } => {
                if let Some(existing) = self.modules.iter_mut().find(|x| x.id == module.id) {
                    *existing = module;
                } else {
                    self.modules.push(module);
                }
                self.request(Pending::ListModules, Request::ListModules);
            }
            Event::ToolDeleted { id } => self.tools.retain(|x| x.id != id),
            Event::OptimiserActivity { tool_name, text } => {
                if tool_name == "prompt"
                    && ["measured", "no new proposal", "failed"]
                        .iter()
                        .any(|w| text.contains(w))
                {
                    self.proposal_generating = false;
                }
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
                    detail: None,
                    already_allowed: Vec::new(),
                    code_preview: None,
                });
            }
            Event::InvocationPending {
                session_id,
                tool_name,
                handle,
                ..
            } => {
                if let Some(call) = self.running_call(&session_id, &tool_name) {
                    call.handle = Some(handle);
                }
                self.notes
                    .push(format!("{tool_name} still running (pending)"));
            }
            Event::InvocationStalled {
                session_id,
                handle,
                tool_name,
                silent_secs,
            } => {
                if let Some(call) = self.running_call(&session_id, &tool_name) {
                    call.handle = Some(handle);
                    call.stalled_secs = Some(silent_secs);
                    self.notes.push(format!(
                        "{tool_name}: no activity for {silent_secs}s; /cancel stops just this tool"
                    ));
                }
            }
            Event::SubagentStarted {
                session_id,
                subagent,
            } => {
                if self.is_current(&session_id) {
                    self.notes.push(format!(
                        "sub-agent started: {}",
                        truncate_chars(&subagent.task, 80)
                    ));
                    // Under the Tool that called agents.run (the latest
                    // running node).
                    let depth = self
                        .live_tools
                        .iter()
                        .rev()
                        .find(|c| c.status == "running")
                        .map_or(0, |c| c.depth + 1);
                    self.live_tools.push(LiveToolCall {
                        name: format!(
                            "sub-agent{}: {}",
                            if subagent.model.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", subagent.model)
                            },
                            truncate_chars(&subagent.task, 60)
                        ),
                        depth,
                        subagent: Some(subagent.id.clone()),
                        version: 0,
                        arguments: String::new(),
                        status: "running".into(),
                        started: Instant::now(),
                        elapsed_ms: None,
                        handle: None,
                        stalled_secs: None,
                    });
                }
            }
            Event::SubagentStep {
                session_id,
                subagent_id,
                step,
                tool_calls,
            } => {
                if self.is_current(&session_id) {
                    if let Some(node) = self
                        .live_tools
                        .iter_mut()
                        .find(|c| c.subagent.as_deref() == Some(&subagent_id))
                    {
                        node.arguments = if tool_calls.is_empty() {
                            format!("step {step}: answering")
                        } else {
                            format!("step {step}: {}", tool_calls.join(", "))
                        };
                    }
                    self.set_activity(if tool_calls.is_empty() {
                        format!("sub-agent step {step}: answering")
                    } else {
                        format!("sub-agent step {step}: {}", tool_calls.join(", "))
                    });
                }
            }
            Event::SubagentFinished {
                session_id,
                subagent,
            } => {
                if self.is_current(&session_id) {
                    self.notes.push(format!(
                        "sub-agent {} after {} step(s) · /agents to inspect",
                        subagent.status, subagent.steps
                    ));
                    if let Some(node) = self
                        .live_tools
                        .iter_mut()
                        .find(|c| c.subagent.as_deref() == Some(&subagent.id))
                    {
                        node.status = if subagent.status == "completed" {
                            "ok".into()
                        } else {
                            "failed".into()
                        };
                        node.elapsed_ms = Some(node.started.elapsed().as_millis() as i64);
                    }
                }
            }
            Event::InvocationActivity { handle, .. } => {
                for call in &mut self.live_tools {
                    if call.handle.as_deref() == Some(&handle) {
                        call.stalled_secs = None;
                    }
                }
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
                self.running_sessions.remove(&id);
                if self.is_current(&id) {
                    self.current = None;
                    self.messages.clear();
                    self.streaming.clear();
                    self.thinking.clear();
                    self.live_tools.clear();
                    self.activity = None;
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
        // An open ask_user question takes the next line as its answer.
        if let Some(i) = self
            .questions
            .iter()
            .position(|q| q.session_id == session_id)
        {
            let open = &mut self.questions[i];
            let answer = open.questions[open.answers.len()].parse_reply(&text);
            self.notes.push(format!("answered: {}", answer.summary()));
            open.answers.push(answer);
            if open.answers.len() < open.questions.len() {
                let next = open.prompt();
                self.notes.push(next);
                return;
            }
            let open = self.questions.remove(i);
            self.fire(Request::AnswerQuestion {
                question_id: open.id,
                answer: QuestionAnswer::summary_of(&open.answers),
                answers: open.answers,
            });
            return;
        }
        let mode = if queued {
            SendMode::Queue
        } else if self.running() {
            SendMode::Steer
        } else {
            SendMode::Send
        };
        self.follow_chat_bottom();
        // `@path` mentions are attached first: the daemon handles requests
        // concurrently, so the message waits for them.
        let root = std::env::current_dir().ok();
        let found: Vec<String> = formless_core::files::mentions(&text)
            .into_iter()
            .filter(|p| root.as_ref().is_some_and(|r| r.join(p).exists()))
            .collect();
        if found.is_empty() {
            self.fire(Request::SendMessage {
                session_id,
                content: text,
                mode,
            });
            return;
        }
        let root = root.map(|r| r.to_string_lossy().into_owned());
        for path in &found {
            self.request(
                Pending::MentionAttach,
                Request::AttachPath {
                    session_id: session_id.clone(),
                    path: path.clone(),
                    root: root.clone(),
                },
            );
        }
        self.pending_send = Some(PendingSend {
            session_id,
            content: text,
            mode,
            waiting: found.len(),
        });
    }

    /// One `@` attachment answered; send the message once all have.
    fn mention_attached(&mut self) {
        let Some(p) = &mut self.pending_send else {
            return;
        };
        p.waiting = p.waiting.saturating_sub(1);
        if p.waiting == 0 {
            let p = self.pending_send.take().unwrap();
            self.fire(Request::SendMessage {
                session_id: p.session_id,
                content: p.content,
                mode: p.mode,
            });
        }
    }

    /// The `/` palette rows for the input, empty when it isn't shown.
    fn palette_items(&self) -> Vec<PaletteItem> {
        let Some(q) = self.input.strip_prefix('/') else {
            return Vec::new();
        };
        if self.palette_hidden.as_deref() == Some(self.input.as_str())
            || self.search_mode
            || self.renaming
            || self.import_mode.is_some()
        {
            return Vec::new();
        }
        // An argument being typed: complete Tool names for /revoke.
        if let Some(arg) = q.strip_prefix("revoke ") {
            if arg.contains(' ') {
                return Vec::new();
            }
            let mut tools: Vec<_> = self
                .tools
                .iter()
                .filter(|t| !t.retired)
                .filter_map(|t| {
                    formless_core::fuzzy::fuzzy_match(arg, &t.name).map(|(s, _)| (s, t))
                })
                .collect();
            tools.sort_by_key(|t| std::cmp::Reverse(t.0));
            return tools
                .into_iter()
                .take(50)
                .map(|(_, t)| PaletteItem {
                    insert: format!("/revoke {}", t.name),
                    label: t.name.clone(),
                    desc: t.capabilities.join(", "),
                    runnable: true,
                })
                .collect();
        }
        // Past a complete command and into its arguments: nothing to show.
        let typed = q.trim_end();
        if q.contains(' ') && !commands::COMMANDS.iter().any(|c| c.name.starts_with(typed)) {
            return Vec::new();
        }
        commands::matches(q)
            .into_iter()
            .map(|c| PaletteItem {
                insert: if c.args.is_empty() {
                    format!("/{}", c.name)
                } else {
                    format!("/{} ", c.name)
                },
                label: c.usage(),
                desc: c.desc.to_string(),
                runnable: !c.needs_args(),
            })
            .collect()
    }

    /// Keys for the `/` palette; true when the key was used.
    fn palette_key(&mut self, key: &KeyEvent) -> bool {
        let items = self.palette_items();
        if items.is_empty() {
            return false;
        }
        let sel = self.palette_sel.min(items.len() - 1);
        match key.code {
            KeyCode::Up => self.palette_sel = sel.saturating_sub(1),
            KeyCode::Down => self.palette_sel = (sel + 1).min(items.len() - 1),
            KeyCode::Esc => self.palette_hidden = Some(self.input.clone()),
            KeyCode::Tab if key.modifiers.is_empty() => {
                self.input = items[sel].insert.clone();
                self.palette_sel = 0;
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let item = &items[sel];
                if item.runnable {
                    self.input = item.insert.trim_end().to_string();
                    self.palette_sel = 0;
                    self.submit_chat(false);
                } else {
                    self.notes.push(format!("usage: {}", item.label));
                    self.input = item.insert.clone();
                    self.palette_sel = 0;
                }
            }
            _ => return false,
        }
        true
    }

    /// Open, refresh or close the `@` popup for the input's last word.
    fn update_mention(&mut self) {
        let word = self.input.rsplit(char::is_whitespace).next().unwrap_or("");
        let query = match word.strip_prefix('@') {
            Some(q) if !self.input.starts_with('/') => q.to_string(),
            _ => {
                self.mention = None;
                return;
            }
        };
        if self.mention.as_ref().is_some_and(|m| m.query == query) {
            return;
        }
        self.mention = Some(MentionPopup {
            query: query.clone(),
            files: self.mention.take().map(|m| m.files).unwrap_or_default(),
            sel: 0,
            dismissed: false,
        });
        let root = std::env::current_dir()
            .ok()
            .map(|r| r.to_string_lossy().into_owned());
        self.request(
            Pending::ListFiles,
            Request::ListFiles {
                root,
                query,
                limit: Some(8),
            },
        );
    }

    /// Keys for an open `@` popup; true when the key was used.
    fn mention_key(&mut self, key: &KeyEvent) -> bool {
        let Some(m) = self
            .mention
            .as_mut()
            .filter(|m| !m.dismissed && !m.files.is_empty())
        else {
            return false;
        };
        match key.code {
            KeyCode::Up => m.sel = m.sel.saturating_sub(1),
            KeyCode::Down => m.sel = (m.sel + 1).min(m.files.len() - 1),
            KeyCode::Esc => m.dismissed = true,
            KeyCode::Tab | KeyCode::Enter if key.modifiers.is_empty() => {
                let f = m.files[m.sel.min(m.files.len() - 1)].clone();
                let word_len = self
                    .input
                    .rsplit(char::is_whitespace)
                    .next()
                    .unwrap_or("")
                    .len();
                self.input.truncate(self.input.len() - word_len);
                self.input.push('@');
                self.input.push_str(&f.path);
                self.input.push(if f.dir { '/' } else { ' ' });
                self.update_mention();
            }
            _ => return false,
        }
        true
    }

    /// Switch view and fetch what it shows.
    fn open_view(&mut self, view: View) {
        self.view = view;
        match view {
            View::Sessions => self.request(Pending::ListSessions, Request::ListSessions),
            View::Tools => {
                self.request(Pending::ListTools, Request::ListTools);
                self.request(Pending::ListModules, Request::ListModules);
            }
            View::Metrics => {
                self.request(Pending::ListTools, Request::ListTools);
                self.request(Pending::ListModelStats, Request::ListModelStats);
                self.request(
                    Pending::RunMetrics,
                    Request::RunMetrics { limit: Some(200) },
                );
                self.request(Pending::ListBuiltinStats, Request::ListBuiltinStats);
            }
            View::Proposals => self.request(Pending::ListProposals, Request::ListProposals),
            View::Chat | View::Settings | View::Search => {}
        }
    }

    fn cycle_view(&mut self, forward: bool) {
        let n = VIEW_ORDER.len();
        let next = match VIEW_ORDER.iter().position(|v| *v == self.view) {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        };
        self.open_view(VIEW_ORDER[next]);
    }

    fn slash(&mut self, cmd: &str) {
        match cmd {
            "new" => self.request(
                Pending::CreateSession,
                Request::CreateSession { name: None },
            ),
            "sessions" => self.open_view(View::Sessions),
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
            "metrics" => self.open_view(View::Metrics),
            "proposals" => self.open_view(View::Proposals),
            "propose" => self.generate_proposal(),
            "tools" => self.open_view(View::Tools),
            "settings" => self.open_view(View::Settings),
            "model" => self.open_model_picker(),
            cmd if cmd.starts_with("model ") => {
                let model = cmd["model ".len()..].trim().to_string();
                self.set_session_model(model, None);
            }
            "stop" => {
                if let Some(id) = self.current.clone() {
                    self.fire(Request::InterruptRun { session_id: id });
                }
            }
            "approve" | "approvals" => {
                // The dialog opens by itself while any approval waits; this
                // lists them (and says so when none do).
                if self.approvals.is_empty() && self.promotions.is_empty() {
                    self.notes.push(
                        "no approvals waiting; sensitive Tools ask here when they are called"
                            .into(),
                    );
                }
                let waiting: Vec<String> = self
                    .approvals
                    .iter()
                    .map(|a| format!("approval: {} ({})", a.tool_name, a.capabilities.join(", ")))
                    .chain(
                        self.promotions
                            .iter()
                            .map(|p| format!("promotion: {}", p.tool_name)),
                    )
                    .collect();
                for w in waiting {
                    self.notes.push(format!(
                        "{w} · answer in the dialog: y always, o once, n deny"
                    ));
                }
            }
            "panes" => {
                self.multi_pane = !self.multi_pane;
                // Settings go through the daemon (principle 6).
                self.fire(Request::SetSetting {
                    key: "tui.multi_pane".into(),
                    value: serde_json::json!(self.multi_pane),
                });
                self.notes.push(format!(
                    "multi-pane layout {} (remembered)",
                    if self.multi_pane { "on" } else { "off" }
                ));
            }
            "thinking" => {
                self.show_thinking = !self.show_thinking;
                self.notes.push(format!(
                    "thinking display {}",
                    if self.show_thinking { "on" } else { "off" }
                ));
            }
            "mcp" => self.request(Pending::ListMcpServers, Request::ListMcpServers),
            "prompts" => self.request(Pending::ListMcpPrompts, Request::ListMcpPrompts),
            "skills" => self.request(Pending::ListSkills, Request::ListSkills),
            "memory" => self.request(Pending::ListMemories, Request::ListMemories),
            "jobs" => self.request(Pending::ListJobs, Request::ListJobs),
            "roles" | "providers" => self.request(Pending::ListProviders, Request::ListProviders),
            "harness" => self.request(Pending::HarnessChanges, Request::ListHarnessChanges),
            cmd if cmd.starts_with("harness undo") => {
                let id = cmd["harness undo".len()..].trim();
                self.request(
                    Pending::RunControl,
                    Request::UndoHarnessChange {
                        id: (!id.is_empty()).then(|| id.to_string()),
                    },
                );
                self.notes.push("undoing the harness change…".into());
            }
            cmd if cmd.starts_with("role add ") => {
                let mut w = cmd["role add ".len()..].split_whitespace();
                let (name, provider, model) = (w.next(), w.next(), w.next());
                let description = w.collect::<Vec<_>>().join(" ");
                match (name, provider, model) {
                    (Some(name), Some(provider), Some(model)) if !description.is_empty() => {
                        self.fire(Request::UpsertRole {
                            name: name.to_string(),
                            role: serde_json::json!({
                                "provider": provider,
                                "model": if model == "-" { "" } else { model },
                                "description": description,
                            }),
                        });
                        self.notes.push(format!("role {name} saved (new Sessions see it)"));
                    }
                    _ => self.notes.push(
                        "usage: /role add NAME PROVIDER MODEL description… (MODEL - = provider default)"
                            .into(),
                    ),
                }
            }
            cmd if cmd.starts_with("role rm ") => {
                let name = cmd["role rm ".len()..].trim().to_string();
                self.fire(Request::DeleteRole { name: name.clone() });
                self.notes.push(format!("deleted role {name}"));
            }
            "copy" => self.copy_cmd(""),
            cmd if cmd.starts_with("copy ") => self.copy_cmd(cmd[5..].trim()),
            "mouse" => {
                self.mouse_capture = !self.mouse_capture;
                let mut out = stdout();
                if self.mouse_capture {
                    let _ = out.execute(crossterm::event::EnableMouseCapture);
                } else {
                    let _ = out.execute(crossterm::event::DisableMouseCapture);
                }
                self.notes.push(if self.mouse_capture {
                    "mouse on: the wheel scrolls the chat (/mouse to select text instead)".into()
                } else {
                    "mouse off: drag to select and copy text with your terminal (/mouse for wheel scrolling)".into()
                });
            }
            "open" => self.open_artifact(""),
            cmd if cmd.starts_with("open ") => self.open_artifact(cmd[5..].trim()),
            "undo" => match self.last_deleted.take() {
                Some((id, name)) => {
                    self.fire(Request::RestoreSession { id });
                    self.request(Pending::ListSessions, Request::ListSessions);
                    self.notes.push(format!("restoring session {name}"));
                }
                None => self.notes.push("nothing to undo".into()),
            },
            "invocations" | "pending" => {
                self.request(Pending::ListInvocations, Request::ListInvocations)
            }
            cmd if cmd.starts_with("kill ") || cmd.starts_with("drop ") => {
                let prefix = cmd[5..].trim().trim_end_matches('…');
                let found: Vec<String> = self
                    .invocations
                    .iter()
                    .filter(|i| !prefix.is_empty() && i.handle.starts_with(prefix))
                    .map(|i| i.handle.clone())
                    .collect();
                match found.as_slice() {
                    [handle] => {
                        let handle = handle.clone();
                        if cmd.starts_with("kill ") {
                            self.request(Pending::RunControl, Request::CancelInvocation { handle });
                        } else {
                            self.request(
                                Pending::RunControl,
                                Request::AbandonInvocation { handle },
                            );
                        }
                    }
                    [] => self
                        .notes
                        .push("no such invocation (list them with /invocations)".into()),
                    _ => self.notes.push("ambiguous handle; type more of it".into()),
                }
            }
            "usage" => match self.current.clone() {
                Some(session_id) => {
                    self.request(Pending::SessionUsage, Request::SessionUsage { session_id })
                }
                None => self.notes.push("open a session first".into()),
            },
            "audit" => self.request(
                Pending::ListAudit,
                Request::ListAudit {
                    filter: None,
                    limit: Some(50),
                    before: None,
                },
            ),
            cmd if cmd.starts_with("audit ") => self.request(
                Pending::ListAudit,
                Request::ListAudit {
                    filter: Some(cmd["audit ".len()..].trim().to_string()),
                    limit: Some(50),
                    before: None,
                },
            ),
            cmd if cmd.starts_with("revoke ") => {
                let mut words = cmd.split_whitespace().skip(1);
                let name = words.next().unwrap_or_default();
                let capability = words.next().map(str::to_string);
                match self.tools.iter().find(|t| t.name == name) {
                    Some(t) => {
                        self.fire(Request::RevokeApproval {
                            tool_id: t.id.clone(),
                            capability: capability.clone(),
                        });
                        self.notes.push(format!(
                            "revoked {} for {name}; the next call asks again",
                            capability.as_deref().unwrap_or("every approval")
                        ));
                    }
                    None => self
                        .notes
                        .push(format!("no tool '{name}' (open /tools first)")),
                }
            }
            cmd if cmd.starts_with("attach ") => {
                let path = cmd["attach ".len()..].trim().trim_matches(['"', '\'']);
                let path = match path.strip_prefix("~/") {
                    Some(rest) => std::env::var("HOME")
                        .map(|h| format!("{h}/{rest}"))
                        .unwrap_or_else(|_| path.to_string()),
                    None => path.to_string(),
                };
                let Some(session_id) = self.current.clone() else {
                    self.notes.push("open a session first".into());
                    return;
                };
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        use base64::Engine;
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "attachment".into());
                        self.notes.push(format!(
                            "attached {name} ({} KB); it goes with your next message",
                            bytes.len() / 1024
                        ));
                        self.fire(Request::AttachFile {
                            session_id,
                            name,
                            data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
                            mime: None,
                        });
                    }
                    Err(e) => self.notes.push(format!("cannot attach {path}: {e}")),
                }
            }
            cmd if cmd.starts_with("jobs ") => {
                let mut words = cmd.split_whitespace().skip(1);
                let (verb, id) = (
                    words.next().unwrap_or_default(),
                    words.next().unwrap_or_default().to_string(),
                );
                let req = match verb {
                    "enable" | "approve" => Some(Request::SetJobEnabled {
                        id: id.clone(),
                        enabled: true,
                    }),
                    "disable" => Some(Request::SetJobEnabled {
                        id: id.clone(),
                        enabled: false,
                    }),
                    "rm" | "delete" => Some(Request::DeleteJob { id: id.clone() }),
                    _ => None,
                };
                match req {
                    Some(req) if !id.is_empty() => {
                        self.fire(req);
                        self.notes.push(format!("job {verb} {id}"));
                    }
                    _ => self
                        .notes
                        .push("usage: /jobs [enable|disable|rm ID]".into()),
                }
            }
            cmd if cmd.starts_with("memory ") => {
                let mut words = cmd.split_whitespace().skip(1);
                let (verb, id) = (
                    words.next().unwrap_or_default(),
                    words.next().unwrap_or_default().to_string(),
                );
                let rest = words.collect::<Vec<_>>().join(" ");
                let req = match verb {
                    "rm" | "delete" => Some(Request::DeleteMemory { id: id.clone() }),
                    "on" | "off" => Some(Request::UpsertMemory {
                        id: Some(id.clone()),
                        text: None,
                        scope: None,
                        enabled: Some(verb == "on"),
                    }),
                    "edit" if !rest.is_empty() => Some(Request::UpsertMemory {
                        id: Some(id.clone()),
                        text: Some(rest),
                        scope: None,
                        enabled: None,
                    }),
                    _ => None,
                };
                match req {
                    Some(req) if !id.is_empty() => {
                        self.fire(req);
                        self.notes.push(format!("memory {verb} {id}"));
                    }
                    _ => self
                        .notes
                        .push("usage: /memory [rm|on|off ID | edit ID TEXT]".into()),
                }
            }
            cmd if cmd.starts_with("skills enable ") || cmd.starts_with("skills disable ") => {
                let enabled = cmd.starts_with("skills enable ");
                let name = cmd
                    .split_whitespace()
                    .nth(2)
                    .unwrap_or_default()
                    .to_string();
                self.fire(Request::SetSkillEnabled {
                    name: name.clone(),
                    enabled,
                });
                self.notes.push(format!(
                    "{} skill {name}",
                    if enabled { "enabling" } else { "disabling" }
                ));
            }
            cmd if cmd.starts_with("prompt ") => {
                let mut words = cmd["prompt ".len()..].split_whitespace();
                let target = words.next().unwrap_or_default();
                match target.split_once(':') {
                    Some((server, name)) => {
                        let arguments = words
                            .filter_map(|w| w.split_once('='))
                            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                            .collect();
                        self.request(
                            Pending::GetMcpPrompt,
                            Request::GetMcpPrompt {
                                server: server.into(),
                                name: name.into(),
                                arguments,
                            },
                        );
                    }
                    None => self
                        .notes
                        .push("usage: /prompt SERVER:NAME [key=value …] (see /prompts)".into()),
                }
            }
            cmd if cmd.starts_with("mcp restart ") => {
                let name = cmd["mcp restart ".len()..].trim().to_string();
                self.fire(Request::RestartMcpServer { name: name.clone() });
                self.notes.push(format!("restarting MCP server {name}…"));
            }
            "agents" => {
                if let Some(session_id) = self.current.clone() {
                    self.request(
                        Pending::ListSubagents,
                        Request::ListSubagents { session_id },
                    );
                }
            }
            cmd if cmd.starts_with("agents ") => {
                let picked = cmd[7..]
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|i| self.subagents.get(i))
                    .map(|a| a.id.clone());
                match picked {
                    Some(id) => self.request(Pending::GetSubagent, Request::GetSubagent { id }),
                    None => self
                        .notes
                        .push("usage: /agents to list, then /agents <n>".into()),
                }
            }
            "cancel" => {
                let handle = self
                    .live_tools
                    .iter()
                    .rev()
                    .filter(|c| c.status == "running")
                    .find_map(|c| c.handle.clone());
                match handle {
                    Some(handle) => {
                        self.request(Pending::RunControl, Request::CancelInvocation { handle })
                    }
                    None => self.notes.push(
                        "no pending or stalled tool to cancel (Esc stops the whole run)".into(),
                    ),
                }
            }
            "resend" => {
                if let Some(session_id) = self.current.clone() {
                    self.request(
                        Pending::RunControl,
                        Request::RetryProviderRequest { session_id },
                    );
                }
            }
            "resume" | "abandon" => {
                if let Some(session_id) = self.current.clone() {
                    let req = if cmd == "resume" {
                        Request::ResumeRun { session_id }
                    } else {
                        Request::AbandonRun { session_id }
                    };
                    self.request(Pending::RunControl, req);
                    self.run_banner = None;
                }
            }
            "help" => {
                self.pager = Some(Pager {
                    title: "Help".into(),
                    lines: commands::help_lines(),
                    top: 0,
                })
            }
            "quit" => self.should_quit = true,
            other => self.notes.push(match commands::closest(other) {
                Some(name) => format!("unknown command: /{other} — did you mean /{name}?"),
                None => format!("unknown command: /{other} (/help lists them)"),
            }),
        }
    }

    fn open_model_picker(&mut self) {
        let effort = self
            .current
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
            .and_then(|s| s.reasoning_effort.clone());
        let effort_idx = EFFORTS
            .iter()
            .position(|e| e.map(str::to_string) == effort)
            .unwrap_or(0);
        self.model_overlay = Some(ModelOverlay {
            picker: picker::ModelPicker::loading(),
            effort_idx,
        });
        self.request(Pending::ListModels, Request::ListModels);
    }

    fn set_session_model(&mut self, model: String, effort: Option<String>) {
        match self.current.clone() {
            Some(id) => self.request(
                Pending::SetModel,
                Request::SetSessionModel {
                    id,
                    model: Some(model),
                    reasoning_effort: effort,
                },
            ),
            None => self.notes.push("no current session".into()),
        }
    }

    fn model_key(&mut self, key: KeyEvent) {
        let Some(o) = &mut self.model_overlay else {
            return;
        };
        let global = key.code == KeyCode::Char('d')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            || key.code == KeyCode::Enter
                && key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if global || key.code == KeyCode::Enter {
            let Some(model) = o.picker.choice() else {
                return;
            };
            let effort = EFFORTS[o.effort_idx].map(str::to_string);
            self.model_overlay = None;
            if global {
                // Settings go through the daemon (principle 6).
                self.fire(Request::SetSetting {
                    key: "provider.model".into(),
                    value: serde_json::json!(model),
                });
                self.fire(Request::SetSetting {
                    key: "provider.reasoning_effort".into(),
                    value: serde_json::json!(effort),
                });
                self.notes.push(format!(
                    "default model: {model} (effort {})",
                    effort.as_deref().unwrap_or("default")
                ));
            } else {
                self.set_session_model(model, effort);
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.model_overlay = None,
            KeyCode::Up => o.picker.up(),
            KeyCode::Down => o.picker.down(),
            KeyCode::Tab => o.effort_idx = (o.effort_idx + 1) % EFFORTS.len(),
            KeyCode::BackTab => o.effort_idx = (o.effort_idx + EFFORTS.len() - 1) % EFFORTS.len(),
            KeyCode::Backspace => o.picker.backspace(),
            KeyCode::Char(c) => o.picker.type_char(c),
            _ => {}
        }
    }

    fn wizard_poll(&mut self) {
        let mut finish = false;
        if let Some(w) = &mut self.wizard {
            while let Ok(job) = w.jobs.try_recv() {
                match job {
                    WizardJob::Models(Ok(models)) if !models.is_empty() => {
                        let items = models
                            .into_iter()
                            .map(|id| picker::ModelItem {
                                recent: id == w.saved_model,
                                id,
                                source: None,
                            })
                            .collect();
                        w.picker.set_items(items, &w.saved_model.clone());
                        w.step = WizardStep::PickModel;
                        w.status = "Pick a model: type to filter, Up/Down, Enter.".into();
                    }
                    WizardJob::Models(_) => {
                        w.picker = picker::ModelPicker::new(Vec::new());
                        w.picker.query = w.saved_model.clone();
                        w.step = WizardStep::PickModel;
                        w.status = "Could not list models; type a model name, then Enter.".into();
                    }
                    WizardJob::Probe(Ok(r)) if r.tool_calling => {
                        w.prompt_fallback = r.prompt_fallback;
                        finish = true;
                    }
                    WizardJob::Probe(Ok(_)) => {
                        w.step = WizardStep::Failed;
                        w.status = "This endpoint/model supports neither native nor prompt-based tool-calling. Press Enter to pick another model.".into();
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
        if w.prompt_fallback {
            cfg.provider.tool_mode = "prompt".into();
        }
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
            WizardStep::PickModel => match key.code {
                KeyCode::Up => w.picker.up(),
                KeyCode::Down => w.picker.down(),
                KeyCode::Enter if !w.model().trim().is_empty() => {
                    w.step = WizardStep::PickEffort;
                    w.status = "Pick reasoning effort.".into();
                }
                KeyCode::Char(c) => w.picker.type_char(c),
                KeyCode::Backspace => w.picker.backspace(),
                _ => {}
            },
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

    fn editor_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(ed) = &self.editor {
                self.request(
                    Pending::UpdateTool,
                    Request::UpdateToolCode {
                        id: ed.tool_id.clone(),
                        code: ed.text(),
                    },
                );
                self.notes.push("saving edited Tool code...".into());
            }
            return;
        }
        let Some(ed) = &mut self.editor else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.editor = None,
            KeyCode::Enter => {
                ed.chars.insert(ed.cursor, '\n');
                ed.cursor += 1;
            }
            KeyCode::Tab => {
                ed.chars.insert(ed.cursor, ' ');
                ed.chars.insert(ed.cursor + 1, ' ');
                ed.cursor += 2;
            }
            KeyCode::Backspace => {
                if ed.cursor > 0 {
                    ed.cursor -= 1;
                    ed.chars.remove(ed.cursor);
                }
            }
            KeyCode::Delete => {
                if ed.cursor < ed.chars.len() {
                    ed.chars.remove(ed.cursor);
                }
            }
            KeyCode::Left => ed.cursor = ed.cursor.saturating_sub(1),
            KeyCode::Right => ed.cursor = (ed.cursor + 1).min(ed.chars.len()),
            KeyCode::Up => ed.move_vertical(false),
            KeyCode::Down => ed.move_vertical(true),
            KeyCode::Home => {
                while ed.cursor > 0 && ed.chars[ed.cursor - 1] != '\n' {
                    ed.cursor -= 1;
                }
            }
            KeyCode::End => {
                while ed.cursor < ed.chars.len() && ed.chars[ed.cursor] != '\n' {
                    ed.cursor += 1;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                ed.chars.insert(ed.cursor, c);
                ed.cursor += 1;
            }
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.ctrl_c();
            return;
        }
        if let Some(p) = &mut self.pager {
            let page = 20;
            let last = p.lines.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.pager = None,
                KeyCode::Down | KeyCode::Char('j') => p.top = (p.top + 1).min(last),
                KeyCode::Up | KeyCode::Char('k') => p.top = p.top.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => p.top = (p.top + page).min(last),
                KeyCode::PageUp => p.top = p.top.saturating_sub(page),
                KeyCode::Home | KeyCode::Char('g') => p.top = 0,
                KeyCode::End | KeyCode::Char('G') => p.top = last,
                _ => {}
            }
            return;
        }
        if self.wizard.is_some() {
            self.wizard_key(key);
            return;
        }
        if self.editor.is_some() {
            self.editor_key(key);
            return;
        }
        if self.rollback_pick.is_some() {
            self.rollback_key(key);
            return;
        }
        if self.model_overlay.is_some() {
            self.model_key(key);
            return;
        }
        if let KeyCode::Char(c) = key.code {
            if c == 'o' && self.input.is_empty() && !self.approvals.is_empty() {
                let req = self.approvals.pop_front().unwrap();
                self.fire(Request::ApprovalDecision {
                    approval_id: req.approval_id,
                    approved: true,
                    scope: ApprovalScope::Once,
                });
                self.notes.push(format!("approved {} once", req.tool_name));
                return;
            }
            if (c == 'y' || c == 'n') && self.input.is_empty() {
                if !self.approvals.is_empty() {
                    let req = self.approvals.pop_front().unwrap();
                    self.fire(Request::ApprovalDecision {
                        approval_id: req.approval_id,
                        approved: c == 'y',
                        scope: ApprovalScope::Always,
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
        if self.view == View::Chat && (self.palette_key(&key) || self.mention_key(&key)) {
            return;
        }
        if self.view == View::Chat
            && key.code == KeyCode::Char('?')
            && self.input.is_empty()
            && self.approvals.is_empty()
        {
            self.slash("help");
            return;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.cycle_view(key.code == KeyCode::Tab);
            return;
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
                        self.switch_session(hit.session_id.clone());
                        self.request(
                            Pending::GetSession,
                            Request::GetSession { id: hit.session_id },
                        );
                        self.view = View::Chat;
                    }
                }
                KeyCode::Esc => self.view = View::Chat,
                _ => {}
            },
            View::Proposals => match key.code {
                KeyCode::Up => self.proposal_idx = self.proposal_idx.saturating_sub(1),
                KeyCode::Down => {
                    self.proposal_idx =
                        (self.proposal_idx + 1).min(self.proposals.len().saturating_sub(1))
                }
                KeyCode::Char('g') => self.generate_proposal(),
                KeyCode::Char('a') | KeyCode::Enter => {
                    if let Some(p) = self.proposals.get(self.proposal_idx).cloned() {
                        self.fire(Request::ProposalDecision {
                            id: p.id,
                            approved: true,
                        });
                    }
                }
                KeyCode::Char('r') => {
                    if let Some(p) = self.proposals.get(self.proposal_idx).cloned() {
                        self.fire(Request::ProposalDecision {
                            id: p.id,
                            approved: false,
                        });
                    }
                }
                KeyCode::Char('b') => {
                    if let Some(p) = self.proposals.get(self.proposal_idx).cloned() {
                        self.fire(Request::RollbackProposal { id: p.id });
                    }
                }
                KeyCode::Esc => self.view = View::Chat,
                _ => {}
            },
            View::Sessions
                if key.code != KeyCode::Char('d') && self.delete_arm.take().is_some() =>
            {
                self.notes.push("delete cancelled".into());
            }
            View::Sessions => match key.code {
                KeyCode::Up => self.session_idx = self.session_idx.saturating_sub(1),
                KeyCode::Down => {
                    self.session_idx =
                        (self.session_idx + 1).min(self.sessions.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(s) = self.sessions.get(self.session_idx).cloned() {
                        self.switch_session(s.id.clone());
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
                        if self.delete_arm.as_deref() == Some(&s.id) {
                            self.delete_arm = None;
                            self.notes.push(format!(
                                "deleted session {} · /undo to restore it (30s)",
                                s.name
                            ));
                            self.last_deleted = Some((s.id.clone(), s.name.clone()));
                            self.fire(Request::DeleteSession { id: s.id });
                            self.request(Pending::ListSessions, Request::ListSessions);
                        } else {
                            self.notes.push(format!(
                                "press d again to DELETE session {} and its transcript",
                                s.name
                            ));
                            self.delete_arm = Some(s.id);
                        }
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
                KeyCode::Esc => self.view = View::Chat,
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
                KeyCode::Char('E') => {
                    if let Some(tool) = self.tools.get(self.tool_idx).cloned() {
                        match &self.tool_detail {
                            Some(d) if d.tool.id == tool.id => {
                                let code = d
                                    .versions
                                    .iter()
                                    .find(|v| v.version == tool.current_version)
                                    .or_else(|| d.versions.last())
                                    .map(|v| v.code.clone())
                                    .unwrap_or_default();
                                let chars: Vec<char> = code.chars().collect();
                                let cursor = chars.len();
                                self.editor = Some(EditorState {
                                    tool_id: tool.id.clone(),
                                    name: tool.name.clone(),
                                    chars,
                                    cursor,
                                });
                            }
                            _ => {
                                self.request(Pending::GetTool, Request::GetTool { id: tool.id });
                                self.notes
                                    .push("loading Tool detail; press E again to edit".into());
                            }
                        }
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
                        match &self.tool_detail {
                            Some(d) if d.tool.id == tool.id && d.versions.len() > 1 => {
                                // Start on the version before the current one.
                                let idx = d
                                    .versions
                                    .iter()
                                    .position(|v| v.version == tool.current_version)
                                    .unwrap_or(d.versions.len() - 1)
                                    .saturating_sub(1);
                                self.rollback_pick = Some(RollbackPick {
                                    tool_id: tool.id,
                                    idx,
                                    armed: false,
                                });
                            }
                            Some(d) if d.tool.id == tool.id => self
                                .notes
                                .push(format!("{} has only one version", tool.name)),
                            _ => {
                                self.request(Pending::GetTool, Request::GetTool { id: tool.id });
                                self.notes
                                    .push("loading versions… press b again to pick one".into());
                            }
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
                _ if is_queue_key(&key) => self.submit_chat(true),
                KeyCode::Enter => self.submit_chat(false),
                KeyCode::Esc => {
                    if self.running() {
                        if let Some(id) = self.current.clone() {
                            self.fire(Request::InterruptRun { session_id: id });
                        }
                    } else {
                        self.view = View::Chat;
                    }
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    self.palette_sel = 0;
                    self.update_mention();
                }
                KeyCode::Char(c) => {
                    self.input.push(c);
                    self.palette_sel = 0;
                    self.update_mention();
                }
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
    if let Some(ed) = &app.editor {
        draw_editor(f, ed, app);
        return;
    }
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(f.area());
    draw_tab_bar(f, chunks[0], app);
    match app.view {
        View::Chat if pane_layout(app.multi_pane, chunks[1].width) => draw_panes(f, chunks[1], app),
        View::Chat => draw_chat(f, chunks[1], app),
        View::Sessions => draw_sessions(f, chunks[1], app),
        View::Settings => draw_settings(f, chunks[1], app),
        View::Tools => draw_tools(f, chunks[1], app),
        View::Metrics => draw_metrics(f, chunks[1], app),
        View::Proposals => draw_proposals(f, chunks[1], app),
        View::Search => draw_search(f, chunks[1], app),
    }
    let modal = app
        .approvals
        .front()
        .map(|r| (r, false))
        .or(app.promotions.front().map(|r| (r, true)));
    let typing = matches!(app.view, View::Chat | View::Settings);
    draw_input(f, chunks[2], app, typing && modal.is_none());
    draw_status(f, chunks[3], app);
    if app.view == View::Chat {
        let palette = app.palette_items();
        if !palette.is_empty() {
            draw_palette(f, chunks[2], &palette, app.palette_sel, app.color);
        } else if let Some(m) = app.mention.as_ref().filter(|m| !m.dismissed) {
            draw_mention_popup(f, chunks[2], m, app.color);
        }
    }
    if let Some((req, promotion)) = modal {
        let more = app.approvals.len() + app.promotions.len() - 1;
        draw_approval(f, req, promotion, more, app.color);
    } else if let Some(pick) = &app.rollback_pick {
        draw_rollback_picker(f, app, pick);
    } else if let Some(o) = &app.model_overlay {
        draw_model_overlay(f, o, app.color);
    }
    if let Some(p) = &app.pager {
        let area = f.area();
        let height = area.height.saturating_sub(2) as usize;
        let body: Vec<Line> = p
            .lines
            .iter()
            .skip(p.top)
            .take(height)
            .map(|l| Line::raw(l.clone()))
            .collect();
        let title = format!(
            " {} · line {}/{} · ↑↓ PgUp PgDn g G · Esc close ",
            p.title,
            (p.top + 1).min(p.lines.len().max(1)),
            p.lines.len()
        );
        f.render_widget(ratatui::widgets::Clear, area);
        f.render_widget(
            Paragraph::new(body).block(ui::panel(title, app.color, Color::Cyan)),
            area,
        );
    }
}

fn draw_rollback_picker(f: &mut Frame, app: &App, pick: &RollbackPick) {
    let Some(d) = app
        .tool_detail
        .as_ref()
        .filter(|d| d.tool.id == pick.tool_id)
    else {
        return;
    };
    let mut lines = vec![
        Line::raw(""),
        Line::styled(
            "  VER  AUTHOR      CREATED            CALLS  SUCCESS",
            ui::dim(app.color),
        ),
    ];
    for (i, v) in d.versions.iter().enumerate() {
        let m = d
            .version_metrics
            .iter()
            .find(|(n, _)| *n == v.version)
            .map(|(_, m)| m);
        let current = if v.version == d.tool.current_version {
            " (current)"
        } else {
            ""
        };
        let text = format!(
            "{} v{:<3} {:<11} {:<18} {:>5}  {}{current}",
            if i == pick.idx { "›" } else { " " },
            v.version,
            truncate_chars(&v.author, 11),
            ui::clock_time(&v.created_at).unwrap_or_else(|| truncate_chars(&v.created_at, 18)),
            m.map_or(0, |m| m.total),
            ui::rate_text(m.and_then(|m| m.success_rate)),
        );
        let style = if i == pick.idx {
            bold_fg(app.color, Color::Yellow)
        } else {
            Style::default()
        };
        lines.push(Line::styled(text, style));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        if pick.armed {
            "  Enter again to confirm · Esc cancel"
        } else {
            "  ↑/↓ choose · Enter roll back · Esc cancel"
        },
        ui::dim(app.color),
    ));
    let area = f.area();
    let w = 64.min(area.width.saturating_sub(2)).max(1);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(ui::panel(
            format!(" Roll back {} ", d.tool.name),
            app.color,
            Color::Yellow,
        )),
        rect,
    );
}

/// The model list with the filter line, matched chars highlighted and
/// the selection kept on screen.
fn picker_lines(p: &picker::ModelPicker, max_items: usize, color: bool) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![
        Span::styled("Filter   ", ui::dim(color)),
        Span::raw(format!("{}▏", p.query)),
    ])];
    if p.loading {
        out.push(Line::styled("  loading models…", ui::dim(color)));
        return out;
    }
    if let Some(e) = &p.error {
        out.push(Line::styled(
            format!("  could not list models: {}", truncate_chars(e, 70)),
            fg(color, Color::Red),
        ));
    }
    let matches = p.matches();
    if matches.is_empty() {
        let hint = if p.query.trim().is_empty() {
            "  type a model id".to_string()
        } else {
            format!(
                "  no match · Enter uses \"{}\" as the model id",
                p.query.trim()
            )
        };
        out.push(Line::styled(hint, ui::dim(color)));
        return out;
    }
    let sel = p.sel.min(matches.len() - 1);
    let start = sel
        .saturating_sub(max_items / 2)
        .min(matches.len().saturating_sub(max_items));
    let sources = p.multi_source();
    for (i, (item, pos)) in matches.iter().enumerate().skip(start).take(max_items) {
        let selected = i == sel;
        let base = if selected {
            bold_fg(color, Color::Green)
        } else {
            Style::default()
        };
        let hit = base.add_modifier(Modifier::UNDERLINED).fg(if color {
            Color::Yellow
        } else {
            Color::Reset
        });
        let mut spans = vec![Span::styled(if selected { "› " } else { "  " }, base)];
        for (n, ch) in item.id.chars().enumerate() {
            spans.push(Span::styled(
                ch.to_string(),
                if pos.contains(&n) { hit } else { base },
            ));
        }
        if item.recent {
            spans.push(Span::styled("  recent", ui::dim(color)));
        }
        if sources {
            if let Some(src) = &item.source {
                spans.push(Span::styled(format!("  {src}"), ui::dim(color)));
            }
        }
        out.push(Line::from(spans));
    }
    if matches.len() > max_items || matches.len() < p.items.len() {
        out.push(Line::styled(
            format!("  ({}/{} of {})", sel + 1, matches.len(), p.items.len()),
            ui::dim(color),
        ));
    }
    out
}

/// A user-message line with `@path` mentions highlighted.
fn mention_line(text: &str, color: bool) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    for word in text.split_inclusive(char::is_whitespace) {
        let bare = word.trim_end();
        if bare.len() > 1 && bare.starts_with('@') {
            spans.push(Span::styled(
                bare.to_string(),
                fg(color, Color::Cyan).add_modifier(Modifier::UNDERLINED),
            ));
            spans.push(Span::raw(word[bare.len()..].to_string()));
        } else {
            spans.push(Span::raw(word.to_string()));
        }
    }
    Line::from(spans)
}

/// The `/` palette, just above the input box, scrolled to the selection.
fn draw_palette(f: &mut Frame, input: Rect, items: &[PaletteItem], sel: usize, color: bool) {
    if input.y < 3 {
        return;
    }
    let sel = sel.min(items.len() - 1);
    let rows = (input.y as usize).saturating_sub(2).clamp(1, 10);
    let start = sel
        .saturating_sub(rows - 1)
        .min(items.len().saturating_sub(rows));
    let label_w = items
        .iter()
        .map(|i| i.label.chars().count())
        .max()
        .unwrap_or(0)
        .min(40);
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(i, item)| {
            let style = if i == sel {
                bold_fg(color, Color::Green)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    format!(
                        "{} {}  ",
                        if i == sel { "›" } else { " " },
                        ui::pad(&truncate_chars(&item.label, 40), label_w)
                    ),
                    style,
                ),
                Span::styled(item.desc.clone(), ui::dim(color)),
            ])
        })
        .collect();
    let h = (lines.len() as u16 + 2).min(input.y);
    let rect = Rect {
        x: input.x,
        y: input.y - h,
        width: input.width.clamp(1, 100),
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(ui::panel(
            format!(
                " / commands · {} · Tab complete · Enter run · Esc hide ",
                items.len()
            ),
            color,
            Color::Cyan,
        )),
        rect,
    );
}

/// The `@` completions, just above the input box.
fn draw_mention_popup(f: &mut Frame, input: Rect, m: &MentionPopup, color: bool) {
    if m.files.is_empty() || input.y < 3 {
        return;
    }
    let lines: Vec<Line> = m
        .files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let text = format!(
                "{} {}{}",
                if i == m.sel { "›" } else { " " },
                file.path,
                if file.dir { "/" } else { "" }
            );
            if i == m.sel {
                Line::styled(text, bold_fg(color, Color::Green))
            } else {
                Line::raw(text)
            }
        })
        .collect();
    let h = (lines.len() as u16 + 2).min(input.y);
    let w = 72.min(input.width).max(1);
    let rect = Rect {
        x: input.x,
        y: input.y - h,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(ui::panel(
            " @ files · Tab/Enter insert · Esc hide ",
            color,
            Color::Cyan,
        )),
        rect,
    );
}

fn draw_model_overlay(f: &mut Frame, o: &ModelOverlay, color: bool) {
    let area = f.area();
    let max_items = (area.height as usize).saturating_sub(10).clamp(3, 20);
    let mut lines = picker_lines(&o.picker, max_items, color);
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("Effort   ", ui::dim(color)),
        Span::raw(EFFORTS[o.effort_idx].unwrap_or("default (provider)")),
        Span::styled("  (Tab to change)", ui::dim(color)),
    ]));
    lines.push(Line::styled(
        "Enter this Session · Ctrl+Enter/Alt+Enter/Ctrl+D global default · Esc cancel",
        ui::dim(color),
    ));
    let w = 84.min(area.width.saturating_sub(2)).max(1);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 3,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(ui::panel(" Model ", color, Color::Cyan)),
        rect,
    );
}

fn draw_tab_bar(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        " ◇ Formless ",
        bold_fg(app.color, Color::Cyan),
    )];
    let mut views: Vec<View> = VIEW_ORDER.to_vec();
    if app.view == View::Search {
        views.push(View::Search);
    }
    for v in views {
        let label = format!(" {} ", v.label());
        if v == app.view {
            spans.push(Span::styled(
                label,
                bold_fg(app.color, Color::Cyan).add_modifier(Modifier::REVERSED),
            ));
        } else {
            spans.push(Span::styled(label, ui::dim(app.color)));
        }
    }
    let mut used: usize = spans.iter().map(|s| s.width()).sum();
    if used > area.width as usize {
        // Narrow terminal: the view names matter more than the brand.
        used -= spans.remove(0).width();
    }
    let tip = " Tab ⇥ ";
    if used + tip.len() < area.width as usize {
        spans.push(Span::raw(
            " ".repeat(area.width as usize - used - tip.chars().count()),
        ));
        spans.push(Span::styled(tip, ui::dim(app.color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_input(f: &mut Frame, area: Rect, app: &App, show_cursor: bool) {
    let (prompt, color) = if app.renaming {
        ("rename › ", Color::Magenta)
    } else if app.search_mode {
        ("search › ", Color::Magenta)
    } else if app.import_mode.is_some() {
        ("import path › ", Color::Magenta)
    } else if app.editing_id.is_some() {
        ("edit › ", Color::Magenta)
    } else if app.running() {
        ("steer › ", Color::Yellow)
    } else {
        ("› ", Color::Cyan)
    };
    let inner = area.width.saturating_sub(2) as usize;
    let avail = inner.saturating_sub(prompt.chars().count() + 1).max(1);
    // Show the tail of long input so the cursor end stays visible.
    let chars: Vec<char> = app.input.chars().collect();
    let shown: String = chars[chars.len().saturating_sub(avail)..].iter().collect();
    let mut spans = vec![Span::styled(prompt, bold_fg(app.color, color))];
    if shown.is_empty() && !app.running() && prompt == "› " && show_cursor {
        spans.push(Span::styled(
            "Ask Formless to do something…",
            ui::dim(app.color),
        ));
    } else {
        spans.push(Span::raw(shown.clone()));
    }
    let mut block = ui::panel(" Message ", app.color, color);
    if area.width > 64 {
        block = block.title(ui::hint(
            &if app.running() {
                format!("Enter steer · {} queue · Esc stop", app.queue_label())
            } else {
                format!("Enter send · {} queue · /help", app.queue_label())
            },
            app.color,
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    if show_cursor {
        let x = area.x + 1 + Span::raw(prompt).width() as u16 + Span::raw(shown).width() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y + 1));
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let sep = || Span::styled(" │ ", ui::dim(app.color));
    let mut left: Vec<Span> = vec![Span::raw(" ")];
    if let Some(activity) = app.activity_text() {
        left.push(Span::styled(activity, bold_fg(app.color, Color::Yellow)));
    } else if app.running() {
        left.push(Span::styled("● running", bold_fg(app.color, Color::Yellow)));
    } else {
        left.push(Span::styled("● ready", fg(app.color, Color::Green)));
    }
    left.push(sep());
    left.push(Span::styled(
        app.sessions
            .iter()
            .find(|s| Some(&s.id) == app.current.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "no session".into()),
        bold_fg(app.color, Color::Cyan),
    ));
    if !app.model_label.is_empty() {
        left.push(sep());
        left.push(Span::styled(app.model_label.clone(), ui::dim(app.color)));
    }
    if let Some(u) = app
        .current
        .as_ref()
        .and_then(|s| app.run_usage.get(s))
        .filter(|u| u.steps > 0)
    {
        left.push(sep());
        left.push(Span::styled(usage_text(u), ui::dim(app.color)));
    }
    let waiting = app.approvals.len() + app.promotions.len();
    if waiting > 0 {
        left.push(sep());
        left.push(Span::styled(
            format!("⚠ {waiting} awaiting approval"),
            bold_fg(app.color, Color::Red),
        ));
    }
    let width = area.width as usize;
    let used: usize = left.iter().map(|s| s.width()).sum();
    let note = app.notes.last().cloned().unwrap_or_default();
    let lower = note.to_lowercase();
    let note_style = if [
        "paused",
        "failed",
        "denied",
        "error",
        "could not",
        "unknown",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        bold_fg(app.color, Color::Red)
    } else {
        fg(app.color, Color::Gray)
    };
    let room = width.saturating_sub(used + 2);
    let note = truncate_chars(&note, room.saturating_sub(1));
    let mut spans = left;
    if note.is_empty() && used + 40 < width {
        let tip = "Tab views · /help · Ctrl+C ×2 detach ";
        spans.push(Span::raw(" ".repeat(width - used - tip.chars().count())));
        spans.push(Span::styled(tip, ui::dim(app.color)));
    } else if !note.is_empty() {
        let pad = width.saturating_sub(used + Span::raw(note.as_str()).width() + 1);
        spans.push(Span::raw(" ".repeat(pad.max(2))));
        spans.push(Span::styled(note, note_style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Speaker groups: tool results belong to the assistant turn around them.
fn speaker(role: &str) -> &'static str {
    match role {
        "user" => "user",
        "system-note" => "system-note",
        _ => "assistant",
    }
}

fn chat_header(app: &App, role: &str, when: Option<&str>) -> Line<'static> {
    let who = match role {
        "user" => "You",
        "system-note" => "System",
        _ => "Formless",
    };
    let mut spans = vec![
        Span::styled("▍", role_style(app.color, role)),
        Span::styled(who.to_string(), role_style(app.color, role)),
    ];
    if let Some(t) = when.and_then(ui::clock_time) {
        spans.push(Span::styled(format!("  {t}"), ui::dim(app.color)));
    }
    Line::from(spans)
}

/// Keys that queue a message (SPEC §10.5): Ctrl+Enter where the terminal
/// can tell it from Enter, and everywhere Ctrl+J (a raw line feed) or
/// Alt+Enter.
fn is_queue_key(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT),
        KeyCode::Char('j') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

/// Rows the live thinking block always takes (SPEC §10.6).
const THINKING_ROWS: usize = 4;

/// The last `rows` display rows of `text` wrapped at `width` columns
/// (wide characters count double), padded with empty rows: always exactly
/// `rows` rows, each fitting the width.
fn thinking_rows(text: &str, width: usize, rows: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1);
    let mut wrapped: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut row = String::new();
        let mut used = 0;
        for c in line.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > width {
                wrapped.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(c);
            used += w;
        }
        wrapped.push(row);
    }
    let mut out: Vec<String> = wrapped[wrapped.len().saturating_sub(rows)..].to_vec();
    while out.len() < rows {
        out.push(String::new());
    }
    out
}

/// How soon a second Ctrl+C must follow the first to detach.
const CTRL_C_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Fenced code blocks in Markdown text, in order (fence lines dropped).
fn code_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match current.take() {
                Some(lines) => blocks.push(lines.join("\n")),
                None => current = Some(Vec::new()),
            }
        } else if let Some(lines) = current.as_mut() {
            lines.push(line);
        }
    }
    blocks
}

/// OSC 52: the terminal puts `text` on the clipboard (works over SSH;
/// inside tmux it needs passthrough, `set -g allow-passthrough on`).
fn osc52(text: &str) -> String {
    use base64::Engine;
    let seq = format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text)
    );
    if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    }
}

impl App {
    /// `g` / `/propose`: ask for one new measured proposal.
    fn generate_proposal(&mut self) {
        if self.proposal_generating {
            self.notes
                .push("a proposal is already being generated".into());
            return;
        }
        self.request(Pending::GenerateProposal, Request::GenerateProposal);
    }

    /// `/copy` (last answer) or `/copy code [n]` (its n-th code block,
    /// default the last): to the clipboard via OSC 52, and the OS
    /// clipboard too when this is a local terminal.
    fn copy_cmd(&mut self, arg: &str) {
        let Some(last) = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
        else {
            self.notes.push("nothing to copy yet".into());
            return;
        };
        let (text, what) = if let Some(rest) = arg.strip_prefix("code") {
            let blocks = code_blocks(&last.content);
            let n = rest.trim().parse::<usize>().unwrap_or(blocks.len());
            match blocks.get(n.wrapping_sub(1)) {
                Some(b) => (b.clone(), format!("code block {n} of {}", blocks.len())),
                None => {
                    self.notes.push(format!(
                        "the last answer has {} code block(s)",
                        blocks.len()
                    ));
                    return;
                }
            }
        } else {
            (last.content.clone(), "the last answer".to_string())
        };
        use std::io::Write;
        let mut out = stdout();
        let _ = out.write_all(osc52(&text).as_bytes());
        let _ = out.flush();
        if std::env::var_os("SSH_CONNECTION").is_none() {
            let _ = formless_runtime::clipboard_write(&text);
        }
        self.notes
            .push(format!("copied {what} ({} chars)", text.chars().count()));
    }

    /// The queue binding to advertise: one that works in this terminal.
    fn queue_label(&self) -> &'static str {
        if self.enhanced_keys {
            "Ctrl+Enter"
        } else {
            "Ctrl+J"
        }
    }

    /// Ctrl+C: clear a typed message first; pressed again within
    /// [`CTRL_C_WINDOW`] it detaches (Runs keep going in the daemon). The
    /// setup wizard still quits at once.
    fn ctrl_c(&mut self) {
        if self.wizard.is_some() {
            self.should_quit = true;
            return;
        }
        let armed = self.ctrl_c_at.is_some_and(|t| t.elapsed() < CTRL_C_WINDOW);
        if armed && self.input.is_empty() {
            self.should_quit = true;
            return;
        }
        if !self.input.is_empty() {
            self.input.clear();
            self.editing_id = None;
            self.notes
                .push("message cleared · Ctrl+C again to detach".into());
        } else {
            self.notes.push("press Ctrl+C again to detach".into());
        }
        self.ctrl_c_at = Some(Instant::now());
    }

    /// `/open [id-prefix]`: view a stored output in the pager. Without an
    /// id, the newest one referenced in this Session.
    fn open_artifact(&mut self, prefix: &str) {
        let refs: Vec<String> = self
            .messages
            .iter()
            .rev()
            .flat_map(|m| formless_protocol::artifact_refs(&m.content))
            .collect();
        let id = if prefix.is_empty() {
            refs.first().cloned()
        } else if prefix.len() == 36 {
            Some(prefix.to_string())
        } else {
            refs.into_iter().find(|r| r.starts_with(prefix))
        };
        match id {
            Some(id) => self.request(
                Pending::GetArtifact,
                Request::GetArtifact {
                    id,
                    offset: None,
                    length: None,
                },
            ),
            None => self
                .notes
                .push("no stored output to open in this session".into()),
        }
    }
}

fn tool_result_line(content: &str, color: bool) -> Line<'static> {
    let n = content.lines().count();
    let failed = content.contains("\"ok\":false")
        || content.trim_start().to_lowercase().starts_with("error");
    let mut spans = vec![
        Span::styled(
            "  ↳ ",
            if failed {
                bold_fg(color, Color::Red)
            } else {
                fg(color, Color::Yellow)
            },
        ),
        Span::styled(truncate_chars(content.trim(), 160), ui::dim(color)),
    ];
    if n > 1 {
        spans.push(Span::styled(format!("  ({n} lines)"), ui::dim(color)));
    }
    if let Some(id) = formless_protocol::artifact_refs(content).first() {
        spans.push(Span::styled(
            format!("  /open {}", &id[..8]),
            fg(color, Color::Cyan),
        ));
    }
    Line::from(spans)
}

fn welcome_lines(color: bool, height: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = (0..height.saturating_sub(10) / 2)
        .map(|_| Line::raw(""))
        .collect();
    lines.push(Line::styled("◇ Formless", bold_fg(color, Color::Cyan)).centered());
    lines.push(
        Line::styled(
            "an agent that builds, measures, and improves its own Tools",
            ui::dim(color),
        )
        .centered(),
    );
    lines.push(Line::raw(""));
    for example in [
        "“Fetch the title of https://example.com”",
        "“Summarise the largest files in ~/Downloads”",
        "“Watch a log file and flag new errors”",
    ] {
        lines.push(Line::styled(example, fg(color, Color::Gray)).centered());
    }
    lines.push(Line::raw(""));
    lines.push(
        Line::styled(
            "Tab switches views · /help lists commands · Ctrl+C clears, twice detaches",
            ui::dim(color),
        )
        .centered(),
    );
    lines
}

fn chat_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut group: Option<&'static str> = None;
    // Header only when the speaker changes; a blank line separates turns.
    let open_group = |lines: &mut Vec<Line<'static>>,
                      group: &mut Option<&'static str>,
                      role: &str,
                      when: Option<&str>| {
        let g = speaker(role);
        if *group != Some(g) {
            if group.is_some() {
                lines.push(Line::raw(""));
            }
            lines.push(chat_header(app, g, when));
            *group = Some(g);
        }
    };
    for m in &app.messages {
        let when = Some(m.created_at.as_str());
        match m.role.as_str() {
            "user" => {
                open_group(&mut lines, &mut group, "user", when);
                for l in m.content.lines() {
                    lines.push(mention_line(&truncate_chars(l, 2000), app.color));
                }
            }
            "tool" => {
                open_group(&mut lines, &mut group, "assistant", when);
                lines.push(tool_result_line(&m.content, app.color));
            }
            "system-note" => {
                open_group(&mut lines, &mut group, "system-note", when);
                for l in m.content.lines() {
                    lines.push(Line::styled(
                        format!("  {l}"),
                        ui::dim(app.color).add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            _ => {
                open_group(&mut lines, &mut group, "assistant", when);
                lines.extend(app.markdown_cached(m, width));
                if let Some(meta) = &m.meta {
                    for call in tool_call_lines(meta) {
                        lines.push(Line::styled(
                            format!("  {call}"),
                            fg(app.color, Color::Yellow),
                        ));
                    }
                }
            }
        }
    }
    // Live call tree (persisted lines above keep the history): running
    // calls with their nested calls and sub-agents; a finished, successful
    // subtree folds to one line.
    let shown = live_tree(&app.live_tools);
    if !shown.is_empty() {
        open_group(&mut lines, &mut group, "assistant", None);
        for (call, folded) in shown {
            let version = if call.version > 0 {
                format!(" v{}", call.version)
            } else {
                String::new()
            };
            let indent = "  ".repeat(call.depth);
            let branch = if call.depth > 0 { "└ " } else { "" };
            let (mark, style) = match call.status.as_str() {
                "running" => (app.spinner().to_string(), bold_fg(app.color, Color::Yellow)),
                "ok" => ("✓".to_string(), fg(app.color, Color::Green)),
                _ => ("✗".to_string(), fg(app.color, Color::Red)),
            };
            let elapsed = match call.elapsed_ms {
                Some(ms) => format!("{:.1}s", ms as f64 / 1000.0),
                None => format_elapsed(call.started.elapsed()),
            };
            let mut spans = vec![Span::styled(
                format!(
                    "  {indent}{branch}{mark} {}{} · {elapsed}{}  ",
                    call.name,
                    version,
                    if folded > 0 {
                        format!(" · +{folded} nested")
                    } else {
                        String::new()
                    }
                ),
                style,
            )];
            if let Some(secs) = call.stalled_secs {
                spans.push(Span::styled(
                    format!("stalled {secs}s · /cancel  "),
                    bold_fg(app.color, Color::Red),
                ));
            }
            spans.push(Span::styled(
                ui::summarize_args(&call.arguments, 160),
                bold_fg(app.color, Color::Yellow),
            ));
            lines.push(Line::from(spans));
        }
    }
    if app.messages.is_empty() && !app.running() {
        return welcome_lines(app.color, height);
    }
    // Live reasoning: dimmed, separate from the answer, and capped to the
    // latest lines so it never floods the transcript (/thinking hides it).
    if app.show_thinking && !app.thinking.is_empty() {
        if group.is_some() {
            lines.push(Line::raw(""));
        }
        // A fixed-height block (header + THINKING_ROWS rendered rows,
        // pre-wrapped so the paragraph never re-wraps them): nothing below
        // it moves while reasoning streams.
        let n = app.thinking.lines().count();
        lines.push(Line::styled(
            format!("▍Thinking · {n} line{}", if n == 1 { "" } else { "s" }),
            thinking_style(app.color),
        ));
        for row in thinking_rows(&app.thinking, width.saturating_sub(2), THINKING_ROWS) {
            lines.push(Line::styled(format!("  {row}"), thinking_style(app.color)));
        }
        group = None;
    }
    if let Some((status, reason)) = app.run_banner.as_ref().filter(|_| !app.running()) {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(
                "  ⏸ The last Run was {status}{}",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {}", truncate_chars(reason, 200))
                }
            ),
            bold_fg(app.color, Color::Yellow),
        ));
        lines.push(Line::styled(
            "    /resume continues it · /abandon closes it",
            ui::dim(app.color),
        ));
    }
    if !app.streaming.is_empty() || app.running() {
        open_group(&mut lines, &mut group, "assistant", None);
        lines.extend(app.streaming_markdown(width));
        if app.streaming.is_empty() {
            lines.push(Line::styled(
                format!("  {}", app.activity_text().unwrap_or_else(|| "…".into())),
                bold_fg(app.color, Color::Yellow),
            ));
        }
    }
    lines
}

fn draw_chat(f: &mut Frame, area: Rect, app: &mut App) {
    let height = (area.height.saturating_sub(2) as usize).max(1);
    let width = area.width.saturating_sub(2);
    let paragraph =
        Paragraph::new(chat_lines(app, width as usize, height)).wrap(Wrap { trim: false });
    // Scroll by rendered (wrapped) rows so the newest output stays visible.
    let rows = paragraph.line_count(width);
    app.chat_view_height = height;
    app.chat_line_count = rows;
    let max_top = rows.saturating_sub(height);
    let scroll_top = match app.chat_scroll_top {
        Some(top) => top.min(max_top),
        None => max_top,
    };
    if app.chat_scroll_top == Some(scroll_top) && scroll_top >= max_top {
        app.chat_scroll_top = None;
    }
    let mut block = ui::panel(" Chat ", app.color, Color::DarkGray);
    if app.chat_scroll_top.is_some() {
        block = block.title(
            Line::styled(
                format!(" ↑ {} more below · End to follow ", max_top - scroll_top),
                bold_fg(app.color, Color::Yellow),
            )
            .right_aligned(),
        );
    }
    f.render_widget(
        paragraph
            .block(block)
            .scroll((scroll_top.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn draw_sessions(f: &mut Frame, area: Rect, app: &App) {
    let now = chrono::Utc::now();
    let name_w = (area.width as usize).saturating_sub(20).max(10);
    let items: Vec<ListItem> = if app.sessions.is_empty() {
        vec![ListItem::new(Line::styled(
            "No sessions yet — press n to start one.",
            ui::dim(app.color),
        ))]
    } else {
        app.sessions
            .iter()
            .map(|s| {
                let current = Some(&s.id) == app.current.as_ref();
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if current { "● " } else { "  " },
                        fg(app.color, Color::Cyan),
                    ),
                    Span::raw(ui::pad(&s.name, name_w)),
                    Span::styled(ui::relative_time(&s.updated_at, now), ui::dim(app.color)),
                ]))
            })
            .collect()
    };
    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(app.session_idx.min(app.sessions.len() - 1)));
    }
    f.render_stateful_widget(
        List::new(items)
            .block(
                ui::panel(
                    format!(" Sessions ({}) ", app.sessions.len()),
                    app.color,
                    Color::Cyan,
                )
                .title_bottom(ui::hint(
                    "Enter open · n new · f fork · r rename · d delete · e export · i import",
                    app.color,
                )),
            )
            .highlight_style(bold_fg(app.color, Color::Cyan).add_modifier(Modifier::REVERSED))
            .highlight_symbol("› "),
        area,
        &mut state,
    );
}

fn draw_settings(f: &mut Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = match Config::load(&app.paths) {
        Ok(c) => {
            let section =
                |name: &str| Line::styled(name.to_string(), bold_fg(app.color, Color::Cyan));
            let row = |k: &str, v: String| {
                Line::from(vec![
                    Span::styled(format!("  {}", ui::pad(k, 20)), ui::dim(app.color)),
                    if v.is_empty() {
                        Span::styled("(not set)", ui::dim(app.color))
                    } else {
                        Span::raw(v)
                    },
                ])
            };
            let on = |b: bool| if b { "on" } else { "off" }.to_string();
            vec![
                section("Provider"),
                row("base_url", c.provider.base_url.clone()),
                row("model", c.provider.model.clone()),
                row(
                    "reasoning_effort",
                    c.provider
                        .reasoning_effort
                        .clone()
                        .unwrap_or_else(|| "default".into()),
                ),
                row("api_style", c.provider.api_style.clone()),
                row("tool_mode", c.provider.tool_mode.clone()),
                row("auth", c.provider.auth.mode.clone()),
                row("context_tokens", c.provider.context_tokens.to_string()),
                Line::raw(""),
                section("Interface"),
                row("multi_pane", on(c.tui.multi_pane)),
                row("queue_key", c.tui.queue_key.clone()),
                row("thinking", on(app.show_thinking)),
                Line::raw(""),
                section("Relay"),
                row("enabled", on(c.relay.enabled)),
                row("url", c.relay.url.clone()),
                Line::raw(""),
                section("Paths"),
                row("config", app.paths.config_file().display().to_string()),
                row("data", app.paths.data_dir.display().to_string()),
            ]
        }
        Err(e) => vec![Line::styled(e.to_string(), bold_fg(app.color, Color::Red))],
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(
                ui::panel(" Settings ", app.color, Color::Cyan).title_bottom(ui::hint(
                    "edit config.toml (hot-reloaded per Run) · delete it to re-run setup",
                    app.color,
                )),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_search(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = if app.search_hits.is_empty() {
        vec![ListItem::new(Line::styled(
            "No hits. /search to try again.",
            ui::dim(app.color),
        ))]
    } else {
        app.search_hits
            .iter()
            .map(|h| {
                ListItem::new(Line::from(vec![
                    Span::styled(h.session_name.clone(), bold_fg(app.color, Color::Cyan)),
                    Span::styled(format!("  #{} {}  ", h.seq, h.role), ui::dim(app.color)),
                    Span::raw(h.snippet.clone()),
                ]))
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
                ui::panel(
                    format!(" Search ({} hits) ", app.search_hits.len()),
                    app.color,
                    Color::Magenta,
                )
                .title_bottom(ui::hint("Enter open session · Esc back", app.color)),
            )
            .highlight_style(bold_fg(app.color, Color::Magenta).add_modifier(Modifier::REVERSED))
            .highlight_symbol("› "),
        area,
        &mut state,
    );
}

const NO_TOOLS: &str = "No Tools yet — the agent builds them via create_tool.";

fn metrics_summary(tools: &[ToolInfo]) -> String {
    let total_calls: i64 = tools.iter().map(|t| t.invocations).sum();
    let attention = tools.iter().filter(|t| t.needs_attention).count();
    let retired = tools.iter().filter(|t| t.retired).count();
    format!(
        "{} Tools · {total_calls} invocations · {retired} retired · {attention} need attention",
        tools.len()
    )
}

/// Status glyph for a Tool: retired, needs attention, or healthy.
fn tool_glyph(t: &ToolInfo, color: bool) -> Span<'static> {
    if t.retired {
        Span::styled("○ ", ui::dim(color))
    } else if t.needs_attention {
        Span::styled("! ", bold_fg(color, Color::Red))
    } else {
        Span::styled("● ", fg(color, Color::Green))
    }
}

/// Status label for a Tool, matching `tool_glyph`.
fn tool_status(t: &ToolInfo, color: bool) -> Span<'static> {
    if t.retired {
        Span::styled("retired", bold_fg(color, Color::DarkGray))
    } else if t.needs_attention {
        Span::styled("attention", bold_fg(color, Color::Red))
    } else {
        Span::styled("active", fg(color, Color::Green))
    }
}

/// Name, version, calls, and success cells shared by Tool tables.
fn tool_cells(t: &ToolInfo, color: bool) -> Vec<Cell<'static>> {
    vec![
        Cell::from(Line::from(vec![
            tool_glyph(t, color),
            Span::raw(t.name.clone()),
        ])),
        Cell::from(format!("v{}", t.current_version)),
        Cell::from(Line::raw(t.invocations.to_string()).right_aligned()),
        Cell::from(
            Line::styled(
                ui::rate_text(t.success_rate),
                ui::rate_style(color, t.success_rate),
            )
            .right_aligned(),
        ),
    ]
}

fn table_header(cols: &[&'static str], color: bool) -> Row<'static> {
    Row::new(cols.iter().map(|c| Cell::from(*c))).style(ui::dim(color).add_modifier(Modifier::BOLD))
}

fn draw_metrics(f: &mut Frame, area: Rect, app: &App) {
    let detail_h = if app.tool_detail.is_some() { 5 } else { 0 };
    let routing_h = if app.model_stats.is_empty() {
        0
    } else {
        app.model_stats.len() as u16 + 3
    };
    let builtins_h = if app.builtin_stats.is_empty() {
        0
    } else {
        app.builtin_stats.len().min(8) as u16 + 3
    };
    let parts = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(detail_h),
        Constraint::Length(routing_h),
        Constraint::Length(builtins_h),
    ])
    .split(area);
    if !app.builtin_stats.is_empty() {
        let mut stats: Vec<&formless_protocol::BuiltinStatInfo> =
            app.builtin_stats.iter().collect();
        stats.sort_by_key(|s| std::cmp::Reverse(s.calls));
        let rows: Vec<Row> = stats
            .iter()
            .take(8)
            .map(|s| {
                let rate = (s.calls > 0).then(|| (s.calls - s.failures) as f64 / s.calls as f64);
                Row::new(vec![
                    Cell::from(s.name.clone()),
                    Cell::from(Line::raw(s.calls.to_string()).right_aligned()),
                    Cell::from(
                        Line::styled(ui::rate_text(rate), ui::rate_style(app.color, rate))
                            .right_aligned(),
                    ),
                    Cell::from(
                        s.kinds
                            .iter()
                            .map(|(k, n)| format!("{k} {n}"))
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                ])
            })
            .collect();
        f.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(20),
                    Constraint::Length(7),
                    Constraint::Length(8),
                    Constraint::Min(10),
                ],
            )
            .header(table_header(
                &["BUILT-IN", "CALLS", "SUCCESS", "OUTCOMES"],
                app.color,
            ))
            .column_spacing(2)
            .block(ui::panel(" Built-ins ", app.color, Color::Green)),
            parts[3],
        );
    }
    let mut sorted: Vec<&ToolInfo> = app.tools.iter().collect();
    sorted.sort_by_key(|t| std::cmp::Reverse(t.invocations));
    let rows: Vec<Row> = sorted
        .iter()
        .map(|t| {
            let mut row_cells = tool_cells(t, app.color);
            row_cells.push(Cell::from(Span::styled(
                t.success_rate.map(|r| ui::bar(r, 20)).unwrap_or_default(),
                ui::rate_style(app.color, t.success_rate),
            )));
            Row::new(row_cells)
        })
        .collect();
    let title = match app.run_summary.as_ref().filter(|s| s.runs > 0) {
        Some(s) => format!(
            " Metrics · {} · {} Runs: {:.0}% completed, {:.0}% edited/corrected, {:.1} steps avg ",
            metrics_summary(&app.tools),
            s.runs,
            s.completed * 100.0,
            (s.edited + s.corrected) * 100.0,
            s.avg_steps
        ),
        None => format!(" Metrics · {} ", metrics_summary(&app.tools)),
    };
    let block = ui::panel(title, app.color, Color::Green);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(NO_TOOLS, ui::dim(app.color))).block(block),
            parts[0],
        );
    } else {
        f.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(28),
                    Constraint::Length(5),
                    Constraint::Length(7),
                    Constraint::Length(8),
                    Constraint::Min(10),
                ],
            )
            .header(table_header(
                &["TOOL", "VER", "CALLS", "SUCCESS", ""],
                app.color,
            ))
            .column_spacing(2)
            .block(block),
            parts[0],
        );
    }
    if let Some(d) = &app.tool_detail {
        let m = &d.metrics;
        let pct = |r: Option<f64>| {
            r.map(|r| format!("{:.1}%", r * 100.0))
                .unwrap_or_else(|| "-".into())
        };
        let ms = |v: Option<f64>| v.map(|v| format!("{v:.0}ms")).unwrap_or_else(|| "-".into());
        let lines = vec![
            Line::from(vec![
                Span::styled("ok ", ui::dim(app.color)),
                Span::styled(m.success.to_string(), fg(app.color, Color::Green)),
                Span::styled("  failed ", ui::dim(app.color)),
                Span::styled(m.failure.to_string(), fg(app.color, Color::Red)),
                Span::styled(
                    format!(
                        "  denied {}  cancelled {}  invalid {}  of {}",
                        m.denied, m.cancelled, m.validation_failures, m.total
                    ),
                    ui::dim(app.color),
                ),
            ]),
            Line::from(vec![
                Span::styled("rate ", ui::dim(app.color)),
                Span::styled(
                    pct(m.success_rate),
                    ui::rate_style(app.color, m.success_rate),
                ),
                Span::styled(
                    format!(
                        "  retry {}  p50 {}  p95 {}  sessions {}  per success {}",
                        pct(m.retry_rate),
                        ms(m.p50_ms),
                        ms(m.p95_ms),
                        m.sessions,
                        cost_per_success(m)
                    ),
                    ui::dim(app.color),
                ),
            ]),
        ];
        f.render_widget(
            Paragraph::new(lines).block(ui::panel(
                format!(" {} ", d.tool.name),
                app.color,
                Color::Yellow,
            )),
            parts[1],
        );
    }
    if !app.model_stats.is_empty() {
        let rows: Vec<Row> = app
            .model_stats
            .iter()
            .map(|st| {
                let n = st.successes + st.failures;
                let rate = (n > 0).then(|| st.successes as f64 / n as f64);
                Row::new(vec![
                    Cell::from(st.model.clone()),
                    Cell::from(Line::raw(n.to_string()).right_aligned()),
                    Cell::from(
                        Line::styled(ui::rate_text(rate), ui::rate_style(app.color, rate))
                            .right_aligned(),
                    ),
                    Cell::from(
                        Line::raw(if n > 0 {
                            format!("{:.0}ms", st.total_ms as f64 / n as f64)
                        } else {
                            "-".into()
                        })
                        .right_aligned(),
                    ),
                    Cell::from(Line::raw(cache_hit_text(st)).right_aligned()),
                ])
            })
            .collect();
        f.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Min(18),
                    Constraint::Length(7),
                    Constraint::Length(8),
                    Constraint::Length(9),
                    Constraint::Length(7),
                ],
            )
            .header(table_header(
                &["MODEL", "CALLS", "SUCCESS", "AVG", "CACHED"],
                app.color,
            ))
            .column_spacing(2)
            .block(ui::panel(
                " Model routing (measured) ",
                app.color,
                Color::Green,
            )),
            parts[2],
        );
    }
}

fn proposal_status_style(color: bool, status: &str) -> Style {
    match status {
        "applied" => bold_fg(color, Color::Green),
        "measured" => fg(color, Color::Yellow),
        "proposed" => fg(color, Color::Gray),
        _ => ui::dim(color),
    }
}

fn draw_proposals(f: &mut Frame, area: Rect, app: &App) {
    let title = if app.proposal_generating {
        format!(" Prompt proposals · {} generating… ", app.spinner())
    } else {
        " Prompt proposals ".to_string()
    };
    let block = ui::panel(title, app.color, Color::Blue).title_bottom(ui::hint(
        "g generate a new one (model call + evals, costs tokens) · a apply measured · r reject · b roll back",
        app.color,
    ));
    if app.proposals.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                "No proposals yet. Press g (or /propose) to generate a measured one.",
                ui::dim(app.color),
            ))
            .block(block),
            area,
        );
        return;
    }
    let rows: Vec<Row> = app
        .proposals
        .iter()
        .map(|p| {
            let scores = match (p.baseline_score, p.candidate_score) {
                (Some(b), Some(c)) => Line::from(vec![
                    Span::styled(
                        format!("{:.0}%", c * 100.0),
                        if c > b {
                            fg(app.color, Color::Green)
                        } else {
                            fg(app.color, Color::Red)
                        },
                    ),
                    Span::styled(
                        format!(" vs {:.0}% ({})", b * 100.0, p.eval_total),
                        ui::dim(app.color),
                    ),
                ]),
                _ => Line::styled("unmeasured", ui::dim(app.color)),
            };
            Row::new(vec![
                Cell::from(p.kind.clone()),
                Cell::from(Span::styled(
                    p.status.clone(),
                    proposal_status_style(app.color, &p.status),
                )),
                Cell::from(scores),
                Cell::from(Line::from(vec![
                    Span::raw(truncate_chars(&p.content, 90)),
                    Span::styled(format!("  — {}", p.rationale), ui::dim(app.color)),
                ])),
            ])
        })
        .collect();
    let mut state = TableState::default();
    state.select(Some(app.proposal_idx.min(app.proposals.len() - 1)));
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(7),
                Constraint::Length(12),
                Constraint::Length(18),
                Constraint::Min(20),
            ],
        )
        .header(table_header(
            &["KIND", "STATUS", "CAND vs BASE", "CONTENT"],
            app.color,
        ))
        .column_spacing(2)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ")
        .block(block),
        area,
        &mut state,
    );
}

fn tool_detail_lines(app: &App) -> Vec<Line<'static>> {
    let now = chrono::Utc::now();
    let label = |k: &str| Span::styled(format!("{} ", ui::pad(k, 9)), ui::dim(app.color));
    let mut lines = match &app.tool_detail {
        Some(d) => {
            let m = &d.metrics;
            let mut versions: Vec<Span> = vec![label("versions")];
            for v in &d.versions {
                let text = format!("v{} {}  ", v.version, v.author);
                versions.push(if v.version == d.tool.current_version {
                    Span::styled(text, bold_fg(app.color, Color::Yellow))
                } else {
                    Span::styled(text, ui::dim(app.color))
                });
            }
            let mut lines =
                    vec![
                        Line::from(vec![
                            Span::styled(d.tool.name.clone(), bold_fg(app.color, Color::Yellow)),
                            Span::raw("  ["),
                            tool_status(&d.tool, app.color),
                            Span::raw("]"),
                            Span::raw(format!("  {}", d.tool.description)),
                        ]),
                        Line::from(versions),
                        Line::from(vec![
                            label("caps"),
                            Span::raw(if d.tool.capabilities.is_empty() {
                                "(none)".to_string()
                            } else {
                                d.tool.capabilities.join(", ")
                            }),
                            Span::styled(
                                format!("  · memory {} MB", d.tool.memory_mb),
                                ui::dim(app.color),
                            ),
                        ]),
                        Line::from(vec![
                            label("success"),
                            Span::styled(
                                format!(
                                    "{} {}",
                                    m.success_rate.map(|r| ui::bar(r, 16)).unwrap_or_default(),
                                    ui::rate_text(m.success_rate)
                                ),
                                ui::rate_style(app.color, m.success_rate),
                            ),
                            Span::styled(
                                format!(
                            "  {} calls · {} failed · {} denied · {} invalid · p50 {} · p95 {} · per success {}",
                            m.total,
                            m.failure,
                            m.denied,
                            m.validation_failures,
                            m.p50_ms.map(|v| format!("{v:.0}ms")).unwrap_or_else(|| "-".into()),
                            m.p95_ms.map(|v| format!("{v:.0}ms")).unwrap_or_else(|| "-".into()),
                            cost_per_success(m),
                        ),
                                ui::dim(app.color),
                            ),
                        ]),
                    ];
            if let Some(g) = &d.approval {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Approved (revoke with /revoke NAME [CAPABILITY])",
                    bold_fg(app.color, Color::Cyan),
                ));
                for c in &g.capabilities {
                    lines.push(Line::raw(format!("  • {c}")));
                }
                lines.push(Line::styled(
                    format!("  • memory {} MB", g.memory_mb),
                    ui::dim(app.color),
                ));
            }
            if !d.optimiser_attempts.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::styled("Optimiser", bold_fg(app.color, Color::Cyan)));
                for a in d.optimiser_attempts.iter().take(5) {
                    let style = match a.status.as_str() {
                        "promoted" => fg(app.color, Color::Green),
                        "discarded" | "failed" => fg(app.color, Color::Red),
                        _ => fg(app.color, Color::Yellow),
                    };
                    let score = |s: Option<f64>| {
                        s.map(|s| format!("{:.0}%", s * 100.0))
                            .unwrap_or_else(|| "-".into())
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {} ", ui::pad(&a.status, 10)), style),
                        Span::raw(format!(
                            "cand {} vs base {} · {} replays  ",
                            score(a.candidate_score),
                            score(a.baseline_score),
                            a.replay_total
                        )),
                        Span::styled(
                            format!("{} · {}", ui::relative_time(&a.created_at, now), a.detail),
                            ui::dim(app.color),
                        ),
                    ]));
                }
            }
            let current = d
                .versions
                .iter()
                .position(|v| v.version == d.tool.current_version);
            if let Some(i) = current.filter(|i| *i > 0) {
                // What the current version changed (optimiser or agent edit).
                let (prev, cur) = (&d.versions[i - 1], &d.versions[i]);
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("Changes v{} → v{}", prev.version, cur.version),
                    bold_fg(app.color, Color::Cyan),
                ));
                for l in formless_core::diff::version_diff(prev, cur)
                    .lines()
                    .take(40)
                {
                    let style = match l.chars().next() {
                        Some('+') => fg(app.color, Color::Green),
                        Some('-') => fg(app.color, Color::Red),
                        Some('@') => ui::dim(app.color),
                        _ => Style::default(),
                    };
                    lines.push(Line::styled(format!("  {l}"), style));
                }
            }
            if let Some(cur) = d
                .versions
                .iter()
                .find(|v| v.version == d.tool.current_version)
            {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("Code v{} · Shift+E to edit", cur.version),
                    bold_fg(app.color, Color::Cyan),
                ));
                for (i, l) in cur.code.lines().take(40).enumerate() {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{:>4} ", i + 1), ui::dim(app.color)),
                        Span::raw(l.to_string()),
                    ]));
                }
            }
            lines
        }
        None => vec![Line::styled(
            "Select a Tool and press Enter for detail.",
            ui::dim(app.color),
        )],
    };
    if !app.modules.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Shared modules (requireModule)",
            bold_fg(app.color, Color::Cyan),
        ));
        for m in &app.modules {
            lines.push(Line::from(vec![
                Span::raw(format!("  {} v{}  ", m.name, m.current_version)),
                Span::styled(m.description.clone(), ui::dim(app.color)),
            ]));
        }
    }
    lines
}

fn draw_tools(f: &mut Frame, area: Rect, app: &App) {
    let list_h = (app.tools.len() as u16 + 3).min(area.height / 2).max(4);
    let parts = Layout::vertical([Constraint::Length(list_h), Constraint::Min(3)]).split(area);
    let block = ui::panel(format!(" Tools ({}) ", app.tools.len()), app.color, Color::Yellow)
        .title_bottom(ui::hint(
            "Enter detail · E edit · o optimise · x retire · b rollback · e export · i import · p purge",
            app.color,
        ));
    if app.tools.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(NO_TOOLS, ui::dim(app.color))).block(block),
            parts[0],
        );
    } else {
        let mut state = TableState::default();
        state.select(Some(app.tool_idx.min(app.tools.len() - 1)));
        f.render_stateful_widget(
            Table::new(
                app.tools.iter().map(|t| {
                    let mut cells = tool_cells(t, app.color);
                    cells.push(Cell::from(tool_status(t, app.color)));
                    cells.push(Cell::from(Span::styled(
                        if t.capabilities.is_empty() {
                            "(none)".to_string()
                        } else {
                            t.capabilities.join(", ")
                        },
                        ui::dim(app.color),
                    )));
                    Row::new(cells)
                }),
                [
                    Constraint::Length(26),
                    Constraint::Length(5),
                    Constraint::Length(7),
                    Constraint::Length(8),
                    Constraint::Length(9),
                    Constraint::Min(10),
                ],
            )
            .header(table_header(
                &["TOOL", "VER", "CALLS", "SUCCESS", "STATUS", "CAPABILITIES"],
                app.color,
            ))
            .column_spacing(2)
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› ")
            .block(block),
            parts[0],
            &mut state,
        );
    }
    f.render_widget(
        Paragraph::new(tool_detail_lines(app))
            .block(ui::panel(" Detail ", app.color, Color::DarkGray))
            .wrap(Wrap { trim: false }),
        parts[1],
    );
}

fn draw_approval(f: &mut Frame, req: &ApprovalReq, promotion: bool, more: usize, color: bool) {
    let mut lines = match &req.detail {
        // A non-Tool approval (e.g. the agent adding an MCP server).
        Some(detail) => {
            let mut l: Vec<Line> = detail
                .lines()
                .map(|d| Line::styled(d.to_string(), bold_fg(color, Color::Yellow)))
                .collect();
            l.push(Line::raw(""));
            l
        }
        None => vec![
            Line::from(vec![
                Span::raw("Tool "),
                Span::styled(req.tool_name.clone(), bold_fg(color, Color::Yellow)),
                Span::raw(format!(
                    " v{} {}",
                    req.version,
                    if promotion {
                        "is ready to be promoted with:"
                    } else {
                        "wants sensitive capabilities:"
                    }
                )),
            ]),
            Line::raw(""),
        ],
    };
    if req.capabilities.is_empty() {
        lines.push(Line::styled("  • (no capabilities)", ui::dim(color)));
    }
    for cap in &req.capabilities {
        if req.already_allowed.contains(cap) {
            lines.push(Line::styled(
                format!("  • {cap}  (already allowed)"),
                ui::dim(color),
            ));
            continue;
        }
        // Bare `net` / `process` are unscoped and flagged broad (§7.2).
        if cap == "net" || cap == "process" {
            lines.push(Line::from(vec![
                Span::styled(format!("  • {cap}"), bold_fg(color, Color::Red)),
                Span::styled("  unscoped — broad access", fg(color, Color::Red)),
            ]));
        } else {
            lines.push(Line::raw(format!("  • {cap}")));
        }
    }
    if req.detail.is_none() {
        lines.push(Line::raw(format!("  • memory {} MB", req.memory_mb)));
    }
    if let Some(code) = &req.code_preview {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Code (first lines):", ui::dim(color)));
        for l in code.lines().take(15) {
            lines.push(Line::styled(
                format!("  {}", truncate_chars(l, 100)),
                ui::dim(color),
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        if promotion {
            "The new version replaces the current one; rollback stays available."
        } else {
            "Always: lasts for this Tool until its capabilities widen. Once: this call only."
        },
        ui::dim(color),
    ));
    lines.push(Line::raw(""));
    let mut keys = vec![
        Span::styled(
            " y ",
            bold_fg(color, Color::Green).add_modifier(Modifier::REVERSED),
        ),
        Span::raw(if promotion {
            " approve    "
        } else {
            " always    "
        }),
    ];
    if !promotion {
        keys.push(Span::styled(
            " o ",
            bold_fg(color, Color::Yellow).add_modifier(Modifier::REVERSED),
        ));
        keys.push(Span::raw(" once    "));
    }
    keys.extend([
        Span::styled(
            " n ",
            bold_fg(color, Color::Red).add_modifier(Modifier::REVERSED),
        ),
        Span::raw(" deny"),
    ]);
    if more > 0 {
        keys.push(Span::styled(
            format!("      +{more} more waiting"),
            ui::dim(color),
        ));
    }
    lines.push(Line::from(keys));

    let area = f.area();
    let content_w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 4;
    let w = content_w.max(50).min(area.width.saturating_sub(2)).max(1);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .block(
                ui::panel(
                    if promotion {
                        " Promotion approval "
                    } else {
                        " Approval required "
                    },
                    color,
                    Color::Red,
                )
                .border_style(bold_fg(color, Color::Red)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn draw_editor(f: &mut Frame, ed: &EditorState, app: &App) {
    let area = f.area();
    let parts = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let block = ui::panel(format!(" Edit {} ", ed.name), app.color, Color::Yellow);
    let inner = block.inner(parts[0]);
    f.render_widget(block, parts[0]);

    let before = &ed.chars[..ed.cursor];
    let cursor_line = before.iter().filter(|c| **c == '\n').count();
    let cursor_col = before.iter().rev().take_while(|c| **c != '\n').count();
    let text = ed.text();
    let line_total = text.split('\n').count();
    let gutter_w = line_total.to_string().len() as u16 + 2;
    let cols = Layout::horizontal([Constraint::Length(gutter_w), Constraint::Min(1)]).split(inner);
    let height = inner.height as usize;
    let code_w = cols[1].width as usize;
    // Keep the cursor visible both vertically and horizontally (no wrap).
    let scroll_y = cursor_line.saturating_sub(height.saturating_sub(1));
    let scroll_x = cursor_col.saturating_sub(code_w.saturating_sub(1));
    let gutter: Vec<Line> = (scroll_y..line_total.min(scroll_y + height))
        .map(|i| {
            let style = if i == cursor_line {
                fg(app.color, Color::Yellow)
            } else {
                ui::dim(app.color)
            };
            Line::styled(format!("{:>w$} ", i + 1, w = gutter_w as usize - 1), style)
        })
        .collect();
    f.render_widget(Paragraph::new(gutter), cols[0]);
    let lines: Vec<Line> = text.split('\n').map(|l| Line::raw(l.to_string())).collect();
    f.render_widget(
        Paragraph::new(lines).scroll((scroll_y as u16, scroll_x as u16)),
        cols[1],
    );
    f.set_cursor_position((
        cols[1].x + (cursor_col - scroll_x) as u16,
        cols[1].y + (cursor_line - scroll_y) as u16,
    ));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" Ln {}, Col {} ", cursor_line + 1, cursor_col + 1),
                bold_fg(app.color, Color::Yellow),
            ),
            Span::styled(
                " Ctrl+S save as new version (smoke-tested) · Esc cancel",
                ui::dim(app.color),
            ),
        ])),
        parts[1],
    );
}

fn draw_panes(f: &mut Frame, area: Rect, app: &mut App) {
    let cols = Layout::horizontal([
        Constraint::Percentage(22),
        Constraint::Percentage(52),
        Constraint::Percentage(26),
    ])
    .split(area);
    let left =
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).split(cols[0]);
    let now = chrono::Utc::now();
    let name_w = (left[0].width as usize).saturating_sub(13).max(6);
    let session_items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|s| {
            let current = Some(&s.id) == app.current.as_ref();
            ListItem::new(Line::from(vec![
                Span::styled(
                    if current { "● " } else { "  " },
                    fg(app.color, Color::Cyan),
                ),
                Span::styled(
                    ui::pad(&s.name, name_w),
                    if current {
                        bold_fg(app.color, Color::Cyan)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(ui::relative_time(&s.updated_at, now), ui::dim(app.color)),
            ]))
        })
        .collect();
    f.render_widget(
        List::new(session_items).block(ui::panel(" Sessions ", app.color, Color::Cyan)),
        left[0],
    );
    if app.tools.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("No Tools yet.", ui::dim(app.color))).block(ui::panel(
                " Tools ",
                app.color,
                Color::Yellow,
            )),
            left[1],
        );
    } else {
        f.render_widget(
            Table::new(
                app.tools.iter().map(|t| {
                    Row::new(vec![
                        Cell::from(Line::from(vec![
                            tool_glyph(t, app.color),
                            Span::raw(t.name.clone()),
                        ])),
                        Cell::from(
                            Line::styled(
                                ui::rate_text(t.success_rate),
                                ui::rate_style(app.color, t.success_rate),
                            )
                            .right_aligned(),
                        ),
                    ])
                }),
                [Constraint::Min(8), Constraint::Length(5)],
            )
            .block(ui::panel(" Tools ", app.color, Color::Yellow)),
            left[1],
        );
    }
    draw_chat(f, cols[1], app);
    let right = Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).split(cols[2]);
    let total: i64 = app.tools.iter().map(|t| t.invocations).sum();
    let attention = app.tools.iter().filter(|t| t.needs_attention).count();
    let mut overview = vec![
        Line::from(vec![
            Span::styled("tools   ", ui::dim(app.color)),
            Span::raw(app.tools.len().to_string()),
            Span::styled(format!("  · {total} calls"), ui::dim(app.color)),
        ]),
        Line::from(vec![
            Span::styled("attention ", ui::dim(app.color)),
            Span::styled(
                attention.to_string(),
                if attention > 0 {
                    bold_fg(app.color, Color::Red)
                } else {
                    fg(app.color, Color::Green)
                },
            ),
        ]),
    ];
    overview.push(Line::from(vec![
        Span::styled("modules ", ui::dim(app.color)),
        Span::raw(if app.modules.is_empty() {
            "none".to_string()
        } else {
            app.modules
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }),
    ]));
    f.render_widget(
        Paragraph::new(overview)
            .block(ui::panel(" Overview ", app.color, Color::Green))
            .wrap(Wrap { trim: false }),
        right[0],
    );
    let notes: Vec<Line> = if app.notes.is_empty() {
        vec![Line::styled("Nothing yet.", ui::dim(app.color))]
    } else {
        app.notes
            .iter()
            .rev()
            .take(right[1].height.saturating_sub(2) as usize)
            .rev()
            .map(|n| {
                Line::from(vec![
                    Span::styled("· ", ui::dim(app.color)),
                    Span::raw(truncate_chars(n, 120)),
                ])
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(notes)
            .block(
                ui::panel(" Activity ", app.color, Color::Magenta)
                    .title_bottom(ui::hint("/panes", app.color)),
            )
            .wrap(Wrap { trim: false }),
        right[1],
    );
}

fn draw_wizard(f: &mut Frame, w: &Wizard, color: bool) {
    let (step_no, step_name) = match w.step {
        WizardStep::BaseUrl => (1, "Provider"),
        WizardStep::ApiKey | WizardStep::FetchModels => (2, "API key"),
        WizardStep::PickModel => (3, "Model"),
        WizardStep::PickEffort => (4, "Reasoning effort"),
        WizardStep::Probing | WizardStep::Failed => (5, "Tool-calling check"),
    };
    let status_style = if w.step == WizardStep::Failed {
        bold_fg(color, Color::Red)
    } else {
        fg(color, Color::Gray)
    };
    let progress: String = (1..=5)
        .map(|i| if i <= step_no { "●" } else { "○" })
        .collect::<Vec<_>>()
        .join(" ");
    let field = |k: &str, v: String| {
        Line::from(vec![
            Span::styled(format!("{} ", ui::pad(k, 9)), ui::dim(color)),
            Span::raw(v),
        ])
    };
    let mut text = vec![
        Line::from(vec![
            Span::styled(progress, fg(color, Color::Cyan)),
            Span::styled(
                format!("   step {step_no} of 5 · {step_name}"),
                ui::dim(color),
            ),
        ]),
        Line::raw(""),
        Line::styled(w.status.clone(), status_style),
        Line::raw(""),
    ];
    let area = f.area();
    let max_items = (area.height as usize).saturating_sub(12).max(3);
    let pick = |items: Vec<String>, idx: usize| -> Vec<Line<'static>> {
        // Long model lists scroll so the selection is always on screen.
        let start = idx
            .saturating_sub(max_items / 2)
            .min(items.len().saturating_sub(max_items));
        let total = items.len();
        let mut out: Vec<Line<'static>> = items
            .into_iter()
            .enumerate()
            .skip(start)
            .take(max_items)
            .map(|(i, m)| {
                if i == idx {
                    Line::styled(format!("› {m}"), bold_fg(color, Color::Green))
                } else {
                    Line::raw(format!("  {m}"))
                }
            })
            .collect();
        if total > max_items {
            out.push(Line::styled(
                format!("  ({}/{total})", idx + 1),
                ui::dim(color),
            ));
        }
        out
    };
    match w.step {
        WizardStep::BaseUrl => text.push(field("Base URL", format!("{}▏", w.base_url))),
        WizardStep::ApiKey | WizardStep::FetchModels => {
            text.push(field("Base URL", w.base_url.clone()));
            text.push(field(
                "API key",
                format!("{}▏", "•".repeat(w.api_key.len())),
            ));
        }
        WizardStep::PickModel => {
            text.extend(picker_lines(&w.picker, max_items, color));
        }
        WizardStep::PickEffort => {
            text.push(field("Model", w.model()));
            text.push(Line::raw(""));
            text.extend(pick(
                EFFORTS
                    .iter()
                    .map(|e| e.unwrap_or("default (provider)").to_string())
                    .collect(),
                w.effort_idx,
            ));
        }
        WizardStep::Probing | WizardStep::Failed => {
            text.push(field("Model", w.model()));
        }
    }
    text.push(Line::raw(""));
    text.push(Line::styled(
        "Enter continue · Esc back · Ctrl+C quit",
        ui::dim(color),
    ));
    let width = area.width.min(84);
    let height = (text.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width,
        height,
    };
    f.render_widget(
        Paragraph::new(text)
            .block(ui::panel(" ◇ Formless setup ", color, Color::Cyan))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub fn run(paths: Paths) -> Result<()> {
    let mut app = App::new(paths)?;
    enable_raw_mode()?;
    let mut out = stdout();
    out.execute(EnterAlternateScreen)?;
    out.execute(crossterm::event::EnableMouseCapture)?;
    // Ctrl+Enter is only distinguishable with the kitty keyboard protocol.
    app.enhanced_keys = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
        && out
            .execute(crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
            ))
            .is_ok();
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
    if app.enhanced_keys {
        let _ = stdout().execute(crossterm::event::PopKeyboardEnhancementFlags);
    }
    disable_raw_mode()?;
    let _ = stdout().execute(crossterm::event::DisableMouseCapture);
    stdout().execute(LeaveAlternateScreen)?;
    result
}

/// "3 steps · 12.3k in / 1.2k out · 40% cached · $0.021" (SPEC §10.6).
fn usage_text(u: &formless_protocol::UsageInfo) -> String {
    fn k(n: i64) -> String {
        if n >= 1000 {
            format!("{:.1}k", n as f64 / 1000.0)
        } else {
            n.to_string()
        }
    }
    let mut s = format!(
        "{} step{} · {} in / {} out",
        u.steps,
        if u.steps == 1 { "" } else { "s" },
        k(u.prompt_tokens),
        k(u.completion_tokens)
    );
    if u.prompt_tokens > 0 && u.cached_tokens > 0 {
        s.push_str(&format!(
            " · {:.0}% cached",
            100.0 * u.cached_tokens as f64 / u.prompt_tokens as f64
        ));
    }
    if let Some(c) = u.cost {
        s.push_str(&format!(" · ${c:.3}"));
    }
    s
}

/// Cost per Success (SPEC §8.1): "1.2s, 3400 tok", or "-".
fn cost_per_success(m: &formless_protocol::ToolMetricsInfo) -> String {
    match (m.ms_per_success, m.tokens_per_success) {
        (Some(ms), Some(tok)) => format!("{:.1}s, {tok:.0} tok", ms / 1000.0),
        _ => "-".into(),
    }
}

/// " · 80% of 5 Runs completed" for a skill loaded in Runs, else "".
fn skill_success(s: &formless_protocol::SkillInfo) -> String {
    if s.runs == 0 {
        return String::new();
    }
    format!(
        " · {}% of {} Runs completed",
        100 * s.runs_completed / s.runs,
        s.runs
    )
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
            multi_pane: false,
            model_label: "test-model".into(),
            tick: 0,
            activity: None,
            live_tools: Vec::new(),
            chat_scroll_top: None,
            chat_view_height: 10,
            chat_line_count: 100,
            input: String::new(),
            ctrl_c_at: None,
            enhanced_keys: false,
            mouse_capture: true,
            proposal_generating: false,
            running_sessions: Default::default(),
            notes: Vec::new(),
            pending: HashMap::new(),
            tools: Vec::new(),
            tool_idx: 0,
            tool_detail: None,
            rollback_pick: None,
            model_overlay: None,
            mention: None,
            pending_send: None,
            palette_sel: 0,
            palette_hidden: None,
            approvals: std::collections::VecDeque::new(),
            questions: Vec::new(),
            run_usage: HashMap::new(),
            run_banner: None,
            invocations: Vec::new(),
            promotions: std::collections::VecDeque::new(),
            purge_arm: None,
            last_deleted: None,
            pager: None,
            delete_arm: None,
            subagents: Vec::new(),
            import_mode: None,
            renaming: false,
            modules: Vec::new(),
            model_stats: Vec::new(),
            run_summary: None,
            builtin_stats: Vec::new(),
            proposals: Vec::new(),
            proposal_idx: 0,
            editor: None,
            search_hits: Vec::new(),
            search_idx: 0,
            search_mode: false,
            editing_id: None,
            should_quit: false,
            md_cache: Default::default(),
            stream_cache: Default::default(),
        }
    }

    #[test]
    fn new_session_mid_run_is_isolated_from_the_old_run() {
        let mut app = test_app();
        app.on_event(Event::RunStarted {
            session_id: "s1".into(),
            run_id: "r1".into(),
        });
        app.on_event(Event::MessageDelta {
            session_id: "s1".into(),
            run_id: "r1".into(),
            delta: "old ".into(),
        });
        // `/new` while s1 is still working.
        app.switch_session("s2".into());
        assert!(!app.running());
        assert!(app.streaming.is_empty() && app.activity.is_none());

        // The old Run keeps streaming; none of it shows up in s2.
        app.on_event(Event::MessageDelta {
            session_id: "s1".into(),
            run_id: "r1".into(),
            delta: "more".into(),
        });
        app.on_event(Event::ToolCallStarted {
            session_id: "s1".into(),
            run_id: "r1".into(),
            tool_name: "t".into(),
            version: 1,
            arguments: "{}".into(),
            depth: 0,
        });
        assert!(app.streaming.is_empty() && app.live_tools.is_empty());
        assert!(app.activity.is_none());

        // A message in s2 starts a fresh Run rather than steering s1's.
        app.input = "hi".into();
        app.submit_chat(false);
        app.on_event(Event::RunFinished {
            session_id: "s1".into(),
            run_id: "r1".into(),
            status: "completed".into(),
        });
        assert!(!app.running_sessions.contains("s1"));

        // Switching back to a still-running session shows it as running.
        app.on_event(Event::RunStarted {
            session_id: "s2".into(),
            run_id: "r2".into(),
        });
        app.switch_session("s1".into());
        assert!(!app.running());
        app.switch_session("s2".into());
        assert!(app.running() && app.activity.is_some());
    }

    #[test]
    fn activity_and_thinking_follow_run_events() {
        let mut app = test_app();
        app.on_event(Event::RunStarted {
            session_id: "s1".into(),
            run_id: "r1".into(),
        });
        assert!(app.running());
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
            depth: 0,
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
            detail: None,
            memory_mb: 256,
            already_allowed: vec![],
            code_preview: None,
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
        assert!(!app.running());
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
    fn metrics_summary_counts_tools() {
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
        let summary = metrics_summary(&tools);
        assert!(summary.contains("12 invocations"), "{summary}");
        assert!(summary.contains("1 retired") && summary.contains("1 need attention"));
        assert_eq!(ui::rate_text(tools[0].success_rate), "90%");
        assert!(metrics_summary(&[]).starts_with("0 Tools"));
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
    fn editor_edits_text_and_moves_cursor() {
        let mut app = test_app();
        app.editor = Some(EditorState {
            tool_id: "t1".into(),
            name: "writer".into(),
            chars: "line1\nline2".chars().collect(),
            cursor: 11,
        });
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Char('!')));
        app.key(key(KeyCode::Left));
        app.key(key(KeyCode::Backspace));
        assert_eq!(app.editor.as_ref().unwrap().text(), "line1\nline!");
        app.key(key(KeyCode::Home));
        assert_eq!(app.editor.as_ref().unwrap().cursor, 6);
        app.key(key(KeyCode::Up));
        assert_eq!(app.editor.as_ref().unwrap().cursor, 0);
        app.key(key(KeyCode::Enter));
        assert!(app.editor.as_ref().unwrap().text().starts_with("\nline1"));
        app.key(key(KeyCode::Esc));
        assert!(app.editor.is_none(), "Esc cancels the editor");
    }

    #[test]
    fn pane_layout_requires_opt_in_and_width() {
        assert!(!pane_layout(false, 200));
        assert!(!pane_layout(true, 109));
        assert!(pane_layout(true, 110));
        assert!(pane_layout(true, 240));
        // The preference round-trips through config (persistence).
        let app = test_app();
        let mut cfg = Config::default();
        cfg.tui.multi_pane = true;
        cfg.save(&app.paths).unwrap();
        assert!(Config::load(&app.paths).unwrap().tui.multi_pane);
        // Old configs without the key default to off.
        std::fs::write(
            app.paths.config_file(),
            "[tui]\nqueue_key = \"ctrl+enter\"\n",
        )
        .unwrap();
        assert!(!Config::load(&app.paths).unwrap().tui.multi_pane);
    }

    #[test]
    fn no_color_styles_carry_no_foreground() {
        assert_eq!(fg(false, Color::Cyan).fg, None);
        assert_eq!(role_style(false, "user").fg, None);
        assert_eq!(fg(true, Color::Cyan).fg, Some(Color::Cyan));
        assert_eq!(role_style(true, "user").fg, Some(Color::Cyan));
        assert_eq!(role_style(true, "assistant").fg, Some(Color::Green));
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect()
    }

    fn msg(role: &str, content: &str) -> MessageInfo {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        MessageInfo {
            id: format!(
                "m{}",
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ),
            session_id: "s1".into(),
            seq: 1,
            role: role.into(),
            content: content.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: None,
        }
    }

    #[test]
    fn persisted_assistant_step_resets_live_stream() {
        let mut app = test_app();
        let delta = |app: &mut App, d: &str| {
            app.on_event(Event::MessageDelta {
                session_id: "s1".into(),
                run_id: "r1".into(),
                delta: d.into(),
            })
        };
        delta(&mut app, "step one");
        app.on_event(Event::MessageAppended {
            session_id: "s1".into(),
            message: msg("assistant", "step one"),
        });
        assert!(app.streaming.is_empty() && app.thinking.is_empty());
        delta(&mut app, "step two");
        assert_eq!(app.streaming, "step two");
    }

    #[test]
    fn stable_prefix_stops_at_blank_lines_outside_fences() {
        assert_eq!(stable_prefix_len("one line, no end"), 0);
        assert_eq!(stable_prefix_len("para\n\nnext"), 6);
        // A blank line inside an open fence is not a boundary.
        let fenced = "intro\n\n```rust\nfn a() {}\n\nfn b() {}\n";
        assert_eq!(stable_prefix_len(fenced), 7);
        let closed = "```\ncode\n```\n\nafter";
        assert_eq!(stable_prefix_len(closed), closed.len() - "after".len());
    }

    #[test]
    fn incremental_streaming_render_matches_a_full_render() {
        let doc = "# Title\n\nSome **bold** text and `code`.\n\n- one\n- two\n\n\
                   1. first\n2. second\n\n```rust\nfn main() {\n\n    println!(\"hi\");\n}\n```\n\n\
                   > quoted\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nThe end.";
        let mut app = test_app();
        for cut in (0..=doc.len()).filter(|i| doc.is_char_boundary(*i)) {
            app.streaming = doc[..cut].to_string();
            let full = markdown::render(&app.streaming, app.color, "  ", 60);
            assert_eq!(app.streaming_markdown(60), full, "cut at {cut}");
        }
    }

    #[test]
    fn rollback_picker_chooses_any_version_and_confirms() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let tool: ToolInfo = serde_json::from_value(serde_json::json!({
            "id": "t1", "name": "page_title", "description": "", "retired": false,
            "current_version": 3, "capabilities": [], "memory_mb": 256, "invocations": 9,
            "success_rate": 0.5, "needs_attention": false, "created_at": "", "updated_at": ""
        }))
        .unwrap();
        let version = |v: i64| {
            serde_json::json!({"version": v, "author": if v == 2 { "optimiser" } else { "agent" },
                "created_at": "2026-10-21T10:00:00Z", "capabilities": [], "memory_mb": 256,
                "input_schema": "{}", "output_schema": "{}", "code": ""})
        };
        app.tool_detail = Some(
            serde_json::from_value(serde_json::json!({
                "tool": tool, "versions": [version(1), version(2), version(3)],
                "metrics": {"total": 0, "success": 0, "failure": 0, "denied": 0, "cancelled": 0,
                    "validation_failures": 0, "success_rate": null, "retry_rate": null,
                    "p50_ms": null, "p95_ms": null},
            }))
            .unwrap(),
        );
        app.tools = vec![tool];
        app.view = View::Tools;
        app.key(key(KeyCode::Char('b')));
        assert_eq!(
            app.rollback_pick.as_ref().map(|p| p.idx),
            Some(1),
            "starts at v2"
        );
        let screen = render(&mut app, 100, 30);
        assert!(
            screen.contains("Roll back page_title") && screen.contains("optimiser"),
            "{screen}"
        );
        app.key(key(KeyCode::Up));
        app.key(key(KeyCode::Enter));
        assert!(
            app.rollback_pick.as_ref().unwrap().armed,
            "first Enter only arms"
        );
        app.key(key(KeyCode::Enter));
        assert!(app.rollback_pick.is_none());
        assert!(
            app.notes.last().unwrap().contains("back to v1"),
            "{:?}",
            app.notes
        );
        // Any other key cancels.
        app.key(key(KeyCode::Char('b')));
        app.key(key(KeyCode::Char('x')));
        assert!(app.rollback_pick.is_none());
    }

    #[test]
    fn deleting_a_session_needs_a_second_press() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.view = View::Sessions;
        app.sessions = vec![SessionInfo {
            id: "s1".into(),
            name: "notes".into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
        }];
        app.key(key(KeyCode::Char('d')));
        assert_eq!(app.delete_arm.as_deref(), Some("s1"));
        assert!(app.notes.last().unwrap().contains("press d again"));
        // Any other key disarms; the next `d` arms again instead of deleting.
        app.key(key(KeyCode::Down));
        assert!(app.delete_arm.is_none());
        app.key(key(KeyCode::Char('d')));
        assert_eq!(app.delete_arm.as_deref(), Some("s1"));
        // The second press fires the delete (no client here) and disarms.
        app.key(key(KeyCode::Char('d')));
        assert!(app.delete_arm.is_none());
        assert!(!app.notes.last().unwrap().contains("cancelled"));
    }

    #[test]
    fn approval_modal_always_shows_its_keys() {
        let mut app = test_app();
        app.approvals.push_back(ApprovalReq {
            approval_id: "a".into(),
            tool_name: "page_title".into(),
            version: 1,
            capabilities: vec!["net".into(), "fs:read:/tmp".into(), "env:HOME".into()],
            memory_mb: 256,
            detail: None,
            already_allowed: Vec::new(),
            code_preview: None,
        });
        let screen = render(&mut app, 100, 24);
        assert!(
            screen.contains("always") && screen.contains("once") && screen.contains("deny"),
            "{screen}"
        );
        assert!(screen.contains("unscoped"), "{screen}");
        assert!(screen.contains("1 awaiting approval"), "{screen}");
    }

    #[test]
    fn chat_scroll_counts_wrapped_rows() {
        let mut app = test_app();
        app.messages = vec![
            msg("user", &"a long line that wraps several times ".repeat(20)),
            msg("assistant", "**done**\n\nNEWEST LINE"),
        ];
        let screen = render(&mut app, 60, 20);
        assert!(screen.contains("NEWEST LINE"), "{screen}");
        assert!(
            !screen.contains("**"),
            "markdown markers are rendered: {screen}"
        );
    }

    #[test]
    fn tab_cycles_every_view_both_ways() {
        let mut app = test_app();
        let tab = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE));
        let mut seen = Vec::new();
        for _ in 0..VIEW_ORDER.len() {
            tab(&mut app, KeyCode::Tab);
            seen.push(app.view);
        }
        assert_eq!(seen.last(), Some(&View::Chat));
        assert!(VIEW_ORDER.iter().all(|v| seen.contains(v)), "{seen:?}");
        tab(&mut app, KeyCode::BackTab);
        assert_eq!(app.view, View::Settings);
        app.view = View::Search;
        tab(&mut app, KeyCode::Tab);
        assert_eq!(app.view, View::Chat);
    }

    #[test]
    fn every_view_renders_at_tiny_sizes() {
        let mut app = test_app();
        app.messages = vec![msg("user", "hi"), msg("tool", "{\"ok\":false}")];
        app.multi_pane = true;
        for view in VIEW_ORDER.iter().copied().chain([View::Search]) {
            app.view = view;
            for (w, h) in [(1, 1), (12, 4), (30, 8), (120, 30)] {
                render(&mut app, w, h);
            }
        }
        app.approvals.push_back(ApprovalReq {
            approval_id: "a".into(),
            tool_name: "t".into(),
            version: 1,
            capabilities: vec![],
            memory_mb: 1,
            detail: None,
            already_allowed: Vec::new(),
            code_preview: None,
        });
        app.editor = Some(EditorState {
            tool_id: "t".into(),
            name: "t".into(),
            chars: "a\nb".chars().collect(),
            cursor: 3,
        });
        for (w, h) in [(1, 1), (12, 4), (80, 10)] {
            render(&mut app, w, h);
        }
        app.editor = None;
        app.wizard = Some(Wizard::new(&Config::default()));
        for (w, h) in [(1, 1), (12, 4), (80, 24)] {
            render(&mut app, w, h);
        }
    }

    #[test]
    fn every_listed_command_is_dispatched() {
        // The table and the dispatcher can't drift: each entry (with a
        // dummy argument where one is needed) must not be "unknown".
        for c in commands::COMMANDS {
            let mut app = test_app();
            let line = if c.args.is_empty() {
                c.name.to_string()
            } else {
                format!("{} x", c.name)
            };
            app.slash(&line);
            assert!(
                !app.notes.iter().any(|n| n.contains("unknown command")),
                "/{line} is listed but not handled"
            );
        }
        let mut app = test_app();
        app.slash("sesions");
        assert!(app
            .notes
            .last()
            .unwrap()
            .contains("did you mean /sessions?"));
    }

    #[test]
    fn palette_completes_runs_and_hides() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        // `?` on an empty input opens the grouped help.
        app.key(key(KeyCode::Char('?')));
        assert_eq!(app.pager.as_ref().unwrap().title, "Help");
        app.pager = None;

        for c in "/ses".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.palette_items()[0].label, "/sessions");
        render(&mut app, 80, 24);
        app.key(key(KeyCode::Enter));
        assert_eq!(app.view, View::Sessions);
        assert!(app.input.is_empty());
        app.view = View::Chat;

        // A command needing an argument completes instead of running.
        for c in "/atta".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert_eq!(app.input, "/attach ");
        assert!(app.notes.last().unwrap().contains("usage: /attach PATH"));
        for c in "foo".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert!(
            app.palette_items().is_empty(),
            "hidden once arguments start"
        );

        // Tab completes; Esc hides until the input changes.
        app.input = "/role a".into();
        app.key(key(KeyCode::Tab));
        assert_eq!(app.input, "/role add ");
        app.input = "/mo".into();
        app.key(key(KeyCode::Esc));
        assert!(app.palette_items().is_empty());
        app.key(key(KeyCode::Char('d')));
        assert!(!app.palette_items().is_empty());
    }

    #[test]
    fn mention_popup_completes_paths_and_send_waits_for_attachments() {
        use formless_core::files::FileEntry;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        for c in "explain @sr".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.mention.as_ref().unwrap().query, "sr");
        app.mention.as_mut().unwrap().files = vec![
            FileEntry {
                path: "src".into(),
                dir: true,
                bytes: 0,
            },
            FileEntry {
                path: "src/main.rs".into(),
                dir: false,
                bytes: 9,
            },
        ];
        render(&mut app, 80, 24);
        app.key(key(KeyCode::Tab));
        assert_eq!(app.input, "explain @src/");
        assert_eq!(app.mention.as_ref().unwrap().query, "src/");
        app.mention.as_mut().unwrap().files = vec![FileEntry {
            path: "src/main.rs".into(),
            dir: false,
            bytes: 9,
        }];
        app.key(key(KeyCode::Enter));
        assert_eq!(app.input, "explain @src/main.rs ");
        assert!(app.mention.is_none());
        // Esc hides the popup without touching the input.
        app.input = "see @x".into();
        app.update_mention();
        app.mention.as_mut().unwrap().files = vec![FileEntry {
            path: "x.rs".into(),
            dir: false,
            bytes: 1,
        }];
        app.key(key(KeyCode::Esc));
        assert!(app.mention.as_ref().unwrap().dismissed);
        assert_eq!(app.input, "see @x");

        app.pending_send = Some(PendingSend {
            session_id: "s".into(),
            content: "explain @a @b".into(),
            mode: SendMode::Send,
            waiting: 2,
        });
        app.mention_attached();
        assert_eq!(app.pending_send.as_ref().unwrap().waiting, 1);
        app.mention_attached();
        assert!(app.pending_send.is_none());
    }

    #[test]
    fn mentions_are_highlighted_in_user_messages() {
        let line = mention_line("explain @src/main.rs and mail a@b.c", true);
        let marked: Vec<String> = line
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(marked, vec!["@src/main.rs"]);
    }

    #[test]
    fn model_overlay_filters_and_closes() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.slash("model");
        assert!(app.model_overlay.as_ref().unwrap().picker.loading);
        let items = [
            "openai/gpt-4o",
            "anthropic/claude-sonnet-4",
            "anthropic/claude-opus-4",
        ]
        .iter()
        .map(|id| picker::ModelItem {
            id: id.to_string(),
            source: None,
            recent: false,
        })
        .collect();
        app.model_overlay
            .as_mut()
            .unwrap()
            .picker
            .set_items(items, "");
        for c in "son4".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Tab));
        let o = app.model_overlay.as_ref().unwrap();
        assert_eq!(
            o.picker.choice().as_deref(),
            Some("anthropic/claude-sonnet-4")
        );
        assert_eq!(EFFORTS[o.effort_idx], Some("low"));
        for (w, h) in [(1, 1), (12, 4), (80, 24)] {
            render(&mut app, w, h);
        }
        assert!(render(&mut app, 80, 24).contains("Effort"));
        app.key(key(KeyCode::Esc));
        assert!(app.model_overlay.is_none());
    }

    #[test]
    fn approve_lists_waiting_approvals() {
        let mut app = test_app();
        app.slash("approve");
        assert!(app.notes.last().unwrap().contains("no approvals waiting"));
        app.approvals.push_back(ApprovalReq {
            approval_id: "a".into(),
            tool_name: "fetch_page".into(),
            version: 1,
            capabilities: vec!["net:example.com".into()],
            memory_mb: 256,
            detail: None,
            already_allowed: Vec::new(),
            code_preview: None,
        });
        app.slash("approve");
        let last = app.notes.last().unwrap();
        assert!(
            last.contains("fetch_page (net:example.com)") && last.contains("o once"),
            "{last}"
        );
        assert!(!app.notes.iter().any(|n| n.contains("M2")));
    }

    #[test]
    fn live_calls_render_as_a_tree_and_fold_finished_subtrees() {
        let node = |name: &str, depth: usize, status: &str| LiveToolCall {
            name: name.into(),
            depth,
            subagent: None,
            version: 1,
            arguments: String::new(),
            status: status.into(),
            started: Instant::now(),
            elapsed_ms: Some(5),
            handle: None,
            stalled_secs: None,
        };
        let calls = vec![
            node("done_top", 0, "ok"),
            node("outer", 0, "running"),
            node("fetch", 1, "ok"),
            node("parse", 2, "ok"),
            node("parse2", 2, "ok"),
            node("store", 1, "failed"),
            node("retry", 2, "running"),
        ];
        let shown: Vec<(String, usize)> = live_tree(&calls)
            .into_iter()
            .map(|(c, folded)| (c.name.clone(), folded))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("outer".to_string(), 0),
                ("fetch".to_string(), 2),
                ("store".to_string(), 0),
                ("retry".to_string(), 0),
            ],
            "finished top-level calls hide; ok subtrees fold; failed ones stay open"
        );
    }

    #[test]
    fn ctrl_c_clears_then_detaches_on_a_second_press() {
        let mut app = test_app();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.input = "half-typed".into();
        app.key(ctrl_c);
        assert!(
            app.input.is_empty() && !app.should_quit,
            "first press clears"
        );
        app.key(ctrl_c);
        assert!(app.should_quit, "second press within the window detaches");

        let mut app = test_app();
        app.key(ctrl_c);
        assert!(!app.should_quit);
        assert!(app.notes.last().unwrap().contains("again to detach"));
        app.ctrl_c_at = Some(Instant::now() - CTRL_C_WINDOW);
        app.key(ctrl_c);
        assert!(!app.should_quit, "an expired first press only re-arms");
        app.key(ctrl_c);
        assert!(app.should_quit);
    }

    #[test]
    fn queue_keys_work_without_the_kitty_protocol() {
        let k = |code, m| KeyEvent::new(code, m);
        assert!(is_queue_key(&k(KeyCode::Enter, KeyModifiers::CONTROL)));
        assert!(is_queue_key(&k(KeyCode::Enter, KeyModifiers::ALT)));
        assert!(is_queue_key(&k(KeyCode::Char('j'), KeyModifiers::CONTROL)));
        assert!(!is_queue_key(&k(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(!is_queue_key(&k(KeyCode::Char('j'), KeyModifiers::NONE)));
        let mut app = test_app();
        assert_eq!(app.queue_label(), "Ctrl+J", "advertise what works");
        app.enhanced_keys = true;
        assert_eq!(app.queue_label(), "Ctrl+Enter");
    }

    #[test]
    fn code_blocks_and_osc52() {
        let md = "Intro\n```rust\nfn a() {}\n```\ntext\n```\nls -la\necho hi\n```\n";
        assert_eq!(
            code_blocks(md),
            vec!["fn a() {}".to_string(), "ls -la\necho hi".to_string()]
        );
        assert!(code_blocks("no code").is_empty());
        let seq = osc52("hi");
        assert!(seq.contains("]52;c;aGk="), "{seq:?}");
    }

    #[test]
    fn thinking_block_has_a_fixed_height_at_any_width() {
        for text in [
            "",
            "one",
            "a\nb\nc\nd\ne\nf\ng",
            &"x".repeat(500),
            "漢字漢字漢字漢字",
        ] {
            for width in [1, 5, 20, 80] {
                let rows = thinking_rows(text, width, THINKING_ROWS);
                assert_eq!(rows.len(), THINKING_ROWS, "{text:?} at {width}");
                assert!(rows
                    .iter()
                    .all(|r| unicode_width::UnicodeWidthStr::width(r.as_str()) <= width.max(2)));
            }
        }
        assert_eq!(
            thinking_rows("a\nb\nc\nd\ne", 10, 4),
            vec!["b", "c", "d", "e"],
            "keeps the latest rows"
        );
    }
}
