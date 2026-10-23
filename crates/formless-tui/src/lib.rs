pub mod client;
mod clipimg;
mod commands;
mod highlight;
mod images;
mod input;
mod keys;
mod links;
mod lists;
mod markdown;
mod mermaid;
mod notify;
mod picker;
pub mod plain;
mod settings;
mod theme;
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
use std::collections::{HashMap, HashSet};
use std::io::{stdout, Write};
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
        Style::default().fg(theme::remap(color))
    } else {
        Style::default()
    }
}

fn bold_fg(enabled: bool, color: Color) -> Style {
    fg(enabled, color).add_modifier(Modifier::BOLD)
}

fn role_style(enabled: bool, role: &str) -> Style {
    let (name, color) = match role {
        "user" => ("user", Color::Cyan),
        "tool" => ("tool", Color::Yellow),
        "system-note" => ("note", Color::Blue),
        _ => ("assistant", Color::Green),
    };
    match theme::role(name).filter(|_| enabled) {
        // An override is used as given, not remapped.
        Some(c) => Style::default().fg(c).add_modifier(Modifier::BOLD),
        None => bold_fg(enabled, color),
    }
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
    /// Context fill for the status line (quiet) or `/context` (report).
    ContextFill,
    SessionRuns,
    SetCwd,
    SetPin,
    SetArchive,
    SetTags,
    ContextReport,
    SuggestName,
    Subscribed,
    TemplateSession,
    ListTemplates,
    SaveTemplate,
    MentionAttach,
    ApprovalCode,
    /// Providers and Roles for the Settings view.
    SettingsProviders,
    SaveSetting,
    SaveProviderOrRole,
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
    /// Opened from Settings: Enter sets the global default.
    default_only: bool,
}

/// A Session's unsent input and attachments.
#[derive(Default)]
struct Draft {
    input: input::InputLine,
    attachments: Vec<(String, Vec<u8>)>,
}

/// Where text drafts persist between TUI starts.
fn drafts_file(paths: &Paths) -> std::path::PathBuf {
    paths.data_dir.join("tui-drafts.json")
}

/// A bulk action on marked Sessions that needs typed input first.
#[derive(Debug, Clone, Copy, PartialEq)]
enum BulkAction {
    /// The user must type `delete` to confirm.
    Delete,
    /// The input is tags to add.
    Tag,
}

/// What a mouse click can hit.
#[derive(Debug, Clone, PartialEq)]
enum Click {
    View(View),
    /// A row of the Sessions, Tools or Proposals list (visible index).
    Session(usize),
    Tool(usize),
    Proposal(usize),
    /// A persisted tool call's header in the chat.
    Call(String),
}

/// The Ctrl+F find bar: the query, the current match and, from the last
/// draw, the wrapped row of every matching line.
#[derive(Default)]
struct FindBar {
    query: String,
    current: usize,
    rows: Vec<usize>,
    /// Scroll to the current match on the next draw.
    scroll: bool,
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
    /// The highlighted row of the current question: an option, or
    /// `options.len()` for "Other…".
    cursor: usize,
    /// Ticked options of a multi-select question.
    checked: Vec<bool>,
}

impl OpenQuestion {
    fn new(id: String, session_id: String, questions: Vec<Question>) -> Self {
        let mut q = OpenQuestion {
            id,
            session_id,
            questions,
            answers: Vec::new(),
            cursor: 0,
            checked: Vec::new(),
        };
        q.reset_choice();
        q
    }

    fn current(&self) -> &Question {
        &self.questions[self.answers.len()]
    }

    /// Fresh cursor and ticks for the current question.
    fn reset_choice(&mut self) {
        self.cursor = 0;
        self.checked = vec![false; self.current().options.len()];
    }

    fn on_other(&self) -> bool {
        self.cursor >= self.current().options.len()
    }

    /// The answer the panel's selection gives: the ticked options (or the
    /// highlighted one), None on "Other…".
    fn picked(&self) -> Option<QuestionAnswer> {
        let q = self.current();
        let mut selected: Vec<String> = q
            .options
            .iter()
            .zip(&self.checked)
            .filter(|(_, c)| **c)
            .map(|(o, _)| o.label.clone())
            .collect();
        if selected.is_empty() {
            selected.push(q.options.get(self.cursor)?.label.clone());
        }
        Some(QuestionAnswer {
            selected,
            text: None,
        })
    }

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
    /// The Tool behind it, for `v` (its full code).
    tool_id: Option<String>,
    /// The Session whose Run asked.
    session_id: Option<String>,
}

enum WizardJob {
    Models(Result<Vec<String>, String>),
    Probe(Result<ProbeResult, String>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum WizardStep {
    /// Choose a provider preset (or Custom URL).
    PickPreset,
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
    /// Highlighted row in `PickPreset`: a preset, or `PRESETS.len()` for
    /// a custom URL.
    preset_idx: usize,
    preset: Option<&'static formless_core::presets::Preset>,
    /// A failed model fetch from a local server: Enter retries it.
    retry_fetch: bool,
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
        let preset = formless_core::presets::for_url(&cfg.provider.base_url);
        let (step, status) = if has_saved {
            (
                WizardStep::ApiKey,
                "Saved setup found. Re-enter your API key to restore it (Esc to choose another provider).".into(),
            )
        } else {
            (
                WizardStep::PickPreset,
                "Welcome to Formless. Choose your model provider.".into(),
            )
        };
        let preset_idx = preset
            .and_then(|p| {
                formless_core::presets::PRESETS
                    .iter()
                    .position(|x| x.id == p.id)
            })
            .unwrap_or(0);
        let effort_idx = cfg
            .provider
            .reasoning_effort
            .as_deref()
            .and_then(|e| EFFORTS.iter().position(|x| *x == Some(e)))
            .unwrap_or(0);
        Self {
            step,
            preset_idx,
            preset,
            retry_fetch: false,
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

    /// The API-key step, with the preset's key page and any key found in
    /// the environment.
    fn enter_api_key_step(&mut self) {
        self.step = WizardStep::ApiKey;
        let mut status = String::from("Enter your API key (stored in the OS keychain).");
        if let Some(p) = self.preset {
            if let Some(url) = p.key_url {
                status.push_str(&format!(" Get one at {url}"));
            }
            if let Some((var, _)) = formless_core::presets::env_key(p) {
                status = format!("Found ${var}: press Enter to use it, or type a different key.");
            }
        }
        self.status = status;
    }

    /// List the endpoint's models in the background (`FetchModels`).
    fn fetch_models(&mut self) {
        self.step = WizardStep::FetchModels;
        self.status = format!("Fetching models from {}…", self.base_url.trim());
        let (tx, mut cfg) = (self.job_tx.clone(), Config::default());
        cfg.provider.base_url = self.base_url.trim().to_string();
        let (pcfg, key) = (cfg.provider.clone(), self.api_key.clone());
        std::thread::spawn(move || {
            let _ = tx.send(WizardJob::Models(
                list_models_blocking(&pcfg, &key).map_err(|e| e.to_string()),
            ));
        });
    }

    /// Apply the highlighted preset (or go to the custom URL step).
    fn choose_preset(&mut self) {
        let Some(p) = formless_core::presets::PRESETS.get(self.preset_idx) else {
            self.preset = None;
            self.step = WizardStep::BaseUrl;
            self.status = "Enter your provider base URL (any OpenAI-compatible endpoint).".into();
            return;
        };
        self.preset = Some(p);
        self.base_url = p.base_url.to_string();
        if self.saved_model.is_empty() {
            self.saved_model = p.default_model.to_string();
        }
        if p.local {
            // No key; listing models doubles as the reachability check.
            self.api_key.clear();
            self.retry_fetch = true;
            self.fetch_models();
        } else {
            self.enter_api_key_step();
        }
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
    /// Columns scrolled to the right (←/→), for wide content.
    left: usize,
    /// Highlighted lines, shown instead of `lines` while not scrolled
    /// sideways.
    styled: Vec<Line<'static>>,
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
struct App {
    paths: Paths,
    view: View,
    wizard: Option<Wizard>,
    client: Option<DaemonClient>,
    inbox: Option<mpsc::Receiver<ServerMessage>>,
    sessions: Vec<SessionInfo>,
    session_idx: usize,
    session_filter: lists::ListFilter,
    current: Option<String>,
    messages: Vec<MessageInfo>,
    streaming: String,
    /// Live reasoning for the current Run (ephemeral; never persisted).
    thinking: String,
    show_thinking: bool,
    /// Message times and day separators (`tui.timestamps`).
    show_timestamps: bool,
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
    input: input::InputLine,
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
    /// Toasts for recent notes (SPEC §10.6): how many notes have been
    /// shown, and the live ones (text, since, error).
    notes_seen: usize,
    toasts: Vec<(String, Instant, bool)>,
    pending: HashMap<u64, Pending>,
    tools: Vec<ToolInfo>,
    tool_idx: usize,
    tool_filter: lists::ListFilter,
    tool_detail: Option<ToolDetail>,
    /// Open rollback version picker (Tools view, `b`).
    rollback_pick: Option<RollbackPick>,
    /// The `/model` picker (SPEC §17).
    model_overlay: Option<ModelOverlay>,
    /// The `@` file-completion popup (SPEC §5.6).
    mention: Option<MentionPopup>,
    /// A message waiting for its `@` attachments before it is sent.
    pending_send: Option<PendingSend>,
    /// Files and pasted images for the next message, shown as chips in
    /// the input and uploaded when it is sent (SPEC §5.6).
    attachments: Vec<(String, Vec<u8>)>,
    /// The `/` palette's selected row.
    palette_sel: usize,
    /// The input the palette was dismissed (Esc) for.
    palette_hidden: Option<String>,
    /// Settings view (SPEC §10.4): selected row, an inline number edit
    /// (key, text), the key being saved and the last error by key.
    settings_sel: usize,
    settings_edit: Option<(String, input::InputLine)>,
    settings_saving: Option<String>,
    settings_error: Option<(String, String)>,
    settings_form: Option<settings::Form>,
    /// A `d` press waiting for its confirming second one (row key).
    settings_remove_arm: Option<String>,
    /// The last `ListProviders` result.
    providers_info: serde_json::Value,
    /// Redraw from scratch (after handing the terminal to $EDITOR).
    force_redraw: bool,
    /// Whether the terminal has focus (focus events; assumed until told).
    focused: bool,
    /// When each Session's current Run started, for the notify threshold.
    run_started_at: HashMap<String, Instant>,
    /// Notifications (title, body) for the run loop to write out.
    notifications: Vec<(String, String)>,
    approvals: std::collections::VecDeque<ApprovalReq>,
    /// This connection's id from the daemon, and the other clients
    /// showing the current Session (SPEC §10.4).
    conn: Option<u64>,
    others: Vec<AttachedClient>,
    /// Open `ask_user` calls. The next typed line answers the current
    /// Session's oldest one, one question at a time.
    questions: Vec<OpenQuestion>,
    /// Latest Run usage per Session, for the status line (SPEC §10.6).
    run_usage: HashMap<String, formless_protocol::UsageInfo>,
    /// The current Session's Runs, for the footer under each answer.
    session_runs: Vec<RunMetricsInfo>,
    /// Context fill per Session: (last prompt tokens, context size).
    context_fill: HashMap<String, (i64, i64)>,
    /// The current Session's unfinished last Run: (status, reason).
    run_banner: Option<(String, String)>,
    /// The last provider error in full, for `/details`.
    last_error: Option<String>,
    /// The persisted tool call `[`/`]` focused (its call id), and the
    /// calls expanded to show their input and result (SPEC §10.2).
    call_focus: Option<String>,
    expanded_calls: HashSet<String>,
    /// Scroll the chat to the focused call on the next draw.
    scroll_to_focus: bool,
    /// Wrapped row of each user message, from the last draw.
    user_rows: Vec<usize>,
    /// Ctrl+F find in this Session (SPEC §10.2).
    find: Option<FindBar>,
    /// An edit that would discard later turns waits for a second Enter.
    edit_armed: bool,
    /// Text for the input once a fork from a message opens.
    fork_prefill: Option<String>,
    /// Unsent input and attachments per Session (SPEC §10.2), kept when
    /// switching away; text drafts persist across restarts.
    drafts: HashMap<String, Draft>,
    /// Until when the input border flashes after the target Session
    /// changed (SPEC §10.2).
    target_flash: Option<Instant>,
    /// The chat's inner area in the last frame, and the OSC 8 links
    /// painted over it (SPEC §10.2).
    chat_area: Option<Rect>,
    painted_links: Vec<links::Painted>,
    hyperlinks: bool,
    /// Path words already checked against the filesystem: their
    /// `file://` target when the file exists.
    path_cache: HashMap<String, Option<String>>,
    /// Inline images (SPEC §10.2): the terminal's protocol, the
    /// thumbnails this frame places and those last sent.
    image_proto: Option<images::Protocol>,
    /// Side pane widths in the multi-pane layout, percent (left, right).
    pane_widths: (u16, u16),
    /// While an approval is open: whether keys go to the input (Tab
    /// toggles) instead of the approval (SPEC §10.3).
    input_focus: bool,
    /// What to open once the Session list arrives: "last", "new" or
    /// "picker" (`tui.on_start`, or a launch flag).
    on_start: String,
    /// The Ctrl+K Session switcher: (query, selected row).
    switcher: Option<(String, usize)>,
    /// `/quit` while Runs are working asks first (SPEC §10.5).
    quit_confirm: bool,
    /// `?` in an approval: explain each capability under it.
    approval_explain: bool,
    /// The version whose full code `v` asked for.
    approval_code: Option<i64>,
    image_placed: Vec<images::Placed>,
    painted_images: Vec<images::Placed>,
    /// Chat rows when the user scrolled away from the bottom, to count
    /// what arrived since; and where the "new lines" badge was drawn
    /// (row, first and last column) so a click can follow again.
    unseen_from: Option<usize>,
    new_badge: Option<(u16, u16, u16)>,
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
    /// The Session being renamed (the input holds its new name).
    renaming: Option<String>,
    /// The Session whose instructions the input is editing.
    instructing: Option<String>,
    /// Sessions marked with Space for a bulk action (SPEC §10.4), and a
    /// bulk action waiting for its typed input (tags, or "delete").
    marked: HashSet<String>,
    bulk: Option<(BulkAction, Vec<String>)>,
    modules: Vec<ModuleInfo>,
    model_stats: Vec<ModelStatInfo>,
    /// Run outcomes for the Metrics title (SPEC §8.7).
    run_summary: Option<formless_protocol::RunSummary>,
    /// Built-in call stats (SPEC §8.8).
    builtin_stats: Vec<formless_protocol::BuiltinStatInfo>,
    proposals: Vec<PromptProposalInfo>,
    proposal_idx: usize,
    proposal_filter: lists::ListFilter,
    editor: Option<EditorState>,
    search_hits: Vec<SearchHit>,
    search_idx: usize,
    search_mode: bool,
    editing_id: Option<String>,
    should_quit: bool,
    /// Rendered Markdown of settled messages, keyed by content and width.
    /// Rendered settled messages by (id, content length, width).
    md_cache: std::cell::RefCell<HashMap<(String, usize, usize), Vec<Line<'static>>>>,
    /// Clickable regions of the last frame (SPEC §10.5), and the last
    /// click for double-click detection.
    click_targets: std::cell::RefCell<Vec<(Rect, Click)>>,
    last_click: Option<(Click, Instant)>,
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
        theme::set(theme::from_config(&cfg.tui.theme, &cfg.tui.colors));
        markdown::set_line_numbers(cfg.tui.code_line_numbers);
        let image_proto = images::detect(&cfg.tui.inline_images);
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
            session_filter: lists::ListFilter::default(),
            current: None,
            messages: Vec::new(),
            streaming: String::new(),
            thinking: String::new(),
            show_thinking: true,
            color: colors_enabled(),
            multi_pane: cfg.tui.multi_pane,
            show_timestamps: cfg.tui.timestamps,
            hyperlinks: links::enabled(&cfg.tui.hyperlinks),
            model_label: cfg.provider.model.clone(),
            tick: 0,
            activity: None,
            live_tools: Vec::new(),
            chat_scroll_top: None,
            chat_view_height: 10,
            chat_line_count: 0,
            input: input::InputLine::default(),
            ctrl_c_at: None,
            enhanced_keys: false,
            mouse_capture: true,
            proposal_generating: false,
            running_sessions: Default::default(),
            notes: Vec::new(),
            notes_seen: 0,
            toasts: Vec::new(),
            pending: HashMap::new(),
            tools: Vec::new(),
            tool_idx: 0,
            tool_filter: lists::ListFilter::default(),
            tool_detail: None,
            rollback_pick: None,
            model_overlay: None,
            mention: None,
            pending_send: None,
            attachments: Vec::new(),
            palette_sel: 0,
            palette_hidden: None,
            settings_sel: 0,
            settings_edit: None,
            settings_saving: None,
            settings_error: None,
            settings_form: None,
            settings_remove_arm: None,
            providers_info: serde_json::Value::Null,
            force_redraw: false,
            focused: true,
            run_started_at: HashMap::new(),
            notifications: Vec::new(),
            approvals: std::collections::VecDeque::new(),
            conn: None,
            others: Vec::new(),
            questions: Vec::new(),
            run_usage: HashMap::new(),
            context_fill: HashMap::new(),
            session_runs: Vec::new(),
            run_banner: None,
            last_error: None,
            call_focus: None,
            expanded_calls: HashSet::new(),
            scroll_to_focus: false,
            user_rows: Vec::new(),
            find: None,
            edit_armed: false,
            fork_prefill: None,
            drafts: HashMap::new(),
            target_flash: None,
            chat_area: None,
            painted_links: Vec::new(),
            path_cache: HashMap::new(),
            image_proto,
            pane_widths: pane_widths(&cfg.tui.pane_widths),
            input_focus: false,
            approval_explain: false,
            quit_confirm: false,
            switcher: None,
            on_start: cfg.tui.on_start.clone(),
            approval_code: None,
            image_placed: Vec::new(),
            painted_images: Vec::new(),
            unseen_from: None,
            new_badge: None,
            invocations: Vec::new(),
            promotions: std::collections::VecDeque::new(),
            purge_arm: None,
            last_deleted: None,
            pager: None,
            delete_arm: None,
            subagents: Vec::new(),
            import_mode: None,
            renaming: None,
            instructing: None,
            marked: HashSet::new(),
            bulk: None,
            modules: Vec::new(),
            model_stats: Vec::new(),
            run_summary: None,
            builtin_stats: Vec::new(),
            proposals: Vec::new(),
            proposal_idx: 0,
            proposal_filter: lists::ListFilter::default(),
            editor: None,
            search_hits: Vec::new(),
            search_idx: 0,
            search_mode: false,
            editing_id: None,
            should_quit: false,
            md_cache: Default::default(),
            click_targets: Default::default(),
            last_click: None,
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
                            .push(format!("daemon: {}", error.clone().unwrap_or_default()));
                        if matches!(kind, Pending::MentionAttach) {
                            self.mention_attached();
                        }
                        if matches!(kind, Pending::SaveSetting) {
                            if let Some(key) = self.settings_saving.take() {
                                self.settings_error =
                                    Some((key, error.clone().unwrap_or_default()));
                            }
                        }
                        if matches!(kind, Pending::SaveProviderOrRole) {
                            if let Some(f) = &mut self.settings_form {
                                f.error = error.clone();
                            }
                        }
                        continue;
                    }
                    let Some(result) = result else { continue };
                    match kind {
                        Pending::ListSessions => {
                            if let Ok(v) = serde_json::from_value::<Vec<SessionInfo>>(result) {
                                self.sessions = v;
                                if self.current.is_none() {
                                    if self.on_start == "picker" {
                                        self.view = View::Sessions;
                                    }
                                    if let Some(first) = self.start_session() {
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
                                self.adopt_new_session(s);
                            }
                        }
                        Pending::TemplateSession => {
                            if let Ok(s) =
                                serde_json::from_value::<SessionInfo>(result["session"].clone())
                            {
                                let name = s.name.clone();
                                self.adopt_new_session(s);
                                match result["first_message"].as_str() {
                                    Some(first) => {
                                        self.input.set(first);
                                        self.notes.push(format!(
                                            "new Session from template {name}: edit the first message, then Enter"
                                        ));
                                    }
                                    None => {
                                        self.notes.push(format!("new Session from template {name}"))
                                    }
                                }
                            }
                        }
                        Pending::SaveTemplate => self.notes.push(format!(
                            "saved template {} (use it with /new {})",
                            result["name"].as_str().unwrap_or_default(),
                            result["name"].as_str().unwrap_or_default()
                        )),
                        Pending::ListTemplates => {
                            let list = result.as_array().cloned().unwrap_or_default();
                            if list.is_empty() {
                                self.notes.push(
                                    "no templates yet: /template save NAME [FIRST MESSAGE]".into(),
                                );
                            }
                            for t in list {
                                let bits: Vec<String> = [
                                    t["model"].as_str().map(|m| format!("model {m}")),
                                    t["reasoning_effort"]
                                        .as_str()
                                        .map(|e| format!("effort {e}")),
                                    t["cwd"].as_str().map(|c| format!("in {c}")),
                                    t["instructions"]
                                        .as_str()
                                        .map(|_| "instructions".to_string()),
                                    t["first_message"]
                                        .as_str()
                                        .map(|m| format!("“{}”", truncate_chars(m, 40))),
                                ]
                                .into_iter()
                                .flatten()
                                .collect();
                                self.notes.push(format!(
                                    "template {}: {}",
                                    t["name"].as_str().unwrap_or_default(),
                                    if bits.is_empty() {
                                        "defaults".into()
                                    } else {
                                        bits.join(" · ")
                                    }
                                ));
                            }
                        }
                        Pending::ForkSession => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.switch_session(s.id.clone());
                                self.sessions.insert(0, s.clone());
                                self.request(Pending::GetSession, Request::GetSession { id: s.id });
                                self.view = View::Chat;
                                self.notes.push("session forked".into());
                                if let Some(text) = self.fork_prefill.take() {
                                    self.input.set(&text);
                                    self.notes.push(
                                        "the message is in the input: edit it, then Enter".into(),
                                    );
                                }
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
                            if let Some(t) = self.selected_tool().cloned() {
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
                        Pending::SettingsProviders => self.providers_info = result,
                        Pending::SetTags => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.notes.push(if s.tags.is_empty() {
                                    format!("{} has no tags", s.name)
                                } else {
                                    format!("{} tags: #{}", s.name, s.tags.join(" #"))
                                });
                                if let Some(old) = self.sessions.iter_mut().find(|x| x.id == s.id) {
                                    *old = s;
                                }
                            }
                        }
                        Pending::SetArchive => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.notes.push(format!(
                                    "{} {}",
                                    if s.archived {
                                        "archived (v shows archived Sessions)"
                                    } else {
                                        "restored"
                                    },
                                    s.name
                                ));
                                if let Some(old) = self.sessions.iter_mut().find(|x| x.id == s.id) {
                                    *old = s;
                                }
                            }
                        }
                        Pending::SetPin => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.notes.push(format!(
                                    "{} {}",
                                    if s.pinned { "pinned" } else { "unpinned" },
                                    s.name
                                ));
                                if let Some(old) = self.sessions.iter_mut().find(|x| x.id == s.id) {
                                    *old = s;
                                }
                            }
                        }
                        Pending::SetCwd => {
                            if let Ok(s) = serde_json::from_value::<SessionInfo>(result) {
                                self.notes.push(format!(
                                    "working directory: {}",
                                    s.cwd.as_deref().unwrap_or("(the daemon's)")
                                ));
                                if let Some(old) = self.sessions.iter_mut().find(|x| x.id == s.id) {
                                    *old = s;
                                }
                            }
                        }
                        Pending::SessionRuns => {
                            let runs: Vec<RunMetricsInfo> =
                                serde_json::from_value(result["runs"].clone()).unwrap_or_default();
                            if runs.first().map(|r| &r.session_id) == self.current.as_ref() {
                                self.session_runs = runs;
                            }
                        }
                        Pending::ApprovalCode => {
                            if let Ok(d) = serde_json::from_value::<ToolDetail>(result) {
                                let want = self.approval_code.take();
                                let v = d
                                    .versions
                                    .iter()
                                    .find(|v| Some(v.version) == want)
                                    .or(d.versions.last());
                                if let Some(v) = v {
                                    self.pager = Some(code_pager(
                                        &format!("{} v{} code", d.tool.name, v.version),
                                        &v.code,
                                        self.color,
                                    ));
                                }
                            }
                        }
                        Pending::Subscribed => {
                            // The client list can arrive before this reply.
                            self.conn = result["conn"].as_u64();
                            let me = self.conn;
                            self.others.retain(|c| Some(c.conn) != me);
                        }
                        Pending::SuggestName => {
                            match (self.current.clone(), result["name"].as_str()) {
                                (Some(id), Some(name)) => {
                                    self.start_rename(&id, name);
                                    self.notes.push(
                                        "suggested name: Enter to use it, edit it first, or Esc"
                                            .into(),
                                    );
                                }
                                _ => self.notes.push("no name to suggest yet".into()),
                            }
                        }
                        Pending::ContextFill | Pending::ContextReport => {
                            let used = result["used_tokens"].as_i64().unwrap_or(0);
                            let size = result["context_tokens"].as_i64().unwrap_or(0);
                            if let Some(id) = self.current.clone().filter(|_| size > 0) {
                                self.context_fill.insert(id, (used, size));
                            }
                            if matches!(kind, Pending::ContextReport) {
                                let b = &result["breakdown"];
                                let pct = (used * 100).checked_div(size).unwrap_or(0);
                                self.notes.push(format!(
                                    "context: {used} of {size} tokens ({pct}%{}) on {}",
                                    if result["measured"] == true {
                                        ""
                                    } else {
                                        ", estimated"
                                    },
                                    result["model"].as_str().unwrap_or_default()
                                ));
                                self.notes.push(format!(
                                    "  ≈ system prompt {} · Tool schemas {} · history {} tokens · {} attachment(s)",
                                    b["system_prompt"], b["tool_schemas"], b["history"], b["attachments"]
                                ));
                            }
                        }
                        Pending::SaveSetting => {
                            if let Some(key) = self.settings_saving.take() {
                                self.notes.push(format!(
                                    "saved {key} = {} (used from the next Run)",
                                    result["value"]
                                ));
                                if self.settings_error.as_ref().is_some_and(|(k, _)| *k == key) {
                                    self.settings_error = None;
                                }
                            }
                        }
                        Pending::SaveProviderOrRole => {
                            if let Some(f) = self.settings_form.take() {
                                self.notes.push(format!("{} saved", f.title));
                            }
                            self.request(Pending::SettingsProviders, Request::ListProviders);
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
                                left: 0,
                                styled: Vec::new(),
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
                            self.input.set(result["text"].as_str().unwrap_or_default());
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
                            // Names may hold path separators.
                            let label = label.replace(['/', '\\'], "-");
                            // A readable transcript, or the JSON bundle.
                            let (ext, body) =
                                match (result["format"].as_str(), result["content"].as_str()) {
                                    (Some("markdown"), Some(c)) => ("md", c.to_string()),
                                    (Some("html"), Some(c)) => ("html", c.to_string()),
                                    _ => (
                                        "json",
                                        serde_json::to_string_pretty(&result).unwrap_or_default(),
                                    ),
                                };
                            let file = dir.join(format!("{label}.{ext}"));
                            match std::fs::write(&file, body) {
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
        self.others.clear();
        self.request(
            Pending::Subscribed,
            Request::Subscribe {
                sessions: Some(vec![id.clone()]),
                deltas: true,
                client: Some("TUI".into()),
            },
        );
        self.request(
            Pending::ContextFill,
            Request::ContextUsage {
                session_id: id.clone(),
            },
        );
        self.session_runs.clear();
        self.request_session_runs(&id);
        // Each Session keeps its own unsent input (SPEC §10.2).
        if self.current.as_deref() != Some(id.as_str()) {
            if let Some(old) = self.current.take() {
                let draft = Draft {
                    input: std::mem::take(&mut self.input),
                    attachments: std::mem::take(&mut self.attachments),
                };
                if !draft.input.is_empty() || !draft.attachments.is_empty() {
                    self.drafts.insert(old, draft);
                }
            }
            let draft = self.drafts.remove(&id).unwrap_or_default();
            self.input = draft.input;
            self.attachments = draft.attachments;
            self.target_flash = Some(Instant::now() + Duration::from_millis(700));
        }
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
        self.maybe_notify(&ev);
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
                if self.is_current(&session_id) {
                    self.request_session_runs(&session_id);
                }
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
                session_id,
                error,
                explained,
                ..
            } => {
                self.running_sessions.remove(&session_id);
                if !self.is_current(&session_id) {
                    return;
                }
                self.activity = None;
                self.last_error = Some(error.clone());
                match explained {
                    Some(x) => {
                        self.notes.push(format!("run paused: {}", x.message));
                        if let Some(action) = &x.action {
                            self.notes
                                .push(format!("→ {action} · /details shows the raw error"));
                        }
                        self.run_banner = Some(("paused".into(), x.message));
                    }
                    None => {
                        self.notes.push(format!("run paused: {error}"));
                        self.run_banner = Some(("paused".into(), error));
                    }
                }
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
                    theme::set(theme::from_config(&cfg.tui.theme, &cfg.tui.colors));
                    markdown::set_line_numbers(cfg.tui.code_line_numbers);
                    self.image_proto = images::detect(&cfg.tui.inline_images);
                    // Rendered answers carry the old colours and options.
                    self.md_cache.borrow_mut().clear();
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
                let open = OpenQuestion::new(question_id, session_id, questions);
                self.notes.push(open.prompt());
                if self.is_current(&open.session_id) {
                    self.set_activity("awaiting your answer");
                }
                self.questions.push(open);
            }
            Event::RunUsage {
                session_id, usage, ..
            } => {
                if usage.context_tokens > 0 {
                    self.context_fill.insert(
                        session_id.clone(),
                        (usage.context_used, usage.context_tokens),
                    );
                }
                self.run_usage.insert(session_id, usage);
            }
            Event::UserQuestionClosed { question_id } => {
                self.questions.retain(|q| q.id != question_id);
            }
            Event::SessionClients {
                session_id,
                clients,
            } => {
                if self.current.as_deref() == Some(&session_id) {
                    self.others = clients
                        .into_iter()
                        .filter(|c| Some(c.conn) != self.conn)
                        .collect();
                }
            }
            Event::ApprovalAnswered {
                approval_id,
                approved,
                by,
            } => {
                // Answered by another client: close ours and say where.
                let before = self.approvals.len();
                self.approvals.retain(|a| a.approval_id != approval_id);
                if self.approvals.len() < before {
                    self.notes.push(format!(
                        "approval {} in {}",
                        if approved { "granted" } else { "denied" },
                        by.as_deref().unwrap_or("another client")
                    ));
                }
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
                tool_id,
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
                    tool_id,
                    session_id: Some(session_id),
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
                    tool_id: None,
                    session_id: None,
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
        let text = self.input.take();
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
        if let Some((action, ids)) = self.bulk.take() {
            self.finish_bulk(action, ids, &text);
            return;
        }
        if let Some(id) = self.instructing.take() {
            self.set_instructions(id, &text);
            return;
        }
        if let Some(id) = self.renaming.take() {
            let name = text.trim().to_string();
            if !name.is_empty() {
                self.fire(Request::RenameSession { id, name });
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
            let answer = self.questions[i].current().parse_reply(&text);
            self.answer_question(i, answer);
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
        let root = Some(self.work_dir());
        let found: Vec<String> = formless_core::files::mentions(&text)
            .into_iter()
            .filter(|p| root.as_ref().is_some_and(|r| r.join(p).exists()))
            .collect();
        let held = std::mem::take(&mut self.attachments);
        for (name, bytes) in &held {
            use base64::Engine;
            self.request(
                Pending::MentionAttach,
                Request::AttachFile {
                    session_id: session_id.clone(),
                    name: name.clone(),
                    data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    mime: None,
                },
            );
        }
        if found.is_empty() && held.is_empty() {
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
            waiting: found.len() + held.len(),
        });
    }

    /// Hold a file for the next message (a chip in the input).
    fn add_attachment(&mut self, name: String, bytes: Vec<u8>) {
        if bytes.len() > 20 * 1024 * 1024 {
            self.notes.push(format!("{name} is larger than 20 MB"));
            return;
        }
        let image = formless_core::daemon::guess_mime(&name).starts_with("image/");
        self.notes.push(format!(
            "attached {name} ({}); it goes with your next message · Backspace on an empty input removes it",
            size_text(bytes.len())
        ));
        if image && !self.vision_model() {
            self.notes.push(
                "the current model may not read images (set provider.vision = true if it does); it will get a reference instead"
                    .into(),
            );
        }
        self.attachments.push((name, bytes));
    }

    fn attach_path_file(&mut self, path: &std::path::Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "attachment".into());
                self.add_attachment(name, bytes);
            }
            Err(e) => self
                .notes
                .push(format!("cannot attach {}: {e}", path.display())),
        }
    }

    /// `/paste` and Ctrl+V: an image from the system clipboard.
    fn paste_image(&mut self) {
        match clipimg::read_png() {
            Ok(png) => {
                let n = self
                    .attachments
                    .iter()
                    .filter(|(n, _)| n.starts_with("pasted-image"))
                    .count()
                    + 1;
                self.add_attachment(format!("pasted-image-{n}.png"), png);
            }
            Err(e) => self.notes.push(e),
        }
    }

    /// Whether the current model takes images (`provider.vision`, else a
    /// guess from its name).
    fn vision_model(&self) -> bool {
        let Ok(cfg) = Config::load(&self.paths) else {
            return true;
        };
        let model = self
            .current
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
            .and_then(|s| s.model.clone())
            .unwrap_or(cfg.provider.model);
        cfg.provider
            .vision
            .unwrap_or_else(|| formless_core::provider::is_vision_model(&model))
    }

    /// `/compact [focus]`: summarise the Session now (SPEC §11).
    fn compact_now(&mut self, focus: Option<String>) {
        let Some(session_id) = self.current.clone() else {
            self.notes.push("no current session".into());
            return;
        };
        self.request(
            Pending::RunControl,
            Request::CompactSession { session_id, focus },
        );
        self.notes
            .push("compacting: a summary will replace the history so far…".into());
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

    /// Record an answer to open question `i`'s current question; send
    /// them all after the last one.
    fn answer_question(&mut self, i: usize, answer: QuestionAnswer) {
        let open = &mut self.questions[i];
        self.notes.push(format!("answered: {}", answer.summary()));
        open.answers.push(answer);
        if open.answers.len() < open.questions.len() {
            open.reset_choice();
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
    }

    fn session_name(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Session".into())
    }

    /// Queue a notification for events worth interrupting the user for,
    /// while the terminal isn't focused (SPEC §10.6).
    fn maybe_notify(&mut self, ev: &Event) {
        if let Event::RunStarted { session_id, .. } = ev {
            self.run_started_at
                .insert(session_id.clone(), Instant::now());
        }
        let elapsed = |sid: &str| {
            self.run_started_at
                .get(sid)
                .map(|t| t.elapsed())
                .unwrap_or_default()
        };
        let min = Config::load(&self.paths)
            .map(|c| c.tui.notify_min_secs)
            .unwrap_or(20);
        let note = match ev {
            Event::RunFinished {
                session_id, status, ..
            } => {
                let took = elapsed(session_id);
                self.run_started_at.remove(session_id);
                (took.as_secs() >= min && status != "interrupted").then(|| {
                    (
                        format!("Run {status}"),
                        format!(
                            "{} · {}",
                            self.session_name(session_id),
                            format_elapsed(took)
                        ),
                    )
                })
            }
            Event::RunPaused {
                session_id,
                error,
                explained,
                ..
            } => {
                self.run_started_at.remove(session_id);
                let what = explained.as_ref().map_or(error, |x| &x.message);
                Some((
                    "Run paused".into(),
                    format!(
                        "{}: {}",
                        self.session_name(session_id),
                        truncate_chars(what, 120)
                    ),
                ))
            }
            Event::ProviderStalled {
                session_id,
                idle_secs,
                ..
            } => Some((
                "Run stalled".into(),
                format!(
                    "{}: no reply for {idle_secs}s (/resend)",
                    self.session_name(session_id)
                ),
            )),
            Event::ApprovalRequested {
                session_id,
                tool_name,
                ..
            } => Some((
                "Approval needed".into(),
                format!("{}: {tool_name}", self.session_name(session_id)),
            )),
            Event::UserQuestion {
                session_id,
                question,
                questions,
                ..
            } => Some((
                "Question for you".into(),
                format!(
                    "{}: {}",
                    self.session_name(session_id),
                    truncate_chars(questions.first().map_or(question, |q| &q.question), 120)
                ),
            )),
            _ => None,
        };
        if let Some(n) = note.filter(|_| !self.focused) {
            self.notifications.push(n);
        }
    }

    /// The open question of the current Session, if any.
    fn current_question(&self) -> Option<usize> {
        let sid = self.current.as_ref()?;
        self.questions.iter().position(|q| &q.session_id == sid)
    }

    /// Keys for the question panel (SPEC §5.5): ↑/↓ move, Space ticks,
    /// Enter answers, 1–9 pick, Shift+Tab goes back. With text typed (or
    /// on "Other…") keys go to the input and Enter sends it as the answer.
    fn question_key(&mut self, key: &KeyEvent) -> bool {
        let Some(i) = self.current_question() else {
            return false;
        };
        let typing = !self.input.is_empty();
        let open = &mut self.questions[i];
        let rows = open.current().options.len() + 1;
        let multi = open.current().multi_select;
        match key.code {
            KeyCode::BackTab => {
                if open.answers.pop().is_some() {
                    open.reset_choice();
                    self.notes.push("back to the previous question".into());
                }
            }
            KeyCode::Up if !typing => open.cursor = open.cursor.saturating_sub(1),
            KeyCode::Down if !typing => open.cursor = (open.cursor + 1).min(rows - 1),
            KeyCode::Char(' ') if !typing && multi && !open.on_other() => {
                let c = open.cursor;
                open.checked[c] = !open.checked[c];
            }
            KeyCode::Char(d @ '1'..='9') if !typing && !open.on_other() => {
                let n = d as usize - '1' as usize;
                if n >= rows - 1 {
                    return false;
                }
                open.cursor = n;
                if multi {
                    open.checked[n] = !open.checked[n];
                } else if let Some(answer) = open.picked() {
                    self.answer_question(i, answer);
                }
            }
            KeyCode::Enter if !typing && key.modifiers.is_empty() => match open.picked() {
                Some(answer) => self.answer_question(i, answer),
                None => self.notes.push("type your answer, then Enter".into()),
            },
            _ => return false,
        }
        true
    }

    /// The `/` palette rows for the input, empty when it isn't shown.
    fn palette_items(&self) -> Vec<PaletteItem> {
        let Some(q) = self.input.strip_prefix('/') else {
            return Vec::new();
        };
        if self.palette_hidden.as_deref() == Some(&*self.input)
            || self.search_mode
            || self.renaming.is_some()
            || self.instructing.is_some()
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
            KeyCode::Esc => self.palette_hidden = Some(self.input.to_string()),
            KeyCode::Tab if key.modifiers.is_empty() => {
                self.input.set(&items[sel].insert);
                self.palette_sel = 0;
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let item = &items[sel];
                if item.runnable {
                    self.input.set(item.insert.trim_end());
                    self.palette_sel = 0;
                    self.submit_chat(false);
                } else {
                    self.notes.push(format!("usage: {}", item.label));
                    self.input.set(&item.insert);
                    self.palette_sel = 0;
                }
            }
            _ => return false,
        }
        true
    }

    /// Open, refresh or close the `@` popup for the input's last word.
    fn update_mention(&mut self) {
        let word = self
            .input
            .before_cursor()
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or("");
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
        let root = Some(self.work_dir().to_string_lossy().into_owned());
        self.request(
            Pending::ListFiles,
            Request::ListFiles {
                root,
                query,
                limit: Some(8),
                session_id: self.current.clone(),
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
                let word_chars = self
                    .input
                    .before_cursor()
                    .rsplit(char::is_whitespace)
                    .next()
                    .unwrap_or("")
                    .chars()
                    .count();
                for _ in 0..word_chars {
                    self.input.backspace();
                }
                self.input
                    .insert_str(&format!("@{}{}", f.path, if f.dir { '/' } else { ' ' }));
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
                    Request::RunMetrics {
                        limit: Some(200),
                        session_id: None,
                    },
                );
                self.request(Pending::ListBuiltinStats, Request::ListBuiltinStats);
            }
            View::Proposals => self.request(Pending::ListProposals, Request::ListProposals),
            View::Settings => self.request(Pending::SettingsProviders, Request::ListProviders),
            View::Chat | View::Search => {}
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
            cmd if cmd.starts_with("new ") => self.request(
                Pending::TemplateSession,
                Request::CreateSessionFromTemplate {
                    template: cmd[4..].trim().to_string(),
                    name: None,
                },
            ),
            "templates" | "template" | "template list" => {
                self.request(Pending::ListTemplates, Request::ListSessionTemplates)
            }
            cmd if cmd == "export" || cmd.starts_with("export ") => match self.current.clone() {
                Some(id) => {
                    let format = Some(cmd[6..].trim().to_string()).filter(|f| !f.is_empty());
                    self.request(
                        Pending::ExportSession,
                        Request::ExportSession { id, format },
                    );
                }
                None => self.notes.push("no current session".into()),
            },
            cmd if cmd.starts_with("template ") => self.template_cmd(cmd[9..].trim()),
            "search" => {
                self.search_mode = true;
                self.notes
                    .push("Type a search query, then Enter (searches all sessions).".into());
            }
            "edit" | "retry" => match self.messages.iter().rev().find(|m| m.role == "user") {
                Some(m) => {
                    self.editing_id = Some(m.id.clone());
                    self.input.set(&m.content);
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
            cmd if cmd.starts_with("panes ") => self.resize_panes(cmd["panes ".len()..].trim()),
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
            "open diagram" => self.open_diagram("", true),
            cmd if cmd.starts_with("open diagram ") => {
                self.open_diagram(cmd["open diagram ".len()..].trim(), true)
            }
            "open link" => self.open_link(""),
            cmd if cmd.starts_with("open path ") => {
                self.open_path(cmd["open path ".len()..].trim())
            }
            cmd if cmd.starts_with("open link ") => {
                self.open_link(cmd["open link ".len()..].trim())
            }
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
                self.attach_path_file(std::path::Path::new(&path));
            }
            "paste" => self.paste_image(),
            "tag" | "tags" => self.tag_cmd("", true),
            cmd if cmd.starts_with("tag ") => self.tag_cmd(cmd[4..].trim(), true),
            cmd if cmd.starts_with("untag ") => self.tag_cmd(cmd[6..].trim(), false),
            "pin" => match self
                .current
                .as_ref()
                .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
                .cloned()
            {
                Some(s) => self.toggle_pin(&s),
                None => self.notes.push("no current session".into()),
            },
            "cd" => self.change_dir(""),
            cmd if cmd.starts_with("cd ") => self.change_dir(cmd[3..].trim()),
            "notes" => {
                self.pager = Some(Pager {
                    title: format!("notes ({})", self.notes.len()),
                    lines: self.notes.clone(),
                    top: self.notes.len().saturating_sub(20),
                    left: 0,
                    styled: Vec::new(),
                })
            }
            "pager code" => self.open_code(""),
            cmd if cmd.starts_with("pager code ") => {
                self.open_code(cmd["pager code ".len()..].trim())
            }
            "pager diagram" => self.open_diagram("", false),
            cmd if cmd.starts_with("pager diagram ") => {
                self.open_diagram(cmd["pager diagram ".len()..].trim(), false)
            }
            "table" => self.open_table(""),
            cmd if cmd.starts_with("table ") => self.open_table(cmd["table ".len()..].trim()),
            "compact" => self.compact_now(None),
            cmd if cmd.starts_with("compact ") => {
                let focus = cmd["compact ".len()..].trim().to_string();
                self.compact_now(Some(focus));
            }
            "timestamps" => {
                self.show_timestamps = !self.show_timestamps;
                self.fire(Request::SetSetting {
                    key: "tui.timestamps".into(),
                    value: serde_json::json!(self.show_timestamps),
                });
                self.notes.push(format!(
                    "timestamps {} (remembered)",
                    if self.show_timestamps { "on" } else { "off" }
                ));
            }
            cmd if cmd == "instructions" || cmd.starts_with("instructions ") => {
                self.instructions_cmd(cmd[12..].trim())
            }
            cmd if cmd == "rename" || cmd.starts_with("rename ") => {
                self.rename_cmd(cmd[6..].trim())
            }
            "context" => match self.current.clone() {
                Some(session_id) => {
                    self.request(Pending::ContextReport, Request::ContextUsage { session_id })
                }
                None => self.notes.push("no current session".into()),
            },
            "details" => match self.last_error.clone() {
                Some(raw) => {
                    self.pager = Some(Pager {
                        title: "Last error".into(),
                        lines: raw.lines().map(str::to_string).collect(),
                        top: 0,
                        left: 0,
                        styled: Vec::new(),
                    })
                }
                None => self.notes.push("no error to show".into()),
            },
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
                    left: 0,
                    styled: Vec::new(),
                })
            }
            "quit" if !self.running_sessions.is_empty() => self.quit_confirm = true,
            "quit" => self.should_quit = true,
            other => self.notes.push(match commands::closest(other) {
                Some(name) => format!("unknown command: /{other} — did you mean /{name}?"),
                None => format!("unknown command: /{other} (/help lists them)"),
            }),
        }
    }

    fn open_model_picker(&mut self) {
        let effort = if self.view == View::Settings {
            Config::load(&self.paths)
                .ok()
                .and_then(|c| c.provider.reasoning_effort)
        } else {
            self.current
                .as_ref()
                .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
                .and_then(|s| s.reasoning_effort.clone())
        };
        let effort_idx = EFFORTS
            .iter()
            .position(|e| e.map(str::to_string) == effort)
            .unwrap_or(0);
        self.model_overlay = Some(ModelOverlay {
            picker: picker::ModelPicker::loading(),
            effort_idx,
            default_only: self.view == View::Settings,
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
        let global = global || (o.default_only && key.code == KeyCode::Enter);
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
                    WizardJob::Models(Err(e)) if w.retry_fetch => {
                        w.step = WizardStep::Failed;
                        w.status = format!(
                            "Can't reach {} ({e}). Start the server, then press Enter to retry (Esc for another provider).",
                            w.base_url.trim()
                        );
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
        if let Some(p) = w.preset {
            cfg.provider.api_style = p.api_style.to_string();
        }
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
        let presets = formless_core::presets::PRESETS.len();
        match w.step {
            WizardStep::PickPreset => match key.code {
                KeyCode::Up => w.preset_idx = w.preset_idx.saturating_sub(1),
                KeyCode::Down => w.preset_idx = (w.preset_idx + 1).min(presets),
                KeyCode::Char(d @ '1'..='9') => {
                    let n = d as usize - '1' as usize;
                    if n <= presets {
                        w.preset_idx = n;
                        w.choose_preset();
                    }
                }
                KeyCode::Enter => w.choose_preset(),
                _ => {}
            },
            WizardStep::BaseUrl => match key.code {
                KeyCode::Esc => {
                    w.step = WizardStep::PickPreset;
                    w.status = "Choose your model provider.".into();
                }
                KeyCode::Enter if !w.base_url.trim().is_empty() => w.enter_api_key_step(),
                KeyCode::Char(c) => w.base_url.push(c),
                KeyCode::Backspace => {
                    w.base_url.pop();
                }
                _ => {}
            },
            WizardStep::ApiKey => match key.code {
                KeyCode::Esc => {
                    w.step = WizardStep::PickPreset;
                    w.status = "Choose your model provider.".into();
                }
                KeyCode::Enter => {
                    if w.api_key.is_empty() {
                        if let Some((_, k)) = w.preset.and_then(formless_core::presets::env_key) {
                            w.api_key = k;
                        }
                    }
                    w.retry_fetch = false;
                    w.fetch_models();
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
            WizardStep::Failed if w.retry_fetch && key.code == KeyCode::Esc => {
                w.retry_fetch = false;
                w.step = WizardStep::PickPreset;
                w.status = "Choose your model provider.".into();
            }
            WizardStep::Failed if w.retry_fetch && key.code == KeyCode::Enter => w.fetch_models(),
            WizardStep::Failed if key.code == KeyCode::Enter => {
                w.step = WizardStep::PickModel;
                w.status = "Pick a model.".into();
            }
            _ => {}
        }
    }

    fn mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        if self.wizard.is_some() {
            return;
        }
        if self.view != View::Chat && !matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll_chat_up(3),
            MouseEventKind::ScrollDown => self.scroll_chat_down(3),
            MouseEventKind::Down(MouseButton::Left) => {
                // The "↓ N new lines" badge follows the bottom again.
                if let Some((row, x0, x1)) = self.new_badge {
                    if m.row == row && (x0..=x1).contains(&m.column) {
                        self.follow_chat_bottom();
                        return;
                    }
                }
                let hit = self
                    .click_targets
                    .borrow()
                    .iter()
                    .find(|(r, _)| {
                        (r.x..r.x + r.width).contains(&m.column)
                            && (r.y..r.y + r.height).contains(&m.row)
                    })
                    .map(|(_, c)| c.clone());
                if let Some(c) = hit {
                    self.click(c);
                }
            }
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
                KeyCode::Right | KeyCode::Char('l') => p.left += 8,
                KeyCode::Left | KeyCode::Char('h') => p.left = p.left.saturating_sub(8),
                _ => {}
            }
            return;
        }
        if self.wizard.is_some() {
            self.wizard_key(key);
            return;
        }
        if key.code == KeyCode::Char('k')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.editor.is_none()
        {
            self.switcher = Some((String::new(), 0));
            return;
        }
        if self.switcher.is_some() {
            self.switcher_key(key);
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
        if self.quit_confirm {
            match key.code {
                KeyCode::Char('q') | KeyCode::Enter => self.should_quit = true,
                KeyCode::Char('s') => {
                    for session_id in self.running_sessions.clone() {
                        self.fire(Request::InterruptRun { session_id });
                    }
                    self.should_quit = true;
                }
                KeyCode::Esc | KeyCode::Char('c') => self.quit_confirm = false,
                _ => {}
            }
            return;
        }
        let modal_open = !self.approvals.is_empty() || !self.promotions.is_empty();
        if !modal_open {
            self.input_focus = false;
        }
        // Tab moves focus between an open approval and the input; the
        // approval stays on screen either way.
        if modal_open && key.code == KeyCode::Tab && self.view == View::Chat {
            self.input_focus = !self.input_focus;
            return;
        }
        let modal_keys = modal_open && !self.input_focus;
        if let KeyCode::Char(c) = key.code {
            // Typing anything else while the approval has focus moves
            // focus to the input (and types it).
            if modal_keys
                && !matches!(c, 'y' | 'n' | 'o' | 'v' | '?')
                && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
            {
                self.input_focus = true;
            }
        }
        let modal_keys = modal_keys && !self.input_focus;
        if let (true, KeyCode::Char(c)) = (modal_keys, key.code) {
            if c == '?' {
                self.approval_explain = !self.approval_explain;
                return;
            }
            if c == 'v' {
                let req = self.approvals.front().or(self.promotions.front()).cloned();
                match req.and_then(|r| r.tool_id.map(|id| (id, r.version))) {
                    Some((id, version)) => {
                        self.approval_code = Some(version);
                        self.request(Pending::ApprovalCode, Request::GetTool { id });
                    }
                    None => self.notes.push("this approval has no code to show".into()),
                }
                return;
            }
            if c == 'o' && !self.approvals.is_empty() {
                let req = self.approvals.pop_front().unwrap();
                self.fire(Request::ApprovalDecision {
                    approval_id: req.approval_id,
                    approved: true,
                    scope: ApprovalScope::Once,
                });
                self.notes.push(format!("approved {} once", req.tool_name));
                return;
            }
            if c == 'y' || c == 'n' {
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
        if self.view == View::Chat
            && (self.question_key(&key) || self.palette_key(&key) || self.mention_key(&key))
        {
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
        if self.list_filter_key(key.code) || self.list_nav_key(&key) {
            return;
        }
        match self.view {
            View::Settings => self.settings_key(key),
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
                    self.proposal_idx = (self.proposal_idx + 1)
                        .min(self.visible_proposals().len().saturating_sub(1))
                }
                KeyCode::Char('n') => self.generate_proposal(),
                KeyCode::Char('a') | KeyCode::Enter => {
                    if let Some(p) = self.selected_proposal().cloned() {
                        self.fire(Request::ProposalDecision {
                            id: p.id,
                            approved: true,
                        });
                    }
                }
                KeyCode::Char('r') => {
                    if let Some(p) = self.selected_proposal().cloned() {
                        self.fire(Request::ProposalDecision {
                            id: p.id,
                            approved: false,
                        });
                    }
                }
                KeyCode::Char('b') => {
                    if let Some(p) = self.selected_proposal().cloned() {
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
                        (self.session_idx + 1).min(self.visible_sessions().len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(s) = self.selected_session().cloned() {
                        self.switch_session(s.id.clone());
                        self.request(Pending::GetSession, Request::GetSession { id: s.id });
                        self.view = View::Chat;
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(s) = self.selected_session().cloned() {
                        if !self.marked.remove(&s.id) {
                            self.marked.insert(s.id);
                        }
                    }
                }
                KeyCode::Char('d' | 'a' | 'e' | 't') if !self.marked.is_empty() => {
                    let ids: Vec<String> = self
                        .sessions
                        .iter()
                        .filter(|s| self.marked.contains(&s.id))
                        .map(|s| s.id.clone())
                        .collect();
                    self.bulk_key(key.code, ids);
                }
                KeyCode::Esc if !self.marked.is_empty() => self.marked.clear(),
                KeyCode::Char('n') => self.request(
                    Pending::CreateSession,
                    Request::CreateSession { name: None },
                ),
                KeyCode::Char('P') => {
                    if let Some(s) = self.selected_session().cloned() {
                        self.toggle_pin(&s);
                    }
                }
                // Undo the last delete (within `daemon.delete_undo_secs`).
                KeyCode::Char('u') => self.slash("undo"),
                KeyCode::Char('a') => {
                    if let Some(s) = self.selected_session().cloned() {
                        self.request(
                            Pending::SetArchive,
                            Request::SetSessionArchived {
                                id: s.id,
                                archived: !s.archived,
                            },
                        );
                    }
                }
                KeyCode::Char('f') => {
                    if let Some(s) = self.selected_session().cloned() {
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
                    if let Some(s) = self.selected_session().cloned() {
                        if self.delete_arm.as_deref() == Some(&s.id) {
                            self.delete_arm = None;
                            let secs = Config::load(&self.paths)
                                .map(|c| c.daemon.delete_undo_secs)
                                .unwrap_or(30);
                            self.notes.push(format!(
                                "deleted session {} · u (or /undo) restores it for {secs}s",
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
                    if let Some(s) = self.selected_session().cloned() {
                        self.request(
                            Pending::ExportSession,
                            Request::ExportSession {
                                id: s.id,
                                format: None,
                            },
                        );
                    }
                }
                KeyCode::Char('i') => {
                    self.import_mode = Some(ImportKind::Session);
                    self.view = View::Chat;
                    self.notes
                        .push("Type a session bundle file path, then Enter.".into());
                }
                KeyCode::Char('r') => {
                    if let Some(s) = self.selected_session().cloned() {
                        self.start_rename(&s.id, &s.name);
                    }
                }
                KeyCode::Esc => self.view = View::Chat,
                _ => {}
            },
            View::Tools => match key.code {
                KeyCode::Up => self.tool_idx = self.tool_idx.saturating_sub(1),
                KeyCode::Down => {
                    self.tool_idx =
                        (self.tool_idx + 1).min(self.visible_tools().len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(tool) = self.selected_tool().cloned() {
                        self.request(Pending::GetTool, Request::GetTool { id: tool.id });
                    }
                }
                KeyCode::Char('e') => {
                    if let Some(tool) = self.selected_tool().cloned() {
                        self.request(Pending::ExportTool, Request::ExportTool { id: tool.id });
                    }
                }
                KeyCode::Char('E') => {
                    if let Some(tool) = self.selected_tool().cloned() {
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
                    let tool_id = self.selected_tool().map(|t| t.id.clone());
                    self.fire(Request::RunOptimiser { tool_id });
                    self.notes.push("optimiser started".into());
                }
                KeyCode::Char('x') => {
                    if let Some(tool) = self.selected_tool().cloned() {
                        if tool.retired {
                            self.fire(Request::RestoreTool { id: tool.id });
                        } else {
                            self.fire(Request::RetireTool { id: tool.id });
                        }
                    }
                }
                KeyCode::Char('b') => {
                    if let Some(tool) = self.selected_tool().cloned() {
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
                    if let Some(tool) = self.selected_tool().cloned() {
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
            _ => self.input_key(key),
        }
    }

    fn visible_tools(&self) -> Vec<usize> {
        lists::tools(&self.tools, &self.tool_filter)
    }

    fn visible_sessions(&self) -> Vec<usize> {
        lists::sessions(&self.sessions, &self.running_sessions, &self.session_filter)
    }

    fn visible_proposals(&self) -> Vec<usize> {
        lists::proposals(&self.proposals, &self.proposal_filter)
    }

    fn selected_tool(&self) -> Option<&ToolInfo> {
        let v = self.visible_tools();
        v.get(self.tool_idx.min(v.len().saturating_sub(1)))
            .map(|&i| &self.tools[i])
    }

    fn selected_session(&self) -> Option<&SessionInfo> {
        let v = self.visible_sessions();
        v.get(self.session_idx.min(v.len().saturating_sub(1)))
            .map(|&i| &self.sessions[i])
    }

    fn selected_proposal(&self) -> Option<&PromptProposalInfo> {
        let v = self.visible_proposals();
        v.get(self.proposal_idx.min(v.len().saturating_sub(1)))
            .map(|&i| &self.proposals[i])
    }

    /// Vim-style movement in every list view (SPEC §10.5): j/k, g/G (or
    /// Home/End) and Ctrl+D/Ctrl+U by half a page; true when used.
    fn list_nav_key(&mut self, key: &KeyEvent) -> bool {
        let len = match self.view {
            View::Sessions => self.visible_sessions().len(),
            View::Tools | View::Metrics => self.visible_tools().len(),
            View::Proposals => self.visible_proposals().len(),
            _ => return false,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let idx = match self.view {
            View::Sessions => &mut self.session_idx,
            View::Proposals => &mut self.proposal_idx,
            _ => &mut self.tool_idx,
        };
        let last = len.saturating_sub(1);
        let half = 10;
        *idx = match key.code {
            KeyCode::Char('j') if !ctrl => (*idx + 1).min(last),
            KeyCode::Char('k') if !ctrl => idx.saturating_sub(1),
            KeyCode::Char('d') if ctrl => (*idx + half).min(last),
            KeyCode::Char('u') if ctrl => idx.saturating_sub(half),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => last,
            // The Metrics view had no list keys of its own.
            KeyCode::Down if self.view == View::Metrics => (*idx + 1).min(last),
            KeyCode::Up if self.view == View::Metrics => idx.saturating_sub(1),
            _ => return false,
        };
        true
    }

    /// `/`, `s` and `v` in a list view (SPEC §10.4); true when used.
    fn list_filter_key(&mut self, code: KeyCode) -> bool {
        use lists::*;
        let (filter, idx, sorts, quicks) = match self.view {
            View::Tools => (
                &mut self.tool_filter,
                &mut self.tool_idx,
                TOOL_SORTS,
                TOOL_QUICK,
            ),
            View::Sessions => (
                &mut self.session_filter,
                &mut self.session_idx,
                SESSION_SORTS,
                SESSION_QUICK,
            ),
            View::Proposals => (
                &mut self.proposal_filter,
                &mut self.proposal_idx,
                PROPOSAL_SORTS,
                PROPOSAL_QUICK,
            ),
            _ => return false,
        };
        let used = lists::key(filter, code, sorts.len(), quicks.len());
        if used {
            *idx = 0;
        }
        used
    }

    fn settings_rows(&self) -> Vec<settings::Row> {
        match Config::load(&self.paths) {
            Ok(cfg) => settings::rows(&cfg, &self.providers_info, self.show_thinking, &self.paths),
            Err(_) => Vec::new(),
        }
    }

    /// Save one setting through the daemon (audited; other clients get
    /// `SettingsChanged`).
    fn save_setting(&mut self, key: &str, value: serde_json::Value) {
        self.settings_saving = Some(key.to_string());
        self.request(
            Pending::SaveSetting,
            Request::SetSetting {
                key: key.to_string(),
                value,
            },
        );
    }

    /// Settings view keys (SPEC §10.4).
    fn settings_key(&mut self, key: KeyEvent) {
        if let Some(form) = &mut self.settings_form {
            let n = form.fields.len();
            match key.code {
                KeyCode::Esc => self.settings_form = None,
                KeyCode::Tab | KeyCode::Down => form.idx = (form.idx + 1) % n,
                KeyCode::BackTab | KeyCode::Up => form.idx = (form.idx + n - 1) % n,
                KeyCode::Enter if form.idx + 1 < n => form.idx += 1,
                KeyCode::Enter => match form.request() {
                    Ok(req) => {
                        form.error = None;
                        self.request(Pending::SaveProviderOrRole, req);
                    }
                    Err(e) => form.error = Some(e),
                },
                KeyCode::Backspace => form.fields[form.idx].input.backspace(),
                KeyCode::Left => form.fields[form.idx].input.left(),
                KeyCode::Right => form.fields[form.idx].input.right(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.fields[form.idx].input.insert_char(c)
                }
                _ => {}
            }
            return;
        }
        if let Some((k, edit)) = &mut self.settings_edit {
            match key.code {
                KeyCode::Esc => self.settings_edit = None,
                KeyCode::Enter => match settings::parse_number(edit) {
                    Ok(v) => {
                        let k = k.clone();
                        self.settings_edit = None;
                        self.save_setting(&k, v);
                    }
                    Err(e) => self.settings_error = Some((k.clone(), e)),
                },
                KeyCode::Backspace => edit.backspace(),
                KeyCode::Left => edit.left(),
                KeyCode::Right => edit.right(),
                KeyCode::Char(c) => edit.insert_char(c),
                _ => {}
            }
            return;
        }
        let rows = self.settings_rows();
        if rows.is_empty() {
            if key.code == KeyCode::Esc {
                self.view = View::Chat;
            }
            return;
        }
        let sel = self.settings_sel.min(rows.len() - 1);
        let row = rows[sel].clone();
        if key.code != KeyCode::Char('d') {
            self.settings_remove_arm = None;
        }
        match key.code {
            KeyCode::Esc => self.view = View::Chat,
            KeyCode::Up | KeyCode::Char('k') => self.settings_sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.settings_sel = (sel + 1).min(rows.len() - 1),
            KeyCode::Home | KeyCode::Char('g') => self.settings_sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.settings_sel = rows.len() - 1,
            KeyCode::Char('d') => {
                let req = match &row.kind {
                    settings::Kind::Provider(n) => Request::RemoveProvider { name: n.clone() },
                    settings::Kind::Role(n) => Request::DeleteRole { name: n.clone() },
                    _ => return,
                };
                if self.settings_remove_arm.as_deref() == Some(&row.key) {
                    self.settings_remove_arm = None;
                    self.request(Pending::SaveProviderOrRole, req);
                    self.notes.push(format!("removing {}", row.label));
                } else {
                    self.settings_remove_arm = Some(row.key.clone());
                    self.notes
                        .push(format!("press d again to remove {}", row.label));
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => match &row.kind {
                settings::Kind::ReadOnly(why) => {
                    if !why.is_empty() {
                        self.notes.push(format!("{}: {why}", row.label));
                    }
                }
                settings::Kind::Bool => {
                    self.save_setting(&row.key, serde_json::json!(row.value != "on"))
                }
                settings::Kind::Choice(choices) => {
                    self.save_setting(&row.key, settings::next_choice(choices, &row.value))
                }
                settings::Kind::Number => {
                    self.settings_edit =
                        Some((row.key.clone(), input::InputLine::from(row.value.as_str())))
                }
                settings::Kind::Model => self.open_model_picker(),
                settings::Kind::Thinking => self.slash("thinking"),
                settings::Kind::Provider(name) => {
                    let existing = self.providers_info["providers"]
                        .as_array()
                        .and_then(|a| a.iter().find(|p| p["name"] == name.as_str()))
                        .cloned();
                    self.settings_form = Some(settings::Form::provider(existing.as_ref()));
                }
                settings::Kind::Role(name) => {
                    let existing = self.providers_info["roles"][name.as_str()].clone();
                    self.settings_form = Some(settings::Form::role(Some(name), Some(&existing)));
                }
                settings::Kind::AddProvider => {
                    self.settings_form = Some(settings::Form::provider(None))
                }
                settings::Kind::AddRole => {
                    self.settings_form = Some(settings::Form::role(None, None))
                }
                settings::Kind::OpenEditor => self.open_config_in_editor(),
            },
            _ => {}
        }
    }

    /// Hand the terminal to `$VISUAL`/`$EDITOR` on config.toml; the daemon
    /// hot-reloads it on the next Run.
    fn open_config_in_editor(&mut self) {
        let file = self.paths.config_file();
        if let Some(note) = self.run_editor(&file, None) {
            self.notes.push(note);
        } else {
            self.notes
                .push("config.toml saved; the daemon reloads it on the next Run".into());
        }
    }

    /// `/open path P[:LINE]`: a file the answer mentions, in the editor.
    fn open_path(&mut self, arg: &str) {
        let mut parts = arg.splitn(3, ':');
        let path = parts.next().unwrap_or_default();
        let line = parts.next().and_then(|l| l.parse::<u32>().ok());
        match resolve_path_in(&self.work_dir(), path) {
            Some(file) if cfg!(test) => self.notes.push(format!(
                "opening {}{}",
                file.display(),
                line.map(|l| format!(" at line {l}")).unwrap_or_default()
            )),
            Some(file) => {
                if let Some(note) = self.run_editor(&file, line) {
                    self.notes.push(note);
                }
            }
            None => self.notes.push(format!("no file {path}")),
        }
    }

    /// Open `file` (at `line`) in `$VISUAL`/`$EDITOR`. Terminal editors
    /// get the terminal until they exit; GUI editors just start. Returns
    /// a note when something went wrong.
    fn run_editor(&mut self, file: &std::path::Path, line: Option<u32>) -> Option<String> {
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| if cfg!(windows) { "notepad" } else { "vi" }.into());
        let mut words = editor.split_whitespace();
        let program = words.next()?.to_string();
        let mut cmd = std::process::Command::new(&program);
        cmd.args(words);
        let gui = editor_takes_goto(&program);
        let path = file.display().to_string();
        match (line, gui) {
            (Some(l), true) => cmd.args(["-g", &format!("{path}:{l}")]),
            (Some(l), false) if program.ends_with("hx") => cmd.arg(format!("{path}:{l}")),
            (Some(l), false) if !program.contains("notepad") => cmd.arg(format!("+{l}")).arg(&path),
            _ => cmd.arg(&path),
        };
        if gui {
            return cmd
                .spawn()
                .err()
                .map(|e| format!("could not run {editor}: {e}"));
        }
        let mut out = stdout();
        let _ = disable_raw_mode();
        let _ = out.execute(crossterm::event::DisableBracketedPaste);
        let _ = out.execute(crossterm::event::DisableMouseCapture);
        let _ = out.execute(LeaveAlternateScreen);
        let status = cmd.status();
        let _ = out.execute(EnterAlternateScreen);
        if self.mouse_capture {
            let _ = out.execute(crossterm::event::EnableMouseCapture);
        }
        let _ = out.execute(crossterm::event::EnableBracketedPaste);
        let _ = enable_raw_mode();
        self.force_redraw = true;
        match status {
            Ok(s) if s.success() => None,
            Ok(s) => Some(format!("{editor} exited with {s}")),
            Err(e) => Some(format!("could not run {editor}: {e} (set $EDITOR)")),
        }
    }

    /// Ctrl+F opens the find bar; while it's open keys edit the query,
    /// Enter/Shift+Enter step through matches and Esc closes it.
    fn find_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('f') && ctrl {
            self.find = Some(FindBar::default());
            return true;
        }
        let Some(find) = &mut self.find else {
            return false;
        };
        let n = find.rows.len();
        match key.code {
            KeyCode::Esc => {
                self.find = None;
                self.follow_chat_bottom();
                return true;
            }
            KeyCode::Enter if n > 0 => {
                find.current = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    (find.current + n - 1) % n
                } else {
                    (find.current + 1) % n
                };
            }
            KeyCode::Enter => {}
            KeyCode::Backspace => {
                find.query.pop();
                find.current = usize::MAX;
            }
            KeyCode::Char(c) if !ctrl => {
                find.query.push(c);
                find.current = usize::MAX;
            }
            _ => return false,
        }
        find.scroll = true;
        true
    }

    /// Scroll to the previous/next user message (SPEC §10.2); past the
    /// last one, follow the live bottom again.
    fn jump_user_message(&mut self, forward: bool) {
        let max_top = self.chat_line_count.saturating_sub(self.chat_view_height);
        let top = self.chat_scroll_top.unwrap_or(max_top);
        let target = if forward {
            self.user_rows.iter().copied().find(|&r| r > top)
        } else {
            self.user_rows.iter().copied().rev().find(|&r| r < top)
        };
        match target {
            Some(r) if r < max_top => self.chat_scroll_top = Some(r),
            Some(_) | None if forward => self.follow_chat_bottom(),
            Some(r) => self.chat_scroll_top = Some(r.min(max_top)),
            None => self.chat_scroll_top = Some(0),
        }
    }

    /// Persisted tool call ids of the current Session, in order.
    fn call_ids(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter_map(|m| m.meta.as_deref())
            .flat_map(persisted_calls)
            .map(|(id, _, _)| id)
            .filter(|id| !id.is_empty())
            .collect()
    }

    /// What `[`/`]` move through, in order: messages with text
    /// (`msg:<id>`) and persisted tool calls (their call id).
    fn focus_items(&self) -> Vec<String> {
        let mut out = Vec::new();
        for m in &self.messages {
            let text = m.role == "user" || (m.role == "assistant" && !m.content.trim().is_empty());
            if text {
                out.push(format!("msg:{}", m.id));
            }
            if let Some(meta) = &m.meta {
                out.extend(
                    persisted_calls(meta)
                        .into_iter()
                        .map(|(id, _, _)| id)
                        .filter(|id| !id.is_empty()),
                );
            }
        }
        out
    }

    fn focused_user_message(&self) -> Option<&MessageInfo> {
        let id = self.call_focus.as_deref()?.strip_prefix("msg:")?;
        self.messages
            .iter()
            .find(|m| m.id == id && m.role == "user")
    }

    /// Editing a message with later user turns discards them, so the
    /// first Enter only asks.
    fn edit_needs_confirm(&self) -> bool {
        let Some(id) = &self.editing_id else {
            return false;
        };
        let seq = self.messages.iter().find(|m| &m.id == id).map(|m| m.seq);
        let later =
            seq.is_some_and(|s| self.messages.iter().any(|m| m.seq > s && m.role == "user"));
        later && !self.edit_armed && !self.input.trim().is_empty()
    }

    /// The focused item's text: plain, or as Markdown (SPEC §10.2).
    fn focused_text(&self, markdown: bool) -> Option<(String, String)> {
        let focus = self.call_focus.as_deref()?;
        if let Some(id) = focus.strip_prefix("msg:") {
            let m = self.messages.iter().find(|m| m.id == id)?;
            let text = match (markdown, m.role.as_str()) {
                (true, "user") => format!("**You:**\n\n{}", m.content),
                _ => m.content.clone(),
            };
            let what = if m.role == "user" {
                "your message"
            } else {
                "the answer"
            };
            return Some((text, what.into()));
        }
        let (call, result) = self.messages.iter().find_map(|m| {
            let call = persisted_calls(m.meta.as_deref()?)
                .into_iter()
                .find(|(id, _, _)| id == focus)?;
            let result = self
                .messages
                .iter()
                .find(|r| r.role == "tool" && result_call_id(r).as_deref() == Some(focus))
                .map(|r| r.content.clone())
                .unwrap_or_default();
            Some((call, result))
        })?;
        let (_, name, args) = call;
        let pretty = |t: &str| {
            serde_json::from_str::<serde_json::Value>(t)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| t.to_string())
        };
        let text = if markdown {
            format!(
                "**⚙ {name}**\n\n```json\n{}\n```\n\n```\n{}\n```",
                pretty(&args),
                pretty(&result)
            )
        } else {
            format!("{name} {args}\n{result}")
        };
        Some((text, format!("the {name} call")))
    }

    /// Tool-call focus (SPEC §10.2): `[`/`]` on an empty input move it;
    /// while a call is focused Enter toggles it, `z`/`Z` expand or
    /// collapse all, and Esc or any other key leaves the focus.
    fn call_focus_key(&mut self, key: &KeyEvent) -> bool {
        if !self.input.is_empty() || !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return false;
        }
        let ids = self.focus_items();
        let at = self
            .call_focus
            .as_ref()
            .and_then(|f| ids.iter().position(|id| id == f));
        match key.code {
            KeyCode::Char('[') | KeyCode::Char(']') if !ids.is_empty() => {
                let next = match (at, key.code == KeyCode::Char(']')) {
                    (None, _) => ids.len() - 1,
                    (Some(i), true) => (i + 1).min(ids.len() - 1),
                    (Some(i), false) => i.saturating_sub(1),
                };
                self.call_focus = Some(ids[next].clone());
                self.scroll_to_focus = true;
            }
            _ if self.call_focus.is_none() => return false,
            KeyCode::Enter => {
                let id = self.call_focus.clone().unwrap();
                if !id.starts_with("msg:") && !self.expanded_calls.remove(&id) {
                    self.expanded_calls.insert(id);
                }
                self.scroll_to_focus = true;
            }
            KeyCode::Char(c @ ('e' | 'f')) => {
                let Some(m) = self.focused_user_message().cloned() else {
                    self.notes.push("e and f work on your own messages".into());
                    return true;
                };
                self.call_focus = None;
                if c == 'e' {
                    // Rewind to this message: Enter re-runs from it.
                    self.editing_id = Some(m.id.clone());
                    self.edit_armed = false;
                    self.input.set(&m.content);
                    let later = self.messages.iter().filter(|x| x.seq > m.seq).count();
                    self.notes.push(format!(
                        "Editing this message — Enter re-runs from here{}",
                        if later > 0 {
                            format!(" and discards the {later} later message(s) (asks first)")
                        } else {
                            String::new()
                        }
                    ));
                } else if let Some(id) = self.current.clone() {
                    // A new Session with everything before it; the message
                    // waits in the input.
                    self.fork_prefill = Some(m.content.clone());
                    self.request(
                        Pending::ForkSession,
                        Request::ForkSession {
                            id,
                            name: None,
                            up_to_seq: Some(m.seq - 1),
                        },
                    );
                }
            }
            KeyCode::Char(c @ ('c' | 'C')) => {
                if let Some((text, what)) = self.focused_text(c == 'C') {
                    let what = if c == 'C' {
                        format!("{what} as Markdown")
                    } else {
                        what
                    };
                    self.copy_text(&text, &what);
                }
            }
            KeyCode::Char('z') => self.expanded_calls.extend(self.call_ids()),
            KeyCode::Char('Z') => self.expanded_calls.clear(),
            KeyCode::Esc => self.call_focus = None,
            _ => {
                // Typing leaves the focus and goes to the input.
                self.call_focus = None;
                return false;
            }
        }
        true
    }

    /// Text drafts from the last run (attachments aren't kept).
    fn load_drafts(&mut self) {
        let Ok(text) = std::fs::read_to_string(drafts_file(&self.paths)) else {
            return;
        };
        let saved: HashMap<String, String> = serde_json::from_str(&text).unwrap_or_default();
        for (sid, t) in saved {
            self.drafts.insert(
                sid,
                Draft {
                    input: input::InputLine::from(t.as_str()),
                    attachments: Vec::new(),
                },
            );
        }
    }

    /// Persist every Session's text draft, the current one included.
    fn save_drafts(&self) {
        let mut out: HashMap<&str, String> = self
            .drafts
            .iter()
            .filter(|(_, d)| !d.input.is_empty())
            .map(|(k, d)| (k.as_str(), d.input.expanded()))
            .collect();
        if let Some(cur) = self.current.as_deref().filter(|_| !self.input.is_empty()) {
            out.insert(cur, self.input.expanded());
        }
        let file = drafts_file(&self.paths);
        if out.is_empty() {
            let _ = std::fs::remove_file(file);
        } else if let Ok(json) = serde_json::to_string(&out) {
            let _ = std::fs::write(file, json);
        }
    }

    /// The user messages of the current Session, oldest first: the input
    /// history for Up/Down and Ctrl+R.
    fn input_history(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content.clone())
            .collect()
    }

    /// Chat-view keys: the line editor, history, sending and scrolling
    /// (PgUp/PgDn and the wheel; ↑/↓ belong to the input).
    fn input_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.input.searching().is_some() {
            let history = self.input_history();
            match key.code {
                KeyCode::Char('r') if ctrl => self.input.search_older(&history),
                KeyCode::Char('g') if ctrl => self.input.search_cancel(),
                KeyCode::Esc => self.input.search_cancel(),
                KeyCode::Enter => self.input.search_accept(&history),
                KeyCode::Backspace => self.input.search_type(None, &history),
                KeyCode::Char(c) if !ctrl && !alt => self.input.search_type(Some(c), &history),
                _ => {
                    self.input.search_accept(&history);
                    self.input_key(key);
                }
            }
            return;
        }
        if self.find_key(&key) || self.call_focus_key(&key) {
            return;
        }
        // On the empty-Session screen, 1–4 put an example in the input.
        if let KeyCode::Char(d @ '1'..='4') = key.code {
            if self.input.is_empty() && self.messages.is_empty() && !self.running() {
                self.input.set(EXAMPLES[d as usize - '1' as usize]);
                return;
            }
        }
        let page = self.chat_view_height.saturating_sub(1).max(1);
        let empty = self.input.is_empty();
        match key.code {
            KeyCode::Up if ctrl => self.jump_user_message(false),
            KeyCode::Down if ctrl => self.jump_user_message(true),
            KeyCode::Char('{') if empty => self.jump_user_message(false),
            KeyCode::Char('}') if empty => self.jump_user_message(true),
            KeyCode::Home if empty => self.chat_scroll_top = Some(0),
            KeyCode::PageUp => self.scroll_chat_up(page),
            KeyCode::PageDown => self.scroll_chat_down(page),
            KeyCode::Up if !self.input.on_first_line() => self.input.vertical(false),
            KeyCode::Down if !self.input.on_last_line() => self.input.vertical(true),
            KeyCode::Up => {
                let history = self.input_history();
                self.input.history_prev(&history);
            }
            KeyCode::Down => {
                let history = self.input_history();
                self.input.history_next(&history);
            }
            KeyCode::End if self.input.is_empty() => self.follow_chat_bottom(),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => self.input.newline(),
            _ if is_queue_key(&key) => self.submit_chat(true),
            // A trailing `\` + Enter is a newline in any terminal.
            KeyCode::Enter if self.input.before_cursor().ends_with('\\') => {
                self.input.backspace();
                self.input.newline();
            }
            KeyCode::Enter if self.edit_needs_confirm() => {
                self.edit_armed = true;
                self.notes
                    .push("Enter again to discard the later turns and re-run (Esc cancels)".into());
            }
            KeyCode::Enter => {
                self.edit_armed = false;
                self.submit_chat(false);
            }
            KeyCode::Esc if !self.running() && self.toasts.iter().any(|t| t.2) => {
                self.toasts.clear();
            }
            KeyCode::Esc if self.renaming.is_some() || self.instructing.is_some() => {
                self.renaming = None;
                self.instructing = None;
                self.input.clear();
                self.notes.push("cancelled".into());
            }
            KeyCode::Esc if self.bulk.is_some() => {
                self.bulk = None;
                self.input.clear();
                self.notes.push("cancelled".into());
            }
            KeyCode::Esc if self.editing_id.is_some() => {
                self.editing_id = None;
                self.edit_armed = false;
                self.input.clear();
                self.notes.push("edit cancelled".into());
            }
            KeyCode::Esc => {
                if self.running() {
                    if let Some(id) = self.current.clone() {
                        self.fire(Request::InterruptRun { session_id: id });
                    }
                } else {
                    self.view = View::Chat;
                }
            }
            KeyCode::Backspace if self.input.is_empty() && !self.attachments.is_empty() => {
                if let Some((name, _)) = self.attachments.pop() {
                    self.notes.push(format!("removed {name}"));
                }
            }
            KeyCode::Backspace if ctrl || alt => self.input.delete_word_back(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            // Ctrl+Shift+←/→: the multi-pane layout's side panes.
            KeyCode::Left if ctrl && key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.resize_panes("narrower")
            }
            KeyCode::Right if ctrl && key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.resize_panes("wider")
            }
            KeyCode::Left if ctrl || alt => self.input.word_left(),
            KeyCode::Right if ctrl || alt => self.input.word_right(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Char(c) if ctrl => match c {
                'a' => self.input.home(),
                'e' => self.input.end(),
                'b' => self.input.left(),
                'f' => self.input.right(),
                'w' => self.input.delete_word_back(),
                // Ctrl+H is Backspace in many terminals.
                'h' => self.input.backspace(),
                'u' => self.input.kill_to_start(),
                'k' => self.input.kill_to_end(),
                'd' => self.input.delete(),
                'r' => {
                    let history = self.input_history();
                    self.input.search_older(&history);
                }
                'v' => self.paste_image(),
                _ => return,
            },
            KeyCode::Char(c) if alt => match c {
                'b' => self.input.word_left(),
                'f' => self.input.word_right(),
                'd' => self.input.delete_word_forward(),
                _ => return,
            },
            KeyCode::Char(c) => self.input.insert_char(c),
            _ => return,
        }
        self.palette_sel = 0;
        self.update_mention();
    }

    /// A bracketed paste: one block, wherever text is being typed.
    fn paste(&mut self, text: &str) {
        if self.wizard.is_some() || self.model_overlay.is_some() {
            for c in text.chars().filter(|c| !c.is_control()) {
                self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            return;
        }
        if let Some(ed) = &mut self.editor {
            let chars: Vec<char> = text.replace("\r\n", "\n").chars().collect();
            let n = chars.len();
            ed.chars.splice(ed.cursor..ed.cursor, chars);
            ed.cursor += n;
            return;
        }
        if let Some(f) = &mut self.settings_form {
            f.fields[f.idx].input.paste(text.trim());
            return;
        }
        if let Some((_, edit)) = &mut self.settings_edit {
            edit.paste(text.trim());
            return;
        }
        if self.view == View::Chat {
            // A file dragged into the terminal arrives as its path.
            if let Some(path) = clipimg::dropped_file(text) {
                self.attach_path_file(&path);
                return;
            }
            self.input.paste(text);
            self.palette_sel = 0;
            self.update_mention();
        }
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    app.click_targets.borrow_mut().clear();
    app.refresh_toasts();
    if let Some(w) = &app.wizard {
        draw_wizard(f, w, app.color);
        return;
    }
    if let Some(ed) = &app.editor {
        draw_editor(f, ed, app);
        return;
    }
    let input_h = input_height(app, f.area().width)
        .min(f.area().height / 2)
        .max(3);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .split(f.area());
    draw_tab_bar(f, chunks[0], app);
    let toast_area = chunks[1];
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
    let typing = app.view == View::Chat;
    draw_input(
        f,
        chunks[2],
        app,
        typing && (modal.is_none() || app.input_focus),
    );
    draw_status(f, chunks[3], app);
    if app.view == View::Chat {
        let palette = app.palette_items();
        let question = app.current_question().map(|i| &app.questions[i]);
        if let Some(q) = question.filter(|_| palette.is_empty() && modal.is_none()) {
            draw_question_panel(f, chunks[1], q, !app.input.is_empty(), app.color);
        } else if !palette.is_empty() {
            draw_palette(f, chunks[2], &palette, app.palette_sel, app.color);
        } else if let Some(m) = app.mention.as_ref().filter(|m| !m.dismissed) {
            draw_mention_popup(f, chunks[2], m, app.color);
        }
    }
    if let Some((req, promotion)) = modal {
        let more = app.approvals.len() + app.promotions.len() - 1;
        draw_approval(
            f,
            req,
            promotion,
            more,
            app.color,
            !app.input_focus,
            app.approval_explain,
        );
    } else if let Some(pick) = &app.rollback_pick {
        draw_rollback_picker(f, app, pick);
    } else if let Some(o) = &app.model_overlay {
        draw_model_overlay(f, o, app.color);
    }
    if app.view == View::Chat || app.multi_pane {
        draw_toasts(f, toast_area, app);
    }
    if app.quit_confirm {
        draw_quit_confirm(f, app);
    }
    if let Some((query, sel)) = &app.switcher {
        draw_switcher(f, app, query, *sel);
    }
    if let Some(p) = &app.pager {
        let area = f.area();
        let height = area.height.saturating_sub(2) as usize;
        let body: Vec<Line> = if p.left == 0 && !p.styled.is_empty() {
            p.styled.iter().skip(p.top).take(height).cloned().collect()
        } else {
            p.lines
                .iter()
                .skip(p.top)
                .take(height)
                .map(|l| Line::raw(l.chars().skip(p.left).collect::<String>()))
                .collect()
        };
        let title = format!(
            " {} · line {}/{} · ↑↓ ←→ PgUp PgDn g G · Esc close ",
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

/// Highlight case-insensitive `query` matches in a line's spans; `style`
/// gets each match's index in the line. Returns the line and its match
/// count (matches across span boundaries are not found).
fn highlight_matches(
    line: Line<'static>,
    query: &str,
    style: impl Fn(usize) -> Style,
) -> (Line<'static>, usize) {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return (line, 0);
    }
    let mut found = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        let chars: Vec<char> = span.content.chars().collect();
        let low: Vec<char> = chars
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect();
        let (mut start, mut i) = (0, 0);
        while i + q.len() <= chars.len() {
            if low[i..i + q.len()] == q[..] {
                if i > start {
                    spans.push(Span::styled(
                        chars[start..i].iter().collect::<String>(),
                        span.style,
                    ));
                }
                spans.push(Span::styled(
                    chars[i..i + q.len()].iter().collect::<String>(),
                    style(found),
                ));
                found += 1;
                i += q.len();
                start = i;
            } else {
                i += 1;
            }
        }
        if start == 0 {
            spans.push(span);
        } else if start < chars.len() {
            spans.push(Span::styled(
                chars[start..].iter().collect::<String>(),
                span.style,
            ));
        }
    }
    (Line::from(spans).style(line.style), found)
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

/// An open `ask_user` question (SPEC §5.5), at the bottom of the chat
/// area just above the input.
fn draw_question_panel(f: &mut Frame, chat: Rect, open: &OpenQuestion, typing: bool, color: bool) {
    let q = open.current();
    let total = open.questions.len();
    let mut title = String::from(" Question");
    if total > 1 {
        title.push_str(&format!(" {}/{total}", open.answers.len() + 1));
    }
    if let Some(h) = &q.header {
        title.push_str(&format!(" · {h}"));
    }
    title.push(' ');
    let mut lines = vec![Line::styled(
        q.question.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    for (n, o) in q.options.iter().enumerate() {
        let here = n == open.cursor && !typing;
        let mark = match (
            q.multi_select,
            open.checked.get(n).copied().unwrap_or(false),
        ) {
            (true, true) => "[x]",
            (true, false) => "[ ]",
            (false, _) if here => "(•)",
            (false, _) => "( )",
        };
        let style = if here {
            bold_fg(color, Color::Green)
        } else {
            Style::default()
        };
        lines.push(Line::styled(
            format!(
                "{} {}. {mark} {}",
                if here { "›" } else { " " },
                n + 1,
                o.label
            ),
            style,
        ));
        if here {
            if let Some(d) = &o.description {
                lines.push(Line::styled(format!("        {d}"), ui::dim(color)));
            }
        }
    }
    let other_here = open.on_other() || typing;
    lines.push(Line::styled(
        format!(
            "{} {}",
            if other_here { "›" } else { " " },
            if q.options.is_empty() {
                "type your answer below"
            } else {
                "Other… (type your own answer below)"
            }
        ),
        if other_here {
            bold_fg(color, Color::Green)
        } else {
            ui::dim(color)
        },
    ));
    let mut hint = String::from(if typing {
        "Enter sends your text as the answer"
    } else if q.multi_select {
        "↑↓ move · Space tick · Enter answer · 1–9 tick"
    } else {
        "↑↓ move · Enter answer · 1–9 pick"
    });
    if !open.answers.is_empty() {
        hint.push_str(" · Shift+Tab back");
    }
    lines.push(Line::styled(hint, ui::dim(color)));
    let w = chat.width.clamp(1, 100);
    let h = (lines.len() as u16 + 2).min(chat.height);
    if h < 3 {
        return;
    }
    let rect = Rect {
        x: chat.x,
        y: chat.bottom() - h,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .block(ui::panel(title, color, Color::Magenta))
            .wrap(Wrap { trim: false }),
        rect,
    );
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

fn views_for_click(app: &App) -> Vec<View> {
    let mut views: Vec<View> = VIEW_ORDER.to_vec();
    if app.view == View::Search {
        views.push(View::Search);
    }
    views
}

/// Remember a clickable region for this frame.
fn clickable(app: &App, rect: Rect, what: Click) {
    app.click_targets.borrow_mut().push((rect, what));
}

/// Rows of a list or table drawn in `area` (inside its border, after
/// `header` rows), from its scroll `offset`, as click targets.
fn clickable_rows(
    app: &App,
    area: Rect,
    header: u16,
    offset: usize,
    count: usize,
    what: fn(usize) -> Click,
) {
    let top = area.y + 1 + header;
    let rows = area.height.saturating_sub(2 + header) as usize;
    for (k, i) in (offset..count).take(rows).enumerate() {
        clickable(
            app,
            Rect::new(area.x + 1, top + k as u16, area.width.saturating_sub(2), 1),
            what(i),
        );
    }
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
    // Each view's tab is clickable.
    let mut x = area.x;
    for span in &spans {
        let w = span.width() as u16;
        let label = span.content.trim();
        if let Some(v) = views_for_click(app)
            .into_iter()
            .find(|v| v.label() == label)
        {
            clickable(app, Rect::new(x, area.y, w, 1), Click::View(v));
        }
        x += w;
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
    if let Some(find) = app.find.as_ref().filter(|_| app.view == View::Chat) {
        let n = find.rows.len();
        let count = if find.query.is_empty() {
            String::new()
        } else if n == 0 {
            "  no matches".into()
        } else {
            format!("  {}/{n}", find.current.min(n - 1) + 1)
        };
        let line = Line::from(vec![
            Span::styled("find › ", bold_fg(app.color, Color::Magenta)),
            Span::raw(format!("{}▏", find.query)),
            Span::styled(count, bold_fg(app.color, Color::Yellow)),
        ]);
        let block = ui::panel(" Find in this Session ", app.color, Color::Magenta).title(ui::hint(
            "Enter next · Shift+Enter previous · Esc close",
            app.color,
        ));
        f.render_widget(Paragraph::new(line).block(block), area);
        return;
    }
    let (prompt, color) = if let Some((action, ids)) = &app.bulk {
        (
            match action {
                BulkAction::Delete if ids.len() == 1 => "type delete to delete 1 Session › ",
                BulkAction::Delete => "type delete to delete these Sessions › ",
                BulkAction::Tag => "tags to add › ",
            },
            Color::Red,
        )
    } else if app.renaming.is_some() {
        ("rename › ", Color::Magenta)
    } else if app.instructing.is_some() {
        ("instructions › ", Color::Magenta)
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
    let prompt_w = Span::raw(prompt).width();
    let avail = input_text_width(area.width, prompt);
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor = Some((0usize, 0usize));
    if let Some((query, found)) = app.input.searching() {
        let hit = found
            .and_then(|i| app.input_history().get(i).cloned())
            .unwrap_or_default();
        let label = format!("(history search) {query}▏: ");
        lines.push(Line::from(vec![
            Span::styled(label.clone(), bold_fg(app.color, Color::Magenta)),
            Span::raw(truncate_chars(
                &hit.replace('\n', " ⏎ "),
                avail.saturating_sub(label.chars().count()),
            )),
        ]));
        // The ▏ marks the query's end; no terminal cursor.
        cursor = None;
    } else if app.input.is_empty() && !app.running() && prompt == "› " && show_cursor {
        lines.push(Line::from(vec![
            Span::styled(prompt, bold_fg(app.color, color)),
            Span::styled("Ask Formless to do something…", ui::dim(app.color)),
        ]));
    } else {
        let (rows, cur) = app.input.layout(avail);
        let height = area.height.saturating_sub(2).max(1) as usize;
        // Keep the cursor row on screen.
        let top = cur.0.saturating_sub(height - 1);
        for (i, row) in rows.into_iter().enumerate().skip(top).take(height) {
            let lead = if i == 0 {
                Span::styled(prompt, bold_fg(app.color, color))
            } else {
                Span::raw(" ".repeat(prompt_w))
            };
            lines.push(Line::from(vec![lead, Span::raw(row)]));
        }
        cursor = Some((cur.0 - top, cur.1));
    }
    // Which Session gets this message (SPEC §10.2).
    let target = app
        .current
        .as_ref()
        .and_then(|id| app.sessions.iter().find(|s| &s.id == id))
        .map(|s| truncate_chars(&s.name, 32));
    let mut title = match &target {
        Some(name) => format!(" Message › to: {name} "),
        None => String::from(" Message "),
    };
    for (n, (name, bytes)) in app.attachments.iter().enumerate() {
        let label = if name.starts_with("pasted-image") {
            format!("image {}", n + 1)
        } else {
            truncate_chars(name, 24)
        };
        title.push_str(&format!("[{label} · {}] ", size_text(bytes.len())));
    }
    let flashing = app.target_flash.is_some_and(|t| Instant::now() < t);
    let mut block = ui::panel(
        title,
        app.color,
        if flashing { Color::Yellow } else { color },
    );
    if flashing {
        block = block.border_style(bold_fg(app.color, Color::Yellow));
    }
    if area.width > 64 {
        block = block.title(ui::hint(
            &if app.running() {
                format!("Enter steer · {} queue · Esc stop", app.queue_label())
            } else {
                format!(
                    "Enter send · Shift+Enter newline · {} queue · /help",
                    app.queue_label()
                )
            },
            app.color,
        ));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
    if let Some((row, col)) = cursor.filter(|_| show_cursor) {
        let x = area.x + 1 + prompt_w as u16 + col as u16;
        let y = area.y + 1 + row as u16;
        f.set_cursor_position((
            x.min(area.right().saturating_sub(2)),
            y.min(area.bottom().saturating_sub(2)),
        ));
    }
}

/// `240 KB`, `1.2 MB`.
fn size_text(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// Columns for input text inside the box after the prompt.
fn input_text_width(box_width: u16, prompt: &str) -> usize {
    (box_width.saturating_sub(2) as usize)
        .saturating_sub(Span::raw(prompt).width() + 1)
        .max(1)
}

/// The input box's height: its wrapped rows (up to 8) plus borders.
fn input_height(app: &App, width: u16) -> u16 {
    let rows = app
        .input
        .layout(input_text_width(width, "steer › "))
        .0
        .len();
    rows.clamp(1, 8) as u16 + 2
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
    if let Some(dir) = app
        .current
        .as_ref()
        .and_then(|id| app.sessions.iter().find(|s| &s.id == id))
        .and_then(|s| s.cwd.as_deref())
    {
        left.push(Span::styled(
            format!(" {}", short_dir(dir)),
            ui::dim(app.color),
        ));
    }
    if !app.others.is_empty() {
        left.push(Span::styled(
            format!(" · also open in {}", others_text(&app.others)),
            fg(app.color, Color::Magenta),
        ));
    }
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
    if let Some(&(used, size)) = app.current.as_ref().and_then(|s| app.context_fill.get(s)) {
        if size > 0 && used > 0 {
            let pct = used * 100 / size;
            let style = match pct {
                90.. => bold_fg(app.color, Color::Red),
                70.. => fg(app.color, Color::Yellow),
                _ => ui::dim(app.color),
            };
            left.push(sep());
            left.push(Span::styled(format!("ctx {pct}%"), style));
        }
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
    // Notes show as toasts; the right side is this view's key hints,
    // from one table, trimmed by importance to fit (SPEC §10.5).
    let mut spans = left;
    let tip = format!("{} ", keys::line(app.view, width.saturating_sub(used + 3)));
    if tip.len() > 1 {
        spans.push(Span::raw(" ".repeat(width - used - tip.chars().count())));
        spans.push(Span::styled(tip, ui::dim(app.color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Whether a note reports a problem (it stays until dismissed).
fn is_error_note(note: &str) -> bool {
    let lower = note.to_lowercase();
    [
        "paused",
        "failed",
        "denied",
        "error",
        "could not",
        "couldn't",
        "unknown",
        "cannot",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

/// How long an ordinary toast stays.
const TOAST_SECS: u64 = 6;

/// The Ctrl+K Session switcher.
fn draw_switcher(f: &mut Frame, app: &App, query: &str, sel: usize) {
    let area = f.area();
    let matches = app.switcher_matches(query);
    let rows = (area.height as usize).saturating_sub(8).clamp(1, 12);
    let sel = sel.min(matches.len().saturating_sub(1));
    let start = sel.saturating_sub(rows - 1);
    let mut lines = vec![Line::from(vec![
        Span::styled("Session  ", ui::dim(app.color)),
        Span::raw(format!("{query}▏")),
    ])];
    let now = chrono::Utc::now();
    for (k, &i) in matches.iter().enumerate().skip(start).take(rows) {
        let s = &app.sessions[i];
        let here = k == sel;
        let style = if here {
            bold_fg(app.color, Color::Green)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{} {}",
                    if here { "›" } else { " " },
                    truncate_chars(&s.name, 40)
                ),
                style,
            ),
            Span::styled(
                format!(
                    "  {}{}",
                    ui::relative_time(&s.updated_at, now),
                    s.preview
                        .as_deref()
                        .map(|p| format!(" · “{}”", truncate_chars(p, 50)))
                        .unwrap_or_default()
                ),
                ui::dim(app.color),
            ),
        ]));
    }
    if matches.is_empty() {
        lines.push(Line::styled(
            "  no match · Ctrl+N starts a Session with this name",
            ui::dim(app.color),
        ));
    }
    lines.push(Line::styled(
        "Enter switch · Ctrl+N new Session named this · Esc close",
        ui::dim(app.color),
    ));
    let w = 90.min(area.width.saturating_sub(2)).max(1);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 3,
        w,
        h,
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(ui::panel(" Switch Session ", app.color, Color::Cyan)),
        rect,
    );
}

/// "Runs are still working" before quitting.
fn draw_quit_confirm(f: &mut Frame, app: &App) {
    let n = app.running_sessions.len();
    let lines = vec![
        Line::raw(""),
        Line::raw(format!(
            "  {n} Run{} still working in the daemon. {} keep going if you quit.",
            if n == 1 { " is" } else { "s are" },
            if n == 1 { "It will" } else { "They'll" }
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                " q ",
                bold_fg(app.color, Color::Green).add_modifier(Modifier::REVERSED),
            ),
            Span::raw(" quit    "),
            Span::styled(
                " s ",
                bold_fg(app.color, Color::Red).add_modifier(Modifier::REVERSED),
            ),
            Span::raw(" stop them and quit    "),
            Span::styled(" Esc ", ui::dim(app.color).add_modifier(Modifier::REVERSED)),
            Span::raw(" cancel"),
        ]),
    ];
    let area = f.area();
    let w = 66.min(area.width.saturating_sub(2)).max(1);
    let h = 6.min(area.height);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .block(ui::panel(" Quit? ", app.color, Color::Yellow))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// Recent notes as toasts in the top-right corner of `area`, newest at
/// the bottom; errors in red until dismissed.
fn draw_toasts(f: &mut Frame, area: Rect, app: &App) {
    let live: Vec<&(String, Instant, bool)> = app.toasts.iter().rev().take(4).rev().collect();
    if live.is_empty() || area.width < 20 || area.height < 4 {
        return;
    }
    let w = (area.width / 2).clamp(20, 64);
    let lines: Vec<Line> = live
        .iter()
        .map(|(text, _, error)| {
            Line::styled(
                truncate_chars(text, w as usize - 4),
                if *error {
                    bold_fg(app.color, Color::Red)
                } else {
                    fg(app.color, Color::Gray)
                },
            )
        })
        .collect();
    let any_error = live.iter().any(|t| t.2);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(area.right().saturating_sub(w + 1), area.y + 1, w, h);
    let mut block = ui::panel(
        " notes ",
        app.color,
        if any_error {
            Color::Red
        } else {
            Color::DarkGray
        },
    );
    if any_error {
        block = block.title_bottom(ui::hint("Esc dismisses", app.color));
    }
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Speaker groups: tool results belong to the assistant turn around them.
fn speaker(role: &str) -> &'static str {
    match role {
        "user" => "user",
        "system-note" => "system-note",
        _ => "assistant",
    }
}

/// The client a user message came from, when it wasn't this one: another
/// kind of client, or another TUI still attached. (Earlier runs of this
/// TUI had other connection ids, so those alone don't count.)
fn sent_elsewhere(app: &App, m: &MessageInfo) -> Option<String> {
    let meta: serde_json::Value = serde_json::from_str(m.meta.as_deref()?).ok()?;
    let label = meta["client"]["label"].as_str()?;
    let conn = meta["client"]["conn"].as_u64();
    let other_kind = !label.starts_with("TUI");
    let other_tui = conn != app.conn && app.others.iter().any(|o| Some(o.conn) == conn);
    (other_kind || other_tui).then(|| label.to_string())
}

/// "web, TUI ×2" for the other clients on this Session.
fn others_text(others: &[AttachedClient]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for o in others {
        match counts.iter_mut().find(|(l, _)| *l == o.label) {
            Some((_, n)) => *n += 1,
            None => counts.push((&o.label, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(l, n)| {
            if n > 1 {
                format!("{l} ×{n}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
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
    if let Some(t) = when
        .filter(|_| app.show_timestamps)
        .and_then(|w| ui::message_stamp(w, chrono::Utc::now()))
    {
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
/// Image attachments of a message that this machine can read: (name,
/// path).
fn image_attachments(m: &MessageInfo) -> Vec<(String, String)> {
    let Some(meta) = m
        .meta
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    else {
        return Vec::new();
    };
    meta["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["mime"].as_str().is_some_and(|t| t.starts_with("image/")))
        .filter_map(|a| {
            let path = a["path"].as_str()?;
            std::path::Path::new(path).is_file().then(|| {
                (
                    a["name"].as_str().unwrap_or("image").to_string(),
                    path.to_string(),
                )
            })
        })
        .collect()
}

/// `tui.pane_widths` as (left, right), each 0 (hidden) or 10–40%.
fn pane_widths(v: &[u16]) -> (u16, u16) {
    let fix = |w: Option<&u16>, default: u16| match w.copied().unwrap_or(default) {
        0 => 0,
        w => w.clamp(10, 40),
    };
    (fix(v.first(), 22), fix(v.get(1), 26))
}

/// A Session's state icon for the list (SPEC §10.4): waiting on an
/// approval or a question, running, paused, or nothing.
fn session_state(app: &App, s: &SessionInfo) -> (&'static str, Style) {
    let waiting = app
        .approvals
        .iter()
        .any(|a| a.session_id.as_deref() == Some(s.id.as_str()));
    if waiting {
        ("⚠", bold_fg(app.color, Color::Red))
    } else if app.questions.iter().any(|q| q.session_id == s.id) {
        ("?", bold_fg(app.color, Color::Magenta))
    } else if app.running_sessions.contains(&s.id) {
        ("◐", fg(app.color, Color::Yellow))
    } else if s.last_run_status.as_deref() == Some("paused") {
        ("⏸", fg(app.color, Color::Yellow))
    } else {
        (" ", Style::default())
    }
}

/// A directory for display: `~` for home, and only the last two parts
/// of a long path (`…/work/formless`).
fn short_dir(dir: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let d = match dir.strip_prefix(&home).filter(|_| !home.is_empty()) {
        Some(rest) => format!("~{rest}"),
        None => dir.to_string(),
    };
    let parts: Vec<&str> = d.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() > 3 {
        format!("…/{}", parts[parts.len() - 2..].join("/"))
    } else {
        d
    }
}

/// A file a chat mentions, resolved against `base` (the Session's working
/// directory), or absolute, or under `~/`, when it exists.
fn resolve_path_in(base: &std::path::Path, path: &str) -> Option<std::path::PathBuf> {
    let p = match path.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None => base.join(path),
    };
    p.is_file().then(|| p.canonicalize().unwrap_or(p))
}

/// [`resolve_path_in`] the TUI's own directory.
#[cfg(test)]
fn resolve_path(path: &str) -> Option<std::path::PathBuf> {
    resolve_path_in(&std::env::current_dir().ok()?, path)
}

/// GUI editors that take `-g file:line` (VS Code and its forks).
fn editor_takes_goto(program: &str) -> bool {
    let name = std::path::Path::new(program)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    matches!(
        name.as_str(),
        "code" | "code-insiders" | "codium" | "cursor" | "windsurf"
    )
}

/// A pager of numbered, highlighted JavaScript (a Tool's code).
fn code_pager(title: &str, code: &str, color: bool) -> Pager {
    let lines: Vec<&str> = code.lines().collect();
    let w = lines.len().to_string().len();
    let mut hl = highlight::Highlighter::new("js", color);
    let styled = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut spans = vec![Span::styled(format!("{:>w$} │ ", i + 1), ui::dim(color))];
            match &mut hl {
                Some(h) => spans.extend(h.line(l)),
                None => spans.push(Span::raw(l.to_string())),
            }
            Line::from(spans)
        })
        .collect();
    Pager {
        title: title.to_string(),
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{:>w$} │ {l}", i + 1))
            .collect(),
        top: 0,
        left: 0,
        styled,
    }
}

/// Fenced code blocks as (language, body), in order (so index N matches
/// the `[N]` labels and `/copy code N`).
fn fenced_blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("```") {
            match cur.take() {
                Some((lang, body)) => out.push((lang, body.join("\n"))),
                None => cur = Some((rest.trim().to_lowercase(), Vec::new())),
            }
        } else if let Some((_, body)) = cur.as_mut() {
            body.push(line);
        }
    }
    out
}

/// A mermaid.live link that opens `src` in its editor.
fn mermaid_live_url(src: &str) -> String {
    use base64::Engine;
    let state = serde_json::json!({
        "code": src,
        "mermaid": "{\"theme\": \"default\"}",
        "autoSync": true,
        "updateDiagram": true,
    });
    format!(
        "https://mermaid.live/edit#base64:{}",
        base64::engine::general_purpose::URL_SAFE.encode(state.to_string())
    )
}

/// The Markdown tables in `text`: runs of lines that start with `|`.
fn markdown_tables(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim_start().starts_with('|') {
            cur.push(line);
        } else if !cur.is_empty() {
            if cur.len() >= 2 {
                out.push(cur.join("\n"));
            }
            cur.clear();
        }
    }
    out
}

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
        // `/copy N`: the Nth message from the bottom.
        if let Ok(n) = arg.trim().parse::<usize>() {
            let texts: Vec<&MessageInfo> = self
                .messages
                .iter()
                .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
                .filter(|m| !m.content.trim().is_empty())
                .collect();
            match n.checked_sub(1).and_then(|i| texts.iter().rev().nth(i)) {
                Some(m) => {
                    let text = m.content.clone();
                    self.copy_text(&text, &format!("message {n} from the bottom"));
                }
                None => self
                    .notes
                    .push(format!("there are {} messages to copy", texts.len())),
            }
            return;
        }
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
        self.copy_text(&text, &what);
    }

    /// A click on `what` (SPEC §10.5): tabs switch views, list rows
    /// select (a double-click opens), and a tool call toggles.
    fn click(&mut self, what: Click) {
        let double = self
            .last_click
            .as_ref()
            .is_some_and(|(c, t)| *c == what && t.elapsed() < Duration::from_millis(400));
        self.last_click = Some((what.clone(), Instant::now()));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match what {
            Click::View(v) => self.open_view(v),
            Click::Session(i) => {
                self.session_idx = i;
                if double {
                    self.key(enter);
                }
            }
            Click::Tool(i) => {
                self.tool_idx = i;
                if double {
                    self.key(enter);
                }
            }
            Click::Proposal(i) => self.proposal_idx = i,
            Click::Call(id) => {
                if !self.expanded_calls.remove(&id) {
                    self.expanded_calls.insert(id);
                }
            }
        }
    }

    /// Turn notes added since the last frame into toasts, and drop the
    /// ordinary ones that have been up long enough.
    fn refresh_toasts(&mut self) {
        if self.notes_seen > self.notes.len() {
            self.notes_seen = self.notes.len();
        }
        let now = Instant::now();
        for n in &self.notes[self.notes_seen..] {
            self.toasts.push((n.clone(), now, is_error_note(n)));
        }
        self.notes_seen = self.notes.len();
        self.toasts
            .retain(|(_, t, error)| *error || t.elapsed() < Duration::from_secs(TOAST_SECS));
        // A burst (e.g. a long list) keeps only its newest lines.
        let extra = self.toasts.len().saturating_sub(8);
        self.toasts.drain(..extra);
    }

    /// `/tag NAME…` adds tags to the current Session, `/untag NAME…`
    /// removes them, and `/tag` alone lists them (SPEC §10.4).
    fn tag_cmd(&mut self, args: &str, add: bool) {
        let Some(s) = self
            .current
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
            .cloned()
        else {
            self.notes.push("no current session".into());
            return;
        };
        if args.is_empty() {
            let mut all: Vec<&String> = self.sessions.iter().flat_map(|s| &s.tags).collect();
            all.sort();
            all.dedup();
            self.notes.push(format!(
                "{}: {} · all tags: {} · filter Sessions with /#tag",
                s.name,
                if s.tags.is_empty() {
                    "no tags".to_string()
                } else {
                    format!("#{}", s.tags.join(" #"))
                },
                if all.is_empty() {
                    "none".to_string()
                } else {
                    all.iter()
                        .map(|t| format!("#{t}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                }
            ));
            return;
        }
        let given: Vec<String> = args
            .split(|c: char| c.is_whitespace() || c == ',')
            .map(|t| t.trim_start_matches('#').to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        let mut tags = s.tags.clone();
        if add {
            tags.extend(given);
        } else {
            tags.retain(|t| !given.contains(t));
        }
        self.request(Pending::SetTags, Request::SetSessionTags { id: s.id, tags });
    }

    /// A bulk key on the marked Sessions: archive and export act at once;
    /// tag and delete ask for typed input first.
    fn bulk_key(&mut self, code: KeyCode, ids: Vec<String>) {
        let n = ids.len();
        match code {
            KeyCode::Char('a') => {
                // Archive them, or restore them if all are archived.
                let restore = self
                    .sessions
                    .iter()
                    .filter(|s| ids.contains(&s.id))
                    .all(|s| s.archived);
                for id in ids {
                    self.request(
                        Pending::SetArchive,
                        Request::SetSessionArchived {
                            id,
                            archived: !restore,
                        },
                    );
                }
                self.marked.clear();
            }
            KeyCode::Char('e') => {
                for id in ids {
                    self.request(
                        Pending::ExportSession,
                        Request::ExportSession { id, format: None },
                    );
                }
                self.notes.push(format!("exporting {n} Sessions"));
                self.marked.clear();
            }
            KeyCode::Char('t') => {
                self.bulk = Some((BulkAction::Tag, ids));
                self.view = View::Chat;
                self.input.clear();
                self.notes.push(format!(
                    "Type tags to add to {n} Sessions, then Enter (Esc cancels)."
                ));
            }
            _ => {
                self.bulk = Some((BulkAction::Delete, ids));
                self.view = View::Chat;
                self.input.clear();
                self.notes.push(format!(
                    "Type delete and Enter to delete {n} Sessions (Esc cancels)."
                ));
            }
        }
    }

    /// The typed half of a bulk action.
    fn finish_bulk(&mut self, action: BulkAction, ids: Vec<String>, text: &str) {
        match action {
            BulkAction::Delete if text.trim() == "delete" => {
                for id in &ids {
                    self.fire(Request::DeleteSession { id: id.clone() });
                }
                self.notes.push(format!("deleted {} Sessions", ids.len()));
                self.request(Pending::ListSessions, Request::ListSessions);
            }
            BulkAction::Delete => self
                .notes
                .push("not deleted (type delete to confirm)".into()),
            BulkAction::Tag => {
                let add: Vec<String> = text
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .map(|t| t.trim_start_matches('#').to_lowercase())
                    .filter(|t| !t.is_empty())
                    .collect();
                for s in self
                    .sessions
                    .iter()
                    .filter(|s| ids.contains(&s.id))
                    .cloned()
                    .collect::<Vec<_>>()
                {
                    let mut tags = s.tags.clone();
                    tags.extend(add.iter().cloned());
                    self.request(Pending::SetTags, Request::SetSessionTags { id: s.id, tags });
                }
            }
        }
        self.marked.clear();
    }

    /// `/rename` edits the current name, `/rename NAME` sets it, and
    /// `/rename auto` suggests one from the recent messages.
    fn rename_cmd(&mut self, arg: &str) {
        let Some(id) = self.current.clone() else {
            self.notes.push("no current session".into());
            return;
        };
        match arg {
            "" => {
                let name = self.session_name(&id);
                self.start_rename(&id, &name);
            }
            "auto" => {
                self.notes.push("suggesting a name…".into());
                self.request(Pending::SuggestName, Request::SuggestSessionName { id });
            }
            name => self.fire(Request::RenameSession {
                id,
                name: name.to_string(),
            }),
        }
    }

    /// `/template save NAME [FIRST MESSAGE]` or `/template delete NAME`.
    fn template_cmd(&mut self, args: &str) {
        let mut parts = args.splitn(3, ' ');
        let (verb, name, rest) = (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default().trim().to_string(),
            parts.next().map(|r| r.trim().to_string()),
        );
        if name.is_empty() {
            self.notes
                .push("usage: /template save NAME [FIRST MESSAGE] · /template delete NAME".into());
            return;
        }
        match verb {
            "save" => match self.current.clone() {
                Some(id) => self.request(
                    Pending::SaveTemplate,
                    Request::SaveSessionTemplate {
                        id,
                        name,
                        first_message: rest.filter(|r| !r.is_empty()),
                    },
                ),
                None => self.notes.push("no current session".into()),
            },
            "delete" | "rm" => {
                self.fire(Request::DeleteSessionTemplate { name: name.clone() });
                self.notes.push(format!("deleted template {name}"));
            }
            _ => self
                .notes
                .push("usage: /template save NAME [FIRST MESSAGE] · /template delete NAME".into()),
        }
    }

    /// Open a Session the daemon just made.
    fn adopt_new_session(&mut self, s: SessionInfo) {
        // A new Session works where the TUI started.
        if s.cwd.is_none() {
            if let Ok(dir) = std::env::current_dir() {
                self.request(
                    Pending::SetCwd,
                    Request::SetSessionCwd {
                        id: s.id.clone(),
                        cwd: Some(dir.to_string_lossy().into_owned()),
                    },
                );
            }
        }
        self.switch_session(s.id.clone());
        self.sessions.insert(0, s);
        self.messages.clear();
        self.view = View::Chat;
    }

    /// `/instructions` edits this Session's standing instructions in the
    /// input, `/instructions TEXT` sets them, `/instructions clear` clears.
    fn instructions_cmd(&mut self, arg: &str) {
        let Some(id) = self.current.clone() else {
            self.notes.push("no current session".into());
            return;
        };
        match arg {
            "" => {
                let text = self
                    .sessions
                    .iter()
                    .find(|s| s.id == id)
                    .and_then(|s| s.instructions.clone())
                    .unwrap_or_default();
                self.instructing = Some(id);
                self.view = View::Chat;
                self.input.set(&text);
                self.notes.push(
                    "Edit this Session's standing instructions, then Enter (empty clears, Esc cancels)."
                        .into(),
                );
            }
            "clear" => self.set_instructions(id, ""),
            text => self.set_instructions(id, text),
        }
    }

    fn set_instructions(&mut self, id: String, text: &str) {
        let text = text.trim();
        self.fire(Request::SetSessionInstructions {
            id,
            instructions: Some(text.to_string()).filter(|t| !t.is_empty()),
        });
        self.notes.push(if text.is_empty() {
            "instructions cleared".into()
        } else {
            "instructions set; the agent follows them from its next step".into()
        });
    }

    /// Rename Session `id` in the input, starting from `name` (Esc
    /// cancels).
    fn start_rename(&mut self, id: &str, name: &str) {
        self.renaming = Some(id.to_string());
        self.view = View::Chat;
        self.input.set(name);
        self.notes
            .push("Edit the name, then Enter (Esc cancels).".into());
    }

    /// Sessions matching the switcher's query, best first (newest first
    /// with no query): fuzzy on name, tags and the last message.
    fn switcher_matches(&self, query: &str) -> Vec<usize> {
        let mut scored: Vec<(i64, usize)> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.archived)
            .filter_map(|(i, s)| {
                let fields = [
                    s.name.clone(),
                    s.tags
                        .iter()
                        .map(|t| format!("#{t}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                    s.preview.clone().unwrap_or_default(),
                ];
                fields
                    .iter()
                    .enumerate()
                    .filter_map(|(k, f)| {
                        // The name counts most, then tags, then the preview.
                        formless_core::fuzzy::fuzzy_match(query, f)
                            .map(|(sc, _)| sc - 20 * k as i64)
                    })
                    .max()
                    .map(|sc| (sc, i))
            })
            .collect();
        if query.trim().is_empty() {
            scored.sort_by(|a, b| {
                self.sessions[b.1]
                    .updated_at
                    .cmp(&self.sessions[a.1].updated_at)
            });
        } else {
            scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        }
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// Ctrl+K switcher keys: type to filter, ↑/↓, Enter switches, Ctrl+N
    /// starts a Session named after the query, Esc closes.
    fn switcher_key(&mut self, key: KeyEvent) {
        let Some((query, sel)) = self.switcher.clone() else {
            return;
        };
        let matches = self.switcher_matches(&query);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.switcher = None,
            KeyCode::Char('n') if ctrl => {
                let name = Some(query.trim().to_string()).filter(|q| !q.is_empty());
                self.switcher = None;
                self.request(Pending::CreateSession, Request::CreateSession { name });
            }
            KeyCode::Enter => {
                self.switcher = None;
                if let Some(&i) = matches.get(sel.min(matches.len().saturating_sub(1))) {
                    let id = self.sessions[i].id.clone();
                    self.switch_session(id.clone());
                    self.request(Pending::GetSession, Request::GetSession { id });
                    self.view = View::Chat;
                }
            }
            KeyCode::Up => self.switcher = Some((query, sel.saturating_sub(1))),
            KeyCode::Down => {
                self.switcher = Some((query, (sel + 1).min(matches.len().saturating_sub(1))))
            }
            KeyCode::Backspace => {
                let mut q = query;
                q.pop();
                self.switcher = Some((q, 0));
            }
            KeyCode::Char(c) if !ctrl => self.switcher = Some((format!("{query}{c}"), 0)),
            _ => {}
        }
    }

    /// The Session to open on launch (SPEC §10.1): the most recently
    /// active one (not the first row: pinned Sessions list first), or none
    /// when starting fresh.
    fn start_session(&self) -> Option<SessionInfo> {
        if self.on_start == "new" {
            return None;
        }
        self.sessions
            .iter()
            .filter(|s| !s.archived)
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
            .cloned()
    }

    /// Pin or unpin a Session (SPEC §10.4).
    fn toggle_pin(&mut self, s: &SessionInfo) {
        self.request(
            Pending::SetPin,
            Request::SetSessionPinned {
                id: s.id.clone(),
                pinned: !s.pinned,
            },
        );
    }

    /// The current Session's working directory, else the TUI's own
    /// (SPEC §5.1).
    fn work_dir(&self) -> std::path::PathBuf {
        self.current
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| &s.id == id))
            .and_then(|s| s.cwd.clone())
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    /// `/cd [PATH]`: show or change the Session's working directory.
    fn change_dir(&mut self, arg: &str) {
        let Some(id) = self.current.clone() else {
            self.notes.push("no current session".into());
            return;
        };
        if arg.is_empty() {
            self.notes
                .push(format!("working directory: {}", self.work_dir().display()));
            return;
        }
        let target = match arg.strip_prefix('~') {
            Some(rest) => std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(rest.trim_start_matches('/')),
            None => self.work_dir().join(arg),
        };
        // Cached `file://` links were relative to the old directory.
        self.path_cache.clear();
        self.request(
            Pending::SetCwd,
            Request::SetSessionCwd {
                id,
                cwd: Some(target.to_string_lossy().into_owned()),
            },
        );
    }

    /// Fetch a Session's Runs for the answer footers.
    fn request_session_runs(&mut self, session_id: &str) {
        self.request(
            Pending::SessionRuns,
            Request::RunMetrics {
                limit: Some(500),
                session_id: Some(session_id.to_string()),
            },
        );
    }

    /// `/panes wider|narrower|left|right|reset`: side pane widths (SPEC
    /// §10.2), saved in `tui.pane_widths`.
    fn resize_panes(&mut self, how: &str) {
        let (l, r) = self.pane_widths;
        let grow = |w: u16, by: i16| -> u16 {
            if w == 0 {
                0
            } else {
                (w as i16 + by).clamp(10, 40) as u16
            }
        };
        self.pane_widths = match how {
            "wider" => (grow(l, 3), grow(r, 3)),
            "narrower" => (grow(l, -3), grow(r, -3)),
            "left" => (if l == 0 { 22 } else { 0 }, r),
            "right" => (l, if r == 0 { 26 } else { 0 }),
            "reset" => (22, 26),
            _ => {
                self.notes
                    .push("usage: /panes [wider|narrower|left|right|reset]".into());
                return;
            }
        };
        let (l, r) = self.pane_widths;
        self.fire(Request::SetSetting {
            key: "tui.pane_widths".into(),
            value: serde_json::json!([l, r]),
        });
        if !self.multi_pane {
            self.notes
                .push("(the multi-pane layout is off: /panes turns it on)".into());
        }
        self.notes.push(format!(
            "panes: left {} · chat {}% · right {} (remembered)",
            if l == 0 {
                "hidden".to_string()
            } else {
                format!("{l}%")
            },
            100 - l - r,
            if r == 0 {
                "hidden".to_string()
            } else {
                format!("{r}%")
            },
        ));
    }

    /// `/open link [N]`: the Nth link in the last answer (default the
    /// first) with the system opener.
    fn open_link(&mut self, arg: &str) {
        let urls: Vec<String> = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .map(|m| {
                links::find_urls(&m.content)
                    .into_iter()
                    .map(|(_, _, u)| u)
                    .collect()
            })
            .unwrap_or_default();
        let n = arg.parse::<usize>().unwrap_or(1);
        match n.checked_sub(1).and_then(|i| urls.get(i)) {
            Some(url) => {
                let url = url.clone();
                if cfg!(test) {
                    self.notes.push(format!("opening {url}"));
                    return;
                }
                match links::open_external(&url) {
                    Ok(()) => self.notes.push(format!("opening {url}")),
                    Err(e) => self.notes.push(format!("couldn't open {url}: {e}")),
                }
            }
            None => self
                .notes
                .push(format!("the last answer has {} link(s)", urls.len())),
        }
    }

    /// `/pager code [N]`: the last answer's Nth code block (default the
    /// last) unwrapped in the pager, with line numbers and ←/→ scrolling.
    fn open_code(&mut self, arg: &str) {
        let blocks = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .map(|m| code_blocks(&m.content))
            .unwrap_or_default();
        let n = arg.parse::<usize>().unwrap_or(blocks.len());
        let Some(block) = n.checked_sub(1).and_then(|i| blocks.get(i)) else {
            self.notes.push(format!(
                "the last answer has {} code block(s)",
                blocks.len()
            ));
            return;
        };
        let lines: Vec<&str> = block.lines().collect();
        let w = lines.len().to_string().len();
        self.pager = Some(Pager {
            title: format!("code block {n} of {}", blocks.len()),
            lines: lines
                .iter()
                .enumerate()
                .map(|(i, l)| format!("{:>w$} │ {}", i + 1, l.replace('\t', "    ")))
                .collect(),
            top: 0,
            left: 0,
            styled: Vec::new(),
        });
    }

    /// `/pager diagram [N]` (full size, ←/→ scroll) and `/open diagram
    /// [N]` (mermaid.live): the last answer's Mermaid block numbered N
    /// like its `[N]` label, or its last one.
    fn open_diagram(&mut self, arg: &str, browser: bool) {
        let blocks = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .map(|m| fenced_blocks(&m.content))
            .unwrap_or_default();
        let pick = match arg.parse::<usize>() {
            Ok(n) => blocks
                .get(n.wrapping_sub(1))
                .filter(|(l, _)| l == "mermaid"),
            Err(_) => blocks.iter().rev().find(|(l, _)| l == "mermaid"),
        };
        let Some((_, src)) = pick.cloned() else {
            self.notes
                .push("no Mermaid diagram with that number in the last answer".into());
            return;
        };
        if browser {
            let url = mermaid_live_url(&src);
            if cfg!(test) {
                self.notes.push(format!("opening {url}"));
            } else if let Err(e) = links::open_external(&url) {
                self.notes.push(format!("couldn't open mermaid.live: {e}"));
            }
            return;
        }
        match mermaid::render(&src, self.color, 10_000) {
            Some(d) => {
                let plain: Vec<String> = d
                    .lines
                    .iter()
                    .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                    .collect();
                self.pager = Some(Pager {
                    title: format!("mermaid · {}", d.kind),
                    lines: plain,
                    top: 0,
                    left: 0,
                    styled: d.lines,
                });
            }
            None => self.notes.push(
                "this diagram can't be drawn here; /open diagram shows it in the browser".into(),
            ),
        }
    }

    /// `/table [N]`: the last answer's Nth table at full width in the
    /// pager, scrolled sideways with ←/→ (SPEC §10.2).
    fn open_table(&mut self, arg: &str) {
        let Some(last) = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
        else {
            self.notes.push("no answer with a table yet".into());
            return;
        };
        let tables = markdown_tables(&last.content);
        let n = arg.parse::<usize>().unwrap_or(1);
        let Some(table) = n.checked_sub(1).and_then(|i| tables.get(i)) else {
            self.notes
                .push(format!("the last answer has {} table(s)", tables.len()));
            return;
        };
        let lines = markdown::render(table, false, "", 100_000)
            .into_iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        self.pager = Some(Pager {
            title: format!("table {n} of {}", tables.len()),
            lines,
            top: 0,
            left: 0,
            styled: Vec::new(),
        });
    }

    /// Copy to the clipboard: OSC 52 (works over SSH) and, locally, the
    /// system clipboard too.
    fn copy_text(&mut self, text: &str, what: &str) {
        let mut out = stdout();
        let _ = out.write_all(osc52(text).as_bytes());
        let _ = out.flush();
        if std::env::var_os("SSH_CONNECTION").is_none() && !cfg!(test) {
            let _ = formless_runtime::clipboard_write(text);
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

/// Example tasks on an empty Session (SPEC §10.2); 1–4 put one in the
/// input.
const EXAMPLES: [&str; 4] = [
    "Find every TODO in this repo and group them by file",
    "Summarise https://example.com in three bullet points",
    "Every weekday at 9:00, list the top 5 stories on news.ycombinator.com",
    "What can you do, and which Tools have you built so far?",
];

fn welcome_lines(color: bool, height: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = (0..height.saturating_sub(14) / 2)
        .map(|_| Line::raw(""))
        .collect();
    lines.push(Line::styled("◇ Formless", bold_fg(color, Color::Cyan)).centered());
    lines.push(
        Line::styled(
            "It builds the Tools a task needs and keeps them, so it gets more capable the more you use it.",
            ui::dim(color),
        )
        .centered(),
    );
    lines.push(Line::raw(""));
    lines.push(Line::styled("Try one — press its number:", fg(color, Color::Gray)).centered());
    for (i, example) in EXAMPLES.iter().enumerate() {
        lines.push(
            Line::from(vec![
                Span::styled(format!("{} ", i + 1), bold_fg(color, Color::Cyan)),
                Span::styled(example.to_string(), fg(color, Color::Gray)),
            ])
            .centered(),
        );
    }
    lines.push(Line::raw(""));
    lines.push(
        Line::styled(
            "/help commands · /model switch model · @ reference a file · Tab views",
            ui::dim(color),
        )
        .centered(),
    );
    lines
}

/// The keys for a focused message, under it.
fn focus_hint(color: bool, user: bool) -> Line<'static> {
    Line::styled(
        if user {
            "  ▲ e edit & re-run from here · f fork from here · c copy · C Markdown · [ ] move · Esc"
        } else {
            "  ▲ c copy · C copy as Markdown · [ ] move · Esc"
        },
        bold_fg(color, Color::Yellow),
    )
}

/// The footer under each finished Run's last message (SPEC §10.2), by
/// message index: a Run's messages are those before the next Run began.
fn run_footers(app: &App) -> HashMap<usize, String> {
    let mut runs: Vec<&RunMetricsInfo> = app
        .session_runs
        .iter()
        .filter(|r| r.status != "running")
        .collect();
    runs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let latest = runs.last().map(|r| r.run_id.clone());
    let mut out = HashMap::new();
    for (k, run) in runs.iter().enumerate() {
        let until = runs.get(k + 1).map(|r| r.created_at.as_str());
        let last = app.messages.iter().rposition(|m| {
            m.created_at >= run.created_at && until.is_none_or(|u| m.created_at.as_str() < u)
        });
        let Some(i) = last else { continue };
        let took = format_elapsed(Duration::from_millis(run.duration_ms.max(0) as u64));
        let mut text = match run.status.as_str() {
            "completed" => format!("✓ {took}"),
            "interrupted" => format!("■ interrupted after {took}"),
            "paused" => format!("⏸ paused after {took}"),
            other => format!("{other} · {took}"),
        };
        text.push_str(&format!(
            " · {} step{} · {} tool{}",
            run.steps,
            if run.steps == 1 { "" } else { "s" },
            run.tool_calls,
            if run.tool_calls == 1 { "" } else { "s" }
        ));
        // Cost is known for the Run just finished (from RunUsage).
        let cost = app
            .current
            .as_ref()
            .and_then(|s| app.run_usage.get(s))
            .and_then(|u| u.cost)
            .filter(|_| latest.as_deref() == Some(run.run_id.as_str()));
        if let Some(c) = cost {
            text.push_str(&format!(" · ${c:.2}"));
        }
        out.insert(i, text);
    }
    out
}

/// A persisted tool call: (call id, name, raw arguments).
fn persisted_calls(meta: &str) -> Vec<(String, String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(meta) else {
        return Vec::new();
    };
    v["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            (
                c["id"].as_str().unwrap_or_default().to_string(),
                c["function"]["name"].as_str().unwrap_or("tool").to_string(),
                c["function"]["arguments"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The call id a `tool` message answers.
fn result_call_id(m: &MessageInfo) -> Option<String> {
    let meta: serde_json::Value = serde_json::from_str(m.meta.as_deref()?).ok()?;
    meta["tool_call_id"].as_str().map(str::to_string)
}

/// JSON pretty-printed (or the text as is), highlighted, capped.
fn code_block_lines(text: &str, indent: &str, color: bool, max: usize) -> Vec<Line<'static>> {
    let pretty = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok());
    let (body, lang) = match &pretty {
        Some(p) => (p.as_str(), Some("json")),
        None => (text.trim_end(), None),
    };
    let mut hl = lang.and_then(|l| highlight::Highlighter::new(l, color));
    let all: Vec<&str> = body.lines().collect();
    let mut out: Vec<Line<'static>> = all
        .iter()
        .take(max)
        .map(|l| {
            let mut spans = vec![Span::raw(indent.to_string())];
            match &mut hl {
                Some(h) => spans.extend(h.line(l)),
                None => spans.push(Span::styled(l.to_string(), ui::dim(color))),
            }
            Line::from(spans)
        })
        .collect();
    if all.len() > max {
        out.push(Line::styled(
            format!("{indent}… {} more lines", all.len() - max),
            ui::dim(color),
        ));
    }
    out
}

/// The unified diff in a Tool result: the text itself, or the first
/// string field of a JSON object that holds one.
fn result_diff(result: &str) -> Option<String> {
    if highlight::is_unified_diff(result) {
        return Some(result.to_string());
    }
    let v: serde_json::Value = serde_json::from_str(result).ok()?;
    v.as_object()?
        .values()
        .filter_map(|f| f.as_str())
        .find(|s| highlight::is_unified_diff(s))
        .map(str::to_string)
}

/// One persisted call: collapsed to a line (name, input summary and the
/// result's status), or expanded to its full input and result.
fn tool_call_block(
    app: &App,
    (id, name, args): &(String, String, String),
    result: Option<&str>,
) -> Vec<Line<'static>> {
    let focused = app.call_focus.as_deref() == Some(id.as_str());
    let expanded = app.expanded_calls.contains(id);
    let failed = result.is_some_and(|r| {
        r.contains("\"ok\":false") || r.trim_start().to_lowercase().starts_with("error")
    });
    let (mark, mark_style) = match result {
        None => ("…", ui::dim(app.color)),
        Some(_) if failed => ("✗", bold_fg(app.color, Color::Red)),
        Some(_) => ("✓", fg(app.color, Color::Green)),
    };
    let head_style = if focused {
        bold_fg(app.color, Color::Yellow).add_modifier(Modifier::REVERSED)
    } else {
        fg(app.color, Color::Yellow)
    };
    let mut head = vec![
        Span::styled(
            format!("  {} ⚙ {name}", if expanded { "▾" } else { "▸" }),
            head_style,
        ),
        Span::raw(" "),
        Span::styled(mark, mark_style),
    ];
    if !expanded {
        head.push(Span::styled(
            format!("  {}", ui::summarize_args(args, 120)),
            ui::dim(app.color),
        ));
        if let Some((added, removed)) = result
            .and_then(result_diff)
            .map(|d| highlight::diff_stat(&d))
        {
            head.push(Span::styled(
                format!("  +{added}"),
                fg(app.color, Color::Green),
            ));
            head.push(Span::styled(
                format!(" −{removed}"),
                fg(app.color, Color::Red),
            ));
        } else if let Some(r) = result {
            head.push(Span::styled(
                format!(
                    "  → {}",
                    truncate_chars(r.trim().lines().next().unwrap_or_default(), 80)
                ),
                ui::dim(app.color),
            ));
            if let Some(a) = formless_protocol::artifact_refs(r).first() {
                head.push(Span::styled(
                    format!("  /open {}", &a[..8]),
                    fg(app.color, Color::Cyan),
                ));
            }
        }
        return vec![Line::from(head)];
    }
    let mut out = vec![
        Line::from(head),
        Line::styled("    input", ui::dim(app.color)),
    ];
    out.extend(code_block_lines(args, "      ", app.color, 200));
    match result {
        Some(r) => {
            out.push(Line::styled(
                if failed { "    error" } else { "    result" },
                if failed {
                    fg(app.color, Color::Red)
                } else {
                    ui::dim(app.color)
                },
            ));
            match result_diff(r) {
                Some(diff) => out.extend(highlight::diff_lines(&diff, app.color).into_iter().map(
                    |row| {
                        let mut spans = vec![Span::raw("      ")];
                        spans.extend(row);
                        Line::from(spans)
                    },
                )),
                None => out.extend(code_block_lines(r, "      ", app.color, 300)),
            }
        }
        None => out.push(Line::styled("    (no result yet)", ui::dim(app.color))),
    }
    out
}

/// The chat as drawn: its lines, the focused item's line, where each
/// user message starts (for jumping between them), and the rows reserved
/// for image thumbnails (first line, file).
#[derive(Default)]
struct ChatRender {
    lines: Vec<Line<'static>>,
    focus_line: Option<usize>,
    user_lines: Vec<usize>,
    image_lines: Vec<(usize, String)>,
    /// Persisted tool calls' header lines, for clicks.
    call_lines: Vec<(usize, String)>,
}

fn chat_lines(app: &App, width: usize, height: usize) -> ChatRender {
    let mut lines: Vec<Line> = Vec::new();
    let mut focus_line = None;
    let mut user_lines = Vec::new();
    // Rows reserved for image thumbnails: (first line, file).
    let mut image_lines: Vec<(usize, String)> = Vec::new();
    let mut call_lines: Vec<(usize, String)> = Vec::new();
    // Results by call id, so each call renders with its result and the
    // separate `tool` line is skipped.
    let results: HashMap<String, &str> = app
        .messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| result_call_id(m).map(|id| (id, m.content.as_str())))
        .collect();
    let called: HashSet<String> = app
        .messages
        .iter()
        .filter_map(|m| m.meta.as_deref())
        .flat_map(persisted_calls)
        .map(|(id, _, _)| id)
        .filter(|id| !id.is_empty())
        .collect();
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
    let footers = run_footers(app);
    let mut day = None;
    for (mi, m) in app.messages.iter().enumerate() {
        let when = Some(m.created_at.as_str());
        // A separator when the local date changes (not before the first).
        if let Some(d) = ui::local_day(&m.created_at).filter(|_| app.show_timestamps) {
            if day.is_some_and(|prev| prev != d) {
                let label = d.format("%a, %-d %b %Y").to_string();
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("──── {label} ────"),
                    ui::dim(app.color),
                ));
                group = None;
            }
            day = Some(d);
        }
        match m.role.as_str() {
            "user" => {
                user_lines.push(lines.len());
                open_group(&mut lines, &mut group, "user", when);
                if let Some(from) = sent_elsewhere(app, m) {
                    lines.push(Line::styled(
                        format!("  sent from {from}"),
                        ui::dim(app.color),
                    ));
                }
                let focused = app.call_focus.as_deref() == Some(&format!("msg:{}", m.id));
                if focused {
                    focus_line = Some(lines.len());
                }
                for l in m.content.lines() {
                    lines.push(mention_line(&truncate_chars(l, 2000), app.color));
                }
                if app.image_proto.is_some() {
                    for (name, path) in image_attachments(m) {
                        lines.push(Line::styled(format!("  🖼 {name}"), ui::dim(app.color)));
                        image_lines.push((lines.len(), path));
                        lines.extend((0..images::ROWS).map(|_| Line::raw("")));
                    }
                }
                if focused {
                    lines.push(focus_hint(app.color, true));
                }
            }
            "tool" => {
                if result_call_id(m).is_some_and(|id| called.contains(&id)) {
                    continue;
                }
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
                let focused = app.call_focus.as_deref() == Some(&format!("msg:{}", m.id));
                if focused {
                    focus_line = Some(lines.len());
                }
                lines.extend(app.markdown_cached(m, width));
                if focused {
                    lines.push(focus_hint(app.color, false));
                }
                if let Some(meta) = &m.meta {
                    for call in persisted_calls(meta) {
                        if app.call_focus.as_deref() == Some(call.0.as_str()) {
                            focus_line = Some(lines.len());
                        }
                        call_lines.push((lines.len(), call.0.clone()));
                        let result = results.get(&call.0).copied();
                        lines.extend(tool_call_block(app, &call, result));
                    }
                }
            }
        }
        if let Some(footer) = footers.get(&mi) {
            lines.push(Line::styled(format!("  {footer}"), ui::dim(app.color)));
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
        return ChatRender {
            lines: welcome_lines(app.color, height),
            ..Default::default()
        };
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
    ChatRender {
        lines,
        focus_line,
        user_lines,
        image_lines,
        call_lines,
    }
}

fn draw_chat(f: &mut Frame, area: Rect, app: &mut App) {
    app.chat_area = Some(area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    }));
    let height = (area.height.saturating_sub(2) as usize).max(1);
    let width = area.width.saturating_sub(2);
    let ChatRender {
        mut lines,
        focus_line,
        user_lines,
        image_lines,
        call_lines,
    } = chat_lines(app, width as usize, height);
    // Find (Ctrl+F): highlight matches; the current one stands out.
    let mut match_lines = Vec::new();
    if let Some(find) = app.find.as_ref().filter(|f| !f.query.is_empty()) {
        let current = find.current;
        for (i, line) in lines.iter_mut().enumerate() {
            let here = match_lines.len();
            let (lit, n) = highlight_matches(std::mem::take(line), &find.query, |k| {
                if here + k == current {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::LightYellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Black).bg(Color::Yellow)
                }
            });
            *line = lit;
            match_lines.extend(std::iter::repeat_n(i, n));
        }
    }
    // Wrapped row where each line starts: for user messages (`{`/`}`,
    // Ctrl+↑/↓) and find matches.
    let mut line_rows = Vec::with_capacity(lines.len());
    let mut row = 0;
    for l in &lines {
        line_rows.push(row);
        row += Paragraph::new(l.clone())
            .wrap(Wrap { trim: false })
            .line_count(width)
            .max(1);
    }
    app.user_rows = user_lines.iter().map(|&i| line_rows[i]).collect();
    let image_rows: Vec<(usize, String)> = image_lines
        .into_iter()
        .map(|(i, p)| (line_rows[i], p))
        .collect();
    if let Some(find) = &mut app.find {
        find.rows = match_lines.iter().map(|&i| line_rows[i]).collect();
    }
    // Bring a newly focused tool call into view (rows before it, wrapped).
    let focus_row = focus_line
        .filter(|_| std::mem::take(&mut app.scroll_to_focus))
        .map(|i| {
            Paragraph::new(lines[..i].to_vec())
                .wrap(Wrap { trim: false })
                .line_count(width)
        });
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    // Scroll by rendered (wrapped) rows so the newest output stays visible.
    let rows = paragraph.line_count(width);
    app.chat_view_height = height;
    app.chat_line_count = rows;
    let max_top = rows.saturating_sub(height);
    let mut find_row = None;
    if let Some(f) = app.find.as_mut() {
        if std::mem::take(&mut f.scroll) {
            // A new query starts at the match nearest the bottom.
            if f.current >= f.rows.len() {
                f.current = f.rows.len().saturating_sub(1);
            }
            find_row = f.rows.get(f.current).copied();
        }
    }
    if let Some(row) = focus_row.or(find_row) {
        let top = app.chat_scroll_top.unwrap_or(max_top);
        if row < top || row >= top + height {
            app.chat_scroll_top = Some(row.saturating_sub(height / 3).min(max_top));
        }
    }
    let scroll_top = match app.chat_scroll_top {
        Some(top) => top.min(max_top),
        None => max_top,
    };
    if app.chat_scroll_top == Some(scroll_top) && scroll_top >= max_top {
        app.chat_scroll_top = None;
    }
    // Output that arrived while scrolled up (SPEC §10.2).
    match app.chat_scroll_top {
        None => app.unseen_from = None,
        Some(_) if app.unseen_from.is_none() => app.unseen_from = Some(rows),
        Some(_) => {}
    }
    let unseen = app.unseen_from.map_or(0, |from| rows.saturating_sub(from));
    let mut block = ui::panel(" Chat ", app.color, Color::DarkGray);
    app.new_badge = None;
    if unseen > 0 {
        let label = format!(
            " ↓ {unseen} new line{} · End ",
            if unseen == 1 { "" } else { "s" }
        );
        let w = Span::raw(label.as_str()).width() as u16;
        let x1 = area.right().saturating_sub(2);
        app.new_badge = Some((area.bottom().saturating_sub(1), x1.saturating_sub(w), x1));
        block = block.title_bottom(
            Line::styled(
                label,
                bold_fg(app.color, Color::Black).bg(if app.color {
                    Color::Yellow
                } else {
                    Color::Reset
                }),
            )
            .right_aligned(),
        );
    }
    if app.chat_scroll_top.is_some() {
        block = block.title(
            Line::styled(
                {
                    let here = app
                        .user_rows
                        .iter()
                        .filter(|&&r| r <= scroll_top + height / 3)
                        .count();
                    let of = app.user_rows.len();
                    let msg = if of > 0 {
                        format!("message {}/{of} · ", here.max(1))
                    } else {
                        String::new()
                    };
                    // The bottom visible line, of all of them.
                    let line = (scroll_top + height).min(rows);
                    format!(
                        " {msg}line {line}/{rows} · {}% · End to follow ",
                        line * 100 / rows.max(1)
                    )
                },
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
    // A click on a call's header toggles it.
    for (i, id) in &call_lines {
        let row = line_rows[*i];
        if row >= scroll_top && row < scroll_top + height {
            let y = area.y + 1 + (row - scroll_top) as u16;
            clickable(
                app,
                Rect::new(area.x + 1, y, width, 1),
                Click::Call(id.clone()),
            );
        }
    }
    // Thumbnails whose rows are all on screen (drawn after the frame).
    app.image_placed = image_rows
        .into_iter()
        .filter(|(row, _)| *row >= scroll_top && row + images::ROWS as usize <= scroll_top + height)
        .map(|(row, path)| images::Placed {
            x: area.x + 3,
            y: area.y + 1 + (row - scroll_top) as u16,
            cols: images::MAX_COLS.min(width.saturating_sub(4)),
            path,
        })
        .collect();
    // A scrollbar on the right border when there's more than a screen.
    if max_top > 0 {
        let mut state = ScrollbarState::new(max_top + 1).position(scroll_top);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_style(fg(app.color, Color::Gray)),
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

fn draw_sessions(f: &mut Frame, area: Rect, app: &App) {
    let now = chrono::Utc::now();
    let name_w = (area.width as usize / 3).clamp(10, 40);
    let visible = app.visible_sessions();
    let items: Vec<ListItem> = if app.sessions.is_empty() {
        vec![ListItem::new(Line::styled(
            "No sessions yet — press n to start one.",
            ui::dim(app.color),
        ))]
    } else if visible.is_empty() {
        vec![ListItem::new(Line::styled(
            "No match — Esc clears the filter.",
            ui::dim(app.color),
        ))]
    } else {
        visible
            .iter()
            .map(|&i| &app.sessions[i])
            .map(|s| {
                let current = Some(&s.id) == app.current.as_ref();
                let (icon, style) = session_state(app, s);
                let mut meta = format!(
                    "{} · {} msg{}",
                    ui::relative_time(&s.updated_at, now),
                    s.message_count,
                    if s.message_count == 1 { "" } else { "s" }
                );
                if let Some(m) = s.model.as_deref().filter(|m| !m.is_empty()) {
                    meta.push_str(&format!(" · {m}"));
                }
                if let Some(d) = &s.cwd {
                    meta.push_str(&format!(" · {}", short_dir(d)));
                }
                if !s.tags.is_empty() {
                    meta.push_str(&format!(" · #{}", s.tags.join(" #")));
                }
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if app.marked.contains(&s.id) {
                            "✓ "
                        } else if current {
                            "● "
                        } else {
                            "  "
                        },
                        bold_fg(app.color, Color::Cyan),
                    ),
                    Span::styled(format!("{icon} "), style),
                    Span::styled(
                        if s.pinned { "★ " } else { "  " },
                        fg(app.color, Color::Yellow),
                    ),
                    Span::raw(ui::pad(&s.name, name_w)),
                    Span::styled(format!(" {meta}"), ui::dim(app.color)),
                    Span::styled(
                        s.preview
                            .as_deref()
                            .map(|p| format!("  “{p}”"))
                            .unwrap_or_default(),
                        fg(app.color, Color::Gray),
                    ),
                ]))
            })
            .collect()
    };
    let mut state = ListState::default();
    if !visible.is_empty() {
        state.select(Some(app.session_idx.min(visible.len() - 1)));
    }
    let count = visible.len();
    f.render_stateful_widget(
        List::new(items)
            .block(ui::panel(
                {
                    let mut t = app.session_filter.title(
                        "Sessions",
                        visible.len(),
                        app.sessions.len(),
                        lists::SESSION_SORTS,
                        lists::SESSION_QUICK,
                    );
                    if !app.marked.is_empty() {
                        t.push_str(&format!(
                            "· {} selected: a archive · e export · t tag · d delete · Esc clear ",
                            app.marked.len()
                        ));
                    }
                    t
                },
                app.color,
                Color::Cyan,
            ))
            .highlight_style(bold_fg(app.color, Color::Cyan).add_modifier(Modifier::REVERSED))
            .highlight_symbol("› "),
        area,
        &mut state,
    );
    clickable_rows(app, area, 0, state.offset(), count, Click::Session);
}

fn draw_settings(f: &mut Frame, area: Rect, app: &App) {
    let rows = app.settings_rows();
    let sel = app.settings_sel.min(rows.len().saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0;
    let mut section = "";
    for (i, row) in rows.iter().enumerate() {
        if row.section != section {
            if !section.is_empty() {
                lines.push(Line::raw(""));
            }
            section = row.section;
            lines.push(Line::styled(section, bold_fg(app.color, Color::Cyan)));
        }
        let selected = i == sel;
        if selected {
            sel_line = lines.len();
        }
        let mark = if selected { "› " } else { "  " };
        let label_style = if selected {
            bold_fg(app.color, Color::Green)
        } else {
            ui::dim(app.color)
        };
        let value = match &app.settings_edit {
            Some((k, edit)) if *k == row.key => {
                Span::styled(format!("{}▏", &**edit), bold_fg(app.color, Color::Yellow))
            }
            _ if row.value.is_empty() && matches!(row.kind, settings::Kind::Choice(_)) => {
                Span::styled("default", ui::dim(app.color))
            }
            _ if row.value.is_empty() => Span::raw(""),
            _ => Span::raw(row.value.clone()),
        };
        let mut spans = vec![
            Span::styled(format!("{mark}{}", ui::pad(&row.label, 20)), label_style),
            value,
        ];
        if selected {
            spans.push(Span::styled(
                format!("   {}", row.hint()),
                ui::dim(app.color),
            ));
        }
        lines.push(Line::from(spans));
        if let Some((k, e)) = &app.settings_error {
            if *k == row.key {
                lines.push(Line::styled(
                    format!("    ✗ {e}"),
                    fg(app.color, Color::Red),
                ));
            }
        }
    }
    if rows.is_empty() {
        lines.push(Line::styled(
            "config.toml could not be read",
            bold_fg(app.color, Color::Red),
        ));
    }
    // Keep the selection on screen.
    let height = area.height.saturating_sub(2) as usize;
    let top = sel_line.saturating_sub(height.saturating_sub(2));
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(top).collect::<Vec<_>>()).block(ui::panel(
            " Settings ",
            app.color,
            Color::Cyan,
        )),
        area,
    );
    if let Some(form) = &app.settings_form {
        draw_settings_form(f, form, app.color);
    }
}

fn draw_settings_form(f: &mut Frame, form: &settings::Form, color: bool) {
    let mut lines = vec![Line::raw("")];
    for (i, field) in form.fields.iter().enumerate() {
        let active = i == form.idx;
        let text = if field.masked {
            "•".repeat(field.input.chars().count())
        } else {
            field.input.to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} {:<12}", if active { "›" } else { " " }, field.label),
                if active {
                    bold_fg(color, Color::Green)
                } else {
                    ui::dim(color)
                },
            ),
            Span::raw(if active { format!("{text}▏") } else { text }),
        ]));
        if active {
            lines.push(Line::styled(
                format!("               {}", field.hint),
                ui::dim(color),
            ));
        }
    }
    if let Some(e) = &form.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(format!("  ✗ {e}"), fg(color, Color::Red)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Tab/↑↓ field · Enter next, then save · Esc cancel",
        ui::dim(color),
    ));
    let area = f.area();
    let w = 72.min(area.width.saturating_sub(2)).max(1);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 3,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .block(ui::panel(format!(" {} ", form.title), color, Color::Cyan))
            .wrap(Wrap { trim: false }),
        rect,
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
            .block(ui::panel(
                format!(" Search ({} hits) ", app.search_hits.len()),
                app.color,
                Color::Magenta,
            ))
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

/// Success trend: the last 20 finished calls against the 20 before, when
/// both have at least 5 (↑/↓ for a 10-point change or more).
fn tool_trend(recent: &str) -> Option<char> {
    let outcomes: Vec<bool> = recent.chars().map(|c| c == 's').collect();
    let split = outcomes.len().saturating_sub(20);
    let (before, now) = outcomes.split_at(split);
    if before.len() < 5 || now.len() < 5 {
        return None;
    }
    let rate = |w: &[bool]| w.iter().filter(|s| **s).count() as f64 / w.len() as f64;
    match rate(now) - rate(before) {
        d if d >= 0.1 => Some('↑'),
        d if d <= -0.1 => Some('↓'),
        _ => None,
    }
}

/// The last 20 finished calls, one bar each (tall green success, short
/// red failure), then the trend arrow.
fn tool_spark(t: &ToolInfo, color: bool) -> Line<'static> {
    let skip = t.recent.chars().count().saturating_sub(20);
    let mut spans: Vec<Span> = t
        .recent
        .chars()
        .skip(skip)
        .map(|c| match c {
            's' => Span::styled("▆", fg(color, Color::Green)),
            _ => Span::styled("▂", bold_fg(color, Color::Red)),
        })
        .collect();
    match tool_trend(&t.recent) {
        Some('↑') => spans.push(Span::styled(" ↑", bold_fg(color, Color::Green))),
        Some(_) => spans.push(Span::styled(" ↓", bold_fg(color, Color::Red))),
        None => {}
    }
    Line::from(spans)
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
    let visible = app.visible_proposals();
    let mut title = app.proposal_filter.title(
        "Prompt proposals",
        visible.len(),
        app.proposals.len(),
        lists::PROPOSAL_SORTS,
        lists::PROPOSAL_QUICK,
    );
    if app.proposal_generating {
        title.push_str(&format!("· {} generating… ", app.spinner()));
    }
    let block = ui::panel(title, app.color, Color::Blue);
    if app.proposals.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                "No proposals yet. Press n (or /propose) to generate a measured one.",
                ui::dim(app.color),
            ))
            .block(block),
            area,
        );
        return;
    }
    let rows: Vec<Row> = visible
        .iter()
        .map(|&i| &app.proposals[i])
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
    state.select(Some(app.proposal_idx.min(visible.len().saturating_sub(1))));
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
    clickable_rows(app, area, 1, state.offset(), visible.len(), Click::Proposal);
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
    let visible = app.visible_tools();
    let list_h = (visible.len().max(1) as u16 + 3)
        .min(area.height / 2)
        .max(4);
    let parts = Layout::vertical([Constraint::Length(list_h), Constraint::Min(3)]).split(area);
    let title = app.tool_filter.title(
        "Tools",
        visible.len(),
        app.tools.len(),
        lists::TOOL_SORTS,
        lists::TOOL_QUICK,
    );
    let block = ui::panel(title, app.color, Color::Yellow);
    if app.tools.is_empty() || visible.is_empty() {
        let empty = if app.tools.is_empty() {
            NO_TOOLS
        } else {
            "No match — Esc clears the filter."
        };
        f.render_widget(
            Paragraph::new(Line::styled(empty, ui::dim(app.color))).block(block),
            parts[0],
        );
    } else {
        let mut state = TableState::default();
        state.select(Some(app.tool_idx.min(visible.len() - 1)));
        f.render_stateful_widget(
            Table::new(
                visible.iter().map(|&i| &app.tools[i]).map(|t| {
                    let mut cells = tool_cells(t, app.color);
                    cells.push(Cell::from(tool_spark(t, app.color)));
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
                    Constraint::Length(22),
                    Constraint::Length(9),
                    Constraint::Min(10),
                ],
            )
            .header(table_header(
                &[
                    "TOOL",
                    "VER",
                    "CALLS",
                    "SUCCESS",
                    "LAST 20",
                    "STATUS",
                    "CAPABILITIES",
                ],
                app.color,
            ))
            .column_spacing(2)
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› ")
            .block(block),
            parts[0],
            &mut state,
        );
        clickable_rows(app, parts[0], 1, state.offset(), visible.len(), Click::Tool);
    }
    f.render_widget(
        Paragraph::new(tool_detail_lines(app))
            .block(ui::panel(" Detail ", app.color, Color::DarkGray))
            .wrap(Wrap { trim: false }),
        parts[1],
    );
}

fn draw_approval(
    f: &mut Frame,
    req: &ApprovalReq,
    promotion: bool,
    more: usize,
    color: bool,
    focused: bool,
    explain: bool,
) {
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
        if explain {
            lines.push(Line::styled(
                format!("      {}", ui::explain_capability(cap)),
                ui::dim(color),
            ));
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
    lines.push(Line::styled(
        if focused {
            "v full code · ? explain capabilities · Tab: keep typing your message"
        } else {
            "You're typing · Tab: back to this approval"
        },
        ui::dim(color),
    ));

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
                    match (promotion, focused) {
                        (true, true) => " Promotion approval ",
                        (false, true) => " Approval required ",
                        (true, false) => " Promotion approval · Tab to answer ",
                        (false, false) => " Approval required · Tab to answer ",
                    },
                    color,
                    Color::Red,
                )
                .border_style(if focused {
                    bold_fg(color, Color::Red)
                } else {
                    ui::dim(color)
                }),
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
    // Side panes at their set widths (0 hides one; zero-size areas draw
    // nothing); the chat takes the rest.
    let (l, r) = app.pane_widths;
    let cols = Layout::horizontal([
        Constraint::Percentage(l),
        Constraint::Percentage(100 - l - r),
        Constraint::Percentage(r),
    ])
    .split(area);
    let left =
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).split(cols[0]);
    let now = chrono::Utc::now();
    let name_w = (left[0].width as usize).saturating_sub(13).max(6);
    let session_items: Vec<ListItem> = app
        .sessions
        .iter()
        .filter(|s| !s.archived)
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
        WizardStep::PickPreset | WizardStep::BaseUrl => (1, "Provider"),
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
        WizardStep::PickPreset => {
            let mut rows: Vec<String> = formless_core::presets::PRESETS
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let env = formless_core::presets::env_key(p)
                        .map(|(var, _)| format!("  (${var} found)"))
                        .unwrap_or_default();
                    format!("{}. {}{env}", i + 1, p.name)
                })
                .collect();
            rows.push(format!("{}. Custom URL…", rows.len() + 1));
            text.extend(pick(rows, w.preset_idx));
        }
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
    run_with(paths, None)
}

/// [`run`], opening `start` ("last", "new" or "picker") instead of
/// `tui.on_start` (SPEC §10.1): `formless --continue` / `--new`.
pub fn run_with(paths: Paths, start: Option<&str>) -> Result<()> {
    // Screen readers get the line-oriented mode (SPEC §10.7).
    if Config::load(&paths).is_ok_and(|c| c.tui.screen_reader) {
        return plain::run(paths);
    }
    let mut app = App::new(paths)?;
    if let Some(s) = start {
        app.on_start = s.to_string();
    }
    app.load_drafts();
    enable_raw_mode()?;
    let mut out = stdout();
    out.execute(EnterAlternateScreen)?;
    out.execute(crossterm::event::EnableMouseCapture)?;
    // Pastes arrive as one event, so pasted newlines never send.
    let _ = out.execute(crossterm::event::EnableBracketedPaste);
    // Focus events: notify only while the user is elsewhere.
    let _ = out.execute(crossterm::event::EnableFocusChange);
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
            if std::mem::take(&mut app.force_redraw) {
                terminal.clear()?;
                app.painted_links.clear();
            }
            if !app.notifications.is_empty() {
                let mode = Config::load(&app.paths)
                    .map(|c| c.tui.notify)
                    .unwrap_or_default();
                let (term, tmux) = (notify::terminal(), std::env::var_os("TMUX").is_some());
                let mut out = stdout();
                for (title, body) in app.notifications.drain(..) {
                    let _ = out
                        .write_all(notify::sequence(&mode, &term, tmux, &title, &body).as_bytes());
                }
                let _ = out.flush();
            }
            app.chat_area = None;
            let frame = terminal.draw(|f| draw(f, &mut app))?;
            // Clickable links: repaint them with OSC 8 when they change.
            if let Some(proto) = app.image_proto {
                if app.image_placed != app.painted_images {
                    if app.painted_images.is_empty() {
                        let _ = images::paint(&mut stdout(), proto, &app.image_placed);
                        app.painted_images = app.image_placed.clone();
                    } else {
                        // Old thumbnails stay on screen until cleared:
                        // redraw from scratch, then paint the new ones.
                        app.painted_images.clear();
                        app.force_redraw = true;
                    }
                }
            }
            if app.hyperlinks {
                let base = app.work_dir();
                let cache = &mut app.path_cache;
                let mut resolve = |p: &str| {
                    if cache.len() > 4096 {
                        cache.clear();
                    }
                    cache
                        .entry(p.to_string())
                        .or_insert_with(|| {
                            resolve_path_in(&base, p).map(|f| format!("file://{}", f.display()))
                        })
                        .clone()
                };
                let found = app
                    .chat_area
                    .map(|a| links::scan(frame.buffer, a, &mut resolve))
                    .unwrap_or_default();
                if found != app.painted_links {
                    let _ = links::paint(&mut stdout(), frame.buffer, &found);
                    app.painted_links = found;
                }
            }
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    CtEvent::Key(k) => app.key(k),
                    CtEvent::Mouse(m) => app.mouse(m),
                    CtEvent::Paste(text) => app.paste(&text),
                    CtEvent::FocusGained => app.focused = true,
                    CtEvent::FocusLost => app.focused = false,
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    app.save_drafts();
    if app.enhanced_keys {
        let _ = stdout().execute(crossterm::event::PopKeyboardEnhancementFlags);
    }
    disable_raw_mode()?;
    let _ = stdout().execute(crossterm::event::DisableBracketedPaste);
    let _ = stdout().execute(crossterm::event::DisableFocusChange);
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
            session_filter: lists::ListFilter::default(),
            current: Some("s1".into()),
            messages: Vec::new(),
            streaming: String::new(),
            thinking: String::new(),
            show_timestamps: true,
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
            input: input::InputLine::default(),
            ctrl_c_at: None,
            enhanced_keys: false,
            mouse_capture: true,
            proposal_generating: false,
            running_sessions: Default::default(),
            notes: Vec::new(),
            notes_seen: 0,
            toasts: Vec::new(),
            pending: HashMap::new(),
            tools: Vec::new(),
            tool_idx: 0,
            tool_filter: lists::ListFilter::default(),
            tool_detail: None,
            rollback_pick: None,
            model_overlay: None,
            mention: None,
            pending_send: None,
            attachments: Vec::new(),
            palette_sel: 0,
            palette_hidden: None,
            settings_sel: 0,
            settings_edit: None,
            settings_saving: None,
            settings_error: None,
            settings_form: None,
            settings_remove_arm: None,
            providers_info: serde_json::Value::Null,
            force_redraw: false,
            focused: true,
            run_started_at: HashMap::new(),
            notifications: Vec::new(),
            approvals: std::collections::VecDeque::new(),
            conn: None,
            others: Vec::new(),
            questions: Vec::new(),
            run_usage: HashMap::new(),
            context_fill: HashMap::new(),
            session_runs: Vec::new(),
            run_banner: None,
            last_error: None,
            call_focus: None,
            expanded_calls: HashSet::new(),
            scroll_to_focus: false,
            user_rows: Vec::new(),
            find: None,
            edit_armed: false,
            fork_prefill: None,
            drafts: HashMap::new(),
            target_flash: None,
            chat_area: None,
            painted_links: Vec::new(),
            path_cache: HashMap::new(),
            image_proto: None,
            pane_widths: (22, 26),
            input_focus: false,
            approval_explain: false,
            quit_confirm: false,
            switcher: None,
            on_start: "last".into(),
            approval_code: None,
            image_placed: Vec::new(),
            painted_images: Vec::new(),
            hyperlinks: false,
            unseen_from: None,
            new_badge: None,
            invocations: Vec::new(),
            promotions: std::collections::VecDeque::new(),
            purge_arm: None,
            last_deleted: None,
            pager: None,
            delete_arm: None,
            subagents: Vec::new(),
            import_mode: None,
            renaming: None,
            instructing: None,
            marked: HashSet::new(),
            bulk: None,
            modules: Vec::new(),
            model_stats: Vec::new(),
            run_summary: None,
            builtin_stats: Vec::new(),
            proposals: Vec::new(),
            proposal_idx: 0,
            proposal_filter: lists::ListFilter::default(),
            editor: None,
            search_hits: Vec::new(),
            search_idx: 0,
            search_mode: false,
            editing_id: None,
            should_quit: false,
            md_cache: Default::default(),
            click_targets: Default::default(),
            last_click: None,
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
            tool_id: None,
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
        let calls = persisted_calls(&meta.to_string());
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[0].0.as_str(), calls[0].1.as_str()),
            ("1", "create_tool")
        );
        assert!(persisted_calls("not json").is_empty());
        assert!(persisted_calls("{}").is_empty());
    }

    #[test]
    fn tool_calls_collapse_expand_and_take_focus() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let msg =
            |id: &str, role: &str, content: &str, meta: Option<serde_json::Value>| MessageInfo {
                id: id.into(),
                session_id: "s".into(),
                seq: 0,
                role: role.into(),
                content: content.into(),
                created_at: String::new(),
                meta: meta.map(|m| m.to_string()),
            };
        let mut app = test_app();
        app.messages = vec![
            msg("u", "user", "count lines", None),
            msg(
                "a",
                "assistant",
                "",
                Some(serde_json::json!({"tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "line_count", "arguments": "{\"path\":\"/tmp/x\"}"}},
                    {"id": "c2", "type": "function", "function": {"name": "fetch", "arguments": "{}"}}
                ]})),
            ),
            msg(
                "t1",
                "tool",
                "{\"lines\": 42}",
                Some(serde_json::json!({"tool_call_id": "c1"})),
            ),
            msg(
                "t2",
                "tool",
                "Error: denied",
                Some(serde_json::json!({"tool_call_id": "c2"})),
            ),
        ];
        // Collapsed: one line per call with its result folded in.
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("▸ ⚙ line_count ✓"), "{screen}");
        assert!(screen.contains("▸ ⚙ fetch ✗"));
        assert!(!screen.contains("↳"), "results aren't repeated");
        // ] focuses the last call, [ the one before; Enter expands it.
        app.key(key(KeyCode::Char(']')));
        app.key(key(KeyCode::Char('[')));
        assert_eq!(app.call_focus.as_deref(), Some("c1"));
        app.key(key(KeyCode::Enter));
        let screen = render(&mut app, 100, 30);
        assert!(
            screen.contains("▾ ⚙ line_count") && screen.contains("\"lines\": 42"),
            "{screen}"
        );
        assert!(
            screen.contains("\"path\": \"/tmp/x\""),
            "input pretty-printed"
        );
        // z expands all, Z collapses all; typing leaves the focus.
        app.key(key(KeyCode::Char('z')));
        assert_eq!(app.expanded_calls.len(), 2);
        app.key(key(KeyCode::Char('Z')));
        assert!(app.expanded_calls.is_empty());
        app.key(key(KeyCode::Char('h')));
        assert!(app.call_focus.is_none());
        assert_eq!(app.input, "h");
    }

    #[test]
    fn wizard_starts_with_provider_presets() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.wizard = Some(Wizard::new(&Config::default()));
        assert!(render(&mut app, 90, 30).contains("2. Anthropic"));
        // Groq: fills the endpoint and links to its keys page.
        app.key(key(KeyCode::Char('5')));
        let w = app.wizard.as_ref().unwrap();
        assert_eq!(w.step, WizardStep::ApiKey);
        assert_eq!(w.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(w.saved_model, "llama-3.3-70b-versatile");
        assert!(w.status.contains("console.groq.com") || w.status.contains("$GROQ_API_KEY"));
        // Esc returns to the presets; the last row is a custom URL.
        app.key(key(KeyCode::Esc));
        for _ in 0..20 {
            app.key(key(KeyCode::Down));
        }
        app.key(key(KeyCode::Enter));
        let w = app.wizard.as_ref().unwrap();
        assert_eq!(w.step, WizardStep::BaseUrl);
        assert!(w.preset.is_none());
        app.key(key(KeyCode::Esc));
        assert_eq!(app.wizard.as_ref().unwrap().step, WizardStep::PickPreset);
        // A saved setup recognises its preset.
        let mut cfg = Config::default();
        cfg.provider.base_url = "https://openrouter.ai/api/v1".into();
        assert_eq!(Wizard::new(&cfg).preset.unwrap().id, "openrouter");
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
        assert_eq!(fresh.step, WizardStep::PickPreset);
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
                recent: String::new(),
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
                recent: String::new(),
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
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: Vec::new(),
            instructions: None,
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
            tool_id: None,
            session_id: None,
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
            tool_id: None,
            session_id: None,
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
    fn input_edits_recalls_history_and_keeps_pastes_whole() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let with = |code, m| KeyEvent::new(code, m);
        let mut app = test_app();
        app.messages = ["first ask", "second ask"]
            .iter()
            .enumerate()
            .map(|(i, c)| MessageInfo {
                id: format!("m{i}"),
                session_id: "s".into(),
                seq: i as i64,
                role: "user".into(),
                content: c.to_string(),
                created_at: String::new(),
                meta: None,
            })
            .collect();
        for c in "helo".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Left));
        app.key(key(KeyCode::Char('l')));
        assert_eq!(app.input, "hello");
        // Up recalls this Session's messages; Down returns to the draft.
        app.key(key(KeyCode::Up));
        assert_eq!(app.input, "second ask");
        app.key(key(KeyCode::Up));
        assert_eq!(app.input, "first ask");
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        assert_eq!(app.input, "hello");
        // Shift+Enter and a trailing backslash add newlines.
        app.key(with(KeyCode::Enter, KeyModifiers::SHIFT));
        app.key(key(KeyCode::Char('x')));
        app.key(key(KeyCode::Char('\\')));
        app.key(key(KeyCode::Enter));
        app.key(key(KeyCode::Char('y')));
        assert_eq!(app.input, "hello\nx\ny");
        // Up on a later line moves within the input, not through history.
        app.key(key(KeyCode::Up));
        assert_eq!(app.input, "hello\nx\ny");
        for (w, h) in [(1, 1), (12, 6), (80, 24)] {
            render(&mut app, w, h);
        }
        // Ctrl+U clears the cursor's line (now "x"); Ctrl+R finds an
        // earlier message.
        app.key(with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "hello\n\ny");
        app.input.clear();
        app.key(with(KeyCode::Char('r'), KeyModifiers::CONTROL));
        for c in "first".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert!(render(&mut app, 80, 24).contains("(history search) first"));
        app.key(key(KeyCode::Enter));
        assert_eq!(app.input, "first ask");
        // A big paste stays one block and is sent in full.
        app.input.clear();
        let big = vec!["row"; 30].join("\n");
        app.paste(&big);
        assert_eq!(app.input, "[pasted 30 lines #1]");
        assert_eq!(app.input.take(), big);
    }

    #[test]
    fn settings_rows_toggle_cycle_edit_and_open_forms() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.open_view(View::Settings);
        let rows = app.settings_rows();
        let at = |k: &str| rows.iter().position(|r| r.key == k).unwrap();
        // A toggle saves through the daemon.
        app.settings_sel = at("provider.tool_use_nudge");
        app.key(key(KeyCode::Enter));
        assert_eq!(
            app.settings_saving.as_deref(),
            Some("provider.tool_use_nudge")
        );
        // A bad number is caught before saving and shown by the row.
        app.settings_sel = at("provider.max_retries");
        app.key(key(KeyCode::Enter));
        assert!(app.settings_edit.is_some());
        app.key(key(KeyCode::Char('x')));
        app.key(key(KeyCode::Enter));
        assert!(app
            .settings_error
            .as_ref()
            .unwrap()
            .1
            .contains("whole number"));
        assert!(render(&mut app, 100, 40).contains("whole number"));
        app.key(key(KeyCode::Esc));
        assert!(app.settings_edit.is_none());
        // Model opens the picker for the global default.
        app.settings_sel = at("provider.model");
        app.key(key(KeyCode::Enter));
        assert!(app.model_overlay.as_ref().unwrap().default_only);
        app.key(key(KeyCode::Esc));
        // Add provider: a masked key, validated before saving.
        app.settings_sel = at("+provider");
        app.key(key(KeyCode::Enter));
        for c in "local".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        for c in "nope".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        app.key(key(KeyCode::Enter));
        for c in "sk-secret".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        let screen = render(&mut app, 100, 40);
        assert!(!screen.contains("sk-secret") && screen.contains("•••••••••"));
        app.key(key(KeyCode::Enter));
        assert!(app
            .settings_form
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("http"));
        app.key(key(KeyCode::Esc));
        assert!(app.settings_form.is_none());
        for (w, h) in [(1, 1), (20, 5), (80, 24)] {
            render(&mut app, w, h);
        }
        app.key(key(KeyCode::Esc));
        assert_eq!(app.view, View::Chat);
    }

    #[test]
    fn question_panel_answers_with_arrow_keys_and_goes_back() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let opt = |l: &str, d: Option<&str>| QuestionOption {
            label: l.into(),
            description: d.map(str::to_string),
        };
        let mut app = test_app();
        app.current = Some("s".into());
        app.on_event(Event::UserQuestion {
            question_id: "q".into(),
            session_id: "s".into(),
            question: String::new(),
            options: Vec::new(),
            questions: vec![
                Question {
                    question: "Which database?".into(),
                    header: Some("DB".into()),
                    options: vec![opt("Postgres", Some("relational")), opt("SQLite", None)],
                    multi_select: false,
                },
                Question {
                    question: "Which features?".into(),
                    header: None,
                    options: vec![opt("auth", None), opt("search", None), opt("billing", None)],
                    multi_select: true,
                },
            ],
        });
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("Question 1/2 · DB") && screen.contains("relational"));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Enter));
        assert_eq!(app.questions[0].answers[0].selected, vec!["SQLite"]);
        // Multi-select: tick with a number and with Space.
        app.key(key(KeyCode::Char('1')));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Char(' ')));
        assert!(render(&mut app, 100, 30).contains("[x] billing"));
        // Shift+Tab: back to change the first answer.
        app.key(key(KeyCode::BackTab));
        assert_eq!(app.questions[0].answers.len(), 0);
        assert_eq!(app.view, View::Chat, "Shift+Tab didn't switch views");
        app.key(key(KeyCode::Char('1')));
        assert_eq!(app.questions[0].answers[0].selected, vec!["Postgres"]);
        // Typed text is the answer ("Other…").
        for c in "just auth".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert!(app.questions.is_empty(), "all answered and sent");
        assert!(app.notes.iter().any(|n| n == "answered: just auth"));
    }

    #[test]
    fn notifies_only_while_unfocused_and_after_long_runs() {
        let mut app = test_app();
        let started = |app: &mut App, sid: &str| {
            app.on_event(Event::RunStarted {
                session_id: sid.into(),
                run_id: "r".into(),
            })
        };
        let finished = |app: &mut App, sid: &str| {
            app.on_event(Event::RunFinished {
                session_id: sid.into(),
                run_id: "r".into(),
                status: "completed".into(),
            })
        };
        // Focused: nothing.
        app.on_event(Event::ApprovalRequested {
            session_id: "s".into(),
            approval_id: "a".into(),
            tool_name: "fetch".into(),
            version: 1,
            capabilities: vec![],
            memory_mb: 64,
            detail: None,
            already_allowed: vec![],
            code_preview: None,
            tool_id: None,
        });
        assert!(app.notifications.is_empty());
        app.focused = false;
        // A short Run finishes quietly; a long one notifies.
        started(&mut app, "s");
        finished(&mut app, "s");
        assert!(app.notifications.is_empty());
        started(&mut app, "s");
        app.run_started_at
            .insert("s".into(), Instant::now() - Duration::from_secs(60));
        finished(&mut app, "s");
        assert_eq!(app.notifications.last().unwrap().0, "Run completed");
        app.on_event(Event::RunPaused {
            session_id: "s".into(),
            run_id: "r".into(),
            error: "429 rate limited".into(),
            explained: None,
        });
        assert!(app.notifications.last().unwrap().1.contains("429"));
    }

    #[test]
    fn list_views_filter_sort_and_keep_selection_on_visible_rows() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.tools = ["read_file", "http_get", "csv_sum"]
            .iter()
            .enumerate()
            .map(|(i, n)| ToolInfo {
                id: n.to_string(),
                name: n.to_string(),
                description: String::new(),
                retired: false,
                current_version: 1,
                capabilities: vec![],
                memory_mb: 64,
                invocations: i as i64,
                success_rate: Some(0.9),
                needs_attention: false,
                created_at: String::new(),
                updated_at: String::new(),
                recent: String::new(),
            })
            .collect();
        app.view = View::Tools;
        for c in "/http".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert_eq!(app.selected_tool().unwrap().name, "http_get");
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("Tools · 1/3 · /http"), "{screen}");
        // Esc clears the filter, then a second Esc leaves the view.
        app.key(key(KeyCode::Esc));
        assert_eq!(app.visible_tools().len(), 3);
        assert_eq!(app.view, View::Tools);
        // Sort by calls: csv_sum (2 calls) first.
        for _ in 0..3 {
            app.key(key(KeyCode::Char('s')));
        }
        assert_eq!(app.selected_tool().unwrap().name, "csv_sum");
        app.key(key(KeyCode::Down));
        assert_eq!(app.selected_tool().unwrap().name, "http_get");
        app.key(key(KeyCode::Esc));
        app.key(key(KeyCode::Esc));
        assert_eq!(app.view, View::Chat);
    }

    #[test]
    fn dropped_files_become_removable_attachment_chips() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.current = Some("s".into());
        let file = app.paths.data_dir.join("shot.png");
        std::fs::create_dir_all(&app.paths.data_dir).unwrap();
        std::fs::write(&file, vec![0u8; 3000]).unwrap();
        // Dragging the file in pastes its (quoted) path: it's attached.
        app.paste(&format!("'{}'", file.display()));
        assert!(app.input.is_empty());
        assert_eq!(app.attachments.len(), 1);
        assert!(render(&mut app, 100, 24).contains("[shot.png · 3 KB]"));
        // Backspace on an empty input removes the chip.
        app.key(key(KeyCode::Backspace));
        assert!(app.attachments.is_empty());
        // Sending waits for the upload before the message.
        app.slash(&format!("attach {}", file.display()));
        for c in "what is this?".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert!(app.attachments.is_empty());
        assert_eq!(app.pending_send.as_ref().unwrap().waiting, 1);
    }

    #[test]
    fn status_line_shows_context_fill() {
        let mut app = test_app();
        app.current = Some("s".into());
        app.on_event(Event::RunUsage {
            session_id: "s".into(),
            run_id: "r".into(),
            usage: formless_protocol::UsageInfo {
                steps: 1,
                prompt_tokens: 7500,
                context_used: 7500,
                context_tokens: 10000,
                ..Default::default()
            },
        });
        assert!(render(&mut app, 120, 20).contains("ctx 75%"));
    }

    #[test]
    fn message_times_and_day_separators() {
        use chrono::TimeZone;
        let now = chrono::Utc::now();
        assert_eq!(
            ui::message_stamp(&now.to_rfc3339(), now).as_deref(),
            Some("now")
        );
        let ago = |s: i64| (now - chrono::Duration::seconds(s)).to_rfc3339();
        assert_eq!(ui::message_stamp(&ago(150), now).as_deref(), Some("2m"));
        assert_eq!(
            ui::message_stamp(&ago(3 * 3600), now).as_deref(),
            Some("3h")
        );
        assert!(ui::message_stamp(&ago(2 * 86_400), now)
            .unwrap()
            .contains(':'));
        assert!(ui::message_stamp("garbage", now).is_none());

        let mut app = test_app();
        let at = |d: u32, h: u32| {
            chrono::Local
                .with_ymd_and_hms(2026, 3, d, h, 0, 0)
                .unwrap()
                .to_rfc3339()
        };
        let msg = |id: &str, role: &str, when: String| MessageInfo {
            id: id.into(),
            session_id: "s".into(),
            seq: 0,
            role: role.into(),
            content: format!("hello {id}"),
            created_at: when,
            meta: None,
        };
        app.messages = vec![
            msg("a", "user", at(2, 9)),
            msg("b", "assistant", at(2, 10)),
            msg("c", "user", at(3, 9)),
        ];
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("──── Tue, 3 Mar 2026 ────"), "{screen}");
        assert!(
            !screen.contains("2 Mar 2026"),
            "no separator before the first day"
        );
        app.slash("timestamps");
        assert!(!render(&mut app, 100, 30).contains("3 Mar 2026"));
    }

    #[test]
    fn jumps_between_user_messages() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let msg = |i: usize, role: &str, content: String| MessageInfo {
            id: format!("m{i}"),
            session_id: "s".into(),
            seq: i as i64,
            role: role.into(),
            content,
            created_at: String::new(),
            meta: None,
        };
        app.messages = (0..6)
            .map(|i| {
                if i % 2 == 0 {
                    msg(i, "user", format!("question {}", i / 2 + 1))
                } else {
                    msg(i, "assistant", "para\n\n".repeat(30))
                }
            })
            .collect();
        render(&mut app, 80, 20);
        assert_eq!(app.user_rows.len(), 3);
        // From the live bottom, { goes to the last question, then earlier.
        app.key(key(KeyCode::Char('{')));
        assert_eq!(app.chat_scroll_top, Some(app.user_rows[2]));
        app.key(key(KeyCode::Char('{')));
        assert_eq!(app.chat_scroll_top, Some(app.user_rows[1]));
        let screen = render(&mut app, 80, 20);
        assert!(screen.contains("message 2/3"), "{screen}");
        assert!(screen.contains("question 2"));
        // Ctrl+↓ forward; Home to the top; past the last, follow again.
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(app.chat_scroll_top, Some(app.user_rows[2]));
        app.key(key(KeyCode::Home));
        assert_eq!(app.chat_scroll_top, Some(0));
        app.key(key(KeyCode::Char('}')));
        app.key(key(KeyCode::Char('}')));
        app.key(key(KeyCode::Char('}')));
        assert_eq!(app.chat_scroll_top, None);
    }

    #[test]
    fn find_in_session_highlights_and_steps_through_matches() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.messages = (0..8)
            .map(|i| MessageInfo {
                id: format!("m{i}"),
                session_id: "s".into(),
                seq: i,
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: if i == 2 || i == 6 {
                    "the Postgres decision".into()
                } else {
                    "filler\n\n".repeat(10)
                },
                created_at: String::new(),
                meta: None,
            })
            .collect();
        render(&mut app, 80, 16);
        app.key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        for c in "postgres".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        // The first draw finds the matches; the next shows the count.
        render(&mut app, 80, 16);
        let screen = render(&mut app, 80, 16);
        assert_eq!(app.find.as_ref().unwrap().rows.len(), 2);
        assert!(screen.contains("2/2"), "{screen}");
        // Enter wraps to the first match and scrolls to it.
        app.key(key(KeyCode::Enter));
        render(&mut app, 80, 16);
        assert_eq!(app.find.as_ref().unwrap().current, 0);
        assert!(app.chat_scroll_top.is_some());
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.find.as_ref().unwrap().current, 1);
        // Esc closes it and follows the bottom again.
        app.key(key(KeyCode::Esc));
        assert!(app.find.is_none() && app.chat_scroll_top.is_none());
        let (line, n) = highlight_matches(Line::raw("Aa aA"), "aa", |_| Style::default());
        assert_eq!((n, line.spans.len()), (2, 3));
    }

    #[test]
    fn new_lines_badge_while_scrolled_up() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = test_app();
        let msg = |i: i64| MessageInfo {
            id: format!("m{i}"),
            session_id: "s".into(),
            seq: i,
            role: "assistant".into(),
            content: format!("para {i}\n\n").repeat(10),
            created_at: String::new(),
            meta: None,
        };
        app.messages = (0..4).map(msg).collect();
        render(&mut app, 80, 20);
        app.scroll_chat_up(10);
        assert!(!render(&mut app, 80, 20).contains("new line"));
        app.messages.push(msg(4));
        let screen = render(&mut app, 80, 20);
        assert!(screen.contains("new lines · End"), "{screen}");
        let (row, x0, _) = app.new_badge.unwrap();
        app.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x0 + 1,
            row,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.chat_scroll_top.is_none());
        assert!(!render(&mut app, 80, 20).contains("new line"));
    }

    #[test]
    fn chat_scrollbar_and_position_while_scrolled() {
        let mut app = test_app();
        app.messages = (0..4)
            .map(|i| MessageInfo {
                id: format!("m{i}"),
                session_id: "s".into(),
                seq: i,
                role: "assistant".into(),
                content: format!("para {i}\n\n").repeat(10),
                created_at: String::new(),
                meta: None,
            })
            .collect();
        let bottom = render(&mut app, 80, 20);
        assert!(bottom.contains('█'), "scrollbar thumb");
        assert!(
            !bottom.contains("End to follow"),
            "no position while following"
        );
        app.scroll_chat_up(20);
        let screen = render(&mut app, 80, 20);
        let total = app.chat_line_count;
        assert!(screen.contains(&format!("/{total} · ")), "{screen}");
        assert!(screen.contains("% · End to follow"));
    }

    #[test]
    fn empty_session_offers_examples_by_number() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let screen = render(&mut app, 100, 30);
        assert!(
            screen.contains("Try one") && screen.contains("keeps them"),
            "{screen}"
        );
        assert!(screen.contains("@ reference a file"));
        app.key(key(KeyCode::Char('2')));
        assert_eq!(app.input, EXAMPLES[1]);
        // Once there's text, digits type normally.
        app.key(key(KeyCode::Char('3')));
        assert!(app.input.ends_with('3'));
    }

    #[test]
    fn focus_messages_and_copy_them() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let msg = |id: &str, role: &str, content: &str| MessageInfo {
            id: id.into(),
            session_id: "s".into(),
            seq: 0,
            role: role.into(),
            content: content.into(),
            created_at: String::new(),
            meta: None,
        };
        app.messages = vec![
            msg("u1", "user", "first question"),
            msg("a1", "assistant", "first answer"),
            msg("u2", "user", "second question"),
            msg("a2", "assistant", "**second** answer"),
        ];
        app.key(key(KeyCode::Char(']')));
        assert_eq!(app.call_focus.as_deref(), Some("msg:a2"));
        assert!(render(&mut app, 100, 30).contains("c copy · C copy as Markdown"));
        app.key(key(KeyCode::Char('c')));
        assert_eq!(app.notes.last().unwrap(), "copied the answer (17 chars)");
        app.key(key(KeyCode::Char('[')));
        app.key(key(KeyCode::Char('C')));
        assert_eq!(
            app.focused_text(true).unwrap().0,
            "**You:**\n\nsecond question"
        );
        assert!(app
            .notes
            .last()
            .unwrap()
            .contains("your message as Markdown"));
        app.key(key(KeyCode::Esc));
        app.slash("copy 3");
        assert!(app
            .notes
            .last()
            .unwrap()
            .contains("message 3 from the bottom"));
        app.slash("copy 9");
        assert_eq!(app.notes.last().unwrap(), "there are 4 messages to copy");
    }

    #[test]
    fn edit_or_fork_from_any_user_message() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.current = Some("s".into());
        let msg = |id: &str, seq: i64, role: &str, content: &str| MessageInfo {
            id: id.into(),
            session_id: "s".into(),
            seq,
            role: role.into(),
            content: content.into(),
            created_at: String::new(),
            meta: None,
        };
        app.messages = vec![
            msg("u1", 1, "user", "first question"),
            msg("a1", 2, "assistant", "first answer"),
            msg("u2", 3, "user", "second question"),
            msg("a2", 4, "assistant", "second answer"),
        ];
        for k in [']', '[', '[', '['] {
            app.key(key(KeyCode::Char(k)));
        }
        assert_eq!(app.call_focus.as_deref(), Some("msg:u1"));
        assert!(render(&mut app, 120, 30).contains("e edit & re-run from here"));
        app.key(key(KeyCode::Char('e')));
        assert_eq!(app.editing_id.as_deref(), Some("u1"));
        assert_eq!(app.input, "first question");
        // Later turns would be discarded: the first Enter only asks.
        app.key(key(KeyCode::Enter));
        assert!(app.edit_armed && app.editing_id.is_some());
        app.key(key(KeyCode::Enter));
        assert!(app.editing_id.is_none() && app.input.is_empty());
        // f forks from the second question, which waits in the input.
        app.key(key(KeyCode::Char(']')));
        app.key(key(KeyCode::Char('[')));
        assert_eq!(app.call_focus.as_deref(), Some("msg:u2"));
        app.key(key(KeyCode::Char('f')));
        assert_eq!(app.fork_prefill.as_deref(), Some("second question"));
        // e on an answer explains it needs one of your messages.
        app.key(key(KeyCode::Char(']')));
        app.key(key(KeyCode::Char('e')));
        assert!(app.notes.last().unwrap().contains("your own messages"));
    }

    #[test]
    fn drafts_stay_with_their_session() {
        let mut app = test_app();
        std::fs::create_dir_all(&app.paths.data_dir).unwrap();
        app.switch_session("s1".into());
        app.input.set("half-typed for s1");
        app.attachments.push(("a.txt".into(), b"x".to_vec()));
        app.switch_session("s2".into());
        assert!(app.input.is_empty() && app.attachments.is_empty());
        app.input.set("for s2");
        app.switch_session("s1".into());
        assert_eq!(app.input, "half-typed for s1");
        assert_eq!(app.attachments.len(), 1);
        // Text drafts survive a restart (the current one too).
        app.save_drafts();
        let mut again = test_app();
        again.paths = app.paths.clone();
        again.load_drafts();
        again.switch_session("s2".into());
        assert_eq!(again.input, "for s2");
        again.switch_session("s1".into());
        assert_eq!(again.input, "half-typed for s1");
    }

    #[test]
    fn input_border_names_the_target_session() {
        let mut app = test_app();
        app.sessions = ["refactor-auth", "docs"]
            .iter()
            .map(|n| SessionInfo {
                id: n.to_string(),
                name: n.to_string(),
                model: None,
                reasoning_effort: None,
                created_at: String::new(),
                updated_at: String::new(),
                cwd: None,
                message_count: 0,
                preview: None,
                last_run_status: None,
                pinned: false,
                archived: false,
                tags: Vec::new(),
                instructions: None,
            })
            .collect();
        app.switch_session("refactor-auth".into());
        assert!(render(&mut app, 100, 20).contains("Message › to: refactor-auth"));
        assert!(app.target_flash.is_some_and(|t| t > Instant::now()));
        app.switch_session("docs".into());
        assert!(render(&mut app, 100, 20).contains("to: docs"));
    }

    #[test]
    fn tables_open_full_width_in_the_pager() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let wide = format!(
            "Results:\n\n| id | description |\n|---|---|\n| 1 | {} |\n\nand\n\n| a | b |\n|---|---|\n| x | y |",
            "word ".repeat(40)
        );
        app.messages = vec![MessageInfo {
            id: "a".into(),
            session_id: "s".into(),
            seq: 1,
            role: "assistant".into(),
            content: wide,
            created_at: String::new(),
            meta: None,
        }];
        assert_eq!(markdown_tables(&app.messages[0].content).len(), 2);
        app.slash("table");
        let p = app.pager.as_ref().unwrap();
        assert_eq!(p.title, "table 1 of 2");
        assert!(p.lines.iter().any(|l| l.chars().count() > 200), "unshrunk");
        app.key(key(KeyCode::Right));
        assert_eq!(app.pager.as_ref().unwrap().left, 8);
        assert!(render(&mut app, 60, 20).contains("←→"));
        app.key(key(KeyCode::Esc));
        app.slash("table 3");
        assert!(app.notes.last().unwrap().contains("2 table(s)"));
    }

    #[test]
    fn open_links_from_the_last_answer() {
        let mut app = test_app();
        app.messages = vec![MessageInfo {
            id: "a".into(),
            session_id: "s".into(),
            seq: 1,
            role: "assistant".into(),
            content: "See [the docs](https://docs.rs/x) and https://example.com/page.".into(),
            created_at: String::new(),
            meta: None,
        }];
        app.slash("open link");
        assert_eq!(app.notes.last().unwrap(), "opening https://docs.rs/x");
        app.slash("open link 2");
        assert_eq!(
            app.notes.last().unwrap(),
            "opening https://example.com/page"
        );
        app.slash("open link 3");
        assert_eq!(app.notes.last().unwrap(), "the last answer has 2 link(s)");
        // The chat area is recorded for the OSC 8 pass.
        render(&mut app, 80, 20);
        assert!(app.chat_area.is_some());
    }

    #[test]
    fn file_paths_in_the_chat_link_and_open() {
        // Tests run in the crate directory.
        assert!(resolve_path("src/lib.rs").is_some());
        assert!(resolve_path("src/nope.rs").is_none());
        let mut app = test_app();
        app.slash("open path Cargo.toml:3");
        let note = app.notes.last().unwrap();
        assert!(note.contains("Cargo.toml at line 3"), "{note}");
        app.slash("open path missing.rs");
        assert_eq!(app.notes.last().unwrap(), "no file missing.rs");
        // The OSC 8 pass links existing paths to file://.
        let area = Rect::new(0, 0, 50, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        buf.set_string(0, 0, "see src/lib.rs:10 and src/nope.rs", Style::default());
        let found = links::scan(&buf, area, &mut |p| {
            resolve_path(p).map(|f| format!("file://{}", f.display()))
        });
        assert_eq!(found.len(), 1);
        assert!(found[0].url.starts_with("file://") && found[0].url.ends_with("src/lib.rs"));
        assert_eq!((found[0].x0, found[0].x1), (4, 16));
        assert!(editor_takes_goto("/usr/local/bin/code") && !editor_takes_goto("vim"));
    }

    #[test]
    fn theme_follows_settings() {
        let mut app = test_app();
        std::fs::create_dir_all(&app.paths.config_dir).unwrap();
        let mut cfg = Config::default();
        cfg.tui.theme = "light".into();
        cfg.tui.colors.insert("user".into(), "#ff0000".into());
        cfg.save(&app.paths).unwrap();
        app.on_event(Event::SettingsChanged);
        assert_eq!(fg(true, Color::Yellow).fg, Some(Color::Rgb(150, 100, 0)));
        assert_eq!(role_style(true, "user").fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(fg(false, Color::Yellow).fg, None, "NO_COLOR still wins");
        theme::reset();
    }

    #[test]
    fn code_blocks_open_unwrapped_in_the_pager() {
        let mut app = test_app();
        app.messages = vec![MessageInfo {
            id: "a".into(),
            session_id: "s".into(),
            seq: 1,
            role: "assistant".into(),
            content: "```rust\nfn a() {}\n```\n\n```sh\necho one\necho two\n```".into(),
            created_at: String::new(),
            meta: None,
        }];
        app.slash("pager code");
        let p = app.pager.as_ref().unwrap();
        assert_eq!(p.title, "code block 2 of 2");
        assert_eq!(p.lines, vec!["1 │ echo one", "2 │ echo two"]);
        app.pager = None;
        app.slash("pager code 1");
        assert_eq!(app.pager.as_ref().unwrap().lines, vec!["1 │ fn a() {}"]);
    }

    #[test]
    fn tool_results_with_diffs_are_coloured() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1,2 @@\n-old\n+new\n+more";
        let msg = |id: &str, role: &str, content: String, meta: serde_json::Value| MessageInfo {
            id: id.into(),
            session_id: "s".into(),
            seq: 0,
            role: role.into(),
            content,
            created_at: String::new(),
            meta: Some(meta.to_string()),
        };
        let mut app = test_app();
        app.messages = vec![
            msg(
                "a",
                "assistant",
                String::new(),
                serde_json::json!({"tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "apply_patch", "arguments": "{}"}}]}),
            ),
            msg(
                "t",
                "tool",
                serde_json::json!({"diff": diff, "ok": true}).to_string(),
                serde_json::json!({"tool_call_id": "c1"}),
            ),
        ];
        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("+2 −1"), "{screen}");
        app.key(key(KeyCode::Char(']')));
        app.key(key(KeyCode::Enter));
        let screen = render(&mut app, 100, 20);
        assert!(
            screen.contains("      +new") && screen.contains("      -old"),
            "{screen}"
        );
        assert!(
            !screen.contains("\\n"),
            "the diff isn't shown as escaped JSON"
        );
    }

    #[test]
    fn image_attachments_get_thumbnail_rows() {
        let mut app = test_app();
        std::fs::create_dir_all(&app.paths.data_dir).unwrap();
        let png = app.paths.data_dir.join("shot.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nfake").unwrap();
        app.messages = vec![MessageInfo {
            id: "u".into(),
            session_id: "s".into(),
            seq: 1,
            role: "user".into(),
            content: "what's this?".into(),
            created_at: String::new(),
            meta: Some(
                serde_json::json!({"attachments": [
                    {"name": "shot.png", "mime": "image/png", "path": png.to_string_lossy()},
                    {"name": "notes.txt", "mime": "text/plain", "path": png.to_string_lossy()}
                ]})
                .to_string(),
            ),
        }];
        // Without a protocol nothing is reserved.
        render(&mut app, 100, 40);
        assert!(app.image_placed.is_empty());
        app.image_proto = Some(images::Protocol::ITerm);
        let screen = render(&mut app, 100, 40);
        assert!(screen.contains("🖼 shot.png") && !screen.contains("notes.txt"));
        assert_eq!(app.image_placed.len(), 1);
        let p = &app.image_placed[0];
        assert_eq!((p.cols, p.path.as_str()), (40, png.to_str().unwrap()));
        // Too short to show the whole box: not placed.
        render(&mut app, 100, 12);
        assert!(app.image_placed.is_empty());
    }

    #[test]
    fn typing_continues_while_an_approval_is_open() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.approvals.push_back(ApprovalReq {
            approval_id: "a".into(),
            tool_name: "fetch_page".into(),
            version: 1,
            capabilities: vec!["net:example.com".into()],
            memory_mb: 64,
            detail: None,
            already_allowed: Vec::new(),
            code_preview: None,
            tool_id: None,
            session_id: None,
        });
        assert!(render(&mut app, 100, 30).contains("Tab: keep typing"));
        // Typing moves focus to the input; y and n then type normally.
        for c in "hey no".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "hey no");
        assert_eq!(app.approvals.len(), 1, "still waiting");
        let screen = render(&mut app, 100, 30);
        assert!(
            screen.contains("Approval required · Tab to answer"),
            "{screen}"
        );
        assert_eq!(app.view, View::Chat);
        // Tab back to the approval: n denies it, the draft is kept.
        app.key(key(KeyCode::Tab));
        app.key(key(KeyCode::Char('n')));
        assert!(app.approvals.is_empty());
        assert_eq!(app.input, "hey no");
        assert!(app.notes.last().unwrap().contains("denied fetch_page"));
    }

    #[test]
    fn approval_shows_full_code_and_explains_capabilities() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.approvals.push_back(ApprovalReq {
            approval_id: "a".into(),
            tool_name: "save_report".into(),
            version: 3,
            capabilities: vec!["fs:write:/Users/x/out".into()],
            memory_mb: 64,
            detail: None,
            already_allowed: Vec::new(),
            code_preview: Some("function run(i) {".into()),
            tool_id: Some("t1".into()),
            session_id: None,
        });
        app.key(key(KeyCode::Char('?')));
        let screen = render(&mut app, 110, 30);
        assert!(
            screen.contains("can create, overwrite or delete files under /Users/x/out"),
            "{screen}"
        );
        assert_eq!(app.approvals.len(), 1, "? doesn't decide");
        app.key(key(KeyCode::Char('v')));
        assert_eq!(app.approval_code, Some(3), "asks for v3's code");
        let p = code_pager(
            "save_report v3 code",
            "function run(i) {\n  return 1;\n}",
            true,
        );
        assert_eq!(p.lines[1], "2 │   return 1;");
        assert!(p.styled[0].spans.iter().any(|s| s.content == "function"));
    }

    #[test]
    fn side_panes_resize_and_hide() {
        let mut app = test_app();
        app.multi_pane = true;
        app.slash("panes wider");
        assert_eq!(app.pane_widths, (25, 29));
        app.key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(app.pane_widths, (22, 26));
        app.slash("panes left");
        assert_eq!(app.pane_widths, (0, 26));
        // A hidden pane draws nothing and the chat takes its room.
        let screen = render(&mut app, 140, 30);
        assert!(!screen.contains("╭ Sessions"), "{screen}");
        app.slash("panes reset");
        assert!(render(&mut app, 140, 30).contains("╭ Sessions"));
        assert_eq!(pane_widths(&[0, 99]), (0, 40));
        assert_eq!(pane_widths(&[]), (22, 26));
        app.slash("panes sideways");
        assert!(app.notes.last().unwrap().starts_with("usage: /panes"));
    }

    #[test]
    fn mouse_clicks_switch_tabs_select_rows_and_toggle_calls() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let click_at = |app: &mut App, column: u16, row: u16| {
            app.mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        let target = |app: &App, what: &Click| {
            app.click_targets
                .borrow()
                .iter()
                .find(|(_, c)| c == what)
                .map(|(r, _)| (r.x, r.y))
                .unwrap()
        };
        let mut app = test_app();
        app.tools = ["alpha", "beta", "gamma"]
            .iter()
            .map(|n| ToolInfo {
                id: n.to_string(),
                name: n.to_string(),
                description: String::new(),
                retired: false,
                current_version: 1,
                capabilities: vec![],
                memory_mb: 64,
                invocations: 0,
                success_rate: None,
                needs_attention: false,
                created_at: String::new(),
                updated_at: String::new(),
                recent: String::new(),
            })
            .collect();
        render(&mut app, 120, 30);
        let (x, y) = target(&app, &Click::View(View::Tools));
        click_at(&mut app, x + 1, y);
        assert_eq!(app.view, View::Tools);
        render(&mut app, 120, 30);
        let (x, y) = target(&app, &Click::Tool(2));
        click_at(&mut app, x + 2, y);
        assert_eq!(app.selected_tool().unwrap().name, "gamma");
        // A tool call in the chat toggles on click.
        app.view = View::Chat;
        app.messages = vec![MessageInfo {
            id: "a".into(),
            session_id: "s".into(),
            seq: 1,
            role: "assistant".into(),
            content: String::new(),
            created_at: String::new(),
            meta: Some(
                serde_json::json!({"tool_calls": [{"id": "c1", "type": "function",
                    "function": {"name": "fetch", "arguments": "{}"}}]})
                .to_string(),
            ),
        }];
        render(&mut app, 120, 30);
        let (x, y) = target(&app, &Click::Call("c1".into()));
        click_at(&mut app, x + 3, y);
        assert!(app.expanded_calls.contains("c1"));
    }

    #[test]
    fn notes_show_as_toasts_and_errors_stay() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.notes.push("saved provider.model".into());
        app.notes.push("compaction failed: timeout".into());
        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("saved provider.model") && screen.contains("Esc dismisses"));
        // Ordinary toasts expire; errors stay until Esc.
        for t in &mut app.toasts {
            t.1 = Instant::now() - Duration::from_secs(60);
        }
        let screen = render(&mut app, 100, 24);
        assert!(!screen.contains("saved provider.model"));
        assert!(screen.contains("compaction failed"));
        app.key(key(KeyCode::Esc));
        assert!(!render(&mut app, 100, 24).contains("compaction failed"));
        // /notes keeps the whole history.
        app.slash("notes");
        assert_eq!(app.pager.as_ref().unwrap().lines.len(), 2);
    }

    #[test]
    fn finished_runs_get_a_footer_under_their_answer() {
        let mut app = test_app();
        app.current = Some("s".into());
        let msg = |id: &str, role: &str, at: &str| MessageInfo {
            id: id.into(),
            session_id: "s".into(),
            seq: 0,
            role: role.into(),
            content: format!("{id} text"),
            created_at: at.into(),
            meta: None,
        };
        app.messages = vec![
            msg("u1", "user", "2026-01-01T10:00:00Z"),
            msg("a1", "assistant", "2026-01-01T10:00:30Z"),
            msg("u2", "user", "2026-01-01T10:05:00Z"),
            msg("a2", "assistant", "2026-01-01T10:05:10Z"),
        ];
        let run =
            |id: &str, status: &str, at: &str, ms: i64, steps: i64, tools: i64| RunMetricsInfo {
                run_id: id.into(),
                session_id: "s".into(),
                status: status.into(),
                outcome: status.into(),
                steps,
                tool_calls: tools,
                prompt_tokens: 0,
                completion_tokens: 0,
                duration_ms: ms,
                created_at: at.into(),
            };
        app.session_runs = vec![
            run("r2", "interrupted", "2026-01-01T10:05:00Z", 12_000, 1, 0),
            run("r1", "completed", "2026-01-01T10:00:00Z", 134_000, 7, 3),
        ];
        app.run_usage.insert(
            "s".into(),
            formless_protocol::UsageInfo {
                steps: 1,
                cost: Some(0.04),
                ..Default::default()
            },
        );
        let footers = run_footers(&app);
        assert_eq!(footers[&1], "✓ 2m14s · 7 steps · 3 tools");
        assert_eq!(
            footers[&3],
            "■ interrupted after 12s · 1 step · 0 tools · $0.04"
        );
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("✓ 2m14s · 7 steps · 3 tools"), "{screen}");
    }

    #[test]
    fn wide_diagrams_preview_and_open_full_size() {
        let mut app = test_app();
        // A label longer than the pane can't be reflowed to fit.
        let src = format!(
            "graph TD\nA[{}] --> B[done]",
            "a very long step name ".repeat(5)
        );
        let content = format!("```py\nx = 1\n```\n\n```mermaid\n{src}\n```");
        assert_eq!(fenced_blocks(&content)[1].0, "mermaid");
        app.messages = vec![MessageInfo {
            id: "a".into(),
            session_id: "s".into(),
            seq: 1,
            role: "assistant".into(),
            content,
            created_at: String::new(),
            meta: None,
        }];
        let screen = render(&mut app, 80, 40);
        assert!(
            screen.contains("wider than the chat: /pager diagram 2"),
            "{screen}"
        );
        app.slash("pager diagram 2");
        let p = app.pager.as_ref().unwrap();
        assert_eq!(p.title, "mermaid · flowchart");
        assert!(p.lines.iter().any(|l| l.chars().count() > 80), "full width");
        app.pager = None;
        app.slash("pager diagram 1");
        assert!(app.notes.last().unwrap().contains("no Mermaid diagram"));
        app.slash("open diagram");
        let url = app
            .notes
            .last()
            .unwrap()
            .trim_start_matches("opening ")
            .to_string();
        assert!(url.starts_with("https://mermaid.live/edit#base64:"));
        use base64::Engine;
        let json = base64::engine::general_purpose::URL_SAFE
            .decode(url.split("base64:").nth(1).unwrap())
            .unwrap();
        let state: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(state["code"], src);
    }

    #[test]
    fn vim_keys_move_in_every_list_view() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.tools = (0..30)
            .map(|i| ToolInfo {
                id: format!("t{i:02}"),
                name: format!("t{i:02}"),
                description: String::new(),
                retired: false,
                current_version: 1,
                capabilities: vec![],
                memory_mb: 64,
                invocations: 0,
                success_rate: None,
                needs_attention: false,
                created_at: String::new(),
                updated_at: String::new(),
                recent: String::new(),
            })
            .collect();
        for view in [View::Tools, View::Metrics] {
            app.view = view;
            app.tool_idx = 0;
            app.key(key(KeyCode::Char('j')));
            app.key(key(KeyCode::Char('j')));
            app.key(key(KeyCode::Char('k')));
            assert_eq!(app.selected_tool().unwrap().name, "t01", "{view:?}");
            app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
            assert_eq!(app.selected_tool().unwrap().name, "t11");
            app.key(key(KeyCode::Char('G')));
            assert_eq!(app.selected_tool().unwrap().name, "t29");
            app.key(key(KeyCode::Char('g')));
            assert_eq!(app.selected_tool().unwrap().name, "t00");
        }
        // In Proposals, g is "top" now; n generates.
        app.view = View::Proposals;
        app.key(key(KeyCode::Char('g')));
        assert!(!app.proposal_generating);
    }

    #[test]
    fn hint_bar_follows_the_view() {
        let mut app = test_app();
        assert!(render(&mut app, 140, 20).contains("Enter send · / commands"));
        app.view = View::Sessions;
        assert!(render(&mut app, 140, 20).contains("Enter open · n new · f fork"));
        app.view = View::Proposals;
        let narrow = render(&mut app, 60, 20);
        assert!(
            narrow.contains("a apply") && !narrow.contains("v status"),
            "{narrow}"
        );
    }

    #[test]
    fn quit_asks_while_runs_are_working() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.slash("quit");
        assert!(app.should_quit, "nothing running: quits at once");
        let mut app = test_app();
        app.running_sessions.insert("s1".into());
        app.running_sessions.insert("s2".into());
        app.slash("quit");
        assert!(!app.should_quit && app.quit_confirm);
        assert!(render(&mut app, 100, 24).contains("2 Runs are still working"));
        app.key(key(KeyCode::Esc));
        assert!(!app.quit_confirm && !app.should_quit);
        app.slash("quit");
        app.key(key(KeyCode::Char('s')));
        assert!(app.should_quit);
    }

    #[test]
    fn the_session_working_directory_is_shown_and_used() {
        let mut app = test_app();
        let here = std::env::current_dir().unwrap();
        app.sessions = vec![SessionInfo {
            id: "s".into(),
            name: "work".into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: Some(here.join("src").to_string_lossy().into_owned()),
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        }];
        app.switch_session("s".into());
        assert_eq!(app.work_dir(), here.join("src"));
        // Paths resolve against it: `lib.rs` is src/lib.rs here.
        assert!(resolve_path_in(&app.work_dir(), "lib.rs").is_some());
        assert!(render(&mut app, 140, 20).contains("formless-tui/src"));
        app.slash("cd");
        assert!(app.notes.last().unwrap().starts_with("working directory: "));
        assert_eq!(short_dir("/a/b/c/d/e"), "…/d/e");
        assert_eq!(short_dir("/a/b"), "/a/b");
    }

    #[test]
    fn sessions_list_shows_state_count_and_preview() {
        let mut app = test_app();
        let session = |id: &str, status: Option<&str>| SessionInfo {
            id: id.into(),
            name: format!("{id} work"),
            model: Some("gpt-x".into()),
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: None,
            message_count: 12,
            preview: Some(format!("fix the {id} build")),
            last_run_status: status.map(str::to_string),
            pinned: false,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        };
        app.sessions = vec![session("a", Some("paused")), session("b", None)];
        app.running_sessions.insert("b".into());
        app.view = View::Sessions;
        let screen = render(&mut app, 140, 20);
        assert!(screen.contains("12 msgs · gpt-x"), "{screen}");
        assert!(screen.contains("“fix the a build”"));
        let row = |name: &str| {
            screen
                .lines()
                .find(|l| l.contains(name))
                .unwrap_or_default()
                .to_string()
        };
        assert!(
            row("a work").contains('⏸') && row("b work").contains('◐'),
            "{screen}"
        );
    }

    #[test]
    fn pinned_sessions_sort_first() {
        let mut app = test_app();
        let session = |id: &str, at: &str, pinned: bool| SessionInfo {
            id: id.into(),
            name: id.into(),
            model: None,
            reasoning_effort: None,
            created_at: at.into(),
            updated_at: at.into(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        };
        app.sessions = vec![
            session("new", "2026-02-01", false),
            session("home", "2026-01-01", true),
            session("mid", "2026-01-15", false),
        ];
        let names = |app: &App| -> Vec<String> {
            app.visible_sessions()
                .iter()
                .map(|&i| app.sessions[i].name.clone())
                .collect()
        };
        assert_eq!(names(&app), ["home", "new", "mid"]);
        app.session_filter.sort = 2; // by name: pinned still first
        assert_eq!(names(&app), ["home", "mid", "new"]);
        app.view = View::Sessions;
        assert!(render(&mut app, 120, 20).contains("★ home"));
    }

    #[test]
    fn archived_sessions_hide_behind_their_filter() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let session = |id: &str, archived: bool| SessionInfo {
            id: id.into(),
            name: id.into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived,
            tags: Vec::new(),
            instructions: None,
        };
        app.sessions = vec![session("old", true), session("live", false)];
        let names = |app: &App| -> Vec<String> {
            app.visible_sessions()
                .iter()
                .map(|&i| app.sessions[i].name.clone())
                .collect()
        };
        assert_eq!(names(&app), ["live"]);
        app.view = View::Sessions;
        app.key(key(KeyCode::Char('v')));
        app.key(key(KeyCode::Char('v')));
        assert_eq!(names(&app), ["old"]);
        assert!(render(&mut app, 120, 20).contains("archived"));
    }

    #[test]
    fn tags_show_and_filter_the_sessions_list() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let session = |id: &str, tags: &[&str]| SessionInfo {
            id: id.into(),
            name: id.into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            instructions: None,
        };
        app.sessions = vec![
            session("alpha", &["work"]),
            session("beta", &["home", "workshop"]),
        ];
        app.current = Some("alpha".into());
        app.slash("tag");
        assert!(app
            .notes
            .last()
            .unwrap()
            .contains("all tags: #home #work #workshop"));
        app.view = View::Sessions;
        assert!(render(&mut app, 140, 20).contains("#home #workshop"));
        for c in "/#work".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        let names: Vec<String> = app
            .visible_sessions()
            .iter()
            .map(|&i| app.sessions[i].name.clone())
            .collect();
        assert_eq!(names, ["alpha"], "#work is an exact tag, not #workshop");
    }

    #[test]
    fn u_undoes_a_session_delete() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.sessions = vec![SessionInfo {
            id: "s".into(),
            name: "doomed".into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        }];
        app.view = View::Sessions;
        app.key(key(KeyCode::Char('d')));
        app.key(key(KeyCode::Char('d')));
        assert!(app
            .notes
            .last()
            .unwrap()
            .contains("u (or /undo) restores it for 30s"));
        app.key(key(KeyCode::Char('u')));
        assert_eq!(app.notes.last().unwrap(), "restoring session doomed");
        app.key(key(KeyCode::Char('u')));
        assert_eq!(app.notes.last().unwrap(), "nothing to undo");
    }

    #[test]
    fn bulk_actions_on_marked_sessions() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        let session = |id: &str| SessionInfo {
            id: id.into(),
            name: id.into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        };
        app.sessions = vec![session("a1"), session("b2"), session("c3")];
        app.view = View::Sessions;
        app.key(key(KeyCode::Char(' ')));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Char(' ')));
        assert_eq!(app.marked.len(), 2);
        assert!(render(&mut app, 140, 20).contains("2 selected"));
        // Delete needs "delete" typed; anything else keeps them.
        app.key(key(KeyCode::Char('d')));
        assert_eq!(app.bulk.as_ref().unwrap().1.len(), 2);
        assert_eq!(app.view, View::Chat);
        for c in "nope".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert!(app.notes.last().unwrap().contains("not deleted"));
        // Tagging takes the typed tags; Esc clears marks in the list.
        app.view = View::Sessions;
        app.key(key(KeyCode::Char(' ')));
        app.key(key(KeyCode::Char('t')));
        assert_eq!(app.bulk.as_ref().unwrap().0, BulkAction::Tag);
        app.key(key(KeyCode::Esc));
        assert!(app.bulk.is_none());
        app.view = View::Sessions;
        app.key(key(KeyCode::Char(' ')));
        app.key(key(KeyCode::Esc));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn launch_opens_the_most_recent_or_a_fresh_session() {
        let mut app = test_app();
        let session = |id: &str, at: &str, pinned: bool, archived: bool| SessionInfo {
            id: id.into(),
            name: id.into(),
            model: None,
            reasoning_effort: None,
            created_at: at.into(),
            updated_at: at.into(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned,
            archived,
            tags: Vec::new(),
            instructions: None,
        };
        app.sessions = vec![
            session("home", "2026-01-01", true, false),
            session("recent", "2026-03-01", false, false),
            session("old-archived", "2026-04-01", false, true),
        ];
        assert_eq!(app.start_session().unwrap().id, "recent");
        app.on_start = "new".into();
        assert!(app.start_session().is_none());
    }

    #[test]
    fn rename_starts_from_the_current_name() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = test_app();
        app.sessions = vec![SessionInfo {
            id: "a".into(),
            name: "Refactor auth".into(),
            model: None,
            reasoning_effort: None,
            created_at: "2026-01-01".into(),
            updated_at: "2026-01-01".into(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: vec![],
            instructions: None,
        }];
        app.view = View::Sessions;
        app.key(key(KeyCode::Char('r')));
        assert_eq!(app.view, View::Chat);
        assert_eq!(&*app.input, "Refactor auth");
        assert!(render(&mut app, 80, 20).contains("rename ›"));
        app.key(key(KeyCode::Esc));
        assert!(app.renaming.is_none());
        assert_eq!(&*app.input, "");
        // `/rename auto` offers the daemon's suggestion for editing.
        app.current = Some("a".into());
        app.slash("rename auto");
        let (tx, rx) = mpsc::channel();
        app.inbox = Some(rx);
        app.pending.insert(7, Pending::SuggestName);
        tx.send(ServerMessage::Response {
            id: 7,
            ok: true,
            result: Some(serde_json::json!({"name": "Token code split"})),
            error: None,
        })
        .unwrap();
        app.drain_daemon();
        assert_eq!(app.renaming.as_deref(), Some("a"));
        assert_eq!(&*app.input, "Token code split");
        // `/instructions` edits them the same way.
        app.renaming = None;
        app.sessions[0].instructions = Some("answer in Thai".into());
        app.slash("instructions");
        assert_eq!(app.instructing.as_deref(), Some("a"));
        assert_eq!(&*app.input, "answer in Thai");
        assert!(render(&mut app, 80, 20).contains("instructions ›"));
        app.key(key(KeyCode::Esc));
        assert!(app.instructing.is_none());
    }

    #[test]
    fn a_template_session_opens_with_its_first_message_ready() {
        let mut app = test_app();
        let (tx, rx) = mpsc::channel();
        app.inbox = Some(rx);
        app.pending.insert(9, Pending::TemplateSession);
        tx.send(ServerMessage::Response {
            id: 9,
            ok: true,
            result: Some(serde_json::json!({
                "session": {"id": "t1", "name": "triage", "model": null, "reasoning_effort": null,
                            "created_at": "2026-01-01", "updated_at": "2026-01-01", "cwd": "/tmp"},
                "first_message": "Triage today's inbox",
            })),
            error: None,
        })
        .unwrap();
        app.drain_daemon();
        assert_eq!(app.current.as_deref(), Some("t1"));
        assert_eq!(app.view, View::Chat);
        assert_eq!(&*app.input, "Triage today's inbox");
    }

    #[test]
    fn shows_other_clients_and_what_they_did() {
        let mut app = test_app();
        app.current = Some("s1".into());
        app.conn = Some(1);
        let client = |conn, label: &str| AttachedClient {
            conn,
            label: label.into(),
        };
        app.on_event(Event::SessionClients {
            session_id: "s1".into(),
            clients: vec![
                client(1, "TUI"),
                client(2, "web"),
                client(3, "TUI"),
                client(4, "TUI"),
            ],
        });
        assert_eq!(app.others.len(), 3);
        assert!(render(&mut app, 140, 20).contains("also open in web, TUI ×2"));
        // Messages from the web, or from another attached TUI, say so;
        // this TUI's own (even from an earlier run) don't.
        let msg = |seq, conn, label: &str| MessageInfo {
            id: format!("m{seq}"),
            session_id: "s1".into(),
            seq,
            role: "user".into(),
            content: format!("hello {seq}"),
            created_at: "2026-01-01T00:00:00Z".into(),
            meta: Some(serde_json::json!({"client": {"conn": conn, "label": label}}).to_string()),
        };
        assert_eq!(
            sent_elsewhere(&app, &msg(1, 2, "web")).as_deref(),
            Some("web")
        );
        assert_eq!(
            sent_elsewhere(&app, &msg(2, 3, "TUI")).as_deref(),
            Some("TUI")
        );
        assert_eq!(sent_elsewhere(&app, &msg(3, 1, "TUI")), None);
        assert_eq!(sent_elsewhere(&app, &msg(4, 99, "TUI")), None);
        app.messages = vec![msg(1, 2, "web")];
        assert!(render(&mut app, 100, 20).contains("sent from web"));
        // An approval answered in the web closes here.
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
            tool_id: None,
        });
        app.on_event(Event::ApprovalAnswered {
            approval_id: "a1".into(),
            approved: true,
            by: Some("web".into()),
        });
        assert!(app.approvals.is_empty());
        assert!(app.notes.iter().any(|n| n == "approval granted in web"));
    }

    #[test]
    fn tools_show_recent_calls_and_their_trend() {
        // 20 good, then 20 mostly failing: trending down.
        let falling = format!("{}{}", "s".repeat(20), "sfff".repeat(5));
        assert_eq!(tool_trend(&falling), Some('↓'));
        assert_eq!(tool_trend(&"s".repeat(40)), None);
        assert_eq!(tool_trend("ffffff"), None, "no earlier window");
        let mut app = test_app();
        app.tools = vec![ToolInfo {
            id: "t".into(),
            name: "fetch_page".into(),
            description: String::new(),
            retired: false,
            current_version: 1,
            capabilities: vec![],
            memory_mb: 64,
            invocations: 40,
            success_rate: Some(0.6),
            needs_attention: false,
            created_at: String::new(),
            updated_at: String::new(),
            recent: falling,
        }];
        app.view = View::Tools;
        let screen = render(&mut app, 140, 20);
        assert!(screen.contains("LAST 20"), "{screen}");
        assert!(screen.contains("▆▂▂▂▆▂▂▂"), "{screen}");
        assert!(screen.contains("↓"), "{screen}");
    }

    #[test]
    fn ctrl_k_switches_sessions_from_anywhere() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut app = test_app();
        let session = |id: &str, name: &str, at: &str, preview: &str| SessionInfo {
            id: id.into(),
            name: name.into(),
            model: None,
            reasoning_effort: None,
            created_at: at.into(),
            updated_at: at.into(),
            cwd: None,
            message_count: 0,
            preview: Some(preview.into()),
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: vec!["client".into()],
            instructions: None,
        };
        app.sessions = vec![
            session("a", "Refactor auth", "2026-01-01", "split the token code"),
            session(
                "b",
                "Weekly report",
                "2026-02-01",
                "summarise the sales csv",
            ),
        ];
        app.view = View::Tools;
        app.key(ctrl('k'));
        assert!(render(&mut app, 100, 24).contains("Switch Session"));
        // No query: newest first.
        assert_eq!(app.switcher_matches("")[0], 1);
        for c in "rfauth".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Enter));
        assert_eq!(app.current.as_deref(), Some("a"));
        assert_eq!(app.view, View::Chat);
        // The preview is searchable too.
        assert_eq!(app.switcher_matches("sales csv")[0], 1);
        // Ctrl+N starts a new one; Esc closes.
        app.key(ctrl('k'));
        app.key(key(KeyCode::Esc));
        assert!(app.switcher.is_none());
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
            tool_id: None,
            session_id: None,
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
