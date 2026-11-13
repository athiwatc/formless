//! A whole block as display rows, for the full-output view. No GPUI.
//!
//! The shell uses [`measured_row_starts`]: a bounded candidate is shaped by
//! the caller's actual font system and shortened on grapheme boundaries
//! until it fits. Character count is only a candidate-size hint, never a
//! substitute for fallback glyph widths. [`row_starts`] is the simpler
//! character-count utility, useful with genuinely fixed-cell text.
//!
//! Only row starts are retained, four bytes per row, at most four times
//! the block's byte size plus four bytes. Rows borrow the original block.
//! Shaping candidates are bounded by bytes and graphemes. If one grapheme
//! cannot fit those bounds or the viewport, a named error lets the view
//! explain the limit and keep Copy all available, rather than clip text.

use unicode_segmentation::UnicodeSegmentation as _;

/// Maximum text handed to one width measurement (never a whole huge line).
pub const MEASURE_MAX_BYTES: usize = 8 * 1024;
pub const MEASURE_MAX_GRAPHEMES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowError {
    TooLarge,
    GraphemeTooLarge,
    GraphemeTooWide,
}

impl RowError {
    pub fn notice(self) -> &'static str {
        match self {
            Self::TooLarge => "This block is too large to index. Use Copy all.",
            Self::GraphemeTooLarge => {
                "This block contains a character too large to lay out safely. Use Copy all."
            }
            Self::GraphemeTooWide => {
                "A character is wider than this view. Enlarge the window or use Copy all."
            }
        }
    }
}

/// A width-checked row index. `columns` is an upper-bound hint for the
/// number of graphemes in a candidate; `fits` must measure the whole given
/// row, in the same font and available width as the view. At most
/// [`MEASURE_MAX_BYTES`] and [`MEASURE_MAX_GRAPHEMES`] are measured at once.
/// Newlines are never passed to it. CRLF and a final CR are line endings,
/// as in [`row`]; an internal bare CR is left for the font system to handle.
///
/// Prefix widths need not be monotonic (contextual shaping and ligatures).
/// The search may underfill a row, but every chosen prefix has actually
/// passed `fits`. If even the first grapheme does not fit, return an error
/// rather than silently clip it or make no forward progress.
pub fn measured_row_starts<'a>(
    text: &'a str,
    columns: usize,
    mut fits: impl FnMut(&'a str) -> bool,
) -> Result<Box<[u32]>, RowError> {
    u32::try_from(text.len()).map_err(|_| RowError::TooLarge)?;
    let columns = columns.clamp(1, MEASURE_MAX_GRAPHEMES);
    let mut starts = Vec::new();
    let mut ends = Vec::with_capacity(columns);
    let mut line_start = 0;
    loop {
        let line_end = text[line_start..]
            .find('\n')
            .map_or(text.len(), |at| line_start + at);
        let line = &text[line_start..line_end];
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut at = 0;
        starts.push(line_start as u32);
        while at < line.len() {
            ends.clear();
            for (offset, grapheme) in line[at..].grapheme_indices(true).take(columns) {
                let end = offset + grapheme.len();
                if end > MEASURE_MAX_BYTES {
                    if ends.is_empty() {
                        return Err(RowError::GraphemeTooLarge);
                    }
                    break;
                }
                ends.push(end);
            }
            let mut count = ends.len();
            if !fits(&line[at..at + ends[count - 1]]) {
                if count == 1 || !fits(&line[at..at + ends[0]]) {
                    return Err(RowError::GraphemeTooWide);
                }
                // low is a measured fitting prefix; high did not fit.
                let mut low = 1;
                let mut high = count;
                while low + 1 < high {
                    let mid = low + (high - low) / 2;
                    if fits(&line[at..at + ends[mid - 1]]) {
                        low = mid;
                    } else {
                        high = mid;
                    }
                }
                count = low;
            }
            at += ends[count - 1];
            if at < line.len() {
                starts.push((line_start + at) as u32);
            }
        }
        if line_end == text.len() {
            break;
        }
        line_start = line_end + 1;
    }
    Ok(starts.into_boxed_slice())
}

/// Where each display row of `text` starts, as byte offsets, or `None` if
/// the text is too long for four-byte offsets (over 4 GiB; no block is).
/// Empty text is one empty row.
pub fn row_starts(text: &str, columns: usize) -> Option<Vec<u32>> {
    u32::try_from(text.len()).ok()?;
    let columns = columns.max(1);
    let mut starts = Vec::new();
    let mut line_start = 0;
    loop {
        let line_end = text[line_start..]
            .find('\n')
            .map_or(text.len(), |at| line_start + at);
        let line = &text[line_start..line_end];
        let line = line.strip_suffix('\r').unwrap_or(line);
        // Offsets fit: the whole text does.
        starts.push(line_start as u32);
        for (index, (offset, _)) in line.grapheme_indices(true).enumerate() {
            if index > 0 && index % columns == 0 {
                starts.push((line_start + offset) as u32);
            }
        }
        if line_end == text.len() {
            break;
        }
        line_start = line_end + 1;
    }
    Some(starts)
}

/// The text of row `index`, without its line break.
pub fn row<'a>(text: &'a str, starts: &[u32], index: usize) -> &'a str {
    let start = starts[index] as usize;
    let end = starts
        .get(index + 1)
        .map_or(text.len(), |next| *next as usize);
    let row = &text[start..end];
    if let Some(line) = row.strip_suffix('\n') {
        line.strip_suffix('\r').unwrap_or(line)
    } else if end == text.len() {
        row.strip_suffix('\r').unwrap_or(row)
    } else {
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str, columns: usize) -> Vec<&str> {
        let starts = row_starts(text, columns).unwrap();
        (0..starts.len())
            .map(|index| row(text, &starts, index))
            .collect()
    }

    /// Deliberately not fixed-cell: CJK/emoji and tabs need more room than
    /// Latin, while combining sequences stay one indivisible character.
    fn width(text: &str) -> usize {
        text.graphemes(true)
            .map(|g| match g {
                "漢" | "字" | "👩🏽‍💻" | "🇹🇭" => 2,
                "\t" => 4,
                "\r" => 0,
                _ => 1,
            })
            .sum()
    }

    fn measured(text: &str, columns: usize, available: usize) -> Vec<&str> {
        let starts = measured_row_starts(text, columns, |row| width(row) <= available).unwrap();
        let rows: Vec<_> = (0..starts.len()).map(|i| row(text, &starts, i)).collect();
        assert!(rows.iter().all(|row| width(row) <= available));
        rows
    }

    #[test]
    fn measured_rows_fit_wide_glyphs_instead_of_treating_them_as_ascii_cells() {
        let text = "ab漢字👩🏽‍💻🇹🇭e\u{301}ก้่ำ";
        let shown = measured(text, 20, 4);
        assert_eq!(shown.concat(), text);
        let counted = rows(text, 4);
        assert!(counted.iter().any(|row| width(row) > 4));
        assert!(shown.len() > counted.len());
        assert!(shown
            .iter()
            .all(|row| text.graphemes(true).any(|g| row.starts_with(g))));
        assert_eq!(measured("漢字漢字", 100, 4), ["漢字", "漢字"]);
    }

    #[test]
    fn measured_rows_keep_tabs_bidi_combining_marks_and_crlf() {
        let text = "a\tb\r\n\nالعربية ก้่ำ e\u{301}\rinside\nend\r";
        let shown = measured(text, 40, 7);
        assert_eq!(shown.concat(), "a\tbالعربية ก้่ำ e\u{301}\rinsideend");
        // A soft wrap after bare CR must not delete it as though it were CRLF.
        assert_eq!(rows("ab\rcd", 3), ["ab\r", "cd"]);
        assert_eq!(measured("ab\rcd", 3, 3), ["ab\r", "cd"]);
        assert_eq!(measured("\r\n\n", 2, 2), ["", "", ""]);
    }

    #[test]
    fn measured_rows_use_actual_width_even_when_the_character_hint_is_unchanged() {
        let text = "漢字漢字";
        assert_eq!(measured(text, 10, 8), [text]);
        assert_eq!(measured(text, 10, 4), ["漢字", "漢字"]);
        assert_eq!(measured(text, 0, 2), ["漢", "字", "漢", "字"]);
        assert_eq!(measured("", 0, 0), [""]);
        assert_eq!(
            measured_row_starts("漢", 10, |_| false),
            Err(RowError::GraphemeTooWide)
        );
    }

    #[test]
    fn measured_candidates_are_bounded_and_huge_graphemes_fail_before_shaping() {
        let text = "ก้่ำ漢👩🏽‍💻".repeat(2000);
        let starts = measured_row_starts(&text, usize::MAX, |candidate| {
            assert!(candidate.len() <= MEASURE_MAX_BYTES);
            assert!(candidate.graphemes(true).count() <= MEASURE_MAX_GRAPHEMES);
            true
        })
        .unwrap();
        assert_eq!(
            (0..starts.len())
                .map(|i| row(&text, &starts, i))
                .collect::<String>(),
            text
        );
        let giant = "e".to_string() + &"\u{301}".repeat(MEASURE_MAX_BYTES);
        assert_eq!(
            measured_row_starts(&giant, 20, |_| panic!("must not shape a huge cluster")),
            Err(RowError::GraphemeTooLarge)
        );
    }

    #[test]
    fn contextual_nonmonotonic_prefixes_are_never_assumed_to_fit() {
        let text = "abcdefghijklmno";
        let fits = |s: &str| matches!(s.len(), 1 | 2 | 4 | 7);
        let starts = measured_row_starts(text, 8, fits).unwrap();
        let shown: Vec<_> = (0..starts.len()).map(|i| row(text, &starts, i)).collect();
        assert_eq!(shown.concat(), text);
        assert!(shown.iter().all(|s| fits(s)));
    }

    #[test]
    fn five_megabytes_of_heterogeneous_text_is_measured_in_bounded_rows() {
        let text: String = (0..5 * 1024 * 1024)
            .map(|i| char::from(b'!' + ((i * 17 + i / 83) % 90) as u8))
            .collect();
        let mut calls = 0;
        let starts = measured_row_starts(&text, 120, |candidate| {
            calls += 1;
            assert!(candidate.len() <= 120);
            candidate.len() <= 120
        })
        .unwrap();
        assert_eq!(starts.len(), text.len().div_ceil(120));
        assert_eq!(calls, starts.len());
        assert_eq!(
            (0..starts.len())
                .map(|i| row(&text, &starts, i))
                .collect::<String>(),
            text
        );
    }

    #[test]
    fn rows_end_at_line_breaks_and_at_the_column_limit() {
        assert_eq!(rows("", 4), [""]);
        assert_eq!(rows("abc", 4), ["abc"]);
        assert_eq!(rows("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(rows("ab\ncd\n", 4), ["ab", "cd", ""]);
        assert_eq!(rows("ab\r\n\r\ncd", 4), ["ab", "", "cd"]);
        assert_eq!(rows("abcd\nefgh", 4), ["abcd", "efgh"]);
        // A width of zero is treated as one, not as no rows.
        assert_eq!(rows("ab", 0), ["a", "b"]);
    }

    #[test]
    fn a_row_never_cuts_a_character_in_two() {
        // Thai with combining marks, an emoji ZWJ sequence, a flag, é as
        // e + combining acute: each is one character on screen.
        let text = "ก้่ำ👩🏽‍💻🇹🇭e\u{301}x";
        for columns in 1..=6 {
            let shown = rows(text, columns);
            assert_eq!(shown.concat(), text, "{columns}");
            for row in &shown {
                assert!(row.graphemes(true).count() <= columns, "{columns}: {row:?}");
            }
        }
        // SARA AM joins the cluster before it: ก้่ำ is one character.
        assert_eq!(rows(text, 2), ["ก้่ำ👩🏽‍💻", "🇹🇭e\u{301}", "x"]);
    }

    #[test]
    fn every_byte_but_the_line_breaks_is_shown_once_in_order() {
        let text = "first line\r\n\nข้อความไทย ".repeat(50) + &"漢".repeat(1000);
        for columns in [1, 7, 80, 4096] {
            let shown = rows(&text, columns);
            let expected: String = text.replace("\r\n", "").replace('\n', "");
            assert_eq!(shown.concat(), expected, "{columns}");
            let starts = row_starts(&text, columns).unwrap();
            assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(starts
                .iter()
                .all(|start| text.is_char_boundary(*start as usize)));
        }
    }

    #[test]
    fn a_five_megabyte_line_is_many_short_rows() {
        let text = "x".repeat(5 * 1024 * 1024);
        let starts = row_starts(&text, 120).unwrap();
        assert_eq!(starts.len(), text.len().div_ceil(120));
        assert_eq!(
            row(&text, &starts, starts.len() - 1).len(),
            text.len() % 120
        );
        // Four bytes a row.
        assert!(starts.capacity() * 4 <= text.len());
    }

    #[test]
    fn the_worst_case_index_is_four_bytes_a_byte_plus_one_row() {
        for text in ["\n".repeat(1024 * 1024), "a".repeat(1024 * 1024)] {
            let starts = row_starts(&text, 1).unwrap();
            assert_eq!(
                starts.len(),
                text.len() + 1 - usize::from(!text.starts_with('\n'))
            );
            assert!(starts.len() * 4 <= text.len() * 4 + 4);
        }
    }
}
