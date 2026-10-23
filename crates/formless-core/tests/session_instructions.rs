//! Standing instructions per Session (SPEC §5.1).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn instructions_join_the_session_prompt_and_follow_forks() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-instr-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-instr-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let other = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let s = daemon
        .handle(Request::SetSessionInstructions {
            id: sid.clone(),
            instructions: Some("  Answer in Thai.  ".into()),
        })
        .await
        .unwrap();
    assert_eq!(s["instructions"], "Answer in Thai.");
    let prompt = store.session_system_prompt(&sid).unwrap().unwrap();
    assert!(prompt.ends_with("from the user:\nAnswer in Thai."), "{prompt}");
    // Other Sessions keep the shared prompt.
    daemon
        .handle(Request::ContextUsage {
            session_id: other.clone(),
        })
        .await
        .unwrap();
    let shared = store.session_system_prompt(&other).unwrap().unwrap();
    assert!(!shared.contains("Answer in Thai"));
    assert!(prompt.starts_with(&shared), "instructions come after the shared prefix");

    let fork = daemon
        .handle(Request::ForkSession {
            id: sid.clone(),
            name: None,
            up_to_seq: None,
        })
        .await
        .unwrap();
    assert_eq!(fork["instructions"], "Answer in Thai.");

    let s = daemon
        .handle(Request::SetSessionInstructions {
            id: sid.clone(),
            instructions: Some("   ".into()),
        })
        .await
        .unwrap();
    assert!(s["instructions"].is_null());
    let prompt = store.session_system_prompt(&sid).unwrap().unwrap();
    assert!(!prompt.contains("Answer in Thai"));
    let _ = std::fs::remove_dir_all(&dir);
}
