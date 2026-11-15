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
    let (saved, change) = daemon.create_tool_impl(&args, &cfg, &cancel).await.unwrap();
    let id = change.tool_id;
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

#[tokio::test]
async fn tool_example_inputs_are_bounded_previews_on_detail_export_and_read_surfaces() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-example-cap-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut config = Config::default();
    config.provider.base_url = "http://127.0.0.1:9/v1".into();
    config.provider.model = "private-unused".into();
    config.naming.enabled = false;
    config.save(&paths).unwrap();
    let daemon = Daemon::new(paths).unwrap();
    let (session, tool) = daemon
        .with_store(|st| {
            let session = st.create_session(None)?.id;
            let (tool, _) = st.create_tool_version(
                "example_target",
                "private fixture",
                "{}",
                "{}",
                "function run(i){return i}",
                &Capabilities::default(),
                64,
                "agent",
            )?;
            assert!(st.tool_example(&tool.id)?.is_none());
            Ok((session, tool.id))
        })
        .unwrap();
    let exact = serde_json::to_string(&json!({"x":"a".repeat(3992)})).unwrap();
    assert_eq!(exact.len(), 4000);
    let multibyte = format!("{{\"x\":\"{}日\"}}", "a".repeat(3993));
    let clipped_multibyte = format!("{{\"x\":\"{}… (cut)", "a".repeat(3993));
    for (input, output, expected_input, expected_output) in [
        (
            r#"{"text":"ไทย"}"#.to_owned(),
            "{}".to_owned(),
            r#"{"text":"ไทย"}"#.to_owned(),
            "{}".to_owned(),
        ),
        (exact.clone(), "{}".into(), exact, "{}".into()),
        (
            "a".repeat(4001),
            "b".repeat(4001),
            format!("{}… (cut)", "a".repeat(4000)),
            format!("{}… (cut)", "b".repeat(4000)),
        ),
        (
            multibyte,
            "日".repeat(1340),
            clipped_multibyte,
            format!("{}… (cut)", "日".repeat(1333)),
        ),
    ] {
        let id = daemon
            .with_store(|st| {
                st.record_invocation(
                    &tool, 1, None, None, None, "success", "fixture", &input, &output, "", 1, 100,
                )
            })
            .unwrap();
        let conn = rusqlite::Connection::open(daemon.paths.db_file()).unwrap();
        conn.execute(
            "UPDATE invocations SET created_at='2026-01-01T00:00:00Z' WHERE tool_id=?1",
            [&tool],
        )
        .unwrap();
        // A later failure with the same timestamp cannot replace success.
        daemon
            .with_store(|st| {
                st.record_invocation(
                    &tool, 1, None, None, None, "failure", "fixture", "wrong", "wrong", "fixture",
                    1, 100,
                )
            })
            .unwrap();
        conn.execute(
            "UPDATE invocations SET created_at='2026-01-01T00:00:00Z' WHERE tool_id=?1",
            [&tool],
        )
        .unwrap();
        let example = daemon
            .with_reader(|st| st.tool_example(&tool))
            .unwrap()
            .unwrap();
        assert_eq!(example.input, expected_input);
        assert_eq!(example.output, expected_output);
        assert_eq!(example.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(example.version, 1);
        for field in [&example.input, &example.output] {
            assert!(field.len() <= 4000 + "… (cut)".len());
            if field.ends_with("… (cut)") {
                assert_eq!(field.matches("… (cut)").count(), 1);
            }
        }
        let detail = daemon
            .with_reader(|st| st.tool_detail(&tool))
            .unwrap()
            .unwrap();
        assert_eq!(detail.example.unwrap(), example);
        let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap_or(json!(text));
        let bundle = daemon.export_tool_bundle(&tool).unwrap().unwrap();
        let read: Value = serde_json::from_str(
            &daemon
                .read_tool_impl(&session, &json!({"name":"example_target"}))
                .unwrap(),
        )
        .unwrap();
        for value in [&bundle, &read] {
            assert_eq!(value["example"]["input"], parse(&expected_input));
            assert_eq!(value["example"]["output"], parse(&expected_output));
        }
        let stored: (String, String) = conn
            .query_row(
                "SELECT input,output FROM invocations WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            stored,
            (input, output),
            "presentation must not rewrite recorded calls"
        );
    }
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn read_tool_diff_from_names_the_invalid_argument_without_executing_tools() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-read-version-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut config = Config::default();
    config.provider.base_url = "http://127.0.0.1:9/v1".into();
    config.provider.model = "private-unused".into();
    config.naming.enabled = false;
    config.save(&paths).unwrap();
    let daemon = Daemon::new(paths).unwrap();
    let name = "เครื่องมือ_日本語";
    let (session, id) = daemon
        .with_store(|st| {
            let session = st.create_session(None)?.id;
            let mut id = String::new();
            for code in ["function run(){return 1}", "function run(){return 2}"] {
                id = st
                    .create_tool_version(
                        name,
                        "private fixture",
                        "{}",
                        "{}",
                        code,
                        &Capabilities::default(),
                        64,
                        "agent",
                    )?
                    .0
                    .id;
            }
            Ok((session, id))
        })
        .unwrap();
    let missing = daemon
        .read_tool_impl(&session, &json!({"name":name,"diff_from":999}))
        .unwrap_err();
    assert_eq!(
        missing.to_string(),
        format!("diff_from: '{name}' has no version 999")
    );
    assert!(missing.downcast_ref::<RequestFailure>().is_some());
    let read = |args: Value| -> Value {
        serde_json::from_str(&daemon.read_tool_impl(&session, &args).unwrap()).unwrap()
    };
    let omitted = read(json!({"name":name}));
    assert_eq!(omitted["version"], 2);
    assert!(omitted["diff"].is_null());
    let expected = daemon
        .with_reader(|st| st.tool_version_schema_report(&id, 2))
        .unwrap();
    assert_eq!(
        omitted["schema_compatibility"],
        serde_json::to_value(expected).unwrap()
    );
    let detail = daemon
        .with_store(|st| st.tool_detail(&id))
        .unwrap()
        .unwrap();
    for from in [1, 2] {
        let result = read(json!({"name":name,"diff_from":from}));
        assert_eq!(result["name"], name);
        assert_eq!(
            result["diff"],
            crate::diff::version_diff(&detail.versions[(from - 1) as usize], &detail.versions[1])
        );
        let expected = daemon
            .with_reader(|st| {
                st.schema_change_report_from_version(
                    &id,
                    from,
                    &detail.versions[1].input_schema,
                    &detail.versions[1].output_schema,
                )
            })
            .unwrap();
        assert_eq!(
            result["schema_compatibility"],
            serde_json::to_value(expected).unwrap()
        );
    }
    let invalid_version = daemon
        .read_tool_impl(&session, &json!({"name":name,"version":999}))
        .unwrap_err();
    assert_eq!(
        invalid_version.to_string(),
        format!("'{name}' has no version 999")
    );
    assert_eq!(read(json!({"name":name})), omitted);
    assert_eq!(omitted["metrics"]["invocations"], 0);
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
