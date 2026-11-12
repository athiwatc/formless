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
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState, Undo};
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

use crate::memory::{
    edit_span, format_bytes, limits, paste_can_never_fit, snap_to_graphemes, within_composer_limit,
    IntentKind, IntentRecord, Rejections, UndoBudget,
};
use crate::model::{
    send_mode_label, tokens, BlockKind, ComposerActions, Destination, Drafts, Fit, InspectorMode,
    Intent, Layout, PanelPrefs, Region, RunState, Sidebar, SidebarMode, TextScale,
};
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
    ]
);

const SESSION_LIST_CONTEXT: &str = "FormlessSessionList";
const OVERLAY_CONTEXT: &str = "FormlessOverlay";
const INSPECTOR_CONTEXT: &str = "FormlessInspector";
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
/// Region and panel shortcuts. F6 / Shift+F6 move between regions; nothing
/// here binds a bare printable key, so typing in the composer is untouched.
pub fn bind_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    const PANEL_KEYS: [&str; 2] = ["cmd-b", "cmd-alt-b"];
    #[cfg(not(target_os = "macos"))]
    const PANEL_KEYS: [&str; 2] = ["ctrl-b", "ctrl-alt-b"];
    #[cfg(target_os = "macos")]
    const TEXT_SIZE_KEYS: [&str; 4] = ["cmd-=", "cmd-+", "cmd--", "cmd-0"];
    #[cfg(not(target_os = "macos"))]
    const TEXT_SIZE_KEYS: [&str; 4] = ["ctrl-=", "ctrl-+", "ctrl--", "ctrl-0"];

    cx.bind_keys([
        KeyBinding::new("f6", FocusNextRegion, None),
        KeyBinding::new("shift-f6", FocusPreviousRegion, None),
        KeyBinding::new(PANEL_KEYS[0], ToggleSidebar, None),
        KeyBinding::new(PANEL_KEYS[1], ToggleInspector, None),
        KeyBinding::new(TEXT_SIZE_KEYS[0], IncreaseTextSize, None),
        KeyBinding::new(TEXT_SIZE_KEYS[1], IncreaseTextSize, None),
        KeyBinding::new(TEXT_SIZE_KEYS[2], DecreaseTextSize, None),
        KeyBinding::new(TEXT_SIZE_KEYS[3], ResetTextSize, None),
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
    ]);
}

/// Element id of a Session's sidebar row. It is derived from the Session id,
/// not the row's position, so the element keeps its identity (and with it
/// hover, focus and scroll state) when the list reorders.
pub fn session_row_id(session_id: &str) -> ElementId {
    ElementId::from(SharedString::from(format!("session-row-{session_id}")))
}

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

/// A region's border colour: the focus ring while the region itself holds
/// focus, its ordinary border otherwise.
fn region_border(focused: bool, idle: Hsla, cx: &App) -> Hsla {
    if focused {
        cx.theme().ring
    } else {
        idle
    }
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
    /// The overlay that held the window modally at the last settle.
    modal: Option<Region>,
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
        let events = cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, _, cx| {
            // Plain Enter is the primary action; the secondary chord is
            // the secondary one (Queue during a Run). Shift+Enter also
            // reports PressEnter, but it has already inserted a newline.
            if let InputEvent::PressEnter {
                secondary,
                shift: false,
            } = event
            {
                this.submit(*secondary, cx)
            }
        });
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

    fn cycle_region(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
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
        let before = std::mem::replace(&mut self.modal, layout.modal());
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
                self.focus_region(modal, window, cx);
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
            Button::new("toggle-sidebar")
                .small()
                .ghost()
                .label("Sessions")
                .tooltip("Show or hide the Sessions sidebar")
                .track_focus(&self.sidebar_toggle_focus)
                .selected(layout.sidebar != SidebarMode::Collapsed)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_panel(Region::Sidebar, window, cx)
                })),
            Button::new("toggle-inspector")
                .small()
                .ghost()
                .label("Context")
                .tooltip("Show or hide the Context inspector")
                .track_focus(&self.inspector_toggle_focus)
                .selected(layout.inspector != InspectorMode::Hidden)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_panel(Region::Inspector, window, cx)
                })),
            Button::new("toggle-theme")
                .small()
                .ghost()
                .label(if cx.theme().is_dark() {
                    "Light"
                } else {
                    "Dark"
                })
                .tooltip("Switch between the light and dark theme")
                .track_focus(&self.theme_toggle_focus)
                .on_click(|_, window, cx| {
                    let next = if cx.theme().is_dark() {
                        ThemeMode::Light
                    } else {
                        ThemeMode::Dark
                    };
                    Theme::change(next, Some(window), cx);
                }),
        ];
        let header = div()
            .id("header")
            .test_support()
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
            .when(modal, |panel| panel.aria_description(MODAL_DESCRIPTION))
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
            move |index, _, cx| {
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
                        // lays out: the row's bounded window, as markdown
                        // source.
                        let text = SharedString::from(block.preview().clone());
                        div()
                            .id(("block-text", id))
                            .test_support()
                            .role(Role::Label)
                            .aria_value(text.clone())
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
            .when(!reflowed, |region| region.child(self.render_composer(cx)))
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

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let actions = self.composer_actions(cx);
        let disabled = actions.disabled_reason.is_some();
        let primary = Button::new("composer-primary")
            .primary()
            .label(send_mode_label(actions.primary))
            .disabled(disabled)
            .when_some(actions.disabled_reason, |button, reason| {
                button.tooltip(reason)
            })
            .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx)));
        let secondary = actions.secondary.map(|mode| {
            Button::new("composer-secondary")
                .outline()
                .label(send_mode_label(mode))
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| this.submit(true, cx)))
        });
        // Stop keeps its own fixed slot, apart from the primary action.
        let stop = Button::new("composer-stop")
            .danger()
            .label("Stop")
            .disabled(!actions.stop_enabled)
            .on_click(cx.listener(|this, _, _, cx| this.stop(cx)));
        v_flex()
            .id("region-composer")
            .test_support()
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
            .children(self.composer_notice.clone().map(|notice| {
                div()
                    .id("composer-notice")
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
            .when(modal, |panel| panel.aria_description(MODAL_DESCRIPTION))
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
        let intent = readable("status-intent", intent);
        let status = div()
            .id("status")
            .test_support()
            .role(Role::Status)
            .aria_label("Status")
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
                    .when(chat, |page| page.child(self.render_composer(cx)))
                    .child(self.render_status(layout, true, window, cx)),
            )
            .vertical_scrollbar(&self.page_scroll)
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.layout(window);
        let reflowed = self.fit(window) == Fit::Reflowed;
        v_flex()
            .id("shell")
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &FocusNextRegion, window, cx| {
                this.cycle_region(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousRegion, window, cx| {
                this.cycle_region(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_panel(Region::Sidebar, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleInspector, window, cx| {
                this.toggle_panel(Region::Inspector, window, cx)
            }))
            .on_action(cx.listener(|this, _: &IncreaseTextSize, window, cx| {
                this.set_text_scale_growing_window(this.text_scale.larger(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &DecreaseTextSize, window, cx| {
                this.set_text_scale_growing_window(this.text_scale.smaller(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResetTextSize, window, cx| {
                this.set_text_scale_growing_window(TextScale::DEFAULT, window, cx)
            }))
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
