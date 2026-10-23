//! What a Proposal changes, before it's applied (SPEC §10.4).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn proposals_show_the_change_to_guidance_or_memory() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-pdiff-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-pdiff-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let old = store
        .create_proposal("prompt", "Prefer existing Tools.", "fewer duplicates", 10)
        .unwrap();
    store.update_proposal_eval(&old.id, 0.5, 0.9, 20).unwrap();
    store.set_proposal_status(&old.id, "applied").unwrap();
    let new = store
        .create_proposal(
            "prompt",
            "Check the output schema.",
            "fewer invalid outputs",
            12,
        )
        .unwrap();
    let mem = store
        .create_proposal("memory", "The user writes in Thai.", "seen twice", 5)
        .unwrap();
    store
        .add_memory("Dates are DD/MM.", "global", "user", None)
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let d = daemon
        .handle(Request::ProposalDiff { id: new.id.clone() })
        .await
        .unwrap();
    assert_eq!(d["target"], "learned guidance");
    assert_eq!(d["applied"], false);
    let lines: Vec<(String, String)> = d["diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l[0].as_str().unwrap().into(), l[1].as_str().unwrap().into()))
        .collect();
    assert_eq!(
        lines,
        [
            (
                " ".to_string(),
                "Learned guidance (v1): Prefer existing Tools.".to_string()
            ),
            (
                "+".to_string(),
                "Learned guidance (v2): Check the output schema.".to_string()
            ),
        ]
    );
    // An applied one shows what it added.
    let d = daemon
        .handle(Request::ProposalDiff { id: old.id })
        .await
        .unwrap();
    assert_eq!(d["applied"], true);
    assert_eq!(d["diff"][0][0], "+");

    let d = daemon
        .handle(Request::ProposalDiff { id: mem.id })
        .await
        .unwrap();
    assert_eq!(d["target"], "memory");
    assert_eq!(d["diff"][0], serde_json::json!([" ", "- Dates are DD/MM."]));
    assert_eq!(
        d["diff"][1],
        serde_json::json!(["+", "- The user writes in Thai."])
    );
    let _ = std::fs::remove_dir_all(&dir);
}
