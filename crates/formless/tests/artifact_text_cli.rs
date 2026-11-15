//! Synthetic local daemon only; no provider calls or inherited credentials.
use formless_core::{daemon::Daemon, Config, Paths, Store};
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
async fn artifact_cli_reads_real_get_artifact_slices_and_preserves_envelopes_and_errors() {
    let short = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-art-{}", &short[short.len() - 10..])));
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
    let mut bytes = "ไทยe\u{301}👩‍💻\n/inside/content\n".as_bytes().to_vec();
    bytes.push(0xff);
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    let (id, empty, session) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap();
        let file = paths.data_dir.join("artifacts/fixture.txt");
        std::fs::write(&file, &bytes).unwrap();
        let id = store
            .save_artifact(
                Some(&session.id),
                &file.to_string_lossy(),
                "fixture",
                bytes.len() as i64,
            )
            .unwrap();
        let file = paths.data_dir.join("artifacts/empty.txt");
        std::fs::write(&file, []).unwrap();
        let empty = store
            .save_artifact(Some(&session.id), &file.to_string_lossy(), "", 0)
            .unwrap();
        (id, empty, session.id)
    };
    let output = cli(&paths, &["artifacts", "show", &id]).await;
    success(&output);
    assert_eq!(output.stdout, decoded.as_bytes());
    assert!(output.stderr.is_empty());
    let output = cli(
        &paths,
        &[
            "artifacts",
            "show",
            &id,
            "--offset",
            "1",
            "--length",
            "3",
            "--json",
        ],
    )
    .await;
    success(&output);
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"id":id,"session_id":session,"bytes":bytes.len(),"total_chars":decoded.chars().count(),"offset":1,"end":4,"text":decoded.chars().skip(1).take(3).collect::<String>()})
    );
    let mut joined = String::new();
    let total = decoded.chars().count();
    for offset in (0..total).step_by(3) {
        let output = cli(
            &paths,
            &[
                "artifacts",
                "show",
                &id,
                "--offset",
                &offset.to_string(),
                "--length",
                "3",
            ],
        )
        .await;
        success(&output);
        joined.push_str(&String::from_utf8(output.stdout).unwrap());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).contains("Next slice:"),
            offset + 3 < total
        );
    }
    assert_eq!(joined, decoded);
    for (artifact, offset, length) in [
        (&id, 0, 0),
        (&id, total, 3),
        (&id, total + 5, 3),
        (&empty, 0, 3),
    ] {
        let output = cli(
            &paths,
            &[
                "artifacts",
                "show",
                artifact,
                "--offset",
                &offset.to_string(),
                "--length",
                &length.to_string(),
            ],
        )
        .await;
        success(&output);
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
    let output = cli(
        &paths,
        &[
            "artifacts",
            "show",
            &id,
            "--offset",
            &(total + 5).to_string(),
            "--json",
        ],
    )
    .await;
    success(&output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["offset"], total + 5);
    assert_eq!(value["end"], total + 5);
    let output = cli(&paths, &["artifacts", "show", "missing-fixture"]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no artifact"));
    for flag in [
        "--offset=-1",
        "--length=-1",
        "--offset=184467440737095516160000",
    ] {
        let output = cli(&paths, &["artifacts", "show", &id, flag]).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
