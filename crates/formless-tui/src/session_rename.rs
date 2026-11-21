use crate::*;

const PROMPT: &str = "rename › ";

/// A Session name or an outstanding suggestion for that name. Neither is
/// stored in the message composer or in the Session draft cache.
pub(super) struct Rename {
    pub(super) target: String,
    input: Option<input::InputLine>,
    hits: std::cell::RefCell<Vec<(Rect, usize)>>,
    selecting: bool,
}

impl Rename {
    fn new(target: &str, name: Option<&str>) -> Self {
        Self {
            target: target.into(),
            input: name.map(|name| {
                let mut input = input::InputLine::default();
                input.set(name);
                input
            }),
            hits: Default::default(),
            selecting: false,
        }
    }

    pub(super) fn suggesting(&self) -> bool {
        self.input.is_none()
    }

    pub(super) fn clear_hits(&self) {
        self.hits.borrow_mut().clear();
    }

    pub(super) fn height(&self, width: u16) -> u16 {
        let rows = self.input.as_ref().map_or(1, |input| {
            input.layout(input_text_width(width, PROMPT)).0.len()
        });
        rows.clamp(1, 8) as u16 + 2
    }

    pub(super) fn draw(&self, f: &mut Frame, area: Rect, app: &App, show_cursor: bool) {
        let mut block = ui::panel(" Rename Session ", app.color, Color::Magenta);
        if area.width > 55 {
            block = block.title(ui::hint("Enter rename · Esc cancel", app.color));
        }
        let Some(input) = &self.input else {
            f.render_widget(
                Paragraph::new("Suggesting a name… · Esc cancel").block(block),
                area,
            );
            return;
        };
        let show_cursor = show_cursor && app.rename_pointer_available();
        let (lines, cursor) = draft_editor_rows(
            input,
            area,
            PROMPT,
            bold_fg(app.color, Color::Magenta),
            show_cursor.then_some(&self.hits),
        );
        f.render_widget(Paragraph::new(lines).block(block), area);
        if show_cursor {
            set_draft_cursor(f, area, PROMPT, cursor);
        }
    }
}

impl App {
    pub(super) fn start_rename(&mut self, id: &str, name: &str) {
        self.cancel_rename();
        self.renaming = Some(Rename::new(id, Some(name)));
        self.open_view(View::Chat);
    }

    pub(super) fn suggest_rename(&mut self, id: String) {
        if self.offline.is_some() || self.client.is_none() {
            self.notes
                .push("cannot suggest a name while disconnected".into());
            return;
        }
        self.cancel_rename();
        self.renaming = Some(Rename::new(&id, None));
        self.open_view(View::Chat);
        self.request(
            Pending::SuggestName { target: id.clone() },
            Request::SuggestSessionName { id },
        );
    }

    pub(super) fn cancel_rename(&mut self) {
        if self
            .renaming
            .take()
            .is_some_and(|rename| rename.suggesting())
        {
            self.pending.retain(|id, pending| {
                if matches!(pending, Pending::SuggestName { .. }) {
                    self.pending_since.remove(id);
                    false
                } else {
                    true
                }
            });
        }
    }

    pub(super) fn finish_rename_suggestion(
        &mut self,
        target: &str,
        result: Option<serde_json::Value>,
        error: Option<String>,
    ) {
        if self
            .renaming
            .as_ref()
            .is_none_or(|rename| !rename.suggesting() || rename.target != target)
        {
            return;
        }
        if !self.is_current(target) || self.view != View::Chat {
            self.cancel_rename();
            return;
        }
        if let Some(name) = result
            .as_ref()
            .and_then(|value| value["name"].as_str())
            .filter(|name| !name.trim().is_empty())
        {
            self.start_rename(target, name);
        } else {
            self.cancel_rename();
            self.notes.push(error.map_or_else(
                || "no name to suggest yet".into(),
                |error| format!("daemon: {error}"),
            ));
        }
    }

    pub(super) fn rename_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.cancel_rename();
            return;
        }
        let Some(rename) = &mut self.renaming else {
            return;
        };
        let Some(name) = &mut rename.input else {
            return;
        };
        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
            if self.offline.is_some() {
                self.notes
                    .push("not renamed — offline; the name is kept".into());
                return;
            }
            let name = name.expanded().trim().to_string();
            if name.is_empty() {
                return;
            }
            let id = rename.target.clone();
            self.cancel_rename();
            self.fire(Request::RenameSession { id, name });
            return;
        }
        if history_key(name, key) || selection_navigation(name, key) {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Backspace if ctrl || alt => name.delete_word_back(),
            KeyCode::Backspace => name.backspace(),
            KeyCode::Delete => name.delete(),
            KeyCode::Char('a') if ctrl => name.home(),
            KeyCode::Char('e') if ctrl => name.end(),
            KeyCode::Char('u') if ctrl => name.kill_to_start(),
            KeyCode::Char('k') if ctrl => name.kill_to_end(),
            KeyCode::Char(c)
                if !c.is_control() && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                name.insert_char(c);
            }
            _ => {}
        }
    }

    pub(super) fn rename_paste(&mut self, text: &str) {
        if let Some(name) = self
            .renaming
            .as_mut()
            .and_then(|rename| rename.input.as_mut())
        {
            name.insert_str(&single_line_query_paste(text));
        }
    }

    /// The name can be drawn underneath a modal. Only register its own
    /// cursor and mouse targets when the name is the active field.
    fn rename_pointer_available(&self) -> bool {
        let approval_open = !self.approvals.is_empty() || !self.promotions.is_empty();
        self.view == View::Chat
            && self.find.is_none()
            && self.wizard.is_none()
            && self.editor.is_none()
            && !self.external_edit.awaiting_confirmation()
            && !self.hunk_review_is_visible()
            && self.pager.is_none()
            && self.switcher.is_none()
            && self.forecast.modal.is_none()
            && !self.quit_confirm
            && self.rollback_pick.is_none()
            && (approval_open || self.model_overlay.is_none())
            && (!approval_open || self.input_focus)
    }

    pub(super) fn rename_mouse(&mut self, event: crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        let focused = self.rename_pointer_available();
        let Some(rename) = &mut self.renaming else {
            return false;
        };
        let dragging = event.kind == MouseEventKind::Drag(MouseButton::Left);
        if !dragging && event.kind != MouseEventKind::Down(MouseButton::Left) {
            if event.kind == MouseEventKind::Up(MouseButton::Left) {
                rename.selecting = false;
            }
            return false;
        }
        if !focused || dragging && !rename.selecting {
            return false;
        }
        let at = rename.hits.borrow().iter().find_map(|(rect, at)| {
            ((rect.x..rect.right()).contains(&event.column)
                && (rect.y..rect.bottom()).contains(&event.row))
            .then_some(*at)
        });
        if let Some((input, at)) = rename.input.as_mut().zip(at) {
            input.mouse_cursor(
                at,
                dragging || event.modifiers.contains(KeyModifiers::SHIFT),
            );
            rename.selecting = true;
            return true;
        }
        rename.selecting = false;
        false
    }
}

#[cfg(test)]
mod tests;
