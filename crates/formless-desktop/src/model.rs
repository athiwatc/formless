//! Presentation model for the desktop shell: no GPUI types, no I/O.
//!
//! Views read snapshots from here and emit typed [`Intent`]s. The Daemon Core
//! stays authoritative (ADR 0001); nothing in this module decides domain
//! outcomes, it only says what the shell shows and which request a control
//! would make.

use std::collections::{HashSet, VecDeque};

use formless_protocol::{SendMode, SessionInfo};

/// Starting layout tokens from epic #1753 §3, in logical pixels. Provisional:
/// they still need the 200% text-scale pass before being frozen.
pub mod tokens {
    pub const SIDEBAR_MIN: f32 = 240.0;
    pub const SIDEBAR_DEFAULT: f32 = 264.0;
    pub const SIDEBAR_MAX: f32 = 300.0;
    pub const INSPECTOR_MIN: f32 = 300.0;
    pub const INSPECTOR_DEFAULT: f32 = 340.0;
    pub const INSPECTOR_MAX: f32 = 420.0;
    pub const READING_MIN: f32 = 680.0;
    pub const READING_MAX: f32 = 900.0;
    /// Below this the inspector stops being a docked third pane.
    pub const THREE_PANE_MIN: f32 = 1280.0;
    /// Below this a docked sidebar would squeeze the conversation under its
    /// minimum reading width, so the sidebar collapses too.
    pub const SIDEBAR_DOCK_MIN: f32 = SIDEBAR_DEFAULT + READING_MIN;
    /// Spacing grid unit.
    pub const GRID: f32 = 4.0;
    /// The smallest window in which every header and composer control is on
    /// screen at once at 100% text size. At a larger text size that takes
    /// this multiplied by the text size factor; a window smaller than that
    /// is reflowed ([`Fit::Reflowed`]).
    pub const MIN_WINDOW_WIDTH: f32 = 480.0;
    pub const MIN_WINDOW_HEIGHT: f32 = 360.0;
    /// The least height the conversation is given in a reflowed window, or
    /// the height of the window if that is less: never more than one screen.
    pub const REFLOW_CONVERSATION_MIN: f32 = 220.0;
}

/// How the shell is fitted into its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// The window is at least [`TextScale::minimum_window`]: the header,
    /// the conversation, the composer and the status line are all on screen
    /// at once.
    Whole,
    /// The window is smaller than that. The text stays the size that was
    /// asked for. Each part is as wide as the window and wraps inside it,
    /// the parts stack, and the shell scrolls up and down as one page; it
    /// never scrolls sideways.
    Reflowed,
}

/// The user's text size, as a percentage of the default.
///
/// The whole layout scales with it: fixed sizes are multiplied by
/// [`TextScale::factor`] and the layout breakpoints are applied to the window
/// width divided by it. At 200% a window therefore lays out like one half as
/// wide at 100%, with everything twice the size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextScale(u16);

impl TextScale {
    pub const STEPS: [u16; 5] = [100, 125, 150, 175, 200];
    pub const DEFAULT: TextScale = TextScale(100);
    pub const LARGEST: TextScale = TextScale(200);

    /// The step closest to `percent`.
    pub fn from_percent(percent: u16) -> Self {
        let step = Self::STEPS
            .into_iter()
            .min_by_key(|step| step.abs_diff(percent))
            .unwrap_or(100);
        Self(step)
    }

    pub fn percent(self) -> u16 {
        self.0
    }

    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }

    /// One step larger; the largest step stays where it is.
    pub fn larger(self) -> Self {
        Self::STEPS
            .into_iter()
            .find(|step| *step > self.0)
            .map_or(self, Self)
    }

    /// The smallest window that keeps every control on screen at once at
    /// this text size, as (width, height).
    pub fn minimum_window(self) -> (f32, f32) {
        (
            tokens::MIN_WINDOW_WIDTH * self.factor(),
            tokens::MIN_WINDOW_HEIGHT * self.factor(),
        )
    }

    /// How the shell is fitted into a window of this size at this text
    /// size. The text size itself never gives way to the window.
    pub fn fit(self, width: f32, height: f32) -> Fit {
        let (needed_width, needed_height) = self.minimum_window();
        if width >= needed_width && height >= needed_height {
            Fit::Whole
        } else {
            Fit::Reflowed
        }
    }

    /// The width the layout breakpoints see in a window of this size: the
    /// real width divided by the text size factor. A reflowed window never
    /// docks a panel, however wide it is, so there it is also less than the
    /// narrowest width that docks one.
    pub fn layout_width(self, width: f32, height: f32) -> f32 {
        let scaled = width / self.factor();
        match self.fit(width, height) {
            Fit::Whole => scaled,
            Fit::Reflowed => scaled.min(tokens::SIDEBAR_DOCK_MIN - 1.0),
        }
    }

    /// One step smaller; the default is the smallest.
    pub fn smaller(self) -> Self {
        Self::STEPS
            .into_iter()
            .rev()
            .find(|step| *step < self.0)
            .map_or(self, Self)
    }
}

impl Default for TextScale {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The main destinations and transient searches. Search is transient: it
/// is reachable but not part of the persistent navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Destination {
    Chat,
    Sessions,
    Tools,
    Metrics,
    Proposals,
    Settings,
    Search,
    ProfileSearch,
    Comparison,
}

impl Destination {
    /// Persistent navigation order, matching the TUI's view order.
    pub const NAVIGATION: [Destination; 6] = [
        Destination::Chat,
        Destination::Sessions,
        Destination::Settings,
        Destination::Tools,
        Destination::Metrics,
        Destination::Proposals,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Destination::Chat => "Chat",
            Destination::Sessions => "Sessions",
            Destination::Tools => "Tools",
            Destination::Metrics => "Metrics",
            Destination::Proposals => "Proposals",
            Destination::Settings => "Settings",
            Destination::Search => "Search",
            Destination::ProfileSearch => "Profile Search",
            Destination::Comparison => "Session comparison",
        }
    }

    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Destination::Search | Destination::ProfileSearch | Destination::Comparison
        )
    }
}

/// What the shell may say about a Session's Run. `Unknown` is the honest
/// state while disconnected: the client must not claim a Run is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running,
    Paused,
    Unknown,
}

impl RunState {
    /// The existing clients derive "running" from live Run events and
    /// "paused" from `SessionInfo::last_run_status`; `SessionInfo` alone does
    /// not say a Run is in progress.
    pub fn of(session: &SessionInfo, running: &HashSet<String>, connected: bool) -> Self {
        if !connected {
            RunState::Unknown
        } else if running.contains(&session.id) {
            RunState::Running
        } else if session.last_run_status.as_deref() == Some("paused") {
            RunState::Paused
        } else {
            RunState::Idle
        }
    }

    /// Status text that never relies on colour alone.
    pub fn label(self) -> &'static str {
        match self {
            RunState::Idle => "Idle",
            RunState::Running => "Running",
            RunState::Paused => "Paused",
            RunState::Unknown => "Run state unknown",
        }
    }
}

/// The composer's send controls for one Run state. Stop is always a separate
/// control so the primary action never turns into "stop" under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerActions {
    pub primary: SendMode,
    pub secondary: Option<SendMode>,
    pub stop_enabled: bool,
    /// Why sending is unavailable; `None` when it is available.
    pub disabled_reason: Option<&'static str>,
}

pub fn send_mode_label(mode: SendMode) -> &'static str {
    match mode {
        SendMode::Send => "Send",
        SendMode::Steer => "Steer",
        SendMode::Queue => "Queue",
    }
}

impl ComposerActions {
    pub fn for_state(state: RunState, draft_is_empty: bool) -> Self {
        let (primary, secondary, stop_enabled) = match state {
            RunState::Running => (SendMode::Steer, Some(SendMode::Queue), true),
            RunState::Idle | RunState::Paused | RunState::Unknown => (SendMode::Send, None, false),
        };
        let disabled_reason = if state == RunState::Unknown {
            Some("Not connected to the Daemon Core")
        } else if draft_is_empty {
            Some("Write a message first")
        } else {
            None
        };
        Self {
            primary,
            secondary,
            stop_enabled,
            disabled_reason,
        }
    }
}

/// A typed request for the optional client layer. Fixture mode records its
/// metadata without sending. Both modes retain the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    SendMessage {
        session_id: String,
        content: String,
        mode: SendMode,
    },
    InterruptRun {
        session_id: String,
    },
    ResumeRun {
        session_id: String,
    },
    AbandonRun {
        session_id: String,
    },
}

impl Intent {
    pub fn session_id(&self) -> &str {
        match self {
            Self::SendMessage { session_id, .. }
            | Self::InterruptRun { session_id }
            | Self::ResumeRun { session_id }
            | Self::AbandonRun { session_id } => session_id,
        }
    }

    /// These actions can start provider work. Unknown earlier outcomes must
    /// block either one, even after unrelated acknowledgements on reconnect.
    pub fn starts_work(&self) -> bool {
        matches!(self, Self::SendMessage { .. } | Self::ResumeRun { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarMode {
    Docked,
    /// Shown over the conversation because the user asked for it at a width
    /// with no room to dock it. Modal: see [`Layout::modal`].
    Overlay,
    Collapsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorMode {
    Docked,
    /// Shown over the conversation because the user asked for it at a width
    /// with no room to dock it. Modal: see [`Layout::modal`].
    Overlay,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub sidebar: SidebarMode,
    pub inspector: InspectorMode,
    /// The composer belongs to Chat; other destinations have none.
    pub composer: bool,
}

/// The user's panel choices, independent of what currently fits.
///
/// Each panel has a docked preference and a separate overlay flag. They are
/// separate because narrowing the window must never drop a panel over the
/// conversation and its send controls on its own: an overlay only exists
/// after the user asks for it at a width where docking is impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelPrefs {
    /// Whether the sidebar is docked when there is room for it.
    pub sidebar_open: bool,
    pub sidebar_overlay_open: bool,
    /// Whether the inspector is docked when there is room for three panes.
    pub inspector_open: bool,
    pub inspector_overlay_open: bool,
}

impl Default for PanelPrefs {
    fn default() -> Self {
        Self {
            sidebar_open: true,
            sidebar_overlay_open: false,
            inspector_open: true,
            inspector_overlay_open: false,
        }
    }
}

impl PanelPrefs {
    /// The sidebar toggle acts on whichever form fits the current width.
    /// At most one overlay is up at a time.
    pub fn toggle_sidebar(&mut self, width: f32) {
        if width >= tokens::SIDEBAR_DOCK_MIN {
            self.sidebar_open = !self.sidebar_open;
        } else {
            self.sidebar_overlay_open = !self.sidebar_overlay_open;
            self.inspector_overlay_open = false;
        }
    }

    /// The inspector toggle acts on whichever form fits the current width.
    /// At most one overlay is up at a time.
    pub fn toggle_inspector(&mut self, width: f32) {
        if width >= tokens::THREE_PANE_MIN {
            self.inspector_open = !self.inspector_open;
        } else {
            self.inspector_overlay_open = !self.inspector_overlay_open;
            self.sidebar_overlay_open = false;
        }
    }

    pub fn close_overlays(&mut self) {
        self.sidebar_overlay_open = false;
        self.inspector_overlay_open = false;
    }

    /// Drop overlay requests that no longer apply at `width`, so widening and
    /// narrowing again does not bring an old overlay back unasked. Returns
    /// whether anything changed.
    pub fn settle(&mut self, width: f32) -> bool {
        let before = *self;
        if width >= tokens::SIDEBAR_DOCK_MIN {
            self.sidebar_overlay_open = false;
        }
        if width >= tokens::THREE_PANE_MIN {
            self.inspector_overlay_open = false;
        }
        *self != before
    }
}

impl Layout {
    /// The Chat layout at `width`. Narrowing first takes the inspector out of
    /// the row, then the sidebar; either is then only an overlay the user
    /// opens. A closed panel stays closed at any width.
    pub fn for_width(width: f32, prefs: PanelPrefs) -> Self {
        let sidebar = if width >= tokens::SIDEBAR_DOCK_MIN {
            if prefs.sidebar_open {
                SidebarMode::Docked
            } else {
                SidebarMode::Collapsed
            }
        } else if prefs.sidebar_overlay_open {
            SidebarMode::Overlay
        } else {
            SidebarMode::Collapsed
        };
        let inspector = if width >= tokens::THREE_PANE_MIN {
            if prefs.inspector_open {
                InspectorMode::Docked
            } else {
                InspectorMode::Hidden
            }
        } else if prefs.inspector_overlay_open && sidebar != SidebarMode::Overlay {
            InspectorMode::Overlay
        } else {
            InspectorMode::Hidden
        };
        Self {
            sidebar,
            inspector,
            composer: true,
        }
    }

    /// The same layout for another destination: only Chat has a composer.
    pub fn for_destination(mut self, destination: Destination) -> Self {
        self.composer = destination == Destination::Chat;
        self
    }

    /// The overlay region that currently holds the window modally, if any.
    /// While one is up, focus stays inside it until it is dismissed.
    pub fn modal(self) -> Option<Region> {
        if self.sidebar == SidebarMode::Overlay {
            Some(Region::Sidebar)
        } else if self.inspector == InspectorMode::Overlay {
            Some(Region::Inspector)
        } else {
            None
        }
    }
}

/// Major focus regions, cycled with F6 / Shift+F6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    Sidebar,
    Conversation,
    Composer,
    Inspector,
}

impl Region {
    const ORDER: [Region; 4] = [
        Region::Sidebar,
        Region::Conversation,
        Region::Composer,
        Region::Inspector,
    ];

    pub fn is_visible(self, layout: Layout) -> bool {
        match self {
            Region::Sidebar => layout.sidebar != SidebarMode::Collapsed,
            Region::Inspector => layout.inspector != InspectorMode::Hidden,
            Region::Composer => layout.composer,
            Region::Conversation => true,
        }
    }

    /// The next visible region in either direction; hidden panels are
    /// skipped. A modal overlay traps the cycle: it is the only stop.
    pub fn cycle(self, layout: Layout, forward: bool) -> Region {
        if let Some(modal) = layout.modal() {
            return modal;
        }
        let len = Self::ORDER.len();
        let start = Self::ORDER.iter().position(|r| *r == self).unwrap_or(0);
        (1..=len)
            .map(|step| {
                let index = if forward {
                    (start + step) % len
                } else {
                    (start + len - step) % len
                };
                Self::ORDER[index]
            })
            .find(|region| region.is_visible(layout))
            .unwrap_or(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionGroup {
    Pinned,
    Active,
    Recent,
    Archived,
}

impl SessionGroup {
    pub fn label(self) -> &'static str {
        match self {
            SessionGroup::Pinned => "Pinned",
            SessionGroup::Active => "Active",
            SessionGroup::Recent => "Recent",
            SessionGroup::Archived => "Archived",
        }
    }
}

/// One sidebar row: everything the list paints, already derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub group: SessionGroup,
    pub run_state: RunState,
    pub preview: Option<String>,
    pub tags: Vec<String>,
}

impl SessionRow {
    /// View-only metadata projection. Callers decide whether archived rows
    /// belong in their view; the default sidebar excludes them.
    pub fn of(session: &SessionInfo, running: &HashSet<String>, connected: bool) -> Self {
        let run_state = RunState::of(session, running, connected);
        let group = if session.archived {
            SessionGroup::Archived
        } else if session.pinned {
            SessionGroup::Pinned
        } else if matches!(run_state, RunState::Running | RunState::Paused) {
            SessionGroup::Active
        } else {
            SessionGroup::Recent
        };
        Self {
            id: session.id.clone(),
            title: session.name.clone(),
            group,
            run_state,
            preview: session.preview.clone(),
            tags: session.tags.clone(),
        }
    }
}

/// The Session list with selection kept apart from activation: arrows move
/// the selection, only an explicit activation changes the open Session.
#[derive(Debug, Default)]
pub struct Sidebar {
    rows: Vec<SessionRow>,
    selected: Option<String>,
    active: Option<String>,
}

impl Sidebar {
    pub fn rows(&self) -> &[SessionRow] {
        &self.rows
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.index_of(self.selected.as_deref()?)
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub fn active_row(&self) -> Option<&SessionRow> {
        self.rows.get(self.index_of(self.active.as_deref()?)?)
    }

    /// A canonical daemon snapshot confirms only this Session's Run state.
    pub fn set_run_state(&mut self, id: &str, state: RunState) {
        if let Some(index) = self.index_of(id) {
            self.rows[index].run_state = state;
        }
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.id == id)
    }

    /// Replace the list from a fresh `ListSessions` result. Archived Sessions
    /// stay out of the default list. Selection and the open Session follow
    /// their ids across reordering; an id that vanished is cleared, never
    /// silently moved to a neighbour.
    pub fn replace(
        &mut self,
        sessions: &[SessionInfo],
        running: &HashSet<String>,
        connected: bool,
    ) {
        let mut rows: Vec<SessionRow> = sessions
            .iter()
            .filter(|session| !session.archived)
            .map(|session| SessionRow::of(session, running, connected))
            .collect();
        // Stable: the daemon's order is kept inside each group.
        rows.sort_by_key(|row| match row.group {
            SessionGroup::Pinned => 0,
            SessionGroup::Active => 1,
            SessionGroup::Recent => 2,
            SessionGroup::Archived => 3,
        });
        self.rows = rows;
        if self
            .selected
            .as_deref()
            .is_some_and(|id| self.index_of(id).is_none())
        {
            self.selected = None;
        }
        if self
            .active
            .as_deref()
            .is_some_and(|id| self.index_of(id).is_none())
        {
            self.active = None;
        }
    }

    /// Select by Session id, which stays valid when the list reorders.
    pub fn select_id(&mut self, id: &str) {
        if self.index_of(id).is_some() {
            self.selected = Some(id.to_string());
        }
    }

    pub fn select_index(&mut self, index: usize) {
        if let Some(row) = self.rows.get(index) {
            self.selected = Some(row.id.clone());
        }
    }

    /// Move the selection by `delta` rows, clamped; with nothing selected the
    /// first move lands on the first row.
    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let next = match self.selected_index() {
            Some(index) => index.saturating_add_signed(delta).min(self.rows.len() - 1),
            None => 0,
        };
        self.select_index(next);
    }

    /// Open the selected Session. Returns whether the open Session changed.
    pub fn activate_selected(&mut self) -> bool {
        match &self.selected {
            Some(id) if self.active.as_ref() != Some(id) => {
                self.active = Some(id.clone());
                true
            }
            _ => false,
        }
    }
}

/// Unsent composer text per Session, bounded both by how many Sessions it
/// remembers and by the total bytes of text it holds.
///
/// The byte bound has a precondition: it holds only if no single draft given
/// to [`Drafts::set`] is larger than the byte budget. The draft stored most
/// recently is always kept, even when it alone exceeds the budget, because
/// silently discarding what the user just wrote is worse than holding one
/// oversized string; the store then holds that one draft and nothing else.
/// The shell meets the precondition: it only stores drafts within the
/// composer limit (`memory::limits::COMPOSER_MAX_BYTES`), which is checked at
/// compile time to be no larger than the draft budget. Older drafts are
/// evicted, oldest first, until both bounds hold again.
#[derive(Debug)]
pub struct Drafts {
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    entries: VecDeque<(String, String)>,
}

impl Drafts {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            max_bytes,
            bytes: 0,
            entries: VecDeque::new(),
        }
    }

    pub fn get(&self, session_id: &str) -> &str {
        self.entries
            .iter()
            .find(|(id, _)| id == session_id)
            .map_or("", |(_, text)| text.as_str())
    }

    /// Store a draft; an empty draft frees its slot. Returns the ids of the
    /// Sessions whose older drafts were evicted to make room, oldest first.
    /// Eviction discards what a user wrote, so the caller must tell them.
    #[must_use = "evicted drafts are lost user text and have to be reported"]
    pub fn set(&mut self, session_id: &str, text: &str) -> Vec<String> {
        if let Some(index) = self.entries.iter().position(|(id, _)| id == session_id) {
            if let Some((_, old)) = self.entries.remove(index) {
                self.bytes -= old.len();
            }
        }
        if text.is_empty() {
            return Vec::new();
        }
        self.bytes += text.len();
        self.entries
            .push_back((session_id.to_string(), text.to_string()));
        let mut evicted = Vec::new();
        while self.entries.len() > 1
            && (self.entries.len() > self.max_entries || self.bytes > self.max_bytes)
        {
            if let Some((id, text)) = self.entries.pop_front() {
                self.bytes -= text.len();
                evicted.push(id);
            }
        }
        evicted
    }

    /// Whether a draft is stored for this Session.
    pub fn has(&self, session_id: &str) -> bool {
        self.entries.iter().any(|(id, _)| id == session_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of draft text currently held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    User,
    Assistant,
    ToolSummary,
    RunRecap,
    /// A note the daemon adds to a transcript (role `system-note`).
    Note,
}

impl BlockKind {
    pub fn label(self) -> &'static str {
        match self {
            BlockKind::User => "You",
            BlockKind::Assistant => "Formless",
            BlockKind::ToolSummary => "Tool",
            BlockKind::RunRecap => "Run recap",
            BlockKind::Note => "Note",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, pinned: bool, status: Option<&str>) -> SessionInfo {
        SessionInfo {
            id: id.to_string(),
            forked_from: None,
            fork_point_seq: None,
            name: format!("Session {id}"),
            model: None,
            reasoning_effort: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            cwd: None,
            message_count: 0,
            preview: None,
            last_run_status: status.map(str::to_string),
            pinned,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        }
    }

    fn running(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn text_scale_steps_between_100_and_200_percent_and_stops_at_the_ends() {
        let mut scale = TextScale::default();
        assert_eq!((scale.percent(), scale.factor()), (100, 1.0));
        assert_eq!(scale.smaller(), TextScale::DEFAULT);
        let mut seen = vec![scale.percent()];
        while scale.larger() != scale {
            scale = scale.larger();
            seen.push(scale.percent());
        }
        assert_eq!(seen, TextScale::STEPS);
        assert_eq!(scale, TextScale::LARGEST);
        assert_eq!(scale.factor(), 2.0);
        assert_eq!(scale.smaller().percent(), 175);
        assert_eq!(TextScale::from_percent(200), TextScale::LARGEST);
        assert_eq!(TextScale::from_percent(140).percent(), 150);
        assert_eq!(TextScale::from_percent(0).percent(), 100);
        assert_eq!(TextScale::from_percent(9_999).percent(), 200);
    }

    #[test]
    fn a_window_smaller_than_the_text_size_needs_is_reflowed_at_that_size() {
        let largest = TextScale::LARGEST;
        assert_eq!(largest.minimum_window(), (960.0, 720.0));
        // Either dimension one pixel short is enough.
        for (width, height, fit) in [
            (480.0, 360.0, Fit::Reflowed),
            (959.0, 4_000.0, Fit::Reflowed),
            (4_000.0, 719.0, Fit::Reflowed),
            (960.0, 720.0, Fit::Whole),
            (4_000.0, 4_000.0, Fit::Whole),
        ] {
            assert_eq!(largest.fit(width, height), fit, "{width} x {height}");
        }
        // Each step has its own minimum.
        for (percent, width, height) in [
            (100, 480.0, 360.0),
            (125, 600.0, 450.0),
            (150, 720.0, 540.0),
            (175, 840.0, 630.0),
        ] {
            let scale = TextScale::from_percent(percent);
            assert_eq!(scale.fit(width, height), Fit::Whole, "{percent}%");
            assert_eq!(scale.fit(width - 1.0, height), Fit::Reflowed, "{percent}%");
            assert_eq!(scale.fit(width, height - 1.0), Fit::Reflowed, "{percent}%");
        }
    }

    #[test]
    fn a_reflowed_window_never_docks_a_panel_however_wide_it_is() {
        let largest = TextScale::LARGEST;
        // Whole: the breakpoints see the width divided by the factor.
        assert_eq!(largest.layout_width(2_880.0, 1_800.0), 1_440.0);
        // Wide enough to dock both panels, but too short: reflowed, and the
        // panels are overlays.
        let width = largest.layout_width(2_880.0, 600.0);
        assert!(width < tokens::SIDEBAR_DOCK_MIN);
        let layout = Layout::for_width(width, PanelPrefs::default());
        assert_eq!(layout.sidebar, SidebarMode::Collapsed);
        assert_eq!(layout.inspector, InspectorMode::Hidden);
        // Narrow and reflowed: the real width, scaled.
        assert_eq!(largest.layout_width(480.0, 360.0), 240.0);
    }

    #[test]
    fn at_200_percent_a_window_lays_out_like_one_half_as_wide() {
        let prefs = PanelPrefs::default();
        let factor = TextScale::LARGEST.factor();
        // 1440 px at 200% is 720 effective: too narrow for either panel.
        let layout = Layout::for_width(1440.0 / factor, prefs);
        assert_eq!(layout.sidebar, SidebarMode::Collapsed);
        assert_eq!(layout.inspector, InspectorMode::Hidden);
        // 2880 px at 200% is the three-pane layout again.
        let layout = Layout::for_width(2880.0 / factor, prefs);
        assert_eq!(layout.sidebar, SidebarMode::Docked);
        assert_eq!(layout.inspector, InspectorMode::Docked);
    }

    #[test]
    fn navigation_destinations_stay_persistent_and_both_search_scopes_are_transient() {
        let mut all = Destination::NAVIGATION.to_vec();
        all.push(Destination::Search);
        all.push(Destination::ProfileSearch);
        assert_eq!(all.len(), 8);
        assert_eq!(all.iter().collect::<HashSet<_>>().len(), 8);
        assert!(Destination::NAVIGATION.iter().all(|d| !d.is_transient()));
        assert!(Destination::Search.is_transient());
        assert!(Destination::ProfileSearch.is_transient());
    }

    #[test]
    fn run_state_is_unknown_while_disconnected_even_if_it_was_running() {
        let s = session("a", false, Some("paused"));
        assert_eq!(RunState::of(&s, &running(&["a"]), false), RunState::Unknown);
        assert_eq!(RunState::of(&s, &running(&["a"]), true), RunState::Running);
        assert_eq!(RunState::of(&s, &running(&[]), true), RunState::Paused);
        let idle = session("b", false, Some("completed"));
        assert_eq!(RunState::of(&idle, &running(&[]), true), RunState::Idle);
    }

    #[test]
    fn composer_offers_steer_and_queue_only_during_a_run_and_keeps_stop_separate() {
        let idle = ComposerActions::for_state(RunState::Idle, false);
        assert_eq!((idle.primary, idle.secondary), (SendMode::Send, None));
        assert!(!idle.stop_enabled);
        assert_eq!(idle.disabled_reason, None);

        let busy = ComposerActions::for_state(RunState::Running, false);
        assert_eq!(busy.primary, SendMode::Steer);
        assert_eq!(busy.secondary, Some(SendMode::Queue));
        assert!(busy.stop_enabled);

        // Stop does not depend on having a draft.
        let busy_empty = ComposerActions::for_state(RunState::Running, true);
        assert!(busy_empty.stop_enabled);
        assert!(busy_empty.disabled_reason.is_some());
    }

    #[test]
    fn composer_explains_why_sending_is_disabled() {
        let empty = ComposerActions::for_state(RunState::Idle, true);
        assert_eq!(empty.disabled_reason, Some("Write a message first"));
        let offline = ComposerActions::for_state(RunState::Unknown, false);
        assert_eq!(
            offline.disabled_reason,
            Some("Not connected to the Daemon Core")
        );
        assert!(!offline.stop_enabled);
    }

    #[test]
    fn narrowing_hides_the_inspector_before_collapsing_the_sidebar() {
        let prefs = PanelPrefs::default();
        let wide = Layout::for_width(1440.0, prefs);
        assert_eq!(wide.sidebar, SidebarMode::Docked);
        assert_eq!(wide.inspector, InspectorMode::Docked);
        assert_eq!(wide.modal(), None);

        let at_threshold = Layout::for_width(tokens::THREE_PANE_MIN, prefs);
        assert_eq!(at_threshold.inspector, InspectorMode::Docked);

        // Narrowing alone never puts a panel over the conversation.
        let medium = Layout::for_width(1279.0, prefs);
        assert_eq!(medium.sidebar, SidebarMode::Docked);
        assert_eq!(medium.inspector, InspectorMode::Hidden);

        let narrow = Layout::for_width(tokens::SIDEBAR_DOCK_MIN - 1.0, prefs);
        assert_eq!(narrow.sidebar, SidebarMode::Collapsed);
        assert_eq!(narrow.inspector, InspectorMode::Hidden);
        assert_eq!(narrow.modal(), None);
    }

    #[test]
    fn the_inspector_toggle_opens_an_overlay_only_when_narrow() {
        let mut prefs = PanelPrefs::default();
        prefs.toggle_inspector(1100.0);
        let layout = Layout::for_width(1100.0, prefs);
        assert_eq!(layout.inspector, InspectorMode::Overlay);
        assert_eq!(layout.modal(), Some(Region::Inspector));
        // The docked preference was not touched.
        assert!(prefs.inspector_open);
        prefs.toggle_inspector(1100.0);
        assert_eq!(
            Layout::for_width(1100.0, prefs).inspector,
            InspectorMode::Hidden
        );

        prefs.toggle_inspector(1440.0);
        assert_eq!(
            Layout::for_width(1440.0, prefs).inspector,
            InspectorMode::Hidden
        );
        assert_eq!(
            Layout::for_width(1100.0, prefs).inspector,
            InspectorMode::Hidden
        );
    }

    #[test]
    fn the_sidebar_toggle_opens_an_overlay_when_too_narrow_to_dock() {
        let narrow = tokens::SIDEBAR_DOCK_MIN - 1.0;
        let mut prefs = PanelPrefs::default();
        assert_eq!(
            Layout::for_width(narrow, prefs).sidebar,
            SidebarMode::Collapsed
        );

        prefs.toggle_sidebar(narrow);
        let layout = Layout::for_width(narrow, prefs);
        assert_eq!(layout.sidebar, SidebarMode::Overlay);
        assert_eq!(layout.modal(), Some(Region::Sidebar));
        // The docked preference was not touched: widening docks it.
        assert!(prefs.sidebar_open);
        assert_eq!(
            Layout::for_width(1440.0, prefs).sidebar,
            SidebarMode::Docked
        );

        prefs.toggle_sidebar(narrow);
        assert_eq!(
            Layout::for_width(narrow, prefs).sidebar,
            SidebarMode::Collapsed
        );

        // Wide enough to dock, the same toggle collapses and restores it.
        prefs.toggle_sidebar(1440.0);
        assert_eq!(
            Layout::for_width(1440.0, prefs).sidebar,
            SidebarMode::Collapsed
        );
        prefs.toggle_sidebar(1440.0);
        assert_eq!(
            Layout::for_width(1440.0, prefs).sidebar,
            SidebarMode::Docked
        );
    }

    #[test]
    fn only_one_overlay_is_up_at_a_time() {
        let narrow = 800.0;
        let mut prefs = PanelPrefs::default();
        prefs.toggle_sidebar(narrow);
        prefs.toggle_inspector(narrow);
        let layout = Layout::for_width(narrow, prefs);
        assert_eq!(layout.sidebar, SidebarMode::Collapsed);
        assert_eq!(layout.inspector, InspectorMode::Overlay);

        prefs.toggle_sidebar(narrow);
        let layout = Layout::for_width(narrow, prefs);
        assert_eq!(layout.sidebar, SidebarMode::Overlay);
        assert_eq!(layout.inspector, InspectorMode::Hidden);

        prefs.close_overlays();
        assert_eq!(Layout::for_width(narrow, prefs).modal(), None);
    }

    #[test]
    fn widening_forgets_an_overlay_so_narrowing_again_does_not_reopen_it() {
        let mut prefs = PanelPrefs::default();
        prefs.toggle_inspector(1100.0);
        assert!(!prefs.settle(1100.0), "still applies at this width");
        assert!(prefs.settle(1440.0));
        assert_eq!(
            Layout::for_width(1100.0, prefs).inspector,
            InspectorMode::Hidden
        );

        prefs.toggle_sidebar(800.0);
        assert!(prefs.settle(1000.0));
        assert_eq!(
            Layout::for_width(800.0, prefs).sidebar,
            SidebarMode::Collapsed
        );
    }

    #[test]
    fn closed_panels_stay_closed_at_any_width() {
        let prefs = PanelPrefs {
            sidebar_open: false,
            inspector_open: false,
            ..PanelPrefs::default()
        };
        let layout = Layout::for_width(4000.0, prefs);
        assert_eq!(layout.sidebar, SidebarMode::Collapsed);
        assert_eq!(layout.inspector, InspectorMode::Hidden);
    }

    #[test]
    fn region_cycle_skips_hidden_panels_in_both_directions() {
        let all = Layout::for_width(1440.0, PanelPrefs::default());
        assert_eq!(Region::Sidebar.cycle(all, true), Region::Conversation);
        assert_eq!(Region::Inspector.cycle(all, true), Region::Sidebar);
        assert_eq!(Region::Sidebar.cycle(all, false), Region::Inspector);

        let bare = Layout::for_width(
            1440.0,
            PanelPrefs {
                sidebar_open: false,
                inspector_open: false,
                ..PanelPrefs::default()
            },
        );
        assert_eq!(Region::Composer.cycle(bare, true), Region::Conversation);
        assert_eq!(Region::Conversation.cycle(bare, false), Region::Composer);
        // Focus left behind in a panel that just closed moves to a visible one.
        assert_eq!(Region::Inspector.cycle(bare, true), Region::Conversation);
    }

    #[test]
    fn a_modal_overlay_traps_the_region_cycle() {
        let mut prefs = PanelPrefs::default();
        prefs.toggle_inspector(1100.0);
        let layout = Layout::for_width(1100.0, prefs);
        for region in [
            Region::Sidebar,
            Region::Conversation,
            Region::Composer,
            Region::Inspector,
        ] {
            assert_eq!(region.cycle(layout, true), Region::Inspector);
            assert_eq!(region.cycle(layout, false), Region::Inspector);
        }
    }

    #[test]
    fn only_chat_has_a_composer_region() {
        let chat = Layout::for_width(1440.0, PanelPrefs::default());
        assert!(Region::Composer.is_visible(chat));
        for destination in Destination::NAVIGATION {
            let layout = chat.for_destination(destination);
            assert_eq!(layout.composer, destination == Destination::Chat);
        }
        let tools = chat.for_destination(Destination::Tools);
        assert!(!Region::Composer.is_visible(tools));
        // The cycle skips the missing composer.
        assert_eq!(Region::Conversation.cycle(tools, true), Region::Inspector);
        assert_eq!(Region::Inspector.cycle(tools, false), Region::Conversation);
    }

    #[test]
    fn sidebar_groups_pinned_then_active_then_recent_and_hides_archived() {
        let mut archived = session("z", false, None);
        archived.archived = true;
        let sessions = [
            session("recent", false, None),
            session("paused", false, Some("paused")),
            session("pinned", true, None),
            session("running", false, None),
            archived,
        ];
        let mut sidebar = Sidebar::default();
        sidebar.replace(&sessions, &running(&["running"]), true);
        let order: Vec<(&str, SessionGroup)> = sidebar
            .rows()
            .iter()
            .map(|row| (row.id.as_str(), row.group))
            .collect();
        assert_eq!(
            order,
            [
                ("pinned", SessionGroup::Pinned),
                ("paused", SessionGroup::Active),
                ("running", SessionGroup::Active),
                ("recent", SessionGroup::Recent),
            ]
        );
    }

    #[test]
    fn moving_the_selection_does_not_open_a_session() {
        let sessions = [session("a", false, None), session("b", false, None)];
        let mut sidebar = Sidebar::default();
        sidebar.replace(&sessions, &running(&[]), true);
        sidebar.move_selection(1);
        assert_eq!(sidebar.selected_index(), Some(0));
        sidebar.move_selection(1);
        sidebar.move_selection(5);
        assert_eq!(sidebar.selected_index(), Some(1));
        assert_eq!(sidebar.active_id(), None);

        assert!(sidebar.activate_selected());
        assert_eq!(sidebar.active_id(), Some("b"));
        // Activating the already open Session is not a change.
        assert!(!sidebar.activate_selected());

        sidebar.move_selection(-9);
        assert_eq!(sidebar.selected_index(), Some(0));
        assert_eq!(sidebar.active_id(), Some("b"));

        sidebar.select_id("b");
        assert_eq!(sidebar.selected_index(), Some(1));
        sidebar.select_id("missing");
        assert_eq!(sidebar.selected_index(), Some(1));
    }

    #[test]
    fn selection_follows_its_session_across_reordering_and_clears_when_it_vanishes() {
        let mut sidebar = Sidebar::default();
        sidebar.replace(
            &[session("a", false, None), session("b", false, None)],
            &running(&[]),
            true,
        );
        sidebar.select_index(1);
        sidebar.activate_selected();

        // "b" starts running and "c" is pinned: both move, "b" stays selected.
        sidebar.replace(
            &[
                session("a", false, None),
                session("b", false, None),
                session("c", true, None),
            ],
            &running(&["b"]),
            true,
        );
        assert_eq!(sidebar.selected_index(), Some(1));
        assert_eq!(sidebar.rows()[1].id, "b");
        assert_eq!(sidebar.active_id(), Some("b"));

        sidebar.replace(&[session("a", false, None)], &running(&[]), true);
        assert_eq!(sidebar.selected_index(), None);
        assert_eq!(sidebar.active_id(), None);
    }

    const NONE: [&str; 0] = [];

    #[test]
    fn drafts_are_bounded_by_entries_and_empty_drafts_free_their_slot() {
        let mut drafts = Drafts::new(2, usize::MAX);
        assert_eq!(drafts.set("a", "one"), NONE);
        assert_eq!(drafts.set("b", "two"), NONE);
        assert_eq!(drafts.set("a", "one again"), NONE);
        // "b" was the least recently stored, and the caller is told.
        assert_eq!(drafts.set("c", "three"), ["b"]);
        assert_eq!(drafts.get("b"), "");
        assert!(!drafts.has("b"));
        assert_eq!(drafts.get("a"), "one again");
        assert_eq!(drafts.get("c"), "three");
        assert!(drafts.has("a") && drafts.has("c"));
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts.bytes(), "one again".len() + "three".len());

        // Clearing a draft evicts nothing.
        assert_eq!(drafts.set("a", ""), NONE);
        assert_eq!(drafts.len(), 1);
        assert!(!drafts.is_empty());
        assert_eq!(drafts.bytes(), "three".len());
    }

    #[test]
    fn drafts_are_bounded_by_bytes_and_report_every_eviction_oldest_first() {
        let mut drafts = Drafts::new(100, 10);
        assert_eq!(drafts.set("a", "aaaa"), NONE);
        assert_eq!(drafts.set("b", "bbbb"), NONE);
        assert_eq!(drafts.bytes(), 8);
        // Twelve bytes would exceed ten: the oldest goes.
        assert_eq!(drafts.set("c", "cccc"), ["a"]);
        assert_eq!(drafts.get("a"), "");
        assert_eq!(drafts.get("b"), "bbbb");
        assert_eq!(drafts.get("c"), "cccc");
        assert_eq!(drafts.bytes(), 8);

        // Bytes, not characters: three Thai letters are nine bytes, and
        // making room for them costs both older drafts.
        assert_eq!(drafts.set("d", "กขค"), ["b", "c"]);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts.get("d"), "กขค");
        assert_eq!(drafts.bytes(), 9);

        // Replacing a draft counts its new size only.
        assert_eq!(drafts.set("d", "กข"), NONE);
        assert_eq!(drafts.bytes(), 6);
    }

    #[test]
    fn the_newest_draft_is_kept_even_when_it_alone_exceeds_the_byte_budget() {
        let mut drafts = Drafts::new(100, 10);
        assert_eq!(drafts.set("a", "small"), NONE);
        let large = "x".repeat(64);
        assert_eq!(drafts.set("b", &large), ["a"]);
        assert_eq!(drafts.get("a"), "");
        assert_eq!(drafts.get("b"), large);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts.bytes(), 64);

        // The next draft evicts the oversized one and the budget holds again.
        assert_eq!(drafts.set("c", "tiny"), ["b"]);
        assert_eq!(drafts.get("b"), "");
        assert_eq!(drafts.bytes(), 4);
    }
}
