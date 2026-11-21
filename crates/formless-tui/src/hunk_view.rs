//! Data-only interactive hunk presentation. All mutation identities and changed
//! word spans come from the server; this module never reads or patches a path.
use formless_protocol::{ReviewFile, ReviewLine};
use ratatui::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub file: usize,
    pub hunk: usize,
}

pub struct View {
    pub overview: Vec<Line<'static>>,
    pub hunk: Vec<Line<'static>>,
    pub selected: Option<Position>,
    pub total_hunks: usize,
    pub pending_hunks: usize,
}

/// Preserve joiners and combining marks while exposing terminal control data.
/// Escaping changes presentation only, never server IDs, paths or patch bytes.
pub fn display_text(text: &str) -> String {
    crate::text::escape_bidi(text)
        .chars()
        .map(|c| {
            if c.is_control() {
                format!("⟨U+{:04X}⟩", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

pub fn positions(files: &[ReviewFile]) -> Vec<Position> {
    files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.exclusion_reason.is_none())
        .flat_map(|(file, value)| (0..value.hunks.len()).map(move |hunk| Position { file, hunk }))
        .collect()
}

pub fn navigate(files: &[ReviewFile], index: usize, forward: bool) -> usize {
    let last = positions(files).len().saturating_sub(1);
    if forward {
        index.saturating_add(1).min(last)
    } else {
        index.min(last).saturating_sub(1)
    }
}

pub fn navigate_pending(files: &[ReviewFile], index: usize, forward: bool) -> Option<usize> {
    let positions = positions(files);
    let count = positions.len();
    let index = index.min(count.saturating_sub(1));
    (1..=count)
        .map(|step| {
            if forward {
                (index + step) % count
            } else {
                (index + count - step) % count
            }
        })
        .find(|&next| {
            let position = positions[next];
            files[position.file].hunks[position.hunk].decision == "pending"
        })
}

#[derive(Default)]
struct Decisions {
    pending: usize,
    accepted: usize,
    rejected: usize,
}

impl Decisions {
    fn summary(&self) -> String {
        format!(
            "{} pending · {} kept · {} rejected",
            self.pending, self.accepted, self.rejected
        )
    }
}

fn diff_line(line: &ReviewLine, color: bool) -> Line<'static> {
    let (mark, ink) = match line.kind.as_str() {
        "removed" => ('-', Color::Red),
        "added" => ('+', Color::Green),
        _ => (' ', Color::Yellow),
    };
    let number = |n: Option<u64>| n.map_or("—".into(), |n| n.to_string());
    let dim = if color {
        crate::fg(color, Color::DarkGray)
    } else {
        Style::default()
    };
    let mut spans = vec![
        Span::styled(
            format!("{:>6} {:>6} ", number(line.old_line), number(line.new_line)),
            dim,
        ),
        Span::styled(
            format!("{mark} "),
            if mark == ' ' {
                dim
            } else {
                crate::bold_fg(color, ink)
            },
        ),
    ];
    for word in &line.text {
        let mut style = Style::default();
        if word.changed {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            if color {
                style = style.patch(crate::fg(color, ink));
            }
        }
        spans.push(Span::styled(display_text(&word.text), style));
    }
    let ending = match line.ending.as_str() {
        "\n" => "LF".into(),
        "\r\n" => "CRLF".into(),
        "" => "no newline at EOF".into(),
        other => format!(
            "ending {}",
            display_text(&serde_json::to_string(other).unwrap_or_default())
        ),
    };
    spans.push(Span::styled(format!("  ⟨{ending}⟩"), dim));
    Line::from(spans)
}

pub fn render(files: &[ReviewFile], index: usize, color: bool) -> View {
    let all = positions(files);
    let selected = all.get(index.min(all.len().saturating_sub(1))).copied();
    let mut overview = vec![Line::default()];
    let mut total = Decisions::default();
    for file in files {
        let label = format!(
            "{} {}",
            display_text(&file.status),
            display_text(&file.path)
        );
        let row = if let Some(reason) = &file.exclusion_reason {
            format!("{label} · excluded · {}", display_text(reason))
        } else {
            let mut count = Decisions::default();
            for hunk in &file.hunks {
                match hunk.decision.as_str() {
                    "pending" => count.pending += 1,
                    "accepted" => count.accepted += 1,
                    "rejected" => count.rejected += 1,
                    _ => {}
                }
            }
            total.pending += count.pending;
            total.accepted += count.accepted;
            total.rejected += count.rejected;
            format!("{} · {label}", count.summary())
        };
        overview.push(Line::raw(row));
        overview.extend(
            file.warnings
                .iter()
                .map(|warning| Line::raw(format!("  {}", display_text(warning)))),
        );
    }
    overview[0] = Line::raw(format!(
        "{} files · {} editable hunks · {}",
        files.len(),
        all.len(),
        total.summary()
    ));
    let hunk = match selected {
        None => vec![Line::raw(
            "No editable hunks. Binary/generated and other excluded files remain listed above.",
        )],
        Some(position) => {
            let file = &files[position.file];
            let hunk = &file.hunks[position.hunk];
            let mut lines = vec![
                Line::raw(format!(
                    "{} · hunk {}/{} · {}",
                    display_text(&file.path),
                    index.min(all.len() - 1) + 1,
                    all.len(),
                    display_text(&hunk.decision)
                )),
                Line::raw(format!(
                    "@@ -{},{} +{},{} @@",
                    hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
                )),
            ];
            lines.extend(hunk.lines.iter().map(|line| diff_line(line, color)));
            lines
        }
    };
    View {
        overview,
        hunk,
        selected,
        total_hunks: all.len(),
        pending_hunks: total.pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::{Paragraph, Widget};

    fn file(path: &str, excluded: Option<&str>) -> ReviewFile {
        serde_json::from_value(serde_json::json!({"id":path,"path":path,"status":"modified","before_hash":"before","after_hash":"after","exclusion_reason":excluded,"warnings":[],"hunks":[{"id":format!("{path}:hunk"),"old_start":2,"old_lines":1,"new_start":2,"new_lines":1,"decision":"pending","lines":[{"kind":"removed","old_line":2,"new_line":null,"text":[{"text":"let value = ","changed":false},{"text":"7","changed":true},{"text":";","changed":false}],"ending":"\r\n"},{"kind":"added","old_line":null,"new_line":2,"text":[{"text":"let value = ","changed":false},{"text":"9","changed":true},{"text":";","changed":false}],"ending":""}]}]})).unwrap()
    }

    #[test]
    fn excluded_files_are_visible_but_never_receive_a_hunk_selection() {
        let files = vec![
            file("binary.dat", Some("Binary file")),
            file("src/main.rs", None),
            file("generated.rs", Some("Generated file")),
            file("other.rs", None),
        ];
        assert_eq!(
            positions(&files),
            [Position { file: 1, hunk: 0 }, Position { file: 3, hunk: 0 }]
        );
        assert_eq!(navigate(&files, 0, false), 0);
        assert_eq!(navigate(&files, 0, true), 1);
        assert_eq!(navigate(&files, 1, true), 1);
        let view = render(&files, 0, true);
        let overview = view
            .overview
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(overview.contains("binary.dat · excluded · Binary file"));
        assert!(overview.contains("generated.rs · excluded · Generated file"));
        assert_eq!(view.total_hunks, 2);
        let excluded = render(&[file("binary.dat", Some("Binary file"))], 0, true);
        assert_eq!(excluded.selected, None);
        assert_eq!(excluded.total_hunks, 0);
    }

    #[test]
    fn decision_counts_and_pending_navigation_only_include_editable_hunks() {
        let mut files = vec![
            file("first.rs", None),
            file("excluded", Some("Binary file")),
            file("second.rs", None),
            file("third.rs", None),
        ];
        files[2].hunks[0].decision = "accepted".into();
        let mut rejected = files[3].hunks[0].clone();
        rejected.decision = "rejected".into();
        files[3].hunks.push(rejected);
        let view = render(&files, 1, false);
        let rows: Vec<_> = view.overview.iter().map(ToString::to_string).collect();
        assert_eq!(
            rows[0],
            "4 files · 4 editable hunks · 2 pending · 1 kept · 1 rejected"
        );
        assert_eq!(
            rows[1],
            "1 pending · 0 kept · 0 rejected · modified first.rs"
        );
        assert!(rows[2].contains("excluded · Binary file"));
        assert_eq!(
            rows[3],
            "0 pending · 1 kept · 0 rejected · modified second.rs"
        );
        assert_eq!(
            rows[4],
            "1 pending · 0 kept · 1 rejected · modified third.rs"
        );
        assert_eq!(navigate_pending(&files, 0, true), Some(2));
        assert_eq!(navigate_pending(&files, 2, true), Some(0));
        assert_eq!(navigate_pending(&files, 0, false), Some(2));
        assert_eq!(navigate_pending(&files, 2, false), Some(0));
        assert_eq!(navigate_pending(&files, 1, true), Some(2));
        files[0].hunks[0].decision = "accepted".into();
        assert_eq!(navigate_pending(&files, 2, true), Some(2));
        files[3].hunks[0].decision = "accepted".into();
        assert_eq!(navigate_pending(&files, 1, true), None);
        assert_eq!(navigate_pending(&files, 1, false), None);
        assert_eq!(navigate_pending(&[], 0, true), None);
    }

    #[test]
    fn changed_words_are_highlighted_without_colouring_the_unchanged_line() {
        let view = render(&[file("src/main.rs", None)], 0, true);
        let row = &view.hunk[3];
        let changed = row.spans.iter().find(|span| span.content == "9").unwrap();
        let unchanged = row
            .spans
            .iter()
            .find(|span| span.content == "let value = ")
            .unwrap();
        assert_eq!(changed.style.fg, Some(Color::Green));
        assert!(changed.style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(unchanged.style, Style::default());
        let width = 100;
        let height = view.hunk.len() as u16;
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        Paragraph::new(view.hunk.clone()).render(buffer.area, &mut buffer);
        let rendered = row.to_string();
        let prefix_byte = rendered.find("let value = ").unwrap();
        let prefix = crate::text::width(&rendered[..prefix_byte]);
        let word = prefix + "let value = ".len();
        assert_eq!(buffer[(word as u16, 3)].fg, Color::Green);
        assert_ne!(buffer[(prefix as u16, 3)].fg, Color::Green);
        assert!(view.hunk[2].to_string().contains("⟨CRLF⟩"));
        assert!(view.hunk[3].to_string().contains("⟨no newline at EOF⟩"));
        let monochrome = render(&[file("src/main.rs", None)], 0, false);
        let word = monochrome.hunk[3]
            .spans
            .iter()
            .find(|span| span.content == "9")
            .unwrap();
        assert_eq!(word.style.fg, None);
        assert!(word.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn control_data_is_visible_while_unicode_graphemes_are_preserved() {
        let text = "a\u{301} 👩🏽‍💻 ไทย سلام\u{202e}\u{1b}[31m\t";
        let shown = display_text(text);
        assert!(shown.contains("a\u{301} 👩🏽‍💻 ไทย سلام"));
        assert!(shown.contains("⟨U+202E⟩"));
        assert!(shown.contains("⟨U+001B⟩[31m⟨U+0009⟩"));
        assert!(!shown.chars().any(char::is_control));
        let mut value = file("unsafe\u{1b}[31m.rs", None);
        value.hunks[0].lines[1].text[0].text = text.into();
        let view = render(&[value], 0, true);
        for line in view.overview.iter().chain(&view.hunk) {
            assert!(!line.to_string().chars().any(char::is_control));
            assert!(!line.to_string().contains('\u{202e}'));
        }
    }
    #[test]
    fn colorblind_hunks_keep_bold_gutters_word_marks_numbers_and_newline_labels() {
        let _theme = crate::theme::test_theme("colorblind");
        let files = [file("src/main.rs", None)];
        let colored = render(&files, 0, true);
        let plain = render(&files, 0, false);
        assert_eq!(
            colored
                .hunk
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            plain
                .hunk
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        let added = &colored.hunk[3];
        let gutter = added
            .spans
            .iter()
            .find(|span| span.content == "+ ")
            .unwrap();
        assert!(gutter.style.add_modifier.contains(Modifier::BOLD));
        let (r, g, b) = crate::colorblind_rgb(crate::SemanticTone::Good);
        assert_eq!(gutter.style.fg, Some(Color::Rgb(r, g, b)));
        let word = added.spans.iter().find(|span| span.content == "9").unwrap();
        assert!(word.style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(word.style.fg, Some(Color::Rgb(r, g, b)));
        assert!(plain.hunk[3]
            .spans
            .iter()
            .find(|span| span.content == "+ ")
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(plain.hunk[2].to_string().contains("CRLF"));
    }
}
