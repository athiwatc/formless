//! Session templates (SPEC §5.1).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn a_session_saved_as_a_template_sets_up_new_ones() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-tpl-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-tpl-{short}"),
    };
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    store
        .set_session_model(&sid, Some("big-model".into()), Some("high".into()))
        .unwrap();
    store
        .set_session_instructions(&sid, Some("Be terse."))
        .unwrap();
    store
        .set_session_cwd(&sid, Some(project.to_str().unwrap()))
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let t = daemon
        .handle(Request::SaveSessionTemplate {
            id: sid.clone(),
            name: "triage".into(),
            first_message: Some("Triage today's inbox".into()),
        })
        .await
        .unwrap();
    assert_eq!(t["model"], "big-model");
    let list = daemon.handle(Request::ListSessionTemplates).await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let r = daemon
        .handle(Request::CreateSessionFromTemplate {
            template: "Triage".into(),
            name: None,
        })
        .await
        .unwrap();
    let s = &r["session"];
    assert_ne!(s["id"], sid.as_str());
    assert_eq!(s["name"], "triage");
    assert_eq!(s["model"], "big-model");
    assert_eq!(s["reasoning_effort"], "high");
    assert_eq!(s["instructions"], "Be terse.");
    assert_eq!(s["cwd"], project.to_str().unwrap());
    assert_eq!(r["first_message"], "Triage today's inbox");

    assert!(daemon
        .handle(Request::CreateSessionFromTemplate {
            template: "nope".into(),
            name: None,
        })
        .await
        .is_err());
    daemon
        .handle(Request::DeleteSessionTemplate {
            name: "triage".into(),
        })
        .await
        .unwrap();
    let list = daemon.handle(Request::ListSessionTemplates).await.unwrap();
    assert!(list.as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
