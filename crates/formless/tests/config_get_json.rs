//! Local effective configuration only; no daemon, provider or model request.
use formless_core::{Config, Paths};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Output, time::Duration};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn get(paths: &Paths, key: &str, json: bool, model_override: Option<&str>) -> Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_formless"));
    command.env_clear();
    for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.args(["config", "get", key]);
    if json {
        command.arg("--json");
    }
    command
        .env("FORMLESS_DATA_DIR", &paths.data_dir)
        .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
        .kill_on_drop(true);
    if let Some(model) = model_override {
        command.env("FORMLESS_MODEL", model);
    }
    tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn config_get_json_encodes_local_values_and_keeps_human_errors_and_overrides() {
    let fixture = Fixture(std::env::temp_dir().join(format!(
            "formless-config-json-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    let paths = Paths::from_dirs(fixture.0.join("data"), fixture.0.join("config")).unwrap();
    let mut config = Config::default();
    for text in ["true", "ไทย \"quoted\"\nnext", "{\"key\":true}"] {
        config.provider.model = text.into();
        config.save(&paths).unwrap();
        let encoded = get(&paths, "provider.model", true, None).await;
        assert!(
            encoded.status.success(),
            "{}",
            String::from_utf8_lossy(&encoded.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded.stdout).unwrap(),
            json!(text)
        );
        assert_eq!(
            String::from_utf8(encoded.stdout).unwrap(),
            format!("{}\n", serde_json::to_string(text).unwrap())
        );
        let human = get(&paths, "provider.model", false, None).await;
        assert!(human.status.success());
        assert_eq!(human.stdout, format!("{text}\n").as_bytes());
    }
    let boolean = get(&paths, "naming.enabled", true, None).await;
    assert!(boolean.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&boolean.stdout).unwrap(),
        json!(config.naming.enabled)
    );
    let overridden = get(&paths, "provider.model", true, Some("local-env-fixture")).await;
    assert!(overridden.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&overridden.stdout).unwrap(),
        json!("local-env-fixture")
    );
    let human_error = get(&paths, "not_a_setting", false, None).await;
    let json_error = get(&paths, "not_a_setting", true, None).await;
    assert_eq!(human_error.status.code(), Some(2));
    assert_eq!(json_error.status.code(), human_error.status.code());
    assert!(human_error.stdout.is_empty() && json_error.stdout.is_empty());
    assert_eq!(human_error.stderr, json_error.stderr);
    assert!(!paths.data_dir.join("daemon.pid").exists());
}
