//! Synthetic metadata/handler checks; no Skill files, providers or Tools run.
use super::*;

#[tokio::test]
async fn scoped_session_search_handler_preserves_legacy_results_and_reports_empty_cutoff() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-scoped-search-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    daemon
        .with_store(|store| {
            let session = store.create_session(None)?;
            store.append_message(&session.id, "user", "needle ??")?;
            Ok(())
        })
        .unwrap();
    let legacy = daemon
        .handle(Request::SearchSessions {
            query: "??".into(),
            limit: Some(1),
        })
        .await
        .unwrap();
    let scoped = daemon
        .handle(Request::SearchSessionsWithScope {
            query: "??".into(),
            limit: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(legacy.as_array().unwrap().len(), 1);
    assert_eq!(scoped, json!({"hits":legacy,"searched_latest":10000}));
    let empty = daemon
        .handle(Request::SearchSessionsWithScope {
            query: "!".into(),
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(empty, json!({"hits":[],"searched_latest":10000}));
    let indexed = daemon
        .handle(Request::SearchSessionsWithScope {
            query: "needle".into(),
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(indexed, json!({"hits":legacy}));
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn summary_search_handler_ranks_before_limits_preserving_other_fields_and_eligibility() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-summary-search-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
    cfg.provider.model = "unused-private-fixture".into();
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let session = daemon
        .with_store(|store| {
            let session = store.create_session(Some("private search fixture".into()))?;
            store.create_tool_version(
                "fixture_search",
                "database backup",
                "{}",
                "{}",
                "function run(input) { return input; }",
                &Capabilities::default(),
                64,
                "user",
            )?;
            Ok(session.id)
        })
        .unwrap();
    let args = json!({"query":"database backup","limit":1});
    // Warm only the existing Tool-loading/pin path before comparing responses.
    daemon.find_tools_impl(&session, &args).unwrap();
    let mut baseline: Value =
        serde_json::from_str(&daemon.find_tools_impl(&session, &args).unwrap()).unwrap();
    assert!(!baseline["tools"].as_array().unwrap().is_empty());
    daemon
        .with_store(|store| {
            for (name, description, enabled) in [
                ("a-generic", "database", true),
                ("z-complete", "database backup ไทย", true),
                ("c-db", "db helper", true),
                ("b-substring", "adbc", true),
                ("aa-disabled", "database backup", false),
                ("ab-retired", "database backup", true),
            ] {
                if enabled {
                    store.create_module_version(
                        name,
                        description,
                        "module.exports={fixture:true};",
                        "user",
                    )?;
                }
                store.add_skill_version(
                    name,
                    description,
                    1,
                    root.join("not-read").to_str().unwrap(),
                    "fixture",
                    "user",
                    false,
                )?;
                store.set_skill_enabled(name, enabled)?;
            }
            Ok(())
        })
        .unwrap();
    let conn = rusqlite::Connection::open(daemon.paths.db_file()).unwrap();
    conn.execute("UPDATE modules SET retired=1 WHERE name='ab-retired'", [])
        .unwrap();
    conn.execute("UPDATE skills SET retired=1 WHERE name='ab-retired'", [])
        .unwrap();
    let before = daemon
        .with_store(|store| {
            Ok(json!({"modules":store.list_modules()?,"skills":store.list_skills()?}))
        })
        .unwrap();
    let mut actual: Value =
        serde_json::from_str(&daemon.find_tools_impl(&session, &args).unwrap()).unwrap();
    assert_eq!(
        actual["modules"],
        json!([{"name":"z-complete","description":"database backup ไทย","version":1}])
    );
    assert_eq!(
        actual["skills"],
        json!([{"name":"z-complete","description":"database backup ไทย","load_with":"load_skill"}])
    );
    for report in [&mut baseline, &mut actual] {
        report["modules"] = Value::Null;
        report["skills"] = Value::Null;
    }
    assert_eq!(
        actual, baseline,
        "Tool hits, wrappers, duplicates, upgrades, suggestions and note retain their paths"
    );
    for query in ["db", "DB"] {
        let report: Value = serde_json::from_str(
            &daemon
                .find_tools_impl(&session, &json!({"query":query}))
                .unwrap(),
        )
        .unwrap();
        for kind in ["modules", "skills"] {
            assert_eq!(
                report[kind]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["name"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["c-db"]
            );
        }
    }
    for query in ["", "--- !!!"] {
        let report: Value = serde_json::from_str(
            &daemon
                .find_tools_impl(&session, &json!({"query":query}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(report["modules"], json!([]));
        assert_eq!(report["skills"], json!([]));
    }
    assert_eq!(
        daemon
            .with_store(|store| Ok(
                json!({"modules":store.list_modules()?,"skills":store.list_skills()?})
            ))
            .unwrap(),
        before
    );
    drop(conn);
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
