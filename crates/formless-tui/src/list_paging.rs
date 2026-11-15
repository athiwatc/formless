//! Per-view selectable capacity from the most recent rendered list body.
use super::*;

fn slot(view: View) -> Option<usize> {
    Some(match view {
        View::Sessions => 0,
        View::Tools => 1,
        View::Metrics => 2,
        View::Proposals => 3,
        View::Search => 4,
        _ => return None,
    })
}
impl App {
    pub(super) fn record_list_capacity(&self, view: View, count: usize) {
        if let Some(slot) = slot(view) {
            let mut rows = self.list_page_rows.get();
            rows[slot] = Some(count);
            self.list_page_rows.set(rows);
        }
    }
    pub(super) fn list_capacity(&self, view: View) -> usize {
        slot(view)
            .and_then(|slot| self.list_page_rows.get()[slot])
            .unwrap_or(20)
            .max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    const VIEWS: [View; 5] = [
        View::Sessions,
        View::Tools,
        View::Metrics,
        View::Proposals,
        View::Search,
    ];
    fn app() -> App {
        let mut app = crate::tests::test_app();
        app.sessions=(0..100).map(|i|serde_json::from_value(serde_json::json!({"id":format!("s{i:03}"),"name":format!("Session {i:03}"),"created_at":"","updated_at":""})).unwrap()).collect();
        app.tools = (0..100)
            .map(|i| {
                let mut tool = crate::tests::description_fixture_detail().tool;
                tool.id = format!("t{i:03}");
                tool.name = tool.id.clone();
                tool.title = String::new();
                tool.invocations = 100 - i;
                tool
            })
            .collect();
        app.proposals=(0..100).map(|i|serde_json::from_value(serde_json::json!({"id":format!("p{i:03}"),"kind":"prompt","content":format!("Proposal {i:03}"),"rationale":"","status":"proposed","baseline_score":null,"candidate_score":null,"eval_total":0,"est_tokens":0,"created_at":"","applied_at":null})).unwrap()).collect();
        app.search_hits = (0..100)
            .map(|i| SearchHit {
                session_id: "s000".into(),
                session_name: "Session".into(),
                message_id: format!("m{i}"),
                seq: i,
                role: "user".into(),
                snippet: format!("hit {i}"),
                created_at: String::new(),
            })
            .collect();
        app
    }
    fn index(app: &App, view: View) -> usize {
        match view {
            View::Sessions => app.session_idx,
            View::Tools => app.tool_idx,
            View::Metrics => app.metrics_tools.borrow().index,
            View::Proposals => app.proposal_idx,
            View::Search => app.search_idx,
            _ => unreachable!(),
        }
    }
    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.key(KeyEvent::new(code, modifiers));
    }
    fn render(app: &App, view: View, height: u16) {
        app.click_targets.borrow_mut().clear();
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal
            .draw(|frame| match view {
                View::Sessions => draw_sessions(frame, frame.area(), app),
                View::Tools => draw_tools(frame, frame.area(), app),
                View::Metrics => draw_metrics(frame, frame.area(), app),
                View::Proposals => draw_proposals(frame, frame.area(), app),
                View::Search => draw_search(frame, frame.area(), app),
                _ => unreachable!(),
            })
            .unwrap();
    }

    #[test]
    fn pages_and_half_pages_use_each_drawn_list_body_and_clamp() {
        for view in VIEWS {
            let mut app = app();
            app.view = view;
            render(&app, view, 18);
            let capacity = app.list_capacity(view);
            match view {
                View::Sessions | View::Search => assert_eq!(capacity, 16),
                View::Proposals => assert_eq!(capacity, 15),
                View::Tools | View::Metrics => {
                    let visible = app
                        .click_targets
                        .borrow()
                        .iter()
                        .filter(|(_, target)| {
                            matches!(
                                (view, target),
                                (View::Tools, Click::Tool(_))
                                    | (View::Metrics, Click::MetricsTool(_))
                            )
                        })
                        .count();
                    assert_eq!(capacity, visible);
                    assert!(capacity < 15);
                }
                _ => unreachable!(),
            }
            key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
            assert_eq!(index(&app, view), capacity, "{view:?}");
            key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert_eq!(index(&app, view), capacity + (capacity / 2).max(1));
            key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
            assert_eq!(index(&app, view), (capacity / 2).max(1));
            key(&mut app, KeyCode::End, KeyModifiers::NONE);
            assert_eq!(index(&app, view), 99);
            key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
            assert_eq!(index(&app, view), 99);
            key(&mut app, KeyCode::Home, KeyModifiers::NONE);
            assert_eq!(index(&app, view), 0);
            key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
            assert_eq!(index(&app, view), 0);
        }
    }
    #[test]
    fn grouped_tool_headers_consume_rows_and_each_view_retains_its_own_capacity() {
        let mut app = app();
        app.view = View::Tools;
        render(&app, View::Tools, 18);
        let plain = app.list_capacity(View::Tools);
        app.tool_filter.group = 3;
        render(&app, View::Tools, 18);
        let grouped = app.list_capacity(View::Tools);
        assert_eq!(grouped, plain - 1);
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(app.tool_idx, grouped);
        render(&app, View::Metrics, 40);
        assert_ne!(app.list_capacity(View::Metrics), grouped);
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(app.tool_idx, grouped);
        render(&app, View::Tools, 10);
        let smaller = app.list_capacity(View::Tools);
        assert!(smaller < grouped);
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(app.tool_idx, smaller);
    }
    #[test]
    fn pre_draw_fallback_empty_lists_and_search_keys_are_safe() {
        let mut app = app();
        app.view = View::Search;
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(app.search_idx, 20);
        key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(app.search_idx, 10);
        key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.search_idx, 10);
        key(&mut app, KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(app.search_idx, 99);
        key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(app.search_idx, 0);
        key(&mut app, KeyCode::PageDown, KeyModifiers::ALT);
        assert_eq!(app.search_idx, 0);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.view, View::Chat);
        app.sessions.clear();
        app.tools.clear();
        app.proposals.clear();
        app.search_hits.clear();
        for view in VIEWS {
            app.view = view;
            render(&app, view, 4);
            for code in [
                KeyCode::PageDown,
                KeyCode::PageUp,
                KeyCode::End,
                KeyCode::Home,
            ] {
                key(&mut app, code, KeyModifiers::NONE);
                assert_eq!(index(&app, view), 0);
            }
        }
    }
    fn filter(app: &mut App, view: View) -> &mut lists::ListFilter {
        match view {
            View::Sessions => &mut app.session_filter,
            View::Tools => &mut app.tool_filter,
            View::Proposals => &mut app.proposal_filter,
            _ => unreachable!(),
        }
    }
    #[test]
    fn filter_editing_consumes_pages_and_control_letters_keep_query_ownership() {
        for view in [View::Sessions, View::Tools, View::Proposals] {
            let mut app = app();
            app.view = view;
            app.session_idx = 7;
            app.tool_idx = 7;
            app.proposal_idx = 7;
            filter(&mut app, view).editing = true;
            filter(&mut app, view).query = "alpha beta".into();
            for code in [KeyCode::PageDown, KeyCode::PageUp] {
                key(&mut app, code, KeyModifiers::NONE);
                assert_eq!(index(&app, view), 7);
                assert_eq!(filter(&mut app, view).query, "alpha beta");
            }
            key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert_eq!(index(&app, view), 7);
            assert_eq!(filter(&mut app, view).query, "alpha beta");
            key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
            assert_eq!(index(&app, view), 0);
            assert!(filter(&mut app, view).query.is_empty());
            key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(filter(&mut app, view).query, "j");
            assert_eq!(index(&app, view), 0);
        }
    }
    #[test]
    fn visible_pager_and_model_overlay_keep_page_key_ownership() {
        let mut app = app();
        app.view = View::Tools;
        app.tool_idx = 7;
        app.pager = Some(Pager {
            title: "Fixture".into(),
            lines: (0..200).map(|i| i.to_string()).collect(),
            top: 0,
            left: 0,
            styled: Vec::new(),
        });
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert!(app.pager.as_ref().unwrap().top > 0);
        assert_eq!(app.tool_idx, 7);
        app.pager = None;
        app.model_overlay = Some(ModelOverlay {
            picker: picker::ModelPicker::new(
                (0..100)
                    .map(|i| picker::ModelItem {
                        id: format!("model{i}"),
                        source: None,
                        recent: false,
                        detail: None,
                    })
                    .collect(),
            ),
            meta: serde_json::json!({}),
            effort_idx: 0,
            default_only: false,
        });
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(app.model_overlay.as_ref().unwrap().picker.sel, 10);
        assert_eq!(app.tool_idx, 7);
    }
}
