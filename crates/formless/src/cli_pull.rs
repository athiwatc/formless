use anyhow::{anyhow, Context, Result};
use formless_core::paths::Paths;
use formless_protocol::{Event, Request, ServerMessage};
use formless_tui::client::DaemonClient;
use std::time::Duration;

pub fn run(paths: &Paths, model: &str, provider: Option<&str>) -> Result<()> {
    formless_core::daemon::ensure_daemon(paths)?;
    let (client, events) = DaemonClient::connect(paths)?;
    let ping_id = client.send(Request::Ping);
    loop {
        match events
            .recv_timeout(Duration::from_secs(15))
            .context("checking daemon pull support")?
        {
            ServerMessage::Response {
                id,
                ok,
                result,
                error,
            } if id == ping_id => {
                if !ok {
                    return Err(anyhow!(
                        error.unwrap_or_else(|| "daemon pull capability check failed".into())
                    ));
                }
                require_pull_jobs_feature(&result.unwrap_or_default())?;
                break;
            }
            _ => {}
        }
    }
    let subscribe_id = client.send(Request::Subscribe {
        sessions: None,
        deltas: false,
        client: Some("plain CLI pull".into()),
    });
    loop {
        match events
            .recv_timeout(Duration::from_secs(15))
            .context("waiting for daemon subscription")?
        {
            ServerMessage::Response { id, ok, error, .. } if id == subscribe_id => {
                if !ok {
                    return Err(anyhow!(
                        error.unwrap_or_else(|| "subscription failed".into())
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    let request_id = client.send(Request::OllamaPull {
        model: model.to_string(),
        provider: provider.map(str::to_string),
    });
    let mut pending = Vec::new();
    let pull_id = loop {
        match events
            .recv()
            .context("daemon connection closed during model pull")?
        {
            ServerMessage::Response {
                id,
                ok,
                result,
                error,
            } if id == request_id => {
                if !ok {
                    return Err(anyhow!(
                        error.unwrap_or_else(|| "model pull could not start".into())
                    ));
                }
                break result
                    .and_then(|result| result["pull_id"].as_str().map(str::to_string))
                    .context("daemon did not return a pull ID")?;
            }
            ServerMessage::Event(event) => pending.push(event),
            _ => {}
        }
    };

    for event in pending {
        if let Some(success) = print_progress(&event, &pull_id, &paths.profile) {
            return terminal_result(success);
        }
    }
    loop {
        let message = events
            .recv()
            .context("daemon connection closed during model pull")?;
        if let ServerMessage::Event(event) = message {
            if let Some(success) = print_progress(&event, &pull_id, &paths.profile) {
                return terminal_result(success);
            }
        }
    }
}

fn terminal_result(success: bool) -> Result<()> {
    if success {
        Ok(())
    } else {
        Err(anyhow!("model pull did not complete"))
    }
}

fn resume_command(model: &str, provider: Option<&str>, profile: &str) -> String {
    let provider = provider
        .filter(|provider| *provider != formless_core::config::DEFAULT_PROVIDER)
        .map(|provider| format!(" --provider {provider}"))
        .unwrap_or_default();
    let profile = if profile == "default" {
        String::new()
    } else {
        format!("--profile {profile} ")
    };
    format!("formless {profile}pull {model}{provider}")
}

fn print_progress(event: &Event, pull_id: &str, profile: &str) -> Option<bool> {
    let Event::ModelPullProgress {
        model,
        status,
        completed,
        total,
        done,
        error,
        pull_id: Some(event_id),
        provider,
        requested_model,
        expected_size,
        warning,
        ..
    } = event
    else {
        return None;
    };
    if event_id != pull_id {
        return None;
    }
    let progress = match (completed, total) {
        (Some(completed), Some(total)) if *total > 0 => {
            format!(
                " {}% ({} / {} bytes)",
                completed * 100 / total,
                completed,
                total
            )
        }
        _ => String::new(),
    };
    let expected = expected_size
        .map(|size| format!("; expected size {size} bytes"))
        .unwrap_or_default();
    println!(
        "pull {} {}: {}{progress}{expected}",
        terminal_safe(pull_id),
        terminal_safe(model),
        terminal_safe(status)
    );
    if let Some(warning) = warning {
        println!("warning: {}", terminal_safe(warning));
    }
    if *done {
        if let Some(error) = error {
            println!("error: {}", terminal_safe(error));
            if status == "incomplete" {
                let resume = resume_command(
                    requested_model.as_deref().unwrap_or(model),
                    provider.as_deref(),
                    profile,
                );
                println!(
                    "partial download can be resumed with `{}`",
                    terminal_safe(&resume)
                );
            }
        } else if status == "success" {
            println!("pulled {}; select it with `/model`", terminal_safe(model));
        }
        Some(error.is_none() && status == "success")
    } else {
        None
    }
}

fn terminal_safe(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control()
            || matches!(
                ch,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{2028}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            safe.extend(ch.escape_default());
        } else {
            safe.push(ch);
        }
    }
    safe
}

fn require_pull_jobs_feature(result: &serde_json::Value) -> Result<()> {
    if result["features"].as_array().is_some_and(|features| {
        features
            .iter()
            .any(|feature| feature.as_str() == Some(formless_protocol::OLLAMA_PULL_JOBS_FEATURE))
    }) {
        Ok(())
    } else {
        Err(anyhow!(
            "the running daemon does not support tracked Ollama pulls; restart it with this version"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_event(status: &str, error: Option<&str>) -> Event {
        Event::ModelPullProgress {
            model: "llama3".into(),
            status: status.into(),
            completed: None,
            total: None,
            done: true,
            error: error.map(str::to_string),
            pull_id: Some("pull-1".into()),
            provider: Some("remote".into()),
            requested_model: Some("A".into()),
            expected_size: None,
            warning: None,
        }
    }

    #[test]
    fn success_and_failure_events_map_to_cli_exit_codes() {
        let cli_exit_code = |result: Result<()>| {
            result
                .map(|()| 0)
                .unwrap_or_else(|error| crate::exit_code(&error))
        };
        let success =
            print_progress(&terminal_event("success", None), "pull-1", "default").unwrap();
        assert_eq!(cli_exit_code(terminal_result(success)), 0);

        let failure = print_progress(
            &terminal_event("incomplete", Some("stream ended")),
            "pull-1",
            "research",
        )
        .unwrap();
        assert_eq!(cli_exit_code(terminal_result(failure)), 1);
        assert_eq!(
            resume_command("llama3", Some("remote"), "research"),
            "formless --profile research pull llama3 --provider remote"
        );
        assert_eq!(
            resume_command("llama3", None, "default"),
            "formless pull llama3"
        );
    }

    #[test]
    fn resume_command_preserves_requested_alias_and_provider_profile() {
        assert_eq!(
            resume_command("A", Some("remote"), "research"),
            "formless --profile research pull A --provider remote"
        );
    }

    #[test]
    fn terminal_output_escapes_control_sequences() {
        assert_eq!(
            terminal_safe("Ollama\u{1b}[2J\u{202e}"),
            "Ollama\\u{1b}[2J\\u{202e}"
        );
    }

    #[test]
    fn tracked_pull_requires_daemon_feature_before_dispatch() {
        assert!(require_pull_jobs_feature(&serde_json::json!({
            "features": [formless_protocol::OLLAMA_PULL_JOBS_FEATURE]
        }))
        .is_ok());
        assert!(require_pull_jobs_feature(&serde_json::json!({"features": []})).is_err());
        assert!(require_pull_jobs_feature(&serde_json::Value::Null).is_err());
    }
}
