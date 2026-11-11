//! Timeline blocks, bounded windows onto large blocks, and streamed tails.
//!
//! No GPUI types. The view lays out one bounded window of a block
//! ([`TimelineBlock::preview`]), never the whole block, so one enormous
//! output cannot make a frame expensive. The window can be moved: it starts
//! at the head of a finished block, follows the tail of a block that is
//! being streamed, and can be paged through the rest.

use std::ops::Range;
use std::sync::Arc;

use crate::memory::limits;
use crate::model::BlockKind;

/// How long changes may wait before the view is told, so a burst of deltas
/// costs one update. Roughly one frame at 60 Hz.
pub const FLUSH_INTERVAL_MS: u64 = 16;

/// The length of the prefix of `text` to lay out: at most `max_bytes` and
/// `max_lines`, ending just after a line break where there is one and always
/// on a character boundary. Looks at no more than `max_bytes` of the text.
pub fn preview_len(text: &str, max_bytes: usize, max_lines: usize) -> usize {
    let mut window = text.len().min(max_bytes);
    while !text.is_char_boundary(window) {
        window -= 1;
    }
    let head = &text[..window];
    if let Some((newline, _)) = head.match_indices('\n').nth(max_lines.saturating_sub(1)) {
        // More lines than allowed: stop after the last allowed one.
        return newline + 1;
    }
    if window == text.len() {
        return window;
    }
    // Cut by size: prefer the last whole line that fits.
    head.rfind('\n').map_or(window, |newline| newline + 1)
}

/// Where a window that ends at `end` starts: at most `max_bytes` and
/// `max_lines` back, at the start of a line where there is one and always on
/// a character boundary. Looks at no more than `max_bytes` of the text.
pub fn window_start(text: &str, end: usize, max_bytes: usize, max_lines: usize) -> usize {
    let mut floor = end.saturating_sub(max_bytes);
    while !text.is_char_boundary(floor) {
        floor += 1;
    }
    let tail = &text[floor..end];
    // A line break at the very end closes the last line; it does not start
    // another.
    let lines = tail.strip_suffix('\n').unwrap_or(tail);
    if let Some((newline, _)) = lines.rmatch_indices('\n').nth(max_lines.saturating_sub(1)) {
        // More lines than allowed: start after the break before the first
        // allowed one.
        return floor + newline + 1;
    }
    if floor == 0 {
        return 0;
    }
    // Cut by size: prefer the first whole line that fits.
    lines
        .find('\n')
        .map_or(floor, |newline| floor + newline + 1)
}

/// The fence a line opens or closes, by CommonMark's rule for fenced code:
/// up to three spaces, then at least three backticks or tildes.
fn fence_run(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let marker = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = rest.chars().take_while(|c| *c == marker).count();
    (run >= 3).then_some((marker, run))
}

/// Which fenced code block, if any, is open at each point of a text.
///
/// A window that starts inside a fenced block would otherwise be laid out as
/// prose. This remembers where fences open and close so the window can be
/// given the opening line it is missing. It reads each complete line once, as
/// the text grows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FenceIndex {
    /// (offset just after a fence line, the opening line now in force).
    events: Vec<(usize, Option<Arc<str>>)>,
    /// Lines before this offset have been read.
    scanned: usize,
    open: Option<(char, usize)>,
}

impl FenceIndex {
    /// Read the lines of `text` that are complete and not yet read.
    fn extend(&mut self, text: &str) {
        while let Some(length) = text[self.scanned..].find('\n') {
            let line = &text[self.scanned..self.scanned + length];
            let after = self.scanned + length + 1;
            match (self.open, fence_run(line)) {
                (None, Some(fence)) => {
                    self.open = Some(fence);
                    self.events.push((after, Some(Arc::from(line))));
                }
                (Some((marker, opened)), Some((closing, run)))
                    if closing == marker
                        && run >= opened
                        && line.trim_start_matches(' ')[run..].trim().is_empty() =>
                {
                    self.open = None;
                    self.events.push((after, None));
                }
                _ => {}
            }
            self.scanned = after;
        }
    }

    /// The opening line of the fenced block that contains `offset`.
    fn opener_at(&self, offset: usize) -> Option<&str> {
        let before = self.events.partition_point(|(after, _)| *after <= offset);
        before
            .checked_sub(1)
            .and_then(|index| self.events[index].1.as_deref())
    }
}

/// Which part of a block its row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// From the start of the block.
    Head,
    /// Up to the end of the block, moving with it as it grows.
    Tail,
    /// From this byte offset, which is the start of a line where the block
    /// has lines.
    At(usize),
}

/// One timeline row. `id` is stable for the life of the transcript so list
/// state, selection and caches can key on it instead of on a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineBlock {
    pub id: u64,
    pub kind: BlockKind,
    markdown: String,
    fences: FenceIndex,
    anchor: Anchor,
    window: Range<usize>,
    preview: Arc<str>,
}

impl TimelineBlock {
    pub fn new(id: u64, kind: BlockKind, markdown: String) -> Self {
        let mut block = Self {
            id,
            kind,
            markdown,
            fences: FenceIndex::default(),
            anchor: Anchor::Head,
            window: 0..0,
            preview: Arc::from(""),
        };
        block.fences.extend(&block.markdown);
        block.refresh();
        block
    }

    /// Recompute the window for the current anchor and the text to lay out.
    fn refresh(&mut self) {
        const BYTES: usize = limits::BLOCK_PREVIEW_MAX_BYTES;
        const LINES: usize = limits::BLOCK_PREVIEW_MAX_LINES;
        let text = &self.markdown;
        self.window = match self.anchor {
            Anchor::Head => 0..preview_len(text, BYTES, LINES),
            Anchor::Tail => window_start(text, text.len(), BYTES, LINES)..text.len(),
            Anchor::At(start) => start..start + preview_len(&text[start..], BYTES, LINES),
        };
        let shown = &text[self.window.clone()];
        // A window that starts inside a fenced code block is given that
        // block's opening line, so it is still laid out as code. This is for
        // layout only; `markdown` is what a copy must use.
        self.preview = match self.fences.opener_at(self.window.start) {
            Some(opener) => Arc::from(format!("{opener}\n{shown}")),
            None => Arc::from(shown),
        };
    }

    /// The whole block, exactly as received. Copying must use this.
    pub fn markdown(&self) -> &str {
        &self.markdown
    }

    /// What the row lays out: the window, preceded by the opening line of a
    /// fenced code block it starts inside. Cheap to clone.
    pub fn preview(&self) -> &Arc<str> {
        &self.preview
    }

    pub fn anchor(&self) -> Anchor {
        self.anchor
    }

    /// The part of the block the row shows, as byte offsets.
    pub fn window(&self) -> Range<usize> {
        self.window.clone()
    }

    /// Bytes of the block before the window: not laid out.
    pub fn hidden_before(&self) -> usize {
        self.window.start
    }

    /// Bytes of the block after the window: not laid out.
    pub fn hidden_after(&self) -> usize {
        self.markdown.len() - self.window.end
    }

    /// Whether the row shows less than the whole block.
    pub fn is_truncated(&self) -> bool {
        self.hidden_before() + self.hidden_after() > 0
    }

    pub fn total_bytes(&self) -> usize {
        self.markdown.len()
    }

    pub fn show_start(&mut self) {
        self.anchor = Anchor::Head;
        self.refresh();
    }

    /// Show the end of the block, and keep showing it as the block grows.
    pub fn show_end(&mut self) {
        self.anchor = Anchor::Tail;
        self.refresh();
    }

    /// Move the window back by one window. At the start this does nothing.
    pub fn show_earlier(&mut self) {
        let end = self.window.start;
        let start = window_start(
            &self.markdown,
            end,
            limits::BLOCK_PREVIEW_MAX_BYTES,
            limits::BLOCK_PREVIEW_MAX_LINES,
        );
        self.anchor = if start == 0 {
            Anchor::Head
        } else {
            Anchor::At(start)
        };
        self.refresh();
    }

    /// Move the window on by one window, starting exactly where this one
    /// ends. At the end this does nothing. The last window is not the same
    /// as [`Self::show_end`]: it starts where the page before it stopped,
    /// and it only grows with the block until it is full.
    pub fn show_later(&mut self) {
        if self.hidden_after() > 0 {
            self.anchor = Anchor::At(self.window.end);
            self.refresh();
        }
    }

    fn push(&mut self, text: &str) {
        let reached_end = self.hidden_after() == 0;
        self.markdown.push_str(text);
        self.fences.extend(&self.markdown);
        // A window on the tail moves with the text. Any other window only
        // changes while it still reaches the end of the block, that is,
        // until it is full; text added past a full window cannot change it.
        if self.anchor == Anchor::Tail || reached_end {
            self.refresh();
        }
    }
}

/// How to move a block's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMove {
    Start,
    Earlier,
    Later,
    End,
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
    /// stream already in progress is finished first. The new block's row
    /// follows its end, so the newest text is what is shown however long
    /// the block gets.
    pub fn begin_stream(&mut self, kind: BlockKind) -> u64 {
        let id = self.blocks.last().map_or(1, |block| block.id + 1);
        let mut block = TimelineBlock::new(id, kind, String::new());
        block.show_end();
        self.blocks.push(block);
        self.streaming = true;
        id
    }

    /// Move the window of the block with this id. Returns the block's
    /// position, or `None` if there is no such block.
    pub fn move_window(&mut self, block_id: u64, to: WindowMove) -> Option<usize> {
        let index = self.blocks.iter().position(|block| block.id == block_id)?;
        let block = &mut self.blocks[index];
        match to {
            WindowMove::Start => block.show_start(),
            WindowMove::Earlier => block.show_earlier(),
            WindowMove::Later => block.show_later(),
            WindowMove::End => block.show_end(),
        }
        Some(index)
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

    const BYTES: usize = limits::BLOCK_PREVIEW_MAX_BYTES;
    const LINES: usize = limits::BLOCK_PREVIEW_MAX_LINES;

    /// `count` numbered lines of about 60 bytes each.
    fn numbered_lines(count: usize) -> String {
        (1..=count)
            .map(|line| format!("line {line:06} of output, padded out to a realistic width..\n"))
            .collect()
    }

    #[test]
    fn a_small_block_is_laid_out_whole() {
        let b = block("short\ntext");
        assert!(!b.is_truncated());
        assert_eq!(&**b.preview(), "short\ntext");
        assert_eq!(b.markdown(), "short\ntext");
        assert_eq!((b.hidden_before(), b.hidden_after()), (0, 0));
    }

    #[test]
    fn a_long_block_is_cut_after_a_line_break_within_the_byte_limit() {
        let line = "x".repeat(99) + "\n";
        let text = line.repeat(2_000);
        let end = preview_len(&text, 1_050, usize::MAX);
        // Ten whole 100-byte lines fit in 1,050 bytes.
        assert_eq!(end, 1_000);
        assert!(text[..end].ends_with('\n'));

        // No line break to cut at: fall back to the byte limit.
        let unbroken = "y".repeat(5_000);
        assert_eq!(preview_len(&unbroken, 1_000, usize::MAX), 1_000);
    }

    #[test]
    fn a_block_with_many_short_lines_is_cut_by_line_count() {
        let text = "a\n".repeat(10_000);
        let end = preview_len(&text, usize::MAX, 400);
        assert_eq!(text[..end].lines().count(), 400);
        let start = window_start(&text, text.len(), usize::MAX, 400);
        assert_eq!(text[start..].lines().count(), 400);
        // With no final line break the last line still counts as one.
        let open = "a\n".repeat(10_000) + "b";
        let start = window_start(&open, open.len(), usize::MAX, 400);
        assert_eq!(open[start..].lines().count(), 400);
        assert!(open[start..].ends_with('b'));
    }

    #[test]
    fn a_cut_never_splits_a_character_at_either_end() {
        // No line breaks at all: Thai letters, three bytes each, and a
        // multi-code-point emoji.
        for text in ["ก".repeat(1_000), "👩🏽‍💻".repeat(200), "aก👩🏽‍💻".repeat(150)]
        {
            for max in [0, 1, 2, 3, 4, 5, 7, 100, 2_999] {
                let end = preview_len(&text, max, usize::MAX);
                assert!(end <= max);
                assert!(text.is_char_boundary(end));
                let start = window_start(&text, text.len(), max, usize::MAX);
                assert!(text.len() - start <= max);
                assert!(text.is_char_boundary(start));
            }
        }
    }

    #[test]
    fn a_five_megabyte_block_has_a_bounded_window_and_keeps_its_full_text() {
        let line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
        let text = line.repeat(5 * 1024 * 1024 / line.len());
        let mut b = block(&text);
        assert!(b.is_truncated());
        assert_eq!(b.anchor(), Anchor::Head);
        assert!(b.preview().len() <= BYTES);
        assert!(b.preview().lines().count() <= LINES);
        assert!(b.total_bytes() >= 5 * 1024 * 1024 - line.len());
        assert_eq!(b.markdown(), text);
        assert!(text.starts_with(&**b.preview()));
        assert_eq!(b.hidden_before(), 0);
        assert_eq!(b.hidden_after(), text.len() - b.preview().len());

        // The end is as bounded as the start.
        b.show_end();
        assert!(b.preview().len() <= BYTES);
        assert!(b.preview().lines().count() <= LINES);
        assert!(text.ends_with(&**b.preview()));
        assert_eq!(b.hidden_after(), 0);
        assert_eq!(b.markdown(), text);
    }

    #[test]
    fn paging_forward_shows_every_byte_exactly_once() {
        for text in [
            numbered_lines(3_000),
            // One enormous line: cut by size, on character boundaries.
            "ก".repeat(40_000),
            // Lines of very different lengths, with multi-byte text.
            (0..2_500)
                .map(|i| format!("{}\n", "คำ👩🏽‍💻".repeat(i % 7)))
                .collect::<String>(),
        ] {
            let mut b = block(&text);
            let mut seen = String::new();
            let mut windows = 0;
            loop {
                let window = b.window();
                assert_eq!(window.start, seen.len(), "no gap and no overlap");
                assert!(window.len() <= BYTES);
                assert!(text[window.clone()].lines().count() <= LINES);
                seen.push_str(&text[window]);
                windows += 1;
                if b.hidden_after() == 0 {
                    break;
                }
                b.show_later();
            }
            assert!(windows > 1);
            assert_eq!(seen, text);
        }
    }

    #[test]
    fn paging_backward_reaches_the_start_and_leaves_no_byte_unshown() {
        let text = numbered_lines(3_000);
        let mut b = block(&text);
        b.show_end();
        let mut covered_from = b.window().start;
        while b.hidden_before() > 0 {
            b.show_earlier();
            let window = b.window();
            assert!(window.len() <= BYTES);
            // Each step back reaches at least to where the last one began.
            assert!(window.end >= covered_from);
            assert!(window.start < covered_from);
            covered_from = window.start;
        }
        assert_eq!(b.anchor(), Anchor::Head);
        assert_eq!(covered_from, 0);
        // Stepping back at the start changes nothing.
        let at_start = b.window();
        b.show_earlier();
        assert_eq!(b.window(), at_start);
    }

    #[test]
    fn a_window_that_starts_inside_fenced_code_is_given_the_opening_line() {
        let code = numbered_lines(1_500);
        let text = format!("Intro paragraph.\n\n```rust\n{code}```\n\nAfterword.\n");
        let mut b = block(&text);
        // The first window starts before the fence and needs nothing added.
        assert!(b.preview().starts_with("Intro paragraph."));

        b.show_later();
        let window = b.window();
        assert!(window.start > text.find("```rust").unwrap());
        // Laid out: the opening line, language and all, then the window.
        assert_eq!(&**b.preview(), format!("```rust\n{}", &text[window]));
        // The block itself is untouched.
        assert_eq!(b.markdown(), text);

        // The tail starts inside the fence too and ends after it closes.
        b.show_end();
        assert!(b.preview().starts_with("```rust\n"));
        assert!(b.preview().ends_with("```\n\nAfterword.\n"));
    }

    #[test]
    fn fences_are_tracked_by_commonmark_rules() {
        let mut index = FenceIndex::default();
        let text = "before\n~~~~ text\n```\n~~~\n~~~~\nafter\n    ```\ndone\n";
        index.extend(text);
        let at = |needle: &str| text.find(needle).unwrap();
        assert_eq!(index.opener_at(at("before")), None);
        // Inside the tilde fence: backticks do not close it, and neither does
        // a shorter run of tildes.
        assert_eq!(index.opener_at(at("```")), Some("~~~~ text"));
        assert_eq!(index.opener_at(at("~~~\n")), Some("~~~~ text"));
        assert_eq!(index.opener_at(at("~~~~\nafter")), Some("~~~~ text"));
        assert_eq!(index.opener_at(at("after")), None);
        // Four spaces of indent is not a fence.
        assert_eq!(index.opener_at(at("done")), None);
    }

    #[test]
    fn a_fence_line_that_arrives_in_pieces_is_still_seen() {
        let mut timeline = Timeline::default();
        timeline.begin_stream(BlockKind::Assistant);
        for piece in ["Here is the code:\n\n`", "``", "py", "thon\n"] {
            timeline.push_delta(piece);
        }
        let body = numbered_lines(1_200);
        for line in body.split_inclusive('\n') {
            timeline.push_delta(line);
        }
        let tail = &timeline.blocks()[0];
        assert!(tail.hidden_before() > 0);
        assert!(tail.preview().starts_with("```python\n"));
        assert!(tail
            .preview()
            .ends_with("line 001200 of output, padded out to a realistic width..\n"));
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
    fn a_streamed_block_keeps_showing_its_newest_text_past_the_limit() {
        let mut timeline = Timeline::default();
        timeline.begin_stream(BlockKind::Assistant);
        let mut pushed = String::new();
        let mut index = 0;
        while pushed.len() < BYTES * 3 {
            index += 1;
            // Multi-byte text, and deltas that end mid-line.
            let ending = if index % 9 == 0 { "\n\n" } else { "" };
            let delta = format!("คำที่ {index} 👩🏽‍💻 {ending}");
            timeline.push_delta(&delta);
            pushed.push_str(&delta);

            let tail = &timeline.blocks()[0];
            assert!(tail.preview().len() <= BYTES);
            // Whatever was streamed last is on show.
            assert!(tail.preview().ends_with(&delta), "delta {index}");
            assert_eq!(tail.hidden_after(), 0);
        }
        let tail = &timeline.blocks()[0];
        assert_eq!(tail.anchor(), Anchor::Tail);
        assert!(tail.is_truncated());
        assert!(tail.hidden_before() > BYTES);
        assert_eq!(tail.markdown(), pushed);
        assert!(pushed.is_char_boundary(tail.window().start));
    }

    #[test]
    fn a_window_moved_off_the_tail_stays_put_while_the_stream_goes_on() {
        let mut timeline = Timeline::default();
        let id = timeline.begin_stream(BlockKind::Assistant);
        let line = "streamed line of text\n";
        for _ in 0..(BYTES * 2 / line.len()) {
            timeline.push_delta(line);
        }
        // The reader goes back to the start.
        assert_eq!(timeline.move_window(id, WindowMove::Start), Some(0));
        let pinned = timeline.blocks()[0].preview().clone();
        let hidden_after = timeline.blocks()[0].hidden_after();
        for _ in 0..500 {
            timeline.push_delta(line);
        }
        let tail = &timeline.blocks()[0];
        // Same allocation: nothing was recomputed, nothing moved.
        assert!(Arc::ptr_eq(&pinned, tail.preview()));
        assert_eq!(tail.anchor(), Anchor::Head);
        // What arrived meanwhile is counted, so the row can say so.
        assert_eq!(tail.hidden_after(), hidden_after + 500 * line.len());

        // Going back to the end shows the newest text and follows it again.
        timeline.move_window(id, WindowMove::End);
        timeline.push_delta("the very latest\n");
        let tail = &timeline.blocks()[0];
        assert_eq!(tail.anchor(), Anchor::Tail);
        assert!(tail.preview().ends_with("the very latest\n"));
        assert_eq!(tail.hidden_after(), 0);

        // An unknown block is not an error and moves nothing.
        assert_eq!(timeline.move_window(id + 1, WindowMove::Start), None);
    }

    #[test]
    fn a_short_head_window_grows_with_the_block_until_it_is_full() {
        let mut timeline = Timeline::default();
        let id = timeline.begin_stream(BlockKind::Assistant);
        timeline.move_window(id, WindowMove::Start);
        timeline.push_delta("first\n");
        timeline.push_delta("second\n");
        assert_eq!(&**timeline.blocks()[0].preview(), "first\nsecond\n");
        assert!(!timeline.blocks()[0].is_truncated());
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
