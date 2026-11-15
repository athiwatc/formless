//! A bounded, active-anchored strip. Hidden neighbors have no view hit region.
use crate::text;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Brand,
    View(usize),
    Hint,
}
pub(crate) struct Part {
    pub(crate) text: String,
    pub(crate) kind: Kind,
}
fn part(text: impl Into<String>, kind: Kind) -> Part {
    Part {
        text: text.into(),
        kind,
    }
}
fn used(parts: &[Part]) -> usize {
    parts.iter().map(|p| text::width(&p.text)).sum()
}

pub(crate) fn layout(
    labels: &[&str],
    active: usize,
    columns: usize,
    brand: &str,
    tip: &str,
) -> Vec<Part> {
    if labels.is_empty() || columns == 0 {
        return Vec::new();
    }
    let active = active.min(labels.len() - 1);
    let full: Vec<String> = labels.iter().map(|label| format!(" {label} ")).collect();
    let widths: Vec<usize> = full.iter().map(|label| text::width(label)).collect();
    let all: usize = widths.iter().sum();
    let mut parts = Vec::new();
    if all <= columns {
        if all + text::width(brand) <= columns {
            parts.push(part(brand, Kind::Brand));
        }
        parts.extend(
            full.into_iter()
                .enumerate()
                .map(|(i, label)| part(label, Kind::View(i))),
        );
    } else {
        let (mut start, mut end) = (active, active);
        let mut width = widths[active];
        let cues = usize::from(start > 0) + usize::from(end + 1 < labels.len());
        if width + cues > columns {
            // At tiny widths prefer a visible active fallback over cues alone.
            let show_cues = columns > cues;
            let budget = columns - if show_cues { cues } else { 0 };
            if show_cues && start > 0 {
                parts.push(part("‹", Kind::Hint));
            }
            parts.push(part(
                text::truncate(labels[active], budget),
                Kind::View(active),
            ));
            if show_cues && end + 1 < labels.len() {
                parts.push(part("›", Kind::Hint));
            }
        } else {
            loop {
                let mut grew = false;
                if start > 0
                    && width
                        + widths[start - 1]
                        + usize::from(start > 1)
                        + usize::from(end + 1 < labels.len())
                        <= columns
                {
                    start -= 1;
                    width += widths[start];
                    grew = true;
                }
                if end + 1 < labels.len()
                    && width
                        + widths[end + 1]
                        + usize::from(start > 0)
                        + usize::from(end + 2 < labels.len())
                        <= columns
                {
                    end += 1;
                    width += widths[end];
                    grew = true;
                }
                if !grew {
                    break;
                }
            }
            if start > 0 {
                parts.push(part("‹", Kind::Hint));
            }
            parts.extend((start..=end).map(|i| part(full[i].clone(), Kind::View(i))));
            if end + 1 < labels.len() {
                parts.push(part("›", Kind::Hint));
            }
        }
    }
    let remaining = columns.saturating_sub(used(&parts));
    let tip_width = text::width(tip);
    if tip_width > 0 && tip_width <= remaining {
        parts.push(part(" ".repeat(remaining - tip_width), Kind::Hint));
        parts.push(part(tip, Kind::Hint));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_layout_keeps_active_and_bounded_order_with_overflow_cues() {
        let labels = [
            "Chat",
            "Sessions",
            "Tools",
            "Metrics",
            "Proposals",
            "Settings",
            "Search",
        ];
        for active in 0..labels.len() {
            for columns in 0..150 {
                let parts = layout(&labels, active, columns, " ◇ Formless ", " Tab ⇥ ");
                assert!(used(&parts) <= columns);
                let indices: Vec<_> = parts
                    .iter()
                    .filter_map(|p| {
                        if let Kind::View(i) = p.kind {
                            Some(i)
                        } else {
                            None
                        }
                    })
                    .collect();
                if columns > 0 {
                    assert!(indices.contains(&active));
                }
                assert!(indices.windows(2).all(|pair| pair[0] + 1 == pair[1]));
                if columns >= 15 && indices.len() < labels.len() {
                    assert!(parts.iter().any(|p| p.text == "‹" || p.text == "›"));
                }
            }
        }
    }

    #[test]
    fn tab_layout_uses_graphemes_and_display_width_for_the_tip() {
        let parts = layout(&["left", "👩‍💻e\u{301}", "right"], 1, 5, "brand", "tip");
        assert_eq!(used(&parts), 5);
        assert!(parts
            .iter()
            .any(|p| p.kind == Kind::View(1) && p.text == "👩‍💻e\u{301}"));
        assert!(layout(&["A"], 0, 7, "B", " ⇥ ")
            .iter()
            .any(|p| p.text == " ⇥ "));
        assert!(!layout(&["A"], 0, 6, "B", " ⇥ ")
            .iter()
            .any(|p| p.text == " ⇥ "));
    }

    #[test]
    fn drawn_tab_hit_regions_match_visible_geometry_and_keep_active_view() {
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};
        for view in crate::VIEW_ORDER.into_iter().chain([crate::View::Search]) {
            let mut app = crate::tests::test_app();
            app.view = view;
            for width in [12, 20, 40, 80, 120, 0, 1, 2, 3, 8] {
                app.click_targets.borrow_mut().clear();
                let area = Rect::new(2, 1, width, 1);
                let mut terminal = Terminal::new(TestBackend::new(width + 4, 3)).unwrap();
                terminal
                    .draw(|frame| crate::draw_tab_bar(frame, area, &app))
                    .unwrap();
                let hits = app.click_targets.borrow();
                assert!(
                    hits.iter()
                        .all(|(rect, _)| rect.width > 0 && rect.intersection(area) == *rect),
                    "off-bar hit: {width}"
                );
                if width > 0 {
                    assert!(
                        hits.iter().any(|(_, hit)| hit == &crate::Click::View(view)),
                        "active view hidden: {width}"
                    );
                } else {
                    assert!(hits.is_empty());
                }
            }
        }
    }
}
