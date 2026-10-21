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
/// (`await tools.name(input)`), plus `tools.call(name, input)`. The same
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
         \x20 const out = {{ call }};\n\
         \x20 for (const n of __uses) out[n] = (input) => t.call(n, input);\n\
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
/// whitespace and comments dropped and string, template and number
/// literals blanked, so the same script with other paths or values
/// matches.
pub fn fingerprint(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
            // Keep one separator between words so `a b` and `ab` differ.
            if out.chars().last().is_some_and(ident) && next.is_some_and(ident) {
                out.push(' ');
            }
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c == '"' || c == '\'' || c == '`' {
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            out.push('S');
        } else if c.is_ascii_digit() && !out.chars().last().is_some_and(ident) {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            out.push('0');
        } else {
            out.push(c);
            i += 1;
        }
    }
    Sha256::digest(out.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

    #[test]
    fn script_code_puts_the_source_after_the_offset() {
        let code = script_code("FIRST_LINE", &["t".into()]);
        assert_eq!(code.lines().nth(LINE_OFFSET), Some("FIRST_LINE"));
    }
}
