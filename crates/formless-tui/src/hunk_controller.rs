//! Keyboard/controller boundary for server-owned hunk reviews.
use crate::*;
use hunk_ui::ViewportMove;
use unicode_segmentation::UnicodeSegmentation;

impl App {
    pub(super) fn hunk_review_is_visible(&self) -> bool {
        self.hunk_review.context.is_some()
            && self.pager.is_none()
            && self.wizard.is_none()
            && self.switcher.is_none()
            && self.rollback_pick.is_none()
            && self.model_overlay.is_none()
            && self.forecast.modal.is_none()
            && !self.quit_confirm
            && (self.editor.is_some() || (self.approvals.is_empty() && self.promotions.is_empty()))
    }

    pub(super) fn hunk_reply_is_current(&self, reply: &hunk_ui::Reply) -> bool {
        if self.hunk_review.generation != reply.generation
            || self.hunk_review.context.as_ref() != Some(&reply.context)
            || self.current.as_deref() != Some(reply.context.session_id())
            || self.offline.is_some()
            || self.client.is_none()
        {
            return false;
        }
        if let hunk_ui::Context::Tool {
            tool_id,
            base_version,
            current_hash,
            editor_generation,
            ..
        } = &reply.context
        {
            return *editor_generation == self.editor_generation
                && self.editor.as_ref().is_some_and(|editor| {
                    editor.mode == EditMode::Code
                        && &editor.tool_id == tool_id
                        && editor.review_base_version == Some(*base_version)
                        && formless_core::hunk_review::image_hash(Some(editor.text().as_bytes()))
                            == *current_hash
                });
        }
        true
    }

    fn hunk_request(&mut self, action: hunk_ui::Action, request: Request) {
        let Some(context) = self.hunk_review.context.clone() else {
            return;
        };
        let reply = hunk_ui::Reply {
            generation: self.hunk_review.generation,
            context,
            review_id: self
                .hunk_review
                .document
                .as_ref()
                .map(|review| review.review_id.clone()),
        };
        if !self.hunk_reply_is_current(&reply)
            || self
                .hunk_review
                .document
                .as_ref()
                .is_some_and(|document| !reply.context.matches(document, false))
        {
            self.hunk_review.clear();
            self.notes
                .push("Review owner or draft changed; open a fresh review.".into());
            return;
        }
        self.hunk_review.error = None;
        self.hunk_review.busy = true;
        self.request(Pending::HunkReview { reply, action }, request);
    }

    pub(super) fn begin_run_review(&mut self, run_id: Option<String>) {
        let Some(session_id) = self.current.clone() else {
            self.notes.push("Open a Session before /review.".into());
            return;
        };
        if self.offline.is_some() || self.client.is_none() {
            self.notes
                .push("Reconnect before opening a file review.".into());
            return;
        }
        self.cancel_branch_summary();
        self.hunk_review.clear();
        self.pager = None;
        self.reviewing = None;
        self.hunk_review.context = Some(hunk_ui::Context::Run {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
        });
        self.hunk_request(
            hunk_ui::Action::Inspect,
            Request::InspectRunReview { session_id, run_id },
        );
    }

    pub(super) fn begin_tool_hunks(&mut self) {
        let Some(session_id) = self.current.clone() else {
            self.notes
                .push("Open a Session before reviewing a Tool draft.".into());
            return;
        };
        let Some(editor) = self.editor.as_ref().filter(|e| e.mode == EditMode::Code) else {
            return;
        };
        let Some(base_version) = editor.review_base_version else {
            self.notes.push(
                "Reopen the Tool editor to pin its original version before reviewing.".into(),
            );
            return;
        };
        if self.offline.is_some() || self.client.is_none() {
            self.notes
                .push("Reconnect before opening a Tool draft review.".into());
            return;
        }
        let candidate_code = editor.text();
        let tool_id = editor.tool_id.clone();
        let hash = formless_core::hunk_review::image_hash(Some(candidate_code.as_bytes()));
        self.cancel_branch_summary();
        self.hunk_review.clear();
        self.pager = None;
        self.reviewing = None;
        self.editor_review = None;
        self.hunk_review.context = Some(hunk_ui::Context::Tool {
            session_id: session_id.clone(),
            tool_id: tool_id.clone(),
            base_version,
            original_hash: hash.clone(),
            current_hash: hash,
            editor_generation: self.editor_generation,
        });
        self.hunk_request(
            hunk_ui::Action::Inspect,
            Request::PrepareToolReview {
                session_id,
                tool_id,
                base_version,
                candidate_code,
            },
        );
    }

    fn close_hunk_review(&mut self) {
        let busy = self.hunk_review.busy;
        let run = matches!(self.hunk_review.context, Some(hunk_ui::Context::Run { .. }));
        self.hunk_review.clear();
        self.notes.push(if busy && run {
            "Review closed. A dispatched file action may still finish; reopen /review to inspect and undo.".into()
        } else if busy {
            "Review closed; late replies will not replace the Tool draft.".into()
        } else { "Review closed; your draft is kept.".into() });
    }

    pub(super) fn hunk_review_result(
        &mut self,
        reply: hunk_ui::Reply,
        action: hunk_ui::Action,
        value: serde_json::Value,
    ) {
        if !self.hunk_reply_is_current(&reply) {
            return;
        }
        self.hunk_review.busy = false;
        if matches!(action, hunk_ui::Action::Comment) {
            match serde_json::from_value::<ReviewCommentResult>(value) {
                Ok(result)
                    if result.message.session_id == reply.context.session_id()
                        && result.message.role == "user" =>
                {
                    self.hunk_review.comment = None;
                    self.on_event(Event::MessageAppended {
                        session_id: result.message.session_id.clone(),
                        message: result.message,
                    });
                    self.notes.push(if result.steered {
                        "Review comment sent to the active Run.".into()
                    } else {
                        "Review comment saved for the next explicit Run; no Run was started.".into()
                    });
                }
                _ => self.notes.push(
                    "Review comment reply did not match this Session; inspect before retrying."
                        .into(),
                ),
            }
            return;
        }
        let document = if matches!(action, hunk_ui::Action::Inspect) {
            serde_json::from_value::<HunkReview>(value).ok()
        } else {
            serde_json::from_value::<ReviewDecisionResult>(value)
                .ok()
                .map(|result| result.review)
        };
        let Some(document) = document.filter(|document| {
            reply
                .context
                .matches(document, matches!(action, hunk_ui::Action::Decision))
                && reply
                    .review_id
                    .as_ref()
                    .is_none_or(|id| id == &document.review_id)
        }) else {
            self.hunk_review.clear();
            self.notes.push(
                "Review reply did not match its owner, version or content hash; reopen to inspect."
                    .into(),
            );
            return;
        };
        if let hunk_ui::Context::Tool {
            current_hash,
            editor_generation,
            ..
        } = self.hunk_review.context.as_mut().unwrap()
        {
            // Ownership and prior bytes were checked above. Only the server's
            // exact returned draft is applied; Ctrl+S remains the save gate.
            let editor = self.editor.as_mut().unwrap();
            let code = document.draft_code.as_ref().unwrap();
            if editor.text() != *code {
                let mode = editor.vim.mode;
                editor.vim.reset();
                editor.vim.mode = mode;
                if editor.original.is_none() {
                    editor.original = Some(editor.text());
                }
                let mut line = editor.take_modal_line();
                let at = line.cursor_byte();
                line.set(code);
                line.set_modal_cursor(at);
                editor.apply_modal_line(line);
                self.editor_generation = self.editor_generation.wrapping_add(1);
            }
            *current_hash = document.candidate_hash.clone().unwrap();
            *editor_generation = self.editor_generation;
        }
        self.hunk_review.replace(document);
    }

    pub(super) fn hunk_review_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.close_hunk_review();
            return;
        }
        if let Some(comment) = &mut self.hunk_review.comment {
            if self.hunk_review.busy {
                return;
            }
            match key.code {
                KeyCode::Esc => self.hunk_review.comment = None,
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    comment.insert_char('\n')
                }
                KeyCode::Enter => {
                    let text = comment.expanded();
                    if text.trim().is_empty() {
                        return;
                    }
                    if text.len() > 4096 {
                        let error = "Keep a review comment within 4096 UTF-8 bytes.";
                        self.hunk_review.error = Some(error.into());
                        self.notes.push(error.into());
                        return;
                    }
                    if let Some((review_id, file_id, hunk_id, expected_hash, _)) =
                        self.selected_review_ids()
                    {
                        self.hunk_request(
                            hunk_ui::Action::Comment,
                            Request::CommentReviewHunk {
                                review_id,
                                file_id,
                                hunk_id,
                                expected_hash,
                                comment: text,
                            },
                        );
                    }
                }
                KeyCode::Backspace => comment.backspace(),
                KeyCode::Delete => comment.delete(),
                KeyCode::Left => comment.left(),
                KeyCode::Right => comment.right(),
                KeyCode::Home => comment.home(),
                KeyCode::End => comment.end(),
                KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                    comment.insert_char(c)
                }
                _ => {}
            }
            return;
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            self.close_hunk_review();
            return;
        }
        if self.hunk_review.busy {
            return;
        }
        if ctrl
            && key.code == KeyCode::Char('s')
            && matches!(
                self.hunk_review.context,
                Some(hunk_ui::Context::Tool { .. })
            )
        {
            self.hunk_review.clear();
            self.editor_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('f') => {
                if let Some(document) = &self.hunk_review.document {
                    let mut lines =
                        hunk_view::render(&document.files, self.hunk_review.focus, self.color)
                            .overview;
                    lines.extend(
                        document
                            .warnings
                            .iter()
                            .map(|warning| Line::raw(hunk_view::display_text(warning))),
                    );
                    self.pager = Some(Pager {
                        title: "Review files · Esc returns to hunks".into(),
                        lines: lines.iter().map(ToString::to_string).collect(),
                        styled: lines,
                        top: 0,
                        left: 0,
                    });
                }
            }
            KeyCode::Char('n' | 'p') => {
                if let Some(review) = &self.hunk_review.document {
                    self.hunk_review.focus = hunk_view::navigate(
                        &review.files,
                        self.hunk_review.focus,
                        key.code == KeyCode::Char('n'),
                    );
                    self.hunk_review.reset_viewport();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.hunk_review.move_viewport(ViewportMove::Rows(1))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.hunk_review.move_viewport(ViewportMove::Rows(-1))
            }
            KeyCode::PageDown => self.hunk_review.move_viewport(ViewportMove::Rows(15)),
            KeyCode::PageUp => self.hunk_review.move_viewport(ViewportMove::Rows(-15)),
            KeyCode::Home => self.hunk_review.move_viewport(ViewportMove::Top),
            KeyCode::End => self.hunk_review.move_viewport(ViewportMove::Bottom),
            KeyCode::Right | KeyCode::Char('l') => {
                self.hunk_review.move_viewport(ViewportMove::Columns(8))
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.hunk_review.move_viewport(ViewportMove::Columns(-8))
            }
            KeyCode::Char('c') if self.selected_review_ids().is_some() => {
                self.hunk_review.comment = Some(input::InputLine::default())
            }
            KeyCode::Char('a' | 'r') => {
                if let Some((review_id, file_id, hunk_id, expected_hash, decision)) =
                    self.selected_review_ids()
                {
                    if key.code == KeyCode::Char('a') && decision == "rejected" {
                        self.notes
                            .push("Undo the rejection before accepting this hunk.".into());
                        return;
                    }
                    self.hunk_request(
                        hunk_ui::Action::Decision,
                        Request::DecideReviewHunk {
                            review_id,
                            file_id,
                            hunk_id,
                            expected_hash,
                            decision: if key.code == KeyCode::Char('a') {
                                "accept"
                            } else {
                                "reject"
                            }
                            .into(),
                        },
                    );
                }
            }
            KeyCode::Char('u') => {
                if let Some(review) = &self.hunk_review.document {
                    if let Some(change) = review
                        .undo_changes
                        .iter()
                        .rev()
                        .find(|change| !change.undone)
                    {
                        if let Some(file) = review.files.iter().find(|file| {
                            file.id == change.file_id && file.exclusion_reason.is_none()
                        }) {
                            let request = Request::UndoReviewChange {
                                review_id: review.review_id.clone(),
                                change_id: change.change_id.clone(),
                                expected_hash: file.after_hash.clone(),
                            };
                            self.hunk_request(hunk_ui::Action::Decision, request);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn selected_review_ids(&self) -> Option<(String, String, String, String, String)> {
        let review = self.hunk_review.document.as_ref()?;
        if !self.hunk_review.context.as_ref()?.matches(review, false) {
            return None;
        }
        let position = *hunk_view::positions(&review.files).get(self.hunk_review.focus)?;
        let file = &review.files[position.file];
        let hunk = &file.hunks[position.hunk];
        Some((
            review.review_id.clone(),
            file.id.clone(),
            hunk.id.clone(),
            file.after_hash.clone(),
            hunk.decision.clone(),
        ))
    }
}

fn line_width(line: &Line<'_>, width: usize) -> usize {
    line.styled_graphemes(Style::default())
        .map(|grapheme| text::width(grapheme.symbol))
        .filter(|&columns| columns <= width)
        .sum()
}

fn clip_line_left(mut line: Line<'static>, mut columns: usize, width: usize) -> Line<'static> {
    for span in &mut line.spans {
        if columns == 0 {
            break;
        }
        let mut end = 0;
        for (offset, grapheme) in span.content.grapheme_indices(true) {
            let cells = text::width(grapheme);
            // Paragraph skips graphemes that cannot fit the entire pane.
            if cells <= width {
                columns = columns.saturating_sub(cells);
            }
            end = offset + grapheme.len();
            if columns == 0 {
                // Drop a partially hidden starting grapheme as a unit so
                // it cannot displace the last column at the right edge.
                break;
            }
        }
        match &mut span.content {
            std::borrow::Cow::Borrowed(text) => *text = &text[end..],
            std::borrow::Cow::Owned(text) => drop(text.drain(..end)),
        }
    }
    line
}

#[cfg(test)]
#[test]
fn hunk_horizontal_crop_reaches_the_last_cell_at_wide_and_large_offsets() {
    let style = Style::default().add_modifier(Modifier::UNDERLINED);
    let line = Line::from(vec![Span::raw("界界"), Span::styled("x", style)]);
    let clipped = clip_line_left(line, 1, 4);
    assert_eq!(clipped.to_string(), "界x");
    assert_eq!(clipped.spans.last().unwrap().style, style);

    let line = Line::raw("界x界z");
    assert_eq!(line_width(&line, 1), 2);
    let clipped = clip_line_left(line, 1, 1);
    assert_eq!(line_width(&clipped, 1), 1);
    assert!(clipped.to_string().ends_with('z'));

    let line = Line::raw(format!("{}z", "x".repeat(70_000)));
    assert_eq!(clip_line_left(line, 70_000, 1).to_string(), "z");
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Clear, area);
    let Some(document) = &app.hunk_review.document else {
        let text = app
            .hunk_review
            .error
            .as_ref()
            .map(|error| {
                format!(
                    "Review could not be read: {}\nEsc closes this view; your draft is kept.",
                    hunk_view::display_text(error)
                )
            })
            .unwrap_or_else(|| {
                "Reading a server-owned review…\nEsc closes this view; your draft is kept.".into()
            });
        f.render_widget(
            Paragraph::new(text).block(ui::panel(" Hunk review ", app.color, Color::Cyan)),
            area,
        );
        return;
    };
    let view = hunk_view::render(&document.files, app.hunk_review.focus, app.color);
    let is_tool = document.tool_id.is_some();
    let overview_height = (view.overview.len() + document.warnings.len() + 2).min(8) as u16;
    let rows = Layout::vertical([
        Constraint::Length(overview_height),
        Constraint::Min(3),
        Constraint::Length(if app.hunk_review.comment.is_some() {
            6
        } else {
            5
        }),
    ])
    .split(area);
    let mut overview = view.overview;
    overview.extend(
        document
            .warnings
            .iter()
            .map(|warning| Line::raw(hunk_view::display_text(warning))),
    );
    f.render_widget(
        Paragraph::new(overview).block(ui::panel(
            if is_tool {
                " Tool draft · not saved "
            } else {
                " Run file review "
            },
            app.color,
            Color::Cyan,
        )),
        rows[0],
    );
    let block = ui::panel(
        " Changed words are underlined · old / new line ",
        app.color,
        Color::Cyan,
    );
    let inner = block.inner(rows[1]);
    let width = usize::from(inner.width.max(1));
    let max_width = view
        .hunk
        .iter()
        .map(|line| line_width(line, width))
        .max()
        .unwrap_or_default();
    let state = &mut app.hunk_review;
    let max_scroll = view
        .hunk
        .len()
        .saturating_sub(usize::from(inner.height.max(1)));
    state.layout_viewport(max_scroll, max_width.saturating_sub(width));
    let body = view
        .hunk
        .into_iter()
        .skip(state.scroll)
        .take(usize::from(inner.height))
        .map(|line| clip_line_left(line, state.left, width))
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(body).block(block), rows[1]);
    let status = if let Some(error) = &app.hunk_review.error {
        format!("Server: {}", hunk_view::display_text(error))
    } else if app.hunk_review.busy {
        "Waiting for the server; closing does not cancel a dispatched action.".into()
    } else if view.total_hunks == 0 {
        "No editable text hunks. Excluded files remain listed above.".into()
    } else {
        "Comments use server-derived path:line context; idle comments do not start a Run.".into()
    };
    let footer = if let Some(comment) = &app.hunk_review.comment {
        format!(
            "Comment on selected hunk · Enter send · Shift+Enter newline · Esc back\n{}{}",
            hunk_view::display_text(&comment.expanded()),
            app.hunk_review
                .error
                .as_ref()
                .map(|error| format!("\nServer: {}", hunk_view::display_text(error)))
                .unwrap_or_else(|| if app.hunk_review.busy {
                    "\nSaving comment…".into()
                } else {
                    String::new()
                })
        )
    } else {
        format!("n/p hunk · a keep · r reject · u undo · c comment · f all files\n↑↓ PgUp/PgDn Home/End · ←→ horizontal · Esc close{}\n{}",
            if is_tool { " · Ctrl+S normal save" } else { "" },
            status)
    };
    f.render_widget(
        Paragraph::new(footer).block(ui::panel(" Review keys ", app.color, Color::Yellow)),
        rows[2],
    );
}
