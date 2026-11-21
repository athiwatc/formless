//! The shell's commands: one typed list behind the header toolbar, the
//! composer's buttons, the global keyboard shortcuts and the command
//! palette. No GPUI types, no I/O.
//!
//! Each [`Command`] has one [`Entry`] for a given [`State`]: its labels,
//! shortcut, whether it is on (for a toggle) and, when it cannot run, the
//! reason in words. A control that shows a command and the code that runs
//! it read the same entry, so a button, its shortcut and its palette row
//! can never disagree about whether it is available.
//!
//! The composer commands keep the existing intent semantics
//! ([`ComposerActions`]): they only say which typed [`crate::model::Intent`]
//! a control would record. Nothing here sends anything.

use crate::model::{send_mode_label, ComposerActions, Destination, TextScale};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryMove {
    First,
    Older,
    Newer,
    Latest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    NewSession,
    SessionUsage,
    ToggleStepMode,
    RunAction(crate::run_actions::Kind),
    /// The composer's primary action: Send, or Steer while a Run is going.
    Send,
    /// The composer's second action: Queue, only while a Run is going.
    SendSecondary,
    Stop,
    ResumeRun,
    AbandonRun,
    Questions,
    Approvals,
    SessionDetails,
    SessionModel,
    SessionPendingFiles,
    MarkComparisonLeft,
    CompareMarked,
    InspectRun,
    ReviewFork,
    ViewInvocations,
    OpenInvocationSession,
    ToggleArchived,
    ToggleSessions,
    ToggleContext,
    ToggleTheme,
    ToggleEnterSends,
    SaveDesktopPreferences,
    ReloadDesktopPreferences,
    IncreaseTextSize,
    DecreaseTextSize,
    ResetTextSize,
    FocusNextRegion,
    FocusPreviousRegion,
    MoveHistory(HistoryMove),
    GoTo(Destination),
    /// Open the command palette, or close it if it is open.
    ShowCommands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Composer,
    Run,
    Session,
    Panels,
    Navigation,
    Focus,
    Text,
    Appearance,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Composer => "Composer",
            Group::Run => "Run",
            Group::Session => "Session",
            Group::Panels => "Panels",
            Group::Navigation => "Go to",
            Group::Focus => "Focus",
            Group::Text => "Text size",
            Group::Appearance => "Appearance",
        }
    }
}

/// What the commands depend on, read from the shell when it is drawn and
/// again when a command is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub composer: ComposerActions,
    /// Whether the open destination has a composer (only Chat does).
    pub has_composer: bool,
    pub has_history: bool,
    /// A current, connected canonical snapshot has a recoverable last Run.
    pub can_recover: bool,
    pub can_questions: bool,
    pub can_approvals: bool,
    pub can_create_session: bool,
    pub usage_reason: Option<&'static str>,
    pub step_mode_reason: Option<&'static str>,
    pub can_search_profile: bool,
    pub can_edit_session: bool,
    pub can_browse_archived: bool,
    pub archived_included: bool,
    /// Whether an input method is composing text in the composer.
    pub composing: bool,
    pub enter_sends: bool,
    pub can_preferences: bool,
    pub can_mark_comparison: bool,
    pub comparison_reason: Option<&'static str>,
    pub run_inspection_reason: Option<&'static str>,
    pub fork_reason: Option<&'static str>,
    pub invocations_reason: Option<&'static str>,
    pub invocation_open_reason: Option<&'static str>,
    pub sessions_shown: bool,
    pub context_shown: bool,
    pub dark: bool,
    pub text_scale: TextScale,
    pub destination: Destination,
    pub palette_open: bool,
}

/// One command as it stands in one [`State`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub command: Command,
    /// The short label a toolbar button or composer button shows.
    pub label: &'static str,
    /// The label a palette row shows, which names the command on its own.
    pub title: String,
    pub group: Group,
    /// Tooltip text: what the command does.
    pub hint: &'static str,
    /// For a toggle or a destination, whether it is on now.
    pub checked: Option<bool>,
    /// Why the command cannot run now; `None` when it can.
    pub disabled_reason: Option<&'static str>,
}

impl Entry {
    pub fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }

    /// The shortcut shown for this command, if it has one.
    pub fn shortcut(&self) -> Option<String> {
        keys(self.command).first().map(|key| shortcut_text(key))
    }

    /// Whether every word of `query` is found, ignoring case, in the
    /// command's title, group or hint. An empty query matches everything.
    pub fn matches(&self, query: &str) -> bool {
        let haystack =
            format!("{} {} {}", self.title, self.group.label(), self.hint).to_lowercase();
        query
            .to_lowercase()
            .split_whitespace()
            .all(|word| haystack.contains(word))
    }
}

pub const NO_COMPOSER: &str = "Only Chat has a composer";
pub const COMPOSING: &str = "Finish composing text first";
pub const NO_SECONDARY: &str =
    "Queue is unavailable in this view or Run; native Queue requires Formless TUI";
pub const NO_RUN: &str = "No Run is in progress";
pub const NO_RECOVERY: &str = "No confirmed paused or interrupted Run is available";
pub const LARGEST: &str = "Text is already at its largest size";
pub const SMALLEST: &str = "Text is already at its smallest size";
pub const DEFAULT_SIZE: &str = "Text is already at its default size";

/// The global keyboard shortcuts, in GPUI keystroke syntax; the first is
/// the one shown. Every one has a modifier or is a function key, so none of
/// them is ever text typed into the composer or a search field.
pub fn keys(command: Command) -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    macro_rules! primary {
        ($key:literal) => {
            concat!("cmd-", $key)
        };
    }
    #[cfg(not(target_os = "macos"))]
    macro_rules! primary {
        ($key:literal) => {
            concat!("ctrl-", $key)
        };
    }
    match command {
        Command::NewSession => &[],
        Command::SessionUsage | Command::ToggleStepMode => &[],
        Command::RunAction(_) => &[],
        Command::ToggleSessions => &[primary!("b")],
        Command::ToggleContext => &[primary!("alt-b")],
        Command::IncreaseTextSize => &[primary!("="), primary!("+")],
        Command::DecreaseTextSize => &[primary!("-")],
        Command::ResetTextSize => &[primary!("0")],
        Command::FocusNextRegion => &["f6"],
        Command::FocusPreviousRegion => &["shift-f6"],
        Command::ShowCommands => &[primary!("shift-p")],
        Command::GoTo(Destination::Search) => &[primary!("f")],
        Command::Send
        | Command::SendSecondary
        | Command::Stop
        | Command::ResumeRun
        | Command::AbandonRun
        | Command::Questions
        | Command::Approvals
        | Command::SessionDetails
        | Command::SessionModel
        | Command::SessionPendingFiles
        | Command::MarkComparisonLeft
        | Command::CompareMarked
        | Command::InspectRun
        | Command::ReviewFork
        | Command::ViewInvocations
        | Command::OpenInvocationSession
        | Command::ToggleArchived
        | Command::ToggleTheme
        | Command::ToggleEnterSends
        | Command::SaveDesktopPreferences
        | Command::ReloadDesktopPreferences
        | Command::MoveHistory(_)
        | Command::GoTo(_) => &[],
    }
}

/// Native modified Enter, scoped to the composer rather than global.
pub fn composer_submit_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd-enter"
    } else {
        "ctrl-enter"
    }
}

/// Every command that has a global shortcut.
pub const GLOBAL: [Command; 9] = [
    Command::ToggleSessions,
    Command::ToggleContext,
    Command::IncreaseTextSize,
    Command::DecreaseTextSize,
    Command::ResetTextSize,
    Command::FocusNextRegion,
    Command::FocusPreviousRegion,
    Command::ShowCommands,
    Command::GoTo(Destination::Search),
];

/// A GPUI keystroke as people write it: `cmd-shift-p` is `⌘⇧P` on macOS and
/// `Ctrl+Shift+P` elsewhere.
pub fn shortcut_text(key: &str) -> String {
    // A trailing "-" is the minus key, not a separator.
    let (mods, last) = match key.strip_suffix("--") {
        Some(rest) => (rest, "-"),
        None => key.rsplit_once('-').unwrap_or(("", key)),
    };
    let mods: Vec<&str> = mods.split('-').filter(|m| !m.is_empty()).collect();
    let last = last.to_uppercase();
    if cfg!(target_os = "macos") {
        let mut text: String = mods
            .iter()
            .map(|m| match *m {
                "ctrl" => "⌃",
                "alt" => "⌥",
                "shift" => "⇧",
                "cmd" => "⌘",
                other => other,
            })
            .collect();
        text.push_str(&last);
        text
    } else {
        let mut parts: Vec<String> = mods
            .iter()
            .map(|m| match *m {
                "ctrl" => "Ctrl".to_string(),
                "alt" => "Alt".to_string(),
                "shift" => "Shift".to_string(),
                "cmd" => "Super".to_string(),
                other => other.to_string(),
            })
            .collect();
        parts.push(last);
        parts.join("+")
    }
}

/// The commands the palette lists, in order. The palette itself is not
/// one of them.
pub fn palette(state: &State) -> Vec<Entry> {
    let mut commands = vec![
        Command::Send,
        Command::NewSession,
        Command::SessionUsage,
        Command::ToggleStepMode,
    ];
    commands.extend(crate::run_actions::Kind::ALL.map(Command::RunAction));
    if state.has_composer && state.composer.secondary.is_some() {
        commands.push(Command::SendSecondary);
    }
    commands.extend([
        Command::Stop,
        Command::ResumeRun,
        Command::AbandonRun,
        Command::Questions,
        Command::Approvals,
        Command::SessionDetails,
        Command::SessionModel,
        Command::SessionPendingFiles,
        Command::MarkComparisonLeft,
        Command::CompareMarked,
        Command::InspectRun,
        Command::ReviewFork,
        Command::ViewInvocations,
        Command::OpenInvocationSession,
        Command::ToggleArchived,
        Command::ToggleSessions,
        Command::ToggleContext,
    ]);
    commands.extend(Destination::NAVIGATION.map(Command::GoTo));
    commands.push(Command::GoTo(Destination::Search));
    commands.push(Command::GoTo(Destination::ProfileSearch));
    commands.extend(
        [
            HistoryMove::First,
            HistoryMove::Older,
            HistoryMove::Newer,
            HistoryMove::Latest,
        ]
        .map(Command::MoveHistory),
    );
    commands.extend([
        Command::FocusNextRegion,
        Command::FocusPreviousRegion,
        Command::IncreaseTextSize,
        Command::DecreaseTextSize,
        Command::ResetTextSize,
        Command::ToggleTheme,
        Command::ToggleEnterSends,
        Command::SaveDesktopPreferences,
        Command::ReloadDesktopPreferences,
    ]);
    commands
        .into_iter()
        .map(|command| entry(command, state))
        .collect()
}

pub fn entry(command: Command, state: &State) -> Entry {
    let composer_reason = |reason: Option<&'static str>| {
        if !state.has_composer {
            Some(NO_COMPOSER)
        } else {
            reason.or(state.composing.then_some(COMPOSING))
        }
    };
    let (label, title, group, hint, checked, disabled_reason): (
        &'static str,
        String,
        Group,
        &'static str,
        Option<bool>,
        Option<&'static str>,
    ) = match command {
        Command::NewSession => (
            "New session", "New session".into(), Group::Session,
            "Start a new Session in this Profile", None,
            if state.composing { Some(COMPOSING) } else { (!state.can_create_session).then_some("Wait for a ready Profile connection") },
        ),
        Command::SessionUsage => (
            "Read usage", "Read recorded Session usage".into(), Group::Session,
            "Read recorded tokens and cost for the open Session", None,
            if state.composing { Some(COMPOSING) } else { state.usage_reason },
        ),
        Command::ToggleStepMode => (
            "Step mode", "Toggle Step mode".into(), Group::Run,
            "Change Step mode using the sampled daemon state", None,
            if state.composing { Some(COMPOSING) } else { state.step_mode_reason },
        ),
        Command::Send => {
            let label = send_mode_label(state.composer.primary);
            (
                label,
                label.to_string(),
                Group::Composer,
                "Record the composer's message as the primary action",
                None,
                composer_reason(state.composer.disabled_reason),
            )
        }
        Command::SendSecondary => {
            let label = state.composer.secondary.map_or("Queue", send_mode_label);
            let reason = if state.composer.secondary.is_none() {
                Some(NO_SECONDARY)
            } else {
                state.composer.disabled_reason
            };
            (
                label,
                label.to_string(),
                Group::Composer,
                "Record the composer's message to run after the current Run",
                None,
                composer_reason(reason),
            )
        }
        Command::ToggleEnterSends => (
            "Enter sends",
            if state.enter_sends { "Use Enter for newlines" } else { "Use Enter to send" }.to_string(),
            Group::Composer,
            "Change send keys for this window; drafts and undo stay intact",
            Some(state.enter_sends),
            if !state.has_composer { Some(NO_COMPOSER) } else if state.composing { Some(COMPOSING) } else { None },
        ),
        Command::ViewInvocations=>("Invocations", "View unfinished Tool Invocations".into(),Group::Navigation,"Read a Profile-wide snapshot of unfinished Tool work. No cancel, abandon or model call",None,if state.composing {Some(COMPOSING)}else{state.invocations_reason}),
        Command::OpenInvocationSession=>("Open Session", "Open selected Invocation's Session".into(),Group::Navigation,"Navigate to the available unarchived Session associated with the selected read-only record",None,if state.composing {Some(COMPOSING)}else{state.invocation_open_reason}),
        Command::RunAction(kind) => (kind.label(), kind.label().into(), Group::Run, kind.help(), None, if state.composing { Some(COMPOSING) } else if state.can_edit_session && state.destination == Destination::Chat { None } else { Some("Open a connected Session in Chat first") }),
        Command::ReviewFork=>("Review fork", "Review a new Session fork".into(),Group::Session,"Review copying stored history into a new Session. Confirmation creates it; no model call or Run starts",None,if state.composing {Some(COMPOSING)}else{state.fork_reason}),
        Command::InspectRun => ("Inspect Run", "Inspect selected recorded Run".into(),Group::Navigation,"Read exact persisted Run status, counters and reported spend from Metrics. No Run control or model call",None,state.run_inspection_reason),
        Command::MarkComparisonLeft => (
            "Mark left", "Mark selected Session as comparison left".into(), Group::Session,
            "Choose the left Session for a read-only persisted history comparison. No history is read yet", None,
            (!state.can_mark_comparison).then_some("Choose a Session in the connected Sessions directory"),
        ),
        Command::CompareMarked => (
            "Compare", "Compare marked left with selected Session".into(), Group::Session,
            "Read stored lineage, whole-message differences and recorded usage. No model call or paid summary", None,
            state.comparison_reason,
        ),
        Command::SaveDesktopPreferences => (
            "Save desktop preferences", "Save desktop preferences".into(), Group::Appearance,
            "Save text size, light/dark appearance, docked panels and send keys. No Session data or drafts", None,
            (!state.can_preferences).then_some("Preferences are disabled or no path is available; use --preferences PATH"),
        ),
        Command::ReloadDesktopPreferences => (
            "Reload desktop preferences", "Reload desktop preferences".into(), Group::Appearance,
            "Reload saved view choices, replacing unsaved choices. Drafts stay intact", None,
            if !state.can_preferences { Some("Preferences are disabled or no path is available; use --preferences PATH") } else if state.composing { Some(COMPOSING) } else { None },
        ),
        Command::Stop => (
            "Stop",
            "Stop".to_string(),
            Group::Composer,
            "Record a request to stop the running Run",
            None,
            if !state.has_composer {
                Some(NO_COMPOSER)
            } else if !state.composer.stop_enabled {
                Some(NO_RUN)
            } else {
                None
            },
        ),
        Command::ResumeRun | Command::AbandonRun => (
            if command == Command::ResumeRun {
                "Resume Run"
            } else {
                "Abandon Run"
            },
            if command == Command::ResumeRun {
                "Resume paused or interrupted Run"
            } else {
                "Abandon paused or interrupted Run"
            }
            .to_string(),
            Group::Run,
            if command == Command::ResumeRun {
                "Continue from the persisted transcript; existing daemon confirmation requirements apply"
            } else {
                "Give up the paused or interrupted Run without sending the draft"
            },
            None,
            if !state.has_composer {
                Some(NO_COMPOSER)
            } else if !state.can_recover {
                Some(NO_RECOVERY)
            } else {
                None
            },
        ),
        Command::Approvals => (
            "Approvals", "Review Profile security approval".into(), Group::Run,
            "Read one complete pending Tool call; allow once or deny without persistent grants", None,
            if state.composing { Some(COMPOSING) } else { (!state.can_approvals).then_some("Wait for a ready Profile connection") },
        ),
        Command::Questions => (
            "Questions",
            "Review Profile questions".into(),
            Group::Run,
            "Answer pending questions for their original Sessions",
            None,
            (!state.can_questions).then_some("Connect to a Profile first"),
        ),
        Command::SessionDetails => (
            "Session details",
            "Edit Session name, tags, pin, model/Role or effort".into(),
            Group::Session,
            "Edit the chosen Sessions-list entry, or the open Session outside that list",
            None,
            if state.composing {
                Some(COMPOSING)
            } else {
                (!state.can_edit_session).then_some("Choose a Session while connected")
            },
        ),
        Command::SessionModel | Command::SessionPendingFiles => {
            let (label, title, hint) = if command == Command::SessionModel {
                (
                    "Model",
                    "Review Session model or Role",
                    "Open Chat's model and reasoning effort settings for the current Session",
                )
            } else {
                (
                    "Pending files",
                    "Review pending files",
                    "Open pending attachment and reference review for the current Session",
                )
            };
            (
                label,
                title.into(),
                Group::Composer,
                hint,
                None,
                if !state.has_composer {
                    Some(NO_COMPOSER)
                } else if state.composing {
                    Some(COMPOSING)
                } else {
                    (!state.can_edit_session).then_some("Choose a Session while connected")
                },
            )
        }
        Command::ToggleArchived => (
            "Archived",
            if state.archived_included {
                "Hide archived Sessions"
            } else {
                "Include archived Sessions"
            }
            .into(),
            Group::Session,
            "Include archived Sessions in the directory; archived entries open restore details",
            Some(state.archived_included),
            if state.composing {
                Some(COMPOSING)
            } else {
                (!state.can_browse_archived).then_some("Connect to a Profile first")
            },
        ),
        Command::ToggleSessions => (
            "Sessions",
            if state.sessions_shown {
                "Hide Sessions"
            } else {
                "Show Sessions"
            }
            .to_string(),
            Group::Panels,
            "Show or hide the Sessions sidebar",
            Some(state.sessions_shown),
            None,
        ),
        Command::ToggleContext => (
            "Context",
            if state.context_shown {
                "Hide Context"
            } else {
                "Show Context"
            }
            .to_string(),
            Group::Panels,
            "Show or hide the Context inspector",
            Some(state.context_shown),
            None,
        ),
        Command::ToggleTheme => (
            if state.dark { "Light" } else { "Dark" },
            if state.dark {
                "Use the light theme"
            } else {
                "Use the dark theme"
            }
            .to_string(),
            Group::Appearance,
            "Switch between the light and dark theme",
            None,
            None,
        ),
        Command::IncreaseTextSize => (
            "Larger text",
            "Larger text".to_string(),
            Group::Text,
            "Make all text one step larger",
            None,
            (state.text_scale == state.text_scale.larger()).then_some(LARGEST),
        ),
        Command::DecreaseTextSize => (
            "Smaller text",
            "Smaller text".to_string(),
            Group::Text,
            "Make all text one step smaller",
            None,
            (state.text_scale == state.text_scale.smaller()).then_some(SMALLEST),
        ),
        Command::ResetTextSize => (
            "Default text size",
            "Default text size".to_string(),
            Group::Text,
            "Put text back at its default size",
            None,
            (state.text_scale == TextScale::DEFAULT).then_some(DEFAULT_SIZE),
        ),
        Command::FocusNextRegion => (
            "Next region",
            "Focus the next region".to_string(),
            Group::Focus,
            "Move focus to the next part of the window",
            None,
            None,
        ),
        Command::FocusPreviousRegion => (
            "Previous region",
            "Focus the previous region".to_string(),
            Group::Focus,
            "Move focus to the previous part of the window",
            None,
            None,
        ),
        Command::MoveHistory(direction) => {
            let (label, hint) = match direction {
                HistoryMove::First => ("First message", "Home when the Conversation is focused"),
                HistoryMove::Older => ("Previous message", "Up when the Conversation is focused"),
                HistoryMove::Newer => ("Next message", "Down when the Conversation is focused"),
                HistoryMove::Latest => (
                    "Jump to latest message",
                    "End when the Conversation is focused and resumes following",
                ),
            };
            (
                label,
                label.into(),
                Group::Navigation,
                hint,
                None,
                if state.destination != Destination::Chat {
                    Some("Open Chat first")
                } else if state.composing {
                    Some(COMPOSING)
                } else {
                    (!state.has_history).then_some("No loaded messages")
                },
            )
        }
        Command::GoTo(destination) => (
            destination.label(),
            format!("Go to {}", destination.label()),
            Group::Navigation,
            if destination == Destination::Chat {
                "Open the conversation"
            } else if destination == Destination::Sessions {
                "Browse and filter fixture Sessions (archived Sessions excluded)"
            } else if destination == Destination::Search {
                "Find literal, case-sensitive text in the loaded transcript only"
            } else if destination == Destination::ProfileSearch {
                "Search persisted messages across this connected Profile, including archived Sessions"
            } else {
                "Scaffold: this destination has no view yet"
            },
            Some(state.destination == destination),
            (destination == Destination::ProfileSearch && !state.can_search_profile)
                .then_some("Connect to a Profile first"),
        ),
        Command::ShowCommands => (
            "Commands",
            "Show all commands".to_string(),
            Group::Navigation,
            "Search for a command and run it",
            Some(state.palette_open),
            None,
        ),
    };
    Entry {
        command,
        label,
        title,
        group,
        hint,
        checked,
        disabled_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RunState;

    fn state(run: RunState, draft_is_empty: bool) -> State {
        State {
            composer: ComposerActions::for_state(run, draft_is_empty),
            has_composer: true,
            has_history: true,
            can_recover: false,
            can_questions: false,
            can_approvals: false,
            can_create_session: false,
            usage_reason: Some("Choose a connected Session in Chat first"),
            step_mode_reason: Some("Step mode is not available for this Session"),
            can_search_profile: false,
            can_edit_session: false,
            can_browse_archived: false,
            archived_included: false,
            composing: false,
            enter_sends: true,
            can_preferences: false,
            can_mark_comparison: false,
            comparison_reason: Some("Mark a left Session first"),
            run_inspection_reason: Some("Select a Run entry in Metrics first"),
            fork_reason: Some("Choose a connected Session first"),
            invocations_reason: Some("Connect to a Profile first"),
            invocation_open_reason: Some("Select an Invocation record first"),
            sessions_shown: true,
            context_shown: false,
            dark: false,
            text_scale: TextScale::DEFAULT,
            destination: Destination::Chat,
            palette_open: false,
        }
    }

    #[test]
    fn preference_commands_require_a_path_and_reload_respects_preedit() {
        let mut s = state(RunState::Idle, true);
        for command in [
            Command::SaveDesktopPreferences,
            Command::ReloadDesktopPreferences,
        ] {
            assert!(!entry(command, &s).enabled());
            assert!(keys(command).is_empty());
        }
        s.can_preferences = true;
        s.has_composer = false;
        assert!(entry(Command::SaveDesktopPreferences, &s).enabled());
        assert!(entry(Command::ReloadDesktopPreferences, &s).enabled());
        s.composing = true;
        assert!(entry(Command::SaveDesktopPreferences, &s).enabled());
        assert_eq!(
            entry(Command::ReloadDesktopPreferences, &s).disabled_reason,
            Some(COMPOSING)
        );
    }

    #[test]
    fn send_key_choice_is_explicit_and_cannot_change_during_composition_or_outside_chat() {
        let mut s = state(RunState::Idle, true);
        let command = Command::ToggleEnterSends;
        assert_eq!(entry(command, &s).title, "Use Enter for newlines");
        assert_eq!(entry(command, &s).checked, Some(true));
        s.enter_sends = false;
        assert_eq!(entry(command, &s).title, "Use Enter to send");
        assert_eq!(entry(command, &s).checked, Some(false));
        s.composing = true;
        assert_eq!(entry(command, &s).disabled_reason, Some(COMPOSING));
        s.has_composer = false;
        assert_eq!(entry(command, &s).disabled_reason, Some(NO_COMPOSER));
        assert!(keys(command).is_empty());
        assert!(palette(&s).iter().any(|e| e.command == command));
    }

    #[test]
    fn recovery_commands_require_confirmed_recovery_and_never_send_the_draft() {
        let mut state = state(RunState::Paused, true);
        for command in [Command::ResumeRun, Command::AbandonRun] {
            assert_eq!(entry(command, &state).disabled_reason, Some(NO_RECOVERY));
        }
        state.can_recover = true;
        for command in [Command::ResumeRun, Command::AbandonRun] {
            assert!(entry(command, &state).enabled());
            assert_eq!(entry(command, &state).group, Group::Run);
            assert!(keys(command).is_empty());
        }
        state.has_composer = false;
        assert_eq!(
            entry(Command::ResumeRun, &state).disabled_reason,
            Some(NO_COMPOSER)
        );
    }

    #[test]
    fn composer_commands_follow_the_intent_semantics() {
        let idle = state(RunState::Idle, false);
        let send = entry(Command::Send, &idle);
        assert_eq!(send.label, "Send");
        assert!(send.enabled());
        assert_eq!(entry(Command::Stop, &idle).disabled_reason, Some(NO_RUN));
        assert_eq!(
            entry(Command::SendSecondary, &idle).disabled_reason,
            Some(NO_SECONDARY)
        );

        let running = state(RunState::Running, false);
        assert_eq!(entry(Command::Send, &running).label, "Steer");
        assert_eq!(entry(Command::SendSecondary, &running).label, "Queue");
        assert!(entry(Command::SendSecondary, &running).enabled());
        assert!(entry(Command::Stop, &running).enabled());

        // The composer's own reasons come first, unchanged.
        let empty = state(RunState::Running, true);
        assert_eq!(
            entry(Command::Send, &empty).disabled_reason,
            Some("Write a message first")
        );
        assert!(entry(Command::Stop, &empty).enabled());
        let offline = state(RunState::Unknown, false);
        assert_eq!(
            entry(Command::Send, &offline).disabled_reason,
            Some("Not connected to the Daemon Core")
        );
        assert_eq!(entry(Command::Stop, &offline).disabled_reason, Some(NO_RUN));
    }

    #[test]
    fn nothing_is_sent_while_text_is_being_composed_or_without_a_composer() {
        let mut composing = state(RunState::Running, false);
        composing.composing = true;
        for command in [Command::Send, Command::SendSecondary] {
            assert_eq!(entry(command, &composing).disabled_reason, Some(COMPOSING));
        }
        // Stop takes no text, so a composition does not hold it back.
        assert!(entry(Command::Stop, &composing).enabled());

        let mut elsewhere = state(RunState::Running, false);
        elsewhere.has_composer = false;
        elsewhere.destination = Destination::Tools;
        for command in [Command::Send, Command::SendSecondary, Command::Stop] {
            assert_eq!(
                entry(command, &elsewhere).disabled_reason,
                Some(NO_COMPOSER)
            );
        }
        assert!(!palette(&elsewhere)
            .iter()
            .any(|entry| entry.command == Command::SendSecondary));
    }

    #[test]
    fn text_size_commands_are_unavailable_at_their_limits() {
        let mut s = state(RunState::Idle, true);
        assert_eq!(
            entry(Command::DecreaseTextSize, &s).disabled_reason,
            Some(SMALLEST)
        );
        assert_eq!(
            entry(Command::ResetTextSize, &s).disabled_reason,
            Some(DEFAULT_SIZE)
        );
        assert!(entry(Command::IncreaseTextSize, &s).enabled());
        s.text_scale = TextScale::LARGEST;
        assert_eq!(
            entry(Command::IncreaseTextSize, &s).disabled_reason,
            Some(LARGEST)
        );
        assert!(entry(Command::DecreaseTextSize, &s).enabled());
        assert!(entry(Command::ResetTextSize, &s).enabled());
    }

    #[test]
    fn toggles_say_what_they_would_do() {
        let mut s = state(RunState::Idle, true);
        let sessions = entry(Command::ToggleSessions, &s);
        assert_eq!(
            (sessions.label, sessions.title.as_str()),
            ("Sessions", "Hide Sessions")
        );
        assert_eq!(sessions.checked, Some(true));
        assert_eq!(entry(Command::ToggleContext, &s).title, "Show Context");
        assert_eq!(entry(Command::ToggleTheme, &s).label, "Dark");
        s.dark = true;
        assert_eq!(entry(Command::ToggleTheme, &s).label, "Light");
        let chat = entry(Command::GoTo(Destination::Chat), &s);
        assert_eq!(
            (chat.title.as_str(), chat.checked),
            ("Go to Chat", Some(true))
        );
        assert_eq!(
            entry(Command::GoTo(Destination::Tools), &s).hint,
            "Scaffold: this destination has no view yet"
        );
        assert_eq!(entry(Command::ShowCommands, &s).checked, Some(false));
        s.palette_open = true;
        assert_eq!(entry(Command::ShowCommands, &s).checked, Some(true));
        assert!(entry(Command::ShowCommands, &s).shortcut().is_some());
    }

    #[test]
    fn the_palette_lists_every_command_but_itself_once() {
        let running = state(RunState::Running, false);
        let listed: Vec<Command> = palette(&running).iter().map(|e| e.command).collect();
        assert!(!listed.contains(&Command::ShowCommands));
        let unique: std::collections::HashSet<_> = listed.iter().collect();
        assert_eq!(unique.len(), listed.len());
        for command in GLOBAL.into_iter().filter(|c| *c != Command::ShowCommands) {
            assert!(listed.contains(&command), "{command:?}");
        }
        for destination in Destination::NAVIGATION {
            assert!(listed.contains(&Command::GoTo(destination)));
        }
        assert!(listed.contains(&Command::SendSecondary));
        // Idle: no second composer action to list.
        assert!(!palette(&state(RunState::Idle, false))
            .iter()
            .any(|e| e.command == Command::SendSecondary));
    }

    #[test]
    fn search_matches_every_word_in_any_case() {
        let s = state(RunState::Idle, true);
        let titles = |query: &str| -> Vec<String> {
            palette(&s)
                .into_iter()
                .filter(|e| e.matches(query))
                .map(|e| e.title)
                .collect()
        };
        assert_eq!(titles("").len(), palette(&s).len());
        assert_eq!(titles("go tools"), ["Go to Tools"]);
        assert_eq!(titles("HIDE sess"), ["Hide Sessions"]);
        // The group and the hint are searched too.
        assert!(titles("text size").contains(&"Larger text".to_string()));
        assert!(titles("theme").contains(&"Use the dark theme".to_string()));
        assert!(titles("nothing like this").is_empty());
        // Non-ASCII input is fine and simply matches nothing here.
        assert!(titles("ส่ง 送信").is_empty());
    }

    #[test]
    fn shortcuts_are_unique_and_never_plain_typing() {
        let mut seen = std::collections::HashSet::new();
        for command in GLOBAL {
            assert!(!keys(command).is_empty(), "{command:?}");
            for key in keys(command) {
                assert!(seen.insert(*key), "{key} bound twice");
                let modified = ["cmd-", "ctrl-", "alt-"].iter().any(|m| key.contains(m));
                let function_key = key.trim_start_matches("shift-").starts_with('f');
                assert!(modified || function_key, "{key} could be typed text");
            }
        }
        for command in [
            Command::Send,
            Command::SendSecondary,
            Command::Stop,
            Command::ToggleTheme,
        ] {
            assert!(keys(command).is_empty());
        }
    }

    #[test]
    fn shortcut_text_reads_as_people_write_it() {
        if cfg!(target_os = "macos") {
            assert_eq!(shortcut_text("cmd-shift-p"), "⌘⇧P");
            assert_eq!(shortcut_text("cmd--"), "⌘-");
            assert_eq!(shortcut_text("cmd-alt-b"), "⌘⌥B");
        } else {
            assert_eq!(shortcut_text("ctrl-shift-p"), "Ctrl+Shift+P");
            assert_eq!(shortcut_text("ctrl--"), "Ctrl+-");
        }
        assert!(shortcut_text("shift-f6").ends_with("F6"));
        assert_eq!(shortcut_text("f6"), "F6");
        let send = entry(Command::Send, &state(RunState::Idle, false));
        assert_eq!(send.shortcut(), None);
        let sessions = entry(Command::ToggleSessions, &state(RunState::Idle, true));
        assert_eq!(
            sessions.shortcut(),
            Some(shortcut_text(keys(Command::ToggleSessions)[0]))
        );
    }
    #[test]
    fn approval_review_is_profile_scoped_ready_only_and_composition_safe() {
        let mut s = state(crate::model::RunState::Idle, true);
        s.can_approvals = false;
        assert!(!entry(Command::Approvals, &s).enabled());
        s.can_approvals = true;
        s.has_composer = false;
        assert!(entry(Command::Approvals, &s).enabled());
        assert!(palette(&s).iter().any(|e| e.command == Command::Approvals));
        assert!(keys(Command::Approvals).is_empty());
        s.composing = true;
        assert_eq!(
            entry(Command::Approvals, &s).disabled_reason,
            Some(COMPOSING)
        );
    }
}
