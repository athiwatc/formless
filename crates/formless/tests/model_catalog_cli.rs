//! Local HTTP model listing + synthetic daemon; never runs a model probe.
#[path = "../../formless-core/tests/support/env.rs"]
mod credential_env;
use formless_core::{daemon::Daemon, Config, Paths, Store};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::Output,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn cli(paths: &Paths, args: &[&str]) -> Output {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_formless"));
    cmd.env_clear();
    for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
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
#[tokio::test(flavor = "multi_thread")]
async fn models_cli_reports_listing_and_partial_failure_without_testing_or_selecting_models() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_IDLE_GRACE_SECS");
    env.remove("FORMLESS_SECURE");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    env.set("FORMLESS_BASE_URL", format!("http://{address}/v1"));
    env.set("FORMLESS_MODEL", "friendly");
    let mode = Arc::new(AtomicU8::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let http = tokio::spawn({
        let mode = mode.clone();
        let requests = requests.clone();
        async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                stream.read_line(&mut request).await.unwrap();
                requests.lock().unwrap().push(request.trim().to_string());
                loop {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let status = if mode.load(Ordering::SeqCst) == 1 {
                    "403 Forbidden"
                } else {
                    "200 OK"
                };
                let body=match mode.load(Ordering::SeqCst){1=>serde_json::json!({"error":{"message":"fixture listing denied"}}),2=>serde_json::json!({"data":[]}),_=>serde_json::json!({"data":[{"id":"fixture-ไทย","context_window":1234},{"id":"fixture-a"}]})}.to_string();
                let response=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        }
    });
    let _http = Task(http);
    let token = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-models-{}", &token[token.len() - 10..])));
    let paths = Paths::from_dirs(fixture.0.join("d"), fixture.0.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{address}/v1");
    cfg.provider.model = "friendly".into();
    cfg.provider
        .aliases
        .insert("friendly".into(), "fixture-a".into());
    cfg.daemon.idle_grace_secs = 3600;
    cfg.save(&paths).unwrap();
    let config_before = std::fs::read(paths.config_file()).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let task = tokio::spawn({
        let d = daemon.clone();
        async move {
            d.serve().await.unwrap();
        }
    });
    let _daemon = Task(task);
    for _ in 0..100 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(formless_core::daemon::daemon_reachable(&paths));
    let session = {
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap();
        store
            .set_session_model(&session.id, Some("recent-only".into()), None)
            .unwrap();
        session.id
    };
    let output = cli(&paths, &["models", "list", "--json"]).await;
    assert!(
        output.status.success(),
        "{}",
        format!(
            "stderr={} stdout={}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        )
    );
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["models"],
        serde_json::json!(["fixture-ไทย", "fixture-a"])
    );
    assert_eq!(value["current"]["model"], "friendly");
    assert_eq!(value["recent"], serde_json::json!(["recent-only"]));
    assert_eq!(value["aliases"]["friendly"], "fixture-a");
    assert!(value["error"].is_null());
    let output = cli(&paths, &["models", "list"]).await;
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.find("  fixture-ไทย\n").unwrap() < text.find("  fixture-a\n").unwrap());
    assert!(text.contains("recent-only") && text.contains("friendly"));
    mode.store(1, Ordering::SeqCst);
    for json in [false, true] {
        let args = if json {
            vec!["models", "list", "--json"]
        } else {
            vec!["models", "list"]
        };
        let output = cli(&paths, &args).await;
        assert_eq!(output.status.code(), Some(1));
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if json {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(!value["error"].is_null());
            assert_eq!(value["models"], serde_json::json!([]));
            assert_eq!(value["recent"], serde_json::json!(["recent-only"]));
        } else {
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("partial report") && text.contains("recent-only"));
        }
    }
    mode.store(2, Ordering::SeqCst);
    let output = cli(&paths, &["models", "list"]).await;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("endpoint listed no models"));
    assert_eq!(std::fs::read(paths.config_file()).unwrap(), config_before);
    assert_eq!(
        Store::open(&paths.db_file())
            .unwrap()
            .get_session(&session)
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("recent-only")
    );
    let recorded = requests.lock().unwrap().clone();
    assert!(
        recorded
            .iter()
            .all(|request| request == "GET /v1/models HTTP/1.1"),
        "only listing calls: {recorded:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn azure_provider_add_migrates_from_a_dated_route_to_v1() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_MODEL");
    env.remove("FORMLESS_IDLE_GRACE_SECS");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(std::env::temp_dir().join(format!("fl-azure-{nonce}")));
    let paths = Paths::from_dirs(fixture.0.join("d"), fixture.0.join("c")).unwrap();
    let prefix = ["providers", "add", "azure", "--no-key", "--azure-resource"];
    let bad = cli(
        &paths,
        &[
            prefix.as_slice(),
            &[
                "http://contoso.openai.azure.com",
                "--azure-deployment",
                "model",
            ],
        ]
        .concat(),
    )
    .await;
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("HTTPS endpoint"));
    assert!(!paths.db_file().exists());

    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.daemon.idle_grace_secs = 3600;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let _serve = Task(tokio::spawn(async move { daemon.serve().await.unwrap() }));
    for _ in 0..100 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(formless_core::daemon::daemon_reachable(&paths));

    let common = [
        prefix.as_slice(),
        &[
            "contoso.openai.azure.com",
            "--azure-deployment",
            "fixture-model",
        ],
    ]
    .concat();
    let dated = cli(
        &paths,
        &[common.as_slice(), &["--api-version", "2024-10-21"]].concat(),
    )
    .await;
    assert!(
        dated.status.success(),
        "{}",
        String::from_utf8_lossy(&dated.stderr)
    );
    let cfg = Config::load_file_only(&paths).unwrap();
    let provider = &cfg.providers["azure"];
    assert_eq!(
        provider.base_url,
        "https://contoso.openai.azure.com/openai/deployments/fixture-model"
    );
    assert_eq!(provider.query_params["api-version"], "2024-10-21");

    let v1 = cli(&paths, &common).await;
    assert!(
        v1.status.success(),
        "{}",
        String::from_utf8_lossy(&v1.stderr)
    );
    let cfg = Config::load_file_only(&paths).unwrap();
    let provider = &cfg.providers["azure"];
    assert_eq!(
        provider.base_url,
        "https://contoso.openai.azure.com/openai/v1"
    );
    assert_eq!(provider.model, "fixture-model");
    assert_eq!(provider.api_key_header.as_deref(), Some("api-key"));
    assert!(provider.query_params.is_empty());
}
