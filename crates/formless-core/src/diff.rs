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
        let lines = line_diff(&a, &b);
        // Keep changed lines plus two lines of context around them.
        let near = |i: usize| {
            lines[i.saturating_sub(2)..(i + 3).min(lines.len())]
                .iter()
                .any(|(k, _)| *k != ' ')
        };
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
    }
    if out.is_empty() {
        "(no changes)".into()
    } else {
        out.join("\n")
    }
}
