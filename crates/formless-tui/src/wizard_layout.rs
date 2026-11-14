//! Geometry only for the setup wizard; no setup/input/configuration operations.
use super::*;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Existing bounded setup hints use hard rows, retaining complete graphemes.
pub(super) fn hint_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut row = String::new();
        let mut used = 0;
        for grapheme in line.graphemes(true) {
            let cells = grapheme.width();
            if used + cells > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push_str(grapheme);
            used += cells;
        }
        rows.push(row);
    }
    rows
}
fn rows(lines: &[Line<'static>], width: u16) -> usize {
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

// Picker highlighting can emit one span per scalar. Rejoin identical styles
// so combining/ZWJ graphemes remain whole without changing any scalar's style.
fn coalesce(mut line: Line<'static>) -> Line<'static> {
    let spans = std::mem::take(&mut line.spans);
    for span in spans {
        if let Some(previous) = line
            .spans
            .last_mut()
            .filter(|previous| previous.style == span.style)
        {
            previous.content.to_mut().push_str(&span.content);
        } else {
            line.spans.push(span);
        }
    }
    line
}

pub(super) fn draw(
    f: &mut Frame,
    lines: Vec<Line<'static>>,
    footer: Line<'static>,
    title: String,
    color: bool,
) {
    let lines: Vec<_> = lines.into_iter().map(coalesce).collect();
    let area = f.area();
    let width = area.width.min(84);
    let inner_w = width.saturating_sub(2);
    let footer_rows = rows(std::slice::from_ref(&footer), inner_w.max(1));
    if inner_w < 2 || (area.height as usize) < footer_rows.saturating_add(3) {
        f.render_widget(
            Paragraph::new("Setup: enlarge terminal").wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let body_rows = rows(&lines, inner_w);
    let height =
        (body_rows.saturating_add(footer_rows).saturating_add(2)).min(area.height as usize) as u16;
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width,
        height,
    };
    let block = ui::panel(title, color, Color::Cyan);
    let inner = block.inner(rect);
    let footer_h = (footer_rows.min(inner.height as usize)) as u16;
    let body_h = inner.height.saturating_sub(footer_h);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    // A selected picker row takes priority over its filter cursor. Other steps
    // follow the existing input marker. Count hidden context before scrolling.
    let focus = lines
        .iter()
        .position(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.content.starts_with("› "))
        })
        .or_else(|| {
            lines
                .iter()
                .position(|line| line.spans.iter().any(|span| span.content.contains('▏')))
        })
        .unwrap_or(0);
    let before = rows(&lines[..focus], inner_w);
    let active = lines
        .get(focus)
        .map(|line| rows(std::slice::from_ref(line), inner_w))
        .unwrap_or(0);
    let mut top = if before.saturating_add(active) > body_h as usize {
        before.saturating_sub((body_h as usize).saturating_sub(active.min(body_h as usize)))
    } else {
        0
    };
    let mut start = 0;
    while start < focus {
        let height = rows(&lines[start..start + 1], inner_w);
        if top < height {
            break;
        }
        top -= height;
        start += 1;
    }
    // Pinned Paragraph adds scroll + height and area.top + logical row
    // in u16 arithmetic. Bound both intermediates, not only the offset.
    // Jump to the selected/input row's beginning if either could overflow.
    if top
        .saturating_add(body_h as usize)
        .saturating_add(inner.y as usize)
        > u16::MAX as usize
    {
        start = focus;
        top = 0;
    }
    let clipped = active > body_h as usize && body_h > 1;
    let shown_h = body_h.saturating_sub(u16::from(clipped));
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .scroll((top as u16, 0)),
        Rect {
            height: shown_h,
            ..inner
        },
    );
    if clipped {
        f.render_widget(
            Paragraph::new(Line::styled(
                "… row clipped; enlarge terminal",
                ui::dim(color),
            )),
            Rect {
                y: inner.y + shown_h,
                height: 1,
                ..inner
            },
        );
    }
    f.render_widget(
        Paragraph::new(footer).wrap(Wrap { trim: false }),
        Rect {
            y: inner.y + body_h,
            height: footer_h,
            ..inner
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    fn render(w: &Wizard, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw_wizard(f, w, true)).unwrap();
        terminal.backend().buffer().clone()
    }
    fn flat(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace() && !"│─┌┐└┘".contains(*c))
            .collect()
    }
    fn wizard() -> Wizard {
        let mut w = Wizard::new(&Config::default());
        w.step = WizardStep::PickModel;
        w.profile = "fixture-profile".into();
        w.status = "Choose a fixture model".into();
        w.picker = picker::ModelPicker::new(
            (0..24)
                .map(|i| picker::ModelItem {
                    id: format!("model-{i:02}-{}", "x".repeat(120)),
                    source: Some(
                        if i == 10 {
                            "selected-source"
                        } else {
                            "other-source"
                        }
                        .into(),
                    ),
                    detail: Some(
                        if i == 10 {
                            "chosen-detail"
                        } else {
                            "other-detail"
                        }
                        .into(),
                    ),
                    recent: false,
                })
                .collect(),
        );
        w.picker.sel = 10;
        w
    }
    fn hint(cancel: bool) -> &'static str {
        if cancel {
            "Enter continue · Esc back (at Provider: cancel) · Ctrl+C quit"
        } else {
            "Enter continue · Esc back · Ctrl+C quit"
        }
    }
    #[test]
    fn wizard_long_selected_rows_and_both_wrapped_footers_remain_visible() {
        for cancel in [false, true] {
            let mut w = wizard();
            w.can_cancel = cancel;
            let original = w.picker.items.clone();
            for (width, height) in [(80, 24), (100, 24), (40, 24)] {
                let buffer = render(&w, width, height);
                let text = flat(&buffer);
                assert!(text.contains("model-10"), "{width}x{height}: {text}");
                assert!(
                    text.contains("chosen-detail") && text.contains("selected-source"),
                    "{text}"
                );
                assert!(
                    text.contains(
                        &hint(cancel)
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect::<String>()
                    ),
                    "{text}"
                );
                if width >= 80 {
                    assert!(text.contains("profile:fixture-profile"));
                }
                assert!(buffer
                    .content()
                    .iter()
                    .any(|cell| cell.symbol() == "›" && cell.modifier.contains(Modifier::BOLD)));
            }
            assert_eq!(w.picker.items, original);
            assert_eq!(w.picker.sel, 10);
        }
    }
    #[test]
    fn wizard_long_filter_no_match_and_short_list_reserve_footer() {
        let mut w = wizard();
        w.picker.query = "no match ไทย ".repeat(60);
        for cancel in [false, true] {
            w.can_cancel = cancel;
            for width in [80, 100, 40] {
                let text = flat(&render(&w, width, 24));
                assert!(
                    text.contains(
                        &hint(cancel)
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect::<String>()
                    ),
                    "{text}"
                );
            }
        }
        w.picker = picker::ModelPicker::new(vec![picker::ModelItem {
            id: "short".into(),
            source: None,
            detail: None,
            recent: false,
        }]);
        let text = flat(&render(&w, 80, 24));
        assert!(
            text.contains("Filter")
                && text.contains("›short")
                && text.contains("Chooseafixturemodel"),
            "{text}"
        );
    }
    #[test]
    fn wizard_unicode_hints_resize_and_tiny_fallback_are_bounded() {
        let source = "ไทย e\u{301} 👨‍👩‍👧‍👦 日本語";
        let rows = hint_rows(source, 8);
        assert_eq!(rows.concat(), source);
        assert!(rows.iter().any(|row| row.contains("👨‍👩‍👧‍👦")));
        let mut w = wizard();
        w.picker = picker::ModelPicker::new(vec![picker::ModelItem {
            id: source.into(),
            source: None,
            detail: None,
            recent: false,
        }]);
        for (width, height) in [(80, 24), (40, 12), (20, 4), (3, 80), (4, 2), (1, 1), (0, 0)] {
            let buffer = render(&w, width, height);
            assert_eq!(buffer.area.width, width);
            assert_eq!(buffer.area.height, height);
            if width >= 40 {
                assert!(buffer.content().iter().any(|cell| cell.symbol() == "👨‍👩‍👧‍👦"));
            }
            if width == 20 || width == 3 {
                assert!(flat(&buffer).contains("enlargeterminal"));
            }
        }
    }
    #[test]
    fn wizard_oversized_prefix_and_active_row_use_bounded_viewing_fallback() {
        let mut w = wizard();
        w.status = "prefix ".repeat(70000);
        let text = flat(&render(&w, 80, 24));
        assert!(text.contains("model-10"));
        assert!(text.contains("Entercontinue"));
        w.status = "fixture".into();
        w.picker = picker::ModelPicker::new(vec![picker::ModelItem {
            id: format!("huge-{}", "x".repeat(5000)),
            source: None,
            detail: None,
            recent: false,
        }]);
        let text = flat(&render(&w, 40, 12));
        assert!(
            text.contains("›huge-")
                && text.contains("rowclipped;enlargeterminal")
                && text.contains("Entercontinue"),
            "{text}"
        );
    }
    fn boundary_fixture(prefix_rows: usize, y: u16) {
        let mut terminal = Terminal::with_options(
            TestBackend::new(40, 20),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(Rect::new(0, y, 40, 12)),
            },
        )
        .unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    vec![
                        Line::raw("x".repeat(38 * prefix_rows)),
                        Line::raw("› selected"),
                    ],
                    Line::raw("footer"),
                    "fixture".into(),
                    false,
                )
            })
            .unwrap();
        let text = flat(terminal.backend().buffer());
        assert!(
            text.contains("›selected") && text.contains("footer"),
            "{text}"
        );
    }

    #[test]
    fn wizard_boundary_residual_offset_plus_height_must_not_overflow() {
        boundary_fixture(65_535, 0);
    }

    #[test]
    fn wizard_boundary_shifted_viewport_coordinates_must_not_overflow() {
        boundary_fixture(65_534, 3);
    }
}
