//! The Phase 0 shell: one window laid out as sidebar, conversation with
//! composer, context inspector and status line, built from GPUI Kit controls.
//!
//! This is a scaffold, not a client. It renders the synthetic
//! [`crate::fixture`] only and has no Daemon Core connection: composer
//! actions are recorded as [`Intent`]s and shown in the status line, never
//! sent, and the draft stays in place. Only Chat has a view; every other
//! destination shows a notice saying so.

use std::cell::{Ref, RefCell};
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use formless_protocol::SessionInfo;
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

use crate::commands::{self, Command};
use crate::full_view;
use crate::memory::{
    edit_span, format_bytes, limits, paste_can_never_fit, snap_to_graphemes, within_composer_limit,
    IntentKind, IntentRecord, Rejections, UndoBudget,
};
use crate::model::{
    send_mode_label, tokens, BlockKind, ComposerActions, Destination, Drafts, Fit, InspectorMode,
    Intent, Layout, PanelPrefs, Region, RunState, Sidebar, SidebarMode, TextScale,
};
use crate::replies::SessionSnapshot;
use crate::timeline::{
    Anchor, Coalescer, StreamWork, Timeline, TimelineBlock, WindowMove, FLUSH_INTERVAL_MS,
};

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
        SelectPreviousCommand,
        SelectNextCommand,
        RunSelectedCommand,
        ClosePalette,
        ScrollViewerUp,
        ScrollViewerDown,
        ScrollViewerPageUp,
        ScrollViewerPageDown,
        ScrollViewerStart,
        ScrollViewerEnd,
        CloseViewer,
    ]
);

const SESSION_LIST_CONTEXT: &str = "FormlessSessionList";
const OVERLAY_CONTEXT: &str = "FormlessOverlay";
const INSPECTOR_CONTEXT: &str = "FormlessInspector";
const PALETTE_CONTEXT: &str = "FormlessPalette";
const VIEWER_CONTEXT: &str = "FormlessViewer";
const VIEWER_DESCRIPTION: &str =
    "Modal. The whole block as plain text. Arrow, Page Up, Page Down, \
     Home and End scroll; Escape closes and returns to where you were.";
/// The smallest row width the full-output view cuts a block into, in
/// characters, however narrow the window.
const VIEWER_MIN_COLUMNS: usize = 20;
/// The palette's search field. These bindings are more specific than the
/// text input's own, so Up, Down, Enter and Escape there move through and
/// run the list instead of moving a caret in a one-line field.
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
                Command::Send
                | Command::SendSecondary
                | Command::Stop
                | Command::ToggleTheme
                | Command::GoTo(_) => unreachable!("{command:?} has no global shortcut"),
            })
    });
    cx.bind_keys(global.collect::<Vec<_>>());
    cx.bind_keys([
        KeyBinding::new("escape", DismissOverlay, Some(OVERLAY_CONTEXT)),
        KeyBinding::new("down", SelectNextSession, Some(SESSION_LIST_CONTEXT)),
        KeyBinding::new("up", SelectPreviousSession, Some(SESSION_LIST_CONTEXT)),
        KeyBinding::new("enter", ActivateSession, Some(SESSION_LIST_CONTEXT)),
        // The inspector holds text only, so with focus in it the arrow and
        // page keys scroll it: in a short window it is taller than its panel.
        KeyBinding::new("up", ScrollInspectorUp, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("down", ScrollInspectorDown, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("pageup", ScrollInspectorPageUp, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("pagedown", ScrollInspectorPageDown, Some(INSPECTOR_CONTEXT)),
        KeyBinding::new("up", SelectPreviousCommand, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("down", SelectNextCommand, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("enter", RunSelectedCommand, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("escape", ClosePalette, Some(PALETTE_INPUT_CONTEXT)),
        KeyBinding::new("escape", ClosePalette, Some(PALETTE_CONTEXT)),
        KeyBinding::new("up", ScrollViewerUp, Some(VIEWER_CONTEXT)),
        KeyBinding::new("down", ScrollViewerDown, Some(VIEWER_CONTEXT)),
        KeyBinding::new("pageup", ScrollViewerPageUp, Some(VIEWER_CONTEXT)),
        KeyBinding::new("pagedown", ScrollViewerPageDown, Some(VIEWER_CONTEXT)),
        KeyBinding::new("home", ScrollViewerStart, Some(VIEWER_CONTEXT)),
        KeyBinding::new("end", ScrollViewerEnd, Some(VIEWER_CONTEXT)),
        KeyBinding::new("escape", CloseViewer, Some(VIEWER_CONTEXT)),
    ]);
}

/// Element id of a Session's sidebar row. It is derived from the Session id,
/// not the row's position, so the element keeps its identity (and with it
/// hover, focus and scroll state) when the list reorders.
pub fn session_row_id(session_id: &str) -> ElementId {
    ElementId::from(SharedString::from(format!("session-row-{session_id}")))
}

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
}

/// The open full-output view: one block, as display rows.
struct Viewer {
    block_id: u64,
    /// Where each row starts in the block ([`full_view::row_starts`]).
    starts: Vec<u32>,
    /// The row width `starts` was made for, in characters.
    columns: usize,
    scroll: gpui_kit::UniformListScrollHandle,
    /// Set once the block has been copied, to say so.
    copied: bool,
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
    composer: Entity<TextareaState>,
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
        let (composer, composer_subscriptions) = Self::build_composer("", window, cx);
        // A resize can end an overlay (the panel docks again) or hide the
        // panel that holds focus.
        let subscriptions =
            vec![cx.observe_window_bounds(window, |this, window, cx| this.settle(window, cx))];
        composer.update(cx, |composer, cx| composer.focus(window, cx));

        let session_sizes = Self::row_sizes(sidebar.rows().len(), TextScale::default());
        Self {
            sidebar,
            blocks: Rc::new(RefCell::new(Timeline::new(blocks))),
            stream: Coalescer::default(),
            drafts: Drafts::new(limits::DRAFT_MAX_ENTRIES, limits::DRAFT_MAX_BYTES),
            prefs: PanelPrefs::default(),
            text_scale: TextScale::default(),
            applied_scale: TextScale::default(),
            page_scroll: ScrollHandle::new(),
            reveal_frames: 0,
            inspector_scroll: ScrollHandle::new(),
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
                composing: false,
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
            composer,
            session_scroll: VirtualListScrollHandle::new(),
            session_sizes,
            sidebar_focus: cx.focus_handle(),
            conversation_focus: cx.focus_handle(),
            inspector_focus: cx.focus_handle(),
            sidebar_toggle_focus: cx.focus_handle(),
            inspector_toggle_focus: cx.focus_handle(),
            theme_toggle_focus: cx.focus_handle(),
            commands_toggle_focus: cx.focus_handle(),
            status_focus: cx.focus_handle().tab_stop(true),
            composer_subscriptions,
            _subscriptions: subscriptions,
        }
    }

    /// A composer holding `text`, with an empty undo history, and what
    /// routes its events and its changes to the shell.
    fn build_composer(
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextareaState>, [Subscription; 2]) {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
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
        let had_focus = self.composer.focus_handle(cx).contains_focused(window, cx);
        let (composer, subscriptions) = Self::build_composer(text, window, cx);
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
        if draft == self.accepted_draft {
            return;
        }
        // Typing brings the composer back into view on a reflowed page the
        // user had scrolled away from it.
        self.revealed = None;
        // Text still being composed by an input method is not judged: taking
        // it back mid-composition would fight the input method. It is judged
        // the moment the composition ends, committed or not.
        let composing = self.composer.update(cx, |composer, cx| {
            composer.marked_text_range(window, cx).is_some()
        });
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
        if self.palette.is_some() {
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
        if self.composer.focus_handle(cx).contains_focused(window, cx) {
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
    fn reveal_focused(&mut self, window: &mut Window, cx: &App) {
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

    /// Hand an intent on. There is no client layer to hand it to yet, so it
    /// is dropped here; only its record stays.
    fn emit(&mut self, intent: Intent, cx: &mut Context<Self>) {
        self.last_intent = Some(IntentRecord::from(&intent));
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
    /// included: the reply is the daemon's whole, current transcript.
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
        let count = snapshot.blocks.len();
        *self.blocks.borrow_mut() = Timeline::new(snapshot.blocks);
        self.timeline
            .update(cx, |timeline, cx| timeline.reset(count, cx));
        cx.notify();
        Ok(())
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
        let columns = self.viewer_columns(window, cx);
        let starts = {
            let blocks = self.blocks.borrow();
            let index = blocks
                .blocks()
                .iter()
                .position(|block| block.id == block_id)
                .ok_or("there is no such block")?;
            if blocks.is_streaming() && index + 1 == blocks.len() {
                return Err(VIEWER_STREAMING);
            }
            full_view::row_starts(blocks.blocks()[index].markdown(), columns)
                .ok_or("the block is too large to show whole")?
        };
        self.prefs.close_overlays();
        self.viewer = Some(Viewer {
            block_id,
            starts,
            columns,
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

    /// How many characters of the monospace font fit across the view's
    /// list, from the font's real advance.
    fn viewer_columns(&self, window: &Window, cx: &App) -> usize {
        let theme = cx.theme();
        let font_size = theme.mono_font_size;
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&gpui_kit::font(theme.mono_font_family.clone()));
        let advance = text_system
            .ch_advance(font_id, font_size)
            .unwrap_or(font_size * 0.6);
        // The dialog's padding and the list's scrollbar.
        let width = Self::viewer_size(window).width - px(48.);
        ((f32::from(width) / f32::from(advance)).floor() as usize).max(VIEWER_MIN_COLUMNS)
    }

    /// Cut the open view's block again when the width it fits has changed
    /// (a resize, a text size change), keeping the row at the top in view.
    fn fit_viewer(&mut self, window: &Window, cx: &App) {
        let columns = self.viewer_columns(window, cx);
        let blocks = self.blocks.borrow();
        let Some(viewer) = &mut self.viewer else {
            return;
        };
        if viewer.columns == columns {
            return;
        }
        let Some(block) = blocks.block(viewer.block_id) else {
            return;
        };
        let Some(starts) = full_view::row_starts(block.markdown(), columns) else {
            return;
        };
        let (top, _) = viewer.top_and_page();
        let top = viewer.starts.get(top).copied().unwrap_or(0);
        let row = starts
            .partition_point(|start| *start <= top)
            .saturating_sub(1);
        viewer.starts = starts;
        viewer.columns = columns;
        viewer
            .scroll
            .scroll_to_item_strict(row, ScrollStrategy::Top);
    }

    /// Move the view by `rows` rows, a page (`rows` 0), or to an end.
    fn scroll_viewer(&mut self, to: WindowMove, rows: isize, cx: &mut Context<Self>) {
        let Some(viewer) = &self.viewer else {
            return;
        };
        let last = viewer.starts.len().saturating_sub(1);
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

    fn copy_viewed_block(&mut self, cx: &mut Context<Self>) {
        let Some(viewer) = &mut self.viewer else {
            return;
        };
        let blocks = self.blocks.borrow();
        if let Some(block) = blocks.block(viewer.block_id) {
            cx.write_to_clipboard(ClipboardItem::new_string(block.markdown().to_string()));
            viewer.copied = true;
        }
        cx.notify();
    }

    /// How many rows the open full-output view has, for tests.
    pub fn viewer_rows(&self) -> Option<usize> {
        self.viewer.as_ref().map(|viewer| viewer.starts.len())
    }

    /// Start a streamed assistant block at the tail of the timeline.
    pub fn begin_stream(&mut self, cx: &mut Context<Self>) {
        self.blocks.borrow_mut().begin_stream(BlockKind::Assistant);
        self.request_flush(cx);
    }

    /// Append streamed text to the tail. The text is kept at once; the view
    /// is told at most once per flush interval, and the row is rebuilt only
    /// when it is next shown.
    pub fn push_stream_delta(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.blocks.borrow_mut().push_delta(text) {
            self.request_flush(cx);
        }
    }

    /// End the stream and show its final state without waiting.
    pub fn finish_stream(&mut self, cx: &mut Context<Self>) {
        self.blocks.borrow_mut().finish_stream();
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
        self.sidebar
            .active_row()
            .map_or(RunState::Idle, |row| row.run_state)
    }

    fn composer_actions(&self, cx: &App) -> ComposerActions {
        ComposerActions::for_state(self.run_state(), self.draft(cx).trim().is_empty())
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
            composing,
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
        let state = self.command_state(window, cx);
        if let Some(reason) = commands::entry(command, &state).disabled_reason {
            // Other refusals are as silent as they always were, but a
            // composition can start without the shell being redrawn, so a
            // Send button or Enter can still be pressed while it runs: the
            // composer says why nothing was recorded.
            if reason == commands::COMPOSING {
                self.composer_notice = Some(format!("Not sent: {reason}.").into());
                cx.notify();
            }
            return Some(reason);
        }
        match command {
            Command::Send => self.submit(false, cx),
            Command::SendSecondary => self.submit(true, cx),
            Command::Stop => self.stop(cx),
            Command::ToggleSessions => self.toggle_panel(Region::Sidebar, window, cx),
            Command::ToggleContext => self.toggle_panel(Region::Inspector, window, cx),
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
    /// the outgoing draft, restore the incoming one and return the timeline
    /// to its tail. An overlay that was used to pick it closes.
    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.sidebar.active_id().map(str::to_string);
        if self.sidebar.activate_selected() {
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
            let count = self.blocks.borrow().len();
            self.timeline
                .update(cx, |timeline, cx| timeline.reset(count, cx));
            self.last_intent = None;
        }
        self.destination = Destination::Chat;
        self.prefs.close_overlays();
        self.settle(window, cx);
    }

    fn set_destination(
        &mut self,
        destination: Destination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.destination = destination;
        self.prefs.close_overlays();
        self.settle(window, cx);
    }

    /// The region holding focus, or `None` when focus is in the header or
    /// nowhere.
    fn focused_region(&self, window: &Window, cx: &App) -> Option<Region> {
        // The composer is drawn inside the conversation column, so it has to
        // be told apart from the conversation before the containment checks.
        if self.composer.focus_handle(cx).contains_focused(window, cx) {
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
        }
    }

    fn cycle_region(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The palette and the full-output view trap the cycle too.
        if let Some(modal @ (Modal::Palette | Modal::Viewer)) =
            self.current_modal(self.layout(window))
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
        let profile = readable("header-profile", "Fixture Profile /")
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
        });
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
            if destination == Destination::Chat {
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
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(block.kind.label()),
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
            .child(
                div()
                    .id("timeline-area")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h_0()
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

    /// Every destination but Chat. It takes the conversation's place and
    /// says plainly that nothing is here, so a working navigation button is
    /// never mistaken for a working page.
    fn render_scaffold_notice(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.conversation_focus.is_focused(window);
        let reflowed = page_height.is_some();
        let name = self.destination.label();
        let explanation = div()
            .max_w(self.scaled(480.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Label::new(format!(
                "This Phase 0 scaffold only has a Chat view. {name} has no screen, \
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
                "This Phase 0 scaffold only has a Chat view. {name} has no screen, data or \
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
            notice.p_3().child(back).child(explanation)
        } else {
            notice.justify_center().p_6().child(explanation).child(back)
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
            .child(
                Textarea::new(&self.composer)
                    .aria_label("Message")
                    .on_paste({
                        let shell = cx.entity();
                        move |item, _, cx| shell.update(cx, |this, cx| this.paste_refused(item, cx))
                    })
                    .w_full(),
            )
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
            ));
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
        let intent = match &self.last_intent {
            Some(record) => match record.kind {
                IntentKind::Send(mode) => format!(
                    "Recorded {} ({} characters), not sent",
                    send_mode_label(mode),
                    record.content_chars
                ),
                IntentKind::Stop => "Recorded Stop, not sent".to_string(),
            },
            None => "No intent recorded".to_string(),
        };
        let destination = if self.destination == Destination::Chat {
            "Chat".to_string()
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
            "Synthetic fixture: no Daemon Core connection",
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
            .border_color(cx.theme().border);
        if reflowed {
            // One under the other, each wrapping to the window's width, so
            // nothing is cut off at the side.
            status
                .track_focus(&self.status_focus)
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
        // One screen of conversation at most, so the composer under it is
        // never more than one screen away.
        let height = window
            .viewport_size()
            .height
            .min(self.scaled(tokens::REFLOW_CONVERSATION_MIN));
        let chat = self.destination == Destination::Chat;
        let main = if chat {
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
    }
}

impl Shell {
    /// The row under the header where the shell is whole: docked panels on
    /// either side of the main part, and any overlay over all of it.
    fn render_body(
        &self,
        layout: Layout,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let main = if self.destination == Destination::Chat {
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
        let mut summary = match kind {
            Some(kind) => format!(
                "{} block, {}, {count} rows of up to {} characters.",
                kind.label(),
                format_bytes(bytes),
                viewer.columns
            ),
            None => "This block is no longer in the timeline.".to_string(),
        };
        if viewer.copied {
            summary.push_str(" Copied to the clipboard.");
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
            .on_action(cx.listener(|this, _: &ScrollViewerUp, _, cx| {
                this.scroll_viewer(WindowMove::Earlier, 1, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerDown, _, cx| {
                this.scroll_viewer(WindowMove::Later, 1, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageUp, _, cx| {
                this.scroll_viewer(WindowMove::Earlier, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageDown, _, cx| {
                this.scroll_viewer(WindowMove::Later, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerStart, _, cx| {
                this.scroll_viewer(WindowMove::Start, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerEnd, _, cx| {
                this.scroll_viewer(WindowMove::End, 0, cx)
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
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        announced(div().id("viewer-summary"))
                            .test_support()
                            .role(Role::Label)
                            .aria_value(summary.clone())
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .child(summary),
                    )
                    .child(
                        Button::new("viewer-copy")
                            .small()
                            .label("Copy all")
                            .tooltip("Copy the whole block to the clipboard")
                            .disabled(kind.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.copy_viewed_block(cx))),
                    )
                    .child(
                        Button::new("viewer-close")
                            .small()
                            .label("Close")
                            .tooltip("Close (Escape)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_viewer(window, cx)),
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
