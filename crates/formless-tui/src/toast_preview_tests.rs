use super::*;
use ratatui::backend::TestBackend;
use unicode_segmentation::UnicodeSegmentation;

fn screen(app: &App, area: Rect) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal
        .draw(|frame| {
            for cell in &mut frame.buffer_mut().content {
                cell.set_symbol("·");
            }
            draw_toasts(frame, area, app);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn error_preview_preserves_whole_unicode_clusters_and_multiline_content() {
    let source = "error: 日本語 👩🏽‍💻 e\u{301} ไทย\nsecond line";
    let (rows, omitted) = toast_error_rows(source, 16, 3);
    assert!(!omitted, "{rows:?}");
    assert!(rows.len() > 1 && rows.len() <= 3);
    assert!(rows.iter().all(|row| text::width(row) <= 16));
    assert_eq!(rows.join(""), source.replace('\n', ""));
    let long = source.repeat(20);
    for cap in 1..=3 {
        let (rows, omitted) = toast_error_rows(&long, 16, cap);
        assert!(omitted && rows.len() <= cap);
        assert!(rows.last().unwrap().ends_with('…'));
        assert!(rows.iter().all(|row| text::width(row) <= 16));
        for cluster in rows.iter().flat_map(|row| row.graphemes(true)) {
            assert!(
                cluster == "…" || source.graphemes(true).any(|original| original == cluster),
                "split cluster {cluster:?}"
            );
        }
    }
}

#[test]
fn simultaneous_errors_fit_offset_rectangles_including_the_smallest_allowed() {
    let mut app = crate::tests::test_app();
    app.toasts = (0..4)
        .map(|i| {
            (
                format!("error {i}: {}", "日本語 👩🏽‍💻 long detail\n".repeat(20)),
                Instant::now(),
                true,
            )
        })
        .collect();
    for (width, height) in [
        (20, 4),
        (20, 5),
        (24, 7),
        (40, 8),
        (100, 20),
        (19, 10),
        (20, 3),
    ] {
        let area = Rect::new(7, 5, width, height);
        let buffer = screen(&app, area);
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if !area.contains((x, y).into()) || y == area.y || width < 20 || height < 4 {
                    assert_eq!(buffer[(x, y)].symbol(), "·", "escaped {area:?} at {x},{y}");
                }
            }
        }
        if width >= 20 && height >= 4 {
            assert!(
                buffer_text(&buffer).contains("/notes"),
                "missing full-text hint in {area:?}"
            );
        }
    }
}

#[test]
fn error_preview_expands_but_normal_toasts_and_retained_notes_keep_their_policy() {
    let mut app = crate::tests::test_app();
    let error = format!("failed: {}", "useful diagnostic detail ".repeat(15));
    app.notes = vec!["saved\nordinary".into(), error.clone()];
    app.notes_seen = 0;
    app.refresh_toasts();
    let area = Rect::new(0, 0, 100, 20);
    let text = buffer_text(&screen(&app, area));
    assert!(text.contains("saved ordinary"));
    assert!(text.matches("useful diagnostic").count() >= 2, "{text}");
    assert!(text.contains("/notes"));
    assert!(text.contains('…'));
    for toast in &mut app.toasts {
        toast.1 = Instant::now() - Duration::from_secs(60);
    }
    app.refresh_toasts();
    assert_eq!(app.toasts.len(), 1);
    assert!(app.toasts[0].2);
    app.slash("notes");
    assert_eq!(app.pager.as_ref().unwrap().lines, app.notes);
    assert_eq!(app.notes[1], error);
    app.pager = None;
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.toasts.is_empty());
    assert_eq!(app.notes.len(), 2);
}
