//! End-of-query editing shared by lightweight query overlays.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(super) fn plain(key: &KeyEvent) -> bool {
    key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT
}

/// Some(changed) means query ownership consumed the key. Other modified
/// character chords are consumed without appending their letter.
pub(super) fn edit(query: &mut String, key: &KeyEvent) -> Option<bool> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            let changed = !query.is_empty();
            query.clear();
            Some(changed)
        }
        (KeyCode::Char('w'), KeyModifiers::CONTROL) | (KeyCode::Backspace, KeyModifiers::ALT) => {
            let end = query.trim_end().len();
            let start = query[..end]
                .char_indices()
                .rev()
                .find(|(_, ch)| ch.is_whitespace())
                .map_or(0, |(index, ch)| index + ch.len_utf8());
            let changed = query.len() != start;
            query.truncate(start);
            Some(changed)
        }
        (KeyCode::Backspace, _) if plain(key) => Some(query.pop().is_some()),
        (KeyCode::Char(ch), _) if plain(key) => {
            query.push(ch);
            Some(true)
        }
        (KeyCode::Char(_) | KeyCode::Backspace, _) => Some(false),
        _ => None,
    }
}

/// Clamped plain navigation shared by query result lists.
pub(super) fn navigate(key: &KeyEvent, selected: usize, len: usize, page: usize) -> Option<usize> {
    if !key.modifiers.is_empty() {
        return None;
    }
    let last = len.saturating_sub(1);
    let selected = selected.min(last);
    Some(match key.code {
        KeyCode::Up => selected.saturating_sub(1),
        KeyCode::Down => selected.saturating_add(1).min(last),
        KeyCode::PageUp => selected.saturating_sub(page.max(1)),
        KeyCode::PageDown => selected.saturating_add(page.max(1)).min(last),
        KeyCode::Home => 0,
        KeyCode::End => last,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    fn models() -> picker::ModelPicker {
        picker::ModelPicker::new(
            (0..30)
                .map(|i| picker::ModelItem {
                    id: format!("model-{i:02}"),
                    source: None,
                    recent: false,
                    detail: None,
                })
                .collect(),
        )
    }

    #[test]
    fn model_and_wizard_queries_keep_special_actions_and_share_readline_edits() {
        let mut app = crate::tests::test_app();
        let (client, sent) = DaemonClient::recording();
        app.client = Some(client);
        app.model_overlay = Some(ModelOverlay {
            picker: models(),
            meta: serde_json::json!({}),
            selected_effort: None,
            default_only: false,
        });
        app.model_overlay.as_mut().unwrap().picker.query = "model 日本語".into();
        app.model_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.model_overlay.as_ref().unwrap().picker.query, "model ");
        app.model_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.model_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        assert!(app.model_overlay.as_ref().unwrap().picker.query.is_empty());
        app.model_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            app.model_overlay
                .as_ref()
                .unwrap()
                .selected_effort
                .as_deref(),
            Some("minimal")
        );
        app.model_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.model_overlay.as_ref().unwrap().selected_effort, None);
        let _ = picker_lines(&app.model_overlay.as_ref().unwrap().picker, 7, false);
        app.model_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        let lines = picker_lines(&app.model_overlay.as_ref().unwrap().picker, 7, false);
        assert!(lines.iter().any(|line| line
            .spans
            .iter()
            .any(|span| span.content.contains("(8/30 of 30)"))));
        app.model_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        assert!(matches!(request.request,Request::TestModel{model,..} if model=="model-07"));
        for key in [
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        ] {
            app.model_overlay = Some(ModelOverlay {
                picker: models(),
                meta: serde_json::json!({}),
                selected_effort: None,
                default_only: false,
            });
            app.model_key(key);
            assert!(app.model_overlay.is_none());
            let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
            assert!(matches!(request.request,Request::SetSetting{key,..} if key=="provider.model"));
            let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
            assert!(
                matches!(request.request,Request::SetSetting{key,..} if key=="provider.reasoning_effort")
            );
        }
        let mut wizard = Wizard::new(&Config::default());
        wizard.step = WizardStep::PickModel;
        wizard.picker = models();
        wizard.picker.query = "alpha 日本語".into();
        app.wizard = Some(wizard);
        app.wizard_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.wizard.as_ref().unwrap().picker.query, "alpha ");
        app.wizard_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.wizard_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        assert!(app.wizard.as_ref().unwrap().picker.query.is_empty());
        app.wizard_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.wizard.as_ref().unwrap().picker.sel, 29);
        app.wizard_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.wizard.as_ref().unwrap().step, WizardStep::PickEffort);
        assert!(sent.try_recv().is_err());
    }

    #[test]
    fn switcher_and_rollback_page_by_drawn_rows_without_submitting_an_action() {
        let mut app = crate::tests::test_app();
        app.sessions=(0..30).map(|i|serde_json::from_value(serde_json::json!({"id":format!("s{i}"),"name":format!("Session {i}"),"created_at":"","updated_at":""})).unwrap()).collect();
        app.switcher = Some((String::new(), 0));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 18)).unwrap();
        terminal
            .draw(|frame| draw_switcher(frame, &app, "", 0))
            .unwrap();
        app.switcher_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.switcher.as_ref().unwrap().1, 10);
        app.switcher_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.switcher.as_ref().unwrap().1, 29);
        app.switcher_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.switcher.as_ref().unwrap().1, 0);
        app.switcher = Some(("alpha 日本語".into(), 0));
        app.switcher_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.switcher.as_ref().unwrap().0, "alpha ");
        app.switcher_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.switcher_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        assert!(app.switcher.as_ref().unwrap().0.is_empty());
        let mut detail = crate::tests::description_fixture_detail();
        detail.tool.current_version = 30;
        detail.versions=(1..=30).map(|version|serde_json::from_value(serde_json::json!({"version":version,"author":"user","created_at":"","capabilities":[],"memory_mb":16,"input_schema":"{}","output_schema":"{}","code":""})).unwrap()).collect();
        let id = detail.tool.id.clone();
        app.tool_detail = Some(detail);
        app.rollback_pick = Some(RollbackPick {
            tool_id: id,
            idx: 0,
            armed_version: Some(1),
        });
        terminal.backend_mut().resize(100, 14);
        terminal
            .draw(|frame| {
                let _ = draw_rollback_picker(frame, &app, app.rollback_pick.as_ref().unwrap());
            })
            .unwrap();
        app.rollback_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.rollback_pick.as_ref().unwrap().idx, 8);
        assert_eq!(app.rollback_pick.as_ref().unwrap().armed_version, None);
        app.rollback_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.rollback_pick.as_ref().unwrap().idx, 29);
        terminal
            .draw(|frame| {
                let _ = draw_rollback_picker(frame, &app, app.rollback_pick.as_ref().unwrap());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("› v30"));
    }
    #[test]
    fn readline_queries_preserve_unicode_and_ignore_other_modified_characters() {
        let mut query = "日本語 e\u{301} ไทย  ".to_string();
        assert_eq!(
            edit(
                &mut query,
                &KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)
            ),
            Some(true)
        );
        assert_eq!(query, "日本語 e\u{301} ");
        edit(
            &mut query,
            &KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
        );
        assert_eq!(query, "日本語 ");
        for (code, modifiers) in [
            (KeyCode::Char('d'), KeyModifiers::CONTROL),
            (KeyCode::Char('b'), KeyModifiers::ALT),
            (
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
        ] {
            assert_eq!(
                edit(&mut query, &KeyEvent::new(code, modifiers)),
                Some(false)
            );
            assert_eq!(query, "日本語 ");
        }
        edit(
            &mut query,
            &KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert!(query.is_empty());
        edit(
            &mut query,
            &KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT),
        );
        assert_eq!(query, "X");
    }
}
