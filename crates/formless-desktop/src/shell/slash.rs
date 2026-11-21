use super::*;
use crate::slash::{self, Arguments, Entry, Parsed};

#[derive(Default)]
pub(super) struct Popup {
    selected: usize,
    hidden: Option<String>,
    query: String,
    scroll: ScrollHandle,
}

impl Shell {
    pub(super) fn slash_changed(&mut self, cx: &App) {
        let draft = self.draft(cx);
        if self.slash.query != draft.as_ref() {
            self.slash.query = draft.to_string();
            self.slash.selected = 0;
            self.slash.scroll.set_offset(point(px(0.), px(0.)));
            if self.slash.hidden.as_deref() != Some(draft.as_ref()) {
                self.slash.hidden = None;
            }
        }
    }

    pub(super) fn slash_items(&self, window: &Window, cx: &App) -> Vec<&'static Entry> {
        let draft = self.draft(cx);
        if self.background_inert(window)
            || self.destination != Destination::Chat
            || self.forecast_composing
            || !self.composer.focus_handle(cx).contains_focused(window, cx)
            || self.slash.hidden.as_deref() == Some(draft.as_ref())
            || draft.starts_with("//")
        {
            return Vec::new();
        }
        let Some(query) = draft.strip_prefix('/') else {
            return Vec::new();
        };
        let name_end = draft.find(char::is_whitespace).unwrap_or(draft.len());
        let selection = self.composer_selection(cx);
        if selection.start != selection.end
            || selection.end > name_end
            || query.chars().any(char::is_whitespace)
        {
            return Vec::new();
        }
        slash::matches(query)
    }

    fn slash_selected(&self, count: usize, cx: &App) -> usize {
        if self.slash.query == self.draft(cx).as_ref() {
            self.slash.selected.min(count.saturating_sub(1))
        } else {
            0
        }
    }

    fn slash_notice(&mut self, notice: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.composer_notice = Some(notice.into());
        cx.notify();
    }

    pub(super) fn submit_slash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.background_inert(window) {
            return;
        }
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            self.slash_notice(commands::COMPOSING, cx);
            return;
        }
        let draft = self.draft(cx);
        if !within_composer_limit(&draft) || self.rejections.is_pending() {
            return;
        }
        let (entry, args) = match slash::parse(&draft) {
            Parsed::Command(entry, args) => (entry, args),
            Parsed::Unavailable(name) => {
                self.slash_notice(
                    format!("/{name} isn't available in the desktop app yet"),
                    cx,
                );
                return;
            }
            Parsed::Unknown(name) => {
                let suggestion = slash::closest(name)
                    .map_or(String::new(), |near| format!(" — did you mean /{near}?"));
                self.slash_notice(format!("Unknown command /{name}{suggestion}"), cx);
                return;
            }
            Parsed::Message(_) => return,
        };
        if (entry.needs_args() && args.is_empty()) || (entry.args.is_empty() && !args.is_empty()) {
            self.slash_notice(format!("usage: {}", entry.usage()), cx);
            return;
        }
        let args = if entry.name == "retry" && !args.is_empty() {
            let Some(model) = args
                .strip_prefix("--model ")
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
            else {
                self.slash_notice(format!("usage: {}", entry.usage()), cx);
                return;
            };
            model
        } else {
            args
        };
        let args = args.to_string();
        if matches!(entry.command, Command::RunAction(_)) {
            if self.run_actions.ledger.status().blocked() {
                self.slash_notice(self.run_actions.ledger.status().notice(), cx);
                return;
            }
            if self.run_action_owner().is_none() {
                self.slash_notice("Open a connected Session in Chat first", cx);
                return;
            }
        }
        let state = self.command_state(window, cx);
        if let Some(reason) = commands::entry(entry.command, &state).disabled_reason {
            self.slash_notice(reason, cx);
            return;
        }
        // Clearing first retires old send preparations, before the action captures its owner.
        self.rebuild_composer("", None, window, cx);
        self.composer_notice = None;
        if let Some(reason) = self.run_command(entry.command, window, cx) {
            self.rebuild_composer(&draft, None, window, cx);
            self.slash_notice(reason, cx);
            return;
        }
        match entry.arguments {
            Arguments::Run => {
                if self.run_actions.panel.is_none() {
                    self.rebuild_composer(&draft, None, window, cx);
                    self.slash_notice("Close the open review before starting another action", cx);
                    return;
                }
                if !args.is_empty() {
                    self.prefill_run_action(args);
                }
            }
            Arguments::Search if !args.is_empty() => {
                self.profile_query
                    .update(cx, |query, cx| query.set_value(args, window, cx));
                self.begin_profile_search(window, cx);
            }
            Arguments::Name | Arguments::Instructions | Arguments::Tags | Arguments::Pin => {
                self.prefill_session_slash(entry.arguments, args);
            }
            _ => {}
        }
        if self
            .composer_notice
            .as_deref()
            .is_some_and(|notice| notice != ACTION_QUEUED_NOTICE)
        {
            let notice = self.composer_notice.clone();
            self.rebuild_composer(&draft, None, window, cx);
            self.composer_notice = notice;
            return;
        }
        self.composer_notice = None;
        self.slash = Popup::default();
        cx.notify();
    }

    fn accept_slash(
        &mut self,
        entry: &'static Entry,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = self.draft(cx);
        let selection = self.composer_selection(cx);
        let text = format!(
            "/{}{}",
            entry.name,
            if (!run && !entry.args.is_empty()) || entry.needs_args() {
                " "
            } else {
                ""
            }
        );
        let end = text.len();
        self.composer.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
            input.set_selected_range(end..end, cx);
        });
        self.slash.selected = 0;
        if run && !entry.needs_args() {
            self.submit_slash(window, cx);
            if !self.draft(cx).is_empty() {
                self.rebuild_composer(&draft, Some(selection), window, cx);
            }
        } else if run {
            self.slash_notice(format!("usage: {}", entry.usage()), cx);
        }
        cx.notify();
    }

    pub(super) fn slash_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            cx.propagate();
            return;
        }
        let items = self.slash_items(window, cx);
        if items.is_empty() {
            cx.propagate();
            return;
        }
        let selected = self.slash_selected(items.len(), cx);
        match key {
            "up" | "down" => {
                self.slash.query = self.draft(cx).to_string();
                self.slash.selected = if key == "up" {
                    selected.saturating_sub(1)
                } else {
                    (selected + 1).min(items.len() - 1)
                };
                self.slash.scroll.scroll_to_item(self.slash.selected);
            }
            "tab" => self.accept_slash(items[selected], false, window, cx),
            "enter" => self.accept_slash(items[selected], true, window, cx),
            "escape" => self.slash.hidden = Some(self.draft(cx).to_string()),
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_slash(
        &self,
        inert: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if inert {
            return None;
        }
        let items = self.slash_items(window, cx);
        if items.is_empty() {
            return None;
        }
        let selected = self.slash_selected(items.len(), cx);
        Some(
            div()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(self.scaled(crate::theme::SURFACE_RADIUS))
                .child(
                    v_flex()
                        .id("slash-popup")
                        .test_support()
                        .role(Role::ListBox)
                        .aria_label("Slash commands")
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .max_h(self.scaled(180.))
                        .overflow_y_scroll()
                        .track_scroll(&self.slash.scroll)
                        .rounded(self.scaled(crate::theme::SURFACE_RADIUS))
                        .bg(cx.theme().background)
                        .children(items.into_iter().enumerate().map(|(index, entry)| {
                            h_flex()
                                .id(SharedString::from(format!("slash-row-{index}")))
                                .test_support()
                                .role(Role::ListBoxOption)
                                .aria_label(entry.usage())
                                .aria_description(entry.description)
                                .aria_selected(index == selected)
                                .w_full()
                                .min_w_0()
                                .flex_shrink_0()
                                .px_3()
                                .py_1()
                                .gap_3()
                                .cursor_pointer()
                                .when(index == selected, |row| {
                                    row.bg(cx.theme().accent)
                                        .text_color(cx.theme().accent_foreground)
                                })
                                .child(div().min_w_0().truncate().text_sm().child(entry.usage()))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(entry.description),
                                )
                                .on_mouse_down(
                                    gpui_kit::MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        window.prevent_default();
                                        cx.stop_propagation();
                                        this.accept_slash(entry, true, window, cx);
                                    }),
                                )
                        })),
                )
                .into_any_element(),
        )
    }
}
