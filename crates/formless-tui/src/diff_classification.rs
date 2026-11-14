//! Shared unified-diff classification before any context rows are collapsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Header,
    Hunk,
    Added,
    Removed,
    Plain,
}

fn count(range: &str) -> Option<usize> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    start.parse::<usize>().ok()?;
    count.parse().ok()
}

fn hunk_counts(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    Some((count(old)?, count(new)?))
}

/// Valid hunk counts disambiguate signed content, including `---`/`+++`.
/// After exhaustion or an unexpected row, fall back to fragment styling:
/// triple-sign prefixes are headers; other signed rows are changes. Malformed
/// hunk headers clear the counts. No-newline markers consume neither side.
/// Incomplete hunks retain their remaining budget until a new header/invalid
/// row; ambiguous data cannot be validated as a complete patch by this view.
pub(super) fn classify(text: &str) -> impl Iterator<Item = (&str, Kind)> {
    let mut remaining = None;
    text.lines().map(move |line| {
        let kind = if line.starts_with("@@") {
            remaining = hunk_counts(line);
            Kind::Hunk
        } else if line == "\\ No newline at end of file" {
            Kind::Plain
        } else {
            let in_hunk = remaining
                .as_mut()
                .and_then(|(old, new)| match line.as_bytes().first() {
                    Some(b'+') if *new > 0 => {
                        *new -= 1;
                        Some(Kind::Added)
                    }
                    Some(b'-') if *old > 0 => {
                        *old -= 1;
                        Some(Kind::Removed)
                    }
                    Some(b' ') if *old > 0 && *new > 0 => {
                        *old -= 1;
                        *new -= 1;
                        Some(Kind::Plain)
                    }
                    _ => None,
                });
            match in_hunk {
                Some(kind) => kind,
                None => {
                    remaining = None;
                    if line.starts_with("+++") || line.starts_with("---") {
                        Kind::Header
                    } else if line.starts_with('+') {
                        Kind::Added
                    } else if line.starts_with('-') {
                        Kind::Removed
                    } else {
                        Kind::Plain
                    }
                }
            }
        };
        (line, kind)
    })
}
