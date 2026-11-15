//! Print the complete restart response before interpreting its verdict.
use super::*;

pub(super) fn run(
    name: &str,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<bool> {
    let reply = request(Request::RestartMcpServer { name: name.into() })?;
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&reply)?)?;
    } else {
        writeln!(out, "server:  {name}")?;
        match reply["error"].as_str() {
            Some(error) => writeln!(out, "error:   {error}")?,
            None => writeln!(out, "tools:   {}", reply["tools"])?,
        }
    }
    out.flush()?;
    match reply.get("error") {
        Some(Value::String(_)) => Ok(false),
        Some(Value::Null)
            if reply["restarted"].as_str() == Some(name) && reply["tools"].as_u64().is_some() =>
        {
            Ok(true)
        }
        _ => bail!(
            "invalid MCP restart reply for '{}'",
            comparison_terminal_text(name)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restart_reports_keep_complete_json_and_zero_tool_success() {
        for tools in [0_u64, 3, u64::MAX] {
            let reply =
                json!({"restarted":"ไทย","tools":tools,"error":null,"future":{"kept":[null,true]}});
            for json_mode in [false, true] {
                let mut requests = Vec::new();
                let mut out = Vec::new();
                assert!(run(
                    "ไทย",
                    json_mode,
                    |request| {
                        requests.push(request);
                        Ok(reply.clone())
                    },
                    &mut out
                )
                .unwrap());
                assert!(
                    matches!(&requests[..], [Request::RestartMcpServer { name }] if name == "ไทย")
                );
                if json_mode {
                    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), reply);
                } else {
                    assert_eq!(
                        String::from_utf8(out).unwrap(),
                        format!("server:  ไทย\ntools:   {tools}\n")
                    );
                }
            }
        }
    }

    #[test]
    fn failure_and_invalid_replies_are_reported_before_the_verdict() {
        for reply in [
            json!({"error":"restart error 日本語","tools":null}),
            json!({"error":"","tools":null}),
            json!({"future":true}),
            json!({"error":false}),
            json!({"restarted":"other","tools":0,"error":null}),
            json!({"restarted":"fixture","tools":-1,"error":null}),
            json!({"restarted":"fixture","tools":1.5,"error":null}),
        ] {
            for json_mode in [false, true] {
                let mut out = Vec::new();
                let verdict = run("fixture", json_mode, |_| Ok(reply.clone()), &mut out);
                if reply["error"].is_string() {
                    assert!(!verdict.unwrap());
                } else {
                    assert!(verdict
                        .unwrap_err()
                        .to_string()
                        .contains("invalid MCP restart reply"));
                }
                if json_mode {
                    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), reply);
                } else {
                    let expected = match reply["error"].as_str() {
                        Some(error) => format!("server:  fixture\nerror:   {error}\n"),
                        None => format!("server:  fixture\ntools:   {}\n", reply["tools"]),
                    };
                    assert_eq!(out, expected.as_bytes());
                }
            }
        }
    }

    #[test]
    fn request_and_flush_failures_propagate_without_a_reported_verdict() {
        let mut out = Vec::new();
        let error = run("fixture", true, |_| bail!("request failed"), &mut out).unwrap_err();
        assert_eq!(error.to_string(), "request failed");
        assert!(out.is_empty());
        struct BrokenFlush(Vec<u8>);
        impl Write for BrokenFlush {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }
        let mut out = BrokenFlush(Vec::new());
        assert!(run(
            "fixture",
            true,
            |_| Ok(json!({"error":"reported failure"})),
            &mut out
        )
        .unwrap_err()
        .to_string()
        .contains("flush failed"));
        assert_eq!(
            serde_json::from_slice::<Value>(&out.0).unwrap(),
            json!({"error":"reported failure"})
        );
    }
}
