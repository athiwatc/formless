//! Work after the existing bounded artifact read; whole-text decoding and
//! total counting remain necessary. The returned page borrows decoded text.

pub(crate) struct Page<'a> {
    pub total: usize,
    pub selected: usize,
    pub text: &'a str,
}

pub(crate) fn page(text: &str, offset: usize, length: usize) -> Page<'_> {
    let (mut total, mut selected) = (0, 0);
    let (mut start, mut end) = (text.len(), text.len());
    for (index, (byte, ch)) in text.char_indices().enumerate() {
        if index == offset {
            start = byte;
            end = byte;
        }
        if index >= offset && selected < length {
            end = byte + ch.len_utf8();
            selected += 1;
        }
        total += 1;
    }
    Page {
        total,
        selected,
        text: &text[start..end],
    }
}

/// The needle is already lowercased once by the caller. Unicode keeps the
/// original whole-line lowercase semantics (including contextual mappings).
pub(crate) fn contains_lowercase(line: &str, needle: &str) -> bool {
    // Keep the standard library's efficient substring search for repeated
    // prefixes; comparing a full byte window at each offset is quadratic.
    line.to_lowercase().contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn decoded_pages_match_previous_scalar_selection_and_borrowing() {
        for bytes in [
            b"".as_slice(),
            b"ASCII\r\nlast\n",
            "ไทย 日本語 e\u{301} 🦀".as_bytes(),
            b"a\xffb\xf0\x9f\x92",
        ] {
            let text = String::from_utf8_lossy(bytes);
            assert_eq!(
                matches!(text, Cow::Borrowed(_)),
                std::str::from_utf8(bytes).is_ok()
            );
            let total = text.chars().count();
            for offset in [0, 1, 2, total, total + 1, usize::MAX] {
                for length in [0, 1, 3, 100, usize::MAX] {
                    let expected: String = text.chars().skip(offset).take(length).collect();
                    let got = page(&text, offset, length);
                    assert_eq!(got.total, total);
                    assert_eq!(got.selected, expected.chars().count());
                    assert_eq!(got.text, expected);
                    assert_eq!(offset + got.selected, offset + expected.chars().count());
                    assert_eq!(
                        (offset + got.selected).min(got.total),
                        (offset + expected.chars().count()).min(total)
                    );
                }
            }
        }
    }

    #[test]
    fn repeated_prefix_grep_remains_practical() {
        let line = "A".repeat(1_000_000);
        let needle = "a".repeat(10_000) + "b";
        assert!(!contains_lowercase(&line, &needle));
        assert!(contains_lowercase(&(line + "B"), &needle));
    }

    #[test]
    fn ascii_and_unicode_grep_match_whole_line_lowercase() {
        let source = "NeedLE\r\nİSTANBUL\nΟΣ\nος\n日本語\n".to_string()
            + &"LongNeedle ".repeat(600)
            + "\n"
            + &"needle\n".repeat(205);
        for needle in ["", "NEEDLE", "İ", "i\u{307}", "ΟΣ", "Σ", "日本", "missing"] {
            let needle = needle.to_lowercase();
            let old = source
                .lines()
                .enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&needle))
                .take(200)
                .map(|(i, line)| (i + 1, line.chars().take(500).collect::<String>()))
                .collect::<Vec<_>>();
            let new = source
                .lines()
                .enumerate()
                .filter(|(_, line)| contains_lowercase(line, &needle))
                .take(200)
                .map(|(i, line)| (i + 1, line.chars().take(500).collect::<String>()))
                .collect::<Vec<_>>();
            assert_eq!(new, old);
        }
    }
}
