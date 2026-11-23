//! Timeline blocks, bounded windows onto large blocks, and streamed tails.
//!
//! No GPUI types. The view lays out one bounded window of a block
//! ([`TimelineBlock::preview`]), never the whole block, so one enormous
//! output cannot make a frame expensive. The window can be moved: it starts
//! at the head of a finished block, follows the tail of a block that is
//! being streamed, and can be paged through the rest.
//!
//! Streamed text is added to the block at once, but the window is rebuilt
//! only when the view asks ([`Timeline::settle`]): once for each time the
//! row is shown, however many deltas arrived since the last time.

use std::cell::OnceCell;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Plain texts made for assistive technology in this process, the time they
/// took and the markdown they were made from, for measuring that cost.
static ACCESSIBLE_BUILDS: AtomicU64 = AtomicU64::new(0);
static ACCESSIBLE_NANOS: AtomicU64 = AtomicU64::new(0);
static ACCESSIBLE_BYTES: AtomicU64 = AtomicU64::new(0);

/// How many plain texts [`TimelineBlock::accessible_text`] has made in this
/// process, how many nanoseconds that took in all, and how many bytes of
/// markdown they were made from.
pub fn accessible_text_stats() -> (u64, u64, u64) {
    (
        ACCESSIBLE_BUILDS.load(Ordering::Relaxed),
        ACCESSIBLE_NANOS.load(Ordering::Relaxed),
        ACCESSIBLE_BYTES.load(Ordering::Relaxed),
    )
}

use crate::memory::limits;
use crate::model::{BlockKind, GeneratedMessage};

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

/// The most fence markers the opening line given to layout repeats.
const FENCE_RUN_MAX: usize = 32;

/// The longest language name the opening line given to layout carries, and
/// how far past the markers one is looked for.
const FENCE_INFO_MAX: usize = 64;

/// The longest text [`TimelineBlock::preview`] puts before its window: an
/// indent of up to three spaces, the markers, the language name and a line
/// break.
const FENCE_OPENER_MAX_BYTES: usize = 3 + FENCE_RUN_MAX + FENCE_INFO_MAX + 1;

/// The longest text a row lays out, whatever the block holds.
pub const PREVIEW_MAX_BYTES: usize = limits::BLOCK_PREVIEW_MAX_BYTES + FENCE_OPENER_MAX_BYTES;

/// The longest plain text a row keeps for assistive technology. Plain text
/// drops markup, so it is no longer than its window, except that an
/// ordered list's label can be longer than the marker written in the
/// source (an item written `1.` in a list counting from 999999999 is
/// labelled `1000000000.`): at most 16 bytes more for each line of the
/// window.
pub const ACCESSIBLE_MAX_BYTES: usize = PREVIEW_MAX_BYTES + 16 * limits::BLOCK_PREVIEW_MAX_LINES;

/// The text a person reads in rendered markdown, for assistive technology:
/// the words and code, without the syntax that marks them up (fences,
/// emphasis marks, quote markers, link targets). Blocks are on lines of
/// their own. Raw HTML is left out, as the view does not render it.
///
/// What the kit's markdown view draws that carries meaning is kept, as it
/// draws it: an ordered list's numbers (letters in nested lists, as the
/// view labels them), a task item's box as ☑ or ☐, an image's alt text,
/// including an image given by reference. Bullets are left out. Code is
/// kept exactly.
///
/// Markdown that does not parse is returned as it is.
pub fn plain_text(source: &str) -> String {
    use markdown::mdast::{List, Node};

    struct Writer {
        out: String,
        /// Lists the current node is inside, outermost first.
        depth: usize,
        /// A list item's marker has just been written: the item's first
        /// block goes on the same line.
        after_marker: bool,
    }

    impl Writer {
        /// A block starts on a line of its own.
        fn block(&mut self, inner: impl FnOnce(&mut Self)) {
            if !std::mem::take(&mut self.after_marker)
                && !self.out.is_empty()
                && !self.out.ends_with('\n')
            {
                self.out.push('\n');
            }
            inner(self);
        }

        fn children(&mut self, node: &Node) {
            for child in node.children().into_iter().flatten() {
                self.walk(child);
            }
        }

        fn list(&mut self, list: &List) {
            // The view draws no number or bullet in a list that has task
            // items, only their boxes.
            let todo = list
                .children
                .iter()
                .any(|item| matches!(item, Node::ListItem(item) if item.checked.is_some()));
            for (index, child) in list.children.iter().enumerate() {
                let Node::ListItem(item) = child else {
                    self.walk(child);
                    continue;
                };
                self.block(|this| {
                    if list.ordered && !todo {
                        this.out
                            .push_str(&ordered_label(list.start, index, this.depth));
                    }
                    match item.checked {
                        Some(true) => this.out.push_str("☑ "),
                        Some(false) => this.out.push_str("☐ "),
                        None => {}
                    }
                    this.after_marker = !this.out.ends_with('\n') && !this.out.is_empty();
                    this.depth += 1;
                    this.children(child);
                    this.depth -= 1;
                    this.after_marker = false;
                });
            }
        }

        fn walk(&mut self, node: &Node) {
            match node {
                Node::Text(text) => self.out.push_str(&text.value),
                Node::InlineCode(code) => self.out.push_str(&code.value),
                Node::InlineMath(math) => self.out.push_str(&math.value),
                Node::Break(_) => self.out.push('\n'),
                Node::Image(image) => self.out.push_str(&image.alt),
                Node::ImageReference(image) => self.out.push_str(&image.alt),
                Node::Html(_) | Node::Definition(_) | Node::ThematicBreak(_) => {}
                Node::Code(code) => self.block(|this| this.out.push_str(&code.value)),
                Node::Math(math) => self.block(|this| this.out.push_str(&math.value)),
                Node::List(list) => self.list(list),
                Node::Paragraph(_)
                | Node::Heading(_)
                | Node::ListItem(_)
                | Node::TableRow(_)
                | Node::Blockquote(_)
                | Node::Table(_) => self.block(|this| this.children(node)),
                Node::TableCell(_) => {
                    if !self.out.ends_with('\n') && !self.out.is_empty() {
                        self.out.push('\t');
                    }
                    self.children(node);
                }
                _ => self.children(node),
            }
        }
    }

    match markdown::to_mdast(source, &markdown::ParseOptions::gfm()) {
        Ok(root) => {
            let mut writer = Writer {
                out: String::with_capacity(source.len()),
                depth: 0,
                after_marker: false,
            };
            writer.walk(&root);
            writer.out
        }
        Err(_) => source.to_string(),
    }
}

/// An ordered list item's label as the kit's markdown view draws it: the
/// number at the top level, and letters in nested lists (A, B, ... then a,
/// b, ...), except in a list that starts at 0.
fn ordered_label(start: Option<u32>, index: usize, depth: usize) -> String {
    let ordinal = start
        .unwrap_or(1)
        .saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
    if depth == 0 || start == Some(0) {
        return format!("{ordinal}. ");
    }
    let first = if depth == 1 { b'A' } else { b'a' };
    let mut remaining = ordinal;
    let mut letters = Vec::new();
    while remaining > 0 {
        remaining -= 1;
        letters.push(char::from(first + (remaining % 26) as u8));
        remaining /= 26;
    }
    letters.reverse();
    format!("{}. ", letters.into_iter().collect::<String>())
}

/// The fence a line opens or closes, by CommonMark's rule for fenced code:
/// up to three spaces, then at least three backticks or tildes. Returns the
/// marker, the indent and the number of markers.
fn fence_run(line: &str) -> Option<(u8, usize, usize)> {
    let bytes = line.as_bytes();
    let indent = bytes.iter().take(4).take_while(|b| **b == b' ').count();
    if indent > 3 {
        return None;
    }
    let marker = *bytes.get(indent).filter(|b| matches!(b, b'`' | b'~'))?;
    let run = bytes[indent..].iter().take_while(|b| **b == marker).count();
    (run >= 3).then_some((marker, indent, run))
}

/// The language name at the start of a fence's info string, if it ends
/// within the first [`FENCE_INFO_MAX`] bytes of that string. One that goes
/// on past them is left out: half a name is not a language. Looks at no
/// more than those bytes and the character after them.
fn info_word(info: &str) -> &str {
    let mut cut = info.len().min(FENCE_INFO_MAX);
    while !info.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = info[..cut].trim_start();
    let word = head.find(char::is_whitespace).unwrap_or(head.len());
    let ends_here = word < head.len() || info[cut..].chars().next().is_none_or(char::is_whitespace);
    if ends_here {
        &head[..word]
    } else {
        ""
    }
}

/// One fenced code block, as positions in the text. Nothing is copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fenced {
    /// Offset of the first marker of the opening line.
    markers: usize,
    /// How many markers open it. A closing line needs at least as many.
    run: usize,
    /// Offset just after the opening line: where the code starts.
    body: usize,
    /// Offset just after the closing line, or [`Fenced::OPEN`].
    end: usize,
}

impl Fenced {
    const OPEN: usize = usize::MAX;

    /// An opening line for layout that stands for the real one. It is at
    /// most [`FENCE_OPENER_MAX_BYTES`] long however long the real one is:
    /// the same indent and marker, at most [`FENCE_RUN_MAX`] markers, and
    /// the language name if it is short enough to be one.
    ///
    /// The one way this differs from the real line in what it lays out: a
    /// block opened with more than [`FENCE_RUN_MAX`] markers is closed, in
    /// the row, by a line of that many, where the real one needs more.
    fn layout_opener(&self, text: &str) -> String {
        let bytes = text.as_bytes();
        let marker = char::from(bytes[self.markers]);
        let indent = bytes[..self.markers]
            .iter()
            .rev()
            .take(3)
            .take_while(|b| **b == b' ')
            .count();
        let info = info_word(&text[self.markers + self.run..self.body - 1]);
        let mut opener = String::with_capacity(FENCE_OPENER_MAX_BYTES);
        opener.extend(std::iter::repeat_n(' ', indent));
        opener.extend(std::iter::repeat_n(marker, self.run.min(FENCE_RUN_MAX)));
        opener.push_str(info);
        opener.push('\n');
        opener
    }
}

/// Which fenced code block, if any, is open at each point of a text.
///
/// A window that starts inside a fenced block would otherwise be laid out as
/// prose. This remembers where fenced blocks start and end, so the window
/// can be given an opening line. It holds positions only, 32 bytes for each
/// fenced block, and looks for line breaks in each byte of the text once, as
/// the text grows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FenceIndex {
    blocks: Vec<Fenced>,
    /// Where the line still being received starts.
    line: usize,
    /// No line break has been found between `line` and here.
    searched: usize,
    /// Bytes looked at for line breaks, to show each is looked at once.
    #[cfg(test)]
    work: usize,
}

impl FenceIndex {
    /// Read the lines of `text` that are complete and not yet read. `text`
    /// is the text given last time with more added at the end.
    fn extend(&mut self, text: &str) {
        loop {
            let unsearched = &text[self.searched..];
            let found = unsearched.find('\n');
            #[cfg(test)]
            {
                self.work += found.map_or(unsearched.len(), |at| at + 1);
            }
            let Some(length) = found else {
                // A line can arrive in any number of pieces. The next piece
                // is searched from here, not from the start of the line.
                self.searched = text.len();
                return;
            };
            let end = self.searched + length;
            self.read_line(text, self.line..end);
            self.line = end + 1;
            self.searched = end + 1;
        }
    }

    fn read_line(&mut self, text: &str, line: Range<usize>) {
        let Some((marker, indent, run)) = fence_run(&text[line.clone()]) else {
            return;
        };
        let after_markers = &text[line.start + indent + run..line.end];
        match self.blocks.last_mut().filter(|b| b.end == Fenced::OPEN) {
            // A line of backticks with another backtick after them is inline
            // code, not an opening line.
            None if marker == b'`' && after_markers.contains('`') => {}
            None => self.blocks.push(Fenced {
                markers: line.start + indent,
                run,
                body: line.end + 1,
                end: Fenced::OPEN,
            }),
            Some(open) => {
                let closes = text.as_bytes()[open.markers] == marker
                    && run >= open.run
                    && after_markers.trim().is_empty();
                if closes {
                    open.end = line.end + 1;
                }
            }
        }
    }

    /// The fenced block whose code contains `offset`.
    fn fenced_at(&self, offset: usize) -> Option<&Fenced> {
        let before = self.blocks.partition_point(|b| b.body <= offset);
        let fenced = &self.blocks[before.checked_sub(1)?];
        (offset < fenced.end).then_some(fenced)
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
    generated: Option<GeneratedMessage>,
    attachments: Vec<crate::transcript_attachments::Attachment>,
    markdown: String,
    fences: FenceIndex,
    anchor: Anchor,
    window: Range<usize>,
    preview: Arc<str>,
    /// The window's text as plain text, made the first time it is asked
    /// for after the window is rebuilt.
    accessible: OnceCell<Arc<str>>,
    /// How long `markdown` was when the window was last rebuilt.
    settled_len: usize,
}

impl TimelineBlock {
    pub fn new(id: u64, kind: BlockKind, markdown: String) -> Self {
        let mut block = Self {
            id,
            kind,
            generated: None,
            attachments: Vec::new(),
            markdown,
            fences: FenceIndex::default(),
            anchor: Anchor::Head,
            window: 0..0,
            preview: Arc::from(""),
            accessible: OnceCell::new(),
            settled_len: 0,
        };
        block.fences.extend(&block.markdown);
        block.refresh();
        block
    }

    pub fn attachments(&self) -> &[crate::transcript_attachments::Attachment] {
        &self.attachments
    }

    pub fn generated(&self) -> Option<GeneratedMessage> {
        self.generated
    }

    pub(crate) fn with_generated(mut self, generated: Option<GeneratedMessage>) -> Self {
        self.generated = generated;
        self
    }

    pub(crate) fn with_attachments(
        mut self,
        attachments: Vec<crate::transcript_attachments::Attachment>,
    ) -> Self {
        self.attachments = attachments;
        self
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
        // A window that starts inside a fenced code block is given a short
        // opening line that stands for that block's own, so it is still laid
        // out as code. This is for layout only; `markdown` is what a copy
        // must use.
        self.preview = match self.fences.fenced_at(self.window.start) {
            Some(fenced) => Arc::from(fenced.layout_opener(text) + shown),
            None => Arc::from(shown),
        };
        self.settled_len = text.len();
        self.accessible = OnceCell::new();
    }

    /// The whole block, exactly as received. Copying must use this.
    pub fn markdown(&self) -> &str {
        &self.markdown
    }

    /// What the row lays out: the window, preceded by a short opening line
    /// for a fenced code block it starts inside. Never longer than
    /// [`PREVIEW_MAX_BYTES`]. Cheap to clone.
    pub fn preview(&self) -> &Arc<str> {
        &self.preview
    }

    /// What assistive technology is given for the row: [`Self::preview`]
    /// as plain text ([`plain_text`]). Made once for each rebuild of the
    /// window, when first asked for, so a frame that redraws an unchanged
    /// row does not parse it again.
    pub fn accessible_text(&self) -> Arc<str> {
        self.accessible
            .get_or_init(|| {
                let started = Instant::now();
                let text = Arc::from(plain_text(&self.preview));
                ACCESSIBLE_BUILDS.fetch_add(1, Ordering::Relaxed);
                ACCESSIBLE_NANOS.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                ACCESSIBLE_BYTES.fetch_add(self.preview.len() as u64, Ordering::Relaxed);
                text
            })
            .clone()
    }

    /// Bytes of plain text this row keeps for assistive technology now: the
    /// text of its current window if it has been asked for since the
    /// window was last rebuilt, none otherwise. At most
    /// [`ACCESSIBLE_MAX_BYTES`].
    pub fn accessible_retained_bytes(&self) -> usize {
        self.accessible.get().map_or(0, |text| text.len())
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
        debug_assert!(!self.is_stale(), "settle the row before moving it");
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
        debug_assert!(!self.is_stale(), "settle the row before moving it");
        if self.hidden_after() > 0 {
            self.anchor = Anchor::At(self.window.end);
            self.refresh();
        }
    }

    /// Whether text has been added that the window should take in and has
    /// not yet. A window on the tail moves with the text. Any other window
    /// only changes while it still reaches the end of the block, that is,
    /// until it is full; text added past a full window cannot change it.
    ///
    /// While this is true, [`Self::preview`] and [`Self::window`] are what
    /// they were before that text arrived. [`Self::markdown`] is never
    /// behind.
    pub fn is_stale(&self) -> bool {
        self.markdown.len() > self.settled_len
            && (self.anchor == Anchor::Tail || self.window.end == self.settled_len)
    }

    /// Append text. The window is not rebuilt: that waits for
    /// [`Self::settle`], so a thousand deltas between two frames cost one
    /// rebuild, not a thousand.
    fn push(&mut self, text: &str) {
        self.markdown.push_str(text);
        self.fences.extend(&self.markdown);
    }

    /// Rebuild the window if text has been added that it should take in.
    /// Returns the bytes copied to do it, or `None` if there was nothing to
    /// do.
    fn settle(&mut self) -> Option<usize> {
        if !self.is_stale() {
            return None;
        }
        self.refresh();
        Some(self.preview.len())
    }
}

/// Work done to keep the streamed block's row up to date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamWork {
    /// Deltas appended.
    pub deltas: u64,
    /// Times the row's window was rebuilt to take them in.
    pub rebuilds: u64,
    /// Bytes copied into the text those rebuilds lay out.
    pub bytes_copied: u64,
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
    work: StreamWork,
}

impl Timeline {
    pub fn new(blocks: Vec<TimelineBlock>) -> Self {
        Self {
            blocks,
            streaming: false,
            work: StreamWork::default(),
        }
    }

    pub fn blocks(&self) -> &[TimelineBlock] {
        &self.blocks
    }

    /// The block with this id, if there is one.
    pub fn block(&self, id: u64) -> Option<&TimelineBlock> {
        self.blocks.iter().find(|block| block.id == id)
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
        self.finish_stream();
        let id = self.blocks.last().map_or(1, |block| block.id + 1);
        self.begin_stream_with_id(id, kind);
        id
    }

    /// A live row with an explicitly ephemeral id, separate from persisted
    /// message sequences. Used by transcript event reconciliation.
    pub(crate) fn begin_stream_with_id(&mut self, id: u64, kind: BlockKind) {
        debug_assert!(!self.streaming);
        debug_assert!(self.block(id).is_none());
        let mut block = TimelineBlock::new(id, kind, String::new());
        block.show_end();
        self.blocks.push(block);
        self.streaming = true;
    }

    /// Discard a provisional answer, never a persisted block. Completion
    /// replaces it with canonical text; restart must not keep its prefix.
    pub(crate) fn discard_stream(&mut self) {
        if self.streaming {
            self.blocks.pop();
            self.streaming = false;
        }
    }

    /// Insert persisted text before the provisional tail, if one exists.
    pub(crate) fn append_persisted(&mut self, block: TimelineBlock) {
        let index = self.blocks.len() - usize::from(self.streaming);
        self.blocks.insert(index, block);
    }

    /// Move the window of the block with this id. Returns the block's
    /// position, or `None` if there is no such block.
    pub fn move_window(&mut self, block_id: u64, to: WindowMove) -> Option<usize> {
        let index = self.blocks.iter().position(|block| block.id == block_id)?;
        // Earlier and Later start from where the window is now.
        self.settle();
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
    ///
    /// The block's text is complete at once. Its row is not rebuilt until
    /// [`Self::settle`].
    pub fn push_delta(&mut self, text: &str) -> bool {
        match self.blocks.last_mut() {
            Some(tail) if self.streaming => {
                self.work.deltas += 1;
                tail.push(text);
                true
            }
            _ => false,
        }
    }

    /// Bring the streamed block's row up to date with its text: one rebuild
    /// for all the deltas since the last one. Returns whether there was
    /// anything to do. The view calls this before it reads a row.
    ///
    /// Only the last block can need it, and only while it is streamed:
    /// finishing a stream settles it.
    pub fn settle(&mut self) -> bool {
        let Some(copied) = self.blocks.last_mut().and_then(TimelineBlock::settle) else {
            return false;
        };
        self.work.rebuilds += 1;
        self.work.bytes_copied += copied as u64;
        true
    }

    /// Bytes of plain text all rows keep for assistive technology now.
    pub fn accessible_retained_bytes(&self) -> usize {
        self.blocks
            .iter()
            .map(TimelineBlock::accessible_retained_bytes)
            .sum()
    }

    /// Work done so far to keep streamed rows up to date.
    pub fn stream_work(&self) -> StreamWork {
        self.work
    }

    /// End the stream. The block's row is brought up to date, so a
    /// finished block's row never lags behind its text.
    pub fn finish_stream(&mut self) {
        self.settle();
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
    use crate::fixture;

    /// Append a delta and bring the row up to date, as a view that shows
    /// every delta would.
    fn push_shown(timeline: &mut Timeline, text: &str) -> bool {
        let pushed = timeline.push_delta(text);
        timeline.settle();
        pushed
    }

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

    /// The opening line a window starting at `offset` would be given.
    fn opener_at(index: &FenceIndex, text: &str, offset: usize) -> Option<String> {
        index
            .fenced_at(offset)
            .map(|fenced| fenced.layout_opener(text))
    }

    fn index_of(text: &str) -> FenceIndex {
        let mut index = FenceIndex::default();
        index.extend(text);
        index
    }

    #[test]
    fn fences_are_tracked_by_commonmark_rules() {
        let text = "before\n~~~~ text\n```\n~~~\n~~~~\nafter\n    ```\ndone\n";
        let index = index_of(text);
        let at = |needle: &str| opener_at(&index, text, text.find(needle).unwrap());
        let tildes = Some("~~~~text\n".to_string());
        assert_eq!(at("before"), None);
        // Inside the tilde fence: backticks do not close it, and neither does
        // a shorter run of tildes.
        assert_eq!(at("```"), tildes);
        assert_eq!(at("~~~\n"), tildes);
        assert_eq!(at("~~~~\nafter"), tildes);
        assert_eq!(at("after"), None);
        // Four spaces of indent is not a fence.
        assert_eq!(at("done"), None);

        // Backticks around inline code do not open a fence; tildes with a
        // backtick after them do.
        let text = "```inline``` code\nprose\n~~~ a`b\nbody\n";
        let index = index_of(text);
        let at = |needle: &str| opener_at(&index, text, text.find(needle).unwrap());
        assert_eq!(at("prose"), None);
        assert_eq!(at("body"), Some("~~~a`b\n".to_string()));
    }

    #[test]
    fn the_layout_opener_keeps_the_indent_the_marker_and_a_short_language() {
        let opener = |opening_line: &str| {
            let text = format!("{opening_line}\ncode\n");
            opener_at(&index_of(&text), &text, text.find("code").unwrap()).unwrap()
        };
        assert_eq!(opener("```"), "```\n");
        assert_eq!(opener("```rust"), "```rust\n");
        assert_eq!(opener("  ~~~~   python title=\"x\"  "), "  ~~~~python\n");
        assert_eq!(opener("```ภาษาไทย\r"), "```ภาษาไทย\n");

        // A name as long as the limit is kept; one byte more and it is
        // left out rather than shortened.
        let longest = "n".repeat(FENCE_INFO_MAX);
        assert_eq!(opener(&format!("```{longest}")), format!("```{longest}\n"));
        assert_eq!(
            opener(&format!("```{longest} more")),
            format!("```{longest}\n")
        );
        assert_eq!(opener(&format!("```{longest}n")), "```\n");
        // Space before the name counts toward the limit.
        assert_eq!(opener(&format!("```{}rust", " ".repeat(60))), "```rust\n");
        assert_eq!(opener(&format!("```{}rust", " ".repeat(61))), "```\n");
        // The limit falls inside a three-byte character.
        let thai = "ก".repeat(FENCE_INFO_MAX / 3 + 1);
        assert_eq!(opener(&format!("```{thai}")), "```\n");
        // A short name is kept whatever follows it.
        let rest = "x".repeat(4096);
        assert_eq!(opener(&format!("```rust {rest}")), "```rust\n");

        // More markers than the limit: the row's opening line has the limit.
        let many = "`".repeat(FENCE_RUN_MAX + 8);
        let capped = "`".repeat(FENCE_RUN_MAX);
        assert_eq!(opener(&format!("{many}sh")), format!("{capped}sh\n"));
    }

    #[test]
    fn the_index_closes_a_long_fence_only_on_its_real_closing_line() {
        // The index works from the real number of markers, not the number
        // the row's opening line is given.
        let opening = "`".repeat(FENCE_RUN_MAX + 8);
        let shorter = "`".repeat(FENCE_RUN_MAX + 2);
        let text = format!("{opening}\none\n{shorter}\ntwo\n{opening}\nafter\n");
        let index = index_of(&text);
        assert!(index.fenced_at(text.find("one").unwrap()).is_some());
        assert!(index.fenced_at(text.find("two").unwrap()).is_some());
        assert!(index.fenced_at(text.find("after").unwrap()).is_none());
    }

    /// Opening lines that are enormous in each of the ways one can be.
    fn enormous_opening_lines() -> Vec<(String, String)> {
        const HUGE: usize = 1024 * 1024;
        vec![
            // The independent review's case: a megabyte of info string.
            (format!("```{}", "x".repeat(HUGE)), "```\n".to_string()),
            // A megabyte of markers.
            ("~".repeat(HUGE), format!("{}\n", "~".repeat(FENCE_RUN_MAX))),
            // A language name with a megabyte of space before it.
            (format!("```{}rust", " ".repeat(HUGE)), "```\n".to_string()),
            // A language name with a megabyte of Thai after it.
            (
                format!("   ```rust {}", "ก".repeat(HUGE / 3)),
                "   ```rust\n".to_string(),
            ),
        ]
    }

    #[test]
    fn an_enormous_fence_opening_line_cannot_make_the_row_unbounded() {
        for (opening_line, expected) in enormous_opening_lines() {
            let text = format!("{opening_line}\n{}", "body\n".repeat(20_000));
            let mut b = block(&text);
            let check = |b: &TimelineBlock, what: &str| {
                let window = b.window();
                assert!(window.len() <= BYTES, "{what}");
                assert!(b.preview().len() <= PREVIEW_MAX_BYTES, "{what}");
                assert_eq!(
                    &**b.preview(),
                    format!("{expected}{}", &text[window]),
                    "{what}"
                );
            };

            // The head window is the start of the opening line itself: the
            // raw text, cut by size, and nothing added.
            assert_eq!(&**b.preview(), &text[b.window()]);
            assert!(b.preview().len() <= BYTES);

            b.show_end();
            check(&b, "tail");
            b.show_earlier();
            check(&b, "one window earlier");
            b.show_later();
            check(&b, "one window later");

            // Copying is not affected: the block keeps the text as received.
            assert_eq!(b.markdown(), text);
        }
    }

    #[test]
    fn an_enormous_fence_opening_line_that_is_streamed_stays_bounded() {
        for (opening_line, expected) in enormous_opening_lines() {
            let text = format!("{opening_line}\n{}", "body\n".repeat(20_000));
            let mut timeline = Timeline::default();
            timeline.begin_stream(BlockKind::Assistant);
            // Pieces of 1000 bytes that fall anywhere, cut back to a
            // character boundary.
            let mut sent = 0;
            while sent < text.len() {
                let mut end = (sent + 1000).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                push_shown(&mut timeline, &text[sent..end]);
                sent = end;
                let preview = timeline.blocks()[0].preview();
                assert!(preview.len() <= PREVIEW_MAX_BYTES, "after {sent} bytes");
            }
            let tail = &timeline.blocks()[0];
            assert_eq!(tail.hidden_after(), 0);
            assert_eq!(
                &**tail.preview(),
                format!("{expected}{}", &text[tail.window()])
            );
            assert_eq!(tail.markdown(), text);
        }
    }

    #[test]
    fn a_line_that_arrives_in_many_pieces_is_searched_once() {
        // 10,000 pieces of one line: searching from the start of the line
        // each time would look at 5 GB; once each is 1 MB.
        let piece = "x".repeat(100);
        let mut text = String::from("```");
        let mut index = FenceIndex::default();
        for _ in 0..10_000 {
            text.push_str(&piece);
            index.extend(&text);
        }
        assert_eq!(index.work, text.len());
        assert!(index.blocks.is_empty(), "the line is not finished yet");

        text.push_str("\ncode\n```\nafter\n");
        index.extend(&text);
        assert_eq!(index.work, text.len());
        assert_eq!(index, index_of(&text));
        assert!(index.fenced_at(text.find("code").unwrap()).is_some());
        assert!(index.fenced_at(text.find("after").unwrap()).is_none());
    }

    #[test]
    fn the_index_is_the_same_however_the_text_is_cut_into_pieces() {
        let text = "intro\n```rust\nfn main() {}\n~~~\n```  \n\n  ~~~~ ไทย\nก ข ค\n```\n~~~~~\nend\n```\nlast";
        let whole = index_of(text);
        assert_eq!(whole.blocks.len(), 3);
        assert_eq!(whole.blocks[2].end, Fenced::OPEN, "the last is not closed");
        for size in [1, 2, 3, 5, 7, 16, 64] {
            let mut index = FenceIndex::default();
            let mut sent = 0;
            while sent < text.len() {
                let mut end = (sent + size).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                sent = end;
                index.extend(&text[..sent]);
            }
            assert_eq!(index, whole, "pieces of {size} bytes");
        }
    }

    #[test]
    fn a_fence_line_that_arrives_in_pieces_is_still_seen() {
        let mut timeline = Timeline::default();
        timeline.begin_stream(BlockKind::Assistant);
        for piece in ["Here is the code:\n\n`", "``", "py", "thon\n"] {
            push_shown(&mut timeline, piece);
        }
        let body = numbered_lines(1_200);
        for line in body.split_inclusive('\n') {
            push_shown(&mut timeline, line);
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
        assert!(!push_shown(&mut timeline, "ignored"), "no stream yet");
        assert_eq!(timeline.blocks()[1].markdown(), "answer");

        let id = timeline.begin_stream(BlockKind::Assistant);
        assert_eq!(id, 9);
        assert!(push_shown(&mut timeline, "สวัส"));
        assert!(push_shown(&mut timeline, "ดี"));
        let ids: Vec<u64> = timeline.blocks().iter().map(|b| b.id).collect();
        assert_eq!(ids, [7, 8, 9]);
        assert_eq!(timeline.blocks()[0].markdown(), "question");
        assert_eq!(timeline.blocks()[1].markdown(), "answer");
        assert_eq!(timeline.blocks()[2].markdown(), "สวัสดี");
        assert_eq!(&**timeline.blocks()[2].preview(), "สวัสดี");

        timeline.finish_stream();
        assert!(!push_shown(&mut timeline, "late"));
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
            push_shown(&mut timeline, &delta);
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
            push_shown(&mut timeline, line);
        }
        // The reader goes back to the start.
        assert_eq!(timeline.move_window(id, WindowMove::Start), Some(0));
        let pinned = timeline.blocks()[0].preview().clone();
        let hidden_after = timeline.blocks()[0].hidden_after();
        for _ in 0..500 {
            push_shown(&mut timeline, line);
        }
        let tail = &timeline.blocks()[0];
        // Same allocation: nothing was recomputed, nothing moved.
        assert!(Arc::ptr_eq(&pinned, tail.preview()));
        assert_eq!(tail.anchor(), Anchor::Head);
        // What arrived meanwhile is counted, so the row can say so.
        assert_eq!(tail.hidden_after(), hidden_after + 500 * line.len());

        // Going back to the end shows the newest text and follows it again.
        timeline.move_window(id, WindowMove::End);
        push_shown(&mut timeline, "the very latest\n");
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
        push_shown(&mut timeline, "first\n");
        push_shown(&mut timeline, "second\n");
        assert_eq!(&**timeline.blocks()[0].preview(), "first\nsecond\n");
        assert!(!timeline.blocks()[0].is_truncated());
    }

    #[test]
    fn a_burst_of_deltas_rebuilds_the_row_once() {
        let mut timeline = Timeline::default();
        timeline.begin_stream(BlockKind::Assistant);
        let mut text = String::new();
        for index in 0..100_000 {
            let delta = fixture::stream_delta(index);
            text.push_str(delta);
            timeline.push_delta(delta);
        }
        let work = timeline.stream_work();
        assert_eq!(
            (work.deltas, work.rebuilds, work.bytes_copied),
            (100_000, 0, 0)
        );
        // The text is complete at once. The row is what it was.
        let tail = &timeline.blocks()[0];
        assert_eq!(tail.markdown(), text);
        assert!(tail.is_stale());
        assert_eq!(&**tail.preview(), "");

        assert!(timeline.settle());
        assert!(!timeline.settle(), "nothing more to do");
        let work = timeline.stream_work();
        assert_eq!(work.rebuilds, 1);
        assert!(work.bytes_copied as usize <= PREVIEW_MAX_BYTES);
        // It shows what a block given the whole text at once shows of its
        // end: the newest text, and no more than one window.
        let mut whole = block(&text);
        whole.show_end();
        let tail = &timeline.blocks()[0];
        assert!(!tail.is_stale());
        assert_eq!(tail.preview(), whole.preview());
        assert_eq!(tail.window(), whole.window());
        assert_eq!(tail.hidden_after(), 0);
        assert_eq!(tail.markdown(), text);
    }

    #[test]
    fn settling_now_and_then_shows_what_settling_after_every_delta_shows() {
        const DELTAS: usize = 14_000;
        // A reader's moves, by how many deltas had arrived when each was
        // made: to a head window that is still growing, on a page, back a
        // page, to the tail, back a page, and on to a last page that is
        // still growing.
        let moves = [
            (2_000, WindowMove::Start),
            (6_000, WindowMove::Later),
            (6_500, WindowMove::Earlier),
            (9_000, WindowMove::End),
            (11_000, WindowMove::Earlier),
            (11_500, WindowMove::Later),
        ];
        for every in [2, 7, 17, 250, 4_000] {
            let mut each = Timeline::default();
            let mut some = Timeline::default();
            let id = each.begin_stream(BlockKind::Assistant);
            some.begin_stream(BlockKind::Assistant);
            let mut text = String::new();
            let same = |each: &Timeline, some: &Timeline, text: &str, when: &str| {
                let (a, b) = (&each.blocks()[0], &some.blocks()[0]);
                assert!(!b.is_stale(), "{when}");
                assert_eq!(b.preview(), a.preview(), "{when}");
                assert_eq!(b.window(), a.window(), "{when}");
                assert_eq!(b.anchor(), a.anchor(), "{when}");
                assert_eq!(b.markdown(), text, "{when}");
            };
            for index in 0..DELTAS {
                if let Some((_, to)) = moves.iter().find(|(at, _)| *at == index) {
                    each.move_window(id, *to);
                    some.move_window(id, *to);
                    same(
                        &each,
                        &some,
                        &text,
                        &format!("moved at {index}, every {every}"),
                    );
                }
                let delta = fixture::stream_delta(index);
                text.push_str(delta);
                push_shown(&mut each, delta);
                some.push_delta(delta);
                // Behind or not, the row is bounded and the text is whole.
                let row = &some.blocks()[0];
                assert!(row.preview().len() <= PREVIEW_MAX_BYTES);
                assert_eq!(row.total_bytes(), text.len());
                if (index + 1) % every == 0 {
                    some.settle();
                    same(
                        &each,
                        &some,
                        &text,
                        &format!("after {index}, every {every}"),
                    );
                }
            }
            each.finish_stream();
            some.finish_stream();
            same(&each, &some, &text, &format!("finished, every {every}"));
            // One rebuild at most for each time the row was looked at.
            let looks = (DELTAS / every + moves.len() + 1) as u64;
            assert!(some.stream_work().rebuilds <= looks, "every {every}");
            assert!(each.stream_work().rebuilds >= some.stream_work().rebuilds);
        }
    }

    #[test]
    fn finishing_or_restarting_a_stream_leaves_no_row_behind_its_text() {
        let mut timeline = Timeline::default();
        timeline.begin_stream(BlockKind::Assistant);
        timeline.push_delta("one\n");
        timeline.finish_stream();
        assert!(!timeline.blocks()[0].is_stale());
        assert_eq!(&**timeline.blocks()[0].preview(), "one\n");

        // A second stream, and a third begun without finishing it.
        timeline.begin_stream(BlockKind::Assistant);
        timeline.push_delta("two\n");
        timeline.begin_stream(BlockKind::Assistant);
        assert_eq!(&**timeline.blocks()[1].preview(), "two\n");
        assert!(timeline.blocks().iter().all(|block| !block.is_stale()));
        assert_eq!(timeline.stream_work().rebuilds, 2);
    }

    #[test]
    fn a_full_window_that_is_not_on_the_tail_is_never_rebuilt_by_a_stream() {
        let mut timeline = Timeline::default();
        let id = timeline.begin_stream(BlockKind::Assistant);
        for line in numbered_lines(1_500).lines() {
            timeline.push_delta(line);
            timeline.push_delta("\n");
        }
        timeline.move_window(id, WindowMove::Start);
        let before = timeline.stream_work().rebuilds;
        let head = timeline.blocks()[0].preview().clone();
        for index in 0..1_000 {
            timeline.push_delta(fixture::stream_delta(index));
            assert!(!timeline.blocks()[0].is_stale());
            assert!(!timeline.settle());
        }
        assert_eq!(timeline.stream_work().rebuilds, before);
        assert_eq!(timeline.blocks()[0].preview(), &head);
    }

    #[test]
    fn plain_text_keeps_the_words_and_drops_the_markup() {
        let cases = [
            ("**bold** and _em_ and `code`", "bold and em and code"),
            ("# Title\n\nBody", "Title\nBody"),
            ("> ข้อความอ้างอิง\n\nต่อ", "ข้อความอ้างอิง\nต่อ"),
            (
                "```rust\nfn main() {}\n```\n\nAfter.",
                "fn main() {}\nAfter.",
            ),
            ("- one\n- two", "one\ntwo"),
            // Numbers are drawn and mean something; they are kept.
            ("1. one\n2. two", "1. one\n2. two"),
            ("7. seven\n8. eight", "7. seven\n8. eight"),
            ("0. zero\n1. one", "0. zero\n1. one"),
            // Nested ordered lists are lettered, as the view draws them.
            (
                "3. top\n   1. inner\n   2. next\n      1. deep",
                "3. top\nA. inner\nB. next\na. deep",
            ),
            // Task boxes are drawn checked or not, and that is kept; a list
            // with task items draws no numbers or bullets.
            ("- [x] done\n- [ ] to do", "☑ done\n☐ to do"),
            (
                "1. [x] done\n2. [ ] to do\n3. plain",
                "☑ done\n☐ to do\nplain",
            ),
            // References keep the words and the alt text; definitions are not
            // shown.
            (
                "See [the guide][g] and ![a chart][c].\n\n[g]: https://example.invalid\n[c]: c.png",
                "See the guide and a chart.",
            ),
            // An image reference with no definition is not an image at all;
            // its text stays as written.
            ("![missing][nowhere]", "![missing][nowhere]"),
            // Code is kept exactly, markers and all.
            (
                "```\n- [x] not a task\n1. not a list\n```",
                "- [x] not a task\n1. not a list",
            ),
            ("`**not bold**`", "**not bold**"),
            (
                "See [the guide](https://example.invalid) now.",
                "See the guide now.",
            ),
            ("![a chart](x.png)", "a chart"),
            ("line one  \nline two", "line one\nline two"),
            ("a <b>raw</b> tag", "a raw tag"),
            ("| a | b |\n|---|---|\n| 1 | 2 |", "a\tb\n1\t2"),
            ("👩🏽‍💻 🇹🇭 é", "👩🏽‍💻 🇹🇭 é"),
            ("", ""),
        ];
        for (source, expected) in cases {
            assert_eq!(plain_text(source), expected, "{source:?}");
        }
    }

    #[test]
    fn a_rows_accessible_text_is_its_window_as_plain_text_made_once() {
        let mut b = block("Intro.\n\n```rust\nfn main() {}\n```\n\n_done_");
        assert_eq!(&*b.accessible_text(), "Intro.\nfn main() {}\ndone");
        // The same text object until the window is rebuilt.
        assert!(Arc::ptr_eq(&b.accessible_text(), &b.accessible_text()));
        // A window that starts inside a fence is given a short opening
        // line for layout; the accessible text has no fence in it.
        let code = numbered_lines(1_500);
        b = block(&format!("```rust\n{code}```\n"));
        b.show_later();
        assert!(b.preview().starts_with("```rust\n"));
        let text = b.accessible_text();
        assert!(!text.contains("```"), "{}", &text[..40]);
        assert!(text.starts_with("line "), "{}", &text[..40]);
        // Rebuilding the window makes it again.
        let before = b.accessible_text();
        b.show_start();
        assert!(!Arc::ptr_eq(&before, &b.accessible_text()));
    }

    #[test]
    fn making_a_rows_accessible_text_is_counted_once_per_rebuild() {
        let b = block("**one** and `two`\n\n- [x] three");
        let (builds, _, bytes) = accessible_text_stats();
        assert_eq!(&*b.accessible_text(), "one and two\n☑ three");
        b.accessible_text();
        let (builds_after, _, bytes_after) = accessible_text_stats();
        // Other tests make accessible text at the same time, so this
        // checks that this row added at least its one build and its bytes.
        assert!(builds_after > builds);
        assert!(bytes_after >= bytes + b.preview().len() as u64);
    }

    #[test]
    fn a_rows_plain_text_never_exceeds_its_bound() {
        // Plain text drops markup, so it is shorter, except where a list
        // label is longer than the marker in the source. The worst case: a
        // full window of one-character items, each written "1." in a list
        // counting from 999999999, nested so that labels are letters too.
        let mut worst = String::from("999999999. a\n");
        while worst.len() < BYTES {
            worst.push_str("1. a\n");
        }
        let nested = format!("1. top\n{}", "   999999999. a\n".repeat(LINES));
        for source in [worst.as_str(), nested.as_str(), &numbered_lines(2_000)] {
            let mut b = block(source);
            for to in [WindowMove::Start, WindowMove::Later, WindowMove::End] {
                match to {
                    WindowMove::Start => b.show_start(),
                    WindowMove::Later => b.show_later(),
                    _ => b.show_end(),
                }
                let text = b.accessible_text();
                assert!(text.len() <= ACCESSIBLE_MAX_BYTES, "{}", text.len());
                assert_eq!(b.accessible_retained_bytes(), text.len());
            }
        }
        // What the worst case comes to, for the record.
        let mut b = block(&worst);
        b.show_start();
        println!(
            "EVIDENCE accessible_bound: window {} bytes, plain text {} bytes, bound {}",
            b.preview().len(),
            b.accessible_text().len(),
            ACCESSIBLE_MAX_BYTES
        );
    }

    #[test]
    fn moving_a_window_keeps_one_plain_text_not_one_per_move() {
        let mut b = block(&numbered_lines(5_000));
        assert_eq!(b.accessible_retained_bytes(), 0, "nothing until asked");
        let mut largest = 0;
        for round in 0..200 {
            match round % 4 {
                0 => b.show_later(),
                1 => b.show_later(),
                2 => b.show_earlier(),
                _ => b.show_end(),
            }
            // A move that rebuilds the window lets go of the old text at
            // once; one that changes nothing (Later at the end) keeps the
            // text of the window it still shows. Never more than one.
            let kept = b.accessible_retained_bytes();
            assert!(
                kept == 0 || kept == plain_text(b.preview()).len(),
                "round {round}: {kept}"
            );
            let text = b.accessible_text();
            assert_eq!(b.accessible_retained_bytes(), text.len());
            largest = largest.max(text.len());
        }
        assert!(largest <= ACCESSIBLE_MAX_BYTES);
    }

    #[test]
    fn a_streamed_row_keeps_the_plain_text_of_its_current_window_only() {
        let mut timeline = Timeline::default();
        timeline.begin_stream(BlockKind::Assistant);
        for index in 0..20_000 {
            timeline.push_delta(fixture::stream_markdown_delta(index));
            if index % 7 == 0 {
                timeline.settle();
                timeline.blocks()[0].accessible_text();
            }
        }
        timeline.finish_stream();
        let tail = &timeline.blocks()[0];
        // The text it keeps is the text of the window it shows now: not
        // behind the tail, and nothing from earlier windows.
        let text = tail.accessible_text();
        assert_eq!(&*text, plain_text(tail.preview()));
        assert_eq!(tail.hidden_after(), 0);
        assert_eq!(timeline.accessible_retained_bytes(), text.len());
    }

    #[test]
    fn plain_text_kept_for_a_whole_transcript_is_bounded_by_its_rows() {
        // Every row of a large transcript read once by assistive
        // technology, as scrolling through it all with a screen reader on
        // would do. Nothing is let go of until a row's window is rebuilt,
        // so this is the most it can keep.
        let mut blocks = fixture::timeline(10_000);
        blocks.push(fixture::huge_block(10_001, 5 * 1024 * 1024));
        let timeline = Timeline::new(blocks);
        let previews: usize = timeline.blocks().iter().map(|b| b.preview().len()).sum();
        for b in timeline.blocks() {
            b.accessible_text();
        }
        let kept = timeline.accessible_retained_bytes();
        for b in timeline.blocks() {
            assert!(b.accessible_retained_bytes() <= ACCESSIBLE_MAX_BYTES);
        }
        assert!(kept <= timeline.len() * ACCESSIBLE_MAX_BYTES);
        println!(
            "EVIDENCE accessible_retained: {} rows, previews {previews} bytes, plain text kept {kept} bytes",
            timeline.len()
        );
    }

    #[test]
    fn plain_text_of_a_full_window_is_quick_enough_to_make_once_a_frame() {
        // The largest window a row lays out, streamed text with marks in it.
        let mut source = String::new();
        while source.len() < BYTES {
            source.push_str("**ย่อหน้า** with `code` and _emphasis_ 👩🏽‍💻.\n\n");
        }
        let started = std::time::Instant::now();
        let rounds = 20;
        for _ in 0..rounds {
            std::hint::black_box(plain_text(&source));
        }
        let each = started.elapsed() / rounds;
        println!(
            "EVIDENCE plain_text: {} bytes in {:?} each (this build, this machine)",
            source.len(),
            each
        );
    }

    /// Deterministic pseudo-random text built from pieces that stress the
    /// window, fence and plain-text code: multi-byte and combining
    /// characters, CRLF, fences of both kinds and lengths, list and task
    /// markers, quotes, emphasis and long lines.
    fn mixed_text(seed: u64, pieces: usize) -> String {
        const PIECES: [&str; 22] = [
            "ข้อความไทย ",
            "👩🏽‍💻",
            "é",
            "漢字",
            "\r\n",
            "\n",
            "\n\n",
            "```",
            "~~~~ py\n",
            "```rust\n",
            "````\n",
            "- [x] ",
            "- [ ] ",
            "1. ",
            "999999999. ",
            "   2. ",
            "> ",
            "**b** ",
            "_i_ ",
            "`c` ",
            "[l](u) ",
            "x",
        ];
        let mut state = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut text = String::new();
        for _ in 0..pieces {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let piece = PIECES[(state >> 33) as usize % PIECES.len()];
            if (state >> 20).is_multiple_of(50) {
                // Now and then a line far longer than a window.
                text.push_str(&"ก".repeat(20_000));
            }
            text.push_str(piece);
        }
        text
    }

    #[test]
    fn mixed_text_keeps_every_bound_and_boundary_in_every_window() {
        // Fewer in a debug build, where making plain text is slow.
        let seeds = if cfg!(debug_assertions) { 4 } else { 24 };
        for seed in 0..seeds {
            let text = mixed_text(seed, 4_000);
            let mut b = block(&text);
            for step in 0..40 {
                match step % 5 {
                    0 => b.show_start(),
                    1 | 2 => b.show_later(),
                    3 => b.show_earlier(),
                    _ => b.show_end(),
                }
                let window = b.window();
                assert!(
                    text.is_char_boundary(window.start),
                    "seed {seed} step {step}"
                );
                assert!(text.is_char_boundary(window.end), "seed {seed} step {step}");
                assert!(window.len() <= BYTES, "seed {seed} step {step}");
                assert!(
                    b.preview().len() <= PREVIEW_MAX_BYTES,
                    "seed {seed} step {step}"
                );
                assert!(b.preview().ends_with(&text[window.clone()]));
                assert!(
                    b.accessible_text().len() <= ACCESSIBLE_MAX_BYTES,
                    "seed {seed}"
                );
                assert_eq!(b.markdown(), text);
            }
        }
    }

    #[test]
    fn mixed_text_streamed_in_any_pieces_ends_where_the_whole_text_does() {
        let seeds = if cfg!(debug_assertions) { 3 } else { 12 };
        for seed in 0..seeds {
            let text = mixed_text(seed, 3_000);
            let whole = {
                let mut b = block(&text);
                b.show_end();
                (b.window(), b.preview().clone(), b.accessible_text())
            };
            for size in [1usize, 3, 7, 64, 1_000] {
                let mut timeline = Timeline::default();
                timeline.begin_stream(BlockKind::Assistant);
                let mut sent = 0;
                while sent < text.len() {
                    let mut end = (sent + size).min(text.len());
                    while !text.is_char_boundary(end) {
                        end += 1;
                    }
                    timeline.push_delta(&text[sent..end]);
                    sent = end;
                    if sent.is_multiple_of(5) {
                        timeline.settle();
                    }
                }
                timeline.finish_stream();
                let tail = &timeline.blocks()[0];
                assert_eq!(tail.markdown(), text, "seed {seed} size {size}");
                assert_eq!(tail.window(), whole.0, "seed {seed} size {size}");
                assert_eq!(tail.preview(), &whole.1, "seed {seed} size {size}");
                assert_eq!(tail.accessible_text(), whole.2, "seed {seed} size {size}");
                assert_eq!(tail.hidden_after(), 0);
            }
        }
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
