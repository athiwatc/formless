//! A whole block as display rows, for the full-output view. No GPUI.
//!
//! A timeline row shows at most a bounded window of its block (findings
//! §12.1). The full-output view shows all of it, as plain text in rows of
//! at most `columns` characters, in a list that lays out only the rows on
//! screen. A row ends at a line break or after `columns` characters, so a
//! five-megabyte line is many short rows, never one row of five megabytes.
//! Characters are grapheme clusters: a row never cuts one in two.
//!
//! The only thing kept is where each row starts, four bytes a row, made
//! when the view opens and again when its width changes, and dropped when
//! it closes. A row's text is a slice of the block, found when the row is
//! drawn. There is at most one row per byte, plus one (a block of nothing
//! but line breaks), so the index is at most four times the block's size,
//! plus four bytes.

use unicode_segmentation::UnicodeSegmentation as _;

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
    let row = row.strip_suffix('\n').unwrap_or(row);
    row.strip_suffix('\r').unwrap_or(row)
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
