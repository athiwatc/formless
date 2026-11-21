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

/// Most hook output kept (stdout and stderr each); the rest is read and
/// dropped, so a chatty hook can't grow the daemon (#827).
pub const MAX_HOOK_OUTPUT: usize = 64 * 1024;
const PIPE_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(150);

/// Run a hook command (no shell) with the payload on stdin. Exit 0
/// allows; otherwise its output is the reason. The payload is written
/// while the output is read, so a hook that echoes a large payload
/// (`cat`, `jq`) can't deadlock on a full pipe (#827).
pub async fn run_command(
    cmd: &str,
    args: &[String],
    payload: &Value,
    timeout: std::time::Duration,
) -> (bool, String) {
    use formless_runtime::process_group::SupervisedChild;
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut command = tokio::process::Command::new(cmd);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match SupervisedChild::spawn(command) {
        Ok(child) => child,
        Err(e) => return (false, format!("hook command {cmd} failed to start: {e}")),
    };
    let body = payload.to_string().into_bytes();
    let (stdin, stdout, stderr) = child.take_pipes();
    let writer = async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&body).await;
            // Dropped here: the hook sees end of input.
        }
    };
    // Keep bytes read before the drain deadline even when a descendant
    // holds its inherited writer open.
    async fn capped<R: tokio::io::AsyncRead + Unpin>(r: Option<R>, kept: &mut Vec<u8>) {
        let Some(mut r) = r else { return };
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = MAX_HOOK_OUTPUT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exited = {
        let capture = async {
            tokio::join!(writer, capped(stdout, &mut out), capped(stderr, &mut err));
        };
        let wait = tokio::time::timeout(timeout, child.exited());
        tokio::pin!(capture, wait);
        tokio::select! {
            result = &mut wait => {
                if matches!(result, Ok(Ok(()))) {
                    let _ = tokio::time::timeout(PIPE_DRAIN_GRACE, &mut capture).await;
                }
                result
            }
            () = &mut capture => wait.await,
        }
    };
    let status = match exited {
        Ok(Ok(())) => child.finish().await,
        Ok(Err(e)) => Err(e),
        Err(_) => return (false, format!("hook command {cmd} timed out")),
    };
    match status {
        Ok(status) => {
            let mut text = String::from_utf8_lossy(&out).trim().to_string();
            if text.is_empty() {
                text = String::from_utf8_lossy(&err).trim().to_string();
            }
            (status.success(), text.chars().take(2000).collect())
        }
        Err(e) => (false, format!("hook command {cmd} failed: {e}")),
    }
}

#[cfg(all(test, unix))]
mod pipe_tests {
    /// `cat` given a payload far over a pipe buffer finishes, and its
    /// echoed output is capped (#827).
    #[tokio::test]
    async fn an_echoing_hook_never_deadlocks() {
        let payload = serde_json::json!({"arguments": "x".repeat(400 * 1024)});
        let started = std::time::Instant::now();
        let (ok, out) =
            super::run_command("cat", &[], &payload, std::time::Duration::from_secs(20)).await;
        assert!(ok, "{out}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(out.chars().count() <= 2000);
    }

    #[tokio::test]
    async fn a_successful_hook_with_inherited_pipes_finishes_and_reaps_descendants() {
        use std::time::{Duration, Instant};
        let temp = std::env::temp_dir().join(format!("formless-hook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp).unwrap();
        let marker = temp.join("child-survived");
        let args = vec![
            "-c".to_string(),
            r#"(sleep 1; printf survived > "$1") & printf 'allowed\n'"#.to_string(),
            "formless-hook-fixture".to_string(),
            marker.to_string_lossy().into_owned(),
        ];
        let start = Instant::now();
        let (ok, out) =
            super::run_command("sh", &args, &serde_json::json!({}), Duration::from_secs(5)).await;
        assert!(ok, "{out}");
        assert_eq!(out, "allowed");
        assert!(start.elapsed() < Duration::from_millis(850));
        tokio::time::sleep_until((start + Duration::from_millis(1200)).into()).await;
        let child_survived = marker.exists();
        std::fs::remove_dir_all(&temp).unwrap();
        assert!(!child_survived, "a descendant outlived the hook");
    }

    #[tokio::test]
    async fn a_running_hook_still_times_out() {
        use std::time::Duration;
        let args = vec!["-c".into(), "sleep 2".into()];
        let (ok, out) = super::run_command(
            "sh",
            &args,
            &serde_json::json!({}),
            Duration::from_millis(100),
        )
        .await;
        assert!(!ok);
        assert_eq!(out, "hook command sh timed out");
    }
}
