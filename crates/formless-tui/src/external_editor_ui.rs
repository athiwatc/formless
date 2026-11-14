//! The editor result changes one buffer only. Existing send/save/validation
//! paths remain responsible for committing that buffer to the daemon.
use super::*;
use std::{
    path::Path,
    process::{Command, ExitStatus},
};

#[cfg(test)]
type TestRunner = Box<dyn FnMut(&Path, Option<u32>) -> Result<ExitStatus>>;
#[derive(Default)]
pub(super) struct State {
    generation: u64,
    pending: Option<PendingClear>,
    #[cfg(test)]
    runner: Option<TestRunner>,
}
struct PendingClear {
    origin: Origin,
    replacement: String,
}
#[derive(Clone)]
struct Origin {
    generation: u64,
    text: String,
    line: u32,
    target: Target,
}
#[derive(Clone)]
enum Target {
    Composer {
        session: Option<String>,
        instructions: Option<String>,
        message: Option<String>,
        queued: Option<i64>,
        step: bool,
        display: String,
        cursor: usize,
    },
    Tool {
        session: Option<String>,
        editor_generation: u64,
        id: String,
        mode: EditMode,
        cursor: usize,
    },
}
impl Origin {
    fn capture(app: &App) -> Result<Self> {
        let (text, line, target) = if let Some(ed) = &app.editor {
            anyhow::ensure!(
                !app.pending
                    .values()
                    .any(|p| matches!(p, Pending::UpdateTool | Pending::UpdateModule)),
                "wait for the pending editor save before opening another editor"
            );
            (
                ed.text(),
                ed.chars[..ed.cursor].iter().filter(|&&c| c == '\n').count() + 1,
                Target::Tool {
                    session: app.current.clone(),
                    editor_generation: app.editor_generation,
                    id: ed.tool_id.clone(),
                    mode: ed.mode,
                    cursor: ed.cursor,
                },
            )
        } else {
            anyhow::ensure!(
                app.view == View::Chat
                    && !app.search_mode
                    && app.renaming.is_none()
                    && app.import_mode.is_none()
                    && app.bulk.is_none(),
                "external editing is available in the composer, instructions or Tool editor"
            );
            let mut expanded = app.input.clone();
            expanded.modal_expand_pastes();
            (
                expanded.to_string(),
                expanded
                    .before_cursor()
                    .chars()
                    .filter(|&c| c == '\n')
                    .count()
                    + 1,
                Target::Composer {
                    session: app.current.clone(),
                    instructions: app.instructing.clone(),
                    message: app.editing_id.clone(),
                    queued: app.editing_queued,
                    step: app.editing_step,
                    display: app.input.to_string(),
                    cursor: app.input.cursor_byte(),
                },
            )
        };
        Ok(Self {
            generation: app.external_edit.generation,
            text,
            line: line.min(u32::MAX as usize) as u32,
            target,
        })
    }
    fn extension(&self) -> &'static str {
        match self.target {
            Target::Tool {
                mode: EditMode::Code | EditMode::Module,
                ..
            } => "js",
            Target::Tool {
                mode: EditMode::Run | EditMode::Schemas,
                ..
            } => "json",
            _ => "md",
        }
    }
    fn matches(&self, app: &App) -> bool {
        if app.external_edit.generation != self.generation {
            return false;
        }
        match &self.target {
            Target::Composer {
                session,
                instructions,
                message,
                queued,
                step,
                display,
                cursor,
            } => {
                app.editor.is_none()
                    && &app.current == session
                    && &app.instructing == instructions
                    && &app.editing_id == message
                    && &app.editing_queued == queued
                    && app.editing_step == *step
                    && &*app.input == display.as_str()
                    && app.input.expanded() == self.text
                    && app.input.cursor_byte() == *cursor
                    && !app.search_mode
                    && app.renaming.is_none()
                    && app.import_mode.is_none()
                    && app.bulk.is_none()
            }
            Target::Tool {
                session,
                editor_generation,
                id,
                mode,
                cursor,
            } => {
                &app.current == session
                    && app.editor_generation == *editor_generation
                    && app.editor.as_ref().is_some_and(|ed| {
                        &ed.tool_id == id
                            && ed.mode == *mode
                            && ed.cursor == *cursor
                            && ed.text() == self.text
                    })
            }
        }
    }
}
// Preview cancellation depends only on request kind, including versions whose
// variant carries a captured identity/generation rather than being a unit.
#[allow(clippy::unneeded_struct_pattern)]
fn is_tool_preview(pending: &Pending) -> bool {
    matches!(pending, Pending::PreviewTool { .. })
}

impl App {
    pub(super) fn begin_external_edit(&mut self) {
        self.external_edit.generation = self.external_edit.generation.wrapping_add(1);
        self.external_edit.pending = None;
        let origin = match Origin::capture(self) {
            Ok(origin) => origin,
            Err(error) => {
                self.notes.push(error.to_string());
                return;
            }
        };
        // No consent prepared before opening an editor may send the old draft
        // while the terminal handoff pauses event processing, even on no change.
        self.forecast.revision = self.forecast.revision.wrapping_add(1);
        self.forecast.approved = None;
        self.forecast.modal = None;
        let superseded: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(id, pending)| matches!(pending, Pending::RunToolInput).then_some(*id))
            .collect();
        for id in superseded {
            self.pending.remove(&id);
            self.pending_since.remove(&id);
        }
        let result = external_editor::edit(&origin.text, origin.extension(), |file| {
            self.run_external_draft(file, Some(origin.line))
        });
        match result {
            Err(error) => self.notes.push(format!("{error}; original buffer kept")),
            Ok(text) => {
                // Raw unchanged files preserve even their original CRLF and
                // cursor/paste/Vim state; normalization applies only to changes.
                if text == origin.text {
                    self.notes.push(
                        "editor returned no changes; GUI commands must wait until the file closes"
                            .into(),
                    );
                    return;
                }
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if !origin.matches(self) {
                    self.notes
                        .push("editor result discarded: the target buffer changed".into());
                } else if text == origin.text {
                    self.notes.push(
                        "editor returned no changes; GUI commands must wait until the file closes"
                            .into(),
                    );
                } else if !origin.text.is_empty() && text.trim().is_empty() {
                    self.external_edit.pending = Some(PendingClear {
                        origin,
                        replacement: text,
                    });
                    self.notes.push(
                        "Editor returned an empty buffer: y clears, n/Esc keeps the original."
                            .into(),
                    );
                } else {
                    self.apply_external_edit(&origin, &text);
                }
            }
        }
    }
    fn run_external_draft(&mut self, file: &Path, line: Option<u32>) -> Result<ExitStatus> {
        #[cfg(test)]
        {
            let Some(mut run) = self.external_edit.runner.take() else {
                anyhow::bail!("no private fake-editor runner configured for this test")
            };
            let result = run(file, line);
            self.external_edit.runner = Some(run);
            self.force_redraw = true;
            result
        }
        #[cfg(not(test))]
        {
            let spec = external_editor::EditorCommand::from_env()?;
            let mut command = spec.command(file, line, true)?;
            self.run_editor_suspended(&mut command)
        }
    }
    // Single integration seam for editor generation invalidation. A Tool's
    // captured review base version belongs to the open editor and is retained.
    fn apply_external_edit(&mut self, origin: &Origin, text: &str) {
        if !origin.matches(self) {
            self.notes
                .push("editor result discarded: the target buffer changed".into());
            return;
        }
        match origin.target {
            Target::Composer { .. } => {
                let mode = self.vim_input.mode;
                let literal = self.input.is_literal();
                self.input.set(text);
                self.input.set_literal(literal);
                self.vim_input.reset();
                self.vim_input.mode = mode;
                self.palette_hidden = None;
                self.palette_sel = 0;
                self.update_mention();
            }
            Target::Tool { .. } => {
                self.editor_generation = self.editor_generation.wrapping_add(1);
                // Cancel the old uncorrelated preview in the accepted base as
                // well as clearing its displayed result. Late replies are ignored.
                let previews: Vec<_> = self
                    .pending
                    .iter()
                    .filter_map(|(id, pending)| is_tool_preview(pending).then_some(*id))
                    .collect();
                for id in previews {
                    self.pending.remove(&id);
                    self.pending_since.remove(&id);
                }
                self.editor_review = None;
                self.description_save_preview = None;
                self.description_editor_revision = self.description_editor_revision.wrapping_add(1);
                if let Some(ed) = &mut self.editor {
                    let mode = ed.vim.mode;
                    if ed.original.is_none() {
                        ed.original = Some(ed.text());
                    }
                    let mut line = ed.take_modal_line();
                    line.set(&text);
                    ed.apply_modal_line(line);
                    ed.vim.reset();
                    ed.vim.mode = mode;
                }
            }
        }
        self.notes
            .push("editor changes loaded; use the usual send/save keys to commit them".into());
    }
    pub(super) fn external_clear_key(&mut self, key: &KeyEvent) -> bool {
        let Some(pending) = self.external_edit.pending.take() else {
            return false;
        };
        if !pending.origin.matches(self) {
            self.notes
                .push("empty editor result discarded: the target buffer changed".into());
            return true;
        }
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if key.kind == KeyEventKind::Press && plain && key.code == KeyCode::Char('y') {
            self.apply_external_edit(&pending.origin, &pending.replacement);
        } else if key.code == KeyCode::Esc
            || (plain && key.code == KeyCode::Char('n'))
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
        {
            self.notes.push("kept the original buffer".into());
        } else {
            self.external_edit.pending = Some(pending);
        }
        true
    }
    pub(super) fn run_editor_suspended(&mut self, command: &mut Command) -> Result<ExitStatus> {
        self.force_redraw = true;
        let terminal = TerminalHandoff::suspend(self.mouse_capture)?;
        let result = command.status().map_err(anyhow::Error::from);
        drop(terminal);
        result
    }
}

// Construct the guard before leaving raw mode so failed terminal operations,
// failed spawn and unwinding all restore the display and exact signal policy.
struct TerminalHandoff {
    mouse: bool,
    raw: bool,
    alternate: bool,
    _interrupts: external_editor::InterruptGuard,
}
impl TerminalHandoff {
    fn suspend(mouse: bool) -> Result<Self> {
        let interrupts = external_editor::InterruptGuard::install()?;
        let raw = crossterm::terminal::is_raw_mode_enabled()?;
        let mut guard = Self {
            mouse,
            raw,
            alternate: false,
            _interrupts: interrupts,
        };
        disable_raw_mode()?;
        let mut out = stdout();
        out.execute(crossterm::event::DisableBracketedPaste)?;
        out.execute(crossterm::event::DisableMouseCapture)?;
        guard.alternate = true;
        out.execute(LeaveAlternateScreen)?;
        Ok(guard)
    }
}
impl Drop for TerminalHandoff {
    fn drop(&mut self) {
        let mut out = stdout();
        if self.alternate {
            let _ = out.execute(EnterAlternateScreen);
        }
        if self.mouse {
            let _ = out.execute(crossterm::event::EnableMouseCapture);
        }
        let _ = out.execute(crossterm::event::EnableBracketedPaste);
        if self.raw {
            let _ = enable_raw_mode();
        }
    }
}

pub(super) fn draw_confirmation(f: &mut Frame, app: &App) {
    if app.external_edit.pending.is_none() {
        return;
    }
    let area = f.area();
    render_confirmation(area, f.buffer_mut(), app.color);
}

const CONFIRM_TITLE: &str = " Clear edited buffer? ";
const CONFIRM_HEAD: &str = "The editor returned an empty buffer.";
const CONFIRM_KEYS: &str = "y: clear this buffer · n/Esc: keep original";
const CONFIRM_ENTER: &str = "Enter does not clear.";
/// Too-small fallback: same keys, one short line.
const CONFIRM_COMPACT: &str = "Empty buffer. y: clear · n/Esc: keep · Enter does not clear.";
/// Unbordered single row: keys first so clipping keeps y/n/Esc visible.
const CONFIRM_LINE: &str = "y: clear · n/Esc: keep · Enter does not clear.";
/// Rows used by `ui::panel` borders (top + bottom); same chrome as the
/// other dialogs that size themselves as `lines + 2`.
const PANEL_CHROME: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfirmLayout {
    /// All four original lines, including the blank spacer.
    Full,
    /// The three sentences without the blank spacer.
    NoSpacer,
    /// Bordered panel with the one-line compact prompt (may wrap).
    Compact,
    /// No room for borders: a single unbordered row, clipped to the area.
    Line,
}

fn confirmation_text(layout: ConfirmLayout) -> String {
    match layout {
        ConfirmLayout::Full => format!("{CONFIRM_HEAD}\n\n{CONFIRM_KEYS}\n{CONFIRM_ENTER}"),
        ConfirmLayout::NoSpacer => format!("{CONFIRM_HEAD}\n{CONFIRM_KEYS}\n{CONFIRM_ENTER}"),
        ConfirmLayout::Compact => CONFIRM_COMPACT.to_string(),
        ConfirmLayout::Line => CONFIRM_LINE.to_string(),
    }
}

/// Wrapped row count of `text` at `inner` columns, using the same wrapper
/// the renderer uses (`Wrap { trim: false }`).
fn wrapped_rows(text: &str, inner: u16) -> u16 {
    let rows = Paragraph::new(text.to_string())
        .wrap(Wrap { trim: false })
        .line_count(inner);
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// Pick the largest layout that fits `area` and return its rectangle,
/// always contained in `area`. `None` when there is nothing to draw.
fn confirmation_layout(area: Rect) -> Option<(ConfirmLayout, Rect)> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let width = 68.min(area.width.saturating_sub(2));
    let inner = width.saturating_sub(2);
    let centered = |w: u16, h: u16| {
        Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        )
    };
    if inner > 0 {
        for layout in [
            ConfirmLayout::Full,
            ConfirmLayout::NoSpacer,
            ConfirmLayout::Compact,
        ] {
            let height =
                wrapped_rows(&confirmation_text(layout), inner).saturating_add(PANEL_CHROME);
            if height <= area.height {
                return Some((layout, centered(width, height)));
            }
        }
    }
    Some((ConfirmLayout::Line, centered(area.width, 1)))
}

fn render_confirmation(area: Rect, buf: &mut Buffer, color: bool) {
    let Some((layout, rect)) = confirmation_layout(area.intersection(buf.area)) else {
        return;
    };
    Clear.render(rect, buf);
    let text = confirmation_text(layout);
    match layout {
        ConfirmLayout::Line => Paragraph::new(text)
            .style(fg(color, Color::Yellow))
            .render(rect, buf),
        _ => Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(ui::panel(CONFIRM_TITLE, color, Color::Yellow))
            .render(rect, buf),
    }
}

#[cfg(test)]
mod confirmation_render_tests {
    //! Renderer-only fixtures: synthetic buffers, no App, no editor, no clearing.
    use super::*;

    fn render(area: Rect, fill: &[&str]) -> (Buffer, Buffer) {
        let mut buf = Buffer::empty(area);
        for (i, line) in fill.iter().enumerate() {
            let y = area.y + i as u16;
            if y < area.bottom() && area.width > 0 {
                buf.set_stringn(area.x, y, line, area.width as usize, Style::default());
            }
        }
        let before = buf.clone();
        render_confirmation(area, &mut buf, false);
        (before, buf)
    }
    /// Text of each inner row of a bordered panel, right-trimmed.
    fn inner_rows(buf: &Buffer, rect: Rect) -> Vec<String> {
        (rect.y + 1..rect.bottom() - 1)
            .map(|y| {
                (rect.x + 1..rect.right() - 1)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }
    fn row(buf: &Buffer, rect: Rect, y: u16) -> String {
        (rect.x..rect.right())
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }
    fn words(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    fn contained(rect: Rect, area: Rect) -> bool {
        rect.x >= area.x
            && rect.y >= area.y
            && rect.right() <= area.right()
            && rect.bottom() <= area.bottom()
    }

    #[test]
    fn original_sentences_are_kept_verbatim() {
        assert_eq!(
            confirmation_text(ConfirmLayout::Full),
            "The editor returned an empty buffer.\n\ny: clear this buffer · n/Esc: keep original\nEnter does not clear."
        );
        assert_eq!(CONFIRM_TITLE, " Clear edited buffer? ");
        for key in ["y:", "n/Esc", "Enter"] {
            assert!(CONFIRM_COMPACT.contains(key), "{key}");
            assert!(CONFIRM_LINE.contains(key), "{key}");
        }
    }

    #[test]
    fn eighty_columns_fit_all_four_lines_with_two_rows_of_chrome() {
        let area = Rect::new(0, 0, 80, 24);
        let (layout, rect) = confirmation_layout(area).unwrap();
        assert_eq!(layout, ConfirmLayout::Full);
        assert_eq!((rect.width, rect.height), (68, 6));
        let (_, buf) = render(area, &[]);
        assert_eq!(
            inner_rows(&buf, rect),
            [CONFIRM_HEAD, "", CONFIRM_KEYS, CONFIRM_ENTER]
        );
        assert!(row(&buf, rect, rect.y).contains("Clear edited buffer?"));
        assert!(row(&buf, rect, rect.bottom() - 1).starts_with('╰'));
    }

    #[test]
    fn forty_columns_wrap_the_action_line_and_keep_every_key_visible() {
        let area = Rect::new(0, 0, 40, 24);
        let (layout, rect) = confirmation_layout(area).unwrap();
        assert_eq!(layout, ConfirmLayout::Full);
        // inner 36: head 1 + spacer 1 + action 2 + enter 1, plus 2 chrome.
        assert_eq!((rect.width, rect.height), (38, 7));
        let (_, buf) = render(area, &[]);
        let rows = inner_rows(&buf, rect);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0], CONFIRM_HEAD);
        assert_eq!(rows[1], "");
        assert_eq!(rows[4], CONFIRM_ENTER);
        assert_eq!(words(&rows[2..4].join(" ")), CONFIRM_KEYS);
        let visible = rows.join(" ");
        for key in [
            "y: clear this buffer",
            "n/Esc: keep",
            "original",
            "Enter does not clear.",
        ] {
            assert!(visible.contains(key), "{key}");
        }
    }

    #[test]
    fn short_terminals_drop_the_spacer_before_any_sentence_then_go_compact() {
        let (layout, rect) = confirmation_layout(Rect::new(0, 0, 80, 6)).unwrap();
        assert_eq!((layout, rect.height), (ConfirmLayout::Full, 6));

        let area = Rect::new(0, 0, 80, 5);
        let (layout, rect) = confirmation_layout(area).unwrap();
        assert_eq!((layout, rect.height), (ConfirmLayout::NoSpacer, 5));
        let (_, buf) = render(area, &[]);
        assert_eq!(
            inner_rows(&buf, rect),
            [CONFIRM_HEAD, CONFIRM_KEYS, CONFIRM_ENTER]
        );

        let area = Rect::new(0, 0, 80, 4);
        let (layout, rect) = confirmation_layout(area).unwrap();
        assert_eq!((layout, rect.height), (ConfirmLayout::Compact, 3));
        let (_, buf) = render(area, &[]);
        assert_eq!(inner_rows(&buf, rect), [CONFIRM_COMPACT]);
        assert!(row(&buf, rect, rect.y).contains("Clear edited buffer?"));

        let area = Rect::new(0, 0, 80, 2);
        let (layout, rect) = confirmation_layout(area).unwrap();
        assert_eq!(layout, ConfirmLayout::Line);
        assert_eq!(rect, Rect::new(0, 0, 80, 1));
        let (_, buf) = render(area, &[]);
        assert_eq!(row(&buf, rect, 0).trim_end(), CONFIRM_LINE);
    }

    #[test]
    fn narrow_terminals_wrap_or_fall_back_without_losing_keys() {
        let area = Rect::new(0, 0, 20, 30);
        let (layout, rect) = confirmation_layout(area).unwrap();
        assert_eq!(layout, ConfirmLayout::Full);
        let inner = rect.width - 2;
        assert_eq!(
            rect.height,
            wrapped_rows(&confirmation_text(layout), inner) + 2
        );
        assert!(contained(rect, area));
        let (_, buf) = render(area, &[]);
        let rows = inner_rows(&buf, rect);
        assert_eq!(rows.last().map(|r| r.trim()), Some("clear."));
        assert_eq!(words(&rows.join(" ")), words(&confirmation_text(layout)));

        for area in [
            Rect::new(0, 0, 20, 6),
            Rect::new(0, 0, 12, 4),
            Rect::new(0, 0, 30, 3),
        ] {
            let (layout, rect) = confirmation_layout(area).unwrap();
            assert!(
                matches!(layout, ConfirmLayout::Compact | ConfirmLayout::Line),
                "{area:?} {layout:?}"
            );
            assert!(contained(rect, area), "{area:?} {rect:?}");
            let (_, buf) = render(area, &[]);
            if layout == ConfirmLayout::Compact {
                assert_eq!(
                    words(&inner_rows(&buf, rect).join(" ")),
                    CONFIRM_COMPACT,
                    "{area:?}"
                );
            } else {
                let shown = row(&buf, rect, rect.y);
                assert!(CONFIRM_LINE.starts_with(shown.trim_end()), "{area:?}");
                assert!(shown.starts_with("y: clear"), "{area:?}");
                if rect.width >= 16 {
                    assert!(shown.contains("n/Esc"), "{area:?}");
                }
            }
        }
    }

    #[test]
    fn zero_and_tiny_dimensions_never_panic_or_escape_the_area() {
        for (w, h) in [
            (0, 0),
            (0, 5),
            (5, 0),
            (1, 1),
            (2, 1),
            (3, 3),
            (4, 2),
            (5, 3),
            (6, 3),
            (80, 1),
            (1, 40),
            (2, 40),
            (3, 40),
            (u16::MAX / 4, 1),
        ] {
            let area = Rect::new(3, 2, w, h);
            match confirmation_layout(area) {
                None => assert!(w == 0 || h == 0, "{w}x{h}"),
                Some((_, rect)) => {
                    assert!(w > 0 && h > 0);
                    assert!(contained(rect, area), "{w}x{h} {rect:?}");
                    assert!(rect.width > 0 && rect.height > 0);
                }
            }
            if (w as u32) * (h as u32) <= 20_000 {
                let (before, after) = render(area, &[]);
                assert_eq!(before.area, after.area);
            }
        }
        // A frame area larger than the buffer is clamped to the buffer.
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        render_confirmation(Rect::new(0, 0, 200, 100), &mut buf, true);
        assert_eq!(buf.area, Rect::new(0, 0, 10, 3));
    }

    #[test]
    fn unicode_surrounding_content_outside_the_panel_is_untouched() {
        let fill = [
            "漢字かなカナ 한국어 中文字符 漢字かなカナ 한국어 中文字符 漢字かなカナ 한국어",
            "cafe\u{301} e\u{301}\u{302} a\u{308}o\u{308} combining cafe\u{301} e\u{301}\u{302} a\u{308}",
            "👩\u{200d}👩\u{200d}👧 👨\u{200d}💻 🏳\u{fe0f}\u{200d}🌈 zwj 👩\u{200d}👩\u{200d}👧 👨\u{200d}💻 🏳\u{fe0f}\u{200d}🌈",
        ];
        let fill: Vec<&str> = (0..30).map(|i| fill[i % fill.len()]).collect();
        for area in [
            Rect::new(0, 0, 80, 24),
            Rect::new(0, 0, 40, 24),
            Rect::new(5, 3, 41, 9),
            Rect::new(0, 0, 21, 5),
            Rect::new(0, 0, 80, 2),
        ] {
            let (_, rect) = confirmation_layout(area).unwrap();
            let (before, after) = render(area, &fill);
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    let inside =
                        x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom();
                    if !inside {
                        assert_eq!(before[(x, y)], after[(x, y)], "{area:?} cell {x},{y}");
                    }
                }
            }
            let panel: String = (rect.y..rect.bottom())
                .map(|y| row(&after, rect, y))
                .collect();
            assert!(panel.contains("y:") && panel.contains("Esc"), "{area:?}");
            assert!(!panel.contains('漢') && !panel.contains('👩'), "{area:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_editor::tests::{run_private, Fixture};
    use std::sync::{mpsc, Arc, Mutex};
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl_g() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)
    }
    fn fake(app: &mut App, mode: &str, result: &str) -> Arc<Mutex<Option<std::path::PathBuf>>> {
        let mode = mode.to_string();
        let result = result.to_string();
        let path = Arc::new(Mutex::new(None));
        let saved = path.clone();
        app.external_edit.runner = Some(Box::new(move |file, _| {
            *saved.lock().unwrap() = Some(file.to_path_buf());
            run_private(file, &mode, &result, None)
        }));
        path
    }
    fn editor(text: &str, mode: EditMode) -> EditorState {
        EditorState {
            viewport: Default::default(),
            draft: None,
            original: None,
            confirm_exit: false,
            review_base_version: Some(1),
            vim: vim::Vim::default(),
            tool_id: "private-editor-tool".into(),
            name: "private_editor".into(),
            chars: text.chars().collect(),
            cursor: 0,
            mode,
        }
    }
    fn assert_clean(path: &Arc<Mutex<Option<std::path::PathBuf>>>) {
        let path = path.lock().unwrap().clone().unwrap();
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }
    #[test]
    fn ctrl_g_failure_preserves_collapsed_pastes_cursor_vim_attachments_and_scroll() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        for mode in ["nonzero", "cancel"] {
            let mut app = crate::tests::test_app();
            app.vim_mode = true;
            app.vim_input.mode = vim::Mode::Normal;
            let pasted = "first\nsecond\nthird\nfourth\nfifth\nsixth\nseventh\neighth\nninth界\n";
            app.input.paste(pasted);
            app.input.left();
            app.attachments = vec![("private.txt".into(), vec![1, 2, 3])];
            app.chat_scroll_top = Some(17);
            let display = app.input.to_string();
            let cursor = app.input.cursor_byte();
            let vim = format!("{:?}", app.vim_input);
            let fixture = Fixture::new();
            let record = fixture.0.join("expanded-seed");
            let expected = pasted.to_string();
            let record_copy = record.clone();
            let mode = mode.to_string();
            let mut line = app.input.clone();
            line.modal_expand_pastes();
            let expected_line =
                (line.before_cursor().chars().filter(|&c| c == '\n').count() + 1) as u32;
            app.external_edit.runner = Some(Box::new(move |file, line| {
                assert_eq!(line, Some(expected_line));
                run_private(file, &mode, "discard", Some(&record_copy))
            }));
            app.key(ctrl_g());
            assert_eq!(app.input.to_string(), display);
            assert_eq!(app.input.cursor_byte(), cursor);
            assert_eq!(app.input.expanded(), expected);
            assert_eq!(format!("{:?}", app.vim_input), vim);
            assert_eq!(app.chat_scroll_top, Some(17));
            assert_eq!(app.attachments, vec![("private.txt".into(), vec![1, 2, 3])]);
            assert_eq!(std::fs::read_to_string(record).unwrap(), pasted);
            assert!(app.force_redraw);
            assert!(app.notes.last().unwrap().contains("original buffer kept"));
        }
    }
    #[test]
    fn composer_and_instructions_load_without_automatically_sending_or_saving() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        for instructions in [false, true] {
            let mut app = crate::tests::test_app();
            let (client, sent) = DaemonClient::recording();
            app.client = Some(client);
            app.input.set("original draft");
            app.vim_mode = true;
            app.vim_input.mode = vim::Mode::Normal;
            app.chat_scroll_top = Some(10);
            app.attachments = vec![("held.txt".into(), vec![8])];
            if instructions {
                app.instructing = app.current.clone();
            }
            let path = fake(&mut app, "replace", "edited 界\r\nsecond");
            app.key(ctrl_g());
            assert_clean(&path);
            assert_eq!(app.input.expanded(), "edited 界\nsecond");
            assert_eq!(app.input.cursor_byte(), app.input.len());
            assert_eq!(app.vim_input.mode, vim::Mode::Normal);
            assert_eq!(app.chat_scroll_top, Some(10));
            assert_eq!(app.attachments, vec![("held.txt".into(), vec![8])]);
            assert_eq!(app.instructing.is_some(), instructions);
            assert!(sent.try_recv().is_err());
            if instructions {
                app.submit_chat(false);
                let request: RequestEnvelope =
                    serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
                assert!(
                    matches!(request.request,Request::SetSessionInstructions {instructions:Some(ref text),..} if text=="edited 界\nsecond")
                );
            }
        }
    }
    #[test]
    fn unchanged_crlf_tool_keeps_cursor_vim_and_valid_review() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        let mut app = crate::tests::test_app();
        app.vim_mode = true;
        let mut ed = editor("first\r\nsecond", EditMode::Code);
        ed.cursor = 2;
        ed.vim.mode = vim::Mode::Normal;
        app.editor = Some(ed);
        app.editor_review = Some(EditorReview {
            diff: "valid old diff".into(),
            verdict: "valid checks".into(),
            ok: true,
        });
        let path = fake(&mut app, "unchanged", "");
        app.key(ctrl_g());
        assert_clean(&path);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.text(), "first\r\nsecond");
        assert_eq!(ed.cursor, 2);
        assert_eq!(ed.vim.mode, vim::Mode::Normal);
        assert!(app.editor_review.is_some());
    }
    #[test]
    fn tool_external_edit_generation_guards_equal_text_and_preserves_original_base_version() {
        let _env = credential_env::EnvGuard::isolated();
        let mut app = crate::tests::test_app();
        app.editor = Some(editor("unchanged code", EditMode::Code));
        app.editor.as_mut().unwrap().review_base_version = Some(7);
        let original_generation = app.editor_generation;
        let origin = Origin::capture(&app).unwrap();
        app.apply_external_edit(&origin, "replacement code");
        assert_eq!(app.editor_generation, original_generation.wrapping_add(1));
        assert_eq!(app.editor.as_ref().unwrap().text(), "replacement code");
        assert_eq!(app.editor.as_ref().unwrap().review_base_version, Some(7));
        app.key(KeyEvent::new(KeyCode::Char('_'), KeyModifiers::CONTROL));
        assert_eq!(app.editor.as_ref().unwrap().text(), "unchanged code");
        assert!(app.editor_generation > original_generation.wrapping_add(1));
        app.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::ALT));
        assert_eq!(app.editor.as_ref().unwrap().text(), "replacement code");
        let after_history = app.editor_generation;
        // Reopening an identical buffer cannot revive an earlier operation.
        app.editor = Some(editor("unchanged code", EditMode::Code));
        app.apply_external_edit(&origin, "STALE replacement");
        assert_eq!(app.editor.as_ref().unwrap().text(), "unchanged code");
        assert_eq!(app.editor_generation, after_history);
    }
    #[test]
    fn empty_tool_external_confirmation_requires_both_operation_and_editor_generation() {
        let _env = credential_env::EnvGuard::isolated();
        for change_operation in [false, true] {
            let mut app = crate::tests::test_app();
            app.editor = Some(editor("keep identical code", EditMode::Code));
            let origin = Origin::capture(&app).unwrap();
            app.external_edit.pending = Some(PendingClear {
                origin,
                replacement: String::new(),
            });
            if change_operation {
                app.external_edit.generation = app.external_edit.generation.wrapping_add(1);
            } else {
                app.editor_generation = app.editor_generation.wrapping_add(1);
            }
            assert!(app.external_clear_key(&key(KeyCode::Char('y'))));
            assert!(app.external_edit.pending.is_none());
            assert_eq!(app.editor.as_ref().unwrap().text(), "keep identical code");
            assert_eq!(app.editor.as_ref().unwrap().review_base_version, Some(1));
        }
    }
    #[test]
    fn changed_tool_buffer_discards_queued_old_preview_and_keeps_normal_save_path() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        let mut app = crate::tests::test_app();
        let (client, sent) = DaemonClient::recording();
        let (tx, inbox) = mpsc::channel();
        app.client = Some(client);
        app.inbox = Some(inbox);
        app.editor = Some(editor("function run(){return 1}", EditMode::Code));
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        let preview: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        assert!(app.pending.contains_key(&preview.id));
        app.external_edit.runner = Some(Box::new(move |file, _| {
            let status = run_private(file, "replace", "function run(){return 2}", None)?;
            tx.send(ServerMessage::Response {
                id: preview.id,
                ok: true,
                result: Some(serde_json::json!({"ok":true,"diff":"STALE"})),
                error: None,
            })
            .unwrap();
            Ok(status)
        }));
        app.key(ctrl_g());
        assert_eq!(
            app.editor.as_ref().unwrap().text(),
            "function run(){return 2}"
        );
        assert!(!app.pending.contains_key(&preview.id));
        assert!(!app.pending_since.contains_key(&preview.id));
        app.drain_daemon();
        assert!(app.editor_review.is_none());
        assert!(sent.try_recv().is_err());
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            sent.try_recv().is_ok(),
            "only the ordinary save key may start its existing gate"
        );
    }
    #[test]
    fn empty_result_requires_explicit_confirmation_in_all_three_surfaces() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        for surface in ["composer", "instructions", "tool"] {
            let mut app = crate::tests::test_app();
            let (client, sent) = DaemonClient::recording();
            app.client = Some(client);
            app.input.set("keep original");
            if surface == "instructions" {
                app.instructing = app.current.clone();
            }
            if surface == "tool" {
                app.editor = Some(editor("keep original", EditMode::Code));
            }
            let path = fake(&mut app, "empty", "");
            app.key(ctrl_g());
            assert_clean(&path);
            assert!(app.external_edit.pending.is_some());
            app.key(key(KeyCode::Enter));
            assert!(app.external_edit.pending.is_some());
            assert!(sent.try_recv().is_err());
            let backend = ratatui::backend::TestBackend::new(90, 25);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let screen = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(screen.contains("Clear edited buffer?"));
            app.key(key(KeyCode::Char('n')));
            assert!(app.external_edit.pending.is_none());
            assert_eq!(
                app.editor
                    .as_ref()
                    .map(EditorState::text)
                    .unwrap_or_else(|| app.input.expanded()),
                "keep original"
            );
            app.key(ctrl_g());
            app.key(key(KeyCode::Char('y')));
            assert!(app.external_edit.pending.is_none());
            assert_eq!(
                app.editor
                    .as_ref()
                    .map(EditorState::text)
                    .unwrap_or_else(|| app.input.expanded()),
                ""
            );
            assert!(sent.try_recv().is_err());
        }
    }
    #[test]
    fn empty_confirmation_rejects_changed_session_or_buffer_and_ctrl_c_does_not_interrupt() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        let mut app = crate::tests::test_app();
        let (client, sent) = DaemonClient::recording();
        app.client = Some(client);
        app.input.set("old draft");
        app.running_sessions.insert("s1".into());
        fake(&mut app, "empty", "");
        app.key(ctrl_g());
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.input.expanded(), "old draft");
        assert!(sent.try_recv().is_err());
        app.key(ctrl_g());
        app.current = Some("different-session".into());
        app.input.set("new Session draft");
        app.key(key(KeyCode::Char('y')));
        assert_eq!(app.input.expanded(), "new Session draft");
        assert!(app.external_edit.pending.is_none());
        app.key(ctrl_g());
        app.input.set("changed while confirmation waited");
        app.key(key(KeyCode::Char('y')));
        assert_eq!(app.input.expanded(), "changed while confirmation waited");
        assert!(sent.try_recv().is_err());
    }
    #[test]
    fn slash_editor_and_history_search_ownership_use_the_correct_surface() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        let mut app = crate::tests::test_app();
        app.input.set("draft");
        let path = fake(&mut app, "replace", "slash result");
        app.slash("editor");
        assert_eq!(app.input.expanded(), "slash result");
        assert_clean(&path);
        app.input.set("history query");
        app.input.search_older(&[] as &[&str]);
        let calls = Arc::new(Mutex::new(0));
        let counted = calls.clone();
        app.external_edit.runner = Some(Box::new(move |_, _| {
            *counted.lock().unwrap() += 1;
            anyhow::bail!("must not invoke editor from history search")
        }));
        app.key(ctrl_g());
        assert!(app.input.searching().is_none());
        assert_eq!(*calls.lock().unwrap(), 0);
    }
    #[test]
    fn file_types_and_pending_save_guard_do_not_bypass_the_editor_workflow() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        for (mode, extension) in [
            (EditMode::Code, "js"),
            (EditMode::Module, "js"),
            (EditMode::Description, "md"),
            (EditMode::Run, "json"),
            (EditMode::Schemas, "json"),
        ] {
            let mut app = crate::tests::test_app();
            app.editor = Some(editor("seed", mode));
            app.external_edit.runner = Some(Box::new(move |file, _| {
                assert_eq!(file.extension().unwrap(), extension);
                run_private(file, "replace", "changed", None)
            }));
            app.key(ctrl_g());
            assert_eq!(app.editor.as_ref().unwrap().mode, mode);
            assert_eq!(app.editor.as_ref().unwrap().text(), "changed");
            assert_eq!(app.editor.as_ref().unwrap().cursor, 7);
        }
        let mut app = crate::tests::test_app();
        app.editor = Some(editor("pending save", EditMode::Code));
        app.pending.insert(42, Pending::UpdateTool);
        let calls = Arc::new(Mutex::new(0));
        let counted = calls.clone();
        app.external_edit.runner = Some(Box::new(move |_, _| {
            *counted.lock().unwrap() += 1;
            anyhow::bail!("must wait for save")
        }));
        app.key(ctrl_g());
        assert_eq!(*calls.lock().unwrap(), 0);
        assert_eq!(app.editor.as_ref().unwrap().text(), "pending save");
        assert!(app.notes.last().unwrap().contains("pending editor save"));
    }
}

#[cfg(test)]
mod queued_tests {
    use super::*;
    use crate::external_editor::tests::run_private;
    use std::sync::mpsc;
    fn ctrl_g() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)
    }
    #[test]
    fn queued_forecast_confirmation_cannot_send_even_when_editor_returns_unchanged_or_failed() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        for mode in ["unchanged", "nonzero", "empty", "replace"] {
            let mut app = crate::tests::test_app();
            app.input.set("original pending draft");
            let (client, sent) = DaemonClient::recording();
            let (tx, inbox) = mpsc::channel();
            app.client = Some(client);
            app.inbox = Some(inbox);
            let generation = app.forecast_generation();
            app.pending.insert(
                73,
                Pending::ForecastConfirm {
                    generation,
                    action: ForecastAction::Send { queued: false },
                },
            );
            app.pending_since.insert(73, Instant::now());
            let mode = mode.to_string();
            app.external_edit.runner = Some(Box::new(move |file, _| {
                let status = run_private(file, &mode, "edited draft", None)?;
                tx.send(ServerMessage::Response {
                    id: 73,
                    ok: true,
                    result: Some(serde_json::json!({})),
                    error: None,
                })
                .unwrap();
                Ok(status)
            }));
            app.key(ctrl_g());
            app.drain_daemon();
            assert!(
                sent.try_recv().is_err(),
                "editor handoff must invalidate the old send intent before any queued reply"
            );
            assert!(!app.input.is_empty());
        }
    }
    #[test]
    fn queued_session_deletion_stashes_the_edited_draft_and_rejects_empty_consent() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        for empty in [false, true] {
            let mut app = crate::tests::test_app();
            app.input.set("original draft");
            let (client, sent) = DaemonClient::recording();
            let (tx, inbox) = mpsc::channel();
            app.client = Some(client);
            app.inbox = Some(inbox);
            app.external_edit.runner = Some(Box::new(move |file, _| {
                let status = run_private(
                    file,
                    if empty { "empty" } else { "replace" },
                    "edited surviving draft",
                    None,
                )?;
                tx.send(ServerMessage::Event(Event::SessionDeleted {
                    id: "s1".into(),
                }))
                .unwrap();
                Ok(status)
            }));
            app.key(ctrl_g());
            app.drain_daemon();
            assert!(app.current.is_none());
            assert_eq!(
                app.drafts.get("s1").unwrap().input.expanded(),
                if empty {
                    "original draft"
                } else {
                    "edited surviving draft"
                }
            );
            app.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
            assert!(app.external_edit.pending.is_none());
            assert_eq!(
                app.drafts.get("s1").unwrap().input.expanded(),
                if empty {
                    "original draft"
                } else {
                    "edited surviving draft"
                }
            );
            assert!(sent.try_recv().is_err());
        }
    }
    #[test]
    fn superseded_queued_tool_input_reply_cannot_replace_the_active_edited_buffer() {
        let env = credential_env::EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "private-editor-dummy");
        let mut app = crate::tests::test_app();
        let (client, sent) = DaemonClient::recording();
        let (tx, inbox) = mpsc::channel();
        app.client = Some(client);
        app.inbox = Some(inbox);
        app.input.set("keep composing");
        app.pending.insert(81, Pending::RunToolInput);
        app.pending_since.insert(81, Instant::now());
        let detail = crate::tests::description_fixture_detail();
        app.external_edit.runner = Some(Box::new(move |file, _| {
            let status = run_private(file, "replace", "edited composer", None)?;
            tx.send(ServerMessage::Response {
                id: 81,
                ok: true,
                result: Some(serde_json::to_value(&detail).unwrap()),
                error: None,
            })
            .unwrap();
            Ok(status)
        }));
        app.key(ctrl_g());
        app.drain_daemon();
        assert_eq!(app.input.expanded(), "edited composer");
        assert!(app.editor.is_none());
        assert!(!app.pending.contains_key(&81));
        assert!(!app.pending_since.contains_key(&81));
        assert!(sent.try_recv().is_err());
    }
}
