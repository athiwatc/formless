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
}

/// The seven destinations of the existing clients. Search is transient: it
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
        }
    }

    pub fn is_transient(self) -> bool {
        self == Destination::Search
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
            RunState::Unknown => "Not connected",
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

/// A typed request the shell would hand to the client layer. Phase 0 has no
/// client layer: the shell records the intent and leaves the draft in place.
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
}

impl SessionGroup {
    pub fn label(self) -> &'static str {
        match self {
            SessionGroup::Pinned => "Pinned",
            SessionGroup::Active => "Active",
            SessionGroup::Recent => "Recent",
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
            .map(|session| {
                let run_state = RunState::of(session, running, connected);
                let group = if session.pinned {
                    SessionGroup::Pinned
                } else if matches!(run_state, RunState::Running | RunState::Paused) {
                    SessionGroup::Active
                } else {
                    SessionGroup::Recent
                };
                SessionRow {
                    id: session.id.clone(),
                    title: session.name.clone(),
                    group,
                    run_state,
                    preview: session.preview.clone(),
                }
            })
            .collect();
        // Stable: the daemon's order is kept inside each group.
        rows.sort_by_key(|row| match row.group {
            SessionGroup::Pinned => 0,
            SessionGroup::Active => 1,
            SessionGroup::Recent => 2,
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
/// The draft stored most recently is always kept, even when it alone exceeds
/// the byte budget: silently discarding what the user just wrote is worse
/// than holding one oversized string. Older drafts are evicted, oldest
/// first, until both bounds hold again.
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

    /// Store a draft; an empty draft frees its slot.
    pub fn set(&mut self, session_id: &str, text: &str) {
        if let Some(index) = self.entries.iter().position(|(id, _)| id == session_id) {
            if let Some((_, old)) = self.entries.remove(index) {
                self.bytes -= old.len();
            }
        }
        if text.is_empty() {
            return;
        }
        self.bytes += text.len();
        self.entries
            .push_back((session_id.to_string(), text.to_string()));
        while self.entries.len() > 1
            && (self.entries.len() > self.max_entries || self.bytes > self.max_bytes)
        {
            if let Some((_, evicted)) = self.entries.pop_front() {
                self.bytes -= evicted.len();
            }
        }
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
}

impl BlockKind {
    pub fn label(self) -> &'static str {
        match self {
            BlockKind::User => "You",
            BlockKind::Assistant => "Formless",
            BlockKind::ToolSummary => "Tool",
            BlockKind::RunRecap => "Run recap",
        }
    }
}

/// One timeline row. `id` is stable for the life of the transcript so list
/// state, selection and caches can key on it instead of on a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineBlock {
    pub id: u64,
    pub kind: BlockKind,
    pub markdown: String,
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
    fn all_seven_destinations_exist_and_only_search_is_transient() {
        let mut all = Destination::NAVIGATION.to_vec();
        all.push(Destination::Search);
        assert_eq!(all.len(), 7);
        assert_eq!(all.iter().collect::<HashSet<_>>().len(), 7);
        assert!(Destination::NAVIGATION.iter().all(|d| !d.is_transient()));
        assert!(Destination::Search.is_transient());
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

    #[test]
    fn drafts_are_bounded_by_entries_and_empty_drafts_free_their_slot() {
        let mut drafts = Drafts::new(2, usize::MAX);
        drafts.set("a", "one");
        drafts.set("b", "two");
        drafts.set("a", "one again");
        drafts.set("c", "three");
        // "b" was the least recently stored.
        assert_eq!(drafts.get("b"), "");
        assert_eq!(drafts.get("a"), "one again");
        assert_eq!(drafts.get("c"), "three");
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts.bytes(), "one again".len() + "three".len());

        drafts.set("a", "");
        assert_eq!(drafts.len(), 1);
        assert!(!drafts.is_empty());
        assert_eq!(drafts.bytes(), "three".len());
    }

    #[test]
    fn drafts_are_bounded_by_bytes_and_evict_oldest_first() {
        let mut drafts = Drafts::new(100, 10);
        drafts.set("a", "aaaa");
        drafts.set("b", "bbbb");
        assert_eq!(drafts.bytes(), 8);
        // Twelve bytes would exceed ten: the oldest goes.
        drafts.set("c", "cccc");
        assert_eq!(drafts.get("a"), "");
        assert_eq!(drafts.get("b"), "bbbb");
        assert_eq!(drafts.get("c"), "cccc");
        assert_eq!(drafts.bytes(), 8);

        // Bytes, not characters: three Thai letters are nine bytes.
        drafts.set("d", "กขค");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts.get("d"), "กขค");
        assert_eq!(drafts.bytes(), 9);

        // Replacing a draft counts its new size only.
        drafts.set("d", "กข");
        assert_eq!(drafts.bytes(), 6);
    }

    #[test]
    fn the_newest_draft_is_kept_even_when_it_alone_exceeds_the_byte_budget() {
        let mut drafts = Drafts::new(100, 10);
        drafts.set("a", "small");
        let large = "x".repeat(64);
        drafts.set("b", &large);
        assert_eq!(drafts.get("a"), "");
        assert_eq!(drafts.get("b"), large);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts.bytes(), 64);

        // The next draft evicts the oversized one and the budget holds again.
        drafts.set("c", "tiny");
        assert_eq!(drafts.get("b"), "");
        assert_eq!(drafts.bytes(), 4);
    }
}
