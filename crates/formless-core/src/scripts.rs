//! Orchestration scripts (`run_script`, SPEC §7.12): the code a script
//! runs as, and the source fingerprint that spots recurring scripts.

use sha2::{Digest, Sha256};

/// Ambient host APIs a script never gets: every effect is a Tool call.
const STRIPPED: &[&str] = &[
    "fetch",
    "fetchChunks",
    "fs",
    "process",
    "env",
    "clipboard",
    "notify",
    "notifications",
    "llm",
    "agents",
    "store",
    "emit",
];

/// Tool code for a script: `source` becomes the body of an async
/// `run(input)`, so it may `await` and `return` at its top level. Inside
/// it, `tools` holds only the Tools in `uses`, each an async function
/// (`await tools.name(input)`), plus `tools.call(name, input)`. A Tool named `call` uses the dispatcher
/// spelling `tools.call("call", input)` instead of a shorthand. The same
/// code is what `save_as` saves as a Tool.
pub fn script_code(source: &str, uses: &[String]) -> String {
    let uses = serde_json::to_string(uses).unwrap_or_else(|_| "[]".into());
    let stripped = serde_json::to_string(STRIPPED).unwrap_or_default();
    format!(
        "// Orchestration script (run_script, SPEC §7.12).\n\
         for (const k of {stripped}) delete globalThis[k];\n\
         const __uses = {uses};\n\
         function __scriptTools() {{\n\
         \x20 const t = globalThis.tools, allowed = new Set(__uses);\n\
         \x20 const call = (name, input) => allowed.has(String(name))\n\
         \x20   ? t.call(name, input)\n\
         \x20   : Promise.reject(new Error(\"'\" + name + \"' is not in this script's uses list\"));\n\
         \x20 const out = Object.create(null, {{ call: {{ value: call, enumerable: true }} }});\n\
         \x20 for (const n of __uses) if (n !== \"call\") out[n] = (input) => t.call(n, input);\n\
         \x20 return out;\n\
         }}\n\
         async function run(input) {{\n\
         const tools = __scriptTools();\n\
         {source}\n\
         }}\n"
    )
}

/// Lines `script_code` puts before the script's first line, so error
/// positions can be reported in the agent's own line numbers.
pub const LINE_OFFSET: usize = 14;

/// Fingerprint of a script's shape: SHA-256 over its tokens with
/// whitespace and comments dropped and string, template, regex and number
/// literals blanked, so the same script with other paths or values
/// matches. A template's `${…}` interpolations are code and count (#832).
/// Ambiguous slash contexts use a domain-marked exact-source hash instead:
/// missing a shape match is safer than erasing different executable code.
pub fn fingerprint(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    if shape(&chars, &mut i, &mut out, false).is_none() {
        out.clear();
        out.push_str("\0source\0");
        out.push_str(source);
    }
    Sha256::digest(out.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What recurrence is counted by: the shape plus the Tools the script
/// may call, so one source wired to other Tools is another script (#832).
pub fn recurrence_key(source: &str, uses: &[String]) -> String {
    let mut uses: Vec<&str> = uses.iter().map(String::as_str).collect();
    uses.sort_unstable();
    uses.dedup();
    let key = format!("{}\n{}", fingerprint(source), uses.join(","));
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Append the shape of code from `*i` to `out`. Inside an interpolation
/// (`nested`), stops after the `}` that closes it.
fn shape(chars: &[char], i: &mut usize, out: &mut String, nested: bool) -> Option<()> {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut depth = 0usize;
    while *i < chars.len() {
        let c = chars[*i];
        let next = chars.get(*i + 1).copied();
        if c.is_whitespace() {
            // Keep one separator between words so `a b` and `ab` differ.
            if out.chars().last().is_some_and(ident) && next.is_some_and(ident) {
                out.push(' ');
            }
            *i += 1;
        } else if c == '/' && next == Some('/') {
            while *i < chars.len() && chars[*i] != '\n' {
                *i += 1;
            }
        } else if c == '/' && next == Some('*') {
            *i += 2;
            while *i < chars.len() && !(chars[*i] == '*' && chars.get(*i + 1) == Some(&'/')) {
                *i += 1;
            }
            *i += 2;
        } else if c == '/' && (out.ends_with("++") || out.ends_with("--") || out.ends_with('}')) {
            // ++/-- can end an expression; } can close an object or a block.
            // Without syntactic context, blanking a guessed regex can swallow
            // divisors and subsequent Tool calls. Propagate fallback even from
            // nested template interpolations. Comments were handled above.
            return None;
        } else if c == '/' && regex_allowed(out) {
            // A regex literal: its `//`, `/*` and quotes are text.
            let mut class = false;
            *i += 1;
            while *i < chars.len() && chars[*i] != '\n' {
                match chars[*i] {
                    '\\' => *i += 1,
                    '[' => class = true,
                    ']' => class = false,
                    '/' if !class => break,
                    _ => {}
                }
                *i += 1;
            }
            *i += 1;
            while *i < chars.len() && chars[*i].is_ascii_alphabetic() {
                *i += 1;
            }
            out.push('R');
        } else if c == '`' {
            // Template text is blanked; interpolations are code.
            *i += 1;
            out.push('S');
            while *i < chars.len() && chars[*i] != '`' {
                if chars[*i] == '\\' {
                    *i += 2;
                } else if chars[*i] == '$' && chars.get(*i + 1) == Some(&'{') {
                    *i += 2;
                    out.push_str("${");
                    shape(chars, i, out, true)?;
                    out.push('}');
                } else {
                    *i += 1;
                }
            }
            *i += 1;
        } else if c == '"' || c == '\'' {
            *i += 1;
            while *i < chars.len() && chars[*i] != c && chars[*i] != '\n' {
                if chars[*i] == '\\' {
                    *i += 1;
                }
                *i += 1;
            }
            *i += 1;
            out.push('S');
        } else if c.is_ascii_digit() && !out.chars().last().is_some_and(ident) {
            while *i < chars.len() && (chars[*i].is_ascii_alphanumeric() || chars[*i] == '.') {
                *i += 1;
            }
            out.push('0');
        } else {
            if nested {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    if depth == 0 {
                        *i += 1;
                        return Some(());
                    }
                    depth -= 1;
                }
            }
            out.push(c);
            *i += 1;
        }
    }
    Some(())
}

/// Whether a `/` here starts a regex rather than dividing: it follows an
/// operator, an opening bracket, the start, or a keyword like `return`.
fn regex_allowed(out: &str) -> bool {
    let Some(last) = out.chars().last() else {
        return true;
    };
    if "=(,:[!&|?{};+-*%<>~^".contains(last) {
        return true;
    }
    let word: String = out
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    matches!(
        word.as_str(),
        "return" | "typeof" | "case" | "in" | "of" | "yield" | "await"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_literals_whitespace_and_comments() {
        let a = "const r = await tools.grep({pattern: \"TODO\", root: '/a'});\nreturn r.length;";
        let b = "// count\nconst r = await tools.grep({ pattern: `FIXME`, root: \"/b/c\" });  return r.length;";
        assert_eq!(fingerprint(a), fingerprint(b));
        assert_ne!(fingerprint(a), fingerprint("return 1;"));
        assert_eq!(fingerprint("x = 1.5e3"), fingerprint("x = 42"));
        assert_ne!(fingerprint("a b"), fingerprint("ab"));
    }

    /// Regex literals are literals; template interpolations are code;
    /// recurrence counts the Tools too (#832).
    #[test]
    fn fingerprint_knows_regexes_and_templates() {
        // `//` inside a regex is not a comment: the code after it counts.
        let a = "const re = /https:\\/\\//; return await tools.a(re);";
        let b = "const re = /https:\\/\\//; return await tools.b(re);";
        assert_ne!(fingerprint(a), fingerprint(b));
        // Other regex text, same shape.
        assert_eq!(
            fingerprint("x = /a\"b/g; f(x)"),
            fingerprint("x = /c*d/; f(x)")
        );
        assert_eq!(
            fingerprint("x = /\\/*/; f(x)"),
            fingerprint("x = /y/; f(x)")
        );
        // Division is not a regex.
        assert_ne!(fingerprint("a = b / c / d"), fingerprint("a = b"));
        // Interpolations count; the template text doesn't.
        let t1 = "log(`found ${await tools.grep(q)} hits`)";
        let t2 = "log(`found ${await tools.find(q)} hits`)";
        assert_ne!(fingerprint(t1), fingerprint(t2));
        assert_eq!(
            fingerprint(t1),
            fingerprint("log(`other ${await tools.grep(q)} words`)")
        );
        // A nested template inside an interpolation doesn't end the outer one.
        assert_eq!(
            fingerprint("x = `a ${`b ${c}`} d`; y()"),
            fingerprint("x = `z ${`w ${c}`} v`; y()")
        );
        let uses = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_ne!(
            recurrence_key(a, &uses(&["grep"])),
            recurrence_key(a, &uses(&["find"]))
        );
        assert_eq!(
            recurrence_key(a, &uses(&["b", "a"])),
            recurrence_key(a, &uses(&["a", "b"]))
        );
    }

    #[test]
    fn postfix_division_preserves_following_code_and_divisors() {
        let uses = vec!["grep".into(), "find".into()];
        for end in ["count++", "count--", "count /* trailing */ ++", "count++\n"] {
            let a = format!("{end} / limit; return await tools.grep(q)");
            let b = format!("{end} / limit; return await tools.find(q)");
            assert_ne!(fingerprint(&a), fingerprint(&b), "{end}");
            assert_ne!(
                recurrence_key(&a, &uses),
                recurrence_key(&b, &uses),
                "{end}"
            );
            assert_ne!(
                fingerprint(&format!("{end} / left / scale")),
                fingerprint(&format!("{end} / right / scale")),
                "{end}"
            );
        }
    }

    #[test]
    fn closing_brace_division_preserves_following_code_and_divisors() {
        let uses = vec!["grep".into(), "find".into()];
        for end in [
            "const x = {a:1}",
            "const x = {a:{n:1}}",
            "const x = {a:1} /* note */",
        ] {
            let a = format!("{end} / limit; return await tools.grep(q)");
            let b = format!("{end} / limit; return await tools.find(q)");
            assert_ne!(fingerprint(&a), fingerprint(&b), "{end}");
            assert_ne!(
                recurrence_key(&a, &uses),
                recurrence_key(&b, &uses),
                "{end}"
            );
            assert_ne!(
                fingerprint(&format!("{end} / left / scale")),
                fingerprint(&format!("{end} / right / scale")),
                "{end}"
            );
        }
    }

    #[test]
    fn ambiguous_slashes_use_exact_source_even_inside_templates() {
        for source in [
            "count++ / limit; return await tools.grep(q)",
            "const x = {a:1} / limit; return await tools.grep(q)",
            "function f(){} /pattern/.test(s); return await tools.grep(q)",
            "const x = `text ${count++ / limit}`; return await tools.grep(q)",
        ] {
            assert_ne!(
                fingerprint(source),
                fingerprint(&format!(" {source}")),
                "conservative fallback: {source}"
            );
            assert_ne!(
                fingerprint(source),
                fingerprint(&source.replace("grep", "find")),
                "{source}"
            );
        }
        // Unambiguous regex/literal normalization stays available.
        assert_eq!(
            fingerprint("return /alpha/.test('one')"),
            fingerprint(" return /beta/.test('two') ")
        );
        assert_eq!(
            fingerprint("let x = ++count / 2"),
            fingerprint("let x=++count/3")
        );
    }

    #[test]
    fn script_code_puts_the_source_after_the_offset() {
        let code = script_code("FIRST_LINE", &["t".into()]);
        assert_eq!(code.lines().nth(LINE_OFFSET), Some("FIRST_LINE"));
    }
}
