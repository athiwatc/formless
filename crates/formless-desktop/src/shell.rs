//! The Phase 0 shell: one window laid out as sidebar, conversation with
//! composer, context inspector and status line, built from GPUI Kit controls.
//!
//! This is a scaffold, not a client. It renders the synthetic
//! [`crate::fixture`] only and has no Daemon Core connection: composer
//! actions are recorded as [`Intent`]s and shown in the status line, never
//! sent, and the draft stays in place. Only Chat has a view; every other
//! destination shows a notice saying so.

use std::collections::HashSet;
use std::rc::Rc;

use formless_protocol::SessionInfo;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::label::Label;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    h_flex, v_flex, v_virtual_list, ActiveTheme as _, Disableable as _, FocusTrapElement as _,
    Selectable as _, Sizable as _, Theme, ThemeMode, VirtualListScrollHandle,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, size, AnyElement, App, Context, ElementId, Entity, FocusHandle, Focusable as _, Hsla,
    KeyBinding, Pixels, ScrollStrategy, SharedString, Size, Subscription, TestSupportExt as _,
    Window,
};

use crate::model::{
    send_mode_label, tokens, ComposerActions, Destination, Drafts, InspectorMode, Intent, Layout,
    PanelPrefs, Region, RunState, Sidebar, SidebarMode, TimelineBlock,
};

gpui_kit::actions!(
    formless_desktop,
    [
        FocusNextRegion,
        FocusPreviousRegion,
        ToggleSidebar,
        ToggleInspector,
        DismissOverlay,
        SelectNextSession,
        SelectPreviousSession,
        ActivateSession,
    ]
);

const SESSION_LIST_CONTEXT: &str = "FormlessSessionList";
const OVERLAY_CONTEXT: &str = "FormlessOverlay";
const SESSION_ROW_HEIGHT: f32 = 44.0;
/// Draft retention: at most this many Sessions and this much text in total.
const DRAFT_MAX_ENTRIES: usize = 64;
const DRAFT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Region and panel shortcuts. F6 / Shift+F6 move between regions; nothing
/// here binds a bare printable key, so typing in the composer is untouched.
pub fn bind_keys(cx: &mut App) {
    #[cfg(target_os = "macos")]
    const PANEL_KEYS: [&str; 2] = ["cmd-b", "cmd-alt-b"];
    #[cfg(not(target_os = "macos"))]
    const PANEL_KEYS: [&str; 2] = ["ctrl-b", "ctrl-alt-b"];

    cx.bind_keys([
        KeyBinding::new("f6", FocusNextRegion, None),
        KeyBinding::new("shift-f6", FocusPreviousRegion, None),
        KeyBinding::new(PANEL_KEYS[0], ToggleSidebar, None),
        KeyBinding::new(PANEL_KEYS[1], ToggleInspector, None),
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

pub struct Shell {
    sidebar: Sidebar,
    blocks: Rc<Vec<TimelineBlock>>,
    drafts: Drafts,
    prefs: PanelPrefs,
    destination: Destination,
    last_intent: Option<Intent>,
    /// The overlay that held the window modally at the last settle.
    modal: Option<Region>,
    /// Where focus was when the current overlay came up.
    restore_focus: Option<Region>,
    timeline: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    session_scroll: VirtualListScrollHandle,
    session_sizes: Rc<Vec<Size<Pixels>>>,
    sidebar_focus: FocusHandle,
    conversation_focus: FocusHandle,
    inspector_focus: FocusHandle,
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
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder("Message Formless")
        });
        let subscriptions = vec![
            cx.subscribe(&composer, |this, _, event: &InputEvent, cx| {
                match event {
                    // Plain Enter is the primary action; the secondary chord
                    // is the secondary one (Queue during a Run). Shift+Enter
                    // also reports PressEnter, but it has already inserted a
                    // newline.
                    InputEvent::PressEnter {
                        secondary,
                        shift: false,
                    } => this.submit(*secondary, cx),
                    // Button availability depends on whether there is a draft.
                    InputEvent::Change => cx.notify(),
                    _ => {}
                }
            }),
            // A resize can end an overlay (the panel docks again) or hide the
            // panel that holds focus.
            cx.observe_window_bounds(window, |this, window, cx| this.settle(window, cx)),
        ];
        composer.update(cx, |composer, cx| composer.focus(window, cx));

        let session_sizes = Self::row_sizes(sidebar.rows().len());
        Self {
            sidebar,
            blocks: Rc::new(blocks),
            drafts: Drafts::new(DRAFT_MAX_ENTRIES, DRAFT_MAX_BYTES),
            prefs: PanelPrefs::default(),
            destination: Destination::Chat,
            last_intent: None,
            modal: None,
            restore_focus: None,
            timeline,
            composer,
            session_scroll: VirtualListScrollHandle::new(),
            session_sizes,
            sidebar_focus: cx.focus_handle(),
            conversation_focus: cx.focus_handle(),
            inspector_focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn row_sizes(count: usize) -> Rc<Vec<Size<Pixels>>> {
        Rc::new(vec![
            size(
                px(tokens::SIDEBAR_DEFAULT),
                px(SESSION_ROW_HEIGHT)
            );
            count
        ])
    }

    pub fn sidebar(&self) -> &Sidebar {
        &self.sidebar
    }

    pub fn destination(&self) -> Destination {
        self.destination
    }

    pub fn last_intent(&self) -> Option<&Intent> {
        self.last_intent.as_ref()
    }

    pub fn draft(&self, cx: &App) -> SharedString {
        self.composer.read(cx).value()
    }

    pub fn layout(&self, window: &Window) -> Layout {
        Layout::for_width(f32::from(window.viewport_size().width), self.prefs)
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
        let mode = match (secondary, actions.secondary) {
            (true, Some(mode)) => mode,
            (true, None) => return,
            (false, _) => actions.primary,
        };
        let Some(session_id) = self.sidebar.active_id().map(str::to_string) else {
            return;
        };
        self.last_intent = Some(Intent::SendMessage {
            session_id,
            content: self.draft(cx).to_string(),
            mode,
        });
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.composer_actions(cx).stop_enabled {
            return;
        }
        if let Some(session_id) = self.sidebar.active_id().map(str::to_string) {
            self.last_intent = Some(Intent::InterruptRun { session_id });
            cx.notify();
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
                self.drafts.set(&previous, &draft);
            }
            let restored = self
                .sidebar
                .active_id()
                .map(|id| self.drafts.get(id).to_string())
                .unwrap_or_default();
            self.composer
                .update(cx, |composer, cx| composer.set_value(restored, window, cx));
            let count = self.blocks.len();
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

    fn current_region(&self, window: &Window, cx: &App) -> Region {
        // The composer is drawn inside the conversation column, so it has to
        // be told apart from the conversation before the containment checks.
        if self.composer.focus_handle(cx).contains_focused(window, cx) {
            Region::Composer
        } else if self.sidebar_focus.contains_focused(window, cx) {
            Region::Sidebar
        } else if self.inspector_focus.contains_focused(window, cx) {
            Region::Inspector
        } else if self.conversation_focus.contains_focused(window, cx) {
            Region::Conversation
        } else {
            Region::Composer
        }
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
    /// An overlay that comes up takes focus and remembers where focus was.
    /// When it goes away focus returns there. Otherwise focus only moves if
    /// the region holding it is no longer on screen.
    fn settle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.settle(f32::from(window.viewport_size().width));
        let layout = self.layout(window);
        let focused = self.current_region(window, cx);
        let before = std::mem::replace(&mut self.modal, layout.modal());
        let fallback = if layout.composer {
            Region::Composer
        } else {
            Region::Conversation
        };
        match (before, self.modal) {
            (before, Some(modal)) if before != Some(modal) => {
                if before.is_none() {
                    self.restore_focus = Some(focused);
                }
                self.focus_region(modal, window, cx);
            }
            (Some(_), None) => {
                let target = self
                    .restore_focus
                    .take()
                    .filter(|region| region.is_visible(layout))
                    .unwrap_or(fallback);
                self.focus_region(target, window, cx);
            }
            _ if !focused.is_visible(layout) => self.focus_region(fallback, window, cx),
            _ => {}
        }
        cx.notify();
    }

    fn toggle_panel(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
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
            .h(px(44.))
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
            .key_context(SESSION_LIST_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNextSession, _, cx| this.move_selection(1, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPreviousSession, _, cx| this.move_selection(-1, cx)),
            )
            .on_action(cx.listener(|this, _: &ActivateSession, window, cx| {
                this.activate_selected(window, cx)
            }))
            .w(px(tokens::SIDEBAR_DEFAULT))
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
        );
        v_flex()
            .id(session_row_id(&row.id))
            .test_support()
            .h(px(SESSION_ROW_HEIGHT))
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
        let focused = self.conversation_focus.is_focused(window);
        let timeline = MessageScroller::new("timeline", self.timeline.clone(), {
            move |index, _, cx| {
                let block = &blocks[index];
                v_flex()
                    .id(("block", block.id as usize))
                    .test_support()
                    .w_full()
                    .max_w(px(tokens::READING_MAX))
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
                        TextView::markdown(("markdown", block.id as usize), block.markdown.clone())
                            .selectable(true),
                    )
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
                            .child(format!("{} blocks", self.blocks.len())),
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
                    .max_w(px(480.))
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
            .child(Textarea::new(&self.composer).aria_label("Message").w_full())
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
            .w(px(tokens::INSPECTOR_DEFAULT))
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
            Some(Intent::SendMessage { mode, content, .. }) => format!(
                "Recorded {} ({} characters), not sent",
                send_mode_label(*mode),
                content.chars().count()
            ),
            Some(Intent::InterruptRun { .. }) => "Recorded Stop, not sent".to_string(),
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
            .h(px(28.))
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
                "{destination} · inspector {}",
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
