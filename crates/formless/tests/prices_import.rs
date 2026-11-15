//! Isolated localhost listing; no daemon, model probe or real provider request.
use formless_core::{config::ModelPrice, Config, Paths};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Output,
    sync::{Arc, Mutex},
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
        .kill_on_drop(true);
    tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output())
        .await
        .unwrap()
        .unwrap()
}
fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn import_reports_cache_only_changes_and_preserves_dry_run_and_selection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let row = |id: &str, write: Option<&str>| {
        json!({"id":id,"pricing":{
            "prompt":"0.000001","completion":"0.000002","input_cache_read":"0.00000025","input_cache_write":write
        }})
    };
    let body = Arc::new(Mutex::new(
        json!({"data":[
            row("write-only",Some("0.00000075")),row("remove-ไทย",None),
            row("add",Some("0.0000005")),row("same",Some("0.0000005")),
            row("unselected",Some("0.0000005"))
        ]})
        .to_string(),
    ));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let _http = Task(tokio::spawn({
        let body = body.clone();
        let requests = requests.clone();
        async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut request = String::new();
                stream.read_line(&mut request).await.unwrap();
                requests.lock().unwrap().push(request.trim().to_owned());
                loop {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let body = body.lock().unwrap().clone();
                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                stream
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        }
    }));
    let fixture = Fixture(std::env::temp_dir().join(format!(
            "fl-price-import-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    let paths = Paths::from_dirs(fixture.0.join("d"), fixture.0.join("c")).unwrap();
    let old = ModelPrice {
        input: 1.0,
        output: 2.0,
        cached_input: Some(0.25),
        cache_write_input: Some(0.5),
        retrieved_at: Some("old timestamp".into()),
    };
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{addr}/v1");
    cfg.provider.model = "write-only".into();
    cfg.provider.fallback_models = vec!["remove-ไทย".into(), "add".into(), "same".into()];
    cfg.provider.prices =
        BTreeMap::from_iter(["write-only", "remove-ไทย", "add", "same"].map(|id| {
            let mut price = old.clone();
            if id == "add" {
                price.cache_write_input = None;
            }
            (id.into(), price)
        }))
        .into_iter()
        .collect();
    cfg.save(&paths).unwrap();
    let before = std::fs::read(paths.config_file()).unwrap();
    let dry = stdout(cli(&paths, &["prices", "import", "--dry-run"]).await);
    assert!(dry.contains("write-only"), "{dry}");
    assert!(dry.contains("cache-write input $0.5 -> $0.75"), "{dry}");
    assert!(
        dry.contains("cache-write input $0.5 -> unset (uses input $1)"),
        "{dry}"
    );
    assert!(
        dry.contains("cache-write input unset (uses input $1) -> $0.5"),
        "{dry}"
    );
    assert!(dry.contains("cached input $0.25 -> $0.25"));
    assert!(dry.contains("input $1 -> $1") && dry.contains("output $2 -> $2"));
    assert!(!dry.contains("same:") && !dry.contains("unselected"));
    assert!(dry.contains("dry run: nothing written"));
    assert_eq!(std::fs::read(paths.config_file()).unwrap(), before);
    let real = stdout(cli(&paths, &["prices", "import"]).await);
    let mut dry_lines: Vec<_> = dry
        .lines()
        .filter(|line| !line.starts_with("(dry run:"))
        .collect();
    let mut real_lines: Vec<_> = real
        .lines()
        .filter(|line| !line.starts_with("saved "))
        .collect();
    dry_lines.sort();
    real_lines.sort();
    assert_eq!(dry_lines, real_lines);
    assert!(real.contains("saved 3 price(s)"));
    let saved = Config::load_file_only(&paths).unwrap();
    assert_eq!(
        saved.provider.prices["write-only"].cache_write_input,
        Some(0.75)
    );
    assert_eq!(saved.provider.prices["remove-ไทย"].cache_write_input, None);
    assert_eq!(saved.provider.prices["add"].cache_write_input, Some(0.5));
    assert_ne!(
        saved.provider.prices["write-only"].retrieved_at,
        old.retrieved_at
    );
    assert_eq!(saved.provider.prices["same"], old);
    assert!(!saved.provider.prices.contains_key("unselected"));
    let unchanged = std::fs::read(paths.config_file()).unwrap();
    assert!(stdout(cli(&paths, &["prices", "import"]).await).contains("up to date"));
    assert_eq!(std::fs::read(paths.config_file()).unwrap(), unchanged);
    assert!(
        stdout(cli(&paths, &["prices", "import", "--all", "--dry-run"]).await)
            .contains("unselected")
    );
    assert_eq!(std::fs::read(paths.config_file()).unwrap(), unchanged);
    *body.lock().unwrap() = json!({"data":[]}).to_string();
    let empty = cli(&paths, &["prices", "import"]).await;
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("listing has no prices"));
    assert_eq!(std::fs::read(paths.config_file()).unwrap(), unchanged);
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r == "GET /v1/models HTTP/1.1"));
}
