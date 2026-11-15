//! Bounded palette row and border-hint presentation; dispatch lives in App.
use crate::{text, ui};
use ratatui::prelude::*;

pub(crate) fn row(
    label: &str,
    desc: &str,
    label_width: usize,
    columns: usize,
    selected: bool,
    color: bool,
) -> Line<'static> {
    if columns == 0 {
        return Line::default();
    }
    let marker = if selected { "›" } else { " " };
    let prefix = if columns >= 2 {
        format!("{marker} ")
    } else {
        marker.into()
    };
    let available = columns.saturating_sub(text::width(&prefix));
    // Where possible reserve two separator columns and an explicit description
    // truncation marker, instead of allowing the label to hide all description.
    let reserve = if !desc.is_empty() && available >= 4 {
        3
    } else {
        0
    };
    let label_width = label_width.min(available.saturating_sub(reserve));
    let label = if label_width == 0 {
        String::new()
    } else {
        ui::pad(label, label_width)
    };
    let style = if selected {
        crate::bold_fg(color, Color::Green)
    } else {
        Style::default()
    };
    let mut spans = vec![Span::styled(format!("{prefix}{label}"), style)];
    let remaining = columns.saturating_sub(spans[0].width());
    if !desc.is_empty() && remaining >= 3 {
        spans.push(Span::styled(
            format!("  {}", text::truncate(desc, remaining - 2)),
            ui::dim(color),
        ));
    }
    Line::from(spans)
}

pub(crate) fn title(count: usize, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let full = format!(" / commands · {count} · Tab complete · Enter run · Esc hide ");
    let compact = format!("/ {count} · Tab/Enter/Esc");
    for candidate in [
        full.as_str(),
        compact.as_str(),
        "Tab/Enter/Esc",
        "↹ ↵ Esc",
        "Esc",
    ] {
        if text::width(candidate) <= columns {
            return candidate.into();
        }
    }
    text::truncate("Esc", columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_rows_fit_display_columns_and_mark_shortened_descriptions() {
        for label in ["/command with arguments", "/中文标签", "/👩‍💻e\u{301}"] {
            for width in 0..100 {
                let line = row(
                    label,
                    &"long description ".repeat(20),
                    text::width(label),
                    width,
                    true,
                    false,
                );
                assert!(line.width() <= width, "{width}: {line}");
                if width > 0 {
                    assert!(line.to_string().starts_with('›'));
                }
                if width >= 6 {
                    assert!(line.to_string().contains('…'));
                }
            }
        }
        assert!(row("x", "y", 1, 20, true, true).spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(row("👩‍💻e\u{301}", "", 3, 5, false, false)
            .to_string()
            .contains("👩‍💻e\u{301}"));
    }

    #[test]
    fn palette_hints_fit_and_keep_all_three_keys_when_possible() {
        for width in 0..100 {
            assert!(text::width(&title(20, width)) <= width);
        }
        for key in ["Tab", "Enter", "Esc"] {
            assert!(title(20, 18).contains(key));
        }
        assert!(title(20, 100).contains("Tab complete"));
    }

    #[test]
    fn actual_palette_draws_selected_window_and_compact_hints() {
        use ratatui::{backend::TestBackend, Terminal};
        let items: Vec<_> = (0..12)
            .map(|i| crate::PaletteItem {
                insert: format!("/command{i}"),
                label: format!("/中文command{i}"),
                desc: "description far wider than the popup ".repeat(10),
                runnable: true,
            })
            .collect();
        let mut terminal = Terminal::new(TestBackend::new(40, 14)).unwrap();
        terminal
            .draw(|frame| crate::draw_palette(frame, Rect::new(0, 12, 40, 2), &items, 11, false))
            .unwrap();
        let shown: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(shown.contains("command11"), "{shown}");
        assert!(
            shown.contains('…'),
            "description truncation must be visible: {shown}"
        );
        assert!(shown.contains("Esc"), "compact key hints must fit: {shown}");
        for width in [0, 1, 2, 4, 12, 20, 80] {
            let mut terminal = Terminal::new(TestBackend::new(width.max(1), 14)).unwrap();
            terminal
                .draw(|frame| {
                    crate::draw_palette(frame, Rect::new(0, 12, width, 2), &items, 11, false)
                })
                .unwrap();
        }
    }
}
