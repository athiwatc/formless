//! The history-search query and optional preview share one display-column row.
use crate::text;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn parts(query: &str, hit: &str, columns: usize) -> (String, String) {
    if columns == 0 {
        return (String::new(), String::new());
    }
    let full = format!("(history search) {query}▏: ");
    if text::width(&full) <= columns {
        let preview = preview(hit, columns - text::width(&full));
        return (full, preview);
    }
    let wanted = text::width(query).min(8);
    let prefix = ["(history search) ", "(search) ", "? ", ""]
        .into_iter()
        .find(|p| text::width(p) + wanted < columns)
        .unwrap_or("");
    let budget = columns.saturating_sub(text::width(prefix) + 1);
    let visible = tail(query, budget);
    let mut label = format!("{prefix}{visible}▏");
    let left = columns.saturating_sub(text::width(&label));
    if text::width(query) <= budget && left >= 2 {
        label.push_str(": ");
        return (label, preview(hit, left - 2));
    }
    (label, String::new())
}

fn preview(hit: &str, columns: usize) -> String {
    if columns == 0 {
        String::new()
    } else {
        crate::history_preview(hit, columns)
    }
}

// Keep the end of the query; a leading ellipsis marks omitted graphemes.
fn tail(query: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    if text::width(query) <= columns {
        return query.to_owned();
    }
    let mut used = 0;
    let mut start = query.len();
    for (at, cluster) in query.grapheme_indices(true).rev() {
        let width = text::width(cluster);
        if used + width > columns - 1 {
            break;
        }
        used += width;
        start = at;
    }
    format!("…{}", &query[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_fit_does_not_add_a_preview_beyond_the_row() {
        for query in ["abc", "中文", "👩‍💻"] {
            let label = format!("(history search) {query}▏: ");
            let (actual, preview) = parts(query, "long preview", text::width(&label));
            assert_eq!(actual, label);
            assert!(preview.is_empty());
        }
    }

    #[test]
    fn drawn_search_keeps_query_tail_and_marker_without_changing_matching() {
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};
        for query in [
            "a".repeat(100) + "END",
            "中文".repeat(50) + "尾",
            "👩‍💻".repeat(40) + "👍🏽",
        ] {
            for width in [12, 20, 40, 80] {
                let mut app = crate::tests::test_app();
                app.input.search_older(&[query.as_str()]);
                for c in query.chars() {
                    app.input.search_type(Some(c), &[query.as_str()]);
                }
                let before = app.input.searching().map(|(q, i)| (q.to_owned(), i));
                let mut terminal = Terminal::new(TestBackend::new(width, 3)).unwrap();
                terminal
                    .draw(|frame| {
                        crate::draw_input(frame, Rect::new(0, 0, width, 3), &app, true, None)
                    })
                    .unwrap();
                let shown: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(shown.contains('▏'), "{width}: {shown}");
                assert!(
                    shown.contains(query.graphemes(true).next_back().unwrap()),
                    "{width}: {shown}"
                );
                assert_eq!(
                    app.input.searching().map(|(q, i)| (q.to_owned(), i)),
                    before
                );
            }
        }
    }

    #[test]
    fn query_rows_are_bounded_and_keep_the_end_marker() {
        for query in [
            "a very long query ending HERE".repeat(10),
            "中文查询末尾".repeat(15),
            "👩‍💻e\u{301}👍🏽END".repeat(10),
        ] {
            let original = query.clone();
            for width in 0..100 {
                let (label, preview) = parts(&query, "preview text", width);
                assert!(
                    text::width(&label) + text::width(&preview) <= width,
                    "{width}: {label}"
                );
                if width > 0 {
                    assert!(label.contains('▏'));
                }
                if width >= 12 {
                    assert!(label.contains(query.graphemes(true).next_back().unwrap()));
                }
            }
            assert_eq!(query, original);
        }
    }

    #[test]
    fn fitting_queries_leave_display_columns_for_the_existing_preview() {
        for query in ["abc", "中文", "👩‍💻", "e\u{301}", ""] {
            let (label, preview) = parts(query, "first\nsecond with more text", 40);
            assert_eq!(label, format!("(history search) {query}▏: "));
            assert_eq!(
                preview,
                crate::history_preview("first\nsecond with more text", 40 - text::width(&label))
            );
        }
        assert_eq!(
            parts("long query", "hit", 0),
            (String::new(), String::new())
        );
        assert_eq!(parts("long query", "hit", 1), ("▏".into(), String::new()));
        assert_eq!(tail("aa👩‍💻e\u{301}", 4), "…👩‍💻e\u{301}");
    }
}
