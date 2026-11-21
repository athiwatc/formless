//! Literal, case-sensitive search of finished text already loaded in this
//! window. No transport, history loading, regex, GPUI or transcript copies.
//! Results are source positions; consumers must supply the current corpus
//! revision when validating one. Non-overlapping matches follow source order.

use crate::directory::visual_text;
use crate::timeline::Timeline;
use unicode_segmentation::GraphemeCursor;

pub const QUERY_MAX_CHARS: usize = 256;
pub const MAX_HITS: usize = 200;
pub const MAX_SCAN_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SCAN_BLOCKS: usize = 4096;
pub const SNIPPET_MAX_BYTES: usize = 265;
const SNIPPET_CONTEXT_BYTES: usize = 512;
const SNIPPET_PROCESS_BYTES: usize = 4096;
const NO_PREVIEW: &str = "Preview unavailable within the safe text bound; open the match.";

/// Only offer a boundary the cursor can prove from this bounded chunk.
/// Complex clusters/RI runs requiring more context get no preview instead
/// of extending an unrestricted scan or pretending a chunk edge is safe.
fn nearby_boundary(text: &str, offset: usize, forward: bool) -> Option<usize> {
    let mut low = offset.saturating_sub(SNIPPET_CONTEXT_BYTES);
    while !text.is_char_boundary(low) {
        low -= 1;
    }
    let mut high = offset.saturating_add(SNIPPET_CONTEXT_BYTES).min(text.len());
    while !text.is_char_boundary(high) {
        high += 1;
    }
    let chunk = &text[low..high];
    let mut cursor = GraphemeCursor::new(offset, text.len(), true);
    if cursor.is_boundary(chunk, low).ok()? {
        return Some(offset);
    }
    if forward {
        cursor.next_boundary(chunk, low).ok().flatten()
    } else {
        cursor.prev_boundary(chunk, low).ok().flatten()
    }
}

pub fn valid_query(text: &str) -> bool {
    text.chars().take(QUERY_MAX_CHARS + 1).count() <= QUERY_MAX_CHARS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub block_index: usize,
    pub block_id: u64,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    EmptyQuery,
    QueryTooLong,
    NoSource,
    Results,
}

#[derive(Debug, Default)]
pub struct Search {
    query: String,
    revision: u64,
    state: State,
    hits: Vec<Hit>,
    selected: usize,
    pub hit_limited: bool,
    pub scan_limited: bool,
    pub live_tail_omitted: bool,
    /// Source prefixes admitted to the matching scan, not a CPU/RAM budget.
    pub scanned_bytes: usize,
    pub scanned_blocks: usize,
}

impl Search {
    pub fn clear(&mut self, revision: u64) {
        self.query.clear();
        self.revision = revision;
        self.state = State::EmptyQuery;
        self.hits.clear();
        self.selected = 0;
        self.hit_limited = false;
        self.scan_limited = false;
        self.live_tail_omitted = false;
        self.scanned_bytes = 0;
        self.scanned_blocks = 0;
    }

    pub fn run(&mut self, timeline: &Timeline, query: &str, revision: u64, loaded: bool) {
        self.clear(revision);
        if !valid_query(query) {
            self.state = State::QueryTooLong;
            return;
        }
        self.query.push_str(query);
        if !loaded {
            self.state = State::NoSource;
            return;
        }
        if query.is_empty() {
            return;
        }
        self.state = State::Results;
        self.live_tail_omitted = timeline.is_streaming();
        for (block_index, block) in timeline.blocks().iter().enumerate() {
            if timeline.is_streaming() && block_index + 1 == timeline.len() {
                continue;
            }
            if self.scanned_blocks == MAX_SCAN_BLOCKS {
                self.scan_limited = true;
                break;
            }
            self.scanned_blocks += 1;
            let text = block.markdown();
            let mut end = text.len().min(MAX_SCAN_BYTES - self.scanned_bytes);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let prefix = &text[..end];
            let mut from = 0;
            while let Some(offset) = prefix[from..].find(query) {
                let start = from + offset;
                let found_end = start + query.len();
                if self.hits.len() == MAX_HITS {
                    self.hit_limited = true;
                    self.scanned_bytes += found_end;
                    return;
                }
                self.hits.push(Hit {
                    block_index,
                    block_id: block.id,
                    start,
                    end: found_end,
                });
                from = found_end;
            }
            self.scanned_bytes += end;
            if end < text.len() {
                self.scan_limited = true;
                break;
            }
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }
    pub fn choice(&self) -> Option<usize> {
        (!self.hits.is_empty()).then(|| self.selected.min(self.hits.len() - 1))
    }
    pub fn move_selection(&mut self, delta: isize) {
        if let Some(index) = self.choice() {
            self.selected = index.saturating_add_signed(delta).min(self.hits.len() - 1);
        }
    }

    /// Range, identity and exact literal text are rechecked. A same-ID,
    /// same-length replacement still needs a new caller-owned revision.
    pub fn valid_hit(&self, timeline: &Timeline, revision: u64, hit: Hit) -> bool {
        if revision != self.revision || self.state != State::Results || !self.hits.contains(&hit) {
            return false;
        }
        let Some(block) = timeline.blocks().get(hit.block_index) else {
            return false;
        };
        block.id == hit.block_id
            && !(timeline.is_streaming() && hit.block_index + 1 == timeline.len())
            && block.markdown().get(hit.start..hit.end) == Some(self.query.as_str())
    }

    /// Made only for visible result rows. Grapheme adjustment has bounded
    /// context and never walks a whole huge cluster. If safe boundaries
    /// cannot be proved in that chunk, show a named preview limitation.
    /// At most 4 KiB of preview text plus bounded boundary-check chunks
    /// are processed; at most 265 bytes are handed to font layout.
    pub fn snippet(timeline: &Timeline, hit: Hit) -> Option<String> {
        let block = timeline.blocks().get(hit.block_index)?;
        if block.id != hit.block_id {
            return None;
        }
        let text = block.markdown();
        text.get(hit.start..hit.end)?;
        if hit.end - hit.start > SNIPPET_PROCESS_BYTES {
            return Some(NO_PREVIEW.into());
        }
        let mut start = hit.start.saturating_sub(64);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = hit.end.saturating_add(96).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let (Some(start), Some(end)) = (
            nearby_boundary(text, start, false),
            nearby_boundary(text, end, true),
        ) else {
            return Some(NO_PREVIEW.into());
        };
        if end - start > SNIPPET_PROCESS_BYTES {
            return Some(NO_PREVIEW.into());
        }
        let range = start..end;
        let mut shown = visual_text(&text[range.clone()]);
        if range.start > 0 {
            shown.insert(0, '…');
        }
        if range.end < text.len() && !shown.ends_with('…') {
            shown.push('…');
        }
        Some(shown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::BlockKind, timeline::TimelineBlock};

    fn timeline(text: &str) -> Timeline {
        Timeline::new(vec![TimelineBlock::new(
            7,
            BlockKind::Assistant,
            text.into(),
        )])
    }

    #[test]
    fn literal_unicode_matches_keep_source_byte_ranges_and_case() {
        let t = timeline("a漢abc漢 NeedLE .* needle");
        let mut s = Search::default();
        s.run(&t, "漢", 1, true);
        assert_eq!(
            s.hits()
                .iter()
                .map(|h| (h.start, h.end))
                .collect::<Vec<_>>(),
            [(1, 4), (7, 10)]
        );
        assert!(s.hits().iter().all(|hit| s.valid_hit(&t, 1, *hit)));
        s.run(&t, "needle", 1, true);
        assert_eq!(s.hits().len(), 1);
        s.run(&t, ".*", 1, true);
        assert_eq!(s.hits().len(), 1); // not a regex
        s.run(&timeline("aaa"), "aa", 1, true);
        assert_eq!(s.hits().len(), 1); // non-overlapping
    }

    #[test]
    fn empty_over_limit_and_unloaded_states_never_claim_a_completed_search() {
        let t = timeline("needle");
        let mut s = Search::default();
        s.run(&t, "", 1, true);
        assert_eq!(s.state(), State::EmptyQuery);
        s.run(&t, &"ก".repeat(QUERY_MAX_CHARS + 1), 1, true);
        assert_eq!(s.state(), State::QueryTooLong);
        assert!(s.query().is_empty());
        s.run(&t, "needle", 2, false);
        assert_eq!(s.state(), State::NoSource);
        assert!(s.hits().is_empty());
        assert_eq!(s.scanned_bytes, 0);
    }

    #[test]
    fn result_limit_is_explicit_but_an_exactly_full_complete_search_is_not_truncated() {
        let mut s = Search::default();
        s.run(&timeline(&"x ".repeat(MAX_HITS)), "x", 1, true);
        assert_eq!(s.hits().len(), MAX_HITS);
        assert!(!s.hit_limited);
        s.run(&timeline(&"x ".repeat(MAX_HITS + 1)), "x", 1, true);
        assert_eq!(s.hits().len(), MAX_HITS);
        assert!(s.hit_limited);
    }

    #[test]
    fn byte_and_block_budgets_are_explicit_and_utf8_safe() {
        let t = timeline(&("x".repeat(MAX_SCAN_BYTES - 1) + "漢needle"));
        let mut s = Search::default();
        s.run(&t, "needle", 1, true);
        assert!(s.scan_limited);
        assert!(s.hits().is_empty());
        assert!(s.scanned_bytes <= MAX_SCAN_BYTES);
        let mut blocks: Vec<_> = (0..MAX_SCAN_BLOCKS)
            .map(|i| TimelineBlock::new(i as u64, BlockKind::User, String::new()))
            .collect();
        blocks.push(TimelineBlock::new(
            MAX_SCAN_BLOCKS as u64,
            BlockKind::User,
            "needle".into(),
        ));
        s.run(&Timeline::new(blocks), "needle", 2, true);
        assert!(s.scan_limited);
        assert!(s.hits().is_empty());
        assert_eq!(s.scanned_blocks, MAX_SCAN_BLOCKS);
    }

    #[test]
    fn live_tail_is_excluded_and_deltas_do_not_change_finished_hits() {
        let mut t = timeline("needle");
        t.begin_stream(BlockKind::Assistant);
        t.push_delta("needle");
        let mut s = Search::default();
        s.run(&t, "needle", 1, true);
        assert!(s.live_tail_omitted);
        assert_eq!(s.hits().len(), 1);
        let hit = s.hits()[0];
        t.push_delta(" more");
        assert!(s.valid_hit(&t, 1, hit));
        t.finish_stream();
        s.run(&t, "needle", 2, true);
        assert!(!s.live_tail_omitted);
        assert_eq!(s.hits().len(), 2);
    }

    #[test]
    fn revisions_identity_and_ranges_are_rechecked_before_activation() {
        let mut s = Search::default();
        let t = timeline("needle");
        s.run(&t, "needle", 1, true);
        let hit = s.hits()[0];
        assert!(!s.valid_hit(&t, 2, hit)); // even identical replacement text is another revision
        assert!(!s.valid_hit(&timeline("gone!!"), 1, hit));
        assert!(!s.valid_hit(&t, 1, Hit { block_id: 8, ..hit }));
        assert!(!s.valid_hit(
            &t,
            1,
            Hit {
                end: usize::MAX,
                ..hit
            }
        ));
        assert!(!s.valid_hit(
            &t,
            1,
            Hit {
                block_index: usize::MAX,
                ..hit
            }
        ));
    }

    #[test]
    fn snippets_are_bounded_grapheme_safe_and_do_not_modify_source() {
        let text = "a".repeat(90) + &"👩🏽‍💻ก้่ำ".repeat(1000);
        let t = timeline(&text);
        let mut s = Search::default();
        s.run(&t, "🏽", 1, true);
        let shown = Search::snippet(&t, s.hits()[0]).unwrap();
        assert!(shown.len() <= SNIPPET_MAX_BYTES);
        assert!(shown.contains("👩🏽‍💻"));
        assert_eq!(t.blocks()[0].markdown(), text);
        let giant = "e".to_string() + &"\u{301}".repeat(8192) + "needle";
        let t = timeline(&giant);
        s.run(&t, "needle", 2, true);
        assert_eq!(Search::snippet(&t, s.hits()[0]).unwrap(), NO_PREVIEW);
    }

    #[test]
    fn selection_is_clamped_and_rebuilds_cannot_keep_stale_choices() {
        let t = timeline("x x x");
        let mut s = Search::default();
        s.run(&t, "x", 1, true);
        s.move_selection(999);
        assert_eq!(s.choice(), Some(2));
        s.move_selection(-999);
        assert_eq!(s.choice(), Some(0));
        s.run(&t, "nothing", 2, true);
        assert_eq!(s.choice(), None);
        s.move_selection(1);
        assert_eq!(s.choice(), None);
    }
}
