//! Read-only saved templates through a synthetic local daemon; no model calls.
use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::TemplateInfo;
use serde_json::Value;
use std::{path::PathBuf, process::Output, sync::Arc};
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(tokio::task::JoinHandle<anyhow::Result<()>>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn cli(paths: &Paths, args: &[&str]) -> Output {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_formless"));
    cmd.env_clear();
    for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd.args(args)
        .env("FORMLESS_DATA_DIR", &paths.data_dir)
        .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
        .env("NO_COLOR", "1")
        .output()
        .await
        .unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn saved_template_cli_preserves_reply_order_exact_names_json_nulls_and_complete_bodies() {
    let token = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-tpl-{}", &token[token.len() - 10..])));
    let paths = Paths::from_dirs(fixture.0.join("d"), fixture.0.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "fixture".into();
    cfg.daemon.idle_grace_secs = 3600;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let _server = Server(tokio::spawn(daemon.clone().serve()));
    for _ in 0..100 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(formless_core::daemon::daemon_reachable(&paths));
    let empty = cli(&paths, &["sessions", "templates", "list"]).await;
    success(&empty);
    assert_eq!(
        String::from_utf8(empty.stdout).unwrap(),
        "No saved Session templates.\n"
    );
    let empty = cli(&paths, &["sessions", "templates", "list", "--json"]).await;
    success(&empty);
    assert_eq!(
        serde_json::from_slice::<Value>(&empty.stdout).unwrap(),
        serde_json::json!([])
    );
    let long = format!("START ไทย\n{}\nTAIL 日本語", "x".repeat(20000));
    let expected = {
        let store = Store::open(&paths.db_file()).unwrap();
        for template in [
            TemplateInfo {
                name: "work".into(),
                model: None,
                reasoning_effort: None,
                instructions: None,
                cwd: None,
                first_message: None,
            },
            TemplateInfo {
                name: "Work".into(),
                model: Some("fixture model".into()),
                reasoning_effort: Some("".into()),
                instructions: Some(long.clone()),
                cwd: Some("/fixture project".into()),
                first_message: Some("".into()),
            },
            TemplateInfo {
                name: "ไทย".into(),
                model: Some("".into()),
                reasoning_effort: None,
                instructions: Some("".into()),
                cwd: None,
                first_message: Some("first ไทย\nsecond 日本語".into()),
            },
        ] {
            store.save_template(&template).unwrap();
        }
        serde_json::to_value(store.list_templates().unwrap()).unwrap()
    };
    let output = cli(&paths, &["sessions", "templates", "list", "--json"]).await;
    success(&output);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    let output = cli(&paths, &["sessions", "templates", "list"]).await;
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("START") && !text.contains("TAIL"));
    let positions: Vec<_> = expected
        .as_array()
        .unwrap()
        .iter()
        .map(|t| text.find(t["name"].as_str().unwrap()).unwrap())
        .collect();
    assert!(positions.windows(2).all(|p| p[0] < p[1]));
    for entry in expected.as_array().unwrap() {
        let name = entry["name"].as_str().unwrap();
        let output = cli(&paths, &["sessions", "templates", "show", name, "--json"]).await;
        success(&output);
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            *entry
        );
    }
    let output = cli(&paths, &["sessions", "templates", "show", "Work"]).await;
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&long));
    assert!(text.contains("Effort: (empty)") && text.contains("First message: (empty)"));
    let output = cli(&paths, &["sessions", "templates", "show", "work"]).await;
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Model: (unset)") && text.contains("Instructions: (unset)"));
    for name in ["WORK", "wor", "missing"] {
        let output = cli(&paths, &["sessions", "templates", "show", name]).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("sessions templates list"));
    }
    let output = cli(&paths, &["man"]).await;
    success(&output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("FORMLESS SESSIONS TEMPLATES LIST")
            && text.contains("FORMLESS SESSIONS TEMPLATES SHOW")
    );
    let after = Store::open(&paths.db_file())
        .unwrap()
        .list_templates()
        .unwrap();
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        expected,
        "readers cannot mutate templates"
    );
}
