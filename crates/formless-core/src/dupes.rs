//! Near-duplicate Tools (SPEC §8.10): registries accumulate `read_file`,
//! `read_text_file`, `file_read`. Pairs are scored on code, name,
//! description and input fields, and proposed for merging.

use formless_protocol::{ToolInfo, ToolVersionInfo};
use std::collections::BTreeSet;

/// Pairs scoring at least this are flagged.
pub const THRESHOLD: f64 = 0.6;

#[derive(Debug, Clone, PartialEq)]
pub struct DupPair {
    pub a: String,
    pub b: String,
    pub score: f64,
    pub reason: String,
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "from", "into", "that", "this", "its", "are", "returns", "return",
    "given", "using", "via", "tool", "input", "output",
];

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() >= 3 && !STOP.contains(&w.as_str()))
        .map(|w| w.strip_suffix('s').map(str::to_string).unwrap_or(w))
        .collect()
}

fn name_tokens(name: &str) -> BTreeSet<String> {
    // camelCase, PascalCase and acronym-to-word transitions match snake_case.
    let mut spaced = String::new();
    let mut prev_lower = false;
    let mut prev_upper = false;
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_uppercase()
            && (prev_lower || (prev_upper && chars.peek().is_some_and(|next| next.is_lowercase())))
        {
            spaced.push('_');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        prev_upper = c.is_uppercase();
        spaced.push(c);
    }
    spaced
        .split(['_', '-'])
        .map(|w| w.to_lowercase())
        .map(|w| w.strip_suffix('s').map(str::to_string).unwrap_or(w))
        .filter(|w| !w.is_empty())
        .collect()
}

fn schema_fields(schema: &str) -> BTreeSet<String> {
    serde_json::from_str::<serde_json::Value>(schema)
        .ok()
        .and_then(|v| {
            v["properties"]
                .as_object()
                .map(|p| p.keys().cloned().collect())
        })
        .unwrap_or_default()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

#[path = "dupes_code.rs"]
mod lexical;

/// Comment/layout-insensitive exact tokens, preserving literal values and
/// restricted line breaks. Uncertain syntax falls back to raw source.
fn code_key(code: &str) -> String {
    lexical::key(code)
}

/// Input fields alike; two Tools that both take nothing count as alike
/// (they shouldn't be marked down for it, #829).
fn fields_alike(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        1.0
    } else {
        jaccard(a, b)
    }
}

/// How alike two Tools are (0..1), and why.
pub fn similarity(
    a: (&ToolInfo, &ToolVersionInfo),
    b: (&ToolInfo, &ToolVersionInfo),
) -> (f64, String) {
    if code_key(&a.1.code) == code_key(&b.1.code) {
        return (1.0, "identical code".into());
    }
    let name = jaccard(&name_tokens(&a.0.name), &name_tokens(&b.0.name));
    let desc = jaccard(&words(&a.0.description), &words(&b.0.description));
    let fields = fields_alike(
        &schema_fields(&a.1.input_schema),
        &schema_fields(&b.1.input_schema),
    );
    let score = 0.4 * name + 0.3 * desc + 0.3 * fields;
    let mut why = Vec::new();
    if name >= 0.5 {
        why.push("similar names");
    }
    if desc >= 0.5 {
        why.push("similar descriptions");
    }
    if fields >= 0.5 {
        why.push("same input fields");
    }
    (score, why.join(", "))
}

/// Every flagged pair among active, non-MCP Tools, most alike first.
pub fn near_duplicates(tools: &[(ToolInfo, ToolVersionInfo)]) -> Vec<DupPair> {
    let tools: Vec<&(ToolInfo, ToolVersionInfo)> = tools
        .iter()
        .filter(|(t, _)| !t.retired && !t.name.starts_with(crate::mcp::PREFIX))
        .collect();
    let mut out = Vec::new();
    for (i, a) in tools.iter().enumerate() {
        for b in &tools[i + 1..] {
            let (score, reason) = similarity((&a.0, &a.1), (&b.0, &b.1));
            if score >= THRESHOLD {
                let (x, y) = if a.0.name <= b.0.name {
                    (&a.0.name, &b.0.name)
                } else {
                    (&b.0.name, &a.0.name)
                };
                out.push(DupPair {
                    a: x.clone(),
                    b: y.clone(),
                    score,
                    reason,
                });
            }
        }
    }
    out.sort_by(|p, q| q.score.total_cmp(&p.score));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acronym_names_match_snake_case_without_splitting_capital_runs() {
        for (name, snake) in [
            ("HTTPGetJson", "http_get_json"),
            ("getHTTPResponse", "get_http_response"),
            ("XMLHttpRequest", "xml_http_request"),
            ("JSONToPDF", "json_to_pdf"),
            ("readFile", "read_file"),
            ("HTTP2Json", "http2_json"),
            ("getHTTPResponses", "get_http_responses"),
        ] {
            assert_eq!(name_tokens(name), name_tokens(snake), "{name}");
            assert_eq!(jaccard(&name_tokens(name), &name_tokens(snake)), 1.0);
        }
        assert_eq!(name_tokens("URL"), BTreeSet::from(["url".to_string()]));
        assert_eq!(
            name_tokens("MCP_LIST"),
            BTreeSet::from(["mcp".to_string(), "list".to_string()])
        );
    }

    #[test]
    fn literals_keep_their_spaces_and_names_tokenise_alike() {
        assert_ne!(code_key("return \"a b\";"), code_key("return \"ab\";"));
        assert_eq!(code_key("return  1 ;\n"), code_key("return 1;"));
        assert_eq!(code_key("x = `a ${b}`"), code_key("x=`a ${b}`"));
        assert_eq!(name_tokens("readFile"), name_tokens("read_file"));
        assert_eq!(
            name_tokens("HTTPGetJson").len(),
            3,
            "{:?}",
            name_tokens("HTTPGetJson")
        );
        let none = BTreeSet::new();
        assert_eq!(
            fields_alike(&none, &none),
            1.0,
            "two no-input Tools are alike"
        );
    }
}
