//! Value-preserving keys, separate from lossy script-shape fingerprints.
//! This is a conservative lexer, not a parser: uncertain slash goals and
//! unsupported syntax retain the entire raw source instead of guessing.

#[derive(Clone, Copy, PartialEq)]
enum Goal {
    Expression,
    Operand,
    Unknown,
}

pub(super) fn key(source: &str) -> String {
    let mut lexer = Lexer {
        chars: source.chars().collect(),
        at: 0,
    };
    let mut out = String::from("tokens:");
    if lexer.code(&mut out, false, true, 0).is_some() {
        out
    } else {
        format!("raw:{source}")
    }
}

struct Lexer {
    chars: Vec<char>,
    at: usize,
}
fn line(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

impl Lexer {
    fn starts(&self, text: &str) -> bool {
        self.chars.len() - self.at >= text.chars().count()
            && self.chars[self.at..]
                .iter()
                .copied()
                .zip(text.chars())
                .all(|(a, b)| a == b)
    }
    fn trivia(&mut self) -> Option<bool> {
        let mut newline = false;
        loop {
            match self.chars.get(self.at).copied() {
                Some(c) if c.is_whitespace() || c == '\u{feff}' => {
                    newline |= line(c);
                    self.at += 1;
                }
                Some('/') if self.starts("//") => {
                    while self.at < self.chars.len() && !line(self.chars[self.at]) {
                        self.at += 1;
                    }
                }
                Some('/') if self.starts("/*") => {
                    self.at += 2;
                    while self.at < self.chars.len() && !self.starts("*/") {
                        newline |= line(self.chars[self.at]);
                        self.at += 1;
                    }
                    if self.at == self.chars.len() {
                        return None;
                    }
                    self.at += 2;
                }
                _ => return Some(newline),
            }
        }
    }
    fn quoted(&mut self, quote: char) -> Option<()> {
        self.at += 1;
        while let Some(&c) = self.chars.get(self.at) {
            self.at += 1;
            if c == quote {
                return Some(());
            }
            if c == '\\' {
                let escaped = *self.chars.get(self.at)?;
                self.at += 1;
                if escaped == '\r' && self.chars.get(self.at) == Some(&'\n') {
                    self.at += 1;
                }
            } else if line(c) {
                return None;
            }
        }
        None
    }
    fn regex(&mut self) -> Option<()> {
        self.at += 1;
        let mut class = false;
        while let Some(&c) = self.chars.get(self.at) {
            self.at += 1;
            match c {
                c if line(c) => return None,
                '\\' => {
                    if line(*self.chars.get(self.at)?) {
                        return None;
                    }
                    self.at += 1;
                }
                '[' if class => return None, // Nested Unicode sets: raw fallback.
                '[' => class = true,
                ']' => class = false,
                '/' if !class => {
                    while self
                        .chars
                        .get(self.at)
                        .is_some_and(char::is_ascii_alphabetic)
                    {
                        if self.chars[self.at] == 'v' {
                            return None;
                        }
                        self.at += 1;
                    }
                    return Some(());
                }
                _ => {}
            }
        }
        None
    }
    fn template(&mut self, out: &mut String, depth: usize) -> Option<()> {
        self.at += 1;
        while let Some(&c) = self.chars.get(self.at) {
            match c {
                '\\' => {
                    self.chars.get(self.at + 1)?;
                    self.at += 2;
                }
                '`' => {
                    self.at += 1;
                    return Some(());
                }
                '$' if self.chars.get(self.at + 1) == Some(&'{') => {
                    self.at += 2;
                    self.code(out, true, false, depth + 1)?;
                }
                _ => self.at += 1,
            }
        }
        None
    }
    fn code(&mut self, out: &mut String, nested: bool, emit: bool, depth: usize) -> Option<()> {
        if depth > 64 {
            return None;
        }
        let mut braces = 0usize;
        let mut goal = Goal::Expression;
        let mut restricted = false;
        let mut property = false;
        loop {
            let newline = self.trivia()?;
            let Some(&c) = self.chars.get(self.at) else {
                return (!nested).then_some(());
            };
            if nested && c == '}' && braces == 0 {
                self.at += 1;
                return Some(());
            }
            let start = self.at;
            let before = goal;
            let was_property = property;
            property = false;
            if matches!(c, '\'' | '"') {
                self.quoted(c)?;
                goal = Goal::Operand;
            } else if c == '`' {
                self.template(out, depth)?;
                goal = Goal::Operand;
            } else if c == '/' && goal == Goal::Expression {
                self.regex()?;
                goal = Goal::Operand;
            } else if c == '/' && goal == Goal::Unknown {
                return None;
            } else if c.is_alphanumeric() || c == '_' || c == '$' || !c.is_ascii() {
                self.at += 1;
                while self.chars.get(self.at).is_some_and(|c| {
                    c.is_alphanumeric()
                        || matches!(*c, '_' | '$')
                        || (!c.is_ascii() && !c.is_whitespace())
                }) {
                    self.at += 1;
                }
                if c.is_ascii_digit() {
                    while self.chars.get(self.at).is_some_and(|c| {
                        c.is_ascii_alphanumeric()
                            || matches!(*c, '_' | '.')
                            || (matches!(*c, '+' | '-')
                                && matches!(self.chars[self.at - 1], 'e' | 'E'))
                    }) {
                        self.at += 1;
                    }
                }
                let word: String = self.chars[start..self.at].iter().collect();
                goal = if was_property {
                    Goal::Operand
                } else {
                    match word.as_str() {
                        "return" | "throw" | "case" | "typeof" | "void" | "delete" | "new"
                        | "in" | "instanceof" | "else" | "do" => Goal::Expression,
                        "await" | "yield" | "of" | "extends" | "default" => Goal::Unknown,
                        _ => Goal::Operand,
                    }
                };
            } else {
                if self.starts("<!--") || self.starts("-->") {
                    return None;
                }
                let operator = [
                    ">>>=", "===", "!==", "**=", "&&=", "||=", "??=", ">>>", "...", "=>", "++",
                    "--", "?.", "<<", ">>", "**", "&&", "||", "??", "+=", "-=", "*=", "/=", "%=",
                    "&=", "|=", "^=", "==", "!=", "<=", ">=",
                ]
                .into_iter()
                .find(|op| self.starts(op));
                if let Some(op) = operator {
                    self.at += op.len();
                    property = op == "?.";
                    goal = match op {
                        "++" | "--" => before,
                        "?." => Goal::Unknown,
                        _ => Goal::Expression,
                    };
                } else {
                    self.at += 1;
                    goal = match c {
                        '(' | '[' | ';' | ',' | ':' | '?' | '~' | '+' | '-' | '*' | '/' | '%'
                        | '<' | '>' | '=' | '!' | '&' | '|' | '^' => Goal::Expression,
                        '{' => {
                            braces += 1;
                            Goal::Expression
                        }
                        '}' => {
                            braces = braces.saturating_sub(1);
                            Goal::Unknown
                        }
                        ')' => Goal::Unknown,
                        ']' => Goal::Operand,
                        '.' => {
                            property = true;
                            Goal::Unknown
                        }
                        _ => return None,
                    };
                }
            }
            let token: String = self.chars[start..self.at].iter().collect();
            if emit {
                // Boundaries prevent a + +b from becoming a++b. Preserve
                // restricted/postfix line breaks, including those in comments.
                if newline && (restricted || matches!(token.as_str(), "++" | "--" | "=>")) {
                    out.push_str("|newline|");
                }
                out.push_str(&token.len().to_string());
                out.push(':');
                out.push_str(&token);
            }
            restricted = !was_property
                && matches!(
                    token.as_str(),
                    "return"
                        | "throw"
                        | "break"
                        | "continue"
                        | "yield"
                        | "await"
                        | "async"
                        | "using"
                );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::key;
    #[test]
    fn comments_and_layout_keep_values_and_token_boundaries() {
        for (a, b) in [
            ("// don't\nreturn 1;", "/* can't */ return  1 ;"),
            (
                "const re = /'/g; return re;",
                "const re=/'/g;/* \" */return re;",
            ),
            (
                r"const re=/https?:\/\//; f(re);",
                r"const re = /https?:\/\//;f(re);",
            ),
            (
                "const n = 1.2e+3; const d = n / 2;",
                "const n=1.2e+3;const d=n/2;",
            ),
        ] {
            assert_eq!(key(a), key(b), "{a}");
            assert!(key(a).starts_with("tokens:"));
        }
        for (a, b) in [
            ("return 'a b';", "return 'ab';"),
            ("return /a b/g;", "return /ab/g;"),
            ("return 1;", "return 2;"),
            ("a + +b", "a++b"),
            ("a b", "ab"),
            ("a/* note */b", "ab"),
            ("using\nx=y;", "using x=y;"),
            ("async x\n=>x", "async x=>x"),
            ("return\nx;", "return x;"),
            ("return/*\n*/x;", "return/**/x;"),
            ("a\n++b", "a++\nb"),
            ("async\nfunction f(){}", "async function f(){}"),
        ] {
            assert_ne!(key(a), key(b), "{a} / {b}");
        }
    }
    #[test]
    fn templates_remain_verbatim_including_nested_interpolations() {
        let a = concat!("const t = `hello $", "{`inner $", "{'a b'}`}`; return t;");
        assert_ne!(key(a), key(&a.replace("hello", "goodbye")));
        assert!(key(a).starts_with("tokens:"));
        assert_eq!(key(a), key(&format!("// note\n{a} // end")));
        assert_ne!(key(a), key(&a.replace("'a b'", "'ab'")));
    }
    #[test]
    fn uncertain_or_unterminated_syntax_retains_raw_source() {
        for source in [
            "if (ok) /'/.test(x);",
            "f() / 2",
            "for (const x of /'/) {}",
            "class X extends /'/ {}",
            "const r=/[[a]--[b]]/v;",
            "return 'unfinished",
            "/* unfinished",
            "const t=`unfinished",
            "<!-- legacy comment\nf()",
            r"const \u0061=1;",
        ] {
            assert_eq!(key(source), format!("raw:{source}"));
            assert_ne!(key(source), key(&format!("{source} ")));
        }
        let mut deep = String::from("x");
        for _ in 0..100 {
            deep = format!("{}{{{deep}}}{}", "`$", "`");
        }
        assert_eq!(key(&deep), format!("raw:{deep}"));
    }
}
