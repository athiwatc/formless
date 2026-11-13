//! Read-only projection of the existing unarchived Session list. No GPUI,
//! no second Session store, no domain actions. Filtering never activates a
//! Session. Selection follows its ID across filtering and source reordering.

use crate::model::SessionRow;
use unicode_segmentation::UnicodeSegmentation as _;

pub const FILTER_MAX_CHARS: usize = 256;

pub fn valid_filter(text: &str) -> bool {
    text.chars().take(FILTER_MAX_CHARS + 1).count() <= FILTER_MAX_CHARS
}

/// Bounded visual text; the full value can remain the accessible name.
pub fn visual_text(text: &str) -> String {
    let mut end = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if offset + grapheme.len() > 256 {
            break;
        }
        end = offset + grapheme.len();
    }
    let mut shown = text[..end].replace(['\r', '\n', '\t'], " ");
    if end < text.len() {
        shown.push('…');
    }
    shown
}

#[derive(Debug, Default)]
pub struct Directory {
    /// Positions in the current Sidebar projection, not copied Session bodies.
    matches: Vec<usize>,
    selected: Option<String>,
    too_long: bool,
}

impl Directory {
    pub fn rebuild(&mut self, rows: &[SessionRow], query: &str) {
        self.too_long = !valid_filter(query);
        self.matches.clear();
        if !self.too_long {
            let query = query.to_lowercase();
            let words: Vec<_> = query.split_whitespace().collect();
            for (index, row) in rows.iter().enumerate() {
                if words.is_empty() {
                    self.matches.push(index);
                    continue;
                }
                let haystack = format!(
                    "{} {} {}",
                    row.id,
                    row.title,
                    row.preview.as_deref().unwrap_or("")
                )
                .to_lowercase();
                if words.iter().all(|word| haystack.contains(word)) {
                    self.matches.push(index);
                }
            }
        }
        if self.choice(rows).is_none() {
            self.selected = self.matches.first().map(|index| rows[*index].id.clone());
        }
    }

    pub fn matches(&self) -> &[usize] {
        &self.matches
    }
    pub fn too_long(&self) -> bool {
        self.too_long
    }

    pub fn choice(&self, rows: &[SessionRow]) -> Option<usize> {
        let id = self.selected.as_deref()?;
        self.matches
            .iter()
            .position(|index| rows.get(*index).is_some_and(|row| row.id == id))
    }

    pub fn chosen_id<'a>(&self, rows: &'a [SessionRow]) -> Option<&'a str> {
        let choice = self.choice(rows)?;
        Some(rows[*self.matches.get(choice)?].id.as_str())
    }

    pub fn contains(&self, rows: &[SessionRow], id: &str) -> bool {
        self.matches
            .iter()
            .any(|index| rows.get(*index).is_some_and(|row| row.id == id))
    }

    pub fn move_selection(&mut self, rows: &[SessionRow], delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let index = self
            .choice(rows)
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(self.matches.len() - 1);
        self.selected = Some(rows[self.matches[index]].id.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fixture, model::Sidebar};

    fn rows() -> Vec<SessionRow> {
        let sessions = fixture::sessions(200);
        let mut sidebar = Sidebar::default();
        sidebar.replace(&sessions, &fixture::running(&sessions), true);
        sidebar.rows().to_vec()
    }

    #[test]
    fn filtering_preserves_source_order_and_matches_all_words_in_title_preview_or_id() {
        let rows = rows();
        let mut d = Directory::default();
        d.rebuild(&rows, "INVOICE tool");
        assert!(!d.matches().is_empty());
        assert!(d.matches().windows(2).all(|pair| pair[0] < pair[1]));
        assert!(d
            .matches()
            .iter()
            .all(|index| rows[*index].title.contains("invoice")));
        d.rebuild(&rows, "fixture-session-7");
        assert!(d.contains(&rows, "fixture-session-7"));
        d.rebuild(&rows, "ข้อความอ้างอิง");
        assert!(!d.matches().is_empty());
    }

    #[test]
    fn selection_follows_id_across_reordering_and_filtering_but_does_not_activate() {
        let mut rows = rows();
        let mut d = Directory::default();
        d.rebuild(&rows, "");
        d.move_selection(&rows, 5);
        let chosen = d.chosen_id(&rows).unwrap().to_string();
        rows.reverse();
        d.rebuild(&rows, "");
        assert_eq!(d.chosen_id(&rows), Some(chosen.as_str()));
        d.rebuild(&rows, &chosen);
        assert_eq!(d.chosen_id(&rows), Some(chosen.as_str()));
        d.rebuild(&rows, "no such title 123456789");
        assert_eq!(d.chosen_id(&rows), None);
        d.move_selection(&rows, 1);
        assert_eq!(d.choice(&rows), None);
    }

    #[test]
    fn empty_and_invalid_queries_are_explicit_and_never_leave_a_stale_choice() {
        let rows = rows();
        let mut d = Directory::default();
        d.rebuild(&rows, "");
        assert_eq!(d.matches().len(), rows.len());
        d.rebuild(&rows, &"ก".repeat(FILTER_MAX_CHARS + 1));
        assert!(d.too_long());
        assert!(d.matches().is_empty());
        assert_eq!(d.chosen_id(&rows), None);
        d.rebuild(&[], "");
        assert!(!d.too_long());
        assert!(d.matches().is_empty());
        assert!(valid_filter(&"ก".repeat(FILTER_MAX_CHARS)));
    }

    #[test]
    fn visual_text_is_bounded_without_splitting_graphemes_or_laying_out_huge_names() {
        let text = "👩🏽‍💻ก้่ำ".repeat(1000);
        let shown = visual_text(&text);
        assert!(shown.len() <= 259);
        assert!(shown.ends_with('…'));
        let prefix = shown.strip_suffix('…').unwrap();
        assert!(text.starts_with(prefix));
        assert!(text
            .grapheme_indices(true)
            .any(|(offset, _)| offset == prefix.len()));
        assert_eq!(visual_text("a\r\nb\tc"), "a  b c");
    }
}
