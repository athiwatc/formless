//! Metrics has its own all-Tool, invocation-ordered selection model.
use super::*;

#[derive(Default)]
pub(super) struct State {
    pub(super) selected: Option<String>,
    pub(super) index: usize,
    pub(super) table: TableState,
}

impl App {
    pub(super) fn metrics_rows(&self) -> Vec<ToolInfo> {
        let ranged = !self.metrics_range.is_multiple_of(METRIC_RANGES.len());
        let mut rows = self
            .tools
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                if ranged {
                    let (calls, rate) =
                        self.range_tools.get(&tool.id).copied().unwrap_or((0, None));
                    tool.invocations = calls;
                    tool.success_rate = rate;
                }
                tool
            })
            .collect::<Vec<_>>();
        // Stable sorting retains the daemon's order for equal counts.
        rows.sort_by_key(|tool| std::cmp::Reverse(tool.invocations));
        rows
    }

    pub(super) fn metrics_selection(&self, rows: &[ToolInfo]) -> Option<usize> {
        let mut state = self.metrics_tools.borrow_mut();
        let index = state
            .selected
            .as_ref()
            .and_then(|id| rows.iter().position(|tool| &tool.id == id))
            .unwrap_or(state.index)
            .min(rows.len().saturating_sub(1));
        let selected = rows.get(index).map(|tool| tool.id.clone());
        state.index = index;
        state.selected = selected;
        let index = state.selected.as_ref().map(|_| index);
        state.table.select(index);
        if index.is_none() {
            *state.table.offset_mut() = 0;
        }
        index
    }

    pub(super) fn select_metrics_row(&self, rows: &[ToolInfo], index: usize) {
        let mut state = self.metrics_tools.borrow_mut();
        let index = index.min(rows.len().saturating_sub(1));
        state.index = index;
        state.selected = rows.get(index).map(|tool| tool.id.clone());
        let index = state.selected.as_ref().map(|_| index);
        state.table.select(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn app() -> App {
        let mut app = crate::tests::test_app();
        app.view = View::Metrics;
        app.color = false;
        app.tools = (0..30)
            .map(|index| {
                let mut tool = crate::tests::description_fixture_detail().tool;
                tool.id = format!("tool-{index:02}");
                tool.name = tool.id.clone();
                tool.title = String::new();
                tool.invocations = 30 - index;
                tool
            })
            .collect();
        app.tool_filter.query = "unrelated Tools filter".into();
        app.tool_idx = 12;
        app
    }
    fn render(app: &App, height: u16) -> ratatui::buffer::Buffer {
        app.click_targets.borrow_mut().clear();
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal
            .draw(|frame| draw_metrics(frame, frame.area(), app))
            .unwrap();
        terminal.backend().buffer().clone()
    }
    fn text(buffer: &ratatui::buffer::Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn key(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn metrics_selection_scrolls_and_clicks_use_the_same_all_tool_order() {
        let mut app = app();
        let buffer = render(&app, 16);
        assert!(
            text(&buffer)
                .lines()
                .any(|row| row.contains("› ") && row.contains("tool-00")),
            "{}",
            text(&buffer)
        );
        key(&mut app, KeyCode::Char('G'));
        assert_eq!(
            app.metrics_tools.borrow().selected.as_deref(),
            Some("tool-29")
        );
        for height in [16, 24, 14] {
            let buffer = render(&app, height);
            let screen = text(&buffer);
            let selected = screen
                .lines()
                .enumerate()
                .find(|(_, row)| row.contains("› ") && row.contains("tool-29"))
                .unwrap_or_else(|| panic!("{screen}"));
            assert!(app.metrics_tools.borrow().table.offset() > 0);
            assert!(buffer[(4, selected.0 as u16)]
                .modifier
                .contains(Modifier::REVERSED));
        }
        let targets = app.click_targets.borrow().clone();
        let (rect, click) = targets
            .iter()
            .find(|(_, click)| matches!(click,Click::MetricsTool(id) if id!="tool-29"))
            .unwrap();
        let Click::MetricsTool(id) = click else {
            unreachable!()
        };
        let screen = text(&render(&app, 14));
        assert!(screen.lines().nth(rect.y as usize).unwrap().contains(id));
        app.click(click.clone());
        assert_eq!(
            app.metrics_tools.borrow().selected.as_deref(),
            Some(id.as_str())
        );
        assert_eq!(app.tool_idx, 12);
        assert_eq!(app.tool_filter.query, "unrelated Tools filter");
        key(&mut app, KeyCode::Home);
        assert_eq!(
            app.metrics_tools.borrow().selected.as_deref(),
            Some("tool-00")
        );
        key(&mut app, KeyCode::Down);
        assert_eq!(
            app.metrics_tools.borrow().selected.as_deref(),
            Some("tool-01")
        );
    }

    #[test]
    fn ranged_reordering_equal_counts_and_shrinking_keep_or_clamp_identity() {
        let mut app = app();
        let rows = app.metrics_rows();
        app.select_metrics_row(&rows, 1);
        app.metrics_range = 1;
        app.range_tools.insert("tool-29".into(), (100, Some(0.5)));
        app.range_tools.insert("tool-01".into(), (50, None));
        let rows = app.metrics_rows();
        assert_eq!(rows[0].id, "tool-29");
        assert_eq!(rows[0].invocations, 100);
        assert_eq!(app.metrics_selection(&rows), Some(1));
        assert_eq!(
            app.metrics_tools.borrow().selected.as_deref(),
            Some("tool-01")
        );
        app.range_tools.insert("tool-00".into(), (50, None));
        let rows = app.metrics_rows();
        assert_eq!(rows[1].id, "tool-00");
        assert_eq!(rows[2].id, "tool-01");
        assert_eq!(app.metrics_selection(&rows), Some(2));
        app.metrics_range = 0;
        assert_eq!(app.metrics_selection(&app.metrics_rows()), Some(1));
        app.tools.retain(|tool| tool.id == "tool-10");
        assert_eq!(app.metrics_selection(&app.metrics_rows()), Some(0));
        assert_eq!(
            app.metrics_tools.borrow().selected.as_deref(),
            Some("tool-10")
        );
        app.tools.clear();
        assert_eq!(app.metrics_selection(&app.metrics_rows()), None);
        assert_eq!(app.metrics_tools.borrow().table.offset(), 0);
        key(&mut app, KeyCode::End);
        key(&mut app, KeyCode::Enter);
        assert!(app.metrics_tools.borrow().selected.is_none());
    }

    #[test]
    fn enter_requests_highlighted_tool_and_unrelated_cached_detail_stays_hidden() {
        let mut app = app();
        app.tool_detail = Some(crate::tests::description_fixture_detail());
        let (client, sent) = DaemonClient::recording();
        app.client = Some(client);
        key(&mut app, KeyCode::Down);
        let screen = text(&render(&app, 20));
        assert!(screen.contains("Enter loads detail for tool-01 (all time)."));
        assert!(!screen.contains("Inventory reader"));
        key(&mut app, KeyCode::Enter);
        let request: formless_protocol::RequestEnvelope =
            serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        assert!(matches!(request.request,Request::GetTool{id} if id=="tool-01"));
        assert!(sent.try_recv().is_err());
        let mut detail = crate::tests::description_fixture_detail();
        detail.tool = app.tools[1].clone();
        detail.metrics.success = 123;
        app.tool_detail = Some(detail);
        let screen = text(&render(&app, 20));
        assert!(
            screen.contains("tool-01 · all time") && screen.contains("ok 123"),
            "{screen}"
        );
        key(&mut app, KeyCode::Down);
        let screen = text(&render(&app, 20));
        assert!(!screen.contains("ok 123"));
        assert!(screen.contains("Enter loads detail for tool-02"));
    }
}
