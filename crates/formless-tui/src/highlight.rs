//! Small, dependency-free syntax highlighter for fenced code blocks.
//!
//! It is a tokenizer, not a parser: comments, strings, numbers, keywords,
//! literals, type-like and called identifiers, plus `key:`/`key =` for
//! config formats and tags for markup. Block comments carry across lines.
//! Unknown languages return `None` and the caller falls back to plain code.

use ratatui::prelude::*;

use super::fg;

struct Lang {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    keywords: &'static [&'static str],
    literals: &'static [&'static str],
    /// Keywords match regardless of case (SQL).
    ignore_case: bool,
    /// `key` before this separator at line start is a key (YAML/TOML/INI).
    key_sep: Option<char>,
    /// `<tag` / `</tag` are highlighted (HTML/XML).
    tags: bool,
    /// Strings that may span lines, by opening (= closing) delimiter:
    /// Python's `"""`, JS/Go backticks (#876).
    multiline: &'static [&'static str],
    /// Block comments nest (Rust) (#877).
    nested: bool,
    /// `r#"…"#` raw strings (Rust) (#877).
    raw_strings: bool,
    /// A `/` where a value is expected starts a regex literal (JS) (#877).
    regex: bool,
}

const NONE: &[&str] = &[];

const RUST: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    // `'` is a lifetime as often as a char literal, so it is not a quote.
    quotes: &['"'],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "static", "struct", "trait", "type", "unsafe", "use", "where",
        "while",
    ],
    literals: &[
        "true", "false", "self", "Self", "None", "Some", "Ok", "Err", "super",
    ],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: NONE,
    nested: true,
    raw_strings: true,
    regex: false,
};

const JS: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
    keywords: &[
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "static",
        "switch",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    literals: &["true", "false", "null", "undefined", "NaN", "this", "super"],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: &["`"],
    nested: false,
    raw_strings: false,
    regex: true,
};

const PYTHON: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def",
        "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in",
        "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try",
        "while", "with", "yield",
    ],
    literals: &["True", "False", "None", "self", "cls"],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: &["\"\"\"", "'''"],
    nested: false,
    raw_strings: false,
    regex: false,
};

const GO: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '`', '\''],
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    literals: &["true", "false", "nil", "iota"],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: &["`"],
    nested: false,
    raw_strings: false,
    regex: false,
};

const SHELL: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "alias", "case", "cd", "do", "done", "echo", "elif", "else", "end", "esac", "exit",
        "export", "fi", "for", "function", "if", "in", "local", "return", "set", "source", "then",
        "unset", "while",
    ],
    literals: &["true", "false"],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};

const C_LIKE: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "abstract",
        "auto",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "extends",
        "extern",
        "final",
        "for",
        "fun",
        "func",
        "goto",
        "if",
        "implements",
        "import",
        "include",
        "interface",
        "let",
        "namespace",
        "new",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "throw",
        "throws",
        "try",
        "typedef",
        "union",
        "using",
        "val",
        "var",
        "virtual",
        "void",
        "volatile",
        "while",
    ],
    literals: &[
        "true", "false", "null", "nullptr", "nil", "this", "self", "super",
    ],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: &["\"\"\""],
    nested: false,
    raw_strings: false,
    regex: false,
};

const JSON: Lang = Lang {
    line_comments: NONE,
    block_comment: None,
    quotes: &['"'],
    keywords: NONE,
    literals: &["true", "false", "null"],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};

/// YAML and TOML: `#` comments only, so `key: a;b` keeps its `;` (#877).
const CONFIG: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: NONE,
    literals: &["true", "false", "null", "yes", "no", "on", "off"],
    ignore_case: false,
    key_sep: Some(':'),
    tags: false,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};
/// INI and properties: `;` comments too.
const INI: Lang = Lang {
    line_comments: &["#", ";"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: NONE,
    literals: &["true", "false", "null", "yes", "no", "on", "off"],
    ignore_case: false,
    key_sep: Some(':'),
    tags: false,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};

const SQL: Lang = Lang {
    line_comments: &["--"],
    block_comment: Some(("/*", "*/")),
    quotes: &['\''],
    keywords: &[
        "add",
        "all",
        "alter",
        "and",
        "as",
        "asc",
        "begin",
        "by",
        "case",
        "commit",
        "create",
        "default",
        "delete",
        "desc",
        "distinct",
        "drop",
        "else",
        "end",
        "exists",
        "foreign",
        "from",
        "group",
        "having",
        "in",
        "index",
        "inner",
        "insert",
        "into",
        "is",
        "join",
        "key",
        "left",
        "like",
        "limit",
        "not",
        "offset",
        "on",
        "or",
        "order",
        "outer",
        "primary",
        "references",
        "returning",
        "right",
        "select",
        "set",
        "table",
        "then",
        "union",
        "update",
        "values",
        "when",
        "where",
        "with",
    ],
    literals: &["null", "true", "false"],
    ignore_case: true,
    key_sep: None,
    tags: false,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};

const MARKUP: Lang = Lang {
    line_comments: NONE,
    block_comment: Some(("<!--", "-->")),
    quotes: &['"', '\''],
    keywords: NONE,
    literals: NONE,
    ignore_case: false,
    key_sep: None,
    tags: true,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};

const CSS: Lang = Lang {
    line_comments: NONE,
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &["important", "media", "import", "supports", "keyframes"],
    literals: NONE,
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};

/// `--` comments: Lua, Haskell.
const SQL_DASH: Lang = Lang {
    line_comments: &["--"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "and", "case", "class", "data", "do", "else", "elseif", "end", "for", "function", "if",
        "import", "in", "instance", "let", "local", "module", "not", "of", "or", "repeat",
        "return", "then", "until", "where", "while",
    ],
    literals: &["true", "false", "nil", "True", "False", "Nothing"],
    ignore_case: false,
    key_sep: None,
    tags: false,
    multiline: NONE,
    nested: false,
    raw_strings: false,
    regex: false,
};

fn lang(name: &str) -> Option<&'static Lang> {
    Some(match name.to_ascii_lowercase().as_str() {
        "rust" | "rs" => &RUST,
        "js" | "javascript" | "jsx" | "mjs" | "cjs" | "ts" | "typescript" | "tsx" => &JS,
        "py" | "python" | "python3" => &PYTHON,
        "go" | "golang" => &GO,
        "sh" | "bash" | "zsh" | "fish" | "shell" | "console" | "ps" | "powershell"
        | "dockerfile" | "docker" | "make" | "makefile" | "mk" | "ruby" | "rb" | "r" | "elixir"
        | "ex" | "exs" | "perl" | "pl" => &SHELL,
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "java" | "kotlin" | "kt" | "swift" | "cs"
        | "c#" | "csharp" | "f#" | "fsharp" | "scala" | "dart" | "php" | "zig" | "objc" => &C_LIKE,
        "lua" | "haskell" | "hs" => &SQL_DASH,
        "json" | "jsonc" | "json5" => &JSON,
        "yaml" | "yml" | "toml" | "env" | "dotenv" => &CONFIG,
        "ini" | "conf" | "cfg" | "properties" | "editorconfig" | "gitconfig" => &INI,
        "sql" | "sqlite" | "postgres" | "postgresql" | "mysql" => &SQL,
        "html" | "xml" | "svg" | "vue" | "svelte" => &MARKUP,
        "css" | "scss" | "less" => &CSS,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
enum Tok {
    Plain,
    Comment,
    Str,
    Num,
    Keyword,
    Literal,
    Type,
    Call,
    Key,
}

pub(crate) struct Highlighter {
    lang: &'static Lang,
    color: bool,
    /// Open block comments (more than one only where they nest).
    in_block: usize,
    /// Inside a string that runs past the line: what closes it.
    in_str: Option<String>,
}

impl Highlighter {
    pub(crate) fn new(name: &str, color: bool) -> Option<Self> {
        lang(name).map(|lang| Self {
            lang,
            color,
            in_block: 0,
            in_str: None,
        })
    }

    fn style(&self, tok: Tok) -> Style {
        let c = self.color;
        match tok {
            Tok::Plain => Style::default(),
            Tok::Comment => super::ui::dim(c).add_modifier(Modifier::ITALIC),
            Tok::Str => fg(c, Color::Green),
            Tok::Num => fg(c, Color::LightYellow),
            Tok::Keyword => fg(c, Color::Magenta).add_modifier(Modifier::BOLD),
            Tok::Literal => fg(c, Color::LightMagenta),
            Tok::Type => fg(c, Color::Cyan),
            Tok::Call | Tok::Key => fg(c, Color::Blue),
        }
    }

    /// Highlight one line; open block comments and multi-line strings
    /// carry to the next call.
    pub(crate) fn line(&mut self, line: &str) -> Vec<Span<'static>> {
        let l = self.lang;
        let chars: Vec<char> = line.chars().collect();
        let at = |i: usize, pat: &str| {
            (i..)
                .zip(pat.chars())
                .all(|(k, p)| chars.get(k) == Some(&p))
        };
        let mut toks: Vec<(Tok, String)> = Vec::new();
        let mut push = |tok: Tok, s: &[char]| {
            if let Some((t, last)) = toks.last_mut() {
                if std::mem::discriminant(t) == std::mem::discriminant(&tok) {
                    last.extend(s);
                    return;
                }
            }
            toks.push((tok, s.iter().collect()));
        };
        let mut i = 0;
        // Config keys: `  name:` / `name =` at line start.
        if let Some(sep) = l.key_sep {
            let lead = chars.iter().take_while(|c| c.is_whitespace()).count();
            let end = (lead..chars.len()).find(|&j| chars[j] == sep || chars[j] == '=');
            if let Some(end) = end {
                let key = &chars[lead..end];
                let bare = !key.is_empty()
                    && key
                        .iter()
                        .all(|c| c.is_alphanumeric() || "_-. \"'[]".contains(*c));
                if bare && !l.line_comments.iter().any(|c| at(lead, c)) {
                    push(Tok::Plain, &chars[..lead]);
                    push(Tok::Key, key);
                    i = end;
                }
            }
        }
        // Where the string closed by `close` ends, scanning from `from`:
        // just past the delimiter, or None when it runs past the line.
        let close_at = |from: usize, close: &str| -> Option<usize> {
            let mut j = from;
            while j < chars.len() {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if at(j, close) {
                    return Some(j + close.chars().count());
                }
                j += 1;
            }
            None
        };
        while i < chars.len() {
            if let Some(close) = self.in_str.clone() {
                // Raw strings (`"#…` closers) have no escapes.
                let stop = if l.raw_strings {
                    (i..chars.len())
                        .find(|&j| at(j, &close))
                        .map(|j| j + close.chars().count())
                } else {
                    close_at(i, &close)
                };
                push(Tok::Str, &chars[i..stop.unwrap_or(chars.len())]);
                if stop.is_some() {
                    self.in_str = None;
                }
                i = stop.unwrap_or(chars.len());
                continue;
            }
            if self.in_block > 0 {
                let (open, close) = l.block_comment.expect("in_block implies a block comment");
                let mut j = i;
                while j < chars.len() && self.in_block > 0 {
                    if l.nested && at(j, open) {
                        self.in_block += 1;
                        j += open.chars().count();
                    } else if at(j, close) {
                        self.in_block -= 1;
                        j += close.chars().count();
                    } else {
                        j += 1;
                    }
                }
                push(Tok::Comment, &chars[i..j]);
                i = j;
                continue;
            }
            if let Some((open, _)) = l.block_comment {
                if at(i, open) {
                    self.in_block = 1;
                    push(Tok::Comment, &chars[i..i + open.chars().count()]);
                    i += open.chars().count();
                    continue;
                }
            }
            let c = chars[i];
            let word_start = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            // Rust raw strings: `r"…"`, `r#"…"#`, `br#"…"#` (#877).
            if l.raw_strings
                && word_start
                && (c == 'r' || (c == 'b' && chars.get(i + 1) == Some(&'r')))
            {
                let r = i + usize::from(c == 'b');
                let hashes = chars[r + 1..].iter().take_while(|&&h| h == '#').count();
                if chars.get(r + 1 + hashes) == Some(&'"') {
                    let close = format!("\"{}", "#".repeat(hashes));
                    let from = r + 2 + hashes;
                    let mut j = from;
                    let stop = loop {
                        if j >= chars.len() {
                            break None;
                        }
                        if at(j, &close) {
                            break Some(j + close.chars().count());
                        }
                        j += 1;
                    };
                    push(Tok::Str, &chars[i..stop.unwrap_or(chars.len())]);
                    if stop.is_none() {
                        self.in_str = Some(close);
                    }
                    i = stop.unwrap_or(chars.len());
                    continue;
                }
            }
            if l.line_comments.iter().any(|p| at(i, p))
                && (c != '#' || i == 0 || chars[i - 1].is_whitespace())
            {
                push(Tok::Comment, &chars[i..]);
                break;
            }
            // A JS regex literal where a value is expected, so the `//` in
            // `/https:\/\//` is not a comment (#877).
            if l.regex && c == '/' && !at(i, "//") && !at(i, "/*") {
                let before = chars[..i].iter().rev().find(|c| !c.is_whitespace());
                let prev_word: String = {
                    let end = chars[..i]
                        .iter()
                        .rposition(|c| !c.is_whitespace())
                        .map_or(0, |e| e + 1);
                    let start = chars[..end]
                        .iter()
                        .rposition(|c| !(c.is_alphanumeric() || *c == '_'))
                        .map_or(0, |s| s + 1);
                    chars[start..end].iter().collect()
                };
                let value_expected = before.is_none_or(|b| "=(,:[!&|?{};+-*%<>~^".contains(*b))
                    || matches!(
                        prev_word.as_str(),
                        "return" | "typeof" | "case" | "in" | "of" | "yield"
                    );
                if value_expected {
                    let (mut j, mut class) = (i + 1, false);
                    let mut closed = None;
                    while j < chars.len() {
                        match chars[j] {
                            '\\' => j += 1,
                            '[' => class = true,
                            ']' => class = false,
                            '/' if !class => {
                                closed = Some(j + 1);
                                break;
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    if let Some(mut end) = closed {
                        while end < chars.len() && chars[end].is_ascii_alphabetic() {
                            end += 1;
                        }
                        push(Tok::Str, &chars[i..end]);
                        i = end;
                        continue;
                    }
                }
            }
            // Strings that may span lines: `"""`, backticks (#876).
            if let Some(delim) = l.multiline.iter().find(|d| at(i, d)) {
                let n = delim.chars().count();
                let stop = close_at(i + n, delim);
                push(Tok::Str, &chars[i..stop.unwrap_or(chars.len())]);
                if stop.is_none() {
                    self.in_str = Some(delim.to_string());
                }
                i = stop.unwrap_or(chars.len());
                continue;
            }
            if l.quotes.contains(&c) {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != c {
                    j += if chars[j] == '\\' { 2 } else { 1 };
                }
                let stop = (j + 1).min(chars.len());
                push(Tok::Str, &chars[i..stop]);
                i = stop;
                continue;
            }
            if c.is_ascii_digit() && word_start {
                let mut j = i;
                while j < chars.len()
                    && (chars[j].is_ascii_alphanumeric() || "._".contains(chars[j]))
                {
                    j += 1;
                }
                push(Tok::Num, &chars[i..j]);
                i = j;
                continue;
            }
            if l.tags && c == '<' {
                let mut j = i + 1;
                if chars.get(j) == Some(&'/') {
                    j += 1;
                }
                let name_end = (j..chars.len())
                    .find(|&k| !(chars[k].is_alphanumeric() || "-:_".contains(chars[k])))
                    .unwrap_or(chars.len());
                if name_end > j {
                    push(Tok::Keyword, &chars[i..name_end]);
                    i = name_end;
                    continue;
                }
            }
            if c.is_alphabetic() || c == '_' || c == '$' {
                let mut j = i;
                while j < chars.len()
                    && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '$')
                {
                    j += 1;
                }
                let word: String = chars[i..j].iter().collect();
                let is = |set: &[&str]| {
                    if l.ignore_case {
                        set.iter().any(|k| k.eq_ignore_ascii_case(&word))
                    } else {
                        set.contains(&word.as_str())
                    }
                };
                let tok = if is(l.keywords) {
                    Tok::Keyword
                } else if is(l.literals) {
                    Tok::Literal
                } else if chars.get(j) == Some(&'(') {
                    Tok::Call
                } else if !l.ignore_case
                    && !l.tags
                    && word.starts_with(|c: char| c.is_uppercase())
                    && word.chars().any(|c| c.is_lowercase())
                {
                    Tok::Type
                } else {
                    Tok::Plain
                };
                push(tok, &chars[i..j]);
                i = j;
                continue;
            }
            push(Tok::Plain, &chars[i..i + 1]);
            i += 1;
        }
        toks.into_iter()
            .map(|(t, s)| Span::styled(s, self.style(t)))
            .collect()
    }
}

/// Unified-diff colouring: additions, removals, hunks and file headers.
pub(crate) fn diff_line(line: &str, color: bool) -> Span<'static> {
    let style = if line.starts_with("+++") || line.starts_with("---") {
        super::bold_fg(color, Color::White)
    } else if line.starts_with('+') {
        fg(color, Color::Green)
    } else if line.starts_with('-') {
        fg(color, Color::Red)
    } else if line.starts_with("@@") {
        fg(color, Color::Cyan)
    } else {
        Style::default()
    };
    Span::styled(line.to_string(), style)
}

/// Whether `text` is (or contains) a unified diff: a hunk header and
/// file headers or a `diff --git` line.
pub(crate) fn is_unified_diff(text: &str) -> bool {
    let mut hunk = false;
    let mut header = false;
    for l in text.lines() {
        hunk |= l.starts_with("@@ ");
        header |= l.starts_with("--- ") || l.starts_with("+++ ") || l.starts_with("diff --git");
        if hunk && header {
            return true;
        }
    }
    false
}

/// A diff's lines, coloured, with runs of unchanged context longer than
/// `2 * CONTEXT` collapsed to their first and last `CONTEXT` lines.
pub(crate) fn diff_lines(text: &str, color: bool) -> Vec<Vec<Span<'static>>> {
    const CONTEXT: usize = 3;
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let context = |l: &str| l.starts_with(' ') || l.is_empty();
        if context(lines[i]) {
            let start = i;
            while i < lines.len() && context(lines[i]) {
                i += 1;
            }
            let run = &lines[start..i];
            if run.len() > 2 * CONTEXT + 1 {
                out.extend(run[..CONTEXT].iter().map(|l| vec![diff_line(l, color)]));
                out.push(vec![Span::styled(
                    format!(" ⋯ {} unchanged lines", run.len() - 2 * CONTEXT),
                    super::ui::dim(color),
                )]);
                out.extend(
                    run[run.len() - CONTEXT..]
                        .iter()
                        .map(|l| vec![diff_line(l, color)]),
                );
            } else {
                out.extend(run.iter().map(|l| vec![diff_line(l, color)]));
            }
        } else {
            out.push(vec![diff_line(lines[i], color)]);
            i += 1;
        }
    }
    out
}

/// `+added −removed` for a diff (file headers excluded).
pub(crate) fn diff_stat(text: &str) -> (usize, usize) {
    text.lines().fold((0, 0), |(a, r), l| {
        if l.starts_with('+') && !l.starts_with("+++") {
            (a + 1, r)
        } else if l.starts_with('-') && !l.starts_with("---") {
            (a, r + 1)
        } else {
            (a, r)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_are_detected_coloured_and_collapsed() {
        let mut d = String::from("--- a/x.rs\n+++ b/x.rs\n@@ -1,12 +1,12 @@\n");
        for n in 0..10 {
            d.push_str(&format!(" line {n}\n"));
        }
        d.push_str("-old\n+new\n");
        assert!(is_unified_diff(&d));
        assert!(!is_unified_diff("just\n@@ text"));
        assert_eq!(diff_stat(&d), (1, 1));
        let out = diff_lines(&d, true);
        let text: Vec<String> = out
            .iter()
            .map(|l| l.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            text.contains(&" ⋯ 4 unchanged lines".to_string()),
            "{text:#?}"
        );
        assert_eq!(text.len(), 3 + 3 + 1 + 3 + 2);
        let added = out.iter().find(|l| l[0].content == "+new").unwrap();
        assert_eq!(added[0].style.fg, Some(Color::Green));
    }

    fn toks(lang: &str, line: &str) -> Vec<(String, Option<Color>)> {
        let mut h = Highlighter::new(lang, true).unwrap();
        h.line(line)
            .into_iter()
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect()
    }

    #[test]
    fn tokenizes_common_shapes() {
        let t = toks("rust", "let x = foo(\"hi\", 42); // done");
        assert!(t.contains(&("let".into(), Some(Color::Magenta))));
        assert!(t.contains(&("foo".into(), Some(Color::Blue))));
        assert!(t.contains(&("\"hi\"".into(), Some(Color::Green))));
        assert!(t.contains(&("42".into(), Some(Color::LightYellow))));
        assert!(t.contains(&("// done".into(), Some(Color::DarkGray))));
        let text: String = t.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(text, "let x = foo(\"hi\", 42); // done", "no text is lost");
        // Lifetimes are not strings.
        let t = toks("rust", "fn f<'a>(x: &'a str) -> Vec<u8>");
        assert!(t.contains(&("Vec".into(), Some(Color::Cyan))));
        assert!(Highlighter::new("brainfuck", true).is_none());
    }

    #[test]
    fn block_comments_span_lines_and_config_keys() {
        let mut h = Highlighter::new("js", true).unwrap();
        h.line("a /* start");
        let mid = h.line("still comment");
        assert_eq!(mid[0].style.fg, Some(Color::DarkGray));
        let end = h.line("end */ b");
        assert_eq!(end[0].content, "end */");
        let t = toks("yaml", "  name: \"formless\" # c");
        assert!(t.contains(&("name".into(), Some(Color::Blue))));
        assert!(t.contains(&("\"formless\"".into(), Some(Color::Green))));
        let t = toks("sql", "SELECT id FROM t");
        assert!(t.contains(&("SELECT".into(), Some(Color::Magenta))));
    }

    fn all_str(h: &mut Highlighter, line: &str) -> bool {
        h.line(line)
            .iter()
            .all(|s| s.style.fg == Some(Color::Green))
    }

    /// Strings that run across lines stay strings until they close (#876).
    #[test]
    fn multi_line_strings_carry_across_lines() {
        let mut h = Highlighter::new("python", true).unwrap();
        let first = h.line("    \"\"\"doc with \"q\" inside");
        assert!(
            first
                .iter()
                .skip(1)
                .all(|s| s.style.fg == Some(Color::Green)),
            "{first:?}"
        );
        assert!(all_str(&mut h, "    return is just doc text"));
        let end = h.line("    \"\"\"");
        assert!(end
            .iter()
            .any(|s| s.content.contains("\"\"\"") && s.style.fg == Some(Color::Green)));
        let after = h.line("return x");
        assert_eq!(
            after[0].style.fg,
            Some(Color::Magenta),
            "code again after the docstring"
        );

        let mut h = Highlighter::new("js", true).unwrap();
        h.line("const t = `start");
        assert!(all_str(&mut h, "let 1 inside"));
        let end = h.line("end`; let y = 2;");
        assert_eq!(
            (end[0].content.as_ref(), end[0].style.fg),
            ("end`", Some(Color::Green))
        );
        assert!(end
            .iter()
            .any(|s| s.content == "let" && s.style.fg == Some(Color::Magenta)));

        let mut h = Highlighter::new("go", true).unwrap();
        h.line("s := `raw");
        assert!(all_str(&mut h, "func not code"));
        // One-line strings are unchanged.
        let t = toks("python", "x = \"a\" + 'b'");
        assert!(t.contains(&("\"a\"".into(), Some(Color::Green))));
    }

    /// Nested Rust comments, raw strings, JS regexes, YAML `;` and
    /// unknown languages (#877).
    #[test]
    fn traps_that_used_to_mis_colour() {
        let t = toks("rust", "/* a /* b */ still comment */ let x = 1;");
        assert_eq!(
            t[0],
            (
                "/* a /* b */ still comment */".into(),
                Some(Color::DarkGray)
            ),
            "{t:?}"
        );
        assert!(t.contains(&("let".into(), Some(Color::Magenta))));
        let t = toks("rust", "let s = r#\"a \"b\" c\"#;");
        assert!(
            t.contains(&("r#\"a \"b\" c\"#".into(), Some(Color::Green))),
            "{t:?}"
        );
        let t = toks("js", "const re = /https:\\/\\//; let z = 1;");
        assert!(
            t.contains(&("/https:\\/\\//".into(), Some(Color::Green))),
            "{t:?}"
        );
        assert!(t.contains(&("let".into(), Some(Color::Magenta))), "{t:?}");
        // Division is still division.
        let t = toks("js", "let q = a / b / c;");
        assert!(!t.iter().any(|(_, c)| *c == Some(Color::Green)), "{t:?}");
        let t = toks("yaml", "key: a;b");
        assert!(!t.iter().any(|(_, c)| *c == Some(Color::DarkGray)), "{t:?}");
        let t = toks("ini", "; comment");
        assert_eq!(t[0].1, Some(Color::DarkGray));
        for name in [
            "ruby",
            "lua",
            "c#",
            "dockerfile",
            "make",
            "haskell",
            "elixir",
            "r",
        ] {
            assert!(Highlighter::new(name, true).is_some(), "{name}");
        }
    }
}
