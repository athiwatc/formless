//! Actual agent save/read surfaces: private profile and owned HTTP tripwire only.
use super::*;

#[tokio::test]
async fn schema_save_flag_is_required_despite_forged_author_and_reports_reach_reads() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.remove("FORMLESS_SECURE");
    env.set("FORMLESS_API_KEY", "private-unused-schema-fixture");
    let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    http.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", http.local_addr().unwrap());
    let root = std::env::temp_dir().join(format!("formless-schema-save-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = endpoint.clone();
    cfg.provider.model = "private-no-provider".into();
    cfg.provider.auto_test_models = false;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.tools.approval_summaries = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let cfg = daemon.config().unwrap();
    assert_eq!(
        cfg.provider.base_url, endpoint,
        "fixture must use only its owned endpoint"
    );
    let session = daemon.with_store(|st| st.create_session(None)).unwrap().id;
    let old_input = json!({"type":"object","properties":{"x":{"type":"string"}},"required":["x"],"additionalProperties":false});
    let output = json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false});
    let code = "function run(i){return {value:i.x};}";
    let tool = daemon
        .with_store(|st| {
            st.create_tool_version(
                "schema_target",
                "private target",
                &old_input.to_string(),
                &output.to_string(),
                code,
                &Capabilities::default(),
                64,
                "agent",
            )
        })
        .unwrap()
        .0;
    let caller = daemon
        .with_store(|st| {
            st.create_tool_version(
                "schema_caller",
                "private caller",
                "{}",
                "{}",
                "async function run(i){return await tools.call('schema_target',i);}",
                &Capabilities::default(),
                64,
                "agent",
            )
        })
        .unwrap()
        .0;
    let mut args = json!({"name":"schema_target","description":"private target",
        "input_schema":{"type":"object","properties":{"x":{"type":"string"},"extra":{"type":"string"}},"required":["x","extra"],"additionalProperties":false},
        "output_schema":output,"implementation_js":code,"capabilities":[],"memory_mb":64,
        "author":"user","actor":"user"});
    let cancel = Arc::new(AtomicBool::new(false));
    let error = daemon
        .create_tool_impl(&args, &cfg, &cancel)
        .await
        .unwrap_err();
    let blocked = error
        .downcast_ref::<crate::tool_compatibility::SchemaChangeBlocked>()
        .unwrap();
    assert!(blocked.0.compatibility.is_breaking());
    assert!(blocked
        .0
        .affected_dependents
        .iter()
        .any(|d| d.id == caller.id && d.live));
    assert_eq!(
        daemon
            .with_store(|st| st.tool_detail(&tool.id))
            .unwrap()
            .unwrap()
            .versions
            .len(),
        1
    );
    assert_eq!(
        http.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    args["allow_breaking"] = json!("true");
    assert!(daemon
        .create_tool_impl(&args, &cfg, &cancel)
        .await
        .unwrap_err()
        .to_string()
        .contains("must be a boolean"));
    args["allow_breaking"] = json!(true);
    let (saved, id) = daemon.create_tool_impl(&args, &cfg, &cancel).await.unwrap();
    assert_eq!(id, tool.id);
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["schema_compatibility"]["base_version"], 1);
    assert_eq!(
        saved["schema_compatibility"]["compatibility"]["verdict"],
        "breaking"
    );
    let read: Value = serde_json::from_str(
        &daemon
            .read_tool_impl(&session, &json!({"name":"schema_target","diff_from":1}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read["schema_compatibility"]["base_version"], 1);
    assert_eq!(
        read["schema_compatibility"]["compatibility"]["verdict"],
        "breaking"
    );
    assert!(read["diff"]
        .as_str()
        .unwrap()
        .contains("schema_compatibility"));
    let detail = daemon
        .handle_inner(Request::GetTool { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(
        detail["schema_compatibility"]["compatibility"]["verdict"],
        "breaking"
    );
    let diff = daemon
        .handle_inner(Request::DiffToolVersions { id, from: 1, to: 2 })
        .await
        .unwrap();
    assert_eq!(
        diff["schema_compatibility"]["compatibility"]["verdict"],
        "breaking"
    );
    assert!(diff["diff"]
        .as_str()
        .unwrap()
        .contains("schema_compatibility"));
    assert_eq!(
        http.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "save/read/diff must not contact a provider"
    );
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
