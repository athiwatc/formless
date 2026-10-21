//! Hooks (SPEC §9.8): matching and running user-defined hooks.

use crate::config::HookCfg;
use serde_json::Value;

/// `*` wildcard match ("mcp__*", "*_file", "read_*_text").
pub fn glob(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else if let Some(at) = rest.find(p) {
            rest = &rest[at + p.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Whether a hook applies to a call of `tool` declaring `caps`.
pub fn matches(h: &HookCfg, tool: &str, caps: &[String]) -> bool {
    h.tool.as_deref().is_none_or(|p| glob(p, tool))
        && h.capability
            .as_deref()
            .is_none_or(|c| caps.iter().any(|x| x.starts_with(c)))
}

/// A hook Tool's verdict: `{deny: true, reason}` refuses, anything else
/// allows (its `reason`/`message` or JSON is the output).
pub fn tool_verdict(v: &Value) -> (bool, String) {
    let reason = v["reason"]
        .as_str()
        .or(v["message"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string());
    (v["deny"] != true, reason)
}

/// Run a hook command (no shell) with the payload on stdin. Exit 0
/// allows; otherwise its output is the reason.
pub async fn run_command(
    cmd: &str,
    args: &[String],
    payload: &Value,
    timeout: std::time::Duration,
) -> (bool, String) {
    use tokio::io::AsyncWriteExt;
    let child = tokio::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (false, format!("hook command {cmd} failed to start: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.to_string().as_bytes()).await;
    }
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if text.is_empty() {
                text = err;
            }
            (out.status.success(), text.chars().take(2000).collect())
        }
        Ok(Err(e)) => (false, format!("hook command {cmd} failed: {e}")),
        Err(_) => (false, format!("hook command {cmd} timed out")),
    }
}
