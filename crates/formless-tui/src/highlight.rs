//! Small, dependency-free syntax highlighter for fenced code blocks.
//!
//! It is a tokenizer, not a parser: comments, strings, numbers, keywords,
//! literals, type-like and called identifiers, plus `key:`/`key =` for
//! config formats and tags for markup. Block comments carry across lines.
//! Unknown languages return `None` and the caller falls back to plain code.

use std::collections::VecDeque;

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

const POWERSHELL: Lang = Lang {
    block_comment: Some(("<#", "#>")),
    ..SHELL
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

// PHP hash comments are distinct from C preprocessor directives.
const PHP: Lang = Lang {
    line_comments: &["//", "#"],
    ..C_LIKE
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

// Strict JSON stays comment-free; the extensions opt into their own grammar.
const JSONC: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    ..JSON
};
const JSON5: Lang = Lang {
    quotes: &['"', '\''],
    ..JSONC
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
const TOML: Lang = Lang {
    multiline: &["\"\"\"", "'''"],
    ..CONFIG
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

/// MySQL requires whitespace or a control character after a double dash.
const MYSQL: Lang = Lang {
    line_comments: &["--", "#"],
    quotes: &['\'', '"', '`'],
    multiline: &["'", "\"", "`"],
    ..SQL
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
        "sh" | "bash" | "zsh" | "fish" | "shell" | "console" | "dockerfile" | "docker" | "make"
        | "makefile" | "mk" | "ruby" | "rb" | "r" | "elixir" | "ex" | "exs" | "perl" | "pl" => {
            &SHELL
        }
        "ps" | "powershell" => &POWERSHELL,
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "java" | "kotlin" | "kt" | "swift" | "cs"
        | "c#" | "csharp" | "f#" | "fsharp" | "scala" | "dart" | "zig" | "objc" => &C_LIKE,
        "php" => &PHP,
        "lua" | "haskell" | "hs" => &SQL_DASH,
        "json" => &JSON,
        "jsonc" => &JSONC,
        "json5" => &JSON5,
        "yaml" | "yml" | "env" | "dotenv" => &CONFIG,
        "toml" => &TOML,
        "ini" | "conf" | "cfg" | "properties" | "editorconfig" | "gitconfig" => &INI,
        "sql" | "sqlite" | "postgres" | "postgresql" => &SQL,
        "mysql" => &MYSQL,
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
    rust_numbers: bool,
    powershell: bool,
    python_comments: bool,
    shell: Option<ShellComments>,
    raw_backticks: bool,
    toml_strings: bool,
    mysql_comments: bool,
    php_comments: bool,
    json5_strings: bool,
    color: bool,
    /// Open block comments (more than one only where they nest).
    in_block: usize,
    /// Inside a string that runs past the line: what closes it.
    in_str: Option<String>,
    js_typescript: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ShellContext {
    Command,
    Parameter,
    Pattern,
    Condition,
    RegexStart,
    Regex(usize),
    CaseHeader,
    CasePattern { started: bool },
    CaseBody,
}

struct HereDoc {
    delimiter: String,
    strip_tabs: bool,
    quoted: bool,
    continued: String,
}

struct ShellComments {
    posix: bool,
    ansi_quotes: bool,
    localized_quotes: bool,
    contexts: Vec<ShellContext>,
    here_docs: VecDeque<HereDoc>,
    in_here_doc: bool,
    line_boundary: bool,
    ansi_quote: bool,
}

impl ShellComments {
    // Reserved words such as `[[` and `case` also occur as plain arguments.
    fn command_position(mut prefix: &[char], line_boundary: bool) -> bool {
        loop {
            let Some(end) = prefix.iter().rposition(|c| !c.is_whitespace()) else {
                return line_boundary;
            };
            if ";|&({)".contains(prefix[end]) {
                return !Self::escaped(&prefix[..end]);
            }
            let start = prefix[..=end]
                .iter()
                .rposition(|c| c.is_whitespace() || ";|&({)".contains(*c))
                .map_or(0, |i| i + 1);
            let word: String = prefix[start..=end].iter().collect();
            if !matches!(
                word.as_str(),
                "if" | "elif" | "then" | "else" | "do" | "while" | "until" | "!" | "time"
            ) {
                return false;
            }
            prefix = &prefix[..start];
        }
    }

    fn escaped(prefix: &[char]) -> bool {
        !prefix
            .iter()
            .rev()
            .take_while(|&&c| c == '\\')
            .count()
            .is_multiple_of(2)
    }

    fn ansi_escape(chars: &[char], end: &mut usize, bytes: &mut Vec<u8>) -> Option<()> {
        let c = *chars.get(*end)?;
        *end += 1;
        let simple = match c {
            'a' => Some(7),
            'b' => Some(8),
            'e' | 'E' => Some(27),
            'f' => Some(12),
            'n' => Some(b'\n'),
            'r' => Some(b'\r'),
            't' => Some(b'\t'),
            'v' => Some(11),
            '\\' | '\'' | '"' | '?' => Some(c as u8),
            _ => None,
        };
        if let Some(byte) = simple {
            bytes.push(byte);
            return Some(());
        }
        let (radix, digits) = match c {
            'c' => {
                let c = chars.get(*end)?.to_ascii_uppercase();
                *end += 1;
                return c.is_ascii().then(|| bytes.push((c as u8) ^ 0x40));
            }
            '0'..='7' => {
                *end -= 1;
                (8, 3)
            }
            'x' => (16, 2),
            'u' => (16, 4),
            'U' => (16, 8),
            _ => {
                *end -= 1;
                bytes.push(b'\\');
                return Some(());
            }
        };
        let start = *end;
        let mut value = 0;
        while *end - start < digits {
            let Some(digit) = chars.get(*end).and_then(|c| c.to_digit(radix)) else {
                break;
            };
            value = value * radix + digit;
            *end += 1;
        }
        if *end == start {
            *end -= 1;
            bytes.push(b'\\');
        } else if matches!(c, 'u' | 'U') {
            bytes.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            bytes.push(value as u8);
        }
        Some(())
    }

    fn context(&self) -> ShellContext {
        self.contexts
            .last()
            .copied()
            .unwrap_or(ShellContext::Command)
    }

    fn track(&mut self, chars: &[char], i: usize) {
        use ShellContext::*;

        let prefix = &chars[..i];
        let current = chars[i];
        let context = self.context();
        if (current == '\\' && context != RegexStart) || Self::escaped(prefix) {
            return;
        }
        let previous = prefix.last().copied();
        let next = chars.get(i + 1).copied();
        let mut replace = |mode| *self.contexts.last_mut().expect("context exists") = mode;
        match (context, current) {
            (RegexStart, c) if !c.is_whitespace() => replace(Regex(usize::from(c == '('))),
            (Regex(depth), '(') => replace(Regex(depth + 1)),
            (Regex(depth), ')') => replace(Regex(depth.saturating_sub(1))),
            (Regex(0), c) if c.is_whitespace() => replace(Condition),
            (Regex(_) | RegexStart, _) => {}
            (Condition, '~')
                if previous == Some('=')
                    && prefix[..i - 1].last().is_none_or(|c| c.is_whitespace()) =>
            {
                replace(RegexStart);
            }
            (Condition, ']')
                if next == Some(']')
                    && previous.is_none_or(char::is_whitespace)
                    && chars
                        .get(i + 2)
                        .is_none_or(|c| c.is_whitespace() || ";|&)".contains(*c)) =>
            {
                self.contexts.pop();
            }
            (Parameter, '}') | (Pattern, ')') | (Command, ')') => {
                self.contexts.pop();
            }
            (CasePattern { .. }, ')') => replace(CaseBody),
            (CaseBody, ';') if previous == Some(';') && next != Some('&') => {
                replace(CasePattern { started: false })
            }
            (CaseBody, '&') if previous == Some(';') => replace(CasePattern { started: false }),
            (_, '{') if previous == Some('$') && !Self::escaped(&prefix[..i - 1]) => {
                self.contexts.push(Parameter);
            }
            (_, '(')
                if context == Pattern
                    || previous.is_some_and(|c| "@!?+*$".contains(c))
                        && !Self::escaped(&prefix[..i - 1]) =>
            {
                let mode = if context != Pattern && previous == Some('$') && next != Some('(') {
                    Command
                } else {
                    Pattern
                };
                self.contexts.push(mode);
            }
            (CasePattern { started: false }, '(' | '|') => replace(CasePattern { started: true }),
            (Command | CaseBody, '(') => {
                self.contexts
                    .push(if next == Some('(') { Pattern } else { Command })
            }
            (Command | CaseBody, '[')
                if next == Some('[')
                    && previous.is_none_or(|c| c.is_whitespace() || ";|&(".contains(c))
                    && chars.get(i + 2).is_none_or(|c| c.is_whitespace())
                    && Self::command_position(prefix, self.line_boundary) =>
            {
                self.contexts.push(Condition);
            }
            _ => {}
        }
    }

    fn word(&mut self, chars: &[char], start: usize, end: usize, word: &str) {
        use ShellContext::*;

        if !self.posix
            || !matches!(word, "case" | "in" | "esac")
            || chars
                .get(start.wrapping_sub(1))
                .is_some_and(|c| !c.is_whitespace() && !";|&({)".contains(*c))
            || chars
                .get(end)
                .is_some_and(|c| !c.is_whitespace() && !";|&()".contains(*c))
            || Self::escaped(&chars[..start.saturating_sub(1)])
        {
            return;
        }
        match (self.context(), word) {
            (Command | CaseBody, "case")
                if Self::command_position(&chars[..start], self.line_boundary) =>
            {
                self.contexts.push(CaseHeader);
            }
            (CaseHeader, "in") => {
                *self.contexts.last_mut().expect("case header exists") =
                    CasePattern { started: false };
            }
            (CasePattern { started: false }, "esac") => {
                self.contexts.pop();
            }
            (CaseBody, "esac") if Self::command_position(&chars[..start], self.line_boundary) => {
                self.contexts.pop();
            }
            _ => {}
        }
    }

    fn comment(&self, prefix: &[char]) -> bool {
        use ShellContext::*;

        let context = self.context();
        if matches!(context, Parameter | Pattern | Regex(_)) {
            return false;
        }
        let Some((&previous, before)) = prefix.split_last() else {
            return self.line_boundary;
        };
        let separator = match context {
            Command | CaseBody => ";|&(".contains(previous),
            CasePattern { .. } => {
                "|(".contains(previous) || before.last() == Some(&';') && ";&".contains(previous)
            }
            Condition | RegexStart => {
                "|&".contains(previous)
                    && before.last() == Some(&previous)
                    && !Self::escaped(&before[..before.len() - 1])
            }
            _ => false,
        };
        (previous.is_whitespace() || separator) && !Self::escaped(before)
    }

    fn end_line(&mut self, chars: &[char], ended_by_comment: bool, in_string: bool) {
        let chars = chars.strip_suffix(&['\r']).unwrap_or(chars);
        let continued = !ended_by_comment && Self::escaped(chars);
        self.line_boundary = !continued || self.comment(&chars[..chars.len() - 1]);
        if !continued && !in_string {
            if let Some(context @ ShellContext::Regex(0)) = self.contexts.last_mut() {
                *context = ShellContext::Condition;
            }
            self.in_here_doc = !self.here_docs.is_empty();
        }
    }

    fn here_doc(&mut self, chars: &[char], i: usize) -> Option<usize> {
        use ShellContext::*;

        if chars[i] != '<'
            || !self.posix
            || !matches!(self.context(), Command | CaseBody)
            || Self::escaped(&chars[..i])
            || chars.get(i + 1) != Some(&'<')
            || chars.get(i + 2) == Some(&'<')
            || i > 0 && chars[i - 1] == '<' && !Self::escaped(&chars[..i - 1])
        {
            return None;
        }
        let mut end = i + 2;
        let strip_tabs = chars.get(end) == Some(&'-');
        end += usize::from(strip_tabs);
        while chars.get(end).is_some_and(|c| " \t".contains(*c)) {
            end += 1;
        }
        let start = end;
        let mut quote = None;
        let mut quoted = false;
        let mut ansi_quote = false;
        let mut delimiter = Vec::new();
        if chars.get(end) == Some(&'#') {
            return None;
        }
        while let Some(&c) = chars.get(end) {
            if quote.is_none() && (c.is_whitespace() || ";|&()<>".contains(c)) {
                break;
            }
            end += 1;
            if quote == Some(c) {
                quote = None;
                ansi_quote = false;
            } else if quote.is_none()
                && c == '$'
                && (self.ansi_quotes && chars.get(end) == Some(&'\'')
                    || self.localized_quotes && chars.get(end) == Some(&'"'))
            {
                quote = chars.get(end).copied();
                quoted = true;
                ansi_quote = quote == Some('\'');
                end += 1;
            } else if quote.is_none() && "\"'".contains(c) {
                quote = Some(c);
                quoted = true;
            } else if c == '\\' && ansi_quote {
                Self::ansi_escape(chars, &mut end, &mut delimiter)?;
            } else if c == '\\'
                && quote != Some('\'')
                && (quote.is_none() || chars.get(end).is_some_and(|c| "\\\"$`".contains(*c)))
            {
                delimiter.extend_from_slice(chars.get(end)?.encode_utf8(&mut [0; 4]).as_bytes());
                quoted = true;
                end += 1;
            } else {
                delimiter.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        if end == start || quote.is_some() || delimiter.contains(&0) {
            return None;
        }
        let delimiter = String::from_utf8(delimiter).ok()?;
        self.here_docs.push_back(HereDoc {
            delimiter,
            strip_tabs,
            quoted,
            continued: String::new(),
        });
        Some(end)
    }

    fn literal_line(&mut self, line: &str) -> bool {
        if !self.in_here_doc {
            return false;
        }
        let doc = self.here_docs.front_mut().expect("active here document");
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !doc.quoted
            && !line
                .bytes()
                .rev()
                .take_while(|&b| b == b'\\')
                .count()
                .is_multiple_of(2)
        {
            doc.continued.push_str(&line[..line.len() - 1]);
            return true;
        }
        let line = if doc.continued.is_empty() {
            line
        } else {
            doc.continued.push_str(line);
            &doc.continued
        };
        if (if doc.strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line
        }) == doc.delimiter
        {
            self.here_docs.pop_front();
            self.in_here_doc = !self.here_docs.is_empty();
        } else {
            doc.continued.clear();
        }
        true
    }
}

/// Rust-shaped literals only. A dot before another dot or an identifier
/// belongs to range/member syntax, not the preceding number.
fn rust_number_end(chars: &[char], start: usize) -> usize {
    let mut end = start;
    let radix = if chars.get(start) == Some(&'0') {
        match chars.get(start + 1) {
            Some('x' | 'X') => Some(16),
            Some('o' | 'O') => Some(8),
            Some('b' | 'B') => Some(2),
            _ => None,
        }
    } else {
        None
    };
    if let Some(radix) = radix {
        end += 2;
        while chars
            .get(end)
            .is_some_and(|c| c.is_digit(radix) || *c == '_')
        {
            end += 1;
        }
    } else {
        while chars
            .get(end)
            .is_some_and(|c| c.is_ascii_digit() || *c == '_')
        {
            end += 1;
        }
        if chars.get(end) == Some(&'.')
            && chars
                .get(end + 1)
                .is_none_or(|c| *c != '.' && *c != '_' && !c.is_alphabetic())
        {
            end += 1;
            while chars
                .get(end)
                .is_some_and(|c| c.is_ascii_digit() || *c == '_')
            {
                end += 1;
            }
        }
        if matches!(chars.get(end), Some('e' | 'E')) {
            let mut exponent = end + 1;
            if matches!(chars.get(exponent), Some('+' | '-')) {
                exponent += 1;
            }
            let mut digit = false;
            while let Some(c) = chars
                .get(exponent)
                .filter(|c| c.is_ascii_digit() || **c == '_')
            {
                digit |= c.is_ascii_digit();
                exponent += 1;
            }
            if digit {
                end = exponent;
            }
        }
    }
    for suffix in [
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        "f32", "f64",
    ] {
        let suffix_end = end + suffix.len();
        if chars
            .get(end..suffix_end)
            .is_some_and(|slice| slice.iter().copied().eq(suffix.chars()))
            && chars
                .get(suffix_end)
                .is_none_or(|c| !c.is_alphanumeric() && *c != '_')
        {
            return suffix_end;
        }
    }
    end
}

// After a contextual `await`, an apparent closing slash can instead be the
// second division operator. Check the flags and the next operand token.
fn powershell_quote_end(chars: &[char], mut i: usize, quote: char) -> Option<usize> {
    while let Some(&c) = chars.get(i) {
        if (quote == '"' && c == '`') || (c == quote && chars.get(i + 1) == Some(&quote)) {
            i += 2;
        } else if c == quote {
            return Some(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

fn js_regex_suffix(flags: &[char], mut suffix: &[char], typescript: bool) -> bool {
    if !flags.iter().all(|c| "dgimsuvy".contains(*c)) {
        return false;
    }
    loop {
        let whitespace = suffix.iter().take_while(|c| c.is_whitespace()).count();
        suffix = &suffix[whitespace..];
        if !suffix.starts_with(&['/', '*']) {
            break;
        }
        let Some(end) = suffix[2..].windows(2).position(|pair| pair == ['*', '/']) else {
            return true;
        };
        suffix = &suffix[end + 4..];
    }
    let word: String = suffix
        .iter()
        .take_while(|c| c.is_alphanumeric() || "_$".contains(**c))
        .collect();
    (matches!(word.as_str(), "" | "in" | "instanceof")
        || typescript && matches!(word.as_str(), "as" | "satisfies"))
        && !matches!(suffix.first(), Some('"' | '\''))
}

impl Highlighter {
    pub(crate) fn new(name: &str, color: bool) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        lang(&name).map(|lang| Self {
            lang,
            rust_numbers: matches!(name.as_str(), "rust" | "rs"),
            powershell: matches!(name.as_str(), "ps" | "powershell"),
            python_comments: matches!(name.as_str(), "py" | "python" | "python3"),
            shell: matches!(
                name.as_str(),
                "sh" | "bash" | "zsh" | "fish" | "shell" | "console"
            )
            .then(|| ShellComments {
                posix: name != "fish",
                ansi_quotes: matches!(name.as_str(), "bash" | "zsh"),
                localized_quotes: name == "bash",
                contexts: Vec::new(),
                here_docs: VecDeque::new(),
                in_here_doc: false,
                line_boundary: true,
                ansi_quote: false,
            }),
            raw_backticks: matches!(name.as_str(), "go" | "golang"),
            toml_strings: name == "toml",
            mysql_comments: name == "mysql",
            php_comments: name == "php",
            json5_strings: name == "json5",
            color,
            in_block: 0,
            in_str: None,
            js_typescript: matches!(name.as_str(), "ts" | "tsx" | "typescript"),
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
        if self
            .shell
            .as_mut()
            .is_some_and(|shell| shell.literal_line(line))
        {
            return vec![Span::raw(line.to_owned())];
        }
        let l = self.lang;
        let toml = self.toml_strings;
        let mysql = self.mysql_comments;
        let php = self.php_comments;
        let chars: Vec<char> = line.chars().collect();
        let continues_string =
            self.json5_strings && line.bytes().rev().take_while(|&b| b == b'\\').count() % 2 != 0;
        let at = |i: usize, pat: &str| {
            (i..)
                .zip(pat.chars())
                .all(|(k, p)| chars.get(k) == Some(&p))
        };
        let mut toks: Vec<(Tok, String)> = Vec::new();
        let mut js_block_comments = Vec::new();
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
        let mut ended_by_comment = false;
        // Config keys: `  name:` / `name =` at line start.
        if let Some(sep) = l.key_sep.filter(|_| self.in_str.is_none()) {
            let lead = chars.iter().take_while(|c| c.is_whitespace()).count();
            let end = (lead..chars.len()).find(|&j| chars[j] == sep || chars[j] == '=');
            if let Some(end) = end {
                let key = &chars[lead..end];
                let bare = !key.is_empty()
                    && !l.multiline.iter().any(|delimiter| at(lead, delimiter))
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
        let raw_backticks = self.raw_backticks;
        let shell = self.shell.is_some();
        let posix_shell = self.shell.as_ref().is_some_and(|shell| shell.posix);
        let close_at = |from: usize, close: &str, ansi_quote: bool| -> Option<usize> {
            let escapes = !(l.raw_strings
                || raw_backticks && close == "`"
                || posix_shell && close == "'" && !ansi_quote
                || toml && close.starts_with('\'')
                || mysql && close == "`");
            let mut j = from;
            while j < chars.len() {
                if chars[j] == '\\' && escapes {
                    j += 2;
                    continue;
                }
                if at(j, close) {
                    let end = j + close.chars().count();
                    // TOML allows up to two quotes before the closing delimiter.
                    let trailing = if toml {
                        chars[end..]
                            .iter()
                            .take(2)
                            .take_while(|&&c| c == chars[j])
                            .count()
                    } else {
                        0
                    };
                    return Some(end + trailing);
                }
                j += 1;
            }
            None
        };
        while i < chars.len() {
            if let Some(close) = self.in_str.clone() {
                let ansi_quote = self.shell.as_ref().is_some_and(|shell| shell.ansi_quote);
                let stop = if self.powershell && close.ends_with('@') {
                    (i == 0 && at(0, &close)).then_some(2)
                } else if self.powershell {
                    powershell_quote_end(&chars, i, close.chars().next().unwrap())
                } else {
                    close_at(i, &close, ansi_quote)
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
                if l.regex {
                    js_block_comments.push((i, j));
                }
                i = j;
                continue;
            }
            if let Some((open, _)) = l.block_comment {
                if at(i, open) {
                    self.in_block = 1;
                    let end = i + open.chars().count();
                    push(Tok::Comment, &chars[i..end]);
                    if l.regex {
                        js_block_comments.push((i, end));
                    }
                    i = end;
                    continue;
                }
            }
            let c = chars[i];
            let word_start = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            // Rust raw strings, including byte and C strings.
            if l.raw_strings
                && word_start
                && (c == 'r' || (matches!(c, 'b' | 'c') && chars.get(i + 1) == Some(&'r')))
            {
                let r = i + usize::from(c != 'r');
                let hashes = chars[r + 1..].iter().take_while(|&&h| h == '#').count();
                if chars.get(r + 1 + hashes) == Some(&'"') {
                    let close = format!("\"{}", "#".repeat(hashes));
                    let stop = close_at(r + 2 + hashes, &close, false);
                    push(Tok::Str, &chars[i..stop.unwrap_or(chars.len())]);
                    if stop.is_none() {
                        self.in_str = Some(close);
                    }
                    i = stop.unwrap_or(chars.len());
                    continue;
                }
            }
            let hash_is_comment = || {
                self.shell.as_ref().map_or_else(
                    || self.python_comments || i == 0 || chars[i - 1].is_whitespace(),
                    |shell| shell.comment(&chars[..i]),
                )
            };
            let hash_comment = c != '#'
                || if php {
                    chars.get(i + 1) != Some(&'[')
                } else {
                    mysql
                        || hash_is_comment()
                        || self.powershell
                        || toml && i > 0 && l.quotes.contains(&chars[i - 1])
                };
            if hash_comment
                && l.line_comments.iter().any(|p| {
                    at(i, p)
                        && (!mysql
                            || *p != "--"
                            || chars
                                .get(i + 2)
                                .is_none_or(|c| c.is_ascii_whitespace() || c.is_ascii_control()))
                })
            {
                // A PHP closing tag ends a line comment as well as the code.
                let end = if php {
                    (i..chars.len())
                        .find(|&j| at(j, "?>"))
                        .unwrap_or(chars.len())
                } else {
                    chars.len()
                };
                push(Tok::Comment, &chars[i..end]);
                ended_by_comment = true;
                i = end;
                continue;
            }
            if let Some(shell) = &mut self.shell {
                if let Some(end) = shell.here_doc(&chars, i) {
                    push(Tok::Plain, &chars[i..end]);
                    i = end;
                    continue;
                }
                shell.track(&chars, i);
                if l.quotes.contains(&c) && !ShellComments::escaped(&chars[..i]) {
                    let ansi_quote = shell.ansi_quotes
                        && c == '\''
                        && i > 0
                        && chars[i - 1] == '$'
                        && !ShellComments::escaped(&chars[..i - 1]);
                    let close = c.to_string();
                    let stop = close_at(i + 1, &close, ansi_quote);
                    push(Tok::Str, &chars[i..stop.unwrap_or(chars.len())]);
                    if stop.is_none() {
                        self.in_str = Some(close);
                        shell.ansi_quote = ansi_quote;
                    }
                    i = stop.unwrap_or(chars.len());
                    continue;
                }
            }
            if self.powershell && c == '@' {
                if let Some(quote @ ('"' | '\'')) = chars.get(i + 1) {
                    if chars[i + 2..].iter().all(|c| c.is_whitespace()) {
                        self.in_str = Some(format!("{quote}@"));
                        push(Tok::Str, &chars[i..]);
                        break;
                    }
                }
            }
            // A JS regex literal where a value is expected, so the `//` in
            // `/https:\/\//` is not a comment (#877).
            if l.regex && c == '/' && !at(i, "//") && !at(i, "/*") {
                let token_end = |mut end| loop {
                    end = chars[..end]
                        .iter()
                        .rposition(|c| !c.is_whitespace())
                        .map_or(0, |e| e + 1);
                    match js_block_comments.binary_search_by_key(&end, |&(_, end)| end) {
                        Ok(comment) => end = js_block_comments[comment].0,
                        Err(_) => break end,
                    }
                };
                let end = token_end(i);
                let before = chars[..end].last();
                let start = chars[..end]
                    .iter()
                    .rposition(|c| !(c.is_alphanumeric() || "_$".contains(*c)))
                    .map_or(0, |s| s + 1);
                let prev_word: String = chars[start..end].iter().collect();
                // A property name after a dot or a private-name marker
                // stays an operand even when it spells a keyword.
                let word_boundary = chars[..start]
                    .last()
                    .is_none_or(|c| c.is_whitespace() || c.is_ascii_punctuation());
                let before_word = &chars[..token_end(start)];
                let member_name = before_word.ends_with(&['#'])
                    || before_word.ends_with(&['.']) && !before_word.ends_with(&['.', '.', '.']);
                let after_keyword = word_boundary
                    && !member_name
                    && matches!(
                        prev_word.as_str(),
                        "await"
                            | "case"
                            | "delete"
                            | "do"
                            | "else"
                            | "extends"
                            | "in"
                            | "instanceof"
                            | "new"
                            | "of"
                            | "return"
                            | "throw"
                            | "typeof"
                            | "void"
                            | "yield"
                    );
                let value_expected =
                    before.is_none_or(|b| "=(,:[!&|?{};+-*%<>~^".contains(*b)) || after_keyword;
                let contextual_await = after_keyword && prev_word == "await";
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
                    if let Some(flags) = closed {
                        let mut end = flags;
                        while end < chars.len() && chars[end].is_ascii_alphabetic() {
                            end += 1;
                        }
                        if !contextual_await
                            || js_regex_suffix(
                                &chars[flags..end],
                                &chars[end..],
                                self.js_typescript,
                            )
                        {
                            push(Tok::Str, &chars[i..end]);
                            i = end;
                            continue;
                        }
                    }
                }
            }
            // Strings that may span lines: `"""`, backticks (#876).
            if let Some(delim) = l.multiline.iter().find(|d| at(i, d)) {
                let n = delim.chars().count();
                let stop = close_at(i + n, delim, false);
                push(Tok::Str, &chars[i..stop.unwrap_or(chars.len())]);
                if stop.is_none() {
                    self.in_str = Some(delim.to_string());
                }
                i = stop.unwrap_or(chars.len());
                continue;
            }
            if self.powershell && l.quotes.contains(&c) {
                let stop = powershell_quote_end(&chars, i + 1, c);
                if stop.is_none() {
                    self.in_str = Some(c.to_string());
                }
                let end = stop.unwrap_or(chars.len());
                push(Tok::Str, &chars[i..end]);
                i = end;
                continue;
            }
            if !shell && l.quotes.contains(&c) {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != c {
                    j += if chars[j] == '\\' && !(toml && c == '\'') {
                        2
                    } else {
                        1
                    };
                }
                let stop = (j + 1).min(chars.len());
                push(Tok::Str, &chars[i..stop]);
                if continues_string && j >= chars.len() {
                    self.in_str = Some(c.to_string());
                }
                i = stop;
                continue;
            }
            if l.regex && c == '.' && at(i, "...") {
                push(Tok::Plain, &chars[i..i + 3]);
                i += 3;
                continue;
            }
            let js_number = l.regex
                && (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                    || c.is_ascii_digit()
                        && chars[..i].last().is_none_or(|c| {
                            c.is_whitespace()
                                || *c == '\u{feff}'
                                || c.is_ascii_punctuation() && !"_$".contains(*c)
                        }));
            if js_number || !l.regex && c.is_ascii_digit() && word_start {
                let j = if js_number {
                    formless_core::scripts::javascript_number_end(&chars, i)
                } else if self.rust_numbers {
                    rust_number_end(&chars, i)
                } else {
                    // Keep the existing policy for other language families.
                    let mut j = i;
                    while j < chars.len()
                        && (chars[j].is_ascii_alphanumeric() || "._".contains(chars[j]))
                    {
                        j += 1;
                    }
                    j
                };
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
                if let Some(shell) = &mut self.shell {
                    shell.word(&chars, i, j, &word);
                }
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
        if let Some(shell) = &mut self.shell {
            shell.end_line(&chars, ended_by_comment, self.in_str.is_some());
        }
        // JSON5 needs a fresh escape on every physical line of a
        // continued string, including when the next line is empty.
        if self.json5_strings && !continues_string {
            self.in_str = None;
        }
        toks.into_iter()
            .map(|(t, s)| Span::styled(s, self.style(t)))
            .collect()
    }
}

#[path = "diff_classification.rs"]
mod diff_classification;
use diff_classification::Kind as DiffKind;

/// Render a line whose role has already been determined by the full walk.
fn diff_line(line: &str, kind: DiffKind, color: bool) -> Span<'static> {
    let style = match kind {
        DiffKind::Header => super::bold_fg(color, Color::White),
        DiffKind::Added => super::bold_fg(color, Color::Green),
        DiffKind::Removed => super::bold_fg(color, Color::Red),
        DiffKind::Hunk => fg(color, Color::Cyan),
        DiffKind::Plain => Style::default(),
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
    let lines: Vec<_> = diff_classification::classify(text).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let context = |l: &(&str, DiffKind)| l.0.starts_with(' ') || l.0.is_empty();
        if context(&lines[i]) {
            let start = i;
            while i < lines.len() && context(&lines[i]) {
                i += 1;
            }
            let run = &lines[start..i];
            if run.len() > 2 * CONTEXT + 1 {
                out.extend(
                    run[..CONTEXT]
                        .iter()
                        .map(|&(line, kind)| vec![diff_line(line, kind, color)]),
                );
                out.push(vec![Span::styled(
                    format!(" ⋯ {} unchanged lines", run.len() - 2 * CONTEXT),
                    super::ui::dim(color),
                )]);
                out.extend(
                    run[run.len() - CONTEXT..]
                        .iter()
                        .map(|&(line, kind)| vec![diff_line(line, kind, color)]),
                );
            } else {
                out.extend(
                    run.iter()
                        .map(|&(line, kind)| vec![diff_line(line, kind, color)]),
                );
            }
        } else {
            out.push(vec![diff_line(lines[i].0, lines[i].1, color)]);
            i += 1;
        }
    }
    out
}

/// `+added −removed` for a diff (file headers excluded).
pub(crate) fn diff_stat(text: &str) -> (usize, usize) {
    diff_classification::classify(text).fold((0, 0), |(added, removed), (_, kind)| match kind {
        DiffKind::Added => (added + 1, removed),
        DiffKind::Removed => (added, removed + 1),
        _ => (added, removed),
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
    fn json_extensions_keep_comments_and_strings_separate() {
        for lang in ["jsonc", "json5"] {
            let source = r#"// keep "draft": true /*"#;
            assert_eq!(
                toks(lang, source),
                vec![(source.into(), Some(Color::DarkGray))]
            );
            let mut h = Highlighter::new(lang, true).unwrap();
            h.line(r#"/* "draft": true"#);
            let next = h.line("// keep */ null");
            assert_eq!(next[0].style.fg, Some(Color::DarkGray));
            assert!(next
                .iter()
                .any(|s| s.content == "null" && s.style.fg != Some(Color::DarkGray)));
            let source = r#""https://example.com/ /* \" true""#;
            assert_eq!(
                toks(lang, source),
                vec![(source.into(), Some(Color::Green))]
            );
            let non_nested = toks(lang, "/* outer /* inner */ true");
            assert!(non_nested
                .iter()
                .any(|(s, c)| s == "true" && *c != Some(Color::DarkGray)));
        }
        let source = r"'it\'s // /* true'";
        assert_eq!(
            toks("json5", source),
            vec![(source.into(), Some(Color::Green))]
        );
        for lang in ["json", "jsonc"] {
            assert!(!toks(lang, "'formless'")
                .iter()
                .any(|(_, c)| *c == Some(Color::Green)));
        }
        assert!(!toks("json", r#"// "draft": true /* false */"#)
            .iter()
            .any(|(_, c)| *c == Some(Color::DarkGray)));
    }

    #[test]
    fn json5_string_continuation_keeps_comment_markers_literal() {
        for quote in ['"', '\''] {
            let first = format!("{{{quote}message{quote}: {quote}first\\");
            let mut highlighter = Highlighter::new("json5", true).unwrap();
            highlighter.line(&first);
            let middle = "// continued \\";
            let second = highlighter.line(middle);
            assert_eq!(second[0].style.fg, Some(Color::Green));
            assert_eq!(second[0].content, middle);
            let last = format!("/* literal */{quote}, ok: true}}");
            let third = highlighter.line(&last);
            assert_eq!(third[0].content, format!("/* literal */{quote}"));
            assert_eq!(third[0].style.fg, Some(Color::Green));
            assert!(third.iter().any(|span| {
                span.content == "true" && span.style.fg == Some(Color::LightMagenta)
            }));
        }

        let mut highlighter = Highlighter::new("json5", true).unwrap();
        highlighter.line(r"'even slashes \\");
        assert_eq!(
            highlighter.line("// comment")[0].style.fg,
            Some(Color::DarkGray)
        );
        highlighter.line(r"'continued \");
        highlighter.line("");
        assert_eq!(
            highlighter.line("// comment")[0].style.fg,
            Some(Color::DarkGray)
        );
    }

    #[test]
    fn mysql_comments_do_not_change_generic_sql() {
        let tokens = toks("mysql", "SELECT 1;# keep /* false");
        assert!(tokens.contains(&("# keep /* false".into(), Some(Color::DarkGray))));
        let tokens = toks("mysql", "SELECT 1--2 AS n");
        assert!(!tokens.iter().any(|(_, c)| *c == Some(Color::DarkGray)));
        assert!(tokens
            .iter()
            .any(|(s, c)| s == "AS" && *c == Some(Color::Magenta)));
        for source in ["--", "-- comment", "--\tcomment"] {
            assert_eq!(
                toks("mysql", source),
                vec![(source.into(), Some(Color::DarkGray))]
            );
        }
        for quoted in ["'# -- /*'", r##""# -- /*""##, "`# -- /*`", "`a\\`"] {
            assert_eq!(
                toks("mysql", quoted),
                vec![(quoted.into(), Some(Color::Green))]
            );
        }
        let mut h = Highlighter::new("mysql", true).unwrap();
        h.line("# keep /* false");
        assert!(h.line("SELECT 1").iter().any(|s| s.content == "SELECT"));
        h.line("/* real block");
        assert!(h
            .line("# --")
            .iter()
            .all(|s| s.style.fg == Some(Color::DarkGray)));
        assert!(h.line("*/ SELECT 2").iter().any(|s| s.content == "SELECT"));
        h.line(r#"SELECT "first"#);
        let closing = h.line(r##"# continued" AS label # comment"##);
        assert_eq!(closing[0].content, "# continued\"");
        assert_eq!(closing[0].style.fg, Some(Color::Green));
        assert!(closing
            .iter()
            .any(|s| s.content == "# comment" && s.style.fg == Some(Color::DarkGray)));
        assert!(
            toks("mysql", "`a\\`# comment").contains(&("# comment".into(), Some(Color::DarkGray)))
        );
        for lang in ["sql", "sqlite", "postgres", "postgresql"] {
            assert!(toks(lang, "1--2 AS n").contains(&("--2 AS n".into(), Some(Color::DarkGray))));
            assert!(!toks(lang, "# drop table")
                .iter()
                .any(|(_, c)| *c == Some(Color::DarkGray)));
        }
    }

    #[test]
    fn javascript_numeric_literals_keep_member_calls() {
        for (source, number, method) in [
            ("1.5.toFixed(2)", "1.5", "toFixed"),
            ("1..toString()", "1.", "toString"),
            ("0x1f.toString(16)", "0x1f", "toString"),
            ("1e2.toFixed(2)", "1e2", "toFixed"),
            ("1e+2.toPrecision(2)", "1e+2", "toPrecision"),
            ("2_000n.toString()", "2_000n", "toString"),
            ("077.valueOf()", "077", "valueOf"),
            (".5.toFixed(2)", ".5", "toFixed"),
        ] {
            for lang in ["js", "javascript", "tsx", "typescript"] {
                let tokens = toks(lang, source);
                assert!(
                    tokens.contains(&(number.into(), Some(Color::LightYellow))),
                    "{lang} {source}: {tokens:?}"
                );
                assert!(
                    tokens.contains(&(method.into(), Some(Color::Blue))),
                    "{lang} {source}: {tokens:?}"
                );
            }
        }
        for (source, numbers) in [
            ("let value = 1_000 / 1.5", vec!["1_000", "1.5"]),
            ("0..len", vec!["0."]),
            ("[...5, ....5]", vec!["5", ".5"]),
            ("item1 / α\u{301}2", vec![]),
            ("return\u{feff}2", vec!["2"]),
        ] {
            let tokens = toks("js", source);
            let actual: Vec<_> = tokens
                .iter()
                .filter(|(_, color)| *color == Some(Color::LightYellow))
                .map(|(text, _)| text.as_str())
                .collect();
            assert_eq!(actual, numbers, "{source}: {tokens:?}");
        }
    }

    #[test]
    fn rust_numbers_leave_ranges_members_and_suffix_boundaries_to_normal_tokens() {
        let number = Some(Color::LightYellow);
        let call = Some(Color::Blue);
        for lang in ["rust", "rs", "RuSt"] {
            for (source, numeric, names) in [
                ("0..len", "0", vec![("len", None)]),
                ("0..=len", "0", vec![("len", None)]),
                ("1.max(x)", "1", vec![("max", call)]),
                ("1_u32.max(x)", "1_u32", vec![("max", call)]),
                ("0..n.min(k)", "0", vec![("n", None), ("min", call)]),
            ] {
                let tokens = toks(lang, source);
                assert!(tokens.contains(&(numeric.into(), number)), "{tokens:?}");
                for (name, style) in names {
                    assert!(
                        tokens
                            .iter()
                            .any(|(text, c)| text.contains(name) && *c == style),
                        "{tokens:?}"
                    );
                }
                assert!(
                    !tokens.iter().any(|(text, c)| *c == number
                        && (text.contains("..")
                            || text.contains("max")
                            || text.contains("min")
                            || text.contains("len"))),
                    "{tokens:?}"
                );
                assert_eq!(
                    tokens
                        .iter()
                        .map(|(text, _)| text.as_str())
                        .collect::<String>(),
                    source
                );
            }
        }
        for literal in [
            "3.14",
            "1_000",
            "0x1F",
            "0xff_u32",
            "0o7_55usize",
            "0b10_01u8",
            "1e3",
            "1E-3",
            "1e+3f64",
            "3.14_f32",
            "1.",
            "1e_3_f64",
            "10i128",
            "10u128",
            "10isize",
        ] {
            let tokens = toks("rust", literal);
            assert_eq!(tokens, vec![(literal.into(), number)], "{literal}");
        }
        for source in ["1_u32more", "1e+name", "3.14f32suffix", "0b1usize_more"] {
            let tokens = toks("rust", source);
            assert_eq!(
                tokens
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<String>(),
                source
            );
            assert!(
                tokens.iter().any(|(_, style)| *style != number),
                "{tokens:?}"
            );
        }
        for lang in ["python", "go", "c", "json", "toml", "sql", "css"] {
            assert_eq!(
                toks(lang, "0..len"),
                vec![("0..len".into(), number)],
                "existing {lang} policy"
            );
        }
        let source = "let 日本語 = 0..n.min(1_u32.max(3.14e-2f64)); // 🦀";
        for color in [true, false] {
            let mut highlighter = Highlighter::new("rust", color).unwrap();
            assert_eq!(
                highlighter
                    .line(source)
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>(),
                source
            );
        }
    }

    #[test]
    fn python_hash_comments_do_not_need_whitespace_but_other_languages_keep_boundaries() {
        for lang in ["py", "python", "python3", "PYTHON"] {
            for (source, expected) in [
                ("x=1#keep the rest", "#keep the rest"),
                ("print(x)#done", "#done"),
                ("def f():#entry", "#entry"),
                ("# at the start", "# at the start"),
                ("x = 1  # spaced", "# spaced"),
                ("s='# inside'; x=1#outside", "#outside"),
                ("s=\"# inside\"", ""),
            ] {
                let tokens = toks(lang, source);
                let comments: String = tokens
                    .iter()
                    .filter(|(_, color)| *color == Some(Color::DarkGray))
                    .map(|(text, _)| text.as_str())
                    .collect();
                assert_eq!(comments, expected, "{lang}: {source}");
                for color in [true, false] {
                    let mut highlighter = Highlighter::new(lang, color).unwrap();
                    let actual: String = highlighter
                        .line(source)
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect();
                    assert_eq!(actual, source, "{lang}: {source}");
                }
            }
        }
        for lang in ["sh", "bash", "yaml", "toml", "env", "ini"] {
            for source in ["echo foo#bar", "${#arr}", "key='x#y'"] {
                assert!(
                    !toks(lang, source)
                        .iter()
                        .any(|(_, color)| *color == Some(Color::DarkGray)),
                    "{lang}: {source}"
                );
            }
            assert!(
                toks(lang, "value # comment")
                    .contains(&("# comment".into(), Some(Color::DarkGray))),
                "{lang}"
            );
        }
    }

    #[test]
    fn shell_hash_comments_follow_unescaped_word_boundaries() {
        for lang in ["sh", "bash", "zsh", "fish", "shell", "console"] {
            for (source, expected) in [
                ("echo a;# done", "# done"),
                ("true|# note", "# note"),
                ("true&# note", "# note"),
                ("echo a &&# note", "# note"),
                ("(# note", "# note"),
                ("echo a; # spaced", "# spaced"),
                (r"echo a\;#literal", ""),
                (r"echo a\ #literal", ""),
                (r"echo a\\;# note", "# note"),
                ("echo foo#bar", ""),
                ("echo ${#arr} $#", ""),
                ("echo 'a;# quoted'", ""),
                ("echo [[ value;# outside", "# outside"),
                ("echo ${unset:-x;#literal};# outside", "# outside"),
                ("echo ${outer:-${inner:-x|#literal}};# outside", "# outside"),
                (r##"[[ "#foo" == @(other|#foo) ]];# outside"##, "# outside"),
                (r##"[[ "#foo" =~ (other|#foo) ]];# outside"##, "# outside"),
                ("echo @(other|#foo);# outside", "# outside"),
                (r"echo ${name:-x\};#literal};# outside", "# outside"),
            ] {
                let comments: String = toks(lang, source)
                    .into_iter()
                    .filter(|(_, color)| *color == Some(Color::DarkGray))
                    .map(|(text, _)| text)
                    .collect();
                assert_eq!(comments, expected, "{lang}: {source}");
            }
        }
        for lang in ["yaml", "toml", "ini", "ruby"] {
            assert!(
                toks(lang, "a|# literal")
                    .iter()
                    .all(|(_, color)| *color != Some(Color::DarkGray)),
                "{lang}"
            );
        }
        assert!(toks("powershell", "a|# literal")
            .contains(&("# literal".into(), Some(Color::DarkGray))));
    }

    #[test]
    fn shell_hash_comments_respect_multiline_context() {
        for (source, expected) in [
            ("cat <<EOF\nhello;#literal\nEOF\nprintf done;# real", &["# real"][..]),
            (
                "cat <<'FIRST' <<-E\"ND\"\n\n# data\nFIRST\n\tsecond |# data\n\tEND\ntrue;# outside",
                &["# outside"],
            ),
            ("cat <<$'E\\x4fF'\nhello;#literal\nEOF\nprintf done;# real", &["# real"]),
            ("cat <<$'caf\\303\\251'\nhello;#literal\ncafé\nprintf done;# real", &["# real"]),
            ("cat <<$\"EOF\"\nhello;#literal\nEOF\nprintf done;# real", &["# real"]),
            ("cat <<EOF\nleft\\\nEOF\nhello;#literal\nEOF\nprintf done;# real", &["# real"]),
            ("cat <<EOF\nE\\\nOF\nprintf done;# real", &["# real"]),
            ("cat <<'EOF'\nleft\\\nEOF\nprintf done;# real", &["# real"]),
            ("cat <<EOF\r\nhello;#literal\r\nEOF\r\nprintf done;# real\r", &["# real\r"]),
            ("cat <<''\n# data\n\ntrue;# outside", &["# outside"]),
            ("cat <<< literal\ntrue;# outside", &["# outside"]),
            ("cat \\<< literal\ntrue;# outside", &["# outside"]),
            ("cat <<EOF # header \\\n# data\nEOF\n# outside", &["# header \\", "# outside"]),
            (
                "echo ${unset:-$(case a in a) { :; };; esac) x;#literal};# outside",
                &["# outside"],
            ),
            (
                "echo ${unset:-$(if true; then case a in (esac) :;; esac; fi) x;#literal};# outside",
                &["# outside"],
            ),
            ("echo ${unset:-$(echo hi;# inside\n) x;#literal};# outside", &["# inside", "# outside"]),
            ("[[ a == a ||# condition\n b == c ]];# outside", &["# condition", "# outside"]),
            ("[[ a =~ (a||#literal) ]];# outside", &["# outside"]),
            ("if [[ a =~ a||#literal ]]; then :; fi;# outside", &["# outside"]),
            ("printf '%s' foo\\\n#literal\nprintf done;# real", &["# real"]),
            ("printf '%s' foo;\\\n# continued boundary", &["# continued boundary"]),
            ("echo 'a\n;# inside quote';# outside", &["# outside"]),
            ("printf '%s' $'it\\'s #inside' # outside", &["# outside"]),
        ] {
            let mut highlighter = Highlighter::new("bash", true).unwrap();
            let mut comments = Vec::new();
            for line in source.split('\n') {
                let spans = highlighter.line(line);
                assert_eq!(spans.iter().map(|span| span.content.as_ref()).collect::<String>(), line);
                comments.extend(
                    spans.into_iter()
                        .filter(|span| span.style.fg == Some(Color::DarkGray))
                        .map(|span| span.content.into_owned()),
                );
            }
            assert_eq!(comments, expected, "{source}");
        }
    }

    #[test]
    fn shell_quote_extensions_respect_the_dialect() {
        for (lang, delimiter) in [("sh", "$\"EOF\""), ("zsh", "$\"EOF\""), ("sh", "$'EOF'")] {
            let mut highlighter = Highlighter::new(lang, true).unwrap();
            highlighter.line(&format!("cat <<{delimiter}"));
            assert_eq!(
                highlighter.line("hello;#literal"),
                vec![Span::raw("hello;#literal")]
            );
            assert_eq!(highlighter.line("$EOF"), vec![Span::raw("$EOF")]);
            assert!(highlighter.line("true;# outside").into_iter().any(|span| {
                span.content == "# outside" && span.style.fg == Some(Color::DarkGray)
            }));
        }
    }

    #[test]
    fn php_hash_comments_do_not_open_blocks_or_consume_attributes() {
        for lang in ["php", "PHP"] {
            for first in ["# note: /* not a block", "foo();# /* note"] {
                let mut h = Highlighter::new(lang, true).unwrap();
                let spans = h.line(first);
                assert_eq!(
                    spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
                    first
                );
                let comments: String = spans
                    .iter()
                    .filter(|s| s.style.fg == Some(Color::DarkGray))
                    .map(|s| s.content.as_ref())
                    .collect();
                assert_eq!(comments, &first[first.find('#').unwrap()..]);
                assert_eq!(h.in_block, 0);
                for line in [r#"return "hi";"#, "class User {}"] {
                    let spans = h.line(line);
                    assert!(spans.iter().all(|s| s.style.fg != Some(Color::DarkGray)));
                    assert!(spans.iter().any(|s| {
                        s.content == line.split_whitespace().next().unwrap()
                            && s.style.fg == Some(Color::Magenta)
                    }));
                }
            }
            for line in [r##"#[Route("/users")]"##, r##""# not a comment""##] {
                assert!(toks(lang, line)
                    .iter()
                    .all(|(_, color)| *color != Some(Color::DarkGray)));
            }
            assert!(toks(lang, "// return").contains(&("// return".into(), Some(Color::DarkGray))));
        }
        for lang in ["c", "cpp", "java", "rust"] {
            for line in ["#include <stdio.h>", "#define VALUE 1"] {
                assert!(toks(lang, line)
                    .iter()
                    .all(|(_, color)| *color != Some(Color::DarkGray)));
            }
        }
        assert!(toks("python", "x=1# note").contains(&("# note".into(), Some(Color::DarkGray))));
        assert!(toks("yaml", "key: foo#bar")
            .iter()
            .all(|(_, color)| *color != Some(Color::DarkGray)));
    }

    #[test]
    fn php_line_comments_stop_before_a_closing_tag() {
        for opener in ["#", "//"] {
            let line = format!("<?php foo();{opener} note ?> <p>outside</p>");
            let tokens = toks("php", &line);
            assert_eq!(
                tokens
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<String>(),
                line
            );
            assert!(tokens.contains(&(format!("{opener} note "), Some(Color::DarkGray))));
            assert!(!tokens.iter().any(|(text, color)| {
                *color == Some(Color::DarkGray) && text.contains("outside")
            }));
        }
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

    #[test]
    fn toml_multiline_strings_keep_hashes_and_keys_until_the_closer() {
        for (delimiter, middle, closing) in [
            ("\"\"\"", r#"\""" keep # escaped quote"#, "\"\"\"\"\""),
            ("'''", "keep # literal", "\\'''''"),
        ] {
            let mut h = Highlighter::new("TOML", true).unwrap();
            h.line(&format!("text = {delimiter}"));
            for source in [middle, "name = not a key", "# still content"] {
                let spans = h.line(source);
                assert_eq!(spans.len(), 1, "{spans:?}");
                assert_eq!(spans[0].content, source);
                assert_eq!(spans[0].style.fg, Some(Color::Green));
            }
            let spacing = if delimiter.starts_with('"') { "" } else { " " };
            let spans = h.line(&format!("{closing}{spacing}# outside"));
            assert_eq!(spans[0].content, closing);
            assert_eq!(spans[0].style.fg, Some(Color::Green));
            assert_eq!(spans.last().unwrap().content, "# outside");
            assert_eq!(spans.last().unwrap().style.fg, Some(Color::DarkGray));
            let next = h.line("name = \"formless\" # comment");
            assert!(next
                .iter()
                .any(|s| s.content.trim_end() == "name" && s.style.fg == Some(Color::Blue)));
            assert!(next
                .iter()
                .any(|s| s.content == "\"formless\"" && s.style.fg == Some(Color::Green)));
            assert_eq!(next.last().unwrap().style.fg, Some(Color::DarkGray));

            h.line("items = [");
            let first = h.line(&format!("  {delimiter}a = b"));
            assert!(first.iter().any(|s| {
                s.content == format!("{delimiter}a = b") && s.style.fg == Some(Color::Green)
            }));
            assert!(all_str(&mut h, "# inside the array value"));
            assert!(all_str(&mut h, delimiter));
            h.line("]");
            assert_eq!(h.line("# outside")[0].style.fg, Some(Color::DarkGray));
        }
    }

    /// Strings that run across lines stay strings until they close (#876).
    #[test]
    fn powershell_quotes_and_comments_do_not_colour_following_code() {
        for lang in ["ps", "PowerShell"] {
            let mut highlighter = Highlighter::new(lang, true).unwrap();
            let line = r#"Write-Host "say `"hello`""; if ($true) { echo hi }"#;
            let spans = highlighter.line(line);
            assert!(spans
                .iter()
                .any(|span| span.content == r#""say `"hello`"""#
                    && span.style.fg == Some(Color::Green)));
            assert!(spans
                .iter()
                .any(|span| span.content == "if" && span.style.fg == Some(Color::Magenta)));

            let spans = highlighter.line(r"echo 'it''s\'; if true");
            assert!(spans
                .iter()
                .any(|span| span.content == r"'it''s\'" && span.style.fg == Some(Color::Green)));
            let spans = highlighter.line("echo# tail");
            assert!(spans
                .last()
                .unwrap()
                .style
                .add_modifier
                .contains(Modifier::ITALIC));

            highlighter.line("<#");
            let spans = highlighter.line("if ($true) { echo hi }");
            assert_eq!(spans.len(), 1);
            assert!(spans[0].style.add_modifier.contains(Modifier::ITALIC));
            assert!(highlighter
                .line("#> if true")
                .iter()
                .any(|span| span.content == "if" && span.style.fg == Some(Color::Magenta)));

            for quote in ['\'', '"'] {
                let opener = format!("@{quote}");
                let closer = format!("{quote}@");
                assert_eq!(highlighter.line(&opener)[0].style.fg, Some(Color::Green));
                assert_eq!(
                    highlighter.line(&format!("  {closer}"))[0].style.fg,
                    Some(Color::Green)
                );
                assert_eq!(
                    highlighter.line("<# still data")[0].style.fg,
                    Some(Color::Green)
                );
                assert!(highlighter
                    .line(&format!("{closer}; if true"))
                    .iter()
                    .any(|span| span.content == "if" && span.style.fg == Some(Color::Magenta)));
            }
        }
    }

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

    #[test]
    fn rust_raw_c_strings_include_the_prefix_and_close_without_escapes() {
        for literal in ["cr\"foo\\\"", "cr#\"say \"hi\"\"#", "cr##\"say \"# hi\"##"] {
            let source = format!("let s = {literal}; let after = 1;");
            let tokens = toks("rust", &source);
            assert!(
                tokens.contains(&(literal.into(), Some(Color::Green))),
                "{tokens:?}"
            );
            assert_eq!(
                tokens
                    .iter()
                    .filter(|(s, c)| s == "let" && *c == Some(Color::Magenta))
                    .count(),
                2
            );
            assert_eq!(
                tokens.iter().map(|(s, _)| s.as_str()).collect::<String>(),
                source
            );
        }

        let mut rust = Highlighter::new("rs", true).unwrap();
        rust.line("let s = cr##\"first");
        assert!(all_str(&mut rust, "not closed by \"# or a keyword"));
        let end = rust.line("last\\\"##; let after = 2;");
        assert_eq!(end[0].content, "last\\\"##");
        assert_eq!(end[0].style.fg, Some(Color::Green));
        assert!(end
            .iter()
            .any(|s| s.content == "let" && s.style.fg == Some(Color::Magenta)));
    }

    #[test]
    fn go_backticks_close_after_backslashes_and_js_backticks_still_escape() {
        for literal in ["`C:\\`", "`\\\\server\\share\\`"] {
            let source = format!("s := {literal}; return s");
            let tokens = toks("go", &source);
            assert!(
                tokens.contains(&(literal.into(), Some(Color::Green))),
                "{tokens:?}"
            );
            assert!(
                tokens.contains(&("return".into(), Some(Color::Magenta))),
                "{tokens:?}"
            );
        }
        let mut go = Highlighter::new("golang", true).unwrap();
        go.line("s := `first");
        let end = go.line("last\\`; return s");
        assert_eq!(end[0].content, "last\\`");
        assert_eq!(end[0].style.fg, Some(Color::Green));
        assert!(end
            .iter()
            .any(|s| s.content == "return" && s.style.fg == Some(Color::Magenta)));

        let mut js = Highlighter::new("js", true).unwrap();
        js.line("const s = `C:\\`");
        assert!(all_str(&mut js, "let still in the template"));
        let end = js.line("`; let after = 1;");
        assert!(end
            .iter()
            .any(|s| s.content == "let" && s.style.fg == Some(Color::Magenta)));
    }

    /// Nested Rust comments, raw strings, JS regexes, YAML `;` and
    /// unknown languages (#877).
    #[test]
    fn js_regex_literals_after_expression_keywords_preserve_the_rest_of_the_line() {
        let regex = r"/https:\/\//gi";
        for keyword in [
            "await",
            "throw",
            "void",
            "else",
            "delete",
            "new",
            "do",
            "instanceof",
            "extends",
        ] {
            let source = format!("{keyword} {regex}; next(); // outside");
            let tokens = toks("js", &source);
            assert!(
                tokens.contains(&(regex.into(), Some(Color::Green))),
                "{source}: {tokens:?}"
            );
            assert!(
                tokens.contains(&("next".into(), Some(Color::Blue))),
                "{source}: {tokens:?}"
            );
            let comments: Vec<_> = tokens
                .iter()
                .filter(|(_, color)| *color == Some(Color::DarkGray))
                .map(|(text, _)| text.as_str())
                .collect();
            assert_eq!(comments, ["// outside"], "{source}");
        }
        for source in [
            "throw value / 2 / 3",
            "await value / 2 / 3",
            "const await = 48; return await / 2 / 3",
            "return await / 2 / /* rhs */ 3",
            "return await /x/factor",
            "object.throw / 2 / 3",
            "object./* comment *//* more */throw / 2 / 3",
            "object?.await / 2 / 3",
            "object?. /* note */ await / 2 / 3",
            "this.#delete / 2 / 3",
            "$await / 2 / 3",
            "a\u{301}await / 2 / 3",
            "π\u{200c}await / 2 / 3",
            "f(awaited / 2 / 3)",
        ] {
            let tokens = toks("js", source);
            assert!(
                tokens.iter().all(|(_, color)| *color != Some(Color::Green)),
                "{source}: {tokens:?}"
            );
        }
        for source in [
            format!("await /* note */ {regex};"),
            format!("throw /* reason */ {regex};"),
            format!("return {regex} /* operator */ instanceof RegExp"),
            format!("const result = await {regex} in object"),
            format!("return [...typeof {regex}];"),
        ] {
            assert!(
                toks("js", &source).contains(&(regex.into(), Some(Color::Green))),
                "{source}"
            );
        }
        for lang in ["ts", "tsx", "typescript"] {
            for source in [
                format!("return {regex} as RegExp"),
                format!("return await {regex} satisfies RegExp"),
            ] {
                assert!(
                    toks(lang, &source).contains(&(regex.into(), Some(Color::Green))),
                    "{lang}: {source}"
                );
            }
        }
    }

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
    #[test]
    fn colorblind_and_no_color_diffs_keep_exact_bytes_and_bold_signed_gutters() {
        let _theme = crate::theme::test_theme("colorblind");
        for (text, tone) in [
            ("+added ไทย", crate::SemanticTone::Good),
            ("-removed العربية", crate::SemanticTone::Bad),
        ] {
            let kind = if text.starts_with('+') {
                DiffKind::Added
            } else {
                DiffKind::Removed
            };
            let span = diff_line(text, kind, true);
            let (r, g, b) = crate::colorblind_rgb(tone);
            assert_eq!(span.style.fg, Some(Color::Rgb(r, g, b)));
            assert!(span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD));
            let plain = diff_line(text, kind, false);
            assert_eq!(span.content, plain.content);
            assert_eq!(plain.style.fg, None);
            assert!(plain
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD));
        }
    }
}

#[cfg(test)]
#[path = "diff_classification_tests.rs"]
mod diff_classification_tests;
