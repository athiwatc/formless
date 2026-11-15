//! Synthetic registry and local daemon; compares stored versions without execution.
use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
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
    for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.args(args)
        .env("FORMLESS_DATA_DIR", &paths.data_dir)
        .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
        .output()
        .await
        .unwrap()
}
#[tokio::test(flavor = "multi_thread")]
async fn tool_diff_cli_preserves_daemon_reports_defaults_and_rollback_target() {
    let token = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-diff-{}", &token[token.len() - 10..])));
    let paths = Paths::from_dirs(fixture.0.join("d"), fixture.0.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "fixture".into();
    cfg.daemon.idle_grace_secs = 3600;
    cfg.save(&paths).unwrap();
    let id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        let mut id = String::new();
        for n in 1..=3 {
            let (tool, _) = store
                .create_tool_version(
                    "fixture_diff",
                    "fixture",
                    "{}",
                    "{}",
                    &format!("function run(){{return 'ไทย {n}';}}"),
                    &caps,
                    64,
                    "agent",
                )
                .unwrap();
            id = tool.id;
        }
        id
    };
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let _server = Server(tokio::spawn(daemon.clone().serve()));
    for _ in 0..100 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(formless_core::daemon::daemon_reachable(&paths));
    for (args, from, to, json) in [
        (vec!["tools", "diff", "fixture_diff"], 2, 3, false),
        (
            vec!["tools", "diff", &id, "--from", "1", "--to", "2", "--json"],
            1,
            2,
            true,
        ),
        (
            vec!["tools", "diff", "fixture_diff", "--from", "2", "--to", "2"],
            2,
            2,
            false,
        ),
    ] {
        let expected = daemon
            .handle(Request::DiffToolVersions {
                id: id.clone(),
                from,
                to,
            })
            .await
            .unwrap();
        let output = cli(&paths, &args).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if json {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                expected
            );
        } else {
            assert_eq!(output.stdout, expected["diff"].as_str().unwrap().as_bytes());
        }
    }
    for args in [
        vec!["tools", "diff", "missing-fixture"],
        vec!["tools", "diff", "fixture_diff", "--from", "99"],
        vec!["tools", "diff", "fixture_diff", "--to", "99"],
    ] {
        let output = cli(&paths, &args).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    let store = Store::open(&paths.db_file()).unwrap();
    store.rollback_tool(&id, 1).unwrap();
    let before = serde_json::to_value(store.tool_detail(&id).unwrap()).unwrap();
    let output = cli(&paths, &["tools", "diff", "fixture_diff"]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("preceding version"));
    let output = cli(
        &paths,
        &[
            "tools",
            "diff",
            "fixture_diff",
            "--from",
            "1",
            "--to",
            "3",
            "--json",
        ],
    )
    .await;
    assert!(output.status.success());
    let expected = daemon
        .handle(Request::DiffToolVersions {
            id: id.clone(),
            from: 1,
            to: 3,
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    assert_eq!(
        serde_json::to_value(store.tool_detail(&id).unwrap()).unwrap(),
        before
    );
}
