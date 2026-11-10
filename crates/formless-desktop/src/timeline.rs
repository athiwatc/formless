//! Timeline blocks, bounded previews of large blocks, and streamed tails.
//!
//! No GPUI types. The view lays out [`TimelineBlock::preview`], never the
//! whole block, so one enormous output cannot make a frame expensive.

use std::sync::Arc;

use crate::memory::limits;
use crate::model::BlockKind;

/// How long changes may wait before the view is told, so a burst of deltas
/// costs one update. Roughly one frame at 60 Hz.
pub const FLUSH_INTERVAL_MS: u64 = 16;

/// The length of the prefix of `text` to lay out: at most `max_bytes` and
/// `max_lines`, ending on a line break where there is one and always on a
/// character boundary. Looks at no more than `max_bytes` of the text.
pub fn preview_len(text: &str, max_bytes: usize, max_lines: usize) -> usize {
    let mut window = text.len().min(max_bytes);
    while !text.is_char_boundary(window) {
        window -= 1;
    }
    let head = &text[..window];
    if let Some((end, _)) = head.match_indices('\n').nth(max_lines.saturating_sub(1)) {
        // More lines than allowed: stop after the last allowed one.
        return end;
    }
    if window == text.len() {
        return window;
    }
    // Cut by size: prefer the last whole line that fits.
    head.rfind('\n').unwrap_or(window)
}

/// One timeline row. `id` is stable for the life of the transcript so list
/// state, selection and caches can key on it instead of on a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineBlock {
    pub id: u64,
    pub kind: BlockKind,
    markdown: String,
    preview: Arc<str>,
}

impl TimelineBlock {
    pub fn new(id: u64, kind: BlockKind, markdown: String) -> Self {
        let preview = Self::preview_of(&markdown);
        Self {
            id,
            kind,
            markdown,
            preview,
        }
    }

    fn preview_of(markdown: &str) -> Arc<str> {
        let end = preview_len(
            markdown,
            limits::BLOCK_PREVIEW_MAX_BYTES,
            limits::BLOCK_PREVIEW_MAX_LINES,
        );
        Arc::from(&markdown[..end])
    }

    /// The whole block, exactly as received. Copying must use this.
    pub fn markdown(&self) -> &str {
        &self.markdown
    }

    /// What the row lays out. Cheap to clone.
    pub fn preview(&self) -> &Arc<str> {
        &self.preview
    }

    /// Whether the row shows less than the whole block.
    pub fn is_truncated(&self) -> bool {
        self.preview.len() < self.markdown.len()
    }

    pub fn total_bytes(&self) -> usize {
        self.markdown.len()
    }

    fn push(&mut self, text: &str) {
        // Once the preview is a strict prefix it is already at its limit, and
        // text added past it cannot change it.
        let was_truncated = self.is_truncated();
        self.markdown.push_str(text);
        if !was_truncated {
            self.preview = Self::preview_of(&self.markdown);
        }
    }
}

/// The open Session's blocks, with at most one block still being streamed.
#[derive(Debug, Default)]
pub struct Timeline {
    blocks: Vec<TimelineBlock>,
    streaming: bool,
}

impl Timeline {
    pub fn new(blocks: Vec<TimelineBlock>) -> Self {
        Self {
            blocks,
            streaming: false,
        }
    }

    pub fn blocks(&self) -> &[TimelineBlock] {
        &self.blocks
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Add an empty block at the tail to stream into and return its id. A
    /// stream already in progress is finished first.
    pub fn begin_stream(&mut self, kind: BlockKind) -> u64 {
        let id = self.blocks.last().map_or(1, |block| block.id + 1);
        self.blocks
            .push(TimelineBlock::new(id, kind, String::new()));
        self.streaming = true;
        id
    }

    /// Append to the streamed block. Returns false, changing nothing, when
    /// no stream is in progress. Completed blocks are never touched.
    pub fn push_delta(&mut self, text: &str) -> bool {
        match self.blocks.last_mut() {
            Some(tail) if self.streaming => {
                tail.push(text);
                true
            }
            _ => false,
        }
    }

    pub fn finish_stream(&mut self) {
        self.streaming = false;
    }
}

/// Collapses any number of change notices into one pending flush.
#[derive(Debug, Default)]
pub struct Coalescer {
    pending: bool,
    requests: u64,
    flushes: u64,
}

impl Coalescer {
    /// Note a change. True means the caller must schedule a flush; false
    /// means one is already on its way and will cover this change too.
    pub fn request(&mut self) -> bool {
        self.requests += 1;
        !std::mem::replace(&mut self.pending, true)
    }

    /// The scheduled flush is running. Returns whether there is anything to
    /// flush, and clears the pending state.
    pub fn take(&mut self) -> bool {
        let pending = std::mem::replace(&mut self.pending, false);
        if pending {
            self.flushes += 1;
        }
        pending
    }

    pub fn requests(&self) -> u64 {
        self.requests
    }

    pub fn flushes(&self) -> u64 {
        self.flushes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(markdown: &str) -> TimelineBlock {
        TimelineBlock::new(1, BlockKind::Assistant, markdown.to_string())
    }

    #[test]
    fn a_small_block_is_laid_out_whole() {
        let b = block("short\ntext");
        assert!(!b.is_truncated());
        assert_eq!(&**b.preview(), "short\ntext");
        assert_eq!(b.markdown(), "short\ntext");
    }

    #[test]
    fn a_long_block_is_cut_at_a_line_break_within_the_byte_limit() {
        let line = "x".repeat(99) + "\n";
        let text = line.repeat(2_000);
        let end = preview_len(&text, 1_050, usize::MAX);
        // Ten whole 100-byte lines fit in 1,050 bytes; the cut is before the
        // tenth line's newline.
        assert_eq!(end, 999);
        assert!(text[..end].ends_with('x'));

        // No line break to cut at: fall back to the byte limit.
        let unbroken = "y".repeat(5_000);
        assert_eq!(preview_len(&unbroken, 1_000, usize::MAX), 1_000);
    }

    #[test]
    fn a_block_with_many_short_lines_is_cut_by_line_count() {
        let text = "a\n".repeat(10_000);
        let end = preview_len(&text, usize::MAX, 400);
        assert_eq!(text[..end].lines().count(), 400);
    }

    #[test]
    fn the_cut_never_splits_a_character() {
        let text = "ก".repeat(1_000);
        for max in [0, 1, 2, 3, 4, 100, 2_999] {
            let end = preview_len(&text, max, usize::MAX);
            assert!(end <= max);
            assert!(text.is_char_boundary(end));
        }
    }

    #[test]
    fn a_five_megabyte_block_has_a_bounded_preview_and_keeps_its_full_text() {
        let line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
        let text = line.repeat(5 * 1024 * 1024 / line.len());
        let b = block(&text);
        assert!(b.is_truncated());
        assert!(b.preview().len() <= limits::BLOCK_PREVIEW_MAX_BYTES);
        assert!(b.preview().lines().count() <= limits::BLOCK_PREVIEW_MAX_LINES);
        assert!(b.total_bytes() >= 5 * 1024 * 1024 - line.len());
        assert_eq!(b.markdown(), text);
        assert!(text.starts_with(&**b.preview()));
    }

    #[test]
    fn streaming_appends_to_the_tail_only_and_keeps_ids_stable() {
        let mut timeline = Timeline::new(vec![
            TimelineBlock::new(7, BlockKind::User, "question".to_string()),
            TimelineBlock::new(8, BlockKind::Assistant, "answer".to_string()),
        ]);
        assert!(!timeline.push_delta("ignored"), "no stream yet");
        assert_eq!(timeline.blocks()[1].markdown(), "answer");

        let id = timeline.begin_stream(BlockKind::Assistant);
        assert_eq!(id, 9);
        assert!(timeline.push_delta("สวัส"));
        assert!(timeline.push_delta("ดี"));
        let ids: Vec<u64> = timeline.blocks().iter().map(|b| b.id).collect();
        assert_eq!(ids, [7, 8, 9]);
        assert_eq!(timeline.blocks()[0].markdown(), "question");
        assert_eq!(timeline.blocks()[1].markdown(), "answer");
        assert_eq!(timeline.blocks()[2].markdown(), "สวัสดี");
        assert_eq!(&**timeline.blocks()[2].preview(), "สวัสดี");

        timeline.finish_stream();
        assert!(!timeline.push_delta("late"));
        assert_eq!(timeline.blocks()[2].markdown(), "สวัสดี");
    }

    #[test]
    fn a_streamed_block_stops_growing_its_preview_once_it_is_at_the_limit() {
        let mut timeline = Timeline::default();
        timeline.begin_stream(BlockKind::Assistant);
        let chunk = "streamed line of text\n";
        let mut pushed = 0;
        while pushed < limits::BLOCK_PREVIEW_MAX_BYTES * 3 {
            timeline.push_delta(chunk);
            pushed += chunk.len();
        }
        let tail = &timeline.blocks()[0];
        assert!(tail.is_truncated());
        assert!(tail.preview().len() <= limits::BLOCK_PREVIEW_MAX_BYTES);
        assert_eq!(tail.total_bytes(), pushed);
        let preview_before = tail.preview().clone();
        timeline.push_delta(chunk);
        // Same allocation: nothing was recomputed.
        assert!(Arc::ptr_eq(&preview_before, timeline.blocks()[0].preview()));
    }

    #[test]
    fn a_burst_of_requests_schedules_one_flush() {
        let mut coalescer = Coalescer::default();
        assert!(coalescer.request(), "first change schedules");
        for _ in 0..999 {
            assert!(!coalescer.request(), "later changes ride along");
        }
        assert!(coalescer.take());
        assert!(!coalescer.take(), "nothing left");
        assert_eq!((coalescer.requests(), coalescer.flushes()), (1_000, 1));
        assert!(coalescer.request(), "the next change schedules again");
    }
}
