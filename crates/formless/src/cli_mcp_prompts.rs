//! Live MCP discovery and the daemon's existing text projection. Rendering is
//! buffered before stdout; no configuration or Session submission is involved.
use anyhow::{Context, Result};
use formless_protocol::Request;
use serde_json::{Map, Value};
use std::io::Write;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NamedArg {
    key: String,
    value: String,
}

pub(crate) fn parse_arg(raw: &str) -> std::result::Result<NamedArg, String> {
    let (key, value) = raw.split_once('=').ok_or("--arg requires KEY=VALUE")?;
    if key.is_empty() {
        return Err("--arg requires a nonempty key".into());
    }
    Ok(NamedArg {
        key: key.into(),
        value: value.into(),
    })
}

pub(crate) fn arguments(args: &[NamedArg]) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for arg in args {
        if out.contains_key(&arg.key) {
            return Err(crate::usage(format!("duplicate --arg key: {}", arg.key)));
        }
        out.insert(arg.key.clone(), Value::String(arg.value.clone()));
    }
    Ok(out)
}

pub(crate) fn list(
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<()> {
    let value = request(Request::ListMcpPrompts)?;
    let text = if json {
        format!("{}\n", serde_json::to_string_pretty(&value)?)
    } else {
        human_list(&value)?
    };
    out.write_all(text.as_bytes())?;
    Ok(())
}

fn human_list(value: &Value) -> Result<String> {
    let rows = value
        .as_array()
        .context("daemon prompt report was not an array")?;
    if rows.is_empty() {
        return Ok("no prompts reported by reachable enabled servers\n".into());
    }
    let mut out = String::from("Reported prompts (discovery may be incomplete):\n");
    for row in rows {
        out.push_str(&format!(
            "\nServer: {}\nPrompt: {}\n",
            row["server"].as_str().unwrap_or("(not reported)"),
            row["name"].as_str().unwrap_or("(not reported)")
        ));
        out.push_str(&format!(
            "Description: {}\n",
            row["description"]
                .as_str()
                .map(|s| s.lines().next().unwrap_or(""))
                .unwrap_or("(not reported)")
        ));
        if let Some(args) = row["arguments"].as_array() {
            let names: Vec<String> = args
                .iter()
                .filter_map(|a| {
                    a["name"]
                        .as_str()
                        .map(|name| match a["required"].as_bool() {
                            Some(true) => format!("{name} (required)"),
                            Some(false) => format!("{name} (optional)"),
                            None => name.into(),
                        })
                })
                .collect();
            out.push_str(&format!(
                "Arguments: {}\n",
                if names.is_empty() {
                    "none reported".into()
                } else {
                    names.join(", ")
                }
            ));
        } else {
            out.push_str("Arguments: (not reported)\n");
        }
    }
    Ok(out)
}

pub(crate) fn prompt(
    server: String,
    name: String,
    arguments: Map<String, Value>,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<()> {
    let value = request(Request::GetMcpPrompt {
        server,
        name,
        arguments,
        positional: Vec::new(),
    })?;
    let text = if json {
        format!("{}\n", serde_json::to_string_pretty(&value)?)
    } else {
        value["text"]
            .as_str()
            .context("daemon prompt report did not contain text")?
            .into()
    };
    out.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use serde_json::json;

    #[test]
    fn parser_preserves_named_strings_and_rejects_bad_arguments_before_setup() {
        for args in [
            vec!["--arg", "missing"],
            vec!["--arg", "=empty"],
            vec!["--arg", "x=1", "--arg", "x=2"],
        ] {
            let mut cmd = vec!["formless", "mcp", "prompt", "server", "name"];
            cmd.extend(args);
            match crate::Cli::try_parse_from(cmd) {
                Err(e) => assert_eq!(e.exit_code(), 2),
                Ok(cli) => {
                    let error = crate::validate_mcp_prompt_args(&cli).unwrap_err();
                    assert_eq!(crate::exit_code(&error), 2);
                }
            }
        }
        let cli = crate::Cli::try_parse_from([
            "formless",
            "mcp",
            "prompt",
            "日本語 server",
            "full-name",
            "--arg",
            "key=space value=a=b",
            "--arg",
            "empty=",
            "--arg",
            "json={\"x\":1}",
            "--json",
        ])
        .unwrap();
        crate::validate_mcp_prompt_args(&cli).unwrap();
        let Some(crate::Cmd::Mcp {
            action:
                crate::McpAction::Prompt {
                    server,
                    name,
                    args,
                    json,
                },
        }) = cli.cmd
        else {
            panic!("prompt")
        };
        assert_eq!(server, "日本語 server");
        assert_eq!(name, "full-name");
        assert!(json);
        assert_eq!(
            arguments(&args).unwrap(),
            json!({"key":"space value=a=b","empty":"","json":"{\"x\":1}"})
                .as_object()
                .unwrap()
                .clone()
        );
        for verb in ["prompts", "prompt"] {
            let mut root = crate::Cli::command();
            let mcp = root.find_subcommand_mut("mcp").unwrap();
            let help = mcp
                .find_subcommand_mut(verb)
                .unwrap()
                .render_long_help()
                .to_string();
            assert!(help.contains("--json"));
            assert!(help.contains("start"));
            if verb == "prompt" {
                assert!(help.contains("projection"));
                assert!(help.contains("non-text"));
            }
        }
    }

    #[test]
    fn list_preserves_order_identifiers_optional_metadata_and_complete_json() {
        let long = "server-".to_string() + &"日本語".repeat(100);
        let report = json!([
            {"server":long,"name":"prompt-ไทย","description":"first line\nsecond line","arguments":[{"name":"required-long-name","required":true},{"name":"optional","required":false},{"name":"unknown","required":null},{"name":7}],"future":{"x":[1,"🦀"]}},
            {"server":"second","name":"prompt-two","description":null,"arguments":null},
            {"server":"third","name":"prompt-three","description":3}
        ]);
        let mut calls = 0;
        let mut bytes = Vec::new();
        list(
            false,
            |req| {
                calls += 1;
                assert!(matches!(req, Request::ListMcpPrompts));
                Ok(report.clone())
            },
            &mut bytes,
        )
        .unwrap();
        assert_eq!(calls, 1);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(&long));
        assert!(text.find(&long) < text.find("Server: second"));
        assert!(text.contains("Prompt: prompt-ไทย"));
        assert!(!text.contains("second line"));
        assert!(text.contains("required-long-name (required), optional (optional), unknown"));
        assert!(!text.contains("unknown (optional)"));
        assert_eq!(text.matches("Arguments: (not reported)").count(), 2);
        assert!(text.contains("may be incomplete"));
        let mut bytes = Vec::new();
        list(true, |_| Ok(report.clone()), &mut bytes).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), report);
        let mut bytes = Vec::new();
        list(false, |_| Ok(json!([])), &mut bytes).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "no prompts reported by reachable enabled servers\n"
        );
    }

    #[test]
    fn detail_is_verbatim_and_failures_do_not_write_partial_reports() {
        let args = arguments(&[
            parse_arg("x= space =🦀 ").unwrap(),
            parse_arg("empty=").unwrap(),
        ])
        .unwrap();
        for text in ["", "no newline", "日本語\n\n"] {
            for json in [false, true] {
                let report = json!({"text":text,"future":{"roles":"not reconstructed"}});
                let mut bytes = Vec::new();
                let mut calls = 0;
                prompt(
                    "server".into(),
                    "name".into(),
                    args.clone(),
                    json,
                    |req| {
                        calls += 1;
                        let Request::GetMcpPrompt {
                            server,
                            name,
                            arguments,
                            positional,
                        } = req
                        else {
                            panic!("unexpected request")
                        };
                        assert_eq!(server, "server");
                        assert_eq!(name, "name");
                        assert_eq!(arguments, args);
                        assert!(positional.is_empty());
                        Ok(report.clone())
                    },
                    &mut bytes,
                )
                .unwrap();
                assert_eq!(calls, 1);
                if json {
                    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), report);
                } else {
                    assert_eq!(bytes, text.as_bytes());
                }
            }
        }
        for json in [false, true] {
            let mut bytes = Vec::new();
            let error = list(json, |_| anyhow::bail!("server failure"), &mut bytes).unwrap_err();
            assert_eq!(error.to_string(), "server failure");
            assert!(bytes.is_empty());
            let error = prompt(
                "s".into(),
                "p".into(),
                args.clone(),
                json,
                |_| anyhow::bail!("daemon failure"),
                &mut bytes,
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "daemon failure");
            assert!(bytes.is_empty());
        }
    }
}
