//! Feasibility study: word-break opportunities for Thai text.
//!
//! GPUI 0.3.12 builds its line breaker without a model for Thai, so a run of
//! Thai has no break opportunity inside it and a line breaks wherever it
//! fills. Thai is written without spaces between words; where a line may
//! break has to come from a dictionary.
//!
//! This module finds Thai word boundaries with ICU's dictionary segmenter
//! (data that `icu_segmenter` already ships and the GPUI layer already links)
//! and marks them with U+200B ZERO WIDTH SPACE, which every line breaker
//! treats as a break opportunity and which has no width.
//!
//! It is not wired into the shell. Doing that properly needs two things this
//! study does not provide: inserting only into prose (never into code, link
//! targets or anything else where an extra character changes meaning), and
//! taking the marks out again when text is copied.

use std::borrow::Cow;

use icu_segmenter::WordSegmenter;

pub const ZERO_WIDTH_SPACE: char = '\u{200B}';

fn is_thai(c: char) -> bool {
    ('\u{0E00}'..='\u{0E7F}').contains(&c)
}

/// Byte offsets where one Thai word ends and the next begins. Only
/// boundaries with Thai on both sides are returned: the edges of a Thai run
/// already break (at a space, punctuation or another script).
pub fn word_boundaries(text: &str) -> Vec<usize> {
    if !text.chars().any(is_thai) {
        return Vec::new();
    }
    WordSegmenter::new_dictionary(Default::default())
        .segment_str(text)
        .filter(|&offset| {
            let before = text[..offset].chars().next_back();
            let after = text[offset..].chars().next();
            before.is_some_and(is_thai) && after.is_some_and(is_thai)
        })
        .collect()
}

/// `text` with a zero width space at every Thai word boundary. Text with no
/// Thai in it is returned as it is, without allocating.
pub fn with_break_opportunities(text: &str) -> Cow<'_, str> {
    let boundaries = word_boundaries(text);
    if boundaries.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut marked = String::with_capacity(text.len() + boundaries.len() * 3);
    let mut start = 0;
    for boundary in boundaries {
        marked.push_str(&text[start..boundary]);
        marked.push(ZERO_WIDTH_SPACE);
        start = boundary;
    }
    marked.push_str(&text[start..]);
    Cow::Owned(marked)
}

/// Remove zero width spaces that sit between two Thai letters: the inverse
/// of [`with_break_opportunities`] for text that had none there to begin
/// with. A zero width space the author put between Thai letters is removed
/// too; the two cannot be told apart afterwards.
pub fn without_break_opportunities(text: &str) -> Cow<'_, str> {
    if !text.contains(ZERO_WIDTH_SPACE) {
        return Cow::Borrowed(text);
    }
    let chars: Vec<char> = text.chars().collect();
    let kept: String = chars
        .iter()
        .enumerate()
        .filter(|(index, c)| {
            let between_thai = *index > 0
                && is_thai(chars[index - 1])
                && chars.get(index + 1).copied().is_some_and(is_thai);
            !(**c == ZERO_WIDTH_SPACE && between_thai)
        })
        .map(|(_, c)| *c)
        .collect();
    Cow::Owned(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    fn words(text: &str) -> Vec<&str> {
        let mut edges = vec![0];
        edges.extend(word_boundaries(text));
        edges.push(text.len());
        edges
            .windows(2)
            .map(|pair| &text[pair[0]..pair[1]])
            .collect()
    }

    #[test]
    fn common_thai_phrases_split_into_their_words() {
        assert_eq!(words("สวัสดีครับ"), ["สวัสดี", "ครับ"]);
        assert_eq!(words("ภาษาไทย"), ["ภาษา", "ไทย"]);
        let sentence = "ภาษาไทยไม่มีช่องว่างระหว่างคำ";
        let split = words(sentence);
        println!("EVIDENCE thai_words: {}", split.join(" | "));
        assert!(split.len() >= 6, "{split:?}");
        assert_eq!(split.concat(), sentence);
    }

    #[test]
    fn a_boundary_never_falls_inside_a_character() {
        let samples = [
            "ช่วยสรุปค่าใช้จ่ายของเดือนนี้ให้หน่อย แล้วบอกด้วยว่าหมวดไหนเกินงบมากที่สุด",
            "ข้อความอ้างอิงภาษาไทยที่ยาวพอจะตัดบรรทัดได้หลายครั้งเมื่อหน้าต่างแคบลงเพื่อทดสอบการตัดคำ",
            "วางแผนทริปเชียงใหม่ช่วงปีใหม่",
            "น้ำ ก็ ที่ ผู้ เปิ้ล",
        ];
        for sample in samples {
            let graphemes: std::collections::HashSet<usize> = sample
                .grapheme_indices(true)
                .map(|(offset, _)| offset)
                .collect();
            let boundaries = word_boundaries(sample);
            assert!(
                !boundaries.is_empty() || !sample.contains("ภาษา"),
                "{sample}"
            );
            for boundary in boundaries {
                assert!(sample.is_char_boundary(boundary));
                // Never between a consonant and its vowel or tone mark.
                assert!(graphemes.contains(&boundary), "{sample} at {boundary}");
            }
        }
    }

    #[test]
    fn text_without_thai_is_returned_untouched_and_unallocated() {
        for text in ["", "plain English text", "日本語のテスト", "한국어 👩🏽‍💻"]
        {
            assert!(word_boundaries(text).is_empty());
            assert!(matches!(with_break_opportunities(text), Cow::Borrowed(same) if same == text));
        }
    }

    #[test]
    fn only_the_inside_of_thai_runs_is_marked_and_the_marks_come_out_again() {
        let text = "Summary: สวัสดีครับ and `code` then ภาษาไทย.";
        let marked = with_break_opportunities(text);
        assert_eq!(
            marked,
            format!(
                "Summary: สวัสดี{ZERO_WIDTH_SPACE}ครับ and `code` then ภาษา{ZERO_WIDTH_SPACE}ไทย."
            )
        );
        // Nothing is added at the edge of a run or anywhere in other scripts.
        assert_eq!(marked.matches(ZERO_WIDTH_SPACE).count(), 2);
        assert_eq!(without_break_opportunities(&marked), text);
        // Unmarked text passes through.
        assert!(matches!(
            without_break_opportunities(text),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_zero_width_space_outside_thai_is_left_alone() {
        let text = format!("a{ZERO_WIDTH_SPACE}b สวัสดี{ZERO_WIDTH_SPACE}ครับ");
        assert_eq!(
            without_break_opportunities(&text),
            format!("a{ZERO_WIDTH_SPACE}b สวัสดีครับ")
        );
    }
}
