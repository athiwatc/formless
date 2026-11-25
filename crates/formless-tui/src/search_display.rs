//! Presentation for one result batch. Highlighting approximates visible
//! term prefixes and literal punctuation without changing the server's search.
use crate::text;
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};
use std::fmt::Write;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, PartialEq)]
pub(super) struct Context {
    pub query: String,
    pub limit: usize,
    pub searched_latest: Option<usize>,
}

pub(super) fn label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() || text::is_bidi_control(c) {
            let _ = write!(escaped, "⟨U+{:04X}⟩", c as u32);
        } else {
            escaped.push(c);
        }
    }
    escaped
}

pub(super) fn title(count: usize, context: Option<&Context>, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut title = format!(" Search ({count} hits)");
    if let Some(context) = context {
        let capped = count == context.limit;
        if let Some(latest) = context.searched_latest {
            let _ = write!(title, " · Searched latest {latest} messages");
            let full_width = text::width(&title) + if capped { " · more may exist".len() } else { 0 };
            if full_width > width {
                let marker = format!(" Latest {latest}");
                let compact = format!("{marker} · {count} hits{}", if capped { " (cap)" } else { "" });
                return if text::width(&compact) <= width {
                    compact
                } else {
                    text::truncate(&marker, width)
                };
            }
        }
        if capped {
            title.push_str(" · more may exist");
            if text::width(&title) > width {
                let compact = format!(" {count} hits · more may exist");
                return if text::width(&compact) <= width {
                    compact
                } else {
                    text::truncate(&format!(" {count} hits (cap)"), width)
                };
            }
        }
        title.push_str(" · ");
        title.push_str(&label(&context.query));
    }
    title.push(' ');
    text::truncate(&title, width)
}

pub(super) fn empty(context: Option<&Context>, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let empty = context.map_or_else(
        || "No hits. /search to try again.".into(),
        |context| {
            format!(
                "No hits for “{}”. /search to try again.",
                label(&context.query)
            )
        },
    );
    text::truncate(&empty, width)
}

pub(super) struct Terms<'a> {
    prefixes: Vec<String>,
    literal: Option<&'a str>,
}

// Apply the same context-independent lowercase mapping to both sides. This
// preserves expandable characters without claiming full Unicode case folding.
fn fold(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().flat_map(char::to_lowercase)
}

impl<'a> Terms<'a> {
    pub(super) fn new(query: &'a str) -> Self {
        let query = query.trim();
        let prefixes: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(|word| fold(word).collect())
            .collect();
        let literal = (prefixes.is_empty() && !query.is_empty()).then_some(query);
        Self { prefixes, literal }
    }

    pub(super) fn snippet_spans(&self, snippet: &str, style: Style) -> Vec<Span<'static>> {
        if self.prefixes.is_empty() && self.literal.is_none() {
            return vec![Span::styled(label(snippet), style)];
        }
        let marked_style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let mut folded = String::new();
        // Map lowercase byte offsets back to whole original graphemes.
        let clusters: Vec<_> = snippet
            .grapheme_indices(true)
            .map(|(original, cluster)| {
                let start = folded.len();
                folded.extend(fold(cluster));
                (original, cluster, start)
            })
            .collect();
        let mut matched_until = 0;
        let mut spans = Vec::new();
        let mut run_start = 0;
        let mut was_marked = false;
        let mut was_word = false;
        for (original, cluster, start) in clusters {
            let is_word = cluster.chars().any(char::is_alphanumeric);
            if let Some(literal) = self.literal {
                if folded[start..].starts_with(literal) {
                    matched_until = matched_until.max(start + literal.len());
                }
            } else if is_word && !was_word {
                for term in &self.prefixes {
                    if folded[start..].starts_with(term) {
                        matched_until = matched_until.max(start + term.len());
                    }
                }
            }
            was_word = is_word;
            let marked = start < matched_until;
            if marked != was_marked {
                if run_start < original {
                    let ink = if was_marked { marked_style } else { style };
                    spans.push(Span::styled(label(&snippet[run_start..original]), ink));
                }
                run_start = original;
                was_marked = marked;
            }
        }
        if run_start < snippet.len() {
            let ink = if was_marked { marked_style } else { style };
            spans.push(Span::styled(label(&snippet[run_start..]), ink));
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn titles_report_only_the_actual_request_cap_and_escape_to_fit() {
        let context = Context {
            query: "ไทย界👩‍💻\u{202e}\n".into(),
            limit: 50,
            searched_latest: None,
        };
        let wide = title(50, Some(&context), 90);
        assert!(wide.contains("more may exist"));
        assert!(wide.contains("⟨U+202E⟩⟨U+000A⟩"));
        for count in [0, 49, 51] {
            assert!(!title(count, Some(&context), 90).contains("more may exist"));
        }
        assert!(!title(50, None, 90).contains("more may exist"));
        assert!(title(50, Some(&context), 28).contains("more may exist"));
        assert_eq!(title(50, Some(&context), 14), " 50 hits (cap)");
        for width in 0..80 {
            assert!(text::width(&title(50, Some(&context), width)) <= width);
            assert!(text::width(&empty(Some(&context), width)) <= width);
        }
    }

    #[test]
    fn compact_titles_keep_the_search_scope_even_at_the_result_cap() {
        let context = Context { query: "??".into(), limit: 50, searched_latest: Some(10_000) };
        assert_eq!(title(50, Some(&context), 14), " Latest 10000");
        assert_eq!(title(0, Some(&context), 14), " Latest 10000");
        assert!(title(50, Some(&context), 30).contains("50 hits (cap)"));
    }

    #[test]
    fn snippets_keep_graphemes_and_styles_and_merge_overlapping_prefixes() {
        let style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::ITALIC);
        let snippet = "FooBar food suffixfoo İtem 界e\u{301}! ΟΣ οσ 👩‍💻";
        let spans = Terms::new("foo fo İte 界e ΟΣ absent").snippet_spans(snippet, style);
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            snippet
        );
        let marked: Vec<_> = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(marked, ["Foo", "foo", "İte", "界e\u{301}", "ΟΣ", "οσ"]);
        assert!(spans
            .iter()
            .all(|s| s.style.fg == Some(Color::Cyan)
                && s.style.add_modifier.contains(Modifier::ITALIC)));

        let marks: Vec<_> = Terms::new("!!")
            .snippet_spans("a !!! and ??", style)
            .into_iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.into_owned())
            .collect();
        assert_eq!(marks, ["!!!"]);
    }

    #[test]
    fn snippets_escape_inert_control_scalars_after_highlighting() {
        let spans = Terms::new("ไทย").snippet_spans("ไทย\u{202e}\t界👩‍💻", Style::default());
        assert_eq!(spans[0].content, "ไทย");
        assert!(spans[0].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(spans[1].content, "⟨U+202E⟩⟨U+0009⟩界👩‍💻");
        assert!(spans[1].style.add_modifier.is_empty());
    }
}
