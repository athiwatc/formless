//! Line diffs between Tool versions (code, schemas, Capabilities), for
//! reviewing agent edits and optimiser promotions.

use formless_protocol::ToolVersionInfo;

/// One diff line: `' '` unchanged, `'-'` only in the old text, `'+'` only
/// in the new one.
pub type DiffLine = (char, String);

/// Longest-common-subsequence line diff. Very large inputs (beyond what a
/// Tool plausibly holds) fall back to "all removed, all added".
pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a.len().saturating_mul(b.len()) > 4_000_000 {
        return a
            .iter()
            .map(|l| ('-', l.to_string()))
            .chain(b.iter().map(|l| ('+', l.to_string())))
            .collect();
    }
    // lcs[i][j] = LCS length of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push((' ', a[i].to_string()));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(('-', a[i].to_string()));
            i += 1;
        } else {
            out.push(('+', b[j].to_string()));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| ('-', l.to_string())));
    out.extend(b[j..].iter().map(|l| ('+', l.to_string())));
    out
}

/// The changed lines from `old` to `new` (`- `/`+ ` prefixed) with two
/// lines of context around them; skipped runs show as `  …`.
pub fn context_diff(old: &str, new: &str) -> Vec<String> {
    let lines = line_diff(old, new);
    let near = |i: usize| {
        lines[i.saturating_sub(2)..(i + 3).min(lines.len())]
            .iter()
            .any(|(k, _)| *k != ' ')
    };
    let mut out = Vec::new();
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
    // Lines alike but the texts differ: only the final newline changed.
    if !old.eq(new) && lines.iter().all(|(k, _)| *k == ' ') {
        out.push(
            match (old.ends_with('\n'), new.ends_with('\n')) {
                (true, false) => "\\ the final newline was removed",
                (false, true) => "\\ a final newline was added",
                _ => "\\ only whitespace at line ends changed",
            }
            .into(),
        );
    }
    out
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
    if out.is_empty() {
        "(no changes)".into()
    } else {
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
