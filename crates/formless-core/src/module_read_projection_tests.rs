//! Private module history only: no JavaScript or provider execution.
use super::*;

#[tokio::test]
async fn module_read_projection_preserves_valid_output_and_narrows_only_unrequested_code() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-module-read-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
    cfg.provider.model = "private-unused".into();
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let name = "Módulo_日本語";
    let id = daemon
        .with_store(|st| {
            let mut id = String::new();
            for code in [
                "module.exports={value:'ไทย'};".to_owned(),
                format!(
                    "/*{}*/module.exports={{value:2}};",
                    "private comment ".repeat(1000)
                ),
            ] {
                id = st
                    .create_module_version(name, "Unicode description ไทย", &code, "user")?
                    .0
                    .id;
            }
            st.create_tool_version(
                "module_caller",
                "private fixture",
                "{}",
                "{}",
                &format!("async function run(){{return await modules.load('{name}');}}"),
                &Capabilities::default(),
                64,
                "user",
            )?;
            Ok(id)
        })
        .unwrap();
    for requested in [None, Some(1), Some(2)] {
        let want = requested.unwrap_or(2);
        let versions = daemon.with_store(|st| st.module_versions(&id)).unwrap();
        let info = daemon
            .with_store(|st| st.get_module_by_name(name))
            .unwrap()
            .unwrap()
            .0;
        let dependents = daemon.with_store(|st| st.module_dependents(name)).unwrap();
        let projection = daemon
            .with_store(|st| st.module_versions_for_read(&id, want))
            .unwrap();
        assert_eq!(projection.len(), versions.len());
        for (projected, original) in projection.iter().zip(&versions) {
            assert_eq!(
                (&projected.0, &projected.2, &projected.3),
                (&original.0, &original.2, &original.3)
            );
            assert_eq!(
                projected.1.as_deref(),
                (original.0 == want).then_some(original.1.as_str())
            );
        }
        let args = if let Some(version) = requested {
            json!({"name":name,"version":version})
        } else {
            json!({"name":name})
        };
        let actual: Value = serde_json::from_str(&daemon.read_module_impl(&args).unwrap()).unwrap();
        let expected = json!({"name":info.name,"description":info.description,"version":want,"current_version":2,"retired":info.retired,
            "versions":versions.iter().map(|(version,_,author,at)|json!({"version":version,"author":author,"created_at":at})).collect::<Vec<_>>(),
            "implementation_js":versions.iter().find(|v|v.0==want).unwrap().1,"required_by":dependents});
        assert_eq!(actual, expected);
        let full = daemon
            .handle_inner(Request::GetModule { id: id.clone() })
            .await
            .unwrap();
        assert!(
            full.to_string().contains("private comment"),
            "GetModule still returns history code"
        );
    }
    assert_eq!(
        daemon
            .read_module_impl(&json!({"name":"unknown-ไทย"}))
            .unwrap_err()
            .to_string(),
        "no module named 'unknown-ไทย'"
    );
    assert_eq!(
        daemon
            .read_module_impl(&json!({"name":name,"version":99}))
            .unwrap_err()
            .to_string(),
        format!("module '{name}' has no version 99")
    );
    let conn = rusqlite::Connection::open(daemon.paths.db_file()).unwrap();
    conn.execute(
        "UPDATE module_versions SET code=x'FF' WHERE module_id=?1 AND version=1",
        [&id],
    )
    .unwrap();
    assert!(
        daemon.read_module_impl(&json!({"name":name})).is_ok(),
        "unrequested history code is intentionally not decoded"
    );
    assert!(daemon
        .read_module_impl(&json!({"name":name,"version":1}))
        .is_err());
    assert!(
        daemon.with_store(|st| st.module_versions(&id)).is_err(),
        "full history API is unchanged"
    );
    conn.execute(
        "UPDATE module_versions SET author=x'FF' WHERE module_id=?1 AND version=1",
        [&id],
    )
    .unwrap();
    let error = daemon
        .read_module_impl(&json!({"name":name,"version":99}))
        .unwrap_err()
        .to_string();
    assert!(
        !error.contains("has no version"),
        "metadata errors still precede missing-version selection"
    );
    conn.execute("UPDATE module_versions SET code='module.exports=1',author='user' WHERE module_id=?1 AND version=1",[&id]).unwrap();
    conn.execute(
        "UPDATE module_versions SET code=x'FF' WHERE module_id=?1 AND version=2",
        [&id],
    )
    .unwrap();
    assert!(
        daemon
            .read_module_impl(&json!({"name":name,"version":1}))
            .is_err(),
        "the initial current-code read/error remains"
    );
    drop(conn);
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
