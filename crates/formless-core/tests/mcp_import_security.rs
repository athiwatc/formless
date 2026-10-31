//! MCP review replies expose names, never credential values or executable imports.
#[path = "support/env.rs"]
mod credential_env;
use formless_core::{daemon::Daemon, Config, Paths};
use formless_protocol::Request;
use serde_json::{json, Value};
use std::sync::Arc;

fn fixture(_env: &credential_env::EnvGuard) -> (Paths, Arc<Daemon>, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("fl-mcp-review-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let project = base.join("project");
    std::fs::create_dir_all(project.join(".cursor")).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    (paths, daemon, project)
}
async fn candidate(daemon: &Arc<Daemon>, project: &std::path::Path, name: &str) -> Value {
    let list = daemon
        .handle(Request::McpImportCandidates {
            cwd: Some(project.to_string_lossy().into_owned()),
        })
        .await
        .unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap()
        .clone()
}
async fn release(_env: &credential_env::EnvGuard, paths: &Paths, daemon: Arc<Daemon>) {
    let pending = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while pending.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("MCP fixture tasks outlived isolation");
    let _ = std::fs::remove_dir_all(paths.data_dir.parent().unwrap());
}

#[tokio::test]
async fn candidates_omit_values_and_import_uses_a_revalidated_reference() {
    let env = credential_env::EnvGuard::isolated();
    let (paths, daemon, project) = fixture(&env);
    let file = project.join(".cursor/mcp.json");
    std::fs::write(&file, json!({"mcpServers":{"reference server":{"command":"missing-review-server", "env":{"K":"s3cr3t"}}}}).to_string()).unwrap();
    let review = candidate(&daemon, &project, "reference-server").await;
    assert!(!review.to_string().contains("s3cr3t"));
    assert!(review.get("server").is_none());
    assert_eq!(review["env"], json!(["K"]));
    assert_eq!(review["original_name"], "reference server");
    let id = review["candidate_id"].as_str().unwrap().to_string();
    std::fs::write(&file, json!({"mcpServers":{"reference server":{"command":"changed-review-server", "env":{"K":"changed-secret"}}}}).to_string()).unwrap();
    let error = daemon
        .handle(Request::ImportMcpCandidate {
            candidate_id: id,
            skip_secrets: false,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed"));
    assert!(!Config::load_file_only(&paths)
        .unwrap()
        .mcp
        .servers
        .contains_key("reference-server"));
    let review = candidate(&daemon, &project, "reference-server").await;
    let imported = daemon
        .handle(Request::ImportMcpCandidate {
            candidate_id: review["candidate_id"].as_str().unwrap().into(),
            skip_secrets: true,
        })
        .await
        .unwrap();
    assert!(imported["secret_storage"]
        .as_str()
        .unwrap()
        .contains("skipped"));
    let cfg = Config::load_file_only(&paths).unwrap();
    let server = &cfg.mcp.servers["reference-server"];
    assert!(server.disabled && !server.trusted && server.env.is_empty());
    assert_eq!(server.env_keys, ["K"]);
    release(&env, &paths, daemon).await;
}

#[tokio::test]
async fn copying_values_is_explicit_and_list_and_stderr_json_mask_short_passwords() {
    let env = credential_env::EnvGuard::isolated();
    let (paths, daemon, project) = fixture(&env);
    let file = project.join(".cursor/mcp.json");
    std::fs::write(&file, json!({"mcpServers":{"database":{"command":"npx", "args":["postgresql://u:pw@h/db"], "env":{"K":"s3cr3t"}}, "remote":{"url":"https://example.invalid/mcp","headers":{"Authorization":"header-secret"}}}}).to_string()).unwrap();
    for name in ["database", "remote"] {
        let review = candidate(&daemon, &project, name).await;
        let text = review.to_string();
        assert!(
            !text.contains("pw") && !text.contains("s3cr3t") && !text.contains("header-secret")
        );
        let out = daemon
            .handle(Request::ImportMcpCandidate {
                candidate_id: review["candidate_id"].as_str().unwrap().into(),
                skip_secrets: false,
            })
            .await
            .unwrap();
        assert!(out["secret_storage"]
            .as_str()
            .unwrap()
            .contains("plaintext config.toml"));
    }
    let cfg = Config::load_file_only(&paths).unwrap();
    assert_eq!(cfg.mcp.servers["database"].env["K"], "s3cr3t");
    assert_eq!(
        cfg.mcp.servers["remote"].headers["Authorization"],
        "header-secret"
    );
    let list = daemon
        .handle(Request::ListMcpServers)
        .await
        .unwrap()
        .to_string();
    assert!(!list.contains("pw") && !list.contains("s3cr3t") && !list.contains("header-secret"));
    std::fs::create_dir_all(paths.data_dir.join("mcp")).unwrap();
    std::fs::write(
        paths.data_dir.join("mcp/database.log"),
        "postgresql://u:pw@h/db password pw K=s3cr3t",
    )
    .unwrap();
    let view = daemon
        .handle(Request::McpServerLog {
            name: "database".into(),
            lines: None,
        })
        .await
        .unwrap()
        .to_string();
    assert!(!view.contains("pw") && !view.contains("s3cr3t"));
    release(&env, &paths, daemon).await;
}

#[tokio::test]
async fn catalog_execution_needs_consent_before_any_package_can_start() {
    let env = credential_env::EnvGuard::isolated();
    let (paths, daemon, _) = fixture(&env);
    let error = daemon
        .handle(Request::AddMcpFromCatalog {
            id: "memory".into(),
            name: None,
            params: Default::default(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("confirm_run") && error.to_string().contains("@2.0.1"));
    assert!(!Config::load_file_only(&paths)
        .unwrap()
        .mcp
        .servers
        .contains_key("memory"));
    release(&env, &paths, daemon).await;
}

#[tokio::test]
async fn import_all_summarizes_successfully_imported_capability_families() {
    let env = credential_env::EnvGuard::isolated();
    let (source_paths, source, _) = fixture(&env);
    let store = formless_core::Store::open(&source_paths.db_file()).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[
        "net:example.com".into(),
        format!("fs:read:{}", source_paths.data_dir.display()),
        format!("fs:write:{}", source_paths.data_dir.display()),
    ])
    .unwrap();
    store
        .create_tool_version(
            "summary_tool",
            "d",
            "{}",
            "{}",
            "function run(){return {ok:true};}",
            &caps,
            64,
            "agent",
        )
        .unwrap();
    let bundle = source
        .handle(Request::ExportAll { sessions: false })
        .await
        .unwrap();
    let (paths, daemon, _) = fixture(&env);
    let out = daemon.handle(Request::ImportAll { bundle }).await.unwrap();
    assert_eq!(out["failed"], json!([]), "{out}");
    assert_eq!(out["tools"], 1);
    assert_eq!(out["tool_capability_families"]["net"], 1);
    assert_eq!(out["tool_capability_families"]["fs"], 2);
    release(&env, &paths, daemon).await;
    drop(store);
    release(&env, &source_paths, source).await;
}
