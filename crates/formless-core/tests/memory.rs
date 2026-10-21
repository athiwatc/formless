//! User memory (SPEC §7.11): notes written by the agent, deduplicated,
//! scoped, and snapshotted into new Sessions' system prompts.

use formless_core::{Paths, Store};
use std::sync::Arc;

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

#[test]
fn memories_are_validated_deduplicated_and_scoped() {
    let store = Store::open_in_memory().unwrap();
    let (a, new) = store
        .add_memory("Uses  pnpm, not npm", "global", "agent", Some("s1"))
        .unwrap();
    assert!(new);
    assert_eq!(a.text, "Uses pnpm, not npm");
    let (b, new) = store
        .add_memory("uses pnpm, NOT npm ", "global", "agent", None)
        .unwrap();
    assert!(
        !new && b.id == a.id,
        "duplicate refreshes the existing note"
    );
    store
        .add_memory(
            "Tests run with make check",
            "project:/code/app",
            "agent",
            None,
        )
        .unwrap();
    assert!(store.add_memory("", "global", "agent", None).is_err());
    assert!(store
        .add_memory(&"x".repeat(301), "global", "agent", None)
        .is_err());
    assert!(store
        .add_memory("ok", "project:relative", "agent", None)
        .is_err());

    let block = formless_core::memory::prompt_block(&store.list_memories().unwrap());
    assert!(block.contains(&format!("- Uses pnpm, not npm [{}]", a.id)));
    assert!(block.contains("When working in /code/app:\n- Tests run with make check"));

    store.update_memory(&a.id, None, None, Some(false)).unwrap();
    let block = formless_core::memory::prompt_block(&store.list_memories().unwrap());
    assert!(!block.contains("pnpm"), "disabled notes are left out");
    assert!(store.delete_memory(&a.id).unwrap());
    assert!(!store.delete_memory(&a.id).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn remember_and_forget_built_ins_feed_the_next_session() {
    let paths = temp_paths("memory");
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let s1 = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let out: serde_json::Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &s1,
                "remember",
                serde_json::json!({"text": "Answer in Thai"}),
            )
            .await,
    )
    .unwrap();
    let id = out["id"].as_str().unwrap().to_string();
    assert_eq!(out["saved"], "new");
    let m = store.get_memory(&id).unwrap().unwrap();
    assert_eq!(
        (m.author.as_str(), m.session_id.as_deref()),
        ("agent", Some(s1.as_str()))
    );
    assert!(daemon
        .effective_system_prompt()
        .contains("- Answer in Thai"));

    let gone = daemon
        .call_tool_direct(&s1, "forget", serde_json::json!({"id": id}))
        .await;
    assert!(gone.contains("forgotten"), "{gone}");
    let missing = daemon
        .call_tool_direct(&s1, "forget", serde_json::json!({"id": id}))
        .await;
    assert!(missing.starts_with("error:"), "{missing}");
}

#[tokio::test(flavor = "multi_thread")]
async fn users_list_add_edit_and_delete_memories() {
    use formless_protocol::Request;
    let paths = temp_paths("memreq");
    formless_core::Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let added = daemon
        .handle(Request::UpsertMemory {
            id: None,
            text: Some("Projects live in ~/code".into()),
            scope: None,
            enabled: None,
        })
        .await
        .unwrap();
    let id = added["id"].as_str().unwrap().to_string();
    assert_eq!(
        (added["author"].as_str(), added["scope"].as_str()),
        (Some("user"), Some("global"))
    );
    let edited = daemon
        .handle(Request::UpsertMemory {
            id: Some(id.clone()),
            text: Some("Projects live in ~/src".into()),
            scope: None,
            enabled: Some(false),
        })
        .await
        .unwrap();
    assert_eq!(
        (edited["text"].as_str(), edited["enabled"].as_bool()),
        (Some("Projects live in ~/src"), Some(false))
    );
    let list = daemon.handle(Request::ListMemories).await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    daemon
        .handle(Request::DeleteMemory { id: id.clone() })
        .await
        .unwrap();
    assert!(daemon.handle(Request::DeleteMemory { id }).await.is_err());
    assert_eq!(
        daemon.handle(Request::ListMemories).await.unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn secrets_are_refused() {
    use formless_core::memory::find_secret;
    for secret in [
        "My OpenAI key is sk-proj-abcdefghijklmnop1234567890",
        "use ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 for GitHub",
        "AWS: AKIAIOSFODNN7EXAMPLE",
        "Authorization: Bearer abcdefghijklmnopqrstuvwxyz",
        "password is hunter2!",
        "db password: correct-horse",
        "api_key=abcd1234",
        "token aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG",
        "-----BEGIN RSA PRIVATE KEY----- MIIE",
        "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEF123",
    ] {
        assert!(find_secret(secret).is_some(), "missed: {secret}");
    }
    for fine in [
        "Uses pnpm, not npm",
        "Answer in Thai",
        "Projects live in ~/code and tests run with make check",
        "Prefers the password manager over typing passwords",
        "Commit 3f2a9c1b4d5e6f708192a3b4c5d6e7f8091a2b3c is the baseline",
        "Reset tokens expire after an hour in their app",
    ] {
        assert_eq!(find_secret(fine), None, "false positive: {fine}");
    }
    let store = Store::open_in_memory().unwrap();
    let err = store
        .add_memory("my password is hunter22", "global", "user", None)
        .unwrap_err();
    assert!(err.to_string().contains("secret"), "{err}");
    let (m, _) = store
        .add_memory("Uses pnpm", "global", "user", None)
        .unwrap();
    assert!(store
        .update_memory(&m.id, Some("token: abcdef123456"), None, None)
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_output_is_never_remembered_verbatim() {
    let paths = temp_paths("memtool");
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap().id;
    store
        .append_message(
            &s,
            "tool",
            "Customer record: Jane Example lives at 12 Sample Road and owes 300 dollars",
        )
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let out = daemon
        .call_tool_direct(
            &s,
            "remember",
            serde_json::json!({"text": "Jane Example lives at 12 Sample Road and owes 300 dollars"}),
        )
        .await;
    assert!(out.contains("copied from a Tool result"), "{out}");
    assert!(store.list_memories().unwrap().is_empty());
}
