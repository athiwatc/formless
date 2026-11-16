//! The Phase 0 shell: one window laid out as sidebar, conversation with
//! composer, context inspector and status line, built from GPUI Kit controls.
//!
//! With a client attached it consumes serialized canonical daemon updates and
//! emits typed intents. Without one, the preserved synthetic fixture records
//! intents without sending them and shows scaffold notices for resource pages.

use std::cell::{Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use formless_protocol::{Event, SessionInfo};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState, Undo};
use gpui_kit::component::label::Label;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    h_flex, v_flex, v_virtual_list, ActiveTheme as _, Disableable as _, FocusTrapElement as _,
    Selectable as _, Sizable as _, Theme, ThemeMode, VirtualListScrollHandle,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, point, px, size, AnyElement, App, Bounds, ClipboardItem, Context, DispatchPhase,
    ElementId, Entity, EntityInputHandler as _, FocusHandle, Focusable as _, HitboxBehavior, Hsla,
    KeyBinding, Pixels, Role, ScrollEvent, ScrollHandle, ScrollStrategy, SharedString, Size,
    Subscription, TestSupportExt as _, Window,
};

use crate::client::{self, Client, Command as ClientCommand, Update as ClientUpdate};
use crate::commands::{self, Command};
use crate::directory::{self, Directory};
use crate::forecast;
mod approval_review;
mod comparison;
mod invocations;
mod preferences;
mod profile_search;
mod questions;
mod reader;
mod reading_positions;
mod run_inspection;
mod session_fork;
mod sessions;
mod step_mode;
mod step_review;
mod usage;
use crate::full_view;
use crate::memory::{
    edit_span, format_bytes, limits, paste_can_never_fit, snap_to_graphemes, within_composer_limit,
    IntentKind, IntentRecord, Rejections, UndoBudget,
};
use crate::model::{
    send_mode_label, tokens, BlockKind, ComposerActions, Destination, Drafts, Fit, InspectorMode,
    Intent, Layout, PanelPrefs, Region, RunState, SessionGroup, Sidebar, SidebarMode, TextScale,
};
use crate::replies::SessionSnapshot;
use crate::search::{self, Hit as SearchHit, Search as TranscriptSearch};
use crate::timeline::{
    Anchor, Coalescer, StreamWork, Timeline, TimelineBlock, WindowMove, FLUSH_INTERVAL_MS,
};
use crate::transcript::{Change as TranscriptChange, ReloadReason, Transcript, TranscriptState};

gpui_kit::actions!(
    formless_desktop,
    [
        FocusNextRegion,
        FocusPreviousRegion,
        ToggleSidebar,
        ToggleInspector,
        DismissOverlay,
        IncreaseTextSize,
        DecreaseTextSize,
        ResetTextSize,
        SelectNextSession,
        SelectPreviousSession,
        ActivateSession,
        ScrollInspectorUp,
        ScrollInspectorDown,
        ScrollInspectorPageUp,
        ScrollInspectorPageDown,
        ShowCommands,
        FindLoadedText,
        ComposerModifiedEnter,
        SelectPreviousCommand,
        SelectNextCommand,
        RunSelectedCommand,
        ClosePalette,
        CloseForecast,
        CloseQuestions,
        CloseStepReview,
        CloseApprovalReview,
        CloseSessionEditor,
        ScrollViewerUp,
        ScrollViewerDown,
        ScrollViewerPageUp,
        ScrollViewerPageDown,
        ScrollViewerStart,
        ScrollViewerEnd,
        DirectoryNext,
        DirectoryPrevious,
        DirectoryOpen,
        DirectoryBack,
        SearchNext,
        SearchPrevious,
        SearchOpen,
        SearchBack,
        ProfileSearchNext,
        ProfileSearchPrevious,
        ProfileSearchOpen,
        ProfileSearchBack,
        CloseViewer,
        ResourceNext,
        ResourcePrevious,
    ]
);

const SESSION_LIST_CONTEXT: &str = "FormlessSessionList";
const OVERLAY_CONTEXT: &str = "FormlessOverlay";
const INSPECTOR_CONTEXT: &str = "FormlessInspector";
const PALETTE_CONTEXT: &str = "FormlessPalette";
const FORECAST_CONTEXT: &str = "FormlessForecast";
const QUESTIONS_CONTEXT: &str = "FormlessQuestions";
const SESSION_EDITOR_CONTEXT: &str = "FormlessSessionEditor";
const QUESTIONS_READ_CONTEXT: &str = "FormlessQuestions && !Input";
const VIEWER_CONTEXT: &str = "FormlessViewer";
const RESOURCE_CONTEXT: &str = "FormlessResource";
const VIEWER_DESCRIPTION: &str =
    "Modal. The whole block as plain text. Tab reaches the summary and controls. \
     Arrow, Page Up, Page Down, Home and End scroll the focused text; \
     Escape closes and returns to where you were.";
/// Temporary font-layout retention while indexing a whole block: never
/// shape its rows through the window's per-frame cache.
const VIEWER_MEASURE_BATCH: usize = 64;
const VIEWER_WIDTH_CACHE: usize = 128;
/// The palette's search field. These bindings are more specific than the
/// text input's own, so Up, Down, Enter and Escape there move through and
/// run the list instead of moving a caret in a one-line field.
const SEARCH_CONTEXT: &str = "FormlessSearch";
const PROFILE_SEARCH_CONTEXT: &str = "FormlessProfileSearch";
const PROFILE_SEARCH_INPUT_CONTEXT: &str = "FormlessProfileSearch > Input";
const SEARCH_INPUT_CONTEXT: &str = "FormlessSearch > Input";
const DIRECTORY_CONTEXT: &str = "FormlessDirectory";
const DIRECTORY_INPUT_CONTEXT: &str = "FormlessDirectory > Input";
const PALETTE_INPUT_CONTEXT: &str = "FormlessPalette > Input";
const PALETTE_DESCRIPTION: &str = "Modal. Type to search, Up and Down to choose, Enter to run, \
     Escape to close and return to where you were.";
/// Read out with a modal panel's name. The kit has no "modal" accessibility
/// property to set, so the behaviour is also stated in words.
const MODAL_DESCRIPTION: &str =
    "Modal panel. Press Escape to close it and return to where you were.";
const SESSION_ROW_HEIGHT: f32 = 44.0;
const HEADER_HEIGHT: f32 = 44.0;
const STATUS_HEIGHT: f32 = 28.0;
/// How far an arrow key scrolls the inspector, at 100% text size.
const INSPECTOR_LINE: f32 = 40.0;
/// The kit theme's default font sizes, which 100% text size maps to.
const BASE_FONT_SIZE: f32 = 16.0;
const BASE_MONO_FONT_SIZE: f32 = 13.0;
/// The global shortcuts come from [`commands::keys`], so a shortcut, the
/// button that shows it and the command palette never disagree. None of
/// them is a bare printable key, so typing in the composer is untouched.
/// The rest are keys that only mean something inside one part of the window.
pub fn bind_keys(cx: &mut App) {
    let global = commands::GLOBAL.into_iter().flat_map(|command| {
        commands::keys(command)
            .iter()
            .map(move |key| match command {
                Command::ToggleSessions => KeyBinding::new(key, ToggleSidebar, None),
                Command::ToggleContext => KeyBinding::new(key, ToggleInspector, None),
                Command::IncreaseTextSize => KeyBinding::new(key, IncreaseTextSize, None),
                Command::DecreaseTextSize => KeyBinding::new(key, DecreaseTextSize, None),
                Command::ResetTextSize => KeyBinding::new(key, ResetTextSize, None),
                Command::FocusNextRegion => KeyBinding::new(key, FocusNextRegion, None),
                Command::FocusPreviousRegion => KeyBinding::new(key, FocusPreviousRegion, None),
                Command::ShowCommands => KeyBinding::new(key, ShowCommands, None),
                Command::GoTo(Destination::Search) => KeyBinding::new(key, FindLoadedText, None),
                Command::Send
                | Command::SendSecondary
                | Command::Stop
                | Command::ResumeRun
                | Command::AbandonRun
                | Command::Questions
                | Command::Approvals
                | Command::SessionDetails
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
                | Command::GoTo(_) => unreachable!("{command:?} has no global shortcut"),
            })
    });
    cx.bind_keys(global.collect::<Vec<_>>());
    cx.bind_keys([
        KeyBinding::new(
            "escape",
            CloseApprovalReview,
            Some("FormlessApprovalReview"),
        ),
        KeyBinding::new("up", ScrollViewerUp, Some("FormlessApprovalReview")),
        KeyBinding::new("down", ScrollViewerDown, Some("FormlessApprovalReview")),
        KeyBinding::new("pageup", ScrollViewerPageUp, Some("FormlessApprovalReview")),
        KeyBinding::new(
            "pagedown",
            ScrollViewerPageDown,
            Some("FormlessApprovalReview"),
        ),
        KeyBinding::new("home", ScrollViewerStart, Some("FormlessApprovalReview")),
        KeyBinding::new("end", ScrollViewerEnd, Some("FormlessApprovalReview")),
        KeyBinding::new(
            commands::composer_submit_key(),
            ComposerModifiedEnter,
            Some("FormlessComposer > Input"),
        ),
        KeyBinding::new("escape", CloseForecast, Some(FORECAST_CONTEXT)),
        KeyBinding::new("escape", CloseQuestions, Some(QUESTIONS_CONTEXT)),
        KeyBinding::new("escape", CloseStepReview, Some("FormlessStepReview")),
        KeyBinding::new(
            "escape",
            CloseStepReview,
            Some("FormlessStepReview > Input"),
        ),
        KeyBinding::new("up", ScrollViewerUp, Some("FormlessStepReview && !Input")),
        KeyBinding::new(
            "down",
            ScrollViewerDown,
            Some("FormlessStepReview && !Input"),
        ),
        KeyBinding::new(
            "pageup",
            ScrollViewerPageUp,
            Some("FormlessStepReview && !Input"),
        ),
        KeyBinding::new(
            "pagedown",
            ScrollViewerPageDown,
            Some("FormlessStepReview && !Input"),
        ),
        KeyBinding::new(
            "home",
            ScrollViewerStart,
            Some("FormlessStepReview && !Input"),
        ),
        KeyBinding::new("end", ScrollViewerEnd, Some("FormlessStepReview && !Input")),
        KeyBinding::new("escape", CloseSessionEditor, Some(SESSION_EDITOR_CONTEXT)),
        // Input's Escape unmarks and then propagates. Intercept it before
        // that happens so one key cannot both finish preedit and cancel.
        KeyBinding::new(
            "escape",
            CloseSessionEditor,
            Some("FormlessSessionEditor > Input"),
        ),
        KeyBinding::new(
            "up",
            ScrollViewerUp,
            Some("FormlessSessionEditor && !Input"),
        ),
        KeyBinding::new(
            "down",
            ScrollViewerDown,
            Some("FormlessSessionEditor && !Input"),
        ),
        KeyBinding::new(
            "pageup",
            ScrollViewerPageUp,
            Some("FormlessSessionEditor && !Input"),
        ),
        KeyBinding::new(
            "pagedown",
            ScrollViewerPageDown,
            Some("FormlessSessionEditor && !Input"),
        ),
        KeyBinding::new(
            "home",
            ScrollViewerStart,
            Some("FormlessSessionEditor && !Input"),
        ),
        KeyBinding::new(
            "end",
            ScrollViewerEnd,
            Some("FormlessSessionEditor && !Input"),
        ),
        KeyBinding::new("up", ScrollViewerUp, Some(QUESTIONS_READ_CONTEXT)),
        KeyBinding::new("down", ScrollViewerDown, Some(QUESTIONS_READ_CONTEXT)),
        KeyBinding::new("pageup", ScrollViewerPageUp, Some(QUESTIONS_READ_CONTEXT)),
        KeyBinding::new(
            "pagedown",
            ScrollViewerPageDown,
            Some(QUESTIONS_READ_CONTEXT),
        ),
        KeyBinding::new("home", ScrollViewerStart, Some(QUESTIONS_READ_CONTEXT)),
        KeyBinding::new("end", ScrollViewerEnd, Some(QUESTIONS_READ_CONTEXT)),
        KeyBinding::new("up", ScrollViewerUp, Some(FORECAST_CONTEXT)),
        KeyBinding::new("down", ScrollViewerDown, Some(FORECAST_CONTEXT)),
        KeyBinding::new("pageup", ScrollViewerPageUp, Some(FORECAST_CONTEXT)),
        KeyBinding::new("pagedown", ScrollViewerPageDown, Some(FORECAST_CONTEXT)),
        KeyBinding::new("home", ScrollViewerStart, Some(FORECAST_CONTEXT)),
        KeyBinding::new("end", ScrollViewerEnd, Some(FORECAST_CONTEXT)),
        KeyBinding::new("down", SearchNext, Some(SEARCH_CONTEXT)),
        KeyBinding::new("up", SearchPrevious, Some(SEARCH_CONTEXT)),
        KeyBinding::new("enter", SearchOpen, Some(SEARCH_CONTEXT)),
        KeyBinding::new("escape", SearchBack, Some(SEARCH_CONTEXT)),
        KeyBinding::new("down", ProfileSearchNext, Some(PROFILE_SEARCH_CONTEXT)),
        KeyBinding::new("up", ProfileSearchPrevious, Some(PROFILE_SEARCH_CONTEXT)),
        KeyBinding::new("enter", ProfileSearchOpen, Some(PROFILE_SEARCH_CONTEXT)),
        KeyBinding::new("escape", ProfileSearchBack, Some(PROFILE_SEARCH_CONTEXT)),
        KeyBinding::new(
            "down",
            ProfileSearchNext,
            Some(PROFILE_SEARCH_INPUT_CONTEXT),
        ),
        KeyBinding::new(
            "up",
            ProfileSearchPrevious,
            Some(PROFILE_SEARCH_INPUT_CONTEXT),
        ),
        KeyBinding::new(
            "enter",
            ProfileSearchOpen,
            Some(PROFILE_SEARCH_INPUT_CONTEXT),
        ),
        KeyBinding::new(
            "escape",
            ProfileSearchBack,
            Some(PROFILE_SEARCH_INPUT_CONTEXT),
        ),
        KeyBinding::new("down", SearchNext, Some(SEARCH_INPUT_CONTEXT)),
        KeyBinding::new("up", SearchPrevious, Some(SEARCH_INPUT_CONTEXT)),
        KeyBinding::new("enter", SearchOpen, Some(SEARCH_INPUT_CONTEXT)),
        KeyBinding::new("escape", SearchBack, Some(SEARCH_INPUT_CONTEXT)),
        KeyBinding::new("down", DirectoryNext, Some(DIRECTORY_CONTEXT)),
        KeyBinding::new("up", DirectoryPrevious, Some(DIRECTORY_CONTEXT)),
        KeyBinding::new("enter", DirectoryOpen, Some(DIRECTORY_CONTEXT)),
        KeyBinding::new("escape", DirectoryBack, Some(DIRECTORY_CONTEXT)),
        KeyBinding::new("down", DirectoryNext, Some(DIRECTORY_INPUT_CONTEXT)),
        KeyBinding::new("up", DirectoryPrevious, Some(DIRECTORY_INPUT_CONTEXT)),
        KeyBinding::new("enter", DirectoryOpen, Some(DIRECTORY_INPUT_CONTEXT)),
        KeyBinding::new("escape", DirectoryBack, Some(DIRECTORY_INPUT_CONTEXT)),
        KeyBinding::new("escape", DismissOverlay, Some(OVERLAY_CONTEXT)),
        KeyBinding::new("down", SelectNextSession, Some(SESSION_LIST_CONTEXT)),
        KeyBinding::new("up", SelectPreviousSession, Some(SESSION_LIST_CONTEXT)),
        KeyBinding::new("enter", ActivateSession, Some(SESSION_LIST_CONTEXT)),
        // The inspector holds text only, so with focus in it the arrow and
        // page keys scroll it: in a short window it is taller than its panel.
        KeyBinding::new("up", ScrollViewerUp, Some("FormlessForkReview")),
        KeyBinding::new("down", ScrollViewerDown, Some("FormlessForkReview")),
        KeyBinding::new("pageup", ScrollViewerPageUp, Some("FormlessForkReview")),
        KeyBinding::new("pagedown", ScrollViewerPageDown, Some("FormlessForkReview")),
        KeyBinding::new("home", ScrollViewerStart, Some("FormlessForkReview")),
        KeyBinding::new("end", ScrollViewerEnd, Some("FormlessForkReview")),
        KeyBinding::new("up", ScrollInspectorUp, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("down", ScrollInspectorDown, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("pageup", ScrollInspectorPageUp, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("pagedown", ScrollInspectorPageDown, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("up", SelectPreviousCommand, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("down", SelectNextCommand, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("enter", RunSelectedCommand, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("escape", ClosePalette, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("escape", ClosePalette, Some(PALETTE_CONTEXT)),
        KeyBinding::new("up", ScrollViewerUp, Some("FormlessStatus")),
        KeyBinding::new("down", ScrollViewerDown, Some("FormlessStatus")),
        KeyBinding::new("pageup", ScrollViewerPageUp, Some("FormlessStatus")),
        KeyBinding::new("pagedown", ScrollViewerPageDown, Some("FormlessStatus")),
        KeyBinding::new("home", ScrollViewerStart, Some("FormlessStatus")),
        KeyBinding::new("end", ScrollViewerEnd, Some("FormlessStatus")),
        KeyBinding::new("up", ScrollViewerUp, Some(VIEWER_CONTEXT)),
        KeyBinding::new("down", ScrollViewerDown, Some(VIEWER_CONTEXT)),
        KeyBinding::new("pageup", ScrollViewerPageUp, Some(VIEWER_CONTEXT)),
        KeyBinding::new("pagedown", ScrollViewerPageDown, Some(VIEWER_CONTEXT)),
        KeyBinding::new("home", ScrollViewerStart, Some(VIEWER_CONTEXT)),
        KeyBinding::new("end", ScrollViewerEnd, Some(VIEWER_CONTEXT)),
        KeyBinding::new("escape", CloseViewer, Some(VIEWER_CONTEXT)),
        KeyBinding::new("down", ResourceNext, Some(RESOURCE_CONTEXT)),
        KeyBinding::new("up", ResourcePrevious, Some(RESOURCE_CONTEXT)),
    ]);
}

pub fn search_row_id(hit: SearchHit) -> ElementId {
    SharedString::from(format!(
        "search-row-{}-{}-{}",
        hit.block_id, hit.start, hit.end
    ))
    .into()
}

pub fn directory_row_id(session_id: &str) -> ElementId {
    SharedString::from(format!("directory-row-{session_id}")).into()
}

/// Element id of a Session's sidebar row. It is derived from the Session id,
/// not the row's position, so the element keeps its identity (and with it
/// hover, focus and scroll state) when the list reorders.
pub fn session_row_id(session_id: &str) -> ElementId {
    ElementId::from(SharedString::from(format!("session-row-{session_id}")))
}

const BLOCK_COPY_MAX_BYTES: usize = 1024 * 1024;
const VIEWER_STREAMING: &str = "Available once the block has finished arriving";

/// For a row that shows less than its whole block: what is missing on each
/// side, and controls to move the window. `None` when the row shows it all.
fn block_window_line(
    block: &TimelineBlock,
    streaming: bool,
    position: &'static str,
    shell: &Entity<Shell>,
    cx: &App,
) -> Option<AnyElement> {
    if !block.is_truncated() {
        return None;
    }
    let mut text = format!(
        "Showing {} of {}.",
        format_bytes(block.window().len()),
        format_bytes(block.total_bytes())
    );
    if block.hidden_before() > 0 {
        text.push_str(&format!(
            " {} before it is not shown.",
            format_bytes(block.hidden_before())
        ));
    }
    if block.hidden_after() > 0 {
        text.push_str(&format!(
            " {} after it is not shown{}.",
            format_bytes(block.hidden_after()),
            if streaming {
                ", and more is arriving"
            } else {
                ""
            }
        ));
    }
    let id = block.id;
    let button = |action: &'static str, to: WindowMove, enabled: bool| {
        let shell = shell.clone();
        Button::new(SharedString::from(format!(
            "block-{id}-{position}-{}",
            action.to_lowercase()
        )))
        .xsmall()
        .ghost()
        .label(action)
        .disabled(!enabled)
        .on_click(move |_, _, cx| shell.update(cx, |this, cx| this.move_block_window(id, to, cx)))
    };
    let has_earlier = block.hidden_before() > 0;
    let has_later = block.hidden_after() > 0;
    // The whole block, in its own view; not while it is still arriving.
    let view = {
        let shell = shell.clone();
        Button::new(SharedString::from(format!("block-{id}-{position}-view")))
            .xsmall()
            .ghost()
            .label("View whole")
            .disabled(streaming)
            .tooltip(if streaming {
                VIEWER_STREAMING
            } else {
                "Show the whole block as plain text"
            })
            .on_click(move |_, window, cx| {
                shell.update(cx, |this, cx| this.view_block(id, window, cx).ok());
            })
    };
    // "End" also resumes following a block that is still being streamed.
    let can_follow = streaming && block.anchor() != Anchor::Tail;
    Some(
        h_flex()
            .id(SharedString::from(format!("block-{id}-{position}-window")))
            .test_support()
            // What is missing is the name of the group its controls are in.
            .role(Role::Group)
            .aria_label(text.clone())
            .flex_wrap()
            .gap_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(Label::new(text))
            .child(button("Start", WindowMove::Start, has_earlier))
            .child(button("Earlier", WindowMove::Earlier, has_earlier))
            .child(button("Later", WindowMove::Later, has_later))
            .child(button("End", WindowMove::End, has_later || can_follow))
            .child(view)
            .into_any_element(),
    )
}

/// A line of text that assistive technology can read.
///
/// The pinned GPUI gives an element an accessibility node only if it has
/// both an id and a role, and a plain string, the kit's `Label` and its
/// markdown view have neither. Read from a real window, the platform tree
/// held the buttons and the composer and none of the text. So the element
/// that holds the text is made a static-text node whose value is the text.
fn readable(id: impl Into<ElementId>, text: impl Into<SharedString>) -> impl IntoElement + Styled {
    let text = text.into();
    div()
        .id(id)
        .test_support()
        .role(Role::Label)
        .aria_value(text.clone())
        .child(text)
}

/// Mark an element's accessibility node as a polite live region: when its
/// value changes, or it appears, the platform is asked to announce the new
/// value without interrupting. The pinned GPUI has no setter for this, so it
/// is set on the node itself after GPUI has built it.
fn announced<E: StatefulInteractiveElement>(element: E) -> E {
    element.a11y_synthetic_children(|builder| {
        builder
            .parent_node()
            .set_live(gpui_kit::accesskit::Live::Polite)
    })
}

/// Mark a dialog's accessibility node as modal, so assistive technology
/// that honours it keeps to the dialog. The pinned GPUI has no setter for
/// this either.
fn modal_dialog<E: StatefulInteractiveElement>(element: E) -> E {
    element.a11y_synthetic_children(|builder| builder.parent_node().set_modal())
}

/// Take an element, and everything inside it, out of the accessibility tree
/// while `inert` holds: what is behind a modal dialog. AccessKit's hidden
/// flag is inherited by every descendant, and its shared filter leaves a
/// hidden subtree out of what every platform adapter exposes, unless the
/// node holds focus. The pinned GPUI has no setter for it either. Layout,
/// painting, pointer and keyboard handling are not affected: only what
/// assistive technology is given.
fn inert_while<E: StatefulInteractiveElement>(element: E, inert: bool) -> E {
    if inert {
        element.a11y_synthetic_children(|builder| builder.parent_node().set_hidden())
    } else {
        element
    }
}

/// A region's border colour: the focus ring while the region itself holds
/// focus, its ordinary border otherwise.
fn region_border(focused: bool, idle: Hsla, cx: &App) -> Hsla {
    if focused {
        cx.theme().ring
    } else {
        idle
    }
}

/// A command's tooltip: why it cannot run, or what it does and its shortcut.
fn tooltip(entry: &commands::Entry) -> String {
    match (entry.disabled_reason, entry.shortcut()) {
        (Some(reason), _) => reason.to_string(),
        (None, Some(shortcut)) => format!("{} ({shortcut})", entry.hint),
        (None, None) => entry.hint.to_string(),
    }
}

/// What holds the window modally: a panel shown as an overlay, or the
/// command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    Panel(Region),
    Palette,
    /// The full-output view of one block.
    Viewer,
    Forecast,
    Questions,
    Step,
    Approvals,
    SessionEditor,
}

struct ForecastDialog {
    prepared: Box<forecast::Prepared>,
    summary: Vec<String>,
    details: String,
    waiting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptSource {
    Fixture,
    Snapshot,
    NotLoaded,
}

/// The open full-output view: one block, as display rows.
struct Viewer {
    block_id: u64,
    /// Where each width-checked row starts in the block.
    starts: Box<[u32]>,
    layout: ViewerLayout,
    error: Option<full_view::RowError>,
    summary_scroll: ScrollHandle,
    summary_focus: FocusHandle,
    scroll: gpui_kit::UniformListScrollHandle,
    /// Set once the block has been copied, to say so.
    copied: bool,
}

/// Reindex on exact pixel width or font changes, not rounded ASCII columns.
#[derive(Clone, PartialEq)]
struct ViewerLayout {
    width: Pixels,
    font_size: Pixels,
    font: gpui_kit::Font,
}

impl Viewer {
    /// The row at the top of the list and how many rows it shows, from the
    /// list's last layout: every row is as tall as the others. A move asked
    /// for and not yet laid out counts as done.
    fn top_and_page(&self) -> (usize, usize) {
        let state = self.scroll.0.borrow();
        let laid_out = state.last_item_size.and_then(|size| {
            let row = size.contents.height / self.starts.len().max(1) as f32;
            (row > px(0.)).then(|| {
                let top = (-state.base_handle.offset().y / row).floor().max(0.) as usize;
                (top, (size.item.height / row).floor().max(1.) as usize)
            })
        });
        let (top, page) = laid_out.unwrap_or((0, 1));
        match state.deferred_scroll_to_item {
            Some(deferred) => (deferred.item_index, page),
            None => (top, page),
        }
    }
}

/// The open command palette.
struct Palette {
    query: Entity<InputState>,
    /// Index into the commands that match the query.
    selected: usize,
    /// Why the last chosen command did not run, until the query changes or
    /// another command is chosen.
    notice: Option<SharedString>,
    _changes: Subscription,
}

/// The control that had focus when a modal overlay came up, so dismissing the
/// overlay can hand focus back to exactly that control.
struct FocusOrigin {
    handle: FocusHandle,
    /// The region the control sits in, or `None` for a header control (the
    /// header is always on screen). Focus only returns to a control whose
    /// region is still shown.
    region: Option<Region>,
}

/// The parts of a reflowed page, top to bottom. Each can hold focus, so
/// each can be brought into view from the keyboard. The composer is only
/// there in Chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSection {
    Header,
    Conversation,
    Composer,
    Status,
}

impl PageSection {
    /// The parts a page has, in order.
    fn of_page(chat: bool) -> &'static [PageSection] {
        if chat {
            &[
                Self::Header,
                Self::Conversation,
                Self::Composer,
                Self::Status,
            ]
        } else {
            &[Self::Header, Self::Conversation, Self::Status]
        }
    }
}

/// What a reflowed page was last scrolled to show, and the layout it was
/// shown in. When any of it changes, the part that holds focus is scrolled
/// into view again; between changes the page stays where the user put it.
#[derive(Clone, PartialEq)]
struct Reveal {
    /// The control that holds focus, not only its part: moving from one
    /// control to the next in a part the user scrolled away from brings
    /// that part back.
    focused: Option<FocusHandle>,
    section: Option<PageSection>,
    viewport: Size<Pixels>,
    scale: TextScale,
}

/// Where a reflowed page and its parts are, as last laid out.
#[derive(Debug, Clone, PartialEq)]
pub struct PageMetrics {
    /// The part of the window the page scrolls in.
    pub viewport: Bounds<Pixels>,
    /// How far the page is scrolled down, and how far it can be.
    pub scrolled: Pixels,
    pub scroll_max: Pixels,
    /// The page's parts, top to bottom, where they are with the page
    /// scrolled to its top.
    pub sections: Vec<(PageSection, Bounds<Pixels>)>,
}

impl PageMetrics {
    /// Where a part is in the window now, with the page scrolled as it is.
    pub fn shown(&self, section: PageSection) -> Option<Bounds<Pixels>> {
        let (_, mut bounds) = *self.sections.iter().find(|(part, _)| *part == section)?;
        bounds.origin.y -= self.scrolled;
        Some(bounds)
    }

    /// Whether a part is wholly inside the page's part of the window now.
    pub fn is_inside(&self, section: PageSection) -> bool {
        self.shown(section).is_some_and(|part| {
            part.top() >= self.viewport.top()
                && part.bottom() <= self.viewport.bottom()
                && part.left() >= self.viewport.left()
                && part.right() <= self.viewport.right()
        })
    }
}

pub struct Shell {
    client: Option<Client>,
    client_connected: bool,
    client_snapshot_ready: bool,
    client_generation: u64,
    client_status: String,
    client_attention: bool,
    client_questions: crate::questions::State,
    question_panel: Option<questions::Panel>,
    session_editor: Option<sessions::Panel>,
    export_save_prompt: Option<sessions::ExportSavePrompt>,
    export_save_notice: Option<SharedString>,
    question_drafts: Drafts,
    question_draft_ids: Vec<String>,
    questions_toggle_focus: FocusHandle,
    step_panel: Option<step_review::Panel>,
    approval_panel: Option<approval_review::Panel>,
    approval_open_focus: FocusHandle,
    step_draft: Option<(uuid::Uuid, formless_protocol::PendingStep, String)>,
    step_open_focus: FocusHandle,
    mode_enable_focus: FocusHandle,
    mode_observed: crate::step_mode::Status,
    client_step_mode: Option<bool>,
    client_steps: crate::steps::Inventory,
    client_recovery: Option<crate::replies::UnfinishedRun>,
    client_sessions: Vec<SessionInfo>,
    forecast: Option<ForecastDialog>,
    forecast_composing: bool,
    forecast_focus: FocusHandle,
    forecast_cancel_focus: FocusHandle,
    forecast_scroll: ScrollHandle,
    resource_pages: HashMap<Destination, crate::pages::Page>,
    resource_scroll: gpui_kit::UniformListScrollHandle,
    resource_body: ScrollHandle,
    comparison_controls: Vec<(FocusHandle, gpui_kit::ScrollAnchor)>,
    resource_controls: Vec<FocusHandle>,
    resource_focus: Option<(Option<FocusHandle>, Size<Pixels>, TextScale)>,
    resource_frames: u8,
    resource_revision: u64,
    comparison: crate::comparison::State,
    run_inspection: crate::run_inspection::State,
    invocations: crate::invocations::State,
    fork_review: Option<session_fork::Review>,
    fork_observed: crate::session_fork::Status,
    fork_local_error: Option<SharedString>,
    fork_scroll: ScrollHandle,
    fork_controls: Vec<FocusHandle>,
    fork_focus: Option<(Option<FocusHandle>, Size<Pixels>, TextScale)>,
    fork_frames: u8,
    sidebar: Sidebar,
    /// Shared with the timeline's row renderer.
    blocks: Rc<RefCell<Timeline>>,
    /// Collapses stream deltas into one view update per flush interval.
    stream: Coalescer,
    drafts: Drafts,
    prefs: PanelPrefs,
    /// The text size the user asked for. It is the size shown, in a window
    /// of any size: a window too small for it is reflowed, not given
    /// smaller text.
    text_scale: TextScale,
    /// The text size the theme's fonts were last set to.
    applied_scale: TextScale,
    /// Scrolls the whole shell up and down in a reflowed window.
    page_scroll: ScrollHandle,
    /// Frames still to be drawn with the focused part scrolled into view.
    reveal_frames: u8,
    /// Scrolls the inspector's details in a panel too short for them.
    inspector_scroll: ScrollHandle,
    session_usage: crate::session_usage::State,
    usage_controls: Vec<(FocusHandle, gpui_kit::ScrollAnchor)>,
    usage_focus: Option<(Option<FocusHandle>, Size<Pixels>, TextScale)>,
    usage_frames: u8,
    revealed: Option<Reveal>,
    destination: Destination,
    /// What the last composer action was. Sizes only: no message text is
    /// kept once an intent has been handed on.
    last_intent: Option<IntentRecord>,
    /// The composer's text as of its last accepted change: always within
    /// the limit, and what an over-limit edit is rolled back to.
    accepted_draft: SharedString,
    /// Counts composer replacements. Work deferred from a composer change
    /// records the generation it was scheduled under and does nothing if the
    /// composer has been replaced since (a Session switch, another rebuild).
    /// A late callback must never write one Session's text, or apply one
    /// Session's selection, to another Session's composer.
    composer_generation: u64,
    /// The over-limit edit being taken back, if any. From the moment one is
    /// seen until [`Shell::finish_rejection`] has dealt with it, the
    /// composer is not judged again and its contents are not the user's
    /// draft: the record's `previous` is.
    rejections: Rejections,
    undo_budget: UndoBudget,
    /// Why the composer refused or changed something, shown under it.
    composer_notice: Option<SharedString>,
    /// What held the window modally at the last settle.
    modal: Option<Modal>,
    palette: Option<Palette>,
    palette_focus: FocusHandle,
    viewer: Option<Viewer>,
    viewer_focus: FocusHandle,
    palette_scroll: ScrollHandle,
    /// What the commands depended on when the shell was last drawn; what
    /// its buttons show. Running a command reads it afresh.
    commands: commands::State,
    /// What had focus when the current overlay came up.
    restore_focus: Option<FocusOrigin>,
    timeline: Entity<MessageScrollerState>,
    reading_positions: crate::reading_positions::Positions,
    reading_restore: Option<String>,
    reading_notice: Option<&'static str>,
    /// A loaded empty snapshot is different from a Session not loaded yet.
    transcript_source: TranscriptSource,
    transcript_state: Option<TranscriptState>,
    stream_view_owner: Option<client::StreamOwner>,
    stream_view_latest: Option<client::StreamOwner>,
    directory: Directory,
    directory_rows: Vec<crate::model::SessionRow>,
    include_archived: bool,
    directory_query: Entity<InputState>,
    directory_scroll: gpui_kit::UniformListScrollHandle,
    directory_anchor: gpui_kit::ScrollAnchor,
    directory_reveal: Option<(String, u8)>,
    search: TranscriptSearch,
    search_query: Entity<InputState>,
    profile_query: Entity<InputState>,
    profile_observed: SharedString,
    profile_search: crate::profile_search::State,
    profile_return: Destination,
    profile_scroll: gpui_kit::UniformListScrollHandle,
    profile_body: ScrollHandle,
    profile_controls: Vec<FocusHandle>,
    profile_focus: Option<(Option<FocusHandle>, Size<Pixels>, TextScale)>,
    profile_frames: u8,
    profile_show_results: bool,
    search_scroll: gpui_kit::UniformListScrollHandle,
    search_revision: u64,
    search_return: Destination,
    search_notice: Option<&'static str>,
    block_copy_notice: Option<&'static str>,
    composer: Entity<TextareaState>,
    enter_sends: bool,
    preferences: Option<crate::preferences::Store>,
    preference_notice: Option<SharedString>,
    session_scroll: VirtualListScrollHandle,
    session_sizes: Rc<Vec<Size<Pixels>>>,
    sidebar_focus: FocusHandle,
    conversation_focus: FocusHandle,
    inspector_focus: FocusHandle,
    /// The header controls own stable handles so focus can return to them,
    /// and so a reflowed page knows when to scroll the header into view.
    sidebar_toggle_focus: FocusHandle,
    inspector_toggle_focus: FocusHandle,
    theme_toggle_focus: FocusHandle,
    commands_toggle_focus: FocusHandle,
    resume_focus: FocusHandle,
    abandon_focus: FocusHandle,
    resume_anchor: gpui_kit::ScrollAnchor,
    abandon_anchor: gpui_kit::ScrollAnchor,
    composer_anchor: gpui_kit::ScrollAnchor,
    mode_enable_anchor: gpui_kit::ScrollAnchor,
    /// The status line is a tab stop on a reflowed page, where it can be
    /// off screen and holds nothing else that focus could bring into view.
    status_focus: FocusHandle,
    /// The composer's event subscription and its observer. Replaced with it.
    composer_subscriptions: [Subscription; 2],
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(
        sessions: &[SessionInfo],
        running: &HashSet<String>,
        blocks: Vec<TimelineBlock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut sidebar = Sidebar::default();
        sidebar.replace(sessions, running, true);
        sidebar.select_index(0);
        sidebar.activate_selected();

        let timeline = cx.new(|cx| MessageScrollerState::new(blocks.len(), cx));
        let (composer, composer_subscriptions) = Self::build_composer("", true, window, cx);
        // A resize can end an overlay (the panel docks again) or hide the
        // panel that holds focus.
        let mut subscriptions =
            vec![cx.observe_window_bounds(window, |this, window, cx| this.settle(window, cx))];
        let directory_query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filter title, preview or ID")
                .validate(|text, _| directory::valid_filter(text))
        });
        subscriptions.push(cx.subscribe_in(
            &directory_query,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = this.directory_query.read(cx).value();
                    this.directory.rebuild(&this.directory_rows, &query);
                    this.directory_reveal = None;
                    this.revealed = None;
                    if let Some(index) = this.directory.choice(&this.directory_rows) {
                        this.directory_scroll
                            .scroll_to_item_strict(index, ScrollStrategy::Top);
                    }
                    cx.notify();
                }
            },
        ));
        let search_query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Literal, case-sensitive text")
                .validate(|text, _| search::valid_query(text))
        });
        subscriptions.push(cx.subscribe_in(
            &search_query,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) && this.destination == Destination::Search {
                    this.refresh_search(cx);
                }
            },
        ));
        let mut directory = Directory::default();
        let profile_query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search this Profile's persisted messages")
                .validate(|text, _| {
                    text.chars()
                        .take(crate::profile_search::QUERY_CHARS + 1)
                        .count()
                        <= crate::profile_search::QUERY_CHARS
                })
        });
        subscriptions.push(cx.observe(&profile_query, |this, _, cx| {
            let query = this.profile_query.read(cx).value();
            if query != this.profile_observed {
                this.profile_search.changed(&query);
                this.profile_observed = query;
                this.profile_show_results = false;
                this.profile_frames = 2;
            }
            cx.notify();
        }));
        let resource_body = ScrollHandle::new();
        let profile_body = ScrollHandle::new();
        let directory_rows = sidebar.rows().to_vec();
        directory.rebuild(&directory_rows, "");
        composer.update(cx, |composer, cx| composer.focus(window, cx));

        let session_sizes = Self::row_sizes(sidebar.rows().len(), TextScale::default());
        let page_scroll = ScrollHandle::new();
        let directory_anchor = gpui_kit::ScrollAnchor::for_handle(page_scroll.clone());
        let inspector_scroll = ScrollHandle::new();
        Self {
            client: None,
            client_connected: false,
            client_snapshot_ready: false,
            client_generation: 0,
            client_status: String::new(),
            client_attention: false,
            client_questions: crate::questions::State::default(),
            question_panel: None,
            session_editor: None,
            export_save_prompt: None,
            export_save_notice: None,
            question_drafts: Drafts::new(16, 4 * 1024 * 1024),
            question_draft_ids: Vec::new(),
            questions_toggle_focus: cx.focus_handle(),
            step_panel: None,
            approval_panel: None,
            approval_open_focus: cx.focus_handle(),
            step_draft: None,
            step_open_focus: cx.focus_handle(),
            mode_enable_focus: cx.focus_handle(),
            mode_observed: Default::default(),
            client_step_mode: None,
            client_steps: Default::default(),
            client_recovery: None,
            client_sessions: Vec::new(),
            forecast: None,
            forecast_composing: false,
            forecast_focus: cx.focus_handle(),
            forecast_cancel_focus: cx.focus_handle(),
            forecast_scroll: ScrollHandle::new(),
            resource_pages: HashMap::new(),
            resource_scroll: gpui_kit::UniformListScrollHandle::new(),
            resource_controls: (0..4).map(|_| cx.focus_handle()).collect(),
            resource_body,
            resource_focus: None,
            resource_frames: 0,
            comparison_controls: (0..2)
                .map(|_| {
                    (
                        cx.focus_handle(),
                        gpui_kit::ScrollAnchor::for_handle(page_scroll.clone()),
                    )
                })
                .collect(),
            resource_revision: 0,
            comparison: crate::comparison::State::default(),
            run_inspection: crate::run_inspection::State::default(),
            invocations: crate::invocations::State::default(),
            fork_review: None,
            fork_observed: crate::session_fork::Status::Idle,
            fork_local_error: None,
            fork_scroll: ScrollHandle::new(),
            fork_controls: (0..2).map(|_| cx.focus_handle()).collect(),
            fork_focus: None,
            fork_frames: 0,
            sidebar,
            blocks: Rc::new(RefCell::new(Timeline::new(blocks))),
            stream: Coalescer::default(),
            drafts: Drafts::new(limits::DRAFT_MAX_ENTRIES, limits::DRAFT_MAX_BYTES),
            prefs: PanelPrefs::default(),
            text_scale: TextScale::default(),
            applied_scale: TextScale::default(),
            resume_anchor: gpui_kit::ScrollAnchor::for_handle(page_scroll.clone()),
            abandon_anchor: gpui_kit::ScrollAnchor::for_handle(page_scroll.clone()),
            composer_anchor: gpui_kit::ScrollAnchor::for_handle(page_scroll.clone()),
            mode_enable_anchor: gpui_kit::ScrollAnchor::for_handle(page_scroll.clone()),
            page_scroll,
            reveal_frames: 0,
            usage_controls: (0..2)
                .map(|_| {
                    (
                        cx.focus_handle(),
                        gpui_kit::ScrollAnchor::for_handle(inspector_scroll.clone()),
                    )
                })
                .collect(),
            inspector_scroll,
            session_usage: crate::session_usage::State::default(),
            usage_focus: None,
            usage_frames: 0,
            revealed: None,
            destination: Destination::Chat,
            last_intent: None,
            accepted_draft: SharedString::default(),
            composer_generation: 0,
            rejections: Rejections::default(),
            undo_budget: UndoBudget::default(),
            composer_notice: None,
            modal: None,
            commands: commands::State {
                composer: ComposerActions::for_state(RunState::Idle, true),
                has_composer: true,
                can_recover: false,
                can_questions: false,
                can_approvals: false,
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
                context_shown: true,
                dark: false,
                text_scale: TextScale::default(),
                destination: Destination::Chat,
                palette_open: false,
            },
            palette: None,
            palette_focus: cx.focus_handle(),
            viewer: None,
            viewer_focus: cx.focus_handle(),
            palette_scroll: ScrollHandle::new(),
            restore_focus: None,
            timeline,
            reading_positions: crate::reading_positions::Positions::default(),
            reading_restore: None,
            reading_notice: None,
            transcript_source: TranscriptSource::Fixture,
            transcript_state: None,
            stream_view_owner: None,
            stream_view_latest: None,
            directory,
            directory_rows,
            include_archived: false,
            directory_query,
            directory_scroll: gpui_kit::UniformListScrollHandle::new(),
            directory_anchor,
            directory_reveal: None,
            search: TranscriptSearch::default(),
            search_query,
            profile_query,
            profile_observed: SharedString::default(),
            profile_search: crate::profile_search::State::default(),
            profile_return: Destination::Chat,
            profile_scroll: gpui_kit::UniformListScrollHandle::new(),
            profile_controls: (0..4).map(|_| cx.focus_handle()).collect(),
            profile_body,
            profile_focus: None,
            profile_frames: 0,
            profile_show_results: false,
            search_scroll: gpui_kit::UniformListScrollHandle::new(),
            search_revision: 0,
            search_return: Destination::Chat,
            search_notice: None,
            block_copy_notice: None,
            composer,
            enter_sends: true,
            preferences: None,
            preference_notice: None,
            session_scroll: VirtualListScrollHandle::new(),
            session_sizes,
            sidebar_focus: cx.focus_handle(),
            conversation_focus: cx.focus_handle(),
            inspector_focus: cx.focus_handle(),
            sidebar_toggle_focus: cx.focus_handle(),
            inspector_toggle_focus: cx.focus_handle(),
            theme_toggle_focus: cx.focus_handle(),
            commands_toggle_focus: cx.focus_handle(),
            resume_focus: cx.focus_handle(),
            abandon_focus: cx.focus_handle(),
            status_focus: cx.focus_handle().tab_stop(true),
            composer_subscriptions,
            _subscriptions: subscriptions,
        }
    }

    /// A composer holding `text`, with an empty undo history, and what
    /// routes its events and its changes to the shell.
    fn build_composer(
        text: &str,
        enter_sends: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextareaState>, [Subscription; 2]) {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(enter_sends)
                .placeholder("Message Formless")
                .default_value(text.to_string())
        });
        let events = cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                // Plain Enter is the primary action; the secondary chord is
                // the secondary one (Queue during a Run). Shift+Enter also
                // reports PressEnter, but it has already inserted a newline.
                if let InputEvent::PressEnter {
                    secondary,
                    shift: false,
                } = event
                {
                    // In newline mode the kit inserted a newline already.
                    // Modified Enter has its own scoped action, before the
                    // kit's multiline handler can insert anything.
                    if !this.enter_sends {
                        return;
                    }
                    let command = if *secondary {
                        Command::SendSecondary
                    } else {
                        Command::Send
                    };
                    this.run_command(command, window, cx);
                }
            },
        );
        // The limit is checked whenever the composer changes in any way, not
        // on its Change event: ending an input-method composition makes its
        // text part of the draft without emitting one.
        let changes = cx.observe_in(&composer, window, |this, _, window, cx| {
            this.composer_changed(window, cx)
        });
        (composer, [events, changes])
    }

    /// Swap in a fresh composer holding `text`, with the selection at
    /// `selection` if given. This is the only way to drop the kit's undo
    /// history, so it is how a Session switch keeps one Session's text out
    /// of another's undo and how the history is dropped after large edits
    /// (which mitigates its growth; no bound on it is claimed). Focus stays in the
    /// composer if it was there.
    fn rebuild_composer(
        &mut self,
        text: &str,
        selection: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_forecast(cx);
        let had_focus = self.composer.focus_handle(cx).contains_focused(window, cx);
        let (composer, subscriptions) = Self::build_composer(text, self.enter_sends, window, cx);
        self.composer = composer;
        self.composer_subscriptions = subscriptions;
        // An open overlay may be waiting to hand focus back to the composer
        // that was just replaced; point it at the new one.
        if let Some(origin) = &mut self.restore_focus {
            if origin.region == Some(Region::Composer) {
                origin.handle = self.composer.focus_handle(cx);
            }
        }
        self.accepted_draft = text.to_string().into();
        // Anything still queued for the old composer is now void.
        self.composer_generation += 1;
        self.rejections.void();
        self.undo_budget.reset();
        self.composer.update(cx, |composer, cx| {
            if let Some(selection) = selection {
                composer.set_selected_range(selection, cx);
            }
            if had_focus {
                composer.focus(window, cx);
            }
        });
        cx.notify();
    }

    /// Runs whenever the composer changes, whatever path the text came by.
    ///
    /// An edit whose result is over the limit is rejected whole. Nothing is
    /// ever cut to fit, so text after an insertion point is never lost and no
    /// character is split. The kit has no hook to refuse a multi-line edit
    /// before it happens, so the rejection is made afterwards, by
    /// [`Self::finish_rejection`]. It cannot run from inside the composer's
    /// own change delivery, so it is deferred, and it is tied by a ticket to
    /// this edit in this composer (see [`Rejections`]).
    fn composer_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rejections.is_pending() {
            return;
        }
        let draft = self.draft(cx);
        let composing = self.composer.update(cx, |composer, cx| {
            composer.marked_text_range(window, cx).is_some()
        });
        if draft != self.accepted_draft || composing != self.forecast_composing {
            self.invalidate_forecast(cx);
        }
        self.forecast_composing = composing;
        if draft == self.accepted_draft {
            return;
        }
        // Typing brings the composer back into view on a reflowed page the
        // user had scrolled away from it.
        self.revealed = None;
        // Text still being composed by an input method is not judged: taking
        // it back mid-composition would fight the input method. It is judged
        // the moment the composition ends, committed or not.
        if composing {
            return;
        }
        let span = edit_span(&self.accepted_draft, &draft);
        let generation = self.composer_generation;
        if !within_composer_limit(&draft) {
            let ticket = self.rejections.begin(
                generation,
                self.accepted_draft.to_string(),
                snap_to_graphemes(&self.accepted_draft, span.start..span.old_end),
                draft.len(),
            );
            cx.defer_in(window, move |this, window, cx| {
                this.finish_rejection(ticket, window, cx)
            });
            return;
        }
        self.accepted_draft = draft;
        self.undo_budget.record_edit(span);
        if self.undo_budget.is_spent() {
            cx.defer_in(window, move |this, window, cx| {
                this.drop_undo_history(generation, window, cx)
            });
        } else {
            self.composer_notice = None;
        }
        // Button availability depends on whether there is a draft.
        cx.notify();
    }

    /// Take an over-limit edit back, in one uninterrupted step:
    ///
    /// 1. Check that the ticket still names the pending rejection and that
    ///    the composer it was made in is still the current one. If not, this
    ///    callback is stale: it touches nothing, dispatches nothing and
    ///    consumes nothing, because whatever replaced the composer has
    ///    already dealt with the over-limit text.
    /// 2. Ask the kit to undo its own last edit, addressed to this composer
    ///    by its focus handle, not to whatever has focus. At best that puts
    ///    back exactly the previous text and selection and keeps the earlier
    ///    undo history.
    /// 3. Check the result. The undo may have done more (the kit merges
    ///    quick typing into one step) or nothing. If the composer does not
    ///    hold exactly the previous draft, the previous draft is put back
    ///    whole by rebuilding the composer, at the cost of its history.
    ///
    /// The user is always told, including whether the history survived.
    fn finish_rejection(&mut self, ticket: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rejections.is_current(ticket, self.composer_generation) {
            return;
        }
        self.composer
            .focus_handle(cx)
            .dispatch_action(&Undo, window, cx);
        // Still current: the dispatch above runs the kit's handler and
        // nothing of this shell's.
        let Some(rejection) = self
            .rejections
            .take_if_current(ticket, self.composer_generation)
        else {
            return;
        };
        let history_kept = self.draft(cx) == rejection.previous;
        if !history_kept {
            self.rebuild_composer(&rejection.previous, Some(rejection.selection), window, cx);
        }
        self.composer_notice = Some(
            format!(
                "Not added: the message would be {} and the limit is {}. Your text is \
                 unchanged{}",
                format_bytes(rejection.would_be_bytes),
                format_bytes(limits::COMPOSER_MAX_BYTES),
                if history_kept {
                    "."
                } else {
                    ", but its undo history had to be cleared."
                }
            )
            .into(),
        );
        cx.notify();
    }

    /// Drop the composer's undo history once [`UndoBudget`] is spent, keeping
    /// the text and selection it has now. Deferred, so it works from the
    /// composer's current state, not from anything captured earlier, and
    /// only if it is still the composer it was scheduled for.
    fn drop_undo_history(&mut self, generation: u64, window: &mut Window, cx: &mut Context<Self>) {
        if generation != self.composer_generation || self.rejections.is_pending() {
            return;
        }
        let draft = self.draft(cx);
        if !within_composer_limit(&draft) {
            // A later edit went over the limit; its rejection comes first.
            return;
        }
        let selection = self.composer.read(cx).selected_range();
        self.rebuild_composer(&draft, Some(selection), window, cx);
        self.composer_notice = Some(
            "Undo history was cleared to bound memory after large edits. Your text is unchanged."
                .into(),
        );
        cx.notify();
    }

    /// The composer's paste hook. Returns true to consume the paste so that
    /// nothing is inserted. Only a clipboard that could not fit even if it
    /// replaced the whole draft is refused here; a smaller one is inserted
    /// and judged in [`Self::composer_changed`] on the text that results, so
    /// a replaced selection is accounted for exactly.
    fn paste_refused(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let Some(text) = item.text() else {
            return false;
        };
        if !paste_can_never_fit(text.len()) {
            return false;
        }
        self.composer_notice = Some(
            format!(
                "Paste not added: it is {} and the limit is {}. Attach larger content instead.",
                format_bytes(text.len()),
                format_bytes(limits::COMPOSER_MAX_BYTES)
            )
            .into(),
        );
        cx.notify();
        true
    }

    /// Whether an unsent draft is stored for a Session other than the open one.
    pub fn has_stored_draft(&self, session_id: &str) -> bool {
        self.drafts.has(session_id)
    }

    /// The composer's text state, for driving its input handler directly.
    pub fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    /// The open palette's search field, for driving its input handler
    /// directly.
    pub fn palette_query(&self) -> Option<&Entity<InputState>> {
        self.palette.as_ref().map(|palette| &palette.query)
    }

    /// The composer's selection as byte offsets into the draft.
    pub fn composer_selection(&self, cx: &App) -> Range<usize> {
        self.composer.read(cx).selected_range()
    }

    fn row_sizes(count: usize, scale: TextScale) -> Rc<Vec<Size<Pixels>>> {
        let row = size(
            px(tokens::SIDEBAR_DEFAULT * scale.factor()),
            px(SESSION_ROW_HEIGHT * scale.factor()),
        );
        Rc::new(vec![row; count])
    }

    /// A fixed size from the layout tokens, at the current text size.
    fn scaled(&self, value: f32) -> Pixels {
        px(value * self.text_scale.factor())
    }

    /// Scroll the inspector's details by a line or by most of a panel, down
    /// for a positive `direction`.
    fn scroll_inspector(&mut self, direction: f32, page: bool, cx: &mut Context<Self>) {
        let step = if page {
            self.inspector_scroll.bounds().size.height * 0.8
        } else {
            self.scaled(INSPECTOR_LINE)
        };
        let mut offset = self.inspector_scroll.offset();
        let furthest = -self.inspector_scroll.max_offset().y;
        offset.y = (offset.y - step * direction).clamp(furthest, px(0.));
        self.inspector_scroll.set_offset(offset);
        cx.notify();
    }

    /// A panel's width: its token width at the current text size, or the
    /// window's width if that is less, so an overlay panel is never wider
    /// than the window it covers.
    fn panel_width(&self, token: f32, window: &Window) -> Pixels {
        self.scaled(token).min(window.viewport_size().width)
    }

    /// Whether everything but the open dialog is out of the accessibility
    /// tree: true exactly while a Sessions or Context overlay is open.
    pub fn background_inert(&self, window: &Window) -> bool {
        self.current_modal(self.layout(window)).is_some()
    }

    /// What holds the window modally in `layout`: the palette, which is
    /// always on top, or a panel overlay.
    fn current_modal(&self, layout: Layout) -> Option<Modal> {
        if self.session_editor.is_some() {
            Some(Modal::SessionEditor)
        } else if self.approval_panel.is_some() {
            Some(Modal::Approvals)
        } else if self.step_panel.is_some() {
            Some(Modal::Step)
        } else if self.question_panel.is_some() {
            Some(Modal::Questions)
        } else if self.forecast.is_some() {
            Some(Modal::Forecast)
        } else if self.palette.is_some() {
            Some(Modal::Palette)
        } else if self.viewer.is_some() {
            Some(Modal::Viewer)
        } else {
            layout.modal().map(Modal::Panel)
        }
    }

    /// How the shell is fitted into the window as it is now.
    pub fn fit(&self, window: &Window) -> Fit {
        let viewport = window.viewport_size();
        self.text_scale
            .fit(f32::from(viewport.width), f32::from(viewport.height))
    }

    /// The window width the layout breakpoints see.
    fn effective_width(&self, window: &Window) -> f32 {
        let viewport = window.viewport_size();
        self.text_scale
            .layout_width(f32::from(viewport.width), f32::from(viewport.height))
    }

    /// The text size the user asked for, which is the size shown.
    pub fn text_scale(&self) -> TextScale {
        self.text_scale
    }

    /// Set the text size in the window as it is. The text is shown at this
    /// size whatever the window's size: a window smaller than the size needs
    /// is reflowed ([`Fit::Reflowed`]), with every control still reachable.
    pub fn set_text_scale(
        &mut self,
        scale: TextScale,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if scale == self.text_scale {
            return;
        }
        self.text_scale = scale;
        self.settle(window, cx);
    }

    /// Set the text size, and first ask the window to grow to the size that
    /// shows everything at once at that text size, if it is smaller. This is
    /// what the shortcuts do. Whether the window grows is up to the platform
    /// (a display can be too small); if it does not, the shell is reflowed.
    pub fn set_text_scale_growing_window(
        &mut self,
        scale: TextScale,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (needed_width, needed_height) = scale.minimum_window();
        let viewport = window.viewport_size();
        if f32::from(viewport.width) < needed_width || f32::from(viewport.height) < needed_height {
            window.resize(size(
                viewport.width.max(px(needed_width)),
                viewport.height.max(px(needed_height)),
            ));
        }
        self.set_text_scale(scale, window, cx);
    }

    /// Bring the fonts into line with the text size. The kit theme's font
    /// sizes drive every rem-based size (text, controls); the shell's own
    /// fixed sizes and its breakpoints follow the same factor.
    fn apply_text_scale(&mut self, cx: &mut Context<Self>) {
        let scale = self.text_scale;
        if scale == self.applied_scale {
            return;
        }
        self.applied_scale = scale;
        let theme = Theme::global_mut(cx);
        theme.font_size = px(BASE_FONT_SIZE * scale.factor());
        theme.mono_font_size = px(BASE_MONO_FONT_SIZE * scale.factor());
        self.session_sizes = Self::row_sizes(self.sidebar.rows().len(), scale);
        cx.refresh_windows();
    }

    /// Where the reflowed page and its parts were when last laid out.
    pub fn page_metrics(&self) -> PageMetrics {
        let sections = PageSection::of_page(self.destination == Destination::Chat)
            .iter()
            .enumerate()
            .filter_map(|(index, section)| {
                Some((*section, self.page_scroll.bounds_for_item(index)?))
            })
            .collect();
        PageMetrics {
            viewport: self.page_scroll.bounds(),
            scrolled: -self.page_scroll.offset().y,
            scroll_max: self.page_scroll.max_offset().y,
            sections,
        }
    }

    /// Move focus to a part of the page: its first header control, the
    /// conversation, the composer, or the status line.
    pub fn focus_section(
        &mut self,
        section: PageSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match section {
            PageSection::Header => window.focus(&self.sidebar_toggle_focus, cx),
            PageSection::Conversation => window.focus(&self.conversation_focus, cx),
            PageSection::Composer => self
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx)),
            PageSection::Status => window.focus(&self.status_focus, cx),
        }
        // Asking for a part shows it, even if focus was already there.
        self.revealed = None;
        cx.notify();
    }

    /// The part of a reflowed page that holds focus, if one does. Focus in
    /// an overlay panel is in none of them.
    pub fn focused_section(&self, window: &Window, cx: &App) -> Option<PageSection> {
        if self.composer.focus_handle(cx).contains_focused(window, cx)
            || self.resume_focus.is_focused(window)
            || self.abandon_focus.is_focused(window)
        {
            Some(PageSection::Composer)
        } else if self.conversation_focus.contains_focused(window, cx) {
            Some(PageSection::Conversation)
        } else if self.status_focus.is_focused(window) {
            Some(PageSection::Status)
        } else if [
            &self.sidebar_toggle_focus,
            &self.inspector_toggle_focus,
            &self.theme_toggle_focus,
            &self.commands_toggle_focus,
        ]
        .into_iter()
        .any(|handle| handle.is_focused(window))
        {
            Some(PageSection::Header)
        } else {
            None
        }
    }

    /// In a reflowed window, scroll the part that holds focus into view
    /// when focus has moved to another part or the layout has changed (the
    /// window was resized, the text size changed), or the user asked for a
    /// part by name. A focused control is then never left off screen by a
    /// change it did not make. Nothing happens otherwise, so the page stays
    /// where the user scrolled it.
    ///
    /// GPUI scrolls to a child using where that child was in the frame
    /// before. When the layout has just changed, that is where it used to
    /// be, or nowhere. So the request is made for two frames running, and
    /// the second frame is asked for: the first lays the page out as it now
    /// is, the second scrolls to where the part really is.
    fn reveal_focused(&mut self, window: &mut Window, cx: &mut App) {
        let reveal = Reveal {
            focused: window.focused(cx),
            section: self.focused_section(window, cx),
            viewport: window.viewport_size(),
            scale: self.text_scale,
        };
        if self.revealed.as_ref() != Some(&reveal) {
            self.revealed = Some(reveal);
            self.reveal_frames = 2;
        }
        if self.reveal_frames == 0 {
            return;
        }
        self.reveal_frames -= 1;
        // Recovery can make the composer taller than the viewport. Scroll to
        // its focused descendant instead of trying to fit the whole section.
        let anchor = if self.mode_enable_focus.is_focused(window) {
            Some(&self.mode_enable_anchor)
        } else if self.resume_focus.is_focused(window) {
            Some(&self.resume_anchor)
        } else if self.abandon_focus.is_focused(window) {
            Some(&self.abandon_anchor)
        } else if self.client_recovery.is_some()
            && self.composer.focus_handle(cx).contains_focused(window, cx)
        {
            Some(&self.composer_anchor)
        } else {
            None
        };
        if let Some(anchor) = anchor {
            anchor.scroll_to(window, cx);
            window.request_animation_frame();
            return;
        }
        let section = self.revealed.as_ref().and_then(|reveal| reveal.section);
        let parts = PageSection::of_page(self.destination == Destination::Chat);
        if let Some(index) = parts.iter().position(|part| Some(*part) == section) {
            self.page_scroll.scroll_to_item(index);
            if self.reveal_frames > 0 {
                window.request_animation_frame();
            }
        }
    }

    pub fn sidebar(&self) -> &Sidebar {
        &self.sidebar
    }

    pub fn destination(&self) -> Destination {
        self.destination
    }

    pub fn last_intent(&self) -> Option<&IntentRecord> {
        self.last_intent.as_ref()
    }

    pub fn composer_notice(&self) -> Option<&str> {
        self.composer_notice.as_deref()
    }

    pub fn block_count(&self) -> usize {
        self.blocks.borrow().len()
    }

    /// The last block's size and how much of it its row does not show, as
    /// (total bytes, bytes before the window, bytes after it).
    pub fn last_block_window(&self) -> Option<(usize, usize, usize)> {
        self.shown_blocks().blocks().last().map(|block| {
            (
                block.total_bytes(),
                block.hidden_before(),
                block.hidden_after(),
            )
        })
    }

    /// What the last block's row lays out.
    pub fn last_block_preview(&self) -> String {
        self.shown_blocks()
            .blocks()
            .last()
            .map(|block| block.preview().to_string())
            .unwrap_or_default()
    }

    /// The blocks, with the streamed row brought up to date with its text
    /// first. This is how a row is read to be shown: the row is rebuilt
    /// here, when something looks at it, not when each delta arrives.
    fn shown_blocks(&self) -> Ref<'_, Timeline> {
        self.blocks.borrow_mut().settle();
        self.blocks.borrow()
    }

    /// Work done so far to keep streamed rows up to date.
    pub fn stream_work(&self) -> StreamWork {
        self.blocks.borrow().stream_work()
    }

    /// Stream deltas received and view updates made for them.
    pub fn stream_stats(&self) -> (u64, u64) {
        (self.stream.requests(), self.stream.flushes())
    }

    /// Fixture mode records intents. Connected mode hands them to the
    /// bounded worker queue; a draft is kept until a definite outcome.
    fn emit(&mut self, intent: Intent, cx: &mut Context<Self>) {
        self.last_intent = Some(IntentRecord::from(&intent));
        if self.client.is_some() {
            self.composer_notice =
                Some("Action queued; waiting for daemon acknowledgement. Draft kept.".into());
            if intent.starts_work() {
                let draft = self.draft(cx).to_string();
                if let Some(client) = &self.client {
                    if let Err(error) = client.review(intent, draft) {
                        self.composer_notice = Some(format!("{error} Draft kept.").into());
                    }
                }
            } else {
                self.invalidate_forecast(cx);
                self.client_request(ClientCommand::Intent(intent), cx);
            }
        }
        cx.notify();
    }

    pub fn connect_daemon(&mut self, endpoint: client::Endpoint, cx: &mut Context<Self>) {
        self.attach_client(Client::start(endpoint), cx);
    }

    /// Actual transport through TestPlatform only; never a production enable path.
    #[cfg(all(feature = "test-support", feature = "daemon-tests"))]
    pub fn connect_future_attempts_for_validation(
        &mut self,
        endpoint: client::Endpoint,
        cx: &mut Context<Self>,
    ) {
        self.attach_client(Client::start_future_attempts_for_validation(endpoint), cx);
    }
    #[cfg(all(feature = "test-support", feature = "daemon-tests"))]
    pub fn reconnect_future_attempts_for_validation(&mut self) -> Result<(), &'static str> {
        let client = self.client.as_ref().ok_or("No validation client")?;
        if !client.future_attempt_validation() {
            return Err("Not a future-attempt validation client");
        }
        client.send(ClientCommand::Reconnect)
    }

    #[cfg(feature = "test-support")]
    pub fn connect_recording_daemon(&mut self, cx: &mut Context<Self>) {
        self.attach_client(Client::recording(), cx);
    }

    #[cfg(feature = "test-support")]
    pub fn connect_recording_future_attempts(&mut self, cx: &mut Context<Self>) {
        self.attach_client(Client::recording_future_attempts(), cx);
    }
    #[cfg(feature = "test-support")]
    pub fn take_client_commands(&mut self) -> Vec<ClientCommand> {
        self.client
            .as_mut()
            .map(Client::take_recorded)
            .unwrap_or_default()
    }

    pub fn client_epoch(&self) -> Option<u64> {
        self.client.as_ref().map(Client::epoch)
    }

    fn attach_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.retire_stream_view(cx);
        self.stream_view_latest = None;
        self.retire_invocations(
            "Profile attachment changed. Read Invocations again; no read was replayed.",
        );
        self.fork_review = None;
        self.fork_observed = crate::session_fork::Status::Idle;
        self.fork_local_error = None;
        self.reading_positions.clear();
        self.reading_restore = None;
        self.reading_notice = None;
        self.reset_run_inspection("Profile attachment changed. Select a Run in Metrics again; no inspection was replayed.");
        self.retire_session_editor_attachment(client.instance(), client.connection());
        self.reset_comparison(
            "Profile attachment changed. Mark a pair again; nothing was replayed.",
        );
        self.profile_search
            .retire("Profile attachment changed. Search again after connecting.");
        self.session_usage
            .retire("Profile attachment changed. Read usage after selecting a Session.");
        if let Some(previous) = &self.client {
            previous.cancel_unsent_step();
        }
        if self.step_panel.is_some() {
            self.restore_focus = None;
        }
        self.step_panel = None;
        self.step_draft = None;
        if self.approval_panel.is_some() {
            self.restore_focus = None;
        }
        self.approval_panel = None;
        self.client = Some(client);
        self.mode_observed = Default::default();
        self.client_connected = false;
        self.client_snapshot_ready = false;
        self.client_status = "Connecting to the local Daemon Core…".into();
        self.client_attention = false;
        self.client_questions = crate::questions::State::default();
        self.client_recovery = None;
        self.client_steps = Default::default();
        self.client_step_mode = None;
        self.forecast = None;
        self.invalidate_source_view();
        self.transcript_source = TranscriptSource::NotLoaded;
        self.transcript_state = None;
        self.sidebar.replace(&[], &HashSet::new(), false);
        self.session_sizes = Self::row_sizes(0, self.text_scale);
        self.directory = Directory::default();
        self.directory_rows.clear();
        self.include_archived = false;
        self.directory.rebuild(&self.directory_rows, "");
        self.client_sessions.clear();
        self.resource_pages.clear();
        *self.blocks.borrow_mut() = Timeline::new(Vec::new());
        self.timeline
            .update(cx, |timeline, cx| timeline.reset(0, cx));
        self.invalidate_search(cx);
        cx.notify();
    }

    fn client_request(&mut self, command: ClientCommand, cx: &mut Context<Self>) {
        if let Some(client) = &self.client {
            if let Err(error) = client.send(command) {
                self.client_status = error.into();
                self.composer_notice = Some(format!("{error} Draft kept.").into());
            }
        }
        cx.notify();
    }

    fn invalidate_forecast(&mut self, cx: &mut Context<Self>) {
        if let Some(client) = &self.client {
            client.invalidate_context();
        }
        if self.forecast.take().is_some() {
            self.composer_notice =
                Some("Forecast cancelled. Nothing was started; draft kept.".into());
        }
        cx.notify();
    }

    fn cancel_forecast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_forecast(cx);
        self.settle(window, cx);
    }

    fn preparation_current(
        &self,
        prepared: &forecast::Prepared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.client_connected
            && self.client_snapshot_ready
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.epoch() == prepared.epoch)
            && self.client_generation == prepared.generation
            && self.sidebar.active_id() == Some(prepared.session_id.as_str())
            && self.draft(cx).as_ref() == prepared.draft_basis
            && self.composer.update(cx, |composer, cx| {
                composer.marked_text_range(window, cx).is_none()
            })
            && forecast::validate(&prepared.session_id, &prepared.quote).is_ok()
            && match prepared.label {
                "Resume" => self
                    .client_recovery
                    .as_ref()
                    .is_some_and(|run| run.recoverable()),
                "Send" | "Steer" | "Queue" => true,
                _ => false,
            }
    }

    fn confirm_forecast(&mut self, id: uuid::Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self
            .forecast
            .as_ref()
            .filter(|dialog| dialog.prepared.id == id && !dialog.waiting)
        else {
            return;
        };
        if !self.preparation_current(&dialog.prepared, window, cx) {
            self.cancel_forecast(window, cx);
            return;
        }
        match self
            .client
            .as_ref()
            .expect("current preparation has a client")
            .send(ClientCommand::ConfirmPrepared(id))
        {
            Ok(()) => {
                if let Some(dialog) = &mut self.forecast {
                    dialog.waiting = true;
                }
            }
            Err(error) => {
                self.cancel_forecast(window, cx);
                self.composer_notice =
                    Some(format!("{error} Nothing was started; draft kept.").into());
            }
        }
        cx.notify();
    }

    fn commit_prepared(&mut self, id: uuid::Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let result = self
            .client
            .as_ref()
            .ok_or("Not connected")
            .and_then(|client| client.send(ClientCommand::CommitPrepared(id)));
        let was_modal = self.forecast.take().is_some();
        match result {
            Ok(()) => {
                self.composer_notice = Some("Submitting the reviewed action. Draft kept.".into())
            }
            Err(error) => {
                self.invalidate_forecast(cx);
                self.composer_notice = Some(
                    format!("{error} Nothing was started; draft kept. Try again explicitly.")
                        .into(),
                );
            }
        }
        if was_modal {
            self.settle(window, cx);
        }
        cx.notify();
    }

    fn client_select(&mut self, cx: &mut Context<Self>) {
        self.session_usage
            .retire("Session changed. Read recorded usage explicitly for the open Session.");
        self.invalidate_forecast(cx);
        self.client_snapshot_ready = false;
        self.client_recovery = None;
        self.client_steps = Default::default();
        self.client_step_mode = None;
        self.client_generation = self
            .client_generation
            .checked_add(1)
            .expect("window selection generation exhausted");
        self.client_request(
            ClientCommand::Select {
                session_id: self.sidebar.active_id().map(str::to_string),
                generation: self.client_generation,
            },
            cx,
        );
    }

    pub fn client_generation(&self) -> u64 {
        self.client_generation
    }

    /// Drained on the UI thread, outside render. Selection is checked again
    /// here because a reply can already be queued when navigation happens.
    pub fn poll_client(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_fork_status(cx);
        self.sync_step_status(cx);
        self.sync_mode_status(cx);
        self.sync_approval_status(cx);
        self.validate_stream_view(cx);
        let updates: Vec<_> = self
            .client
            .as_ref()
            .map(|client| client.drain().collect())
            .unwrap_or_default();
        for update in updates {
            self.apply_client_update(update, window, cx);
        }
        self.validate_stream_view(cx);
        if self.client_connected
            && self
                .client
                .as_ref()
                .is_some_and(|client| !client.is_ready())
        {
            self.apply_client_update(ClientUpdate::Connection { ready: false, notice: "Daemon connection lost; Run state unknown. Reconnecting without retrying actions.".into() }, window, cx);
        }
    }

    fn install_client_snapshot(
        &mut self,
        generation: u64,
        mut snapshot: SessionSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.client_generation
            || self.sidebar.active_id() != Some(snapshot.session_id.as_str())
            || !self.client_connected
        {
            return;
        }
        self.reading_notice = None;
        let state = if snapshot.running {
            RunState::Running
        } else if snapshot
            .unfinished_run
            .as_ref()
            .is_some_and(|run| run.recoverable())
        {
            RunState::Paused
        } else {
            self.client_sessions
                .iter()
                .find(|s| s.id == snapshot.session_id)
                .map_or(RunState::Idle, |s| RunState::of(s, &HashSet::new(), true))
        };
        self.sidebar.set_run_state(&snapshot.session_id, state);
        if let Some(row) = self
            .directory_rows
            .iter_mut()
            .find(|row| row.id == snapshot.session_id)
        {
            row.run_state = state;
        }
        if snapshot.unfinished_run.is_none()
            && (self.resume_focus.is_focused(window) || self.abandon_focus.is_focused(window))
        {
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
        }
        self.client_steps = snapshot.steps.clone();
        self.client_step_mode = snapshot.step_mode;
        self.client_recovery = snapshot.unfinished_run.take();
        if !self.client_snapshot_ready {
            self.client_status =
                "Connected · canonical messages · live token streaming unavailable".into();
        }
        self.client_snapshot_ready = true;
        let prefix = self
            .blocks
            .borrow()
            .blocks()
            .iter()
            .zip(&snapshot.blocks)
            .take_while(|(old, new)| old == new)
            .count();
        // A replacement overlapping the visible row cannot retain
        // a positional list anchor. Preserve its persisted identity.
        let reading = (self.reading_restore.is_none()
            && prefix <= self.timeline.read(cx).logical_scroll_top().item_ix)
            .then(|| self.current_reading_message(cx))
            .flatten();
        let old_count = self.blocks.borrow().len();
        let count = snapshot.blocks.len();
        // Every accepted canonical baseline establishes fresh reducer state,
        // even when its visible rows are identical. Retirement/gap latches,
        // prior Run ownership and hidden-message sequence changes cannot be
        // inferred from row equality. Keep row/scroll reuse independent.
        self.transcript_state = Some(TranscriptState::new(
            snapshot.session_id.clone(),
            snapshot.last_seq,
        ));
        if prefix != old_count
            || prefix != count
            || self.transcript_source != TranscriptSource::Snapshot
        {
            self.invalidate_source_view();
            let (_, blocks) = Transcript::new(snapshot).into_parts();
            self.transcript_source = TranscriptSource::Snapshot;
            *self.blocks.borrow_mut() = blocks;
            self.timeline.update(cx, |timeline, cx| {
                let known = timeline.item_count();
                let prefix = prefix.min(known);
                timeline.splice(prefix..known, count - prefix, cx);
            });
            self.invalidate_search(cx);
            if let Some(message) = reading {
                self.scroll_to_reading_message(message, cx);
            }
        }
        self.restore_reading_position(cx);
    }

    pub fn apply_client_update(
        &mut self,
        update: ClientUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match update {
            ClientUpdate::ApprovalInventory {
                client,
                connection,
                operation,
                revision,
                result,
            } => self.approval_inventory(client, connection, operation, revision, result, cx),
            ClientUpdate::ApprovalInvalidated {
                client,
                connection,
                revision,
            } => self.approval_invalidated(client, connection, revision, cx),
            ClientUpdate::Transcript(batch) => self.apply_stream_batch(batch, window, cx),
            ClientUpdate::Connection { ready, notice } => {
                self.client_connected = ready;
                self.client_status = notice;
                if ready {
                    self.questions_restored(window, cx);
                }
                if !ready {
                    self.client_steps = Default::default();
                    self.client_step_mode = None;
                    self.retire_stream_view(cx);
                    self.retire_invocations("Connection lost. Read Invocations explicitly after reconnecting; no read is replayed.");
                    self.reset_run_inspection("Connection lost. Select a Run in Metrics after reconnecting; no inspection is replayed.");
                    self.reset_comparison("Connection lost. Mark a pair again after reconnecting; no comparison is replayed.");
                    self.session_usage.retire("Connection lost. Read usage explicitly after reconnecting; nothing was replayed.");
                    if let Some(panel) = &mut self.session_editor {
                        panel.export.retire("Connection lost. Request another export after reconnecting; nothing was replayed.");
                    }
                    self.profile_search.retire(
                        "Connection lost. Results may be stale; search again after connecting.",
                    );
                    self.questions_disconnected(cx);
                    self.forecast = None;
                    if self.modal == Some(Modal::Forecast) {
                        self.settle(window, cx);
                    }
                    self.client_snapshot_ready = false;
                    self.sidebar
                        .replace(&self.client_sessions, &HashSet::new(), false);
                    self.rebuild_directory(cx);
                }
            }
            ClientUpdate::Invocations {
                connection,
                operation,
                result,
            } => self.invocations_outcome(connection, operation, result, cx),
            ClientUpdate::RunInspection {
                connection,
                operation,
                target,
                result,
            } => self.run_inspection_outcome(connection, operation, &target, result, cx),
            ClientUpdate::SessionComparison {
                connection,
                operation,
                pair,
                result,
            } => self.comparison_outcome(connection, operation, &pair, result, cx),
            ClientUpdate::SessionUsage {
                target,
                operation,
                result,
            } => self.receive_session_usage(target, operation, result, cx),
            ClientUpdate::SessionExport {
                connection,
                operation,
                session_id,
                format,
                result,
            } => {
                self.session_export_outcome(connection, operation, &session_id, format, result, cx);
            }
            ClientUpdate::ProfileSearch {
                connection,
                operation,
                query,
                result,
            } => {
                if self.client_connected
                    && self
                        .client
                        .as_ref()
                        .is_some_and(|client| client.connection() == connection)
                {
                    self.profile_search
                        .changed(&self.profile_query.read(cx).value());
                    self.profile_search
                        .receive(connection, operation, &query, result);
                }
            }
            ClientUpdate::Questions(state) => {
                if self
                    .client
                    .as_ref()
                    .is_some_and(|client| client.connection() == state.connection)
                    && self.client_questions != state
                {
                    self.client_questions = state;
                    self.questions_restored(window, cx);
                }
            }
            ClientUpdate::QuestionAnswer {
                connection,
                operation,
                question_id,
                outcome,
            } => self.question_outcome(connection, operation, &question_id, outcome, cx),
            ClientUpdate::SessionEdited {
                connection,
                operation,
                session_id,
                outcome,
            } => self.session_edit_outcome(connection, operation, &session_id, outcome, window, cx),
            ClientUpdate::PendingAttachments {
                client,
                connection,
                operation,
                session_id,
                result,
            } => {
                self.pending_files_received(client, connection, operation, &session_id, result, cx)
            }
            ClientUpdate::ModelCatalog {
                instance,
                connection,
                operation,
                result,
            } => {
                self.model_catalog_received(instance, connection, operation, result, cx);
            }
            ClientUpdate::SessionDetails {
                connection,
                operation,
                session_id,
                result,
            } => self.session_details_received(
                connection,
                operation,
                &session_id,
                result,
                window,
                cx,
            ),
            ClientUpdate::Sessions(sessions) => {
                let previous = self.sidebar.active_id().map(str::to_string);
                let previous_state = self.run_state();
                let vanished = previous.as_ref().is_some_and(|id| {
                    !sessions
                        .iter()
                        .any(|session| &session.id == id && !session.archived)
                });
                if vanished {
                    if let Some(id) = previous.as_deref() {
                        self.park_reading_position(id, cx);
                    }
                    let evicted = previous
                        .as_ref()
                        .map(|id| self.drafts.set(id, &self.accepted_draft))
                        .unwrap_or_default();
                    self.rebuild_composer("", None, window, cx);
                    self.composer_notice = Some(format!("The open Session is no longer available. Its unsent draft was kept for this window. {} older drafts were evicted by the draft retention limit.", evicted.len()).into());
                }
                self.client_sessions = sessions;
                self.reading_positions.retain_available(
                    self.client_sessions
                        .iter()
                        .map(|session| session.id.as_str()),
                );

                let missing = self
                    .comparison
                    .left
                    .as_ref()
                    .is_some_and(|id| !self.client_sessions.iter().any(|s| &s.id == id))
                    || self.comparison.pair.as_ref().is_some_and(|pair| {
                        [&pair.left, &pair.right]
                            .iter()
                            .any(|id| !self.client_sessions.iter().any(|s| &s.id == *id))
                    });
                if missing {
                    self.reset_comparison(
                        "A compared Session is no longer listed. Mark a pair again.",
                    );
                }
                self.sidebar
                    .replace(&self.client_sessions, &HashSet::new(), false);
                if let Some(id) = &previous {
                    self.sidebar.set_run_state(id, previous_state);
                }
                self.session_sizes = Self::row_sizes(self.sidebar.rows().len(), self.text_scale);
                self.rebuild_directory(cx);
                if vanished {
                    self.reading_restore = None;
                    self.reading_notice = None;
                    self.invalidate_source_view();
                    self.transcript_state = None;
                    self.transcript_source = TranscriptSource::NotLoaded;
                    *self.blocks.borrow_mut() = Timeline::new(Vec::new());
                    self.timeline
                        .update(cx, |timeline, cx| timeline.reset(0, cx));
                    self.client_select(cx);
                    self.invalidate_search(cx);
                }
            }
            ClientUpdate::SessionStatusInvalidated {
                client,
                connection,
                generation,
                session_id,
            } => {
                if generation != self.client_generation
                    || self.sidebar.active_id() != Some(session_id.as_str())
                    || !self.client.as_ref().is_some_and(|current| {
                        current.instance() == client && current.connection() == connection
                    })
                {
                    return;
                }
                self.client_snapshot_ready = false;
                self.client_recovery = None;
                self.client_steps = Default::default();
                self.client_step_mode = None;
                self.sidebar.set_run_state(&session_id, RunState::Unknown);
                if let Some(row) = self
                    .directory_rows
                    .iter_mut()
                    .find(|row| row.id == session_id)
                {
                    row.run_state = RunState::Unknown;
                }
                self.client_status = "Session state changed · refreshing controls".into();
                self.invalidate_forecast(cx);
                if self.resume_focus.is_focused(window) || self.abandon_focus.is_focused(window) {
                    self.composer
                        .update(cx, |composer, cx| composer.focus(window, cx));
                }
                self.settle(window, cx);
            }
            ClientUpdate::Snapshot {
                generation,
                snapshot,
            } => {
                if generation == self.client_generation
                    && self.client_connected
                    && self.sidebar.active_id() == Some(snapshot.session_id.as_str())
                {
                    self.stream_view_owner = None;
                    self.install_client_snapshot(generation, snapshot, window, cx);
                }
            }
            ClientUpdate::SnapshotError {
                generation,
                session_id,
                error,
            } => {
                if generation != self.client_generation
                    || self.sidebar.active_id() != Some(session_id.as_str())
                {
                    return;
                }
                self.client_snapshot_ready = false;
                self.client_steps = Default::default();
                self.client_step_mode = None;
                self.sidebar.set_run_state(&session_id, RunState::Unknown);
                if let Some(row) = self
                    .directory_rows
                    .iter_mut()
                    .find(|row| row.id == session_id)
                {
                    row.run_state = RunState::Unknown;
                }
                self.client_status = format!("Could not refresh this Session: {error}. The displayed transcript may be stale; sending is disabled until it reloads.");
            }
            ClientUpdate::Page {
                destination,
                result,
            } => {
                if matches!(
                    destination,
                    Destination::Comparison | Destination::RunDetails | Destination::Invocations
                ) {
                    return;
                }

                let mut page =
                    match result.and_then(|value| crate::pages::Page::decode(destination, value)) {
                        Ok(page) => page,
                        Err(error) => crate::pages::Page {
                            notice: format!(
                                "Could not load {}: {error}. Refresh to retry.",
                                destination.label()
                            ),
                            ..Default::default()
                        },
                    };
                self.resource_revision = self
                    .resource_revision
                    .checked_add(1)
                    .expect("resource revision exhausted");
                page.revision = self.resource_revision;
                self.resource_pages.insert(destination, page);
            }
            ClientUpdate::Action { session_id, result } => {
                let notice = match result { Ok(()) => "Daemon acknowledged the action. Draft kept; edit or clear it before the next message.".to_string(), Err(error) => format!("Daemon rejected the action: {error}. Draft kept.") };
                if self.sidebar.active_id() == Some(session_id.as_str()) {
                    self.composer_notice = Some(notice.clone().into());
                }
                self.client_status = notice;
            }
            ClientUpdate::Attention {
                generation,
                pending,
            } => {
                if generation == self.client_generation {
                    self.client_attention = pending;
                }
            }
            ClientUpdate::Notice(notice) => self.client_status = notice,
            ClientUpdate::Prepared(prepared) => {
                // A retired queued update must not cancel a newer forecast.
                if self.client_epoch() != Some(prepared.epoch)
                    || self.client_generation != prepared.generation
                    || self.sidebar.active_id() != Some(prepared.session_id.as_str())
                    || !self.client_connected
                {
                    return;
                }
                if self
                    .forecast
                    .as_ref()
                    .is_some_and(|dialog| dialog.prepared.id == prepared.id)
                {
                    return;
                }
                if !self.preparation_current(&prepared, window, cx) {
                    self.invalidate_forecast(cx);
                    self.composer_notice = Some("The action changed while its forecast loaded. Nothing was started; draft kept.".into());
                    return;
                }
                if !prepared.quote.confirmation_required {
                    self.commit_prepared(prepared.id, window, cx);
                } else {
                    self.palette = None;
                    self.viewer = None;
                    self.prefs.close_overlays();
                    let summary = forecast::summary(&prepared.quote);
                    let details = serde_json::to_string_pretty(&prepared.quote)
                        .expect("forecast is serializable");
                    self.forecast = Some(ForecastDialog {
                        prepared,
                        summary,
                        details,
                        waiting: false,
                    });
                    self.forecast_scroll.scroll_to_item(0);
                    self.settle(window, cx);
                }
            }
            ClientUpdate::PreparationReady { id, epoch } => {
                if let Some(dialog) = self
                    .forecast
                    .as_ref()
                    .filter(|dialog| dialog.prepared.id == id && dialog.prepared.epoch == epoch)
                {
                    if self.preparation_current(&dialog.prepared, window, cx) {
                        self.commit_prepared(id, window, cx);
                    } else {
                        self.cancel_forecast(window, cx);
                    }
                }
            }
            ClientUpdate::PreparationFailed { id, epoch, error } => {
                if self
                    .client
                    .as_ref()
                    .is_some_and(|client| client.epoch() == epoch)
                {
                    if self
                        .forecast
                        .as_ref()
                        .is_some_and(|dialog| dialog.prepared.id == id)
                    {
                        self.forecast = None;
                        self.settle(window, cx);
                    }
                    self.composer_notice = Some(
                        format!("Forecast unavailable: {error}. Nothing was started; draft kept.")
                            .into(),
                    );
                }
            }
        }
        cx.notify();
    }

    /// Move the window a row shows onto another part of its block. The row
    /// changes height, so it is measured again.
    pub fn move_block_window(&mut self, block_id: u64, to: WindowMove, cx: &mut Context<Self>) {
        let Some(index) = self.blocks.borrow_mut().move_window(block_id, to) else {
            return;
        };
        self.timeline.update(cx, |timeline, cx| {
            timeline.remeasure_items(index..index + 1, cx);
        });
        cx.notify();
    }

    /// Show a decoded `GetSession` reply ([`crate::replies`]) as the open
    /// Session's timeline, in place of what it showed, a stream in progress
    /// included. This is a low-level fixture/view seam, not a client reply
    /// handler: it checks the Session id, not request age or event ordering.
    /// A future client must establish those before calling it. The unwired
    /// `snapshot_guard` provides local ownership/overlap checks only.
    ///
    /// A snapshot for any Session but the open one is refused and nothing
    /// changes, so a late reply never puts one Session's transcript under
    /// another's name. Run state is not taken from it: the Session list and
    /// Run events own that (findings §6).
    pub fn show_session_snapshot(
        &mut self,
        snapshot: SessionSnapshot,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        if self.sidebar.active_id() != Some(snapshot.session_id.as_str()) {
            return Err("the snapshot is not for the open Session");
        }
        self.stream_view_owner = None;
        self.reading_notice = None;
        self.invalidate_source_view();
        self.transcript_source = TranscriptSource::Snapshot;
        let (state, blocks) = Transcript::new(snapshot).into_parts();
        self.transcript_state = Some(state);
        let count = blocks.len();
        *self.blocks.borrow_mut() = blocks;
        self.timeline
            .update(cx, |timeline, cx| timeline.reset(count, cx));
        self.restore_reading_position(cx);
        self.invalidate_search(cx);
        cx.notify();
        Ok(())
    }

    /// Byte offsets and row restore handles belong to the exact source.
    /// The next render settles any dismissed modal to a visible fallback.
    fn invalidate_source_view(&mut self) {
        let had_viewer = self.viewer.take().is_some();
        let row_origin = self
            .restore_focus
            .as_ref()
            .is_some_and(|origin| origin.region == Some(Region::Conversation));
        if had_viewer || row_origin {
            self.restore_focus = None;
        }
    }

    pub fn transcript_reload_needed(&self) -> Option<&ReloadReason> {
        self.transcript_state
            .as_ref()
            .and_then(TranscriptState::reload_needed)
    }

    /// Trusted, in-memory view seam after a decoded snapshot, not a socket
    /// callback. The future client must validate attachment/request ownership
    /// and order events + snapshot installation before calling these seams.
    /// This applies transcript text only, never Run/composer/approval state.
    pub fn apply_transcript_event(
        &mut self,
        event: &Event,
        cx: &mut Context<Self>,
    ) -> Result<TranscriptChange, &'static str> {
        let (change, was_streaming, is_streaming, unchanged_prefix) = {
            let state = self
                .transcript_state
                .as_mut()
                .ok_or("no decoded transcript is loaded for event application")?;
            if self.sidebar.active_id() != Some(state.session_id()) {
                return Err("the loaded transcript is not for the open Session");
            }
            let mut blocks = self.blocks.borrow_mut();
            let was_streaming = blocks.is_streaming();
            // The reducer only appends persisted messages or replaces /
            // discards the provisional last row. Earlier rows survive.
            let unchanged_prefix = blocks.len().saturating_sub(usize::from(was_streaming));
            let change = state.apply(&mut blocks, event);
            (
                change,
                was_streaming,
                blocks.is_streaming(),
                unchanged_prefix,
            )
        };
        if change.reload_changed {
            self.invalidate_source_view();
        }
        if change.timeline_changed {
            if matches!(event, Event::MessageDelta { .. }) && !change.reload_changed {
                // Only the excluded tail changed. Refresh Search on its
                // first appearance, not once per token/chunk.
                if was_streaming != is_streaming {
                    self.invalidate_search(cx);
                }
                self.request_flush(cx);
            } else {
                // Tools can be inserted before the provisional tail, and
                // canonical completion can remove it. Splice the changed
                // suffix, preserving an earlier reading anchor. A full
                // reset would incorrectly force tail-following.
                self.invalidate_source_view();
                let count = self.blocks.borrow().len();
                self.timeline.update(cx, |timeline, cx| {
                    // A coalesced first delta may not have reached the list
                    // yet, so its known prefix can be shorter than the model.
                    let known = timeline.item_count();
                    let prefix = unchanged_prefix.min(known);
                    timeline.splice(prefix..known, count - prefix, cx);
                });
                self.invalidate_search(cx);
                cx.notify();
            }
        } else if change.reload_changed {
            cx.notify();
        }
        Ok(change)
    }

    /// Open the full-output view of a block: all of it, as plain text in
    /// rows of the view's width, in a list that lays out only the rows on
    /// screen. Not for a block still being streamed. Focus goes back where
    /// it was when the view closes.
    pub fn view_block(
        &mut self,
        block_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let layout = self.viewer_layout(window, cx);
        let (starts, error) = {
            let blocks = self.blocks.borrow();
            let index = blocks
                .blocks()
                .iter()
                .position(|block| block.id == block_id)
                .ok_or("there is no such block")?;
            if blocks.is_streaming() && index + 1 == blocks.len() {
                return Err(VIEWER_STREAMING);
            }
            Self::measure_viewer(blocks.blocks()[index].markdown(), &layout, cx)
        };
        self.prefs.close_overlays();
        self.viewer = Some(Viewer {
            block_id,
            starts,
            layout,
            error,
            summary_scroll: ScrollHandle::new(),
            summary_focus: cx.focus_handle().tab_stop(true),
            scroll: gpui_kit::UniformListScrollHandle::new(),
            copied: false,
        });
        self.settle(window, cx);
        Ok(())
    }

    fn close_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.viewer.take().is_some() {
            self.settle(window, cx);
        }
    }

    /// The full-output view's dialog size: the window less a margin.
    fn viewer_size(window: &Window) -> Size<Pixels> {
        let viewport = window.viewport_size();
        size(viewport.width - px(32.), viewport.height - px(32.))
    }

    fn viewer_layout(&self, window: &Window, cx: &App) -> ViewerLayout {
        let mut font = window.text_style().font();
        font.family = cx.theme().mono_font_family.clone();
        ViewerLayout {
            // Padding, border, scrollbar and a little glyph-overhang room.
            width: (Self::viewer_size(window).width - px(48.)).max(px(1.)),
            font_size: cx.theme().mono_font_size,
            font,
        }
    }

    fn measure_viewer(
        text: &str,
        layout: &ViewerLayout,
        cx: &App,
    ) -> (Box<[u32]>, Option<full_view::RowError>) {
        let mut measurer = gpui_kit::WindowTextSystem::new(cx.text_system().clone());
        let font_id = measurer.resolve_font(&layout.font);
        let advance = measurer
            .ch_advance(font_id, layout.font_size)
            .unwrap_or(layout.font_size * 0.6);
        let columns = (f32::from(layout.width) / f32::from(advance)).floor() as usize;
        let mut run = gpui_kit::TextRun {
            font: layout.font.clone(),
            ..Default::default()
        };
        let mut queries = 0;
        // Borrowed keys, no second copy of transcript text. Repeated rows
        // (e.g. a 5 MiB line of x) need only one font measurement.
        let mut widths = HashMap::new();
        let result = full_view::measured_row_starts(text, columns, |row| {
            if let Some(fits) = widths.get(row) {
                return *fits;
            }
            if queries == VIEWER_MEASURE_BATCH {
                measurer = gpui_kit::WindowTextSystem::new(cx.text_system().clone());
                queries = 0;
            }
            run.len = row.len();
            let fits = measurer
                .shape_line(
                    row.to_string().into(),
                    layout.font_size,
                    &[run.clone()],
                    None,
                )
                .width()
                <= layout.width;
            queries += 1;
            if widths.len() < VIEWER_WIDTH_CACHE {
                widths.insert(row, fits);
            }
            fits
        });
        match result {
            Ok(starts) => (starts, None),
            Err(error) => (Box::default(), Some(error)),
        }
    }

    /// Cut the open view's block again when the width it fits has changed
    /// (a resize, a text size change), keeping the row at the top in view.
    fn fit_viewer(&mut self, window: &Window, cx: &App) {
        let layout = self.viewer_layout(window, cx);
        let blocks = self.blocks.borrow();
        let Some(viewer) = &mut self.viewer else {
            return;
        };
        if viewer.layout == layout {
            return;
        }
        let Some(block) = blocks.block(viewer.block_id) else {
            return;
        };
        let (starts, error) = Self::measure_viewer(block.markdown(), &layout, cx);
        let (top, _) = viewer.top_and_page();
        let top = viewer.starts.get(top).copied().unwrap_or(0);
        let row = starts
            .partition_point(|start| *start <= top)
            .saturating_sub(1);
        viewer.starts = starts;
        viewer.layout = layout;
        viewer.error = error;
        viewer.summary_scroll.set_offset(point(px(0.), px(0.)));
        if !viewer.starts.is_empty() {
            viewer
                .scroll
                .scroll_to_item_strict(row, ScrollStrategy::Top);
        }
    }

    /// Move the view by `rows` rows, a page (`rows` 0), or to an end.
    fn scroll_viewer(
        &mut self,
        to: WindowMove,
        rows: isize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = &self.viewer else {
            return;
        };
        if viewer.summary_focus.is_focused(window) || viewer.starts.is_empty() {
            let scroll = &viewer.summary_scroll;
            let line = cx.theme().font_size * 1.5;
            let step = if rows == 0 {
                (scroll.bounds().size.height - line).max(line)
            } else {
                line * rows.unsigned_abs() as f32
            };
            let max = scroll.max_offset().y;
            let y = match to {
                WindowMove::Start => px(0.),
                WindowMove::End => -max,
                WindowMove::Earlier => (scroll.offset().y + step).min(px(0.)),
                WindowMove::Later => (scroll.offset().y - step).max(-max),
            };
            scroll.set_offset(point(px(0.), y));
            cx.notify();
            return;
        }
        let last = viewer.starts.len() - 1;
        let (top, page) = viewer.top_and_page();
        let target = match to {
            WindowMove::Start => 0,
            WindowMove::End => last,
            WindowMove::Earlier | WindowMove::Later => {
                // A page keeps one row of the last page in view.
                let step = if rows == 0 {
                    (page as isize - 1).max(1)
                } else {
                    rows
                };
                let step = if to == WindowMove::Earlier {
                    -step
                } else {
                    step
                };
                (top as isize + step).clamp(0, last as isize) as usize
            }
        };
        viewer
            .scroll
            .scroll_to_item_strict(target, ScrollStrategy::Top);
        cx.notify();
    }

    /// The finished-corpus revision captured by a rendered Copy source control.
    /// Session/snapshot replacement and structural transcript changes retire it.
    pub fn block_copy_revision(&self) -> u64 {
        self.search_revision
    }

    /// Copy exact source, never rendered Markdown or a partial row preview.
    /// Recheck ownership and modal state at dispatch time before touching the
    /// clipboard. This local action does not send an intent to the daemon.
    pub fn copy_block_source(
        &mut self,
        block_id: u64,
        revision: u64,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if revision != self.search_revision
            || self.destination != Destination::Chat
            || self.background_inert(window)
            || self.transcript_reload_needed().is_some()
        {
            return false;
        }
        let blocks = self.blocks.borrow();
        let Some(index) = blocks
            .blocks()
            .iter()
            .position(|block| block.id == block_id)
        else {
            return false;
        };
        let block = &blocks.blocks()[index];
        if (blocks.is_streaming() && index + 1 == blocks.len())
            || block.total_bytes() > BLOCK_COPY_MAX_BYTES
        {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(block.markdown().to_string()));
        self.block_copy_notice = Some("Copied complete block source to the system clipboard.");
        cx.notify();
        true
    }

    fn copy_viewed_block(&mut self, cx: &mut Context<Self>) {
        let Some(viewer) = &mut self.viewer else {
            return;
        };
        let blocks = self.blocks.borrow();
        if let Some(block) = blocks.block(viewer.block_id) {
            cx.write_to_clipboard(ClipboardItem::new_string(block.markdown().to_string()));
            viewer.copied = true;
            viewer.summary_scroll.set_offset(point(px(0.), px(0.)));
        }
        cx.notify();
    }

    /// How many rows the open full-output view has, for tests.
    pub fn viewer_rows(&self) -> Option<usize> {
        self.viewer.as_ref().map(|viewer| viewer.starts.len())
    }

    /// Fixture-only synthetic stream. Decoded transcripts must use typed
    /// events so their sequence/Run metadata cannot be bypassed.
    pub fn begin_stream(&mut self, cx: &mut Context<Self>) {
        if self.transcript_source != TranscriptSource::Fixture {
            return;
        }
        self.blocks.borrow_mut().begin_stream(BlockKind::Assistant);
        self.invalidate_search(cx);
        self.request_flush(cx);
    }

    /// Append streamed text to the tail. The text is kept at once; the view
    /// is told at most once per flush interval, and the row is rebuilt only
    /// when it is next shown.
    pub fn push_stream_delta(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.transcript_source != TranscriptSource::Fixture {
            return;
        }
        if self.blocks.borrow_mut().push_delta(text) {
            self.request_flush(cx);
        }
    }

    /// End the stream and show its final state without waiting.
    pub fn finish_stream(&mut self, cx: &mut Context<Self>) {
        if self.transcript_source != TranscriptSource::Fixture {
            return;
        }
        self.blocks.borrow_mut().finish_stream();
        self.invalidate_search(cx);
        self.stream.request();
        self.flush_stream(cx);
    }

    fn request_flush(&mut self, cx: &mut Context<Self>) {
        if !self.stream.request() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(FLUSH_INTERVAL_MS))
                .await;
            this.update(cx, |this, cx| this.flush_stream(cx)).ok();
        })
        .detach();
    }

    /// Bring the list in line with the data: new rows are appended and the
    /// tail row, the only one that can have changed, is measured again.
    fn flush_stream(&mut self, cx: &mut Context<Self>) {
        if !self.stream.take() {
            return;
        }
        // The tail row is measured below, so it has to be up to date first.
        let count = self.shown_blocks().len();
        self.timeline.update(cx, |timeline, cx| {
            let known = timeline.item_count();
            if count > known {
                timeline.append(count - known, cx);
            }
            if count > 0 {
                timeline.remeasure_items(count - 1..count, cx);
            }
        });
        cx.notify();
    }

    pub fn draft(&self, cx: &App) -> SharedString {
        self.composer.read(cx).value()
    }

    pub fn layout(&self, window: &Window) -> Layout {
        Layout::for_width(self.effective_width(window), self.prefs)
            .for_destination(self.destination)
    }

    fn run_state(&self) -> RunState {
        if self.client.is_some() && (!self.client_connected || !self.client_snapshot_ready) {
            return RunState::Unknown;
        }
        self.sidebar
            .active_row()
            .map_or(RunState::Idle, |row| row.run_state)
    }

    fn composer_actions(&self, cx: &App) -> ComposerActions {
        let mut actions =
            ComposerActions::for_state(self.run_state(), self.draft(cx).trim().is_empty());
        if self.client.is_some() && self.client_connected && !self.client_snapshot_ready {
            actions.disabled_reason = Some("Waiting for a current Session snapshot");
        }
        actions
    }

    /// Record what the composer would ask for. Without a client layer the
    /// request is never acknowledged, so the draft is deliberately kept.
    fn submit(&mut self, secondary: bool, cx: &mut Context<Self>) {
        let actions = self.composer_actions(cx);
        if actions.disabled_reason.is_some() {
            return;
        }
        // Only an unfinished composition can leave the composer over its
        // limit; whatever the reason, over-limit text is never handed on.
        if !within_composer_limit(&self.draft(cx)) {
            self.composer_notice = Some(
                format!(
                    "Not sent: the message is over the {} limit.",
                    format_bytes(limits::COMPOSER_MAX_BYTES)
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let mode = match (secondary, actions.secondary) {
            (true, Some(mode)) => mode,
            (true, None) => return,
            (false, _) => actions.primary,
        };
        let Some(session_id) = self.sidebar.active_id().map(str::to_string) else {
            return;
        };
        let content = self.draft(cx).to_string();
        self.emit(
            Intent::SendMessage {
                session_id,
                content,
                mode,
            },
            cx,
        );
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.composer_actions(cx).stop_enabled {
            return;
        }
        if let Some(session_id) = self.sidebar.active_id().map(str::to_string) {
            self.emit(Intent::InterruptRun { session_id }, cx);
        }
    }

    /// What the commands depend on now.
    pub fn command_state(&self, window: &mut Window, cx: &mut Context<Self>) -> commands::State {
        let layout = self.layout(window);
        let composing = self.composer.update(cx, |composer, cx| {
            composer.marked_text_range(window, cx).is_some()
        });
        commands::State {
            composer: self.composer_actions(cx),
            has_composer: layout.composer,
            can_recover: self.client_connected
                && self.client_snapshot_ready
                && self
                    .client_recovery
                    .as_ref()
                    .is_some_and(|run| run.recoverable()),
            can_questions: self.client.is_some(),
            can_approvals: self.client_connected
                && self.client.as_ref().is_some_and(Client::is_ready),
            can_search_profile: self.client.is_some(),
            can_edit_session: self.client_connected && self.session_edit_target().is_some(),
            can_browse_archived: self.client.is_some(),
            archived_included: self.include_archived,
            composing,
            enter_sends: self.enter_sends,
            can_preferences: self.preferences.is_some(),
            can_mark_comparison: self.comparison_selected_id().is_some()
                && self.comparison_connected(),
            comparison_reason: self.comparison_disabled_reason(),
            run_inspection_reason: self.run_inspection_disabled_reason(),
            fork_reason: self.fork_disabled_reason(),
            invocations_reason: self.invocations_disabled_reason(),
            invocation_open_reason: self.invocation_open_reason(),
            sessions_shown: layout.sidebar != SidebarMode::Collapsed,
            context_shown: layout.inspector != InspectorMode::Hidden,
            dark: cx.theme().is_dark(),
            text_scale: self.text_scale,
            destination: self.destination,
            palette_open: self.palette.is_some(),
        }
    }

    /// Run a command if it is available now; otherwise return why not.
    /// Every button, shortcut and palette row runs its command through
    /// here, so availability is judged in one place, at the moment of
    /// running, not from what was last drawn.
    pub fn run_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<&'static str> {
        if self.session_editor.is_some()
            && !matches!(
                command,
                Command::FocusNextRegion
                    | Command::FocusPreviousRegion
                    | Command::IncreaseTextSize
                    | Command::DecreaseTextSize
                    | Command::ResetTextSize
                    | Command::ToggleTheme
            )
        {
            return Some("Close Session details before changing the underlying view");
        }
        if self.approval_panel.is_some()
            && !matches!(
                command,
                Command::FocusNextRegion
                    | Command::FocusPreviousRegion
                    | Command::IncreaseTextSize
                    | Command::DecreaseTextSize
                    | Command::ResetTextSize
                    | Command::ToggleTheme
            )
        {
            return Some("Close approval review before changing the underlying view");
        }
        if self.step_panel.is_some()
            && !matches!(
                command,
                Command::FocusNextRegion
                    | Command::FocusPreviousRegion
                    | Command::IncreaseTextSize
                    | Command::DecreaseTextSize
                    | Command::ResetTextSize
                    | Command::ToggleTheme
            )
        {
            return Some("Close Step review before changing the underlying view");
        }
        if self.question_panel.is_some()
            && !matches!(
                command,
                Command::Questions
                    | Command::FocusNextRegion
                    | Command::FocusPreviousRegion
                    | Command::IncreaseTextSize
                    | Command::DecreaseTextSize
                    | Command::ResetTextSize
                    | Command::ToggleTheme
            )
        {
            if let Some(panel) = &mut self.question_panel {
                panel.notice=Some("Close question review before changing the underlying view. Answers will be kept.".into());
            }
            cx.notify();
            return Some("Close question review first");
        }
        if self.forecast.is_some()
            && matches!(
                command,
                Command::Send
                    | Command::SendSecondary
                    | Command::ResumeRun
                    | Command::AbandonRun
                    | Command::Stop
            )
        {
            return Some("Confirm or cancel the open forecast first");
        }
        if matches!(
            command,
            Command::SaveDesktopPreferences | Command::ReloadDesktopPreferences
        ) {
            if self.background_inert(window) {
                return Some(
                    "Close the open dialog before saving or reloading desktop preferences",
                );
            }
            if command == Command::ReloadDesktopPreferences {
                if let Some(query) = self.browse_query(self.destination).cloned() {
                    if query.update(cx, |query, cx| {
                        query.marked_text_range(window, cx).is_some()
                    }) {
                        self.preference_notice =
                            Some("Preferences unchanged: finish composing text first.".into());
                        cx.notify();
                        return Some(commands::COMPOSING);
                    }
                }
            }
        }
        if command == Command::ToggleEnterSends && self.background_inert(window) {
            return Some("Close the open dialog before changing send keys");
        }
        let state = self.command_state(window, cx);
        if let Some(reason) = commands::entry(command, &state).disabled_reason {
            // Other refusals are as silent as they always were, but a
            // composition can start without the shell being redrawn, so a
            // Send button or Enter can still be pressed while it runs: the
            // composer says why nothing was recorded.
            if reason == commands::COMPOSING {
                self.composer_notice = Some(if command == Command::ReloadDesktopPreferences {
                    format!("Preferences unchanged: {reason}.").into()
                } else if command == Command::ToggleEnterSends {
                    format!("Send keys unchanged: {reason}.").into()
                } else {
                    format!("Not sent: {reason}.").into()
                });
                cx.notify();
            }
            return Some(reason);
        }
        match command {
            Command::Questions => self.open_questions(window, cx),
            Command::Approvals => self.open_approvals(window, cx),
            Command::SessionDetails => self.open_session_editor(window, cx),
            Command::ToggleArchived => self.toggle_archived_directory(window, cx),
            Command::Send => self.submit(false, cx),
            Command::SendSecondary => self.submit(true, cx),
            Command::Stop => self.stop(cx),
            Command::ResumeRun | Command::AbandonRun => {
                if let Some(session_id) = self.sidebar.active_id().map(str::to_string) {
                    self.emit(
                        if command == Command::ResumeRun {
                            Intent::ResumeRun { session_id }
                        } else {
                            Intent::AbandonRun { session_id }
                        },
                        cx,
                    );
                }
            }
            Command::ToggleSessions => self.toggle_panel(Region::Sidebar, window, cx),
            Command::ToggleContext => self.toggle_panel(Region::Inspector, window, cx),
            Command::ViewInvocations => self.view_invocations(window, cx),
            Command::OpenInvocationSession => {
                if let Some(source) = self.selected_invocation() {
                    self.open_invocation_session(source, self.invocations.revision, window, cx);
                }
            }
            Command::ReviewFork => self.review_fork(window, cx),
            Command::InspectRun => {
                if let Some((target, source)) = self.selected_run_target() {
                    self.read_run_inspection(
                        target,
                        self.run_inspection.revision,
                        Some(source),
                        window,
                        cx,
                    );
                }
            }
            Command::MarkComparisonLeft => {
                if let Some(id) = self.comparison_selected_id().map(str::to_string) {
                    self.mark_comparison(id, self.comparison.revision, window, cx);
                }
            }
            Command::CompareMarked => {
                if let Some(pair) = self.comparison_selected_pair() {
                    self.read_comparison(pair, self.comparison.revision, window, cx);
                }
            }
            Command::SaveDesktopPreferences => self.save_preferences(cx),
            Command::ReloadDesktopPreferences => self.reload_preferences(window, cx),
            Command::ToggleEnterSends => {
                self.invalidate_forecast(cx);
                self.enter_sends = !self.enter_sends;
                self.composer.update(cx, |composer, cx| {
                    composer.set_submit_on_enter(self.enter_sends, cx)
                });
                cx.notify();
            }
            Command::ToggleTheme => {
                let next = if cx.theme().is_dark() {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                };
                Theme::change(next, Some(window), cx);
            }
            Command::IncreaseTextSize => {
                self.set_text_scale_growing_window(self.text_scale.larger(), window, cx)
            }
            Command::DecreaseTextSize => {
                self.set_text_scale_growing_window(self.text_scale.smaller(), window, cx)
            }
            Command::ResetTextSize => {
                self.set_text_scale_growing_window(TextScale::DEFAULT, window, cx)
            }
            Command::FocusNextRegion => self.cycle_region(true, window, cx),
            Command::FocusPreviousRegion => self.cycle_region(false, window, cx),
            Command::GoTo(destination) => self.set_destination(destination, window, cx),
            Command::ShowCommands => {
                if self.palette.is_some() {
                    self.close_palette(window, cx)
                } else {
                    self.open_palette(window, cx)
                }
            }
        }
        None
    }

    /// Open the command palette over everything, with focus in its search
    /// field. A panel overlay that is open closes: one modal at a time.
    /// Focus goes back to where it was when the palette closes.
    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_forecast(cx);
        if self.palette.is_some() {
            return;
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Type to search commands"));
        let changes = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
            if let (InputEvent::Change, Some(palette)) = (event, &mut this.palette) {
                palette.selected = 0;
                palette.notice = None;
                this.palette_scroll.scroll_to_item(0);
                cx.notify();
            }
        });
        // Replace the full-output modal, as we replace panel overlays:
        // never leave a second dialog behind the command palette.
        self.viewer = None;
        self.prefs.close_overlays();
        self.palette = Some(Palette {
            query,
            selected: 0,
            notice: None,
            _changes: changes,
        });
        self.settle(window, cx);
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.settle(window, cx);
        }
    }

    /// Whether an input method is composing in the palette's search field.
    fn palette_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.palette.as_ref().is_some_and(|palette| {
            palette.query.update(cx, |query, cx| {
                query.marked_text_range(window, cx).is_some()
            })
        })
    }

    /// End a composition in the search field the way the field itself would
    /// (its text is kept). Returns whether there was one, in which case the
    /// key that asked for it does nothing else.
    fn finish_palette_composition(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.palette_composing(window, cx) {
            return false;
        }
        if let Some(palette) = &self.palette {
            palette
                .query
                .update(cx, |query, cx| query.unmark_text(window, cx));
        }
        true
    }

    /// The commands the palette shows now, for its query.
    fn palette_entries(&self, cx: &App) -> Vec<commands::Entry> {
        let Some(palette) = &self.palette else {
            return Vec::new();
        };
        let query = palette.query.read(cx).value();
        commands::palette(&self.commands)
            .into_iter()
            .filter(|entry| entry.matches(&query))
            .collect()
    }

    fn move_palette_selection(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A composing input method may use the arrows itself.
        if self.palette_composing(window, cx) {
            return;
        }
        let count = self.palette_entries(cx).len();
        let Some(palette) = &mut self.palette else {
            return;
        };
        if count == 0 {
            return;
        }
        let last = count as isize - 1;
        palette.selected = (palette.selected as isize + delta).clamp(0, last) as usize;
        palette.notice = None;
        self.palette_scroll.scroll_to_item(palette.selected);
        cx.notify();
    }

    fn run_selected_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finish_palette_composition(window, cx) {
            return;
        }
        self.commands = self.command_state(window, cx);
        let entries = self.palette_entries(cx);
        if let Some(index) = self.palette_choice(entries.len()) {
            self.run_from_palette(entries[index].command, window, cx);
        }
    }

    /// The chosen row among `count`, kept inside the list: the list can get
    /// shorter while the palette is open (Queue goes when a Run ends), and
    /// what is shown, said and run must be the same row.
    fn palette_choice(&self, count: usize) -> Option<usize> {
        let palette = self.palette.as_ref()?;
        (count > 0).then(|| palette.selected.min(count - 1))
    }

    /// Run a command chosen in the palette: the palette closes first, so
    /// focus is back where it was and the command acts there. A command
    /// that cannot run leaves the palette open and says why.
    fn run_from_palette(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let entry = commands::entry(command, &self.command_state(window, cx));
        if let Some(reason) = entry.disabled_reason {
            if let Some(palette) = &mut self.palette {
                palette.notice = Some(format!("{} is unavailable: {reason}.", entry.title).into());
            }
            cx.notify();
            return;
        }
        self.close_palette(window, cx);
        self.run_command(command, window, cx);
    }

    fn dismiss_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.finish_palette_composition(window, cx) {
            self.close_palette(window, cx);
        }
    }

    /// A listener that runs `command`, for a button or an action.
    fn on_command<E: 'static>(
        command: Command,
        cx: &mut Context<Self>,
    ) -> impl Fn(&E, &mut Window, &mut App) + 'static {
        let shell = cx.entity().downgrade();
        move |_, window, cx| {
            shell
                .update(cx, |this, cx| this.run_command(command, window, cx))
                .ok();
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.sidebar.move_selection(delta);
        if let Some(index) = self.sidebar.selected_index() {
            self.session_scroll
                .scroll_to_item(index, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Open the selected Session in Chat. If it is a different Session, park
    /// the outgoing draft, restore the incoming one and begin loading its
    /// timeline. A saved reading message is restored after its snapshot arrives.
    /// An overlay that was used to pick it closes.
    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let from_browser = matches!(
            self.destination,
            Destination::Sessions | Destination::Search | Destination::ProfileSearch
        );
        let previous = self.sidebar.active_id().map(str::to_string);
        let changed = self.sidebar.activate_selected();
        if changed {
            if let Some(id) = previous.as_deref() {
                self.park_reading_position(id, cx);
            }
            self.reading_restore = self
                .sidebar
                .active_id()
                .filter(|id| !id.is_empty() && id.len() <= crate::reading_positions::MAX_ID_BYTES)
                .map(str::to_string);
            self.reading_notice = None;
            // A full-output view must never outlive its Session/source.
            if self.viewer.take().is_some() {
                self.restore_focus = None;
            }
            if let Some(previous) = previous {
                // What the composer holds at this moment is not always the
                // user's draft:
                // - An over-limit edit may be waiting to be taken back. Its
                //   record says what the draft is; the composer may hold the
                //   over-limit text, or something else the user or the kit's
                //   undo left there in between.
                // - An input method may be composing, or may have ended a
                //   composition with no signal, leaving text past the limit.
                // In both cases the last accepted draft is stored, the rest
                // is not, and the user is told.
                let draft = self.draft(cx);
                let (parked, text_not_kept) = match self.rejections.pending_previous() {
                    Some(previous) => (SharedString::from(previous.to_string()), true),
                    None if !within_composer_limit(&draft) => (self.accepted_draft.clone(), true),
                    None => (draft, false),
                };
                // Parking a draft can push older ones out. That discards
                // text someone wrote, so it is said, not done silently.
                let evicted = self.drafts.set(&previous, &parked);
                let mut notices = Vec::new();
                if text_not_kept {
                    notices.push(format!(
                        "Text over the {} limit in the Session you left was not kept; its \
                         draft was stored without it.",
                        format_bytes(limits::COMPOSER_MAX_BYTES)
                    ));
                }
                if !evicted.is_empty() {
                    notices.push(format!(
                        "Unsent drafts for {} older Session(s) were discarded: stored drafts \
                         are limited to {} Sessions and {} in total.",
                        evicted.len(),
                        limits::DRAFT_MAX_ENTRIES,
                        format_bytes(limits::DRAFT_MAX_BYTES)
                    ));
                }
                self.composer_notice = (!notices.is_empty()).then(|| notices.join(" ").into());
            } else {
                self.composer_notice = None;
            }
            let restored = self
                .sidebar
                .active_id()
                .map(|id| self.drafts.get(id).to_string())
                .unwrap_or_default();
            // A fresh composer, not a new value in the old one: the old one's
            // undo history holds the other Session's text.
            self.rebuild_composer(&restored, None, window, cx);
            self.transcript_state = None;
            if self.transcript_source != TranscriptSource::Fixture {
                // A decoded transcript belongs to the Session just left,
                // never to the new title. A future client must load it.
                *self.blocks.borrow_mut() = Timeline::new(Vec::new());
                self.transcript_source = TranscriptSource::NotLoaded;
            }
            let count = self.blocks.borrow().len();
            self.timeline
                .update(cx, |timeline, cx| timeline.reset(count, cx));
            self.last_intent = None;
        }
        self.retire_transient_destination(Destination::Chat);
        self.destination = Destination::Chat;
        if changed {
            self.invalidate_search(cx);
            if self.client.is_some() {
                self.client_select(cx);
            }
        }
        self.prefs.close_overlays();
        self.settle(window, cx);
        if from_browser {
            self.focus_region(Region::Composer, window, cx);
        }
    }

    fn browse_query(&self, destination: Destination) -> Option<&Entity<InputState>> {
        match destination {
            Destination::Sessions => Some(&self.directory_query),
            Destination::Search => Some(&self.search_query),
            Destination::ProfileSearch => Some(&self.profile_query),
            _ => None,
        }
    }

    fn retire_transient_destination(&mut self, destination: Destination) {
        if self.destination == destination {
            return;
        }
        match self.destination {
            Destination::Invocations => self
                .retire_invocations("Snapshot closed. View Invocations explicitly for a new read."),
            Destination::ForkReview => {
                self.fork_review = None;
                self.fork_local_error = None;
            }
            Destination::RunDetails => self
                .reset_run_inspection("Inspection closed. Select a Run explicitly to read again."),
            Destination::Comparison => {
                self.comparison
                    .clear_result("Comparison closed. Compare explicitly to read again.");
                self.resource_pages.remove(&Destination::Comparison);
            }
            _ => {}
        }
    }

    fn set_destination(
        &mut self,
        destination: Destination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.destination == Destination::ProfileSearch
            && destination != Destination::ProfileSearch
            && self.finish_profile_composition(window, cx)
        {
            return;
        }
        self.invalidate_forecast(cx);
        if self.viewer.take().is_some() {
            self.restore_focus = None;
        }
        let previous = self.destination;

        self.retire_transient_destination(destination);
        if destination == Destination::ProfileSearch && previous != Destination::ProfileSearch {
            self.profile_return = if previous == Destination::Search {
                self.search_return
            } else {
                previous
            };
        }
        if destination == Destination::Search && previous != Destination::Search {
            self.search_return = if previous == Destination::ProfileSearch {
                self.profile_return
            } else {
                previous
            };
        }
        self.destination = destination;
        if self.client.is_some() {
            self.client_request(ClientCommand::Page(destination), cx);
        }
        self.prefs.close_overlays();
        if destination == Destination::Search {
            self.refresh_search(cx);
        }
        self.settle(window, cx);
        if let Some(query) = self.browse_query(destination).cloned() {
            query.update(cx, |query, cx| query.focus(window, cx));
            self.revealed = None;
            let shell = cx.entity().downgrade();
            window.on_next_frame(move |window, cx| {
                shell
                    .update(cx, |this, cx| {
                        let focused = this
                            .browse_query(destination)
                            .is_some_and(|query| query.focus_handle(cx).is_focused(window));
                        if this.destination == destination
                            && focused
                            && this.current_modal(this.layout(window)).is_none()
                        {
                            this.revealed = None;
                            cx.notify();
                        }
                    })
                    .ok();
            });
        } else if matches!(previous, Destination::Sessions | Destination::Search) {
            self.focus_region(
                if destination == Destination::Chat {
                    Region::Composer
                } else {
                    Region::Conversation
                },
                window,
                cx,
            );
        }
    }

    /// The region holding focus, or `None` when focus is in the header or
    /// nowhere.
    fn focused_region(&self, window: &Window, cx: &App) -> Option<Region> {
        // The composer is drawn inside the conversation column, so it has to
        // be told apart from the conversation before the containment checks.
        if self.composer.focus_handle(cx).contains_focused(window, cx)
            || self.resume_focus.is_focused(window)
            || self.abandon_focus.is_focused(window)
        {
            Some(Region::Composer)
        } else if self.sidebar_focus.contains_focused(window, cx) {
            Some(Region::Sidebar)
        } else if self.inspector_focus.contains_focused(window, cx) {
            Some(Region::Inspector)
        } else if self.conversation_focus.contains_focused(window, cx) {
            Some(Region::Conversation)
        } else {
            None
        }
    }

    /// Where the F6 cycle starts from: the focused region, or the composer
    /// when focus is outside every region.
    fn current_region(&self, window: &Window, cx: &App) -> Region {
        self.focused_region(window, cx).unwrap_or(Region::Composer)
    }

    fn focus_region(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        match region {
            Region::Sidebar => window.focus(&self.sidebar_focus, cx),
            Region::Conversation => window.focus(&self.conversation_focus, cx),
            Region::Inspector => window.focus(&self.inspector_focus, cx),
            Region::Composer => self
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx)),
        }
        // On a reflowed page, moving to a region shows it, even if focus
        // was already there and the page had been scrolled away.
        self.revealed = None;
        cx.notify();
    }

    fn focus_modal(&mut self, modal: Modal, window: &mut Window, cx: &mut Context<Self>) {
        match modal {
            Modal::Panel(region) => self.focus_region(region, window, cx),
            Modal::Palette => {
                if let Some(palette) = &self.palette {
                    palette
                        .query
                        .update(cx, |query, cx| query.focus(window, cx));
                }
                cx.notify();
            }
            Modal::Viewer => {
                window.focus(&self.viewer_focus, cx);
                cx.notify();
            }
            Modal::Forecast => {
                window.focus(&self.forecast_cancel_focus, cx);
                cx.notify();
            }
            Modal::Approvals => {
                if let Some(p) = &self.approval_panel {
                    window.focus(&p.close, cx);
                }
                cx.notify();
            }
            Modal::Step => {
                if let Some(panel) = &self.step_panel {
                    window.focus(&panel.close, cx);
                }
                cx.notify();
            }
            Modal::Questions => {
                if let Some(panel) = &self.question_panel {
                    window.focus(&panel.close_focus, cx);
                }
                cx.notify();
            }
            Modal::SessionEditor => {
                if let Some(panel) = &self.session_editor {
                    window.focus(&panel.cancel, cx);
                }
                cx.notify();
            }
        }
    }

    fn cycle_region(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The palette and the full-output view trap the cycle too.
        if let Some(
            modal @ (Modal::Palette
            | Modal::Viewer
            | Modal::Forecast
            | Modal::Questions
            | Modal::Step
            | Modal::Approvals
            | Modal::SessionEditor),
        ) = self.current_modal(self.layout(window))
        {
            self.focus_modal(modal, window, cx);
            return;
        }
        let next = self
            .current_region(window, cx)
            .cycle(self.layout(window), forward);
        self.focus_region(next, window, cx);
    }

    /// Bring focus into line with the layout after anything that can change
    /// it: a panel toggle, a dismissed overlay, a resize, a new destination.
    ///
    /// An overlay that comes up takes focus and remembers the exact control
    /// that had it. When the overlay goes away focus returns to that control,
    /// provided its region is still shown. Otherwise focus only moves if the
    /// region holding it is no longer on screen.
    fn settle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_text_scale(cx);
        self.prefs.settle(self.effective_width(window));
        let layout = self.layout(window);
        let focused = self.focused_region(window, cx);
        let modal = self.current_modal(layout);
        let before = std::mem::replace(&mut self.modal, modal);
        let fallback = if layout.composer {
            Region::Composer
        } else {
            Region::Conversation
        };
        match (before, self.modal) {
            (before, Some(modal)) if before != Some(modal) => {
                if before.is_none() {
                    self.restore_focus = window.focused(cx).map(|handle| FocusOrigin {
                        handle,
                        region: focused,
                    });
                }
                self.focus_modal(modal, window, cx);
            }
            (Some(_), None) => match self.restore_focus.take() {
                Some(origin) if origin.region.is_none_or(|region| region.is_visible(layout)) => {
                    window.focus(&origin.handle, cx);
                }
                _ => self.focus_region(fallback, window, cx),
            },
            _ if focused.is_some_and(|region| !region.is_visible(layout)) => {
                self.focus_region(fallback, window, cx)
            }
            _ => {}
        }
        cx.notify();
    }

    /// Open the Sessions or the Context panel, as its toggle does: docked
    /// where the window is wide enough, otherwise as a modal dialog. For
    /// checking a real window's accessibility tree with no input at all.
    pub fn open_panel(&mut self, sessions: bool, window: &mut Window, cx: &mut Context<Self>) {
        let region = if sessions {
            Region::Sidebar
        } else {
            Region::Inspector
        };
        let open = match region {
            Region::Sidebar => self.layout(window).sidebar != SidebarMode::Collapsed,
            _ => self.layout(window).inspector != InspectorMode::Hidden,
        };
        if !open {
            self.toggle_panel(region, window, cx);
        }
    }

    fn toggle_panel(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_forecast(cx);
        let width = self.effective_width(window);
        match region {
            Region::Sidebar => self.prefs.toggle_sidebar(width),
            Region::Inspector => self.prefs.toggle_inspector(width),
            Region::Conversation | Region::Composer => return,
        }
        self.settle(window, cx);
    }

    fn dismiss_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.close_overlays();
        self.settle(window, cx);
    }

    /// The header. In a reflowed window the title takes a line of its own
    /// and the controls wrap under it, so none of them is pushed off the
    /// side; the header is then as tall as that takes.
    fn render_header(
        &self,
        layout: Layout,
        reflowed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = self
            .sidebar
            .active_row()
            .map_or_else(|| "No Session".into(), |row| row.title.clone());
        let profile = readable(
            "header-profile",
            if self.client.is_some() {
                "Local Daemon /"
            } else {
                "Fixture Profile /"
            },
        )
        .text_sm()
        .text_color(cx.theme().muted_foreground);
        // The whole title is the heading's name even where the window is
        // too narrow to show all of it.
        let title = div()
            .id("header-title")
            .test_support()
            .role(Role::Heading)
            .aria_level(1)
            .aria_label(title.clone())
            .flex_1()
            .min_w_0()
            .truncate()
            .child(title);
        let controls = [
            (
                "toggle-sidebar",
                Command::ToggleSessions,
                &self.sidebar_toggle_focus,
            ),
            (
                "toggle-inspector",
                Command::ToggleContext,
                &self.inspector_toggle_focus,
            ),
            (
                "toggle-theme",
                Command::ToggleTheme,
                &self.theme_toggle_focus,
            ),
            (
                "show-commands",
                Command::ShowCommands,
                &self.commands_toggle_focus,
            ),
        ]
        .into_iter()
        .chain(self.client.iter().map(|_| {
            (
                "show-questions",
                Command::Questions,
                &self.questions_toggle_focus,
            )
        }))
        .map(|(id, command, focus)| {
            let entry = commands::entry(command, &self.commands);
            Button::new(id)
                .small()
                .ghost()
                .label(entry.label)
                .tooltip(tooltip(&entry))
                .track_focus(focus)
                .selected(entry.checked == Some(true))
                .on_click(Self::on_command(command, cx))
        })
        .collect::<Vec<_>>();
        // A banner landmark, so the header's controls have one node that can
        // be taken out of the tree behind a dialog.
        let header = div()
            .id("header")
            .test_support()
            .role(Role::Banner)
            .aria_label("Header")
            .map(|header| inert_while(header, self.current_modal(layout).is_some()))
            .flex()
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border);
        if reflowed {
            header
                .flex_col()
                .py_2()
                .child(h_flex().gap_2().child(profile).child(title))
                .child(h_flex().flex_wrap().gap_2().children(controls))
        } else {
            header
                .items_center()
                .h(self.scaled(HEADER_HEIGHT))
                .child(profile)
                .child(title)
                .children(controls)
        }
    }

    /// The Sessions panel. As a modal overlay it is a focus trap: Tab stays
    /// inside it until it is dismissed.
    fn render_sidebar(&self, modal: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.sidebar_focus.is_focused(window);
        let navigation = Destination::NAVIGATION.into_iter().map(|destination| {
            let button =
                Button::new(SharedString::from(format!(
                    "destination-{}",
                    destination.label()
                )))
                .small()
                .ghost()
                .label(destination.label())
                .selected(self.destination == destination)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_destination(destination, window, cx)
                }));
            if self.client.is_some()
                || matches!(destination, Destination::Chat | Destination::Sessions)
            {
                button
            } else {
                button.tooltip("Scaffold: this destination has no view yet")
            }
        });
        let panel = v_flex()
            .id("region-sidebar")
            .test_support()
            .role(if modal {
                Role::Dialog
            } else {
                Role::Navigation
            })
            .aria_label("Sessions")
            .when(modal, |panel| {
                modal_dialog(panel.aria_description(MODAL_DESCRIPTION))
            })
            // Docked beside another panel's dialog: behind it.
            .when(!modal, |panel| {
                inert_while(panel, self.background_inert(window))
            })
            .key_context(SESSION_LIST_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNextSession, _, cx| this.move_selection(1, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPreviousSession, _, cx| this.move_selection(-1, cx)),
            )
            .on_action(cx.listener(|this, _: &ActivateSession, window, cx| {
                this.activate_selected(window, cx)
            }))
            .w(self.panel_width(tokens::SIDEBAR_DEFAULT, window))
            .flex_shrink_0()
            .h_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_r_1()
            .border_color(region_border(focused, cx.theme().sidebar_border, cx))
            .child(
                readable(
                    "session-count",
                    format!("Sessions ({})", self.sidebar.rows().len()),
                )
                .px_3()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .id("session-list")
                    .test_support()
                    .role(Role::ListBox)
                    .aria_label("Sessions")
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_virtual_list(
                            cx.entity(),
                            "sessions",
                            self.session_sizes.clone(),
                            |this, range, _, cx| {
                                range
                                    .take_while(|index| *index < this.sidebar.rows().len())
                                    .map(|index| this.render_session_row(index, cx))
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(&self.session_scroll),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .children(navigation),
            );
        if modal {
            // The trap wrapper's id is the element's id in the tree, so it
            // carries the region's own name.
            panel
                .focus_trap("region-sidebar", &self.sidebar_focus)
                .into_any_element()
        } else {
            panel.track_focus(&self.sidebar_focus).into_any_element()
        }
    }

    fn render_session_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let row = &self.sidebar.rows()[index];
        let session_id = row.id.clone();
        let selected = self.sidebar.selected_index() == Some(index);
        let active = self.sidebar.active_id() == Some(row.id.as_str());
        // Status is text, never colour alone; "Open" marks the active Session.
        let status = format!(
            "{} · {}{}",
            row.group.label(),
            row.run_state.label(),
            if active { " · Open" } else { "" }
        ) + if self.drafts.has(&row.id) {
            " · Draft"
        } else {
            ""
        };
        v_flex()
            .id(session_row_id(&row.id))
            .test_support()
            // One option of the list: named by its title, read with its
            // status, and told its place among all the Sessions, of which
            // only those near the viewport exist as elements.
            .role(Role::ListBoxOption)
            .aria_label(row.title.clone())
            .aria_value(format!("{}, {status}", row.title))
            .aria_selected(selected)
            .aria_position_in_set(index + 1)
            .aria_size_of_set(self.sidebar.rows().len())
            .h(self.scaled(SESSION_ROW_HEIGHT))
            .w_full()
            .px_3()
            .justify_center()
            .when(selected, |row| row.bg(cx.theme().accent))
            .when(active, |row| {
                row.border_l_2().border_color(cx.theme().primary)
            })
            // The click names the Session, not the row position it had when
            // this frame was drawn.
            .on_click(cx.listener(move |this, _, window, cx| {
                this.sidebar.select_id(&session_id);
                this.activate_selected(window, cx);
            }))
            .child(div().text_sm().truncate().child(row.title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(status),
            )
    }

    /// The conversation: its heading, the timeline and, under it, the
    /// composer. In a reflowed window (`page_height` is the height it is
    /// given there) the composer is not inside it: it is the page's next
    /// part, so the page can scroll to either.
    fn render_conversation(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reflowed = page_height.is_some();
        let run_state = self.run_state();
        let blocks = self.blocks.clone();
        let shell = cx.entity();
        let reading_width = self.scaled(tokens::READING_MAX);
        let focused = self.conversation_focus.is_focused(window);
        let revision = self.block_copy_revision();
        let copy_stale = self.transcript_reload_needed().is_some();
        let timeline = MessageScroller::new("timeline", self.timeline.clone(), {
            move |index, window, cx| {
                // A frame drawn between two flushes still shows the newest
                // text: the streamed row is brought up to date when a row
                // is built, at most once however many deltas arrived.
                blocks.borrow_mut().settle();
                let blocks = blocks.borrow();
                let Some(block) = blocks.blocks().get(index) else {
                    return div().into_any_element();
                };
                let id = block.id as usize;
                // Only one bounded window of the block is laid out, so a
                // block of any size costs a bounded amount per frame. The
                // line that says so goes above and below the text, because a
                // window can be far taller than the viewport.
                let streaming = blocks.is_streaming() && index + 1 == blocks.len();
                let above = block_window_line(block, streaming, "top", &shell, cx);
                let below = block_window_line(block, streaming, "bottom", &shell, cx);
                v_flex()
                    .id(("block", id))
                    .test_support()
                    // One article of the log, named by who it is from.
                    .role(Role::Article)
                    .aria_label(block.kind.label())
                    .w_full()
                    .max_w(reading_width)
                    .mx_auto()
                    // Less margin where the window is narrow for its text.
                    .map(|row| if reflowed { row.px_2() } else { row.px_4() })
                    .py_2()
                    .gap_1()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(Label::new(block.kind.label()))
                            .child({
                                let shell = shell.clone();
                                Button::new(SharedString::from(format!("block-{id}-copy-source")))
                                    .xsmall()
                                    .ghost()
                                    .label("Copy source")
                                    .accessibility_label("Copy complete block source")
                                    .disabled(streaming || copy_stale || block.total_bytes() > BLOCK_COPY_MAX_BYTES)
                                    .tooltip(if streaming {
                                        VIEWER_STREAMING
                                    } else if copy_stale {
                                        "Reload the transcript before copying source"
                                    } else if block.total_bytes() > BLOCK_COPY_MAX_BYTES {
                                        "Direct copy is limited to 1 MiB; use View whole for larger blocks"
                                    } else {
                                        "Copy the complete source, including Markdown and whitespace, to the system clipboard. May contain private data."
                                    })
                                    .on_click(move |_, window, cx| {
                                        shell.update(cx, |this, cx| {
                                            this.copy_block_source(id as u64, revision, window, cx);
                                        });
                                    })
                            }),
                    )
                    .children(above)
                    .child({
                        // The kit's markdown view gives assistive technology
                        // nothing, so its container carries the text it
                        // lays out: the row's bounded window, as the plain
                        // text a reader sees, not its markdown source.
                        //
                        // Making the plain text of a full window costs
                        // milliseconds, and a streamed row's window is
                        // rebuilt every frame, so it is made only while
                        // assistive technology is reading the window (GPUI
                        // builds no accessibility tree otherwise), and in
                        // the test harness, which has no platform to ask.
                        let text = SharedString::from(block.preview().clone());
                        let read = window.is_a11y_active() || cfg!(feature = "test-support");
                        let accessible: SharedString = if read {
                            block.accessible_text().into()
                        } else {
                            text.clone()
                        };
                        div()
                            .id(("block-text", id))
                            .test_support()
                            .role(Role::Label)
                            .aria_value(accessible)
                            .child(TextView::markdown(("markdown", id), text).selectable(true))
                    })
                    .children(below)
                    .into_any_element()
            }
        });
        v_flex()
            .id("region-conversation")
            .test_support()
            .role(Role::Main)
            .aria_label("Conversation")
            .map(|region| inert_while(region, self.background_inert(window)))
            .track_focus(&self.conversation_focus)
            .map(|region| Self::main_size(region, page_height))
            .border_1()
            .border_color(region_border(focused, cx.theme().background, cx))
            .child(
                h_flex()
                    .map(|row| if reflowed { row.px_2() } else { row.px_4() })
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        readable(
                            "block-count",
                            format!("{} blocks", self.blocks.borrow().len()),
                        )
                        .flex_1()
                        .text_sm(),
                    )
                    .child(
                        readable("run-status", format!("Run: {}", run_state.label()))
                            .text_sm()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .when_some(self.reading_notice, |region, notice| {
                region.child(announced(div().id("reading-position-notice")).test_support().role(Role::Status).aria_value(notice).px_2().py_1().text_xs().child(notice))
            })
            .when_some(self.block_copy_notice, |region, notice| {
                region.child(
                    announced(div().id("block-copy-notice"))
                        .flex_shrink_0()
                        .test_support()
                        .role(Role::Status)
                        .aria_value(notice)
                        .px_2()
                        .py_1()
                        .text_xs()
                        .child(Label::new(notice)),
                )
            })
            .when_some(self.transcript_reload_needed(), |region, reason| {
                region.child(
                    announced(div().id("transcript-reload"))
                        .test_support()
                        .role(Role::Status)
                        .aria_label("Transcript needs reload")
                        .aria_value(reason.notice())
                        .px_2()
                        .py_1()
                        .flex_shrink_0()
                        .text_xs()
                        .child(Label::new(reason.notice())),
                )
            })
            .when(self.client.as_ref().is_some_and(|client| client.future_attempt_validation()), |region| region.child(
                readable("stream-recovery-policy", client::FUTURE_ATTEMPT_LIMITATION)
                    .text_sm().px_2().py_1().flex_shrink_0()
            ))
            .when_some(self.client_step_mode, |region,on|region.child(
                h_flex().min_w_0().w_full().flex_wrap().items_center().gap_2().px_2().py_1()
                    .child(readable("step-mode-state",if on {"Step mode: On (sampled)"} else {"Step mode: Off (sampled)"}).text_sm())
                    .when_some(self.mode_enable_action(),|row,action|row.child(div().id("step-mode-enable-anchor").anchor_scroll(Some(self.mode_enable_anchor.clone())).child(Button::new("step-mode-enable").track_focus(&self.mode_enable_focus).label("Enable Step mode").on_click(cx.listener(move|this,_,window,cx|this.enable_mode(action.clone(),window,cx))))))
                    .when_some(self.mode_disable_action(),|row,action|row.child(div().id("step-mode-disable-anchor").anchor_scroll(Some(self.mode_enable_anchor.clone())).child(Button::new("step-mode-disable").track_focus(&self.mode_enable_focus).label("Disable Step mode").on_click(cx.listener(move|this,_,window,cx|this.disable_mode(action.clone(),window,cx))))))
                    .child(readable("step-mode-help",if on {"Off stops future pauses; waiting Steps and approvals still need review."} else {"For this daemon; waiting prompts and approvals still need review."}).min_w_0().max_w_full().text_xs())
            ))
            .when_some(self.client_steps.summary(), |region, summary| region.child(
                readable("daemon-step-inventory", summary).text_sm().px_2().py_1().max_h(px(120.)).overflow_y_hidden()
            ))
            .when_some(self.client.as_ref().and_then(|c|c.step_status().notice()),|region,text|region.child(readable("native-step-receipt",text).text_sm().px_2().py_1()))
            .when(self.client_attention && self.client.as_ref().is_some_and(Client::is_ready),|region|region.child(Button::new("approval-review-open").track_focus(&self.approval_open_focus).label("Review security approval").on_click(Self::on_command(Command::Approvals,cx))))
            .when(self.step_prompt().is_some(), |region|region.child(Button::new("step-review-open").track_focus(&self.step_open_focus).label("Review Step").on_click(cx.listener(|this,_,window,cx|this.open_step_review(window,cx)))))
            .when(self.client.is_some() && self.client_attention, |region| region.child(
                readable("daemon-pending-attention", "This Profile has pending decisions. Use Questions for user questions or Approvals for one complete supported Tool call; other decisions require Formless TUI.").text_sm().px_2().py_1()
            ))
            .when(self.client.is_some() && !self.client_snapshot_ready, |region| region.child(
                readable("daemon-session-state", if self.sidebar.active_id().is_none() {
                    "Choose a Session from the sidebar or open Sessions to create one."
                } else if self.transcript_source == TranscriptSource::NotLoaded {
                    "Loading the selected Session. Sending is unavailable until a canonical snapshot arrives."
                } else {
                    "This is the last confirmed transcript. Current Run state is unknown; sending is unavailable until this Session reloads."
                }).text_sm().px_2().py_1()
            ))
            .child(
                div()
                    .id("timeline-area")
                    .test_support()
                    .relative()
                    .flex_1()
                    // Reflowed metadata may wrap beyond the nominal page
                    // section height. Grow the page instead of erasing history.
                    .map(|area| if reflowed { area.min_h(self.scaled(80.)) } else { area.min_h_0() })
                    // Before the timeline, so it is first to see the wheel.
                    .when(reflowed, |area| area.child(self.wheel_to_page(cx)))
                    .child(timeline),
            )
            .when(!reflowed, |region| {
                region.child(self.render_composer(false, cx))
            })
    }

    /// Hands the wheel on from the timeline to the reflowed page around it.
    ///
    /// The kit's timeline keeps every wheel event that reaches it, even when
    /// it has nothing further to scroll to, so on a page it would be a trap:
    /// with the timeline under the pointer the page could not be moved. This
    /// is drawn under the timeline, blocks no pointer input, and sees the
    /// wheel first. When the timeline is already at the end the wheel is
    /// turning towards, the page is scrolled instead.
    ///
    /// It acts only when the timeline is what the wheel would reach: its
    /// own hitbox has to be in the window's hit test for the pointer. A
    /// modal overlay in front, or anything else that blocks the pointer,
    /// takes it out, and then the wheel is not this listener's to move the
    /// page with.
    fn wheel_to_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let timeline = self.timeline.clone();
        let page = self.page_scroll.clone();
        let shell = cx.entity_id();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                let line_height = window.line_height();
                window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                        return;
                    }
                    // Negative is towards the end of the page.
                    let delta = event.delta.pixel_delta(line_height).y;
                    let timeline = timeline.read(cx);
                    let timeline_is_done = if delta < px(0.) {
                        !timeline.is_scrolled_up()
                    } else {
                        let top = timeline.logical_scroll_top();
                        top.item_ix == 0 && top.offset_in_item == px(0.)
                    };
                    let offset = page.offset();
                    let target = (offset.y + delta).clamp(-page.max_offset().y, px(0.));
                    if delta == px(0.) || !timeline_is_done || target == offset.y {
                        return;
                    }
                    page.set_offset(point(offset.x, target));
                    cx.stop_propagation();
                    cx.notify(shell);
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The size of the main part (the conversation, or the notice that
    /// stands in for it). Beside docked panels it fills the row. On a
    /// reflowed page it is as wide as the page and at least `page_height`
    /// tall, taller if its own contents are, and takes any height the page
    /// has left over.
    fn main_size<E: Styled>(main: E, page_height: Option<Pixels>) -> E {
        match page_height {
            Some(height) => main.w_full().flex_grow(1.).flex_shrink_0().min_h(height),
            None => main.flex_1().min_w_0().h_full(),
        }
    }

    pub fn search_query(&self) -> &Entity<InputState> {
        &self.search_query
    }
    pub fn search_hits(&self) -> &[SearchHit] {
        self.search.hits()
    }
    pub fn search_results_revision(&self) -> u64 {
        self.search.revision()
    }

    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        let query = self.search_query.read(cx).value();
        self.search.run(
            &self.blocks.borrow(),
            &query,
            self.search_revision,
            self.transcript_source != TranscriptSource::NotLoaded,
        );
        self.search_notice = None;
        if self.search.choice().is_some() {
            self.search_scroll
                .scroll_to_item_strict(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Only changes to the indexed finished corpus invalidate hits. Live
    /// deltas modify the excluded tail and do not rescan on every chunk.
    fn invalidate_search(&mut self, cx: &mut Context<Self>) {
        self.search_revision = self.search_revision.wrapping_add(1);
        self.search.clear(self.search_revision);
        self.search_notice = None;
        self.block_copy_notice = None;
        if self.destination == Destination::Search {
            self.refresh_search(cx);
        }
    }

    /// Native Find opens only loaded-text Search. It must not replace a
    /// dialog or move focus away while an input method owns preedit text.
    fn find_loaded_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.background_inert(window) || self.command_state(window, cx).composing {
            return;
        }
        if let Some(query) = self.browse_query(self.destination).cloned() {
            if query.update(cx, |query, cx| {
                query.marked_text_range(window, cx).is_some()
            }) {
                return;
            }
        }
        if self.destination == Destination::Search {
            self.search_query
                .update(cx, |query, cx| query.focus(window, cx));
        } else {
            self.run_command(Command::GoTo(Destination::Search), window, cx);
        }
    }

    fn search_navigation_focused(&self, window: &Window, cx: &App) -> bool {
        self.conversation_focus.is_focused(window)
            || self.search_query.focus_handle(cx).is_focused(window)
    }

    fn finish_search_composition(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let composing = self.search_query.update(cx, |query, cx| {
            query.marked_text_range(window, cx).is_some()
        });
        if composing {
            self.search_query
                .update(cx, |query, cx| query.unmark_text(window, cx));
        }
        composing
    }

    fn move_search(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_navigation_focused(window, cx) {
            cx.propagate();
            return;
        }
        let composing = self.search_query.update(cx, |query, cx| {
            query.marked_text_range(window, cx).is_some()
        });
        if composing {
            return;
        }
        self.search.move_selection(delta);
        self.search_notice = None;
        if let Some(index) = self.search.choice() {
            self.search_scroll
                .scroll_to_item_strict(index, ScrollStrategy::Top);
        }
        cx.notify();
    }

    fn open_search_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_navigation_focused(window, cx) {
            cx.propagate();
            return;
        }
        if self.finish_search_composition(window, cx) {
            return;
        }
        if let Some(hit) = self
            .search
            .choice()
            .and_then(|index| self.search.hits().get(index))
            .copied()
        {
            self.open_search_hit(hit, self.search.revision(), window, cx);
        }
    }

    /// Activate a result using the revision captured when its row was
    /// drawn. Queued actions must not open a same-ID replacement source.
    pub fn open_search_hit(
        &mut self,
        hit: SearchHit,
        revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.destination != Destination::Search
            || self.current_modal(self.layout(window)).is_some()
        {
            return;
        }
        if self.finish_search_composition(window, cx) {
            return;
        }
        if self.search_query.read(cx).value().as_ref() != self.search.query() {
            self.refresh_search(cx);
            self.search_notice = Some("The query changed; select a current match.");
            cx.notify();
            return;
        }
        if revision != self.search_revision
            || !self
                .search
                .valid_hit(&self.blocks.borrow(), self.search_revision, hit)
        {
            self.search_notice = Some("Results changed; select a current match.");
            cx.notify();
            return;
        }
        match self.view_block(hit.block_id, window, cx) {
            Ok(()) => {
                self.search_notice = None;
                if let Some(viewer) = &self.viewer {
                    if !viewer.starts.is_empty() {
                        let row = viewer
                            .starts
                            .partition_point(|start| *start as usize <= hit.start)
                            .saturating_sub(1);
                        viewer
                            .scroll
                            .scroll_to_item_strict(row, ScrollStrategy::Top);
                    }
                }
            }
            Err(reason) => self.search_notice = Some(reason),
        }
        cx.notify();
    }

    fn search_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.finish_search_composition(window, cx) {
            self.set_destination(self.search_return, window, cx);
        }
    }

    fn search_status(&self) -> String {
        let scope = match self.transcript_source {
            TranscriptSource::Fixture => {
                "Shared synthetic fixture only; no global history".to_string()
            }
            TranscriptSource::Snapshot => self
                .sidebar
                .active_row()
                .map(|row| {
                    format!(
                        "Loaded Session {} ({}) only",
                        directory::visual_text(&row.title),
                        directory::visual_text(&row.id)
                    )
                })
                .unwrap_or_else(|| "Loaded transcript only".into()),
            TranscriptSource::NotLoaded => {
                "Current Session not loaded; no history lookup".to_string()
            }
        };
        let detail = match self.search.state() {
            search::State::EmptyQuery => {
                "Type literal, case-sensitive text to search finished loaded blocks.".to_string()
            }
            search::State::QueryTooLong => format!(
                "Query too long; use at most {} characters.",
                search::QUERY_MAX_CHARS
            ),
            search::State::NoSource => "No transcript loaded for this Session.".to_string(),
            search::State::Results => {
                let mut detail = if let Some(index) = self.search.choice() {
                    let hit = self.search.hits()[index];
                    format!(
                        "{} of {} matches · block {}, byte {} · case-sensitive.",
                        index + 1,
                        self.search.hits().len(),
                        hit.block_id,
                        hit.start
                    )
                } else {
                    "No case-sensitive matches in the searched finished text.".to_string()
                };
                if self.search.hit_limited {
                    detail.insert_str(0, "Partial: first 200 results only. ");
                }
                if self.search.scan_limited {
                    detail.insert_str(0, "Partial: search limited to 4 MiB/4096 blocks. ");
                }
                if self.search.live_tail_omitted {
                    detail.push_str(" Live tail excluded.");
                }
                detail
            }
        };
        format!(
            "{}{}{} {scope}.",
            self.transcript_reload_needed()
                .map(|reason| format!("Reload needed: {} ", reason.notice()))
                .unwrap_or_default(),
            self.search_notice
                .map(|notice| format!("{notice} "))
                .unwrap_or_default(),
            detail
        )
    }

    fn render_search(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let status = self.search_status();
        let count = self.search.hits().len();
        let list = gpui_kit::uniform_list(
            "search-results-list",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| {
                let blocks = this.blocks.borrow();
                range
                    .filter_map(|index| {
                        let hit = *this.search.hits().get(index)?;
                        if !this.search.valid_hit(&blocks, this.search_revision, hit) {
                            return None;
                        }
                        let block = blocks.blocks().get(hit.block_index)?;
                        let title = format!(
                            "{} block {} · bytes {}..{}",
                            block.kind.label(),
                            hit.block_id,
                            hit.start,
                            hit.end
                        );
                        let snippet = TranscriptSearch::snippet(&blocks, hit)
                            .unwrap_or_else(|| "Match no longer available".into());
                        let selected = this.search.choice() == Some(index);
                        let revision = this.search.revision();
                        Some(
                            v_flex()
                                .id(search_row_id(hit))
                                .test_support()
                                .role(Role::ListBoxOption)
                                .aria_label(title.clone())
                                .aria_description(snippet.clone())
                                .aria_selected(selected)
                                .aria_position_in_set(index + 1)
                                .aria_size_of_set(this.search.hits().len())
                                .when(selected, |row| {
                                    row.aria_active_descendant().bg(cx.theme().accent)
                                })
                                .h(this.scaled(64.))
                                .w_full()
                                .px_2()
                                .justify_center()
                                .cursor_pointer()
                                .child(div().text_xs().truncate().child(title))
                                .child(div().text_sm().truncate().child(snippet))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_search_hit(hit, revision, window, cx)
                                })),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.search_scroll)
        .flex_1()
        .min_h_0();
        v_flex().id("region-conversation").test_support().role(Role::Main)
            .aria_label("Loaded-text Search")
            .aria_description("Literal, case-sensitive, non-overlapping matches in finished text loaded in this window only, including Markdown source. Never global or unloaded history. Live tail excluded. At most 200 results, 4 MiB and 4096 blocks; partial results are labelled. Arrows choose, Enter opens the whole block at the match, Escape returns. Finish input-method composition before activating or leaving.")
            .track_focus(&self.conversation_focus)
            .map(|region| inert_while(region, self.background_inert(window)))
            .map(|region| Self::main_size(region, page_height))
            .when_some(page_height, |region, height| region.h(height))
            .p_2().gap_2().border_1()
            .border_color(region_border(self.conversation_focus.contains_focused(window, cx), cx.theme().background, cx))
            .key_context(SEARCH_CONTEXT)
            .on_action(cx.listener(|this, _: &SearchNext, window, cx| this.move_search(1, window, cx)))
            .on_action(cx.listener(|this, _: &SearchPrevious, window, cx| this.move_search(-1, window, cx)))
            .on_action(cx.listener(|this, _: &SearchOpen, window, cx| this.open_search_choice(window, cx)))
            .on_action(cx.listener(|this, _: &SearchBack, window, cx| this.search_back(window, cx)))
            .child(h_flex().gap_2()
                .child(readable("search-title", "Search loaded text").flex_1().min_w_0().truncate())
                .child(Button::new("search-back").small().label(format!("Back to {}", self.search_return.label()))
                    .on_click(cx.listener(|this, _, window, cx| this.search_back(window, cx)))))
            .child(Input::new(&self.search_query).small().aria_label("Search loaded transcript"))
            .child(announced(div().id("search-status")).test_support().role(Role::Label)
                .aria_value(status.clone()).text_xs().truncate().child(directory::visual_text(&status)))
            .child(v_flex().id("search-results").test_support().role(Role::ListBox)
                .aria_label("Matches in finished loaded text").flex_1().min_h_0().child(list))
    }

    fn rebuild_directory(&mut self, cx: &App) {
        self.directory_rows = self.sidebar.rows().to_vec();
        if self.include_archived && self.client.is_some() {
            self.directory_rows.extend(
                self.client_sessions
                    .iter()
                    .filter(|session| session.archived)
                    .map(|session| crate::model::SessionRow::of(session, &HashSet::new(), false)),
            );
        }
        let query = self.directory_query.read(cx).value();
        self.directory.rebuild(&self.directory_rows, &query);
    }

    fn toggle_archived_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.destination == Destination::Sessions
            && self.finish_directory_composition(window, cx)
        {
            return;
        }
        self.include_archived = !self.include_archived;
        self.directory.scope_changed();
        self.rebuild_directory(cx);
        self.set_destination(Destination::Sessions, window, cx);
    }

    #[cfg(feature = "test-support")]
    pub fn directory_selected_id(&self) -> Option<&str> {
        self.directory.chosen_id(&self.directory_rows)
    }

    pub fn directory_query(&self) -> &Entity<InputState> {
        &self.directory_query
    }

    fn directory_composing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.directory_query.update(cx, |query, cx| {
            query.marked_text_range(window, cx).is_some()
        })
    }

    fn finish_directory_composition(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.directory_composing(window, cx) {
            return false;
        }
        self.directory_query
            .update(cx, |query, cx| query.unmark_text(window, cx));
        true
    }

    fn directory_navigation_focused(&self, window: &Window, cx: &App) -> bool {
        self.conversation_focus.is_focused(window)
            || self.directory_query.focus_handle(cx).is_focused(window)
    }

    fn move_directory(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        // Do not steal a button's raw arrow/key behavior through the
        // region binding. In particular Enter belongs to Back when focused.
        if !self.directory_navigation_focused(window, cx) {
            cx.propagate();
            return;
        }
        if self.directory_composing(window, cx) {
            return;
        }
        self.directory
            .move_selection(self.directory_rows.as_slice(), delta);
        if let Some(index) = self.directory.choice(self.directory_rows.as_slice()) {
            self.directory_scroll
                .scroll_to_item_strict(index, ScrollStrategy::Top);
        }
        self.directory_reveal = self
            .directory
            .chosen_id(&self.directory_rows)
            .map(|id| (id.to_string(), 2));
        cx.notify();
    }

    fn reveal_directory_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, frames)) = self.directory_reveal.take() else {
            return;
        };
        if self.fit(window) == Fit::Reflowed
            && self.destination == Destination::Sessions
            && self.directory.chosen_id(&self.directory_rows) == Some(id.as_str())
            && self.directory_navigation_focused(window, cx)
            && !self.background_inert(window)
        {
            // Reveal the list container in the page; UniformList separately
            // puts the chosen virtual row at its top. A virtual row's local
            // anchor cannot be used with the outer page's ScrollHandle.
            self.directory_anchor.scroll_to(window, cx);
            window.request_animation_frame();
            if frames > 1 {
                self.directory_reveal = Some((id, frames - 1));
            }
        }
    }

    fn open_directory_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.directory_navigation_focused(window, cx) {
            cx.propagate();
            return;
        }
        if self.finish_directory_composition(window, cx) {
            return;
        }
        if let Some(id) = self
            .directory
            .chosen_id(self.directory_rows.as_slice())
            .map(str::to_string)
        {
            self.open_directory_id(&id, window, cx);
        }
    }

    fn open_directory_id(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.destination != Destination::Sessions
            || self.finish_directory_composition(window, cx)
            || !self.directory.contains(self.directory_rows.as_slice(), id)
        {
            return;
        }
        self.directory.select_id(&self.directory_rows, id);
        if self
            .directory_rows
            .iter()
            .any(|row| row.id == id && row.group == SessionGroup::Archived)
        {
            self.open_session_editor(window, cx);
        } else {
            self.sidebar.select_id(id);
            self.activate_selected(window, cx);
        }
    }

    fn directory_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.finish_directory_composition(window, cx) {
            self.set_destination(Destination::Chat, window, cx);
        }
    }

    fn render_directory(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = self.directory.matches().len();
        let status = if self.directory.too_long() {
            format!(
                "Filter too long. Use at most {} characters.",
                directory::FILTER_MAX_CHARS
            )
        } else if let Some(index) = self.directory.choice(self.directory_rows.as_slice()) {
            let row = &self.directory_rows.as_slice()[self.directory.matches()[index]];
            format!(
                "{} of {count}: {}, {} · {}",
                index + 1,
                row.title,
                row.group.label(),
                row.run_state.label()
            )
        } else if count > 0 {
            format!("{count} matching Sessions. Choose one with the arrows or a click.")
        } else {
            "No matching Sessions. Clear or change the filter.".to_string()
        };
        let status = if self.include_archived {
            format!("Including archived · {status}")
        } else {
            status
        };
        let list = gpui_kit::uniform_list(
            "directory-list",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let source = *this.directory.matches().get(index)?;
                        let row = this.directory_rows.as_slice().get(source)?;
                        let id = row.id.clone();
                        let selected = this.directory.choice(&this.directory_rows) == Some(index);
                        let meta = format!(
                            "{} · {}{}{}{}",
                            row.group.label(),
                            row.run_state.label(),
                            if this.sidebar.active_id() == Some(row.id.as_str()) {
                                " · Open"
                            } else {
                                ""
                            },
                            if this.drafts.has(&row.id) {
                                " · Draft"
                            } else {
                                ""
                            },
                            if row.group == SessionGroup::Archived {
                                if this.client_connected {
                                    " · Enter for restore details"
                                } else {
                                    " · Connect for restore details"
                                }
                            } else {
                                ""
                            }
                        );
                        let tag_preview = row
                            .tags
                            .iter()
                            .take(2)
                            .map(|tag| directory::visual_text(tag))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let preview = if tag_preview.is_empty() {
                            directory::visual_text(row.preview.as_deref().unwrap_or(""))
                        } else {
                            format!(
                                "Tags: {tag_preview} · {}",
                                directory::visual_text(row.preview.as_deref().unwrap_or(""))
                            )
                        };
                        Some(
                            v_flex()
                                .id(directory_row_id(&id))
                                .test_support()
                                .role(Role::ListBoxOption)
                                .aria_label(row.title.clone())
                                .aria_description(meta.clone())
                                .aria_selected(selected)
                                .aria_position_in_set(index + 1)
                                .aria_size_of_set(this.directory.matches().len())
                                .when(selected, |row| {
                                    row.aria_active_descendant().bg(cx.theme().accent)
                                })
                                .h(this.scaled(72.))
                                .w_full()
                                .px_2()
                                .justify_center()
                                .cursor_pointer()
                                .child(
                                    div()
                                        .text_sm()
                                        .truncate()
                                        .child(directory::visual_text(&row.title)),
                                )
                                .child(div().text_xs().truncate().child(meta))
                                .child(
                                    div()
                                        .text_xs()
                                        .truncate()
                                        .child(directory::visual_text(&preview)),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_directory_id(&id, window, cx)
                                })),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.directory_scroll)
        .flex_1()
        .min_h_0();
        let mark = self.comparison_selected_id().map(str::to_string);
        let pair = self.comparison_selected_pair();
        let revision = self.comparison.revision;
        let focused = self.conversation_focus.contains_focused(window, cx);
        v_flex().id("region-conversation").test_support().role(Role::Main)
            .aria_label(if self.client.is_some() { "Daemon Sessions" } else { "Fixture Sessions" })
            .aria_description(if self.client.is_some() { if self.include_archived {"Daemon Sessions including archived. Filter title, preview, ID or tags. Arrows choose; Enter opens an unarchived Session or restore details for an archived entry. Escape returns to Chat."} else {"Daemon Sessions; archived excluded. Filter title, preview, ID or tags. Arrows choose, Enter opens, Escape returns to Chat. Archived toggles the directory scope. Details edits the chosen entry."} } else { "Read-only fixture Sessions; archived Sessions excluded. Filter title, preview or ID, up to 256 characters. Arrows choose, Enter opens, Escape returns to Chat. Shared synthetic transcript only; no daemon connection or Session management actions." })
            .track_focus(&self.conversation_focus)
            .map(|region| inert_while(region, self.background_inert(window)))
            .map(|region| Self::main_size(region, page_height))
            // The daemon toolbar can wrap to two lines at 200%. Keep a
            // complete row below it; the surrounding page scrolls normally.
            .when_some(page_height, |region, height| region.h(if self.client.is_some(){height.max(self.scaled(360.))}else{height}))
            .p_2().gap_2().border_1()
            .border_color(region_border(focused, cx.theme().background, cx))
            .key_context(DIRECTORY_CONTEXT)
            .on_action(cx.listener(|this, _: &DirectoryNext, window, cx| this.move_directory(1, window, cx)))
            .on_action(cx.listener(|this, _: &DirectoryPrevious, window, cx| this.move_directory(-1, window, cx)))
            .on_action(cx.listener(|this, _: &DirectoryOpen, window, cx| this.open_directory_choice(window, cx)))
            .on_action(cx.listener(|this, _: &DirectoryBack, window, cx| this.directory_back(window, cx)))
            .child(h_flex().gap_2().flex_wrap().flex_shrink_0()
                .child(readable("directory-title", if self.client.is_some() { "Daemon Sessions" } else { "Fixture Sessions" }).flex_1().min_w_0().truncate())
                .when(self.client.is_some(), |header| header
                    .child(Button::new("edit-daemon-session").small().label("Details").disabled(!self.client_connected || self.session_edit_target().is_none())
                        .on_click(cx.listener(|this,_,window,cx| {this.run_command(Command::SessionDetails,window,cx);})))
                    .child(div().id("mark-comparison-anchor").anchor_scroll(Some(self.comparison_controls[0].1.clone())).child(Button::new("mark-comparison-left").track_focus(&self.comparison_controls[0].0).small().label("Mark left").disabled(mark.is_none() || !self.comparison_connected())
                        .on_click(cx.listener(move |this,_,window,cx| {if let Some(id)=&mark {this.mark_comparison(id.clone(),revision,window,cx);}}))))
                    .child(div().id("compare-pair-anchor").anchor_scroll(Some(self.comparison_controls[1].1.clone())).child(Button::new("compare-marked-sessions").track_focus(&self.comparison_controls[1].0).small().label("Compare").disabled(self.comparison_disabled_reason().is_some())
                        .on_click(cx.listener(move |this,_,window,cx| {if let Some(pair)=&pair {this.read_comparison(pair.clone(),revision,window,cx);}}))))
                    .child(Button::new("new-daemon-session").small().label("New Session").disabled(!self.client_connected)
                        .on_click(cx.listener(|this, _, _, cx| this.client_request(ClientCommand::CreateSession, cx))))
                    .child(Button::new("refresh-daemon-sessions").small().label("Refresh").disabled(!self.client_connected)
                        .on_click(cx.listener(|this, _, _, cx| this.client_request(ClientCommand::Refresh, cx)))))
                .child(Button::new("back-to-chat").small().label("Back to Chat")
                    .on_click(cx.listener(|this, _, window, cx| this.directory_back(window, cx)))))
            .child(h_flex().gap_2().flex_shrink_0()
                .child(Input::new(&self.directory_query).small().aria_label("Filter Sessions").flex_1().min_w_0())
                .when(self.client.is_some(),|row|row.child(Button::new("toggle-archived-sessions").small().label(if self.include_archived {"Archived on"} else {"Archived off"}).selected(self.include_archived)
                    .accessibility_label(if self.include_archived {"Hide archived Sessions"} else {"Include archived Sessions"})
                    .on_click(cx.listener(|this,_,window,cx|{this.run_command(Command::ToggleArchived,window,cx);}))))
            )
            .child(announced(div().id("directory-status")).test_support().role(Role::Label)
                .aria_value(status.clone()).text_xs().truncate().child(directory::visual_text(&status)))
            .when(self.client.is_some(), |region| region.child(readable("comparison-mark", match &self.comparison.left {
                Some(id)=>format!("Comparison left: {id}. Select a different Session as right, then Compare. Persisted data only; no model call."),
                None=>"Comparison: Mark left, select another Session, then Compare. Persisted data only; no model call.".into(),
            }).text_xs()))
            .child(v_flex().id("directory-results").test_support().role(Role::ListBox)
                .anchor_scroll(Some(self.directory_anchor.clone()))
                .aria_label(if self.client.is_some() { "Filtered daemon Sessions" } else { "Filtered fixture Sessions" }).flex_1().min_h_0().child(list))
    }

    /// The destinations other than Chat/Sessions. It takes the conversation's place and
    /// says plainly that nothing is here, so a working navigation button is
    /// never mistaken for a working page.
    fn render_scaffold_notice(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.destination == Destination::ForkReview {
            return self.render_session_fork(page_height, window, cx);
        }
        if self.client.is_some()
            || matches!(
                self.destination,
                Destination::Comparison | Destination::RunDetails | Destination::Invocations
            )
        {
            return self.render_resource_page(page_height, window, cx);
        }
        let focused = self.conversation_focus.is_focused(window);
        let reflowed = page_height.is_some();
        let name = self.destination.label();
        let explanation = div()
            .max_w(self.scaled(480.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Label::new(format!(
                "This Phase 0 scaffold has Chat, fixture Sessions and loaded-text Search only. {name} has no screen, \
                 data or actions here, and this notice is not a preview of one."
            )));
        let back = Button::new("back-to-chat")
            .outline()
            .label("Back to Chat")
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_destination(Destination::Chat, window, cx)
            }));
        let notice = v_flex()
            .id("region-conversation")
            .test_support()
            .role(Role::Main)
            .aria_label(format!("{name}: not built"))
            .map(|region| inert_while(region, self.background_inert(window)))
            .aria_description(format!(
                "This Phase 0 scaffold has Chat, fixture Sessions and loaded-text Search only. {name} has no screen, data or \
                 actions here, and this notice is not a preview of one."
            ))
            .track_focus(&self.conversation_focus)
            .map(|region| Self::main_size(region, page_height))
            .items_center()
            .gap_3()
            .border_1()
            .border_color(region_border(focused, cx.theme().background, cx))
            .child(Label::new(format!("{name}: not built")));
        if reflowed {
            // From the top, with the way back before the explanation: the
            // notice can be taller than the window, and its top is what
            // focus brings into view.
            notice
                .p_3()
                .child(back)
                .child(explanation)
                .into_any_element()
        } else {
            notice
                .justify_center()
                .p_6()
                .child(explanation)
                .child(back)
                .into_any_element()
        }
    }

    fn render_resource_page(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let destination = self.destination;
        let invocation_refresh = (self.invocations.revision, self.invocations.connection);
        let invocation_source = self.selected_invocation();
        let run_refresh = (
            self.run_inspection.target.clone(),
            self.run_inspection.revision,
        );
        let inspect_run = self.selected_run_target();
        let inspection_revision = self.run_inspection.revision;
        let comparison_refresh = (self.comparison.pair.clone(), self.comparison.revision);
        let page = self.resource_pages.get(&destination);
        let count = page.map_or(0, |page| page.rows.len());
        let notice = page.map_or(
            if destination == Destination::Invocations {
                self.invocations.notice.clone()
            } else if destination == Destination::RunDetails {
                self.run_inspection.notice.clone()
            } else if destination == Destination::Comparison {
                self.comparison.notice.clone()
            } else {
                "Loading from the Daemon Core…".to_string()
            },
            |page| page.notice.clone(),
        );
        let list = gpui_kit::uniform_list(
            "daemon-resource-list",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let page = this.resource_pages.get(&this.destination)?;
                        let row = page.rows.get(index)?;
                        let selected = page.selected == Some(index);
                        let revision = page.revision;
                        let destination = this.destination;
                        Some(
                            Button::new(("resource-row", index))
                                .small()
                                .w_full()
                                .label(directory::visual_text(&row.label))
                                .selected(selected)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(page) = this.resource_pages.get_mut(&destination) {
                                        if this.destination != destination
                                            || page.revision != revision
                                        {
                                            return;
                                        }
                                        page.selected = Some(index);
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .collect()
            }),
        )
        .h(self.scaled(220.))
        .track_scroll(&self.resource_scroll);
        let selected = page.and_then(|page| page.selected.and_then(|index| page.rows.get(index)));
        let copy_target = self.resource_copy_target();
        let label = selected.map_or("Select an entry".into(), |row| {
            directory::visual_text(&row.label)
        });
        let detail = selected.map_or(String::new(), |row| {
            format!(
                "{}{}",
                row.preview,
                if row.preview_truncated {
                    "\nPreview limited to 500 characters. Copy details for all returned fields."
                } else {
                    ""
                }
            )
        });
        v_flex()
            .id("region-conversation")
            .test_support()
            .role(Role::Main)
            .aria_label(format!("{} · daemon data", destination.label()))
            .aria_description("Read-only daemon data. Up and Down choose entries; Tab reaches Refresh, Back and Copy details.")
            .map(|region| inert_while(region, self.background_inert(window)))
            .track_focus(&self.conversation_focus)
            .key_context(RESOURCE_CONTEXT)
            .on_action(
                cx.listener(|this, _: &ResourceNext, window, cx| this.move_resource(1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ResourcePrevious, window, cx| {
                this.move_resource(-1, window, cx)
            }))
            .map(|region| Self::main_size(region, page_height))
            .when_some(page_height, |region, height| region.h(height))
            .child(
                v_flex()
                    .w_full()
                    .h_full()
                    .p_3()
                    .gap_3()
                    .id("resource-body").overflow_y_scroll().track_scroll(&self.resource_body)
                    .child({
                        let refresh=Button::new("refresh-resource").track_focus(&self.resource_controls[0]).small().flex_shrink_0().label("Refresh")
                            .disabled(!self.client_connected || (destination==Destination::Invocations&&(self.invocations.pending()||self.invocations.connection.is_none())) || (destination==Destination::RunDetails&&(self.run_inspection.pending()||self.run_inspection.target.is_none())) || (destination==Destination::Comparison&&(self.comparison.pending()||self.comparison.pair.is_none())))
                            .on_click(cx.listener(move |this,_,window,cx|{
                                if this.destination!=destination{return;}
                                match destination {
                                    Destination::Invocations=>{if let Some(connection)=invocation_refresh.1{this.read_invocations(invocation_refresh.0,connection,window,cx);}},
                                    Destination::RunDetails=>{if let Some(target)=&run_refresh.0 {this.read_run_inspection(target.clone(),run_refresh.1,None,window,cx);}},
                                    Destination::Comparison=>{if let Some(pair)=&comparison_refresh.0 {this.read_comparison(pair.clone(),comparison_refresh.1,window,cx);}},
                                    _=>this.client_request(ClientCommand::Refresh,cx),
                                }
                            }));
                        let (back_label,back_destination)=match destination {Destination::Comparison=>("Back to Sessions",Destination::Sessions),Destination::RunDetails=>("Back to Metrics",Destination::Metrics),Destination::Invocations=>("Back to Tools",Destination::Tools),_=>("Back to Chat",Destination::Chat)};
                        let back=Button::new("resource-back").track_focus(&self.resource_controls[1]).small().flex_shrink_0().label(back_label)
                            .on_click(cx.listener(move |this,_,window,cx|{if this.destination==destination {this.set_destination(back_destination,window,cx);}}));
                        let actions=h_flex().min_w_0().gap_2().flex_wrap().when(page_height.is_some(),|actions|actions.w_full())
                            .child(div().id("resource-refresh-anchor").child(refresh))
                            .child(div().id("resource-back-anchor").child(back))
                            .when(destination==Destination::Metrics,|actions|actions.child(div().id("inspect-run-anchor")
                                .child(Button::new("inspect-selected-run").small().label("Inspect Run").track_focus(&self.resource_controls[3]).disabled(self.run_inspection_disabled_reason().is_some())
                                    .on_click(cx.listener(move |this,_,window,cx|{if let Some((target,source))=&inspect_run {this.read_run_inspection(target.clone(),inspection_revision,Some(*source),window,cx);}})))));
                        let actions=actions
                            .when(destination==Destination::Tools,|actions|actions.child(Button::new("view-invocations").small().label("Invocations").track_focus(&self.resource_controls[3]).disabled(self.invocations_disabled_reason().is_some()).on_click(cx.listener(|this,_,window,cx|{this.run_command(Command::ViewInvocations,window,cx);}))))
                            .when(destination==Destination::Invocations,|actions|actions.child(Button::new("open-invocation-session").small().label("Open Session").track_focus(&self.resource_controls[3]).disabled(self.invocation_open_reason().is_some()).on_click(cx.listener(move|this,_,window,cx|{if let Some(source)=&invocation_source {this.open_invocation_session(source.clone(),invocation_refresh.0,window,cx);}}))));
                        h_flex().w_full().min_w_0().gap_2().when(page_height.is_some(),|header|header.flex_col().items_start())
                            .child(Label::new(destination.label()).min_w_0().when(page_height.is_none(),|title|title.flex_1())).child(actions)
                    })
                    .child(readable("resource-notice", notice))
                    .child(list)
                    .child(readable("resource-selected", label))
                    .child(readable("resource-detail", detail))
                    .child(
                        div().id("resource-copy-anchor").child(Button::new("copy-resource")
                            .track_focus(&self.resource_controls[2])
                            .small()
                            .label("Copy details")
                            .disabled(selected.is_none())
                            .tooltip("Copy the selected entry's complete returned JSON to the system clipboard. May contain private data.")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(target) = copy_target {
                                    this.copy_resource_details(target, window, cx);
                                }
                            }))),
                    ),
            )
            .into_any_element()
    }

    fn reveal_resource_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let basis = (window.focused(cx), window.viewport_size(), self.text_scale);
        if self.resource_focus.as_ref() != Some(&basis) {
            self.resource_focus = Some(basis);
            self.resource_frames = 2;
        }
        if self.resource_frames > 0 {
            self.resource_frames -= 1;
            if self.destination == Destination::Sessions {
                for (focus, anchor) in &self.comparison_controls {
                    if focus.contains_focused(window, cx) {
                        anchor.scroll_to(window, cx);
                        window.request_animation_frame();
                        break;
                    }
                }
            } else if self.resource_pages.contains_key(&self.destination)
                || matches!(
                    self.destination,
                    Destination::Comparison | Destination::RunDetails | Destination::Invocations
                )
            {
                for (index, focus) in self.resource_controls.iter().enumerate() {
                    if focus.contains_focused(window, cx) {
                        // Direct body children: header=0, notice=1, list=2,
                        // selected label=3, detail=4 and Copy wrapper=5.
                        // The handle preserves nested ancestor scroll offsets.
                        self.resource_body
                            .scroll_to_item(if index == 2 { 5 } else { 0 });
                        window.request_animation_frame();
                        break;
                    }
                }
            }
        }
    }

    /// The exact destination, page and selection shown by Copy details.
    /// No JSON copy is retained in the callback while the user navigates.
    pub fn resource_copy_target(&self) -> Option<(Destination, u64, usize)> {
        let page = self.resource_pages.get(&self.destination)?;
        let selected = page.selected?;
        page.rows.get(selected)?;
        Some((self.destination, page.revision, selected))
    }

    /// Refuse a queued click if refresh, selection or navigation replaced
    /// the entry the user saw, or if a newer dialog owns interaction.
    pub fn copy_resource_details(
        &mut self,
        target: (Destination, u64, usize),
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.background_inert(window) || self.resource_copy_target() != Some(target) {
            return false;
        }
        let (destination, _, selected) = target;
        let row = &self.resource_pages[&destination].rows[selected];
        cx.write_to_clipboard(ClipboardItem::new_string(row.detail.clone()));
        true
    }

    fn move_resource(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = self.resource_pages.get_mut(&self.destination) {
            if !page.rows.is_empty() {
                let selected = page
                    .selected
                    .unwrap_or(0)
                    .saturating_add_signed(delta)
                    .min(page.rows.len() - 1);
                page.selected = Some(selected);
                self.resource_scroll
                    .scroll_to_item_strict(selected, ScrollStrategy::Top);
                self.conversation_focus.focus(window, cx);
                cx.notify();
            }
        }
    }

    /// The composer. `inert` takes it out of the accessibility tree; it is
    /// needed only where the composer is not inside the conversation (a
    /// reflowed page), since the conversation's flag covers what is in it.
    fn render_composer(&self, inert: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let button = |id: &'static str, command: Command, cx: &mut Context<Self>| {
            let entry = commands::entry(command, &self.commands);
            Button::new(id)
                .label(entry.label)
                .disabled(!entry.enabled())
                .tooltip(tooltip(&entry))
                .on_click(Self::on_command(command, cx))
        };
        let primary = button("composer-primary", Command::Send, cx).primary();
        // The second action exists only while a Run is going.
        let secondary = self
            .commands
            .composer
            .secondary
            .map(|_| button("composer-secondary", Command::SendSecondary, cx).outline());
        // Stop keeps its own fixed slot, apart from the primary action.
        let stop = button("composer-stop", Command::Stop, cx).danger();
        let modified_enter = commands::shortcut_text(commands::composer_submit_key());
        let primary_label = send_mode_label(self.commands.composer.primary);
        v_flex()
            .id("region-composer")
            .test_support()
            .role(Role::Group)
            .aria_label("Composer")
            .map(|composer| inert_while(composer, inert))
            .flex_shrink_0()
            .p_3()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .children(self.client_recovery.as_ref().map(|run| {
                v_flex().id("run-recovery").test_support().role(Role::Group)
                    .aria_label("Run recovery").gap_2()
                    .child(readable("unfinished-run", format!("Last Run: {} · {}", run.status, run.run_id)).text_sm())
                    .children(run.error.as_ref().map(|error| {
                        let mut chars = error.chars();
                        let preview: String = chars.by_ref().take(500).collect();
                        readable("unfinished-run-error", format!("{preview}{}", if chars.next().is_some() { "… (details in the TUI)" } else { "" })).text_sm()
                    }))
                    .child(readable("run-recovery-help", "Resume continues from the saved transcript and reviews any required cost confirmation here. Recovery targets the Session; the daemon does not condition it on this displayed Run ID. Abandon ends recovery without sending your draft.").text_sm())
                    .child(h_flex().flex_wrap().gap_2()
                        .child(div().id("resume-anchor").pt_1().anchor_scroll(Some(self.resume_anchor.clone())).child(button("run-resume", Command::ResumeRun, cx).outline().track_focus(&self.resume_focus)))
                        .child(div().id("abandon-anchor").pt_1().anchor_scroll(Some(self.abandon_anchor.clone())).child(button("run-abandon", Command::AbandonRun, cx).outline().track_focus(&self.abandon_focus))))
            }))
            .child(
                div().id("composer-input-anchor").key_context("FormlessComposer").when(self.client_recovery.is_some(), |field| field.pt_1()).anchor_scroll(Some(self.composer_anchor.clone())).child(Textarea::new(&self.composer)
                    .aria_label("Message")
                    .on_paste({
                        let shell = cx.entity();
                        move |item, _, cx| shell.update(cx, |this, cx| this.paste_refused(item, cx))
                    })
                    .w_full()),
            )
            .child(readable("composer-send-keys", if self.enter_sends {
                format!("Enter: {primary_label} · Shift+Enter: newline · {modified_enter}: Queue during a Run")
            } else {
                format!("Enter / Shift+Enter: newline · {modified_enter}: {primary_label} · Queue remains a separate button")
            }).text_xs().text_color(cx.theme().muted_foreground))
            // A refusal or a change to the draft is announced as it appears.
            .children(self.composer_notice.clone().map(|notice| {
                announced(div().id("composer-notice"))
                    .test_support()
                    .role(Role::Label)
                    .aria_value(notice.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(Label::new(notice))
            }))
            .child(
                // Wraps, so the actions stay inside a window too narrow for
                // them side by side.
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(stop)
                    .child(div().flex_1())
                    .children(secondary)
                    .child(primary),
            )
    }

    /// The Context panel. As a modal overlay it is a focus trap: Tab stays
    /// inside it until it is dismissed.
    fn render_inspector(&self, modal: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.inspector_focus.is_focused(window);
        let row = self.sidebar.active_row();
        // Each detail is one line to assistive technology: its name, then
        // its value.
        let detail = |id: &'static str, label: &'static str, value: String| {
            v_flex()
                .id(id)
                .test_support()
                .role(Role::Label)
                .aria_value(format!("{label}: {value}"))
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(div().text_sm().child(value))
        };
        let details = v_flex()
            .id("inspector-details")
            .test_support()
            .size_full()
            // In a short window the details are taller than the panel.
            .overflow_y_scroll()
            .track_scroll(&self.inspector_scroll)
            .p_3()
            .gap_3()
            .child(readable("inspector-title", "Context").text_sm())
            .child(detail(
                "inspector-session",
                "Session",
                row.map_or_else(|| "None".to_string(), |row| row.title.clone()),
            ))
            .child(detail(
                "inspector-session-id",
                "Session id",
                row.map_or_else(|| "None".to_string(), |row| row.id.clone()),
            ))
            .child(detail(
                "inspector-run",
                "Run",
                self.run_state().label().to_string(),
            ))
            // Shown as it is, not rendered as markdown, so it is read as it
            // is too: what is on screen and what is read must match.
            .child(detail(
                "inspector-last-message",
                "Last message",
                row.and_then(|row| row.preview.clone())
                    .unwrap_or_else(|| "None".to_string()),
            ))
            .when(self.client.is_some(), |details| {
                details.child(self.render_session_usage(cx))
            });
        let panel = div()
            .id("region-inspector")
            .test_support()
            .role(if modal {
                Role::Dialog
            } else {
                Role::Complementary
            })
            .aria_label("Context")
            .when(modal, |panel| {
                modal_dialog(panel.aria_description(MODAL_DESCRIPTION))
            })
            .when(!modal, |panel| {
                inert_while(panel, self.background_inert(window))
            })
            .key_context(INSPECTOR_CONTEXT)
            .on_action(cx.listener(|this, _: &ScrollInspectorUp, _, cx| {
                this.scroll_inspector(-1., false, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollInspectorDown, _, cx| {
                this.scroll_inspector(1., false, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollInspectorPageUp, _, cx| {
                this.scroll_inspector(-1., true, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollInspectorPageDown, _, cx| {
                this.scroll_inspector(1., true, cx)
            }))
            .relative()
            .w(self.panel_width(tokens::INSPECTOR_DEFAULT, window))
            .flex_shrink_0()
            .h_full()
            .bg(cx.theme().background)
            .border_l_1()
            .border_color(region_border(focused, cx.theme().border, cx))
            .child(details)
            .vertical_scrollbar(&self.inspector_scroll);
        if modal {
            panel
                .focus_trap("region-inspector", &self.inspector_focus)
                .into_any_element()
        } else {
            panel.track_focus(&self.inspector_focus).into_any_element()
        }
    }

    /// A modal overlay: a backdrop that swallows pointer input for everything
    /// under it and dismisses on click, with the panel on top. Escape
    /// dismisses too. Where the shell is whole, the overlay covers the row
    /// under the header, so the header, and the toggle that opened it, stays
    /// usable. On a reflowed page it covers the window, header and all: the
    /// page under it can be scrolled anywhere, and the panel must not be.
    fn render_overlay(
        &self,
        region: Region,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = match region {
            Region::Sidebar => self.render_sidebar(true, window, cx),
            _ => self.render_inspector(true, window, cx),
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .id("overlay-backdrop")
                    .test_support()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(cx.theme().overlay)
                    .occlude()
                    .on_click(cx.listener(|this, _, window, cx| this.dismiss_overlay(window, cx))),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .map(|panel| match region {
                        Region::Sidebar => panel.left_0(),
                        _ => panel.right_0(),
                    })
                    .shadow_lg()
                    .occlude()
                    .key_context(OVERLAY_CONTEXT)
                    .on_action(cx.listener(|this, _: &DismissOverlay, window, cx| {
                        this.dismiss_overlay(window, cx)
                    }))
                    .child(panel),
            )
    }

    /// Read a status section taller than the reflowed viewport from the
    /// keyboard, without changing its focus or moving another region.
    fn scroll_status(
        &mut self,
        to: WindowMove,
        page: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.fit(window) != Fit::Reflowed
            || !self.status_focus.is_focused(window)
            || self.background_inert(window)
        {
            return;
        }
        let metrics = self.page_metrics();
        let Some((_, status)) = metrics
            .sections
            .iter()
            .find(|(section, _)| *section == PageSection::Status)
        else {
            return;
        };
        let end = -self.page_scroll.max_offset().y;
        let start = (metrics.viewport.top() - status.top()).clamp(end, px(0.));
        let line = self.scaled(32.);
        let step = if page {
            (metrics.viewport.size.height - line).max(line)
        } else {
            line
        };
        let current = self.page_scroll.offset().y;
        let y = match to {
            WindowMove::Start => start,
            WindowMove::End => end,
            WindowMove::Earlier => (current + step).clamp(end, start),
            WindowMove::Later => (current - step).clamp(end, start),
        };
        self.page_scroll.set_offset(point(px(0.), y));
        cx.notify();
    }

    /// The status line. On a reflowed page it can be off screen and nothing
    /// in it takes focus, so there it is a tab stop itself: focus brings it
    /// into view like any other part.
    fn render_status(
        &self,
        layout: Layout,
        reflowed: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reflowed = reflowed || self.client.is_some() || self.preferences.is_some();
        let intent = if self.client.is_some() {
            if let Some(receipt) = self.mode_notice() {
                format!("{} · {}", self.client_status, receipt)
            } else {
                self.client_status.clone()
            }
        } else {
            match &self.last_intent {
                Some(record) => match record.kind {
                    IntentKind::Send(mode) => format!(
                        "Recorded {} ({} characters), not sent",
                        send_mode_label(mode),
                        record.content_chars
                    ),
                    IntentKind::Stop => "Recorded Stop, not sent".to_string(),
                    IntentKind::Resume => "Recorded Resume, not sent".to_string(),
                    IntentKind::Abandon => "Recorded Abandon, not sent".to_string(),
                },
                None => "No intent recorded".to_string(),
            }
        };
        let destination = if matches!(
            self.destination,
            Destination::Chat | Destination::Sessions | Destination::Search
        ) || self.client.is_some()
        {
            self.destination.label().to_string()
        } else {
            format!("{} (scaffold: no view)", self.destination.label())
        };
        let detail = format!(
            "Text {}% · {destination} · inspector {}",
            self.text_scale.percent(),
            match layout.inspector {
                InspectorMode::Docked => "docked",
                InspectorMode::Overlay => "overlay",
                InspectorMode::Hidden => "hidden",
            }
        );
        let fixture = readable(
            "status-fixture",
            if self.client.is_some() {
                if self.client_connected {
                    "Local Daemon Core"
                } else {
                    "Daemon disconnected · Run state unknown"
                }
            } else {
                match self.transcript_source {
                    TranscriptSource::Fixture => "Synthetic fixture: no Daemon Core connection",
                    TranscriptSource::Snapshot => "Decoded snapshot · no daemon connection",
                    TranscriptSource::NotLoaded => "Transcript not loaded · no daemon connection",
                }
            },
        );
        // What the last composer action did. It changes only when the user
        // acts, so it is announced.
        let intent = announced(
            div()
                .id("status-intent")
                .test_support()
                .role(Role::Label)
                .aria_value(intent.clone())
                .child(intent),
        );
        let status = div()
            .id("status")
            .test_support()
            .role(Role::Status)
            .aria_label("Status")
            .map(|status| inert_while(status, self.background_inert(window)))
            .flex()
            .flex_shrink_0()
            .px_3()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .border_t_1()
            .border_color(cx.theme().border)
            .when_some(self.export_save_notice.clone(), |status, notice| {
                status.child(
                    announced(
                        div()
                            .id("export-save-notice")
                            .test_support()
                            .role(Role::Status)
                            .aria_value(notice.clone())
                            .child(Label::new(notice)),
                    )
                    .text_xs(),
                )
            })
            .when_some(self.preference_notice.clone(), |status, notice| {
                status.child(
                    announced(
                        div()
                            .id("preferences-notice")
                            .test_support()
                            .role(Role::Status)
                            .aria_value(notice.clone())
                            .child(Label::new(notice)),
                    )
                    .text_xs(),
                )
            })
            .when_some(self.render_fork_status(), |status, notice| {
                status.child(
                    announced(div().id("fork-status"))
                        .test_support()
                        .role(Role::Status)
                        .aria_value(notice.clone())
                        .child(notice)
                        .text_xs(),
                )
            })
            .when(self.client.is_some() && self.client_attention, |status| {
                status.child(readable(
                    "status-pending-attention",
                    "Profile decisions · Questions / Formless TUI approvals",
                ))
            });
        if reflowed {
            // One under the other, each wrapping to the window's width, so
            // nothing is cut off at the side.
            status
                .track_focus(&self.status_focus)
                .key_context("FormlessStatus")
                .aria_description("In a reflowed window, use Up/Down, Page Up/Down, Home and End to read long status text.")
                .on_action(cx.listener(|this, _: &ScrollViewerUp, window, cx| {
                    this.scroll_status(WindowMove::Earlier, false, window, cx)
                }))
                .on_action(cx.listener(|this, _: &ScrollViewerDown, window, cx| {
                    this.scroll_status(WindowMove::Later, false, window, cx)
                }))
                .on_action(cx.listener(|this, _: &ScrollViewerPageUp, window, cx| {
                    this.scroll_status(WindowMove::Earlier, true, window, cx)
                }))
                .on_action(cx.listener(|this, _: &ScrollViewerPageDown, window, cx| {
                    this.scroll_status(WindowMove::Later, true, window, cx)
                }))
                .on_action(cx.listener(|this, _: &ScrollViewerStart, window, cx| {
                    this.scroll_status(WindowMove::Start, false, window, cx)
                }))
                .on_action(cx.listener(|this, _: &ScrollViewerEnd, window, cx| {
                    this.scroll_status(WindowMove::End, false, window, cx)
                }))
                .border_1()
                .border_color(region_border(
                    self.status_focus.is_focused(window),
                    cx.theme().border,
                    cx,
                ))
                .flex_col()
                .py_1()
                .child(fixture)
                .child(intent)
                .child(readable("status-detail", detail))
        } else {
            status
                .items_center()
                .h(self.scaled(STATUS_HEIGHT))
                .gap_3()
                .child(fixture)
                .child(intent)
                .child(div().flex_1())
                .child(readable("status-detail", detail))
        }
    }

    /// The whole shell as one page that scrolls up and down, for a window
    /// smaller than the text size needs: header, conversation, composer and
    /// status line, each as wide as the window, one under the other.
    fn render_page(
        &mut self,
        layout: Layout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.reveal_focused(window, cx);
        // A screen-sized conversation baseline, growing for wrapped controls
        // and a bounded timeline viewport. The page keeps the composer reachable.
        let height = window
            .viewport_size()
            .height
            .min(self.scaled(tokens::REFLOW_CONVERSATION_MIN));
        let chat = self.destination == Destination::Chat;
        let main = if self.destination == Destination::Search {
            self.render_search(Some(height), window, cx)
                .into_any_element()
        } else if self.destination == Destination::ProfileSearch {
            self.render_profile_search(Some(height), window, cx)
                .into_any_element()
        } else if self.destination == Destination::Sessions {
            self.render_directory(Some(height), window, cx)
                .into_any_element()
        } else if chat {
            self.render_conversation(Some(height), window, cx)
                .into_any_element()
        } else {
            self.render_scaffold_notice(Some(height), window, cx)
                .into_any_element()
        };
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                // These children are `PageSection::of_page`, in order.
                v_flex()
                    .id("shell-page")
                    .test_support()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.page_scroll)
                    .child(self.render_header(layout, true, cx))
                    .child(main)
                    .when(chat, |page| {
                        page.child(self.render_composer(self.current_modal(layout).is_some(), cx))
                    })
                    .child(self.render_status(layout, true, window, cx)),
            )
            .vertical_scrollbar(&self.page_scroll)
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reveal_fork_control(window, cx);
        self.reveal_profile_search(window, cx);
        self.reveal_directory_choice(window, cx);
        self.reveal_session_editor(window, cx);
        self.reveal_session_usage(window, cx);
        self.reveal_resource_control(window, cx);
        self.reveal_question_focus(window, cx);
        self.reveal_step_focus(window, cx);
        self.reveal_approval_focus(window, cx);
        if self
            .forecast
            .as_ref()
            .is_some_and(|dialog| self.client_epoch() != Some(dialog.prepared.epoch))
        {
            self.forecast = None;
        }
        // State replacement has no Window parameter. Finish the modal
        // dismissal/focus fallback here, before stale rows can be drawn.
        if (self.modal == Some(Modal::Viewer) && self.viewer.is_none())
            || (self.modal == Some(Modal::Forecast) && self.forecast.is_none())
            || (self.modal == Some(Modal::Approvals) && self.approval_panel.is_none())
        {
            self.settle(window, cx);
        }
        self.commands = self.command_state(window, cx);
        self.fit_viewer(window, cx);
        let layout = self.layout(window);
        let reflowed = self.fit(window) == Fit::Reflowed;
        v_flex()
            .id("shell")
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(Self::on_command::<FocusNextRegion>(
                Command::FocusNextRegion,
                cx,
            ))
            .on_action(Self::on_command::<FocusPreviousRegion>(
                Command::FocusPreviousRegion,
                cx,
            ))
            .on_action(Self::on_command::<ToggleSidebar>(
                Command::ToggleSessions,
                cx,
            ))
            .on_action(Self::on_command::<ToggleInspector>(
                Command::ToggleContext,
                cx,
            ))
            .on_action(Self::on_command::<IncreaseTextSize>(
                Command::IncreaseTextSize,
                cx,
            ))
            .on_action(Self::on_command::<DecreaseTextSize>(
                Command::DecreaseTextSize,
                cx,
            ))
            .on_action(Self::on_command::<ResetTextSize>(
                Command::ResetTextSize,
                cx,
            ))
            .on_action(Self::on_command::<ShowCommands>(Command::ShowCommands, cx))
            .on_action(cx.listener(|this, _: &ComposerModifiedEnter, window, cx| {
                if this.destination == Destination::Chat
                    && !this.background_inert(window)
                    && this.composer.focus_handle(cx).contains_focused(window, cx)
                {
                    this.run_command(
                        if this.enter_sends {
                            Command::SendSecondary
                        } else {
                            Command::Send
                        },
                        window,
                        cx,
                    );
                }
            }))
            .on_action(
                cx.listener(|this, _: &FindLoadedText, window, cx| {
                    this.find_loaded_text(window, cx)
                }),
            )
            .map(|shell| {
                if reflowed {
                    shell
                        .child(self.render_page(layout, window, cx))
                        .when_some(layout.modal(), |shell, region| {
                            shell.child(self.render_overlay(region, window, cx))
                        })
                } else {
                    shell
                        .child(self.render_header(layout, false, cx))
                        .child(self.render_body(layout, window, cx))
                        .child(self.render_status(layout, false, window, cx))
                }
            })
            .when(self.viewer.is_some(), |shell| {
                shell.child(self.render_viewer(window, cx))
            })
            // On top of everything, a panel overlay included.
            .when(self.palette.is_some(), |shell| {
                shell.child(self.render_palette(window, cx))
            })
            .when(self.forecast.is_some(), |shell| {
                shell.child(self.render_forecast(window, cx))
            })
            .when(self.approval_panel.is_some(), |shell| {
                shell.child(self.render_approval_review(window, cx))
            })
            .when(self.step_panel.is_some(), |shell| {
                shell.child(self.render_step_review(window, cx))
            })
            .when(self.question_panel.is_some(), |shell| {
                shell.child(self.render_questions(window, cx))
            })
            .when(self.session_editor.is_some(), |shell| {
                shell.child(self.render_session_editor(window, cx))
            })
    }
}

impl Shell {
    fn scroll_forecast(&mut self, lines: f32, page: bool, cx: &mut Context<Self>) {
        if self.forecast.is_none() {
            return;
        }
        let amount = if page {
            self.forecast_scroll
                .bounds()
                .size
                .height
                .max(self.scaled(40.))
        } else {
            self.scaled(40.)
        };
        let offset = self.forecast_scroll.offset();
        let y = (offset.y + amount * lines).clamp(-self.forecast_scroll.max_offset().y, px(0.));
        self.forecast_scroll.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn render_forecast(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = self.forecast.as_ref().expect("forecast is open");
        let id = dialog.prepared.id;
        let viewport = window.viewport_size();
        let margin = px(16.);
        let title = format!("Confirm {}", dialog.prepared.label);
        let footer = h_flex()
            .flex_shrink_0()
            .flex_wrap()
            .gap_2()
            .child(
                Button::new("forecast-cancel")
                    .label("Cancel")
                    .tooltip("Cancel and keep the draft")
                    .track_focus(&self.forecast_cancel_focus)
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_forecast(window, cx))),
            )
            .child(
                Button::new("forecast-copy")
                    .label("Copy details")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(dialog) = &this.forecast {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                dialog.details.clone(),
                            ));
                        }
                    })),
            )
            .child(
                Button::new("forecast-confirm")
                    .label(format!("Confirm and {}", dialog.prepared.label))
                    .primary()
                    .disabled(dialog.waiting)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_forecast(id, window, cx)
                    })),
            );
        let panel = v_flex().id("forecast-dialog").test_support().role(Role::Dialog)
            .aria_label(title.clone()).aria_description("Modal. Local forecast, not a spending limit. Arrow and Page keys read the forecast. Tab reaches confirmation; Escape cancels and keeps the draft.")
            .map(modal_dialog).key_context(FORECAST_CONTEXT)
            .on_action(cx.listener(|this, _: &CloseForecast, window, cx| this.cancel_forecast(window, cx)))
            .on_action(cx.listener(|this, _: &ScrollViewerUp, _, cx| this.scroll_forecast(1., false, cx)))
            .on_action(cx.listener(|this, _: &ScrollViewerDown, _, cx| this.scroll_forecast(-1., false, cx)))
            .on_action(cx.listener(|this, _: &ScrollViewerPageUp, _, cx| this.scroll_forecast(1., true, cx)))
            .on_action(cx.listener(|this, _: &ScrollViewerPageDown, _, cx| this.scroll_forecast(-1., true, cx)))
            .on_action(cx.listener(|this, _: &ScrollViewerStart, _, cx| { this.forecast_scroll.set_offset(point(px(0.), px(0.))); cx.notify(); }))
            .on_action(cx.listener(|this, _: &ScrollViewerEnd, _, cx| { this.forecast_scroll.scroll_to_bottom(); cx.notify(); }))
            .w(self.scaled(620.).min(viewport.width - margin * 2.))
            .max_h(viewport.height - margin * 2.).p_3().gap_2()
            .bg(cx.theme().popover).text_color(cx.theme().popover_foreground)
            .border_1().border_color(cx.theme().border).rounded_lg().shadow_lg().occlude()
            .child(readable("forecast-heading", title).text_lg().flex_shrink_0())
            .child(v_flex().id("forecast-body").test_support().flex_1().min_h_0().overflow_y_scroll()
                .track_scroll(&self.forecast_scroll).gap_2()
                .when(dialog.waiting, |body| body.child(announced(div().id("forecast-waiting"))
                    .test_support().role(Role::Label).aria_value("Confirming… Escape cancels.")
                    .child("Confirming… Escape cancels.")))
                .child(readable("forecast-session", format!("Session: {}", dialog.prepared.session_id)))
                .child(readable("forecast-estimate-help", "Local estimates are not a spending limit. Nothing starts until this confirmation is acknowledged and the draft is rechecked."))
                .children(dialog.summary.iter().enumerate().map(|(index, text)| readable(SharedString::from(format!("forecast-detail-{index}")), text.clone()).text_sm())))
            .child(footer).focus_trap("forecast-dialog", &self.forecast_focus);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .id("forecast-backdrop")
                    .test_support()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(cx.theme().overlay)
                    .occlude()
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_forecast(window, cx))),
            )
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .justify_center()
                    .items_start()
                    .pt(margin)
                    .child(panel),
            )
    }

    /// The row under the header where the shell is whole: docked panels on
    /// either side of the main part, and any overlay over all of it.
    fn render_body(
        &self,
        layout: Layout,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let main = if self.destination == Destination::Search {
            self.render_search(None, window, cx).into_any_element()
        } else if self.destination == Destination::ProfileSearch {
            self.render_profile_search(None, window, cx)
                .into_any_element()
        } else if self.destination == Destination::Sessions {
            self.render_directory(None, window, cx).into_any_element()
        } else if self.destination == Destination::Chat {
            self.render_conversation(None, window, cx)
                .into_any_element()
        } else {
            self.render_scaffold_notice(None, window, cx)
                .into_any_element()
        };
        h_flex()
            .relative()
            .flex_1()
            .min_h_0()
            .items_start()
            .when(layout.sidebar == SidebarMode::Docked, |body| {
                body.child(self.render_sidebar(false, window, cx))
            })
            .child(main)
            .when(layout.inspector == InspectorMode::Docked, |body| {
                body.child(self.render_inspector(false, window, cx))
            })
            .when_some(layout.modal(), |body, region| {
                body.child(self.render_overlay(region, window, cx))
            })
    }
}

impl Shell {
    /// What the palette's status line says: why a command did not run, how
    /// many commands match and which is chosen. It is a polite live region,
    /// so a screen reader hears the choice as it moves even where the
    /// platform does not follow the chosen row as focus.
    fn palette_status(&self, entries: &[commands::Entry], cx: &App) -> String {
        let Some(palette) = &self.palette else {
            return String::new();
        };
        if let Some(notice) = &palette.notice {
            return notice.to_string();
        }
        let Some(index) = self.palette_choice(entries.len()) else {
            let query = palette.query.read(cx).value();
            return format!("No command matches \u{201c}{query}\u{201d}.");
        };
        let entry = &entries[index];
        let mut status = format!(
            "{} of {} commands: {}",
            index + 1,
            entries.len(),
            entry.title
        );
        if let Some(shortcut) = entry.shortcut() {
            status.push_str(&format!(", {shortcut}"));
        }
        if let Some(reason) = entry.disabled_reason {
            status.push_str(&format!(", unavailable: {reason}"));
        }
        status.push('.');
        status
    }

    /// The command palette: a modal dialog with a search field, the
    /// commands that match it and a status line. Focus stays in the search
    /// field; Up and Down choose a row, Enter or a click runs it.
    fn render_palette(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette.as_ref().expect("the palette is open");
        let entries = self.palette_entries(cx);
        let selected = self.palette_choice(entries.len());
        let status = self.palette_status(&entries, cx);
        let viewport = window.viewport_size();
        let margin = px(16.);
        let width = self.scaled(560.).min(viewport.width - margin * 2.);
        let muted = cx.theme().muted_foreground;
        let rows = entries.into_iter().enumerate().map(|(index, entry)| {
            let chosen = Some(index) == selected;
            let command = entry.command;
            let shortcut = entry.shortcut();
            let description = match (entry.disabled_reason, &shortcut) {
                (Some(reason), _) => format!("Unavailable: {reason}"),
                (None, Some(shortcut)) => format!("{}, {shortcut}", entry.group.label()),
                (None, None) => entry.group.label().to_string(),
            };
            // The group and the shortcut go under the title, not beside it,
            // so a narrow window at a large text size does not squeeze the
            // title into a tall column.
            let meta = match &shortcut {
                Some(shortcut) => format!("{} \u{b7} {shortcut}", entry.group.label()),
                None => entry.group.label().to_string(),
            };
            v_flex()
                .id(SharedString::from(format!("palette-row-{index}")))
                .test_support()
                .role(Role::ListBoxOption)
                .aria_label(entry.title.clone())
                .aria_selected(chosen)
                .aria_description(description)
                .when_some(shortcut.clone(), |row, shortcut| {
                    row.aria_keyshortcuts(shortcut)
                })
                .when(chosen, |row| row.aria_active_descendant())
                .w_full()
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(chosen, |row| {
                    row.bg(cx.theme().accent)
                        .text_color(cx.theme().accent_foreground)
                })
                .child(div().text_sm().child(entry.title.clone()))
                .child(div().text_xs().child(meta))
                .when_some(entry.disabled_reason, |row, reason| {
                    row.child(div().text_xs().child(reason))
                })
                .when(!chosen, |row| row.text_color(muted))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_from_palette(command, window, cx)
                }))
        });
        let dialog = v_flex()
            .id("palette-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label("Commands")
            .aria_description(PALETTE_DESCRIPTION)
            .map(modal_dialog)
            .key_context(PALETTE_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectPreviousCommand, window, cx| {
                this.move_palette_selection(-1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectNextCommand, window, cx| {
                this.move_palette_selection(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RunSelectedCommand, window, cx| {
                this.run_selected_command(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ClosePalette, window, cx| this.dismiss_palette(window, cx)),
            )
            .w(width)
            .max_h(viewport.height - margin * 2.)
            .p_2()
            .gap_2()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .child(Input::new(&palette.query).aria_label("Search commands"))
            .child(
                announced(div().id("palette-status"))
                    .test_support()
                    .role(Role::Label)
                    .aria_value(status.clone())
                    // One line on screen, so a short window keeps its room
                    // for the list; the whole text is the node's value.
                    .w_full()
                    .flex_shrink_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(status),
            )
            .child(
                v_flex()
                    .id("palette-list")
                    .test_support()
                    .role(Role::ListBox)
                    .aria_label("Commands")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.palette_scroll)
                    .children(rows),
            )
            .focus_trap("palette", &self.palette_focus);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .id("palette-backdrop")
                    .test_support()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(cx.theme().overlay)
                    .occlude()
                    .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx))),
            )
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .justify_center()
                    .items_start()
                    .pt(margin)
                    .child(dialog),
            )
    }
}

impl Shell {
    /// The full-output view: a modal dialog with what the block is, Copy
    /// and Close, and the block's rows in a list that lays out only what is
    /// on screen. Each row is a line of text to assistive technology.
    fn render_viewer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewer = self.viewer.as_ref().expect("the viewer is open");
        let dialog_size = Self::viewer_size(window);
        let (kind, bytes) = {
            let blocks = self.blocks.borrow();
            blocks
                .block(viewer.block_id)
                .map(|block| (Some(block.kind), block.markdown().len()))
                .unwrap_or((None, 0))
        };
        let count = if kind.is_some() {
            viewer.starts.len()
        } else {
            0
        };
        let mut summary = match (kind, viewer.error) {
            (Some(kind), Some(error)) => format!(
                "{} block, {}. {}",
                kind.label(),
                format_bytes(bytes),
                error.notice()
            ),
            (Some(kind), None) => format!(
                "{} block, {}, {count} rows fitted to the view.",
                kind.label(),
                format_bytes(bytes)
            ),
            (None, _) => "This block is no longer in the timeline.".to_string(),
        };
        if let Some(reason) = self.transcript_reload_needed() {
            summary.insert_str(0, &format!("Reload needed: {} ", reason.notice()));
        }
        if viewer.copied {
            summary.insert_str(0, "Copied to the clipboard. ");
        }
        let rows = gpui_kit::uniform_list(
            "viewer-rows",
            count,
            cx.processor(|this, range: Range<usize>, _, _| {
                let blocks = this.blocks.borrow();
                let Some(viewer) = this.viewer.as_ref() else {
                    return Vec::new();
                };
                let Some(block) = blocks.block(viewer.block_id) else {
                    return Vec::new();
                };
                range
                    .filter(|index| *index < viewer.starts.len())
                    .map(|index| {
                        let text = full_view::row(block.markdown(), &viewer.starts, index);
                        let text = SharedString::from(text.to_string());
                        div()
                            .id(("viewer-row", index))
                            .test_support()
                            .role(Role::Label)
                            .aria_value(text.clone())
                            .whitespace_nowrap()
                            .overflow_hidden()
                            // An empty row is as tall as any other.
                            .child(if text.is_empty() { " ".into() } else { text })
                    })
                    .collect()
            }),
        )
        .track_scroll(&viewer.scroll)
        .flex_1()
        .min_h_0()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(cx.theme().mono_font_size);
        let dialog = v_flex()
            .id("viewer-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label("Whole block")
            .aria_description(VIEWER_DESCRIPTION)
            .map(modal_dialog)
            .key_context(VIEWER_CONTEXT)
            .on_action(cx.listener(|this, _: &ScrollViewerUp, window, cx| {
                this.scroll_viewer(WindowMove::Earlier, 1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerDown, window, cx| {
                this.scroll_viewer(WindowMove::Later, 1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageUp, window, cx| {
                this.scroll_viewer(WindowMove::Earlier, 0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageDown, window, cx| {
                this.scroll_viewer(WindowMove::Later, 0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerStart, window, cx| {
                this.scroll_viewer(WindowMove::Start, 0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerEnd, window, cx| {
                this.scroll_viewer(WindowMove::End, 0, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &CloseViewer, window, cx| this.close_viewer(window, cx)),
            )
            .w(dialog_size.width)
            .h(dialog_size.height)
            .p_2()
            .gap_2()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .child(
                // The summary gets the full width. Beside two buttons it
                // became a tall sliver at 480x360/200%, escaping the dialog.
                v_flex()
                    .gap_2()
                    .child(
                        announced(div().id("viewer-summary"))
                            .test_support()
                            .role(Role::Label)
                            .aria_value(summary.clone())
                            .w_full()
                            .min_w_0()
                            // Keep Copy/Close inside even at 200% after
                            // a long notice or copied-state update.
                            .max_h(dialog_size.height * 0.5)
                            .overflow_y_scroll()
                            .track_scroll(&viewer.summary_scroll)
                            .track_focus(&viewer.summary_focus)
                            .vertical_scrollbar(&viewer.summary_scroll)
                            .text_sm()
                            .child(summary),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("viewer-copy")
                                    .small()
                                    .label("Copy all")
                                    .tooltip("Copy the whole block to the clipboard")
                                    .disabled(kind.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.copy_viewed_block(cx)),
                                    ),
                            )
                            .child(
                                Button::new("viewer-close")
                                    .small()
                                    .label("Close")
                                    .tooltip("Close (Escape)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_viewer(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(rows)
            .focus_trap("viewer", &self.viewer_focus);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .id("viewer-backdrop")
                    .test_support()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(cx.theme().overlay)
                    .occlude()
                    .on_click(cx.listener(|this, _, window, cx| this.close_viewer(window, cx))),
            )
            .child(div().absolute().top(px(16.)).left(px(16.)).child(dialog))
    }
}
