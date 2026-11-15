//! One lexical contract for file-mention extraction and completion.
//! `@"..."` reserves quoted paths; only `\"` and `\\` are escapes.
//! Whitespace inside quotes is literal. Incomplete/invalid quoted tokens
//! are never reinterpreted as an ordinary first-word mention on submission.
use std::ops::Range;

fn punctuation(c: char) -> bool {
    matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | ')' | '"' | '\'')
}

fn decode(raw: &str) -> Option<String> {
    let mut decoded = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                escaped @ ('\\' | '"') => decoded.push(escaped),
                _ => return None,
            }
        } else {
            decoded.push(c);
        }
    }
    Some(decoded)
}

struct Token {
    range: Range<usize>,
    content: Range<usize>,
    quoted: bool,
    closed: bool,
    path: Option<String>,
}

fn tokens(text: &str) -> Vec<Token> {
    let mut result = Vec::new();
    let mut at = 0;
    let mut boundary = true;
    while at < text.len() {
        let c = text[at..].chars().next().unwrap();
        if c != '@' || !boundary {
            at += c.len_utf8();
            boundary = c.is_whitespace();
            continue;
        }
        let start = at;
        if text.as_bytes().get(start + 1) == Some(&b'"') {
            let content_start = start + 2;
            let mut end_quote = None;
            let mut chars = text[content_start..].char_indices();
            while let Some((offset, c)) = chars.next() {
                if c == '\\' {
                    chars.next();
                } else if c == '"' {
                    end_quote = Some(content_start + offset);
                    break;
                }
            }
            let content_end = end_quote.unwrap_or(text.len());
            let mut end = end_quote.map_or(text.len(), |quote| quote + 1);
            let mut path = decode(&text[content_start..content_end]);
            // A closed delimiter must be followed by whitespace, the usual
            // mention punctuation, or end of input. Other suffixes are invalid.
            if text[end..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace() && !punctuation(c))
            {
                path = None;
                end += text[end..]
                    .find(char::is_whitespace)
                    .unwrap_or(text.len() - end);
            }
            result.push(Token {
                range: start..end,
                content: content_start..content_end,
                quoted: true,
                closed: end_quote.is_some(),
                path,
            });
            at = end;
        } else {
            let end = start
                + 1
                + text[start + 1..]
                    .find(char::is_whitespace)
                    .unwrap_or(text.len() - start - 1);
            let path = text[start + 1..end]
                .trim_end_matches(punctuation)
                .to_owned();
            result.push(Token {
                range: start..end,
                content: start + 1..end,
                quoted: false,
                closed: true,
                path: Some(path),
            });
            at = end;
        }
        boundary = false;
    }
    result
}

/// Complete valid mention spans, including their display delimiters.
pub fn mention_ranges(text: &str) -> Vec<Range<usize>> {
    tokens(text)
        .into_iter()
        .filter(|t| t.closed && t.path.as_ref().is_some_and(|p| !p.is_empty()))
        .map(|t| t.range)
        .collect()
}

/// Decoded paths in source order, with the existing exact-string deduplication.
pub fn mentions(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for token in tokens(text).into_iter().filter(|t| t.closed) {
        if let Some(path) = token.path.filter(|path| !path.is_empty()) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

pub struct MentionCompletion {
    pub range: Range<usize>,
    pub query: String,
}

/// The whole token at the cursor and its decoded in-progress query. A closed
/// quoted token remains editable before its closing quote, not after it.
pub fn mention_completion(text: &str, cursor: usize) -> Option<MentionCompletion> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let token = tokens(text)
        .into_iter()
        .find(|t| cursor >= t.content.start && cursor <= t.content.end)?;
    if token.quoted && token.closed && token.path.is_none() {
        return None;
    }
    let prefix = &text[token.content.start..cursor];
    let query = if token.quoted {
        decode(prefix)?
    } else {
        prefix.to_owned()
    };
    Some(MentionCompletion {
        range: token.range,
        query,
    })
}

pub struct MentionInsertion {
    pub text: String,
    /// Cursor offset relative to the insertion, before a directory's closing
    /// quote so child completion can continue while submission stays delimited.
    pub cursor: usize,
}

pub fn encode_mention(path: &str, directory: bool) -> MentionInsertion {
    let path = if directory {
        format!("{path}/")
    } else {
        path.to_owned()
    };
    let quoted = path
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\\'))
        || path.chars().last().is_some_and(punctuation);
    let mut text = if quoted {
        format!("@\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("@{path}")
    };
    let cursor = if directory {
        text.len() - usize::from(quoted)
    } else {
        text.push(' ');
        text.len()
    };
    MentionInsertion { text, cursor }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_encoding_round_trips_paths_and_directory_completion() {
        for path in [
            "src/main.rs",
            "README.md",
            "my notes/report.md",
            "日本語 ไทย/งาน.md",
            "a\"b.md",
            "a\\b.md",
            "a.",
            "name)",
            "line\nbreak",
            " leading",
            "trailing ",
        ] {
            let inserted = encode_mention(path, false);
            assert_eq!(mentions(&inserted.text), vec![path]);
            assert_eq!(inserted.cursor, inserted.text.len());
            let inserted = encode_mention(path, true);
            let expected = format!("{path}/");
            assert_eq!(mentions(&inserted.text), vec![expected.clone()]);
            let completion = mention_completion(&inserted.text, inserted.cursor).unwrap();
            assert_eq!(completion.query, expected);
            assert_eq!(completion.range, 0..inserted.text.len());
        }
    }

    #[test]
    fn selected_leading_space_directory_query_keeps_its_indexed_identity() {
        use super::super::{search, FileEntry};
        let entries = [
            FileEntry {
                path: " notes".into(),
                dir: true,
                bytes: 0,
            },
            FileEntry {
                path: " notes/right.md".into(),
                dir: false,
                bytes: 1,
            },
            FileEntry {
                path: "notes/wrong.md".into(),
                dir: false,
                bytes: 2,
            },
        ];
        let insertion = encode_mention(" notes", true);
        let query = mention_completion(&insertion.text, insertion.cursor).unwrap();
        assert_eq!(
            search(&entries, &query.query, 10)
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            vec![" notes/right.md"]
        );
        assert!(search(&entries[..1], &query.query, 10).is_empty());
    }

    #[test]
    fn ordinary_quoted_incomplete_and_invalid_tokens_have_explicit_boundaries() {
        assert_eq!(
            mentions("@src/main.rs @README.md. mail a@b.c @a @a"),
            vec!["src/main.rs", "README.md", "a"]
        );
        assert_eq!(
            mentions("read @\"my notes/report.md\", then @src/main.rs and @\"my notes/report.md\""),
            vec!["my notes/report.md", "src/main.rs"]
        );
        for text in [
            "@\"my notes",
            "@\"my notes @other",
            "@\"bad\\q\"",
            "@\"bad\\",
            "@\"closed\"suffix",
            "@\"\"",
        ] {
            assert!(mentions(text).is_empty(), "{text:?}");
        }
        let text = "read @\"my notes/re";
        let query = mention_completion(text, text.len()).unwrap();
        assert_eq!(query.query, "my notes/re");
        assert_eq!(query.range, 5..text.len());
        let text = "read @\"my notes/\" tail";
        let cursor = text.find("/\"").unwrap() + 1;
        let query = mention_completion(text, cursor).unwrap();
        assert_eq!(query.query, "my notes/");
        assert_eq!(&text[query.range], "@\"my notes/\"");
        assert!(mention_completion(text, cursor + 1).is_none());
        assert!(mention_completion("@\"bad\\q", 7).is_none());
        let text = "@\"a\\\"b\\\\c\"";
        assert_eq!(mentions(text), vec!["a\"b\\c"]);
        let ranges = mention_ranges("x @\"ไทย 日本語\" and @src/main.rs.");
        assert_eq!(
            ranges
                .iter()
                .map(|r| &"x @\"ไทย 日本語\" and @src/main.rs."[r.clone()])
                .collect::<Vec<_>>(),
            vec!["@\"ไทย 日本語\"", "@src/main.rs."]
        );
    }
}
