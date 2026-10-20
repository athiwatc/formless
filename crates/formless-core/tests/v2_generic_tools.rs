//! v2.1 generic Tools (SPEC §7.4/§7.7/§8.1): input-bound Capability
//! scopes give one generic Tool exactly the paths it is called with, and
//! session spread flags Tools that look project-specific.

use formless_core::daemon::Daemon;
use formless_core::optimiser::{is_narrow, prompt_eval_cases};
use formless_core::provider::SYSTEM_PROMPT;
use formless_core::storage::Store;
use formless_core::{Config, Paths};
use formless_runtime::Capabilities;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

fn seed_reader(paths: &Paths) {
    let store = Store::open(&paths.db_file()).unwrap();
    let schema = r#"{"type":"object","properties":{"path":{"type":"string"},"also":{"type":"string"}},"required":["path"]}"#;
    store
        .create_tool_version(
            "read_text_file",
            "Read a UTF-8 text file",
            schema,
            r#"{"type":"string"}"#,
            "function run(i){ return fs.read(i.also || i.path); }",
            &Capabilities::parse(&["fs:read:$input.path".into()]).unwrap(),
            256,
            "agent",
        )
        .unwrap();
}

async fn call(daemon: &Arc<Daemon>, session: &str, input: Value) -> Result<Value, String> {
    daemon
        .clone()
        .execute_tool_value(
            session.into(),
            "run".into(),
            "read_text_file".into(),
            input,
            0,
            None,
            Arc::new(AtomicBool::new(false)),
            None,
            CancellationToken::new(),
            Config::default(),
            true,
        )
        .await
        .map(|(v, _)| v)
}

#[tokio::test]
async fn input_bound_scope_grants_exactly_the_called_path() {
    let paths = temp_paths("gen");
    seed_reader(&paths);
    let dir = paths.data_dir.join("files");
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
    std::fs::write(&a, "alpha").unwrap();
    std::fs::write(&b, "beta").unwrap();
    let (a, b) = (a.to_str().unwrap(), b.to_str().unwrap());

    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let s1 = Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;

    // The same generic Tool reads whichever file it is called with.
    assert_eq!(
        call(&daemon, &s1, json!({"path": a})).await.unwrap(),
        "alpha"
    );
    assert_eq!(
        call(&daemon, &s1, json!({"path": b})).await.unwrap(),
        "beta"
    );

    // ...but nothing outside what the bound field names.
    let err = call(&daemon, &s1, json!({"path": a, "also": b}))
        .await
        .unwrap_err();
    assert!(err.contains("outside granted roots"), "{err}");

    // Spread: three calls, one Session -> flagged as possibly over-specific.
    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _) = store.get_tool_by_name("read_text_file").unwrap().unwrap();
    let m = store.tool_metrics(&info.id, None).unwrap();
    assert_eq!((m.total, m.sessions), (3, 1));
    assert!(is_narrow(&m));

    // Used from a second Session: no longer flagged.
    let s2 = store.create_session(None).unwrap().id;
    call(&daemon, &s2, json!({"path": a})).await.unwrap();
    let m = store.tool_metrics(&info.id, None).unwrap();
    assert_eq!(m.sessions, 2);
    assert!(!is_narrow(&m));
}

#[tokio::test]
async fn unbindable_input_is_a_validation_failure() {
    let paths = temp_paths("genv");
    seed_reader(&paths);
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let s = Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    // Schema-valid (path is a string) but empty, so it cannot bind.
    let err = call(&daemon, &s, json!({"path": ""})).await.unwrap_err();
    assert!(err.contains("input.path"), "{err}");
    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _) = store.get_tool_by_name("read_text_file").unwrap().unwrap();
    assert_eq!(
        store
            .tool_metrics(&info.id, None)
            .unwrap()
            .validation_failures,
        1
    );
}

#[test]
fn prompt_and_evals_push_generic_tools() {
    assert!(SYSTEM_PROMPT.contains("Make Tools generic"));
    assert!(SYSTEM_PROMPT.contains("fs:read:$input.path"));
    assert!(prompt_eval_cases()
        .iter()
        .any(|c| c.name == "generic_tools"));
}
