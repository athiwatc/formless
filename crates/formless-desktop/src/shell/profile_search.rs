use super::*;

impl Shell {
    #[cfg(feature = "test-support")]
    pub fn profile_query(&self) -> Entity<InputState> {
        self.profile_query.clone()
    }

    #[cfg(feature = "test-support")]
    pub fn profile_revision(&self) -> u64 {
        self.profile_search.revision
    }

    pub(super) fn finish_profile_composition(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let composing = self.profile_query.update(cx, |query, cx| {
            query.marked_text_range(window, cx).is_some()
        });
        if composing {
            self.profile_query.update(cx, |query, cx| {
                query.unmark_text(window, cx);
                cx.notify();
            });
        }
        composing
    }

    fn profile_current(&self, cx: &App, revision: u64) -> bool {
        self.client_connected
            && self.client.as_ref().is_some_and(|client| {
                client.is_ready()
                    && self.profile_search.current(
                        client.connection(),
                        &self.profile_query.read(cx).value(),
                        revision,
                    )
            })
    }

    pub(super) fn begin_profile_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finish_profile_composition(window, cx) {
            return;
        }
        let text = self.profile_query.read(cx).value();
        // Match the button's pending guard even before input observers run.
        // A different query retires its old read and can explicitly replace it.
        self.profile_search.changed(&text);
        if self.profile_search.pending() {
            return;
        }
        let Some(client) = &self.client else {
            return;
        };
        if !self.client_connected || !client.is_ready() {
            self.profile_search
                .retire("Connect before searching. No search was queued.");
            cx.notify();
            return;
        }
        let operation = uuid::Uuid::new_v4();
        if let Ok(query) = self
            .profile_search
            .start(client.connection(), operation, &text)
        {
            if let Err(error) = client.send(ClientCommand::SearchProfile {
                connection: client.connection(),
                operation,
                query,
            }) {
                self.profile_search.retire(error);
            }
        }
        cx.notify();
    }

    fn profile_navigation_focused(&self, window: &Window, cx: &App) -> bool {
        self.profile_query.focus_handle(cx).is_focused(window)
            || self.conversation_focus.is_focused(window)
    }

    fn move_profile_search(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.profile_navigation_focused(window, cx) {
            cx.propagate();
            return;
        }
        if self.profile_query.update(cx, |query, cx| {
            query.marked_text_range(window, cx).is_some()
        }) {
            cx.propagate();
            return;
        }
        if !self.profile_current(cx, self.profile_search.revision) {
            return;
        }
        self.profile_search.move_selection(delta);
        if let Some(index) = self.profile_search.selected {
            self.profile_scroll
                .scroll_to_item_strict(index, ScrollStrategy::Top);
        }
        self.profile_show_results = true;
        self.conversation_focus.focus(window, cx);
        self.profile_frames = 2;
        cx.notify();
    }

    fn profile_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.profile_query.focus_handle(cx).is_focused(window) {
            self.begin_profile_search(window, cx);
        } else if self.conversation_focus.is_focused(window) {
            if let Some(index) = self.profile_search.selected {
                self.open_profile_hit(index, self.profile_search.revision, window, cx);
            }
        } else {
            cx.propagate();
        }
    }

    fn checked_profile_hit(
        &mut self,
        index: usize,
        revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<formless_protocol::SearchHit> {
        if self.destination != Destination::ProfileSearch
            || self.background_inert(window)
            || self.finish_profile_composition(window, cx)
        {
            return None;
        }
        if !self.profile_current(cx, revision) {
            self.profile_search.notice =
                "Results are no longer current. Search again before opening or copying.".into();
            cx.notify();
            return None;
        }
        self.profile_search.hits.get(index).cloned()
    }

    /// Row callbacks keep their reply revision rather than using a replacement index.
    pub fn open_profile_hit(
        &mut self,
        index: usize,
        revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hit) = self.checked_profile_hit(index, revision, window, cx) else {
            return;
        };
        let mut sessions = self
            .client_sessions
            .iter()
            .filter(|session| session.id == hit.session_id);
        let available = sessions.next().is_some_and(|session| !session.archived)
            && sessions.next().is_none()
            && self
                .sidebar
                .rows()
                .iter()
                .filter(|row| row.id == hit.session_id)
                .count()
                == 1;
        if !available {
            self.profile_search.notice="This Session is archived, missing or ambiguous in the current list. Refresh Sessions and restore it there before opening. No other Session was opened.".into();
            cx.notify();
            return;
        }
        self.sidebar.select_id(&hit.session_id);
        self.activate_selected(window, cx);
    }

    fn profile_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.finish_profile_composition(window, cx) {
            self.set_destination(self.profile_return, window, cx);
        }
    }

    pub(super) fn reveal_profile_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.destination != Destination::ProfileSearch || self.background_inert(window) {
            return;
        }
        let focus = (window.focused(cx), window.viewport_size(), self.text_scale);
        if self.profile_focus.as_ref() != Some(&focus) {
            self.profile_focus = Some(focus);
            self.profile_show_results = self.conversation_focus.is_focused(window)
                && self.profile_current(cx, self.profile_search.revision);
            self.profile_frames = 2;
        }
        if self.profile_frames == 0 {
            return;
        }
        self.profile_frames -= 1;
        // These are direct children of profile-search-body. Unlike a
        // ScrollAnchor, the handle keeps nested ancestor offsets intact.
        let item = if self.profile_show_results {
            Some(5)
        } else if self
            .profile_query
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            Some(2)
        } else if self
            .profile_controls
            .iter()
            .any(|focus| focus.contains_focused(window, cx))
        {
            Some(3)
        } else {
            None
        };
        if let Some(item) = item {
            if item == 2 || item == 5 {
                self.profile_body.scroll_to_top_of_item(item);
            } else {
                self.profile_body.scroll_to_item(item);
            }
            window.request_animation_frame();
        }
    }

    pub(super) fn render_profile_search(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let revision = self.profile_search.revision;
        let current = self.profile_current(cx, revision);
        let count = if current {
            self.profile_search.hits.len()
        } else {
            0
        };
        let chosen = self.profile_search.selected.filter(|index| *index < count);
        let list = gpui_kit::uniform_list(
            "profile-search-list",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| {
                let revision = this.profile_search.revision;
                if !this.profile_current(cx, revision) {
                    return Vec::new();
                }
                range
                    .filter_map(|index| {
                        let hit = this.profile_search.hits.get(index)?;
                        let selected = this.profile_search.selected == Some(index);
                        Some(
                            v_flex()
                                .id(("profile-search-row", index))
                                .test_support()
                                .role(Role::ListBoxOption)
                                .aria_label(format!(
                                    "{} · {} · message {}: {}",
                                    hit.session_name, hit.role, hit.seq, hit.snippet
                                ))
                                .aria_description(format!(
                                    "Session {} · Message {}",
                                    hit.session_id, hit.message_id
                                ))
                                .aria_selected(selected)
                                .aria_position_in_set(index + 1)
                                .aria_size_of_set(this.profile_search.hits.len())
                                .h(this.scaled(72.))
                                .w_full()
                                .px_2()
                                .justify_center()
                                .cursor_pointer()
                                .when(selected, |row| {
                                    row.bg(cx.theme().accent).aria_active_descendant()
                                })
                                .child(
                                    div()
                                        .text_sm()
                                        .truncate()
                                        .child(directory::visual_text(&hit.session_name)),
                                )
                                .child(div().text_xs().truncate().child(format!(
                                    "{} · message {} · {}",
                                    hit.role, hit.seq, hit.created_at
                                )))
                                .child(
                                    div()
                                        .text_xs()
                                        .truncate()
                                        .child(directory::visual_text(&hit.snippet)),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if this
                                        .checked_profile_hit(index, revision, window, cx)
                                        .is_some()
                                    {
                                        this.profile_search.selected = Some(index);
                                        this.conversation_focus.focus(window, cx);
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .collect()
            }),
        )
        .h(self.scaled(220.))
        .track_scroll(&self.profile_scroll);
        let preview=chosen.and_then(|index|self.profile_search.hits.get(index)).map(|hit|format!("Session {} · Message {}\n{}\nSnippet: {}",hit.session_id,hit.message_id,hit.created_at,crate::questions::preview(&hit.snippet,500))).unwrap_or_else(||"Choose a current result. Snippets can be incomplete; Copy result copies the returned search fields, not the full message.".into());
        v_flex().id("region-conversation").test_support().role(Role::Main).aria_label("Profile Search")
            .aria_description("Search persisted messages across this Profile, including archived Sessions. Enter in the query searches; arrows choose a result. Enter in the results opens its Session without jumping to the message. Escape returns.")
            .map(|region|inert_while(region,self.background_inert(window)))
            .track_focus(&self.conversation_focus).key_context(PROFILE_SEARCH_CONTEXT)
            .on_action(cx.listener(|this,_:&ProfileSearchNext,window,cx|this.move_profile_search(1,window,cx)))
            .on_action(cx.listener(|this,_:&ProfileSearchPrevious,window,cx|this.move_profile_search(-1,window,cx)))
            .on_action(cx.listener(|this,_:&ProfileSearchOpen,window,cx|this.profile_enter(window,cx)))
            .on_action(cx.listener(|this,_:&ProfileSearchBack,window,cx|this.profile_back(window,cx)))
            .map(|region|Self::main_size(region,page_height)).when_some(page_height,|region,height|region.h(height))
            .child(v_flex().id("profile-search-body").test_support().size_full().overflow_y_scroll().track_scroll(&self.profile_body).p_3().gap_3()
                .child(readable("profile-search-title","Profile Search · persisted messages"))
                .child(readable("profile-search-scope","Across this connected Profile, including archived Sessions. Up to 50 matches, ranked by the daemon. Results are snippets; Open Session does not jump to a message."))
                .child(div().id("profile-query-anchor").test_support().pt_1().child(Input::new(&self.profile_query).aria_label("Search Profile messages")))
                .child(h_flex().flex_wrap().gap_2()
                    .child(div().id("profile-submit-anchor").child(Button::new("profile-search-submit").label("Search").track_focus(&self.profile_controls[0]).disabled(!self.client_connected||self.profile_search.pending()).on_click(cx.listener(|this,_,window,cx|this.begin_profile_search(window,cx)))))
                    .child(div().id("profile-open-anchor").child(Button::new("profile-search-open").label("Open Session").track_focus(&self.profile_controls[1]).disabled(chosen.is_none()).on_click(cx.listener(move|this,_,window,cx|{if let Some(index)=chosen {this.open_profile_hit(index,revision,window,cx);}}))))
                    .child(div().id("profile-copy-anchor").child(Button::new("profile-search-copy").label("Copy result").track_focus(&self.profile_controls[2]).disabled(chosen.is_none()).on_click(cx.listener(move|this,_,window,cx|{
                        if let Some(hit)=chosen.and_then(|index|this.checked_profile_hit(index,revision,window,cx)) {cx.write_to_clipboard(ClipboardItem::new_string(serde_json::to_string_pretty(&hit).expect("search hit serializes")));}
                    }))))
                    .child(div().id("profile-back-anchor").child(Button::new("profile-search-back").label("Back").track_focus(&self.profile_controls[3]).on_click(cx.listener(|this,_,window,cx|this.profile_back(window,cx))))))
                .child(announced(div().id("profile-search-notice")).test_support().role(Role::Label).aria_value(self.profile_search.notice.clone()).child(self.profile_search.notice.clone()))
                .child(div().id("profile-search-results").test_support().role(Role::ListBox).aria_label("Matches in persisted Profile messages").child(list))
                .child(readable("profile-search-detail",preview)))
    }
}
