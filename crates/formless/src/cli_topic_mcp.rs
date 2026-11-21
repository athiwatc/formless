use anyhow::{bail, Context, Result};
use formless_core::Paths;
use formless_protocol::{Request, TopicMcpOperation, TopicMcpRequest};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};

fn tools() -> Value {
    let mut tools = vec![];
    for (name, description, schema) in [
        (
            "list_conversations",
            "List shared Formless conversations",
            json!({"type":"object","properties":{}}),
        ),
        (
            "read_conversation",
            "Read a shared Formless conversation",
            json!({"type":"object","properties":{"session_id":{"type":"string"}},"required":["session_id"]}),
        ),
        (
            "append_conversation",
            "Append an assistant message to this attempt",
            json!({"type":"object","properties":{"content":{"type":"string"}},"required":["content"]}),
        ),
        (
            "read_artifact",
            "Read an input artifact by index",
            json!({"type":"object","properties":{"index":{"type":"integer","minimum":0}},"required":["index"]}),
        ),
        (
            "report_result",
            "Report step result and file/text artifacts; this does not complete the feature",
            json!({"type":"object","properties":{"status":{"type":"string","enum":["succeeded","failed","needs_user"]},"summary":{"type":"string"},"outputs":{"type":"array","items":{"type":"object"}}},"required":["status","summary","outputs"]}),
        ),
        (
            "ask_user",
            "Ask 1-4 structured questions; waits for Formless answers",
            json!({"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"question":{"type":"string"},"header":{"type":"string"},"options":{"type":"array","items":{"type":"object","properties":{"label":{"type":"string"},"description":{"type":"string"}},"required":["label"]}},"multi_select":{"type":"boolean"}},"required":["question"]}}},"required":["questions"]}),
        ),
        (
            "request_permission",
            "Raise a permission request for the user; cannot decide permissions",
            json!({"type":"object","properties":{"title":{"type":"string"},"kind":{"type":"string"},"raw_input":{"type":"object"},"options":{"type":"array","items":{"type":"object","properties":{"option_id":{"type":"string"},"name":{"type":"string"},"kind":{"type":"string","enum":["allow_once","allow_always","reject_once","reject_always"]}},"required":["option_id","name","kind"]}}},"required":["title","kind","options"]}),
        ),
    ] {
        tools.push(json!({"name":name,"description":description,"inputSchema":schema}));
    }
    json!({"tools":tools})
}

fn operation(name: &str, args: &Value) -> Result<TopicMcpOperation> {
    if !tools()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == name)
    {
        bail!("unknown scoped topic tool");
    }
    let mut args = args
        .as_object()
        .cloned()
        .context("tool arguments must be an object")?;
    args.insert("operation".into(), json!(name));
    Ok(serde_json::from_value(Value::Object(args))?)
}

fn respond(
    message: Value,
    mut proxy: impl FnMut(TopicMcpOperation) -> Result<Value>,
) -> Option<Value> {
    let id = message.get("id")?.clone();
    let result = match message["method"].as_str() {
        Some("initialize") => Ok(
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"formless-topic","version":env!("CARGO_PKG_VERSION")}}),
        ),
        Some("ping") => Ok(json!({})),
        Some("tools/list") => Ok(tools()),
        Some("tools/call") => {
            let params = &message["params"];
            match operation(params["name"].as_str().unwrap_or(""), &params["arguments"])
                .and_then(&mut proxy)
            {
                Ok(value) => Ok(
                    json!({"content":[{"type":"text","text":value.to_string()}],"isError":false}),
                ),
                Err(error) => Ok(
                    json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true}),
                ),
            }
        }
        _ => Err(anyhow::anyhow!("method not found")),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":error.to_string()}})
        }
    })
}

pub(crate) fn serve(paths: &Paths, attempt: &str) -> Result<()> {
    let token = std::env::var("FORMLESS_TOPIC_CAPABILITY")
        .context("topic MCP requires an attempt capability")?;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    loop {
        let mut line = vec![];
        if stdin
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(());
        }
        if line.len() > 1024 * 1024 {
            bail!("topic MCP request exceeds 1 MiB");
        }
        let response = match serde_json::from_slice(&line) {
            Ok(message) => respond(message, |operation| {
                formless_tui::client::request_once(
                    paths,
                    Request::TopicMcp {
                        request: TopicMcpRequest {
                            attempt_id: attempt.into(),
                            capability_token: token.clone(),
                            operation,
                        },
                    },
                )
            }),
            Err(_) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}),
            ),
        };
        if let Some(response) = response {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn topic_mcp_v2_tools_cannot_approve_start_or_decide_permissions() {
        for name in [
            "approve_topic",
            "reject_topic",
            "start_topic_workflow",
            "harness_permission_decision",
        ] {
            assert!(operation(name, &json!({})).is_err());
        }
        assert!(operation("ask_user", &json!({"questions":[]})).is_ok());
        assert!(respond(
            json!({"method":"notifications/initialized"}),
            |_| unreachable!()
        )
        .is_none());
        let response=respond(json!({"id":1,"method":"tools/call","params":{"name":"report_result","arguments":{"status":"succeeded","summary":"ok","outputs":[]}}}),|op|{
            assert!(matches!(op,TopicMcpOperation::ReportResult{..}));Ok(json!({"accepted":true}))
        }).unwrap();
        assert_eq!(response["result"]["isError"], false);
    }
}
