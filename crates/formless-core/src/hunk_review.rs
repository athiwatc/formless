//! Bounded exact inverse edits. Paths are display labels only: this engine never
//! opens a file, executes a patch, or accepts arbitrary replacement text.
use anyhow::{bail, Result};
use formless_protocol::{ReviewFile, ReviewHunk, ReviewLine, ReviewTextSpan};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_REVIEW_BYTES: usize = 256 * 1024;
const MAX_LINE_PAIRS: usize = 1_000_000;
const MAX_WORD_PAIRS: usize = 65_536;
const MAX_DOCUMENT_WORD_PAIRS: usize = 1_000_000;
const MAX_ENCODED_REVIEW: usize = 2 * 1024 * 1024;
const MAX_REVIEW_LINES: usize = 8192;
const CONTEXT: usize = 3;

/// A missing file is not an empty file. Hashes are meaningful only as identities,
/// never authorization to write a client-supplied path.
pub fn image_hash(image: Option<&[u8]>) -> String {
    let mut hash = Sha256::new();
    match image {
        None => hash.update(b"formless-review-missing-v1"),
        Some(bytes) => {
            hash.update(b"formless-review-present-v1\0");
            hash.update(bytes);
        }
    }
    format!("{:x}", hash.finalize())
}
#[derive(Clone)]
struct RawHunk {
    id: String,
    old: std::ops::Range<usize>,
    new: std::ops::Range<usize>,
}
/// Immutable initial comparison; server-owned decisions compose inverses against
/// its initial after-image, so rejecting later hunks never uses shifted offsets.
#[derive(Clone)]
pub struct ReviewDocument {
    view: ReviewFile,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    raw: Vec<RawHunk>,
}
#[derive(Clone, Copy)]
struct Line<'a> {
    body: &'a str,
    ending: &'a str,
    start: usize,
    end: usize,
}
#[derive(Clone, Copy)]
enum Op {
    Same(usize, usize),
    Remove(usize),
    Add(usize),
}

impl ReviewDocument {
    pub fn build(path: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> Self {
        let before_hash = image_hash(before);
        let after_hash = image_hash(after);
        let id = digest_parts(&[
            path.as_bytes(),
            before_hash.as_bytes(),
            after_hash.as_bytes(),
        ]);
        let status = match (before, after) {
            (None, Some(_)) => "created",
            (Some(_), None) => "removed",
            _ if before == after => "unchanged",
            _ => "modified",
        };
        let mut document = Self {
            view: ReviewFile {
                id,
                path: path.into(),
                status: status.into(),
                before_hash,
                after_hash,
                hunks: Vec::new(),
                exclusion_reason: None,
                warnings: Vec::new(),
            },
            before: None,
            after: None,
            raw: Vec::new(),
        };
        // Never copy an unbounded/binary input into server review state.
        let reason = if before
            .into_iter()
            .chain(after)
            .any(|bytes| bytes.len() > MAX_REVIEW_BYTES)
        {
            Some("File exceeds the 256 KiB per-image interactive review limit")
        } else if generated(path, before, after) {
            Some("Generated file: listed for review, not hunk-editable")
        } else if before
            .into_iter()
            .chain(after)
            .any(|bytes| bytes.contains(&0) || std::str::from_utf8(bytes).is_err())
        {
            Some("Binary or non-UTF-8 file: not hunk-editable")
        } else {
            None
        };
        if let Some(reason) = reason {
            document.view.exclusion_reason = Some(reason.into());
            return document;
        }
        document.before = before.map(<[u8]>::to_vec);
        document.after = after.map(<[u8]>::to_vec);
        if before == after {
            return document;
        }
        let a = lines(std::str::from_utf8(before.unwrap_or_default()).expect("checked UTF-8"));
        let b = lines(std::str::from_utf8(after.unwrap_or_default()).expect("checked UTF-8"));
        if a.len().saturating_add(b.len()) > MAX_REVIEW_LINES {
            document.view.exclusion_reason =
                Some("File exceeds the 8192-line combined interactive review limit".into());
            document.before = None;
            document.after = None;
            return document;
        }
        let ah: Vec<_> = a
            .iter()
            .map(|line| digest_parts(&[line.body.as_bytes(), line.ending.as_bytes()]))
            .collect();
        let bh: Vec<_> = b
            .iter()
            .map(|line| digest_parts(&[line.body.as_bytes(), line.ending.as_bytes()]))
            .collect();
        let operations = if a.len().saturating_mul(b.len()) > MAX_LINE_PAIRS {
            document.view.warnings.push("Line alignment limit reached: one exact replacement hunk; no unchanged matches claimed.".into());
            replacement(a.len(), b.len())
        } else {
            let ops = align(&ah, &bh);
            if ops.iter().any(|op|matches!(op,Op::Same(i,j) if a[*i].body!=b[*j].body || a[*i].ending!=b[*j].ending)) {replacement(a.len(),b.len())}else{ops}
        };
        let edits: Vec<_> = operations
            .iter()
            .enumerate()
            .filter_map(|(i, op)| (!matches!(op, Op::Same(..))).then_some(i))
            .collect();
        let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
        for edit in edits {
            match groups.last_mut() {
                Some(group) if edit - group.end < CONTEXT * 2 => group.end = edit + 1,
                _ => groups.push(edit..edit + 1),
            }
        }
        // Empty-file creation/removal has an existence edit with no text lines.
        if groups.is_empty() {
            groups.push(0..0);
        }
        let mut word_budget = MAX_DOCUMENT_WORD_PAIRS;
        for group in groups {
            let old_start = operations[..group.start]
                .iter()
                .filter(|op| !matches!(op, Op::Add(..)))
                .count();
            let new_start = operations[..group.start]
                .iter()
                .filter(|op| !matches!(op, Op::Remove(..)))
                .count();
            let old_count = operations[group.clone()]
                .iter()
                .filter(|op| !matches!(op, Op::Add(..)))
                .count();
            let new_count = operations[group.clone()]
                .iter()
                .filter(|op| !matches!(op, Op::Remove(..)))
                .count();
            let old = offset(&a, old_start)..offset(&a, old_start + old_count);
            let new = offset(&b, new_start)..offset(&b, new_start + new_count);
            let id = digest_parts(&[
                document.view.id.as_bytes(),
                old.start.to_string().as_bytes(),
                old.end.to_string().as_bytes(),
                new.start.to_string().as_bytes(),
                new.end.to_string().as_bytes(),
            ]);
            let display_start = group.start.saturating_sub(CONTEXT);
            let display_end = (group.end + CONTEXT).min(operations.len());
            let mut rendered = Vec::new();
            for op in &operations[display_start..display_end] {
                match *op {
                    Op::Same(i, j) => rendered.push(ReviewLine {
                        kind: "context".into(),
                        old_line: Some(i as u64 + 1),
                        new_line: Some(j as u64 + 1),
                        text: vec![span(a[i].body, false)],
                        ending: a[i].ending.into(),
                    }),
                    Op::Remove(i) => {
                        rendered.push(ReviewLine {
                            kind: "removed".into(),
                            old_line: Some(i as u64 + 1),
                            new_line: None,
                            text: vec![span(a[i].body, true)],
                            ending: a[i].ending.into(),
                        });
                    }
                    Op::Add(j) => {
                        rendered.push(ReviewLine {
                            kind: "added".into(),
                            old_line: None,
                            new_line: Some(j as u64 + 1),
                            text: vec![span(b[j].body, true)],
                            ending: b[j].ending.into(),
                        });
                    }
                }
            }
            let mut cursor = 0;
            while cursor < rendered.len() {
                if rendered[cursor].kind == "context" {
                    cursor += 1;
                    continue;
                }
                let start = cursor;
                while cursor < rendered.len() && rendered[cursor].kind != "context" {
                    cursor += 1;
                }
                let removed: Vec<_> = (start..cursor)
                    .filter_map(|i| rendered[i].old_line.map(|line| (i, line as usize - 1)))
                    .collect();
                let added: Vec<_> = (start..cursor)
                    .filter_map(|i| rendered[i].new_line.map(|line| (i, line as usize - 1)))
                    .collect();
                for ((ri, i), (rj, j)) in removed.into_iter().zip(added) {
                    if let Some((left, right)) = word_spans(a[i].body, b[j].body, &mut word_budget)
                    {
                        rendered[ri].text = left;
                        rendered[rj].text = right;
                    } else if !document
                        .view
                        .warnings
                        .iter()
                        .any(|w| w.starts_with("Word alignment"))
                    {
                        document.view.warnings.push("Word alignment limit reached for a line pair: those lines are highlighted as whole changes.".into());
                    }
                }
            }
            document.raw.push(RawHunk {
                id: id.clone(),
                old,
                new,
            });
            document.view.hunks.push(ReviewHunk {
                id,
                old_start: old_start as u64 + 1,
                old_lines: old_count as u64,
                new_start: new_start as u64 + 1,
                new_lines: new_count as u64,
                decision: "pending".into(),
                lines: rendered,
            });
        }
        if serde_json::to_vec(&document.view).map_or(true, |bytes| bytes.len() > MAX_ENCODED_REVIEW)
        {
            document.view.exclusion_reason =
                Some("Encoded diff exceeds the 2 MiB interactive rendering limit".into());
            document.view.hunks.clear();
            document.raw.clear();
            document.before = None;
            document.after = None;
        }
        document
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        self.before.as_ref().map_or(0, Vec::len) + self.after.as_ref().map_or(0, Vec::len)
    }
    pub(crate) fn rebased_after(&self, after: Option<&[u8]>) -> Self {
        Self::build(&self.view.path, self.before.as_deref(), after)
    }
    pub fn view(&self) -> &ReviewFile {
        &self.view
    }
    /// Exact content for a set of server-selected initial hunk IDs. Every
    /// unaffected byte comes from the initial after-image; no fuzzy patching.
    pub fn content_for_rejections(&self, ids: &BTreeSet<String>) -> Result<Option<Vec<u8>>> {
        if self.view.exclusion_reason.is_some() {
            bail!("This file is not hunk-editable");
        }
        if ids.iter().any(|id| !self.raw.iter().any(|h| &h.id == id)) {
            bail!("Unknown review hunk");
        }
        if ids.is_empty() {
            return Ok(self.after.clone());
        }
        let mut result = self.after.clone().unwrap_or_default();
        for hunk in self.raw.iter().rev().filter(|h| ids.contains(&h.id)) {
            result.splice(
                hunk.new.clone(),
                self.before.as_deref().unwrap_or_default()[hunk.old.clone()]
                    .iter()
                    .copied(),
            );
        }
        if self.before.is_none() && ids.len() == self.raw.len() {
            return Ok(None);
        }
        Ok(Some(result))
    }
    /// Standalone local-draft inverse: requires the exact reviewed after-image.
    pub fn reject(&self, id: &str, current: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
        if image_hash(current) != self.view.after_hash {
            bail!("The reviewed content changed; inspect again");
        }
        self.content_for_rejections(&BTreeSet::from([id.into()]))
    }

    /// Derive current path:line context from the server's decision set. Earlier
    /// inverse edits can move later hunks; clients must not invent line numbers
    /// from the original display or supply their own path/context patches.
    pub fn context_line(&self, id: &str, rejected: &BTreeSet<String>) -> Result<u64> {
        let hunk = self
            .raw
            .iter()
            .find(|h| h.id == id)
            .ok_or_else(|| anyhow::anyhow!("Unknown review hunk"))?;
        let current = self.content_for_rejections(rejected)?.unwrap_or_default();
        let mut position = hunk.new.start as i64;
        for prior in self.raw.iter().take_while(|prior| prior.id != hunk.id) {
            if rejected.contains(&prior.id) {
                position += prior.old.len() as i64 - prior.new.len() as i64;
            }
        }
        let position = usize::try_from(position)?;
        if position > current.len() {
            bail!("Review context no longer matches its inverse edits");
        }
        Ok(current[..position]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u64
            + 1)
    }
}
fn digest_parts(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    format!("{:x}", h.finalize())
}
fn lines(text: &str) -> Vec<Line<'_>> {
    let mut offset = 0;
    let mut out = Vec::new();
    for raw in text.split_inclusive('\n') {
        let (body, ending) = if let Some(body) = raw.strip_suffix("\r\n") {
            (body, "\r\n")
        } else if let Some(body) = raw.strip_suffix('\n') {
            (body, "\n")
        } else {
            (raw, "")
        };
        out.push(Line {
            body,
            ending,
            start: offset,
            end: offset + raw.len(),
        });
        offset += raw.len();
    }
    out
}
fn offset(lines: &[Line<'_>], index: usize) -> usize {
    lines
        .get(index)
        .map_or_else(|| lines.last().map_or(0, |l| l.end), |line| line.start)
}
fn replacement(a: usize, b: usize) -> Vec<Op> {
    (0..a).map(Op::Remove).chain((0..b).map(Op::Add)).collect()
}
fn align<T: Eq>(a: &[T], b: &[T]) -> Vec<Op> {
    let width = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * width + j] = if a[i] == b[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(Op::Same(i, j));
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            out.push(Op::Remove(i));
            i += 1;
        } else {
            out.push(Op::Add(j));
            j += 1;
        }
    }
    out.extend((i..a.len()).map(Op::Remove));
    out.extend((j..b.len()).map(Op::Add));
    out
}
fn span(text: &str, changed: bool) -> ReviewTextSpan {
    ReviewTextSpan {
        text: text.into(),
        changed,
    }
}
fn word_spans(
    a: &str,
    b: &str,
    budget: &mut usize,
) -> Option<(Vec<ReviewTextSpan>, Vec<ReviewTextSpan>)> {
    let a = word_tokens(a);
    let b = word_tokens(b);
    let work = a.len().saturating_mul(b.len());
    if work > MAX_WORD_PAIRS || work > *budget {
        return None;
    }
    *budget -= work;
    let mut left = Vec::new();
    let mut right = Vec::new();
    for op in align(&a, &b) {
        match op {
            Op::Same(i, j) => {
                left.push(span(a[i], false));
                right.push(span(b[j], false));
            }
            Op::Remove(i) => left.push(span(a[i], true)),
            Op::Add(j) => right.push(span(b[j], true)),
        }
    }
    Some((left, right))
}
// Unicode word boundaries can bisect an extended grapheme (notably emoji
// sequences). Merge such adjacent tokens before alignment so every displayed
// style boundary is also an extended-grapheme boundary.
fn word_tokens(text: &str) -> Vec<&str> {
    let boundaries: BTreeSet<_> = text.grapheme_indices(true).map(|(i, _)| i).collect();
    let mut out = Vec::new();
    let mut start = 0;
    for (offset, token) in text.split_word_bound_indices() {
        let end = offset + token.len();
        if end == text.len() || boundaries.contains(&end) {
            out.push(&text[start..end]);
            start = end;
        }
    }
    out
}
fn generated(path: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if normalized
        .split('/')
        .any(|part| matches!(part, ".git" | "node_modules" | "target" | "dist" | "vendor"))
    {
        return true;
    }
    let name = normalized.rsplit('/').next().unwrap_or_default();
    if matches!(
        name,
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml"
    ) || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".generated.rs")
    {
        return true;
    }
    before.into_iter().chain(after).any(|bytes| {
        let head = &bytes[..bytes.len().min(1024)];
        let head = String::from_utf8_lossy(head).to_ascii_lowercase();
        head.contains("@generated")
            || head.contains("auto-generated")
            || head.contains("automatically generated")
            || head.contains("do not edit")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bytes(text: &str) -> Option<&[u8]> {
        Some(text.as_bytes())
    }
    fn two_hunks() -> (String, String) {
        let before = "first old\r\n2\n3\r\n4\n5\r\n6\n7\n8\r\n9\nlast old".to_string();
        let after = before
            .replace("first old", "first NEW")
            .replace("last old", "last NEW");
        (before, after)
    }
    #[test]
    fn each_hunk_inverse_changes_only_its_exact_bytes_and_undo_is_exact() {
        let (before, after) = two_hunks();
        let doc = ReviewDocument::build("src/example.rs", bytes(&before), bytes(&after));
        assert_eq!(doc.view().hunks.len(), 2);
        let first = &doc.view().hunks[0].id;
        let second = &doc.view().hunks[1].id;
        let rejected = doc.reject(first, bytes(&after)).unwrap().unwrap();
        assert_eq!(rejected, after.replace("first NEW", "first old").as_bytes());
        assert_eq!(
            doc.content_for_rejections(&BTreeSet::from([second.clone()]))
                .unwrap()
                .unwrap(),
            after.replace("last NEW", "last old").as_bytes()
        );
        assert_eq!(
            doc.content_for_rejections(&BTreeSet::from([first.clone(), second.clone()]))
                .unwrap()
                .unwrap(),
            before.as_bytes()
        );
        assert_eq!(
            doc.content_for_rejections(&BTreeSet::new())
                .unwrap()
                .unwrap(),
            after.as_bytes(),
            "undo of all rejects reproduces exact original current bytes"
        );
        assert!(
            doc.reject(first, Some(&rejected)).is_err(),
            "standalone inverse never fuzzy-applies against shifted content"
        );
    }
    #[test]
    fn insert_delete_and_newline_only_hunks_roundtrip_without_normalization() {
        for (old, new) in [
            ("x\r\ny\n", "x\r\nnew\r\ny\n"),
            ("x\nremove\r\ny", "x\ny"),
            ("x\r\n", "x\n"),
            ("last\n", "last"),
            ("last", "last\n"),
            ("", "\n"),
            ("\n", ""),
        ] {
            let doc = ReviewDocument::build("notes.txt", bytes(old), bytes(new));
            assert!(!doc.view().hunks.is_empty(), "{old:?} -> {new:?}");
            let ids = doc.view().hunks.iter().map(|h| h.id.clone()).collect();
            assert_eq!(
                doc.content_for_rejections(&ids).unwrap().unwrap(),
                old.as_bytes(),
                "{old:?} -> {new:?}"
            );
        }
    }
    #[test]
    fn absence_empty_creation_and_removal_have_distinct_identity_and_exact_inverse() {
        assert_ne!(image_hash(None), image_hash(Some(&[])));
        for (old, new) in [
            (None, Some(&b""[..])),
            (None, Some(&b"created\r\n"[..])),
            (Some(&b""[..]), None),
            (Some(&b"removed\n"[..]), None),
        ] {
            let doc = ReviewDocument::build("owned.txt", old, new);
            assert_eq!(doc.view().hunks.len(), 1);
            assert_eq!(
                doc.reject(&doc.view().hunks[0].id, new).unwrap().as_deref(),
                old
            );
        }
    }
    #[test]
    fn intra_line_unicode_words_preserve_every_character_and_only_changed_words_highlight() {
        let doc = ReviewDocument::build(
            "unicode.txt",
            bytes("hello 世界 old 👩‍💻!\r\n"),
            bytes("hello 世界 new 👩‍💻!\r\n"),
        );
        let lines = &doc.view().hunks[0].lines;
        let removed = lines.iter().find(|l| l.kind == "removed").unwrap();
        let added = lines.iter().find(|l| l.kind == "added").unwrap();
        assert_eq!(
            removed
                .text
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "hello 世界 old 👩‍💻!"
        );
        assert_eq!(
            added
                .text
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "hello 世界 new 👩‍💻!"
        );
        assert_eq!(
            removed
                .text
                .iter()
                .filter(|s| s.changed)
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "old"
        );
        assert_eq!(
            added
                .text
                .iter()
                .filter(|s| s.changed)
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "new"
        );
        assert_eq!(added.ending, "\r\n");
        assert_eq!(removed.old_line, Some(1));
        assert_eq!(added.new_line, Some(1));
    }
    #[test]
    fn unchanged_files_and_content_bound_hunk_ids_refuse_wrong_or_stale_edits() {
        assert!(
            ReviewDocument::build("same.txt", bytes("same"), bytes("same"))
                .view()
                .hunks
                .is_empty()
        );
        let one = ReviewDocument::build("a.txt", bytes("a"), bytes("b"));
        let another = ReviewDocument::build("other.txt", bytes("a"), bytes("b"));
        let next = ReviewDocument::build("a.txt", bytes("a"), bytes("c"));
        assert_ne!(one.view().id, another.view().id);
        assert_ne!(one.view().hunks[0].id, next.view().hunks[0].id);
        assert!(one.reject(&another.view().hunks[0].id, bytes("b")).is_err());
        assert!(one.reject(&one.view().hunks[0].id, bytes("c")).is_err());
    }
    #[test]
    fn binary_generated_and_oversize_inputs_are_listed_but_not_hunk_editable() {
        let oversized = vec![b'a'; MAX_REVIEW_BYTES + 1];
        for (path, old, new) in [
            ("binary.dat", Some(&b"\0old"[..]), Some(&b"\0new"[..])),
            ("invalid.txt", Some(&b"\xff"[..]), Some(&b"valid"[..])),
            ("target/build.rs", bytes("old"), bytes("new")),
            (
                "src/generated.rs",
                bytes("// @generated\nold"),
                bytes("new"),
            ),
            ("Cargo.lock", bytes("old"), bytes("new")),
            ("huge.txt", Some(oversized.as_slice()), bytes("new")),
        ] {
            let doc = ReviewDocument::build(path, old, new);
            assert_eq!(doc.view().path, path);
            assert!(doc.view().exclusion_reason.is_some(), "{path}");
            assert!(doc.view().hunks.is_empty());
            assert!(doc.content_for_rejections(&BTreeSet::new()).is_err());
        }
    }
    #[test]
    fn line_work_limit_falls_back_truthfully_but_remains_exactly_reversible() {
        let old = (0..1100).map(|i| format!("old {i}\n")).collect::<String>();
        let new = (0..1100)
            .map(|i| format!("new {i}\r\n"))
            .collect::<String>();
        let doc = ReviewDocument::build("large.txt", bytes(&old), bytes(&new));
        assert_eq!(doc.view().hunks.len(), 1);
        assert!(doc
            .view()
            .warnings
            .iter()
            .any(|w| w.contains("Line alignment limit")));
        assert!(!doc.view().hunks[0]
            .lines
            .iter()
            .any(|l| l.kind == "context"));
        assert_eq!(
            doc.reject(&doc.view().hunks[0].id, bytes(&new))
                .unwrap()
                .unwrap(),
            old.as_bytes()
        );
    }
    #[test]
    fn bounded_word_alignment_is_explicit_and_preserves_whole_text() {
        let old = "a ".repeat(200);
        let new = "b ".repeat(200);
        let doc = ReviewDocument::build("longline.txt", bytes(&old), bytes(&new));
        assert!(doc
            .view()
            .warnings
            .iter()
            .any(|w| w.contains("Word alignment limit")));
        assert_eq!(
            doc.reject(&doc.view().hunks[0].id, bytes(&new))
                .unwrap()
                .unwrap(),
            old.as_bytes()
        );
    }
    #[test]
    fn styled_spans_never_bisect_extended_graphemes_or_drop_thai_combining_marks() {
        for (old, new) in [
            ("e\u{301} 👩‍💻 ไทยเก่า", "e\u{301} 👨‍💻 ไทยใหม่"),
            ("family 👨‍👩‍👧‍👦 old", "family 👩‍👩‍👧‍👦 new"),
        ] {
            let doc = ReviewDocument::build("graphemes.txt", bytes(old), bytes(new));
            for (kind, original) in [("removed", old), ("added", new)] {
                let line = doc.view().hunks[0]
                    .lines
                    .iter()
                    .find(|l| l.kind == kind)
                    .unwrap();
                let mut offset = 0;
                let boundaries: BTreeSet<_> = original
                    .grapheme_indices(true)
                    .map(|(i, _)| i)
                    .chain(std::iter::once(original.len()))
                    .collect();
                for span in &line.text {
                    assert!(boundaries.contains(&offset));
                    offset += span.text.len();
                    assert!(boundaries.contains(&offset));
                }
                assert_eq!(offset, original.len());
                assert_eq!(
                    line.text
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<String>(),
                    original
                );
            }
        }
    }
    #[test]
    fn tiny_bytes_with_many_empty_lines_cannot_expand_unbounded_render_state() {
        let old = "\n".repeat(MAX_REVIEW_BYTES);
        let doc = ReviewDocument::build("many-lines.txt", bytes(&old), bytes("new\n"));
        assert!(doc
            .view()
            .exclusion_reason
            .as_deref()
            .unwrap()
            .contains("8192-line"));
        assert!(doc.view().hunks.is_empty());
        assert!(doc.content_for_rejections(&BTreeSet::new()).is_err());
    }
    #[test]
    fn server_comment_line_tracks_earlier_rejection_without_client_offsets() {
        let before = "old\n2\n3\n4\n5\n6\n7\n8\n9\nlast old\n";
        let after = "inserted a\ninserted b\ninserted c\n2\n3\n4\n5\n6\n7\n8\n9\nlast new\n";
        let doc = ReviewDocument::build("context.txt", bytes(before), bytes(after));
        assert_eq!(doc.view().hunks.len(), 2);
        let first = doc.view().hunks[0].id.clone();
        let last = doc.view().hunks[1].id.clone();
        assert_eq!(doc.context_line(&last, &BTreeSet::new()).unwrap(), 12);
        assert_eq!(
            doc.context_line(&last, &BTreeSet::from([first.clone()]))
                .unwrap(),
            10
        );
        assert_eq!(
            doc.context_line(&last, &BTreeSet::from([first, last.clone()]))
                .unwrap(),
            10
        );
        assert!(doc
            .context_line("arbitrary-client-hunk", &BTreeSet::new())
            .is_err());
    }
}
