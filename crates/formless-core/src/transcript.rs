//! Readable Session exports (SPEC §5.1): Markdown, with Tool calls folded
//! into `<details>`, and a self-contained HTML page. The JSON bundle stays
//! the re-importable format.

use formless_protocol::{MessageInfo, ServedModelInfo};
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag};
use serde_json::Value;
use std::collections::HashMap;

/// Each Tool argument/result longer than this is cut in exports.
const MAX_CALL_CHARS: usize = 4000;
const MARKDOWN_OPTIONS: Options = Options::ENABLE_TABLES.union(Options::ENABLE_STRIKETHROUGH);

/// One turn of the transcript, ready to render.
enum Part {
    User(String),
    Assistant(String, Option<ServedModelInfo>),
    Note(String),
    /// A Tool call: name, arguments, and its result once it came back.
    Call(String, String, Option<CallResult>),
}

/// Text and the explicit stored failure marker always travel together.
struct CallResult {
    text: String,
    failed: bool,
}

fn parts(msgs: &[MessageInfo], arguments: impl Fn(&str) -> String) -> Vec<Part> {
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
                let served_model = serde_json::from_value(meta["model"].clone()).ok();
                if !m.content.trim().is_empty() || served_model.is_some() {
                    out.push(Part::Assistant(m.content.clone(), served_model));
                }
                for c in meta["tool_calls"].as_array().into_iter().flatten() {
                    let args = c["function"]["arguments"].as_str().unwrap_or_default();
                    if let Some(id) = c["id"].as_str() {
                        calls.insert(id.to_string(), out.len());
                    }
                    let name = c["function"]["name"].as_str().unwrap_or("tool");
                    out.push(Part::Call(name.to_string(), arguments(args), None));
                }
            }
            "tool" => {
                let result = CallResult {
                    text: clip(&m.content),
                    failed: meta["is_error"].as_bool() == Some(true),
                };
                match meta["tool_call_id"].as_str().and_then(|id| calls.get(id)) {
                    Some(&i) => {
                        if let Part::Call(_, _, r) = &mut out[i] {
                            *r = Some(result);
                        }
                    }
                    None => {
                        let name = meta["data_source_tool"]
                            .as_str()
                            .or_else(|| meta["name"].as_str())
                            .unwrap_or("tool");
                        out.push(Part::Call(name.into(), String::new(), Some(result)));
                    }
                }
            }
            "system-note" => out.push(Part::Note(m.content.clone())),
            _ => {}
        }
    }
    out
}

fn clip(s: &str) -> String {
    let Some((end, _)) = s.char_indices().nth(MAX_CALL_CHARS) else {
        return s.to_string();
    };
    let mut out = s[..end].to_string();
    out.push_str("\n… (cut)");
    out
}

/// A code fence longer than any backtick run in `s`.
fn fenced(lang: &str, s: &str) -> String {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{lang}\n{}\n{fence}", s.trim_end())
}

/// HTML retains its existing pretty-printed, then clipped argument display.
fn html_arguments(args: &str) -> String {
    let pretty = serde_json::from_str::<Value>(args)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| args.to_string());
    clip(&pretty)
}

/// Remove complete leading blank lines, retaining indentation on the first
/// content line.
fn trim_turn_start(mut text: &str) -> &str {
    while let Some((line, rest)) = text.split_once('\n') {
        if !line.trim().is_empty() {
            break;
        }
        text = rest;
    }
    text
}

fn append_markdown_turn(out: &mut String, heading: &str, text: &str) {
    let text = trim_turn_start(text);
    out.push_str(heading);
    let start = out.len();
    let probe = text.trim_end_matches([' ', '\t', '\r', '\n']);
    let fence_start = if probe.contains("```") || probe.contains("~~~") {
        // Ask the Markdown parser whether a fenced block consumes a following
        // top-level heading. Fences in a quote or list may end with their
        // container. Trim ASCII whitespace so the probe accepts a final
        // closer padded with spaces or tabs as the export already does.
        out.push_str(probe);
        out.push_str("\n\n##");
        let boundary = probe.len() + 2;
        let fence = Parser::new_ext(&out[start..], MARKDOWN_OPTIONS)
            .into_offset_iter()
            .find_map(|(event, range)| {
                (matches!(
                    event,
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_)))
                ) && range.contains(&boundary))
                .then_some(range.start)
            });
        out.truncate(start);
        fence
    } else {
        None
    };
    if let Some(fence_start) = fence_start {
        // Reuse the original delimiter. Retain trailing whitespace here
        // because it is still part of the open fenced block.
        let opener = &text[fence_start..];
        let delimiter = opener.as_bytes()[0];
        let width = opener.bytes().take_while(|&c| c == delimiter).count();
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&opener[..width]);
    } else {
        out.push_str(text.trim_end());
    }
    out.push('\n');
}

/// The transcript as Markdown: user and assistant turns, notes as quotes,
/// and each Tool call as a collapsed `<details>` with its result.
pub fn markdown(name: &str, msgs: &[MessageInfo]) -> String {
    let mut out = format!("# {name}\n");
    for part in parts(msgs, clip) {
        out.push('\n');
        match part {
            Part::User(t) => append_markdown_turn(&mut out, "## You\n\n", &t),
            Part::Assistant(t, served) => {
                append_markdown_turn(&mut out, "## Assistant\n\n", &t);
                if let Some(served) = served {
                    out.push_str(&format!(
                        "\n*via {} ({})*\n",
                        escape_markdown_inline(&served.name),
                        escape_markdown_inline(&served.via)
                    ));
                }
            }
            Part::Note(t) => out.push_str(&format!("> *{}*\n", t.trim().replace('\n', " "))),
            Part::Call(tool, args, result) => {
                out.push_str(&format!(
                    "<details>\n<summary>Tool: {}{}</summary>\n\n",
                    escape(&tool),
                    if result.as_ref().is_some_and(|r| r.failed) {
                        " (failed)"
                    } else {
                        ""
                    }
                ));
                if !args.is_empty() {
                    out.push_str(&fenced("json", &args));
                    out.push_str("\n\n");
                }
                if let Some(r) = result {
                    out.push_str("Result:\n\n");
                    out.push_str(&fenced("", &r.text));
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

fn escape_markdown_inline(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for character in s.chars() {
        if character.is_control() {
            escaped.push(' ');
        } else {
            if character.is_ascii_punctuation() {
                escaped.push('\\');
            }
            escaped.push(character);
        }
    }
    escaped
}

/// Markdown to HTML with any raw HTML in it shown as text, so a message
/// can't add markup or scripts to the page. Images become their alt text
/// and address as text (an `<img>` would fetch from wherever it points
/// when the file is opened), and links keep only http(s) and mailto
/// destinations (#826).
fn md_html(s: &str) -> String {
    use pulldown_cmark::TagEnd;
    let mut in_image: Option<String> = None;
    let mut dropped_link = Vec::new();
    let mut events = Vec::new();
    for e in Parser::new_ext(s, MARKDOWN_OPTIONS) {
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
    for part in parts(msgs, html_arguments) {
        match part {
            Part::User(t) => body.push_str(&format!(
                "<section class=\"user\"><h2>You</h2>{}</section>\n",
                md_html(&t)
            )),
            Part::Assistant(t, served) => {
                let served = served.map_or_else(String::new, |served| {
                    format!(
                        "<p class=\"model\">via {} ({})</p>",
                        escape(&served.name),
                        escape(&served.via)
                    )
                });
                body.push_str(&format!(
                    "<section class=\"assistant\"><h2>Assistant</h2>{served}{}</section>\n",
                    md_html(&t)
                ));
            }
            Part::Note(t) => body.push_str(&format!("<p class=\"note\">{}</p>\n", escape(&t))),
            Part::Call(tool, args, result) => {
                body.push_str(&format!(
                    "<details><summary>Tool: {}{}</summary>",
                    escape(&tool),
                    if result.as_ref().is_some_and(|r| r.failed) {
                        " (failed)"
                    } else {
                        ""
                    }
                ));
                if !args.is_empty() {
                    body.push_str(&format!("<pre>{}</pre>", escape(&args)));
                }
                if let Some(r) = result {
                    body.push_str(&format!("<p>Result:</p><pre>{}</pre>", escape(&r.text)));
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
.model {{ color: var(--muted); font-size: .9em; margin: 0 0 8px; }}
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
            generated_by: None,
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
        assert!(md.contains("\"q\":\"TODO\""), "{md}");
        // A result with backticks gets a longer fence.
        assert!(md.contains("````\na.rs: 3"), "{md}");
        assert!(md.contains("> *The working directory is now /x.*"));
        assert!(md.trim_end().ends_with("There are **3**."));
    }

    #[test]
    fn markdown_repairs_fences_that_consume_following_turns() {
        for (content, closing) in [
            ("```rust\nlet x = 1;  \n   \n", "```"),
            (
                " ~~~~~text\r\n  ~~~\r\n  ~~~~~ still code\r\n```\r\n",
                "~~~~~",
            ),
            ("   ````rust\n```\n````\u{a0}", "````"),
            ("~~~", "~~~"),
            ("```rust\r\nvalue\r", "```"),
        ] {
            for (role, next_role, heading, next_heading) in [
                ("assistant", "user", "Assistant", "You"),
                ("user", "assistant", "You", "Assistant"),
            ] {
                let messages = [msg(role, content, None), msg(next_role, "Continue", None)];
                let before = serde_json::to_value(&messages).unwrap();
                let md = markdown("fixture", &messages);
                let newline = if content.ends_with('\n') { "" } else { "\n" };
                assert_eq!(
                    md,
                    format!(
                        "# fixture\n\n## {heading}\n\n{content}{newline}{closing}\n\n## {next_heading}\n\nContinue\n"
                    )
                );
                let mut rendered = String::new();
                html::push_html(&mut rendered, Parser::new(&md));
                assert!(
                    rendered.contains(&format!("<h2>{next_heading}</h2>\n<p>Continue</p>")),
                    "{rendered}"
                );
                assert_eq!(serde_json::to_value(&messages).unwrap(), before);
            }
        }
    }

    #[test]
    fn markdown_repairs_transcript_fences_before_tool_results() {
        let mut messages = sample();
        messages[1].content = "~~~text\nincomplete".into();
        let md = markdown("fixture", &messages);
        assert!(md.contains("~~~text\nincomplete\n~~~\n\n<details>"), "{md}");
        assert!(
            md.contains("Result:\n\n````\na.rs: 3\n```b```\n````"),
            "{md}"
        );
    }

    #[test]
    fn markdown_model_labels_cannot_inject_links_images_or_blocks() {
        let name = "![pixel](https://evil.example/pixel.png)\n\n[click](https://evil.example)";
        let message = MessageInfo {
            id: "assistant".into(),
            session_id: "session".into(),
            seq: 1,
            role: "assistant".into(),
            generated_by: None,
            content: "answer".into(),
            created_at: String::new(),
            meta: Some(
                serde_json::json!({"model":{"name":name,"via":"fallback","requested":"requested"}})
                    .to_string(),
            ),
        };
        let rendered = markdown("fixture", &[message]);
        assert!(!rendered.contains("![pixel]"), "{rendered}");
        assert!(!rendered.contains("[click]("), "{rendered}");
        let html = md_html(&rendered);
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("<a href"), "{html}");
    }

    #[test]
    fn markdown_keeps_balanced_fences_and_other_blocks_unchanged() {
        for content in [
            "````rust\n```\n   `````",
            " ~~~text\n  ~~~~ \t",
            "    ```rust\n    code",
            "\t~~~text\n\tcode",
            "```invalid`info\nplain",
            "inline ````code````",
            "> ```rust\n> code",
            "- ~~~text\n  code",
            "<div>\n```rust\n</div>",
        ] {
            let md = markdown(
                "fixture",
                &[
                    msg("assistant", content, None),
                    msg("user", "Continue", None),
                ],
            );
            assert_eq!(
                md,
                format!(
                    "# fixture\n\n## Assistant\n\n{}\n\n## You\n\nContinue\n",
                    content.trim_end()
                )
            );
        }
    }

    #[test]
    fn clipping_preserves_scalar_boundaries_and_both_export_outputs() {
        for unit in ["x", "é", "e\u{301}x", "👨‍👩‍👧‍👦", "\n… (cut)"] {
            for count in [0, 3999, 4000, 4001, 100_000] {
                let source: String = unit.chars().cycle().take(count).collect();
                let expected = if count <= MAX_CALL_CHARS {
                    source.clone()
                } else {
                    let prefix: String = unit.chars().cycle().take(MAX_CALL_CHARS).collect();
                    format!("{prefix}\n… (cut)")
                };
                assert_eq!(clip(&source), expected, "{unit:?}, {count} scalars");
                let messages = |text: &str| {
                    let metadata = serde_json::json!({"tool_calls":[{"id":"clip", "function":{"name":"fixture", "arguments":text}}]}).to_string();
                    vec![
                        msg("assistant", "", Some(&metadata)),
                        msg("tool", text, Some(r#"{"tool_call_id":"clip"}"#)),
                    ]
                };
                let original = messages(&source);
                let bounded = messages(&expected);
                assert_eq!(
                    markdown("fixture", &original),
                    markdown("fixture", &bounded)
                );
                assert_eq!(html("fixture", &original), html("fixture", &bounded));
            }
        }
    }

    #[test]
    fn multi_megabyte_tool_arguments_are_bounded_in_both_exports() {
        let arguments = serde_json::json!({"content": "<&é".repeat(1_000_000)}).to_string();
        let meta = serde_json::json!({"tool_calls": [{"id": "large", "function": {
            "name": "write", "arguments": arguments
        }}]})
        .to_string();
        let messages = [msg("assistant", "", Some(&meta))];
        for export in [markdown("large", &messages), html("large", &messages)] {
            assert!(export.contains("… (cut)"));
            // HTML escaping expands each bounded character by at most six
            // bytes; the remainder is the fixed transcript wrapper.
            assert!(
                export.len() < MAX_CALL_CHARS * 6 + 4000,
                "{} bytes",
                export.len()
            );
        }
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

    #[test]
    fn exports_show_served_model_for_assistant_turns_including_tool_calls() {
        let meta = serde_json::json!({
            "model": {"name": "served", "via": "fallback", "requested": "requested"},
            "tool_calls": [{"id": "c1", "function": {"name": "grep", "arguments": "{}"}}]
        })
        .to_string();
        let messages = [msg("assistant", "", Some(&meta))];
        let markdown = markdown("fixture", &messages);
        assert!(markdown.contains("*via served (fallback)*"), "{markdown}");
        assert!(
            markdown.contains("<summary>Tool: grep</summary>"),
            "{markdown}"
        );
        let html = html("fixture", &messages);
        assert!(
            html.contains("<p class=\"model\">via served (fallback)</p>"),
            "{html}"
        );
        assert!(html.contains("<summary>Tool: grep</summary>"), "{html}");
    }

    #[test]
    fn markdown_served_model_is_not_inside_answer_blockquote() {
        let meta = serde_json::json!({
            "model": {"name": "served", "via": "fallback", "requested": "requested"}
        })
        .to_string();
        let markdown = markdown(
            "fixture",
            &[msg("assistant", "> quoted answer", Some(&meta))],
        );
        assert!(
            markdown.contains("> quoted answer\n\n*via served (fallback)*\n"),
            "{markdown}"
        );

        let html = md_html(&markdown);
        let quote_end = html.find("</blockquote>").unwrap();
        let attribution = html.find("<em>via served (fallback)</em>").unwrap();
        assert!(attribution > quote_end, "{html}");
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

#[cfg(test)]
#[path = "transcript_result_tests.rs"]
mod transcript_result_tests;

#[cfg(test)]
#[path = "transcript_source_tests.rs"]
mod transcript_source_tests;
