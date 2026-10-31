//! Readable Session exports (SPEC §5.1): Markdown, with Tool calls folded
//! into `<details>`, and a self-contained HTML page. The JSON bundle stays
//! the re-importable format.

use formless_protocol::MessageInfo;
use pulldown_cmark::{html, Event, Options, Parser};
use serde_json::Value;
use std::collections::HashMap;

/// Tool output longer than this is cut in exports.
const MAX_RESULT_CHARS: usize = 4000;

/// One turn of the transcript, ready to render.
enum Part {
    User(String),
    Assistant(String),
    Note(String),
    /// A Tool call: name, arguments, and its result once it came back.
    Call(String, String, Option<String>),
}

fn parts(msgs: &[MessageInfo]) -> Vec<Part> {
    let mut out = Vec::new();
    // Tool call id → index in `out`, to attach results.
    let mut calls: HashMap<String, usize> = HashMap::new();
    for m in msgs {
        let meta: Value = m
            .meta
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        match m.role.as_str() {
            "user" => out.push(Part::User(m.content.clone())),
            "assistant" => {
                if !m.content.trim().is_empty() {
                    out.push(Part::Assistant(m.content.clone()));
                }
                for c in meta["tool_calls"].as_array().into_iter().flatten() {
                    let args = c["function"]["arguments"].as_str().unwrap_or_default();
                    // Pretty-print JSON arguments when they parse.
                    let args = serde_json::from_str::<Value>(args)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                        .unwrap_or_else(|| args.to_string());
                    if let Some(id) = c["id"].as_str() {
                        calls.insert(id.to_string(), out.len());
                    }
                    let name = c["function"]["name"].as_str().unwrap_or("tool");
                    out.push(Part::Call(name.to_string(), args, None));
                }
            }
            "tool" => {
                let result = clip(&m.content);
                match meta["tool_call_id"].as_str().and_then(|id| calls.get(id)) {
                    Some(&i) => {
                        if let Part::Call(_, _, r) = &mut out[i] {
                            *r = Some(result);
                        }
                    }
                    None => out.push(Part::Call("tool".into(), String::new(), Some(result))),
                }
            }
            "system-note" => out.push(Part::Note(m.content.clone())),
            _ => {}
        }
    }
    out
}

fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_RESULT_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_RESULT_CHARS).collect();
    out.push_str("\n… (cut)");
    out
}

/// A code fence longer than any backtick run in `s`.
fn fenced(lang: &str, s: &str) -> String {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{lang}\n{}\n{fence}", s.trim_end())
}

/// The transcript as Markdown: user and assistant turns, notes as quotes,
/// and each Tool call as a collapsed `<details>` with its result.
pub fn markdown(name: &str, msgs: &[MessageInfo]) -> String {
    let mut out = format!("# {name}\n");
    for part in parts(msgs) {
        out.push('\n');
        match part {
            Part::User(t) => out.push_str(&format!("## You\n\n{}\n", t.trim())),
            Part::Assistant(t) => out.push_str(&format!("## Assistant\n\n{}\n", t.trim())),
            Part::Note(t) => out.push_str(&format!("> *{}*\n", t.trim().replace('\n', " "))),
            Part::Call(tool, args, result) => {
                out.push_str(&format!(
                    "<details>\n<summary>Tool: {}</summary>\n\n",
                    escape(&tool)
                ));
                if !args.is_empty() {
                    out.push_str(&fenced("json", &args));
                    out.push_str("\n\n");
                }
                if let Some(r) = result {
                    out.push_str("Result:\n\n");
                    out.push_str(&fenced("", &r));
                    out.push_str("\n\n");
                }
                out.push_str("</details>\n");
            }
        }
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Markdown to HTML with any raw HTML in it shown as text, so a message
/// can't add markup or scripts to the page. Images become their alt text
/// and address as text (an `<img>` would fetch from wherever it points
/// when the file is opened), and links keep only http(s) and mailto
/// destinations (#826).
fn md_html(s: &str) -> String {
    use pulldown_cmark::{Tag, TagEnd};
    let mut in_image: Option<String> = None;
    let mut dropped_link = Vec::new();
    let mut events = Vec::new();
    for e in Parser::new_ext(s, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        match e {
            Event::Html(t) | Event::InlineHtml(t) => events.push(Event::Text(t)),
            Event::Start(Tag::Image { dest_url, .. }) => {
                events.push(Event::Text("[image: ".into()));
                in_image = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                let url = in_image.take().unwrap_or_default();
                events.push(Event::Text(format!("] ({url})").into()));
            }
            Event::Start(Tag::Link { ref dest_url, .. }) => {
                let lower = dest_url.trim().to_ascii_lowercase();
                let safe = ["http://", "https://", "mailto:", "#"]
                    .iter()
                    .any(|p| lower.starts_with(p))
                    || !lower.contains(':');
                dropped_link.push(!safe);
                if safe {
                    events.push(e);
                }
            }
            Event::End(TagEnd::Link) => {
                if dropped_link.pop() != Some(true) {
                    events.push(e);
                }
            }
            e => events.push(e),
        }
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}

/// A self-contained HTML page of the transcript (no scripts, no external
/// resources).
pub fn html(name: &str, msgs: &[MessageInfo]) -> String {
    let mut body = String::new();
    for part in parts(msgs) {
        match part {
            Part::User(t) => body.push_str(&format!(
                "<section class=\"user\"><h2>You</h2>{}</section>\n",
                md_html(&t)
            )),
            Part::Assistant(t) => body.push_str(&format!(
                "<section class=\"assistant\"><h2>Assistant</h2>{}</section>\n",
                md_html(&t)
            )),
            Part::Note(t) => body.push_str(&format!("<p class=\"note\">{}</p>\n", escape(&t))),
            Part::Call(tool, args, result) => {
                body.push_str(&format!(
                    "<details><summary>Tool: {}</summary>",
                    escape(&tool)
                ));
                if !args.is_empty() {
                    body.push_str(&format!("<pre>{}</pre>", escape(&args)));
                }
                if let Some(r) = result {
                    body.push_str(&format!("<p>Result:</p><pre>{}</pre>", escape(&r)));
                }
                body.push_str("</details>\n");
            }
        }
    }
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; --fg: #1d1d1f; --bg: #fff; --muted: #6e6e73; --line: #d2d2d7; --code: #f5f5f7; }}
@media (prefers-color-scheme: dark) {{ :root {{ --fg: #f5f5f7; --bg: #1c1c1e; --muted: #a1a1a6; --line: #3a3a3c; --code: #2c2c2e; }} }}
body {{ font: 16px/1.55 system-ui, sans-serif; color: var(--fg); background: var(--bg); max-width: 760px; margin: 0 auto; padding: 24px 16px; }}
h1 {{ font-size: 1.6em; }} h2 {{ font-size: .8em; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); margin: 0 0 4px; }}
section {{ border-top: 1px solid var(--line); padding: 12px 0; }}
pre, code {{ background: var(--code); border-radius: 6px; font: 13px/1.45 ui-monospace, monospace; }}
pre {{ padding: 10px 12px; overflow-x: auto; white-space: pre-wrap; overflow-wrap: anywhere; }}
details {{ border: 1px solid var(--line); border-radius: 8px; padding: 6px 10px; margin: 8px 0; }}
summary {{ cursor: pointer; color: var(--muted); }}
.note {{ color: var(--muted); font-style: italic; }}
table {{ border-collapse: collapse; display: block; overflow-x: auto; }} td, th {{ border: 1px solid var(--line); padding: 4px 8px; }}
</style></head>
<body><h1>{title}</h1>
{body}</body></html>
"#,
        title = escape(name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str, meta: Option<&str>) -> MessageInfo {
        MessageInfo {
            id: String::new(),
            session_id: String::new(),
            seq: 0,
            role: role.into(),
            content: content.into(),
            created_at: String::new(),
            meta: meta.map(Into::into),
        }
    }

    fn sample() -> Vec<MessageInfo> {
        vec![
            msg("user", "count the TODOs <script>x</script>", None),
            msg(
                "assistant",
                "",
                Some(
                    r#"{"tool_calls":[{"id":"c1","function":{"name":"grep","arguments":"{\"q\":\"TODO\"}"}}]}"#,
                ),
            ),
            msg("tool", "a.rs: 3\n```b```", Some(r#"{"tool_call_id":"c1"}"#)),
            msg("system-note", "The working directory is now /x.", None),
            msg("assistant", "There are **3**.", None),
        ]
    }

    #[test]
    fn markdown_folds_tool_calls_with_their_results() {
        let md = markdown("TODOs", &sample());
        assert!(md.starts_with("# TODOs\n"));
        assert!(md.contains("## You\n\ncount the TODOs"));
        assert!(md.contains("<summary>Tool: grep</summary>"));
        assert!(md.contains("\"q\": \"TODO\""), "{md}");
        // A result with backticks gets a longer fence.
        assert!(md.contains("````\na.rs: 3"), "{md}");
        assert!(md.contains("> *The working directory is now /x.*"));
        assert!(md.trim_end().ends_with("There are **3**."));
    }

    #[test]
    fn html_is_self_contained_and_escapes_message_markup() {
        let page = html("A <b>name</b>", &sample());
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<title>A &lt;b&gt;name&lt;/b&gt;</title>"));
        assert!(!page.contains("<script>"), "message HTML is shown as text");
        assert!(page.contains("<strong>3</strong>"));
        assert!(page.contains("<summary>Tool: grep</summary>"));
        assert!(!page.contains("http"), "no external resources");
    }

    /// Tool output is attacker-controlled: an exported page must not fetch
    /// an image or carry a script link (#826).
    #[test]
    fn exports_neither_fetch_nor_script() {
        let page = html(
            "t",
            &[msg(
                "assistant",
                "![pixel](https://attacker.example/pixel) [click](javascript:alert(1)) [data](data:text/html,x) [ok](https://example.com) [rel](notes.md)",
                None,
            )],
        );
        assert!(!page.contains("<img"), "{page}");
        assert!(!page.contains("src="), "{page}");
        assert!(
            !page.to_lowercase().contains("href=\"javascript:"),
            "{page}"
        );
        assert!(!page.contains("href=\"data:"), "{page}");
        assert!(
            page.contains("[image: pixel] (https://attacker.example/pixel)"),
            "{page}"
        );
        assert!(page.contains("click") && page.contains("href=\"https://example.com\""));
        assert!(page.contains("href=\"notes.md\""), "relative links stay");
    }
}
