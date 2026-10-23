//! A working directory per Session (SPEC §5.1).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn sessions_have_a_working_directory_for_relative_paths() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-cwd-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-cwd-{short}"),
    };
    let project = dir.join("project");
    std::fs::create_dir_all(project.join("sub")).unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(project.join("sub/notes.txt"), "hello from notes").unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let s = daemon
        .handle(Request::SetSessionCwd {
            id: sid.clone(),
            cwd: Some(project.to_string_lossy().into_owned()),
        })
        .await
        .unwrap();
    let canonical = project.canonicalize().unwrap();
    assert_eq!(s["cwd"], canonical.to_string_lossy().as_ref());
    // Relative to where the Session is; a missing directory is refused.
    let s = daemon
        .handle(Request::SetSessionCwd {
            id: sid.clone(),
            cwd: Some("sub".into()),
        })
        .await
        .unwrap();
    assert!(s["cwd"].as_str().unwrap().ends_with("project/sub"));
    assert!(daemon
        .handle(Request::SetSessionCwd {
            id: sid.clone(),
            cwd: Some("nope".into()),
        })
        .await
        .is_err());
    // The agent is told, in its prompt and the transcript.
    let prompt = store.session_system_prompt(&sid).unwrap().unwrap();
    assert!(prompt.contains("Working directory: ") && prompt.contains("project/sub"));
    let notes = store.list_messages(&sid).unwrap();
    assert!(notes
        .iter()
        .any(|m| m.role == "system-note" && m.content.contains("project/sub")));
    // A fork keeps it.
    let fork = store.fork_session(&sid, None, None).unwrap().unwrap();
    assert!(fork.cwd.unwrap().ends_with("project/sub"));

    // A Tool's input-bound path, given relative, reads from the Session's
    // directory (the grant binds to the absolute path).
    let out = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "read_text", "description": "reads a file", 
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]},
            "output_schema": {}, "implementation_js": "function run(i){ return {text: fs.read(i.path), path: i.path}; }",
            "capabilities": ["fs:read:$input.path"]}))
        .await;
    assert!(out.contains("\"created\""), "{out}");
    let (tool, ver) = store.get_tool_by_name("read_text").unwrap().unwrap();
    store
        .set_approval(
            &tool.id,
            &formless_runtime::Capabilities::parse(&ver.capabilities).unwrap(),
            ver.memory_mb,
        )
        .unwrap();
    let r = daemon
        .call_tool_direct(&sid, "read_text", serde_json::json!({"path": "notes.txt"}))
        .await;
    assert!(r.contains("hello from notes"), "{r}");
    assert!(
        r.contains("project/sub/notes.txt"),
        "absolute path passed to the Tool: {r}"
    );
}
