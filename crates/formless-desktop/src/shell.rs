//! The Phase 0 shell: one window laid out as sidebar, conversation with
//! composer, context inspector and status line, built from GPUI Kit controls.
//!
//! This is a scaffold, not a client. It renders the synthetic
//! [`crate::fixture`] only and has no Daemon Core connection: composer
//! actions are recorded as [`Intent`]s and shown in the status line, never
//! sent, and the draft stays in place. Only Chat has a view; every other
//! destination shows a notice saying so.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use formless_protocol::SessionInfo;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState, Undo};
use gpui_kit::component::label::Label;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    h_flex, v_flex, v_virtual_list, ActiveTheme as _, Disableable as _, FocusTrapElement as _,
    Selectable as _, Sizable as _, Theme, ThemeMode, VirtualListScrollHandle,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, size, AnyElement, App, ClipboardItem, Context, ElementId, Entity,
    EntityInputHandler as _, FocusHandle, Focusable as _, Hsla, KeyBinding, Pixels, Role,
    ScrollStrategy, SharedString, Size, Subscription, TestSupportExt as _, Window,
};

use crate::memory::{
    edit_span, format_bytes, limits, paste_can_never_fit, snap_to_graphemes, within_composer_limit,
    IntentKind, IntentRecord, UndoBudget,
};
use crate::model::{
    send_mode_label, tokens, BlockKind, ComposerActions, Destination, Drafts, InspectorMode,
    Intent, Layout, PanelPrefs, Region, RunState, Sidebar, SidebarMode, TextScale,
};
use crate::timeline::{Coalescer, Timeline, TimelineBlock, FLUSH_INTERVAL_MS};

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
    ]
);

const SESSION_LIST_CONTEXT: &str = "FormlessSessionList";
const OVERLAY_CONTEXT: &str = "FormlessOverlay";
/// Read out with a modal panel's name. The kit has no "modal" accessibility
/// property to set, so the behaviour is also stated in words.
const MODAL_DESCRIPTION: &str =
    "Modal panel. Press Escape to close it and return to where you were.";
const SESSION_ROW_HEIGHT: f32 = 44.0;
const HEADER_HEIGHT: f32 = 44.0;
const STATUS_HEIGHT: f32 = 28.0;
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
    ]);
}

/// Element id of a Session's sidebar row. It is derived from the Session id,
/// not the row's position, so the element keeps its identity (and with it
/// hover, focus and scroll state) when the list reorders.
pub fn session_row_id(session_id: &str) -> ElementId {
    ElementId::from(SharedString::from(format!("session-row-{session_id}")))
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

pub struct Shell {
    sidebar: Sidebar,
    /// Shared with the timeline's row renderer.
    blocks: Rc<RefCell<Timeline>>,
    /// Collapses stream deltas into one view update per flush interval.
    stream: Coalescer,
    drafts: Drafts,
    prefs: PanelPrefs,
    /// The text size. Fixed sizes and layout breakpoints follow it.
    text_scale: TextScale,
    destination: Destination,
    /// What the last composer action was. Sizes only: no message text is
    /// kept once an intent has been handed on.
    last_intent: Option<IntentRecord>,
    /// The composer's text as of its last accepted change: always within
    /// the limit, and what an over-limit edit is rolled back to.
    accepted_draft: SharedString,
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
    /// The header toggles own stable handles so focus can return to them.
    sidebar_toggle_focus: FocusHandle,
    inspector_toggle_focus: FocusHandle,
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
            destination: Destination::Chat,
            last_intent: None,
            accepted_draft: SharedString::default(),
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
    /// of another's undo and how the history is bounded. Focus stays in the
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
    /// before it happens, so the rejection is made afterwards, in two steps:
    ///
    /// 1. The kit is asked to undo its own last edit. That puts back exactly
    ///    the previous text and selection and keeps the earlier undo history.
    /// 2. [`Self::finish_rejection`] checks the result. If the composer does
    ///    not hold exactly the previous draft, the previous draft is put back
    ///    whole by rebuilding the composer, at the cost of its undo history.
    fn composer_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        if draft == self.accepted_draft {
            return;
        }
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
        if !within_composer_limit(&draft) {
            let previous = self.accepted_draft.clone();
            let selection = snap_to_graphemes(&previous, span.start..span.old_end);
            self.composer_notice = Some(
                format!(
                    "Not added: the message would be {} and the limit is {}. Your text is \
                     unchanged.",
                    format_bytes(draft.len()),
                    format_bytes(limits::COMPOSER_MAX_BYTES)
                )
                .into(),
            );
            // The action only reaches the composer while it has focus.
            if self.composer.focus_handle(cx).contains_focused(window, cx) {
                window.dispatch_action(Box::new(Undo), cx);
            }
            // Queued behind the undo, and not run from inside the composer's
            // own event delivery.
            cx.defer_in(window, move |this, window, cx| {
                this.finish_rejection(previous, selection, window, cx)
            });
            cx.notify();
            return;
        }
        self.accepted_draft = draft.clone();
        self.undo_budget.record_edit(span);
        if self.undo_budget.is_spent() {
            let selection = self.composer.read(cx).selected_range();
            self.composer_notice = Some(
                "Undo history was cleared to bound memory after large edits. Your text is \
                 unchanged."
                    .into(),
            );
            cx.defer_in(window, move |this, window, cx| {
                this.rebuild_composer(&draft, Some(selection), window, cx)
            });
        } else {
            self.composer_notice = None;
        }
        // Button availability depends on whether there is a draft.
        cx.notify();
    }

    /// Second step of rejecting an over-limit edit. The kit's undo may have
    /// done exactly what was needed. It may also have undone more (it merges
    /// quick typing into one step) or nothing (the composer did not have
    /// focus). Either way the composer must end up holding `previous`.
    fn finish_rejection(
        &mut self,
        previous: SharedString,
        selection: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.draft(cx) == previous {
            return;
        }
        self.rebuild_composer(&previous, Some(selection), window, cx);
        if let Some(notice) = self.composer_notice.take() {
            self.composer_notice = Some(format!("{notice} Undo history was cleared.").into());
        }
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

    /// The window width the layout breakpoints see: the real width divided
    /// by the text size factor.
    fn effective_width(&self, window: &Window) -> f32 {
        f32::from(window.viewport_size().width) / self.text_scale.factor()
    }

    pub fn text_scale(&self) -> TextScale {
        self.text_scale
    }

    /// Change the text size. The kit theme's font sizes drive every
    /// rem-based size (text, controls); the shell's own fixed sizes and its
    /// breakpoints follow the same factor, so the layout may change, and
    /// focus is settled as for a resize.
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
        let theme = Theme::global_mut(cx);
        theme.font_size = px(BASE_FONT_SIZE * scale.factor());
        theme.mono_font_size = px(BASE_MONO_FONT_SIZE * scale.factor());
        self.session_sizes = Self::row_sizes(self.sidebar.rows().len(), scale);
        cx.refresh_windows();
        self.settle(window, cx);
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

    /// Start a streamed assistant block at the tail of the timeline.
    pub fn begin_stream(&mut self, cx: &mut Context<Self>) {
        self.blocks.borrow_mut().begin_stream(BlockKind::Assistant);
        self.request_flush(cx);
    }

    /// Append streamed text to the tail. The data is updated at once; the
    /// view is told at most once per flush interval.
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
        let count = self.blocks.borrow().len();
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
                let draft = self.draft(cx);
                // Parking a draft can push older ones out. That discards
                // text someone wrote, so it is said, not done silently.
                // An input method can leave text past the limit in the
                // composer with no signal (see `composer_changed`). Such a
                // draft is not parked; the last accepted one is.
                let draft = if within_composer_limit(&draft) {
                    draft
                } else {
                    self.accepted_draft.clone()
                };
                let evicted = self.drafts.set(&previous, &draft);
                self.composer_notice = (!evicted.is_empty()).then(|| {
                    format!(
                        "Unsent drafts for {} older Session(s) were discarded: stored drafts \
                         are limited to {} Sessions and {} in total.",
                        evicted.len(),
                        limits::DRAFT_MAX_ENTRIES,
                        format_bytes(limits::DRAFT_MAX_BYTES)
                    )
                    .into()
                });
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

    fn render_header(&self, layout: Layout, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .sidebar
            .active_row()
            .map_or_else(|| "No Session".into(), |row| row.title.clone());
        h_flex()
            .id("header")
            .test_support()
            .h(self.scaled(HEADER_HEIGHT))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Fixture Profile /"),
            )
            .child(div().flex_1().min_w_0().truncate().child(title))
            .child(
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
            )
            .child(
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
            )
            .child(
                Button::new("toggle-theme")
                    .small()
                    .ghost()
                    .label(if cx.theme().is_dark() {
                        "Light"
                    } else {
                        "Dark"
                    })
                    .tooltip("Switch between the light and dark theme")
                    .on_click(|_, window, cx| {
                        let next = if cx.theme().is_dark() {
                            ThemeMode::Light
                        } else {
                            ThemeMode::Dark
                        };
                        Theme::change(next, Some(window), cx);
                    }),
            )
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
            .w(self.scaled(tokens::SIDEBAR_DEFAULT))
            .flex_shrink_0()
            .h_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_r_1()
            .border_color(region_border(focused, cx.theme().sidebar_border, cx))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Sessions ({})", self.sidebar.rows().len())),
            )
            .child(
                div().flex_1().min_h_0().child(
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

    fn render_conversation(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let run_state = self.run_state();
        let blocks = self.blocks.clone();
        let reading_width = self.scaled(tokens::READING_MAX);
        let focused = self.conversation_focus.is_focused(window);
        let timeline = MessageScroller::new("timeline", self.timeline.clone(), {
            move |index, _, cx| {
                let blocks = blocks.borrow();
                let Some(block) = blocks.blocks().get(index) else {
                    return div().into_any_element();
                };
                let id = block.id as usize;
                // Only the preview is laid out. A block of any size costs a
                // bounded amount per frame, and the row says when it is cut.
                let truncated = block.is_truncated().then(|| {
                    div()
                        .id(("block-truncated", id))
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(Label::new(format!(
                            "Preview: the first {} of {} is shown. The rest is not laid out, \
                             and this scaffold has no full-output viewer.",
                            format_bytes(block.preview().len()),
                            format_bytes(block.total_bytes())
                        )))
                });
                v_flex()
                    .id(("block", id))
                    .test_support()
                    .w_full()
                    .max_w(reading_width)
                    .mx_auto()
                    .px_4()
                    .py_2()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(block.kind.label()),
                    )
                    .child(
                        TextView::markdown(
                            ("markdown", id),
                            SharedString::from(block.preview().clone()),
                        )
                        .selectable(true),
                    )
                    .children(truncated)
                    .into_any_element()
            }
        });
        v_flex()
            .id("region-conversation")
            .test_support()
            .track_focus(&self.conversation_focus)
            .flex_1()
            .min_w_0()
            .h_full()
            .border_1()
            .border_color(region_border(focused, cx.theme().background, cx))
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(format!("{} blocks", self.blocks.borrow().len())),
                    )
                    .child(
                        div()
                            .id("run-status")
                            .test_support()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Run: {}", run_state.label())),
                    ),
            )
            .child(div().flex_1().min_h_0().child(timeline))
            .child(self.render_composer(cx))
    }

    /// Every destination but Chat. It takes the conversation's place and
    /// says plainly that nothing is here, so a working navigation button is
    /// never mistaken for a working page.
    fn render_scaffold_notice(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.conversation_focus.is_focused(window);
        let name = self.destination.label();
        v_flex()
            .id("region-conversation")
            .test_support()
            .track_focus(&self.conversation_focus)
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .border_1()
            .border_color(region_border(focused, cx.theme().background, cx))
            .child(Label::new(format!("{name}: not built")))
            .child(
                div()
                    .max_w(self.scaled(480.))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Label::new(format!(
                        "This Phase 0 scaffold only has a Chat view. {name} has no screen, \
                         data or actions here, and this notice is not a preview of one."
                    ))),
            )
            .child(
                Button::new("back-to-chat")
                    .outline()
                    .label("Back to Chat")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_destination(Destination::Chat, window, cx)
                    })),
            )
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
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(Label::new(notice))
            }))
            .child(
                h_flex()
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
        let detail = |label: &'static str, value: String| {
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(div().text_sm().child(value))
        };
        let panel = v_flex()
            .id("region-inspector")
            .test_support()
            .role(if modal {
                Role::Dialog
            } else {
                Role::Complementary
            })
            .aria_label("Context")
            .when(modal, |panel| panel.aria_description(MODAL_DESCRIPTION))
            .w(self.scaled(tokens::INSPECTOR_DEFAULT))
            .flex_shrink_0()
            .h_full()
            .p_3()
            .gap_3()
            .bg(cx.theme().background)
            .border_l_1()
            .border_color(region_border(focused, cx.theme().border, cx))
            .child(div().text_sm().child("Context"))
            .child(detail(
                "Session",
                row.map_or_else(|| "None".to_string(), |row| row.title.clone()),
            ))
            .child(detail(
                "Session id",
                row.map_or_else(|| "None".to_string(), |row| row.id.clone()),
            ))
            .child(detail("Run", self.run_state().label().to_string()))
            .child(detail(
                "Last message",
                row.and_then(|row| row.preview.clone())
                    .unwrap_or_else(|| "None".to_string()),
            ));
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
    /// dismisses too. The header, and so the toggle that opened it, stays
    /// outside and usable.
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

    fn render_status(&self, layout: Layout, cx: &mut Context<Self>) -> impl IntoElement {
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
        h_flex()
            .id("status")
            .test_support()
            .h(self.scaled(STATUS_HEIGHT))
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .border_t_1()
            .border_color(cx.theme().border)
            .child("Synthetic fixture: no Daemon Core connection")
            .child(intent)
            .child(div().flex_1())
            .child(format!(
                "Text {}% · {destination} · inspector {}",
                self.text_scale.percent(),
                match layout.inspector {
                    InspectorMode::Docked => "docked",
                    InspectorMode::Overlay => "overlay",
                    InspectorMode::Hidden => "hidden",
                }
            ))
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.layout(window);
        let main = if self.destination == Destination::Chat {
            self.render_conversation(window, cx).into_any_element()
        } else {
            self.render_scaffold_notice(window, cx).into_any_element()
        };
        v_flex()
            .id("shell")
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
                this.set_text_scale(this.text_scale.larger(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &DecreaseTextSize, window, cx| {
                this.set_text_scale(this.text_scale.smaller(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResetTextSize, window, cx| {
                this.set_text_scale(TextScale::DEFAULT, window, cx)
            }))
            .child(self.render_header(layout, cx))
            .child(
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
                    }),
            )
            .child(self.render_status(layout, cx))
    }
}
