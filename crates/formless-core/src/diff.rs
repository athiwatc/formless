//! Line diffs for text/file review and Tool-version presentation.

use formless_protocol::ToolVersionInfo;

/// One diff line: `' '` unchanged, `'-'` only in the old text, `'+'` only
/// in the new one.
pub type DiffLine = (char, String);

/// Longest-common-subsequence line diff, preserving common edges before
/// bounding alignment work on the differing middle. The returned line kinds
/// and reconstruction contract stay unchanged even when alignment is capped.
pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    diff_with_alignment(old, new).0
}

const MAX_LCS_CELLS: usize = 4_000_000;

/// The bool reports an unaligned middle for human-readable callers. It is
/// separate from DiffLine so no synthetic lines enter raw diff data.
fn diff_with_alignment(old: &str, new: &str) -> (Vec<DiffLine>, bool) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let common = a.len().min(b.len());
    let prefix = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(common - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<_> = a[..prefix]
        .iter()
        .map(|line| (' ', (*line).to_owned()))
        .collect();
    let middle_a = &a[prefix..a.len() - suffix];
    let middle_b = &b[prefix..b.len() - suffix];
    let unaligned = if middle_a.is_empty() || middle_b.is_empty() {
        // No matrix or approximation is needed for pure insertion/deletion.
        out.extend(middle_a.iter().map(|line| ('-', (*line).to_owned())));
        out.extend(middle_b.iter().map(|line| ('+', (*line).to_owned())));
        false
    } else if middle_a
        .len()
        .saturating_add(1)
        .saturating_mul(middle_b.len().saturating_add(1))
        > MAX_LCS_CELLS
    {
        out.extend(middle_a.iter().map(|line| ('-', (*line).to_owned())));
        out.extend(middle_b.iter().map(|line| ('+', (*line).to_owned())));
        true
    } else {
        aligned_middle(middle_a, middle_b, &mut out);
        false
    };
    out.extend(
        a[a.len() - suffix..]
            .iter()
            .map(|line| (' ', (*line).to_owned())),
    );
    (out, unaligned)
}

fn aligned_middle(a: &[&str], b: &[&str], out: &mut Vec<DiffLine>) {
    // A flat matrix bounds allocation to MAX_LCS_CELLS u32s (16 MB),
    // including borders, without a heap allocation for every row.
    let stride = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * stride];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * stride + j] = if a[i] == b[j] {
                lcs[(i + 1) * stride + j + 1] + 1
            } else {
                lcs[(i + 1) * stride + j].max(lcs[i * stride + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push((' ', a[i].to_string()));
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * stride + j] >= lcs[i * stride + j + 1] {
            out.push(('-', a[i].to_string()));
            i += 1;
        } else {
            out.push(('+', b[j].to_string()));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| ('-', l.to_string())));
    out.extend(b[j..].iter().map(|l| ('+', l.to_string())));
}

/// The changed lines from `old` to `new` (`- `/`+ ` prefixed) with two
/// lines of context around them; skipped runs show as `  …`.
pub fn context_diff(old: &str, new: &str) -> Vec<String> {
    let (lines, unaligned) = diff_with_alignment(old, new);
    let near = |i: usize| {
        lines[i.saturating_sub(2)..(i + 3).min(lines.len())]
            .iter()
            .any(|(k, _)| *k != ' ')
    };
    let mut out = Vec::new();
    if unaligned {
        out.push("  … diff too large to align; showing unaligned middle".into());
    }
    let mut skipped = false;
    for (i, (kind, text)) in lines.iter().enumerate() {
        if *kind == ' ' && !near(i) {
            skipped = true;
            continue;
        }
        if skipped {
            out.push("  …".into());
            skipped = false;
        }
        out.push(format!("{kind} {text}"));
    }
    // A skipped tail is marked too, so the end of a diff is never mistaken
    // for the end of the text (#828).
    if skipped && !out.is_empty() {
        out.push("  …".into());
    }
    // Ending facts are independent of content hunks: an edited line must
    // not hide a final-newline or LF/CRLF change elsewhere in the file.
    if old != new {
        match (old.ends_with('\n'), new.ends_with('\n')) {
            (true, false) => out.push("\\ the final newline was removed".into()),
            (false, true) => out.push("\\ a final newline was added".into()),
            _ => {}
        }
        match (ending_profile(old), ending_profile(new)) {
            (Some(a), Some(b)) if a != b => {
                out.push(format!("\\ line endings changed from {a} to {b}"));
            }
            (Some("mixed LF/CRLF"), Some("mixed LF/CRLF"))
                if !line_endings(old).eq(line_endings(new)) =>
            {
                out.push("\\ line ending layout changed (mixed LF/CRLF)".into());
            }
            _ => {}
        }
    }
    out
}

fn line_endings(text: &str) -> impl Iterator<Item = bool> + '_ {
    let bytes = text.as_bytes();
    bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .map(move |(i, _)| i > 0 && bytes[i - 1] == b'\r')
}

fn ending_profile(text: &str) -> Option<&'static str> {
    let (mut lf, mut crlf) = (false, false);
    for carriage_return in line_endings(text) {
        if carriage_return {
            crlf = true;
        } else {
            lf = true;
        }
    }
    match (lf, crlf) {
        (true, true) => Some("mixed LF/CRLF"),
        (true, false) => Some("LF"),
        (false, true) => Some("CRLF"),
        _ => None,
    }
}

fn pretty(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| json.to_string())
}

/// What changed from `old` to `new`, by section, as unified-style text
/// with a little context; unchanged sections are left out.
pub fn version_diff(old: &ToolVersionInfo, new: &ToolVersionInfo) -> String {
    let sections = [
        ("code", old.code.clone(), new.code.clone()),
        (
            "input_schema",
            pretty(&old.input_schema),
            pretty(&new.input_schema),
        ),
        (
            "output_schema",
            pretty(&old.output_schema),
            pretty(&new.output_schema),
        ),
        (
            "capabilities",
            old.capabilities.join("\n"),
            new.capabilities.join("\n"),
        ),
        (
            "memory_mb",
            old.memory_mb.to_string(),
            new.memory_mb.to_string(),
        ),
    ];
    let mut out = Vec::new();
    for (name, a, b) in sections {
        if a == b {
            continue;
        }
        out.push(format!("@@ {name} (v{} → v{})", old.version, new.version));
        out.extend(context_diff(&a, &b));
    }
    if old.input_schema != new.input_schema || old.output_schema != new.output_schema {
        let report = crate::tool_compatibility::report(
            "",
            old.version,
            &old.input_schema,
            &old.output_schema,
            &new.input_schema,
            &new.output_schema,
        );
        out.push(format!(
            "@@ schema_compatibility (v{} → v{})",
            old.version, new.version
        ));
        out.push(
            serde_json::to_string_pretty(&report.compatibility)
                .unwrap_or_else(|_| "inconclusive".into()),
        );
    }
    if out.is_empty() {
        "(no changes)".into()
    } else {
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reconstruct(lines: &[DiffLine], side: char) -> Vec<&str> {
        lines
            .iter()
            .filter(|(kind, _)| *kind == ' ' || *kind == side)
            .map(|(_, text)| text.as_str())
            .collect()
    }

    #[test]
    fn one_local_edit_in_a_large_file_keeps_the_context_and_listing_budget_small() {
        let lines: Vec<_> = (0..3000)
            .map(|i| format!("line {i:04}: {}", "payload ".repeat(6)))
            .collect();
        let old = lines.join("\n") + "\n";
        let mut changed = lines.clone();
        changed[1500] = "line 1500: changed 雪".into();
        let new = changed.join("\n") + "\n";
        let (diff, unaligned) = diff_with_alignment(&old, &new);
        assert!(!unaligned);
        assert_eq!(diff.iter().filter(|(k, _)| *k == '-').count(), 1);
        assert_eq!(diff.iter().filter(|(k, _)| *k == '+').count(), 1);
        assert_eq!(reconstruct(&diff, '-'), old.lines().collect::<Vec<_>>());
        assert_eq!(reconstruct(&diff, '+'), new.lines().collect::<Vec<_>>());
        let context = context_diff(&old, &new);
        assert_eq!(context.len(), 8, "{context:?}");
        assert_eq!(context.first().unwrap(), "  …");
        assert_eq!(context.last().unwrap(), "  …");
        assert!(context
            .iter()
            .any(|line| line == &format!("- {}", lines[1500])));
        assert!(context.iter().any(|line| line == "+ line 1500: changed 雪"));
        assert!(
            context.join("\n").len() < 1024,
            "a local edit must not consume a large listing budget"
        );
        assert!(
            context_diff(&old, &old).is_empty(),
            "large identical files have no changes"
        );
    }

    #[test]
    fn common_edges_and_empty_middles_preserve_insertions_deletions_and_repeated_lines() {
        for (old, new, removed, added) in [
            ("start\na\nb\nend", "start\na\nx\nb\nend", 0, 1),
            ("start\na\nx\nb\nend", "start\na\nb\nend", 1, 0),
            ("a\na", "a", 1, 0),
            ("a", "a\na", 0, 1),
            ("", "雪\n", 0, 1),
            ("雪\n", "", 1, 0),
            ("", "", 0, 0),
            ("start\na\na\nend", "start\na\nb\na\nend", 0, 1),
        ] {
            let (diff, unaligned) = diff_with_alignment(old, new);
            assert!(!unaligned);
            assert_eq!(diff.iter().filter(|(k, _)| *k == '-').count(), removed);
            assert_eq!(diff.iter().filter(|(k, _)| *k == '+').count(), added);
            assert_eq!(reconstruct(&diff, '-'), old.lines().collect::<Vec<_>>());
            assert_eq!(reconstruct(&diff, '+'), new.lines().collect::<Vec<_>>());
        }
    }

    #[test]
    fn a_large_unaligned_middle_is_labeled_without_synthetic_raw_diff_lines() {
        let prefix = "prefix1\nprefix2\nprefix3\nprefix4\n";
        let suffix = "\nsuffix1\nsuffix2\nsuffix3\nsuffix4";
        let old = format!(
            "{prefix}{}{suffix}",
            (0..2100)
                .map(|i| format!("old{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let new = format!(
            "{prefix}{}{suffix}",
            (0..2100)
                .map(|i| format!("new{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let (diff, unaligned) = diff_with_alignment(&old, &new);
        assert!(unaligned);
        assert!(diff.iter().all(|(kind, _)| matches!(kind, ' ' | '-' | '+')));
        assert_eq!(reconstruct(&diff, '-'), old.lines().collect::<Vec<_>>());
        assert_eq!(reconstruct(&diff, '+'), new.lines().collect::<Vec<_>>());
        assert!(!diff
            .iter()
            .any(|(_, text)| text.contains("too large to align")));
        let context = context_diff(&old, &new);
        assert_eq!(
            context[0],
            "  … diff too large to align; showing unaligned middle"
        );
        assert!(!context
            .iter()
            .any(|line| line.starts_with("- prefix") || line.starts_with("+ suffix")));
    }

    #[test]
    fn short_repeated_sequences_reconstruct_both_sides_with_a_minimal_edit_count() {
        // Independent exhaustive subsequence oracle, not another LCS matrix.
        let mut texts = vec![String::new()];
        for length in 1..=4 {
            for bits in 0..(1 << length) {
                texts.push(
                    (0..length)
                        .map(|i| if bits & (1 << i) == 0 { "a" } else { "b" })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }
        for old in &texts {
            let a: Vec<_> = old.lines().collect();
            for new in &texts {
                let b: Vec<_> = new.lines().collect();
                let common = (0..(1 << a.len()))
                    .filter_map(|mask| {
                        let wanted: Vec<_> = a
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| mask & (1 << i) != 0)
                            .map(|(_, line)| *line)
                            .collect();
                        let mut cursor = 0;
                        for line in &b {
                            if wanted.get(cursor) == Some(line) {
                                cursor += 1;
                            }
                        }
                        (cursor == wanted.len()).then_some(cursor)
                    })
                    .max()
                    .unwrap();
                let diff = line_diff(old, new);
                assert_eq!(reconstruct(&diff, '-'), a, "{old:?} -> {new:?}");
                assert_eq!(reconstruct(&diff, '+'), b, "{old:?} -> {new:?}");
                assert_eq!(
                    diff.iter().filter(|(kind, _)| *kind != ' ').count(),
                    a.len() + b.len() - 2 * common,
                    "{old:?} -> {new:?}"
                );
            }
        }
    }

    #[test]
    fn content_edits_do_not_hide_final_newline_or_line_ending_changes() {
        let added = context_diff("a\nb", "a\nc\n");
        assert!(added.contains(&"- b".into()) && added.contains(&"+ c".into()));
        assert!(
            added.contains(&"\\ a final newline was added".into()),
            "{added:?}"
        );
        let both = context_diff("a\r\nb\r\n", "a\nc");
        assert!(
            both.contains(&"\\ the final newline was removed".into()),
            "{both:?}"
        );
        assert!(
            both.contains(&"\\ line endings changed from CRLF to LF".into()),
            "{both:?}"
        );
        let mixed = context_diff("a\r\nb\n", "a\nc\r\n");
        assert!(
            mixed.contains(&"\\ line ending layout changed (mixed LF/CRLF)".into()),
            "{mixed:?}"
        );
    }

    #[test]
    fn ending_notes_are_precise_without_inventing_whitespace_changes() {
        assert_eq!(
            context_diff("a\r\n", "a\n"),
            ["\\ line endings changed from CRLF to LF"]
        );
        assert_eq!(
            context_diff("a\n", "a\r\n"),
            ["\\ line endings changed from LF to CRLF"]
        );
        assert!(context_diff("a\nb\n", "a\nc\n")
            .iter()
            .all(|line| !line.starts_with("\\ ")));
        assert!(context_diff("a ", "a")
            .iter()
            .all(|line| !line.starts_with("\\ ")));
        assert!(context_diff("a\r\nb\r\n", "a\r\nb\r\n").is_empty());
        assert_eq!(
            context_diff("a\r\nb\n", "a\nb\r\n"),
            ["\\ line ending layout changed (mixed LF/CRLF)"]
        );
        assert!(context_diff("a\n", "a\nb\n")
            .iter()
            .all(|line| !line.starts_with("\\ ")));
    }

    /// A newline-only change says so, and a skipped tail is marked (#828).
    #[test]
    fn newline_only_changes_and_tails_are_shown() {
        assert_eq!(
            context_diff("a", "a\n"),
            vec!["\\ a final newline was added"]
        );
        assert_eq!(
            context_diff("a\n", "a"),
            vec!["\\ the final newline was removed"]
        );
        assert!(context_diff("", "").is_empty());
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n";
        let new = "X\n2\n3\n4\n5\n6\n7\n8\n";
        let d = context_diff(old, new);
        assert_eq!(d.first().map(String::as_str), Some("- 1"));
        assert_eq!(d.last().map(String::as_str), Some("  …"), "{d:?}");
    }
}
