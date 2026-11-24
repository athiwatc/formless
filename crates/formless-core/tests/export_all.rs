//! `formless export-all` / `import-all` (SPEC §15): everything for a new
//! machine in one bundle, without secrets.

#[path = "support/env.rs"]
mod credential_env;

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use serde_json::json;
use std::sync::Arc;

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn everything_moves_to_a_new_machine_without_secrets() {
    let _env = credential_env::EnvGuard::isolated();
    let from = temp_paths("expall");
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.roles.insert("explorer".into(), Default::default());
    let mut p = formless_core::config::ProviderCfg::default();
    p.base_url = "https://example.com/v1".into();
    p.extra_headers.insert("X-Key".into(), "sekrit".into());
    cfg.providers.insert("other".into(), p);
    let mut server = formless_core::config::McpServerCfg::default();
    server.command = "npx".into();
    server.env.insert("TOKEN".into(), "sekrit".into());
    cfg.mcp.servers.insert("files".into(), server);
    cfg.save(&from).unwrap();
    {
        let store = Store::open(&from.db_file()).unwrap();
        store
            .create_module_version(
                "strutil",
                "string helpers",
                "module.exports = { up: (s) => s.toUpperCase() };",
                "agent",
            )
            .unwrap();
        store
            .create_tool_version(
                "shout",
                "upper-cases text",
                r#"{"type":"object","properties":{"t":{"type":"string"}}}"#,
                "{}",
                "function run(i){ return {t: String(i.t || '').toUpperCase()}; }",
                &Capabilities::default(),
                64,
                "agent",
            )
            .unwrap();
        store
            .add_memory("Uses pnpm, not npm", "global", "user", None)
            .unwrap();
        formless_core::skills::install_text(
            &from,
            &store,
            "---\nname: travel-guide\ndescription: Plan a trip.\n---\n# Travel guide\n",
            "local",
            "user",
        )
        .unwrap();
        let s = store.create_session(Some("History".into())).unwrap();
        store.append_message(&s.id, "user", "hello there").unwrap();
        store
            .create_job(
                "check the news",
                None,
                Some("0 9 * * 1-5"),
                "2030-01-01T09:00:00+00:00",
                true,
                "user",
            )
            .unwrap();
    }
    let a = Arc::new(formless_core::daemon::Daemon::new(from.clone()).unwrap());
    let source_store = Store::open(&from.db_file()).unwrap();
    let session_id = source_store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.name == "History")
        .unwrap()
        .id;
    a.handle(Request::AttachFile {
        session_id: session_id.clone(),
        name: "notes.txt".into(),
        data_b64: "aGVsbG8=".into(),
        mime: Some("text/plain".into()),
    })
    .await
    .unwrap();
    let held = source_store.take_pending_attachments(&session_id).unwrap();
    source_store
        .append_message_meta(
            &session_id,
            "user",
            "read the attached note",
            Some(json!({"attachments": held}).to_string()),
        )
        .unwrap();
    source_store
        .save_checkpoint(&session_id, "before attachment", 1)
        .unwrap();
    source_store
        .save_checkpoint(&session_id, "after attachment", 2)
        .unwrap();
    let mut bundle = a
        .handle(Request::ExportAll { sessions: true })
        .await
        .unwrap();
    let session_bundle = &mut bundle["sessions"][0];
    assert_eq!(session_bundle["messages"].as_array().unwrap().len(), 2);
    session_bundle["messages"][0]["seq"] = json!(10);
    session_bundle["messages"][1]["seq"] = json!(20);
    session_bundle["plan"] = json!([{"content":"Review attachment","status":"completed"}]);
    session_bundle["plan_state"] = json!({
        "run_id":"source-run",
        "updated_at":"2026-01-01T00:00:00Z",
        "run_status":"completed",
        "previous_task":false,
        "origin":"live",
    });
    session_bundle["plan_history"] = json!([
        {
            "anchor_seq":10,
            "items":[{"content":"Review attachment","status":"pending"}],
            "state":{
                "run_id":"source-run",
                "updated_at":"2026-01-01T00:00:00Z",
                "run_status":"running",
                "previous_task":false,
                "origin":"live",
            },
        },
        {
            "anchor_seq":20,
            "items":[{"content":"Review attachment","status":"completed"}],
            "state":{
                "run_id":"source-run",
                "updated_at":"2026-01-01T00:00:01Z",
                "run_status":"completed",
                "previous_task":true,
                "origin":"live",
            },
        },
    ]);
    assert_eq!(bundle["format"], "formless.all/1");
    let text = bundle.to_string();
    assert!(!text.contains("sekrit"), "secrets are blanked");
    let blanked: Vec<&str> = bundle["blanked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        blanked.contains(&"providers.other.extra_headers.X-Key"),
        "{blanked:?}"
    );
    assert!(
        blanked.contains(&"mcp.servers.files.env.TOKEN"),
        "{blanked:?}"
    );

    let to = temp_paths("impall");
    let mut fresh = Config::default();
    fresh.naming.enabled = false;
    fresh.save(&to).unwrap();
    let b = Arc::new(formless_core::daemon::Daemon::new(to.clone()).unwrap());
    let target_store = Store::open(&to.db_file()).unwrap();
    let fixed_session = target_store
        .create_session(Some("Local job history".into()))
        .unwrap();
    target_store
        .create_job(
            "check the news",
            Some(&fixed_session.id),
            Some("0 9 * * 1-5"),
            "2030-01-01T09:00:00+00:00",
            true,
            "user",
        )
        .unwrap();
    let r = b
        .handle(Request::ImportAll {
            bundle: bundle.clone(),
        })
        .await
        .unwrap();
    assert_eq!(r["failed"], json!([]), "{r}");
    assert_eq!(
        (
            r["tools"].as_i64(),
            r["modules"].as_i64(),
            r["memories"].as_i64()
        ),
        (Some(1), Some(1), Some(1)),
        "{r}"
    );
    assert_eq!(
        (r["jobs"].as_i64(), r["sessions"].as_i64()),
        (Some(1), Some(1)),
        "{r}"
    );
    assert_eq!(
        (
            r["providers"].as_i64(),
            r["roles"].as_i64(),
            r["mcp_servers"].as_i64()
        ),
        (Some(1), Some(1), Some(1)),
        "{r}"
    );
    assert_eq!(
        r["added"],
        json!({
            "providers": 1,
            "roles": 1,
            "mcp_servers": 1,
            "modules": 1,
            "tools": 1,
            "skills": 1,
            "memories": 1,
            "jobs": 1,
            "sessions": 1,
        }),
        "{r}"
    );
    assert_eq!(r["skipped_existing"]["skills"], 0);
    let store = Store::open(&to.db_file()).unwrap();
    assert!(store.get_tool_by_name("shout").unwrap().is_some());
    assert!(store.get_module_by_name("strutil").unwrap().is_some());
    let jobs = store.list_jobs().unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(
        jobs.iter().filter(|job| job.session_id.is_none()).count(),
        1
    );
    assert!(
        jobs.iter()
            .any(|job| job.session_id.is_none() && !job.enabled),
        "standalone imported jobs come in paused"
    );
    assert!(store
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.name == "History"));
    let cfg = Config::load(&to).unwrap();
    assert!(cfg.roles.contains_key("explorer"));
    assert_eq!(
        cfg.mcp.servers["files"].env["TOKEN"], "",
        "blank, to fill in"
    );
    let skill_path = store.skill_path("travel-guide").unwrap().unwrap();
    store
        .add_skill_version(
            "travel-guide",
            "Updated trip planner.",
            2,
            &skill_path,
            "local",
            "user",
            false,
        )
        .unwrap();
    store.set_skill_enabled("travel-guide", true).unwrap();
    let memory = store.list_memories().unwrap().remove(0);
    store
        .update_memory(&memory.id, None, None, Some(false))
        .unwrap();

    // Re-import skips existing entries without changing their current state.
    let again = b
        .handle(Request::ImportAll {
            bundle: bundle.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        (
            again["tools"].as_i64(),
            again["modules"].as_i64(),
            again["skills"].as_i64(),
            again["memories"].as_i64(),
            again["jobs"].as_i64(),
            again["sessions"].as_i64(),
            again["providers"].as_i64(),
            again["roles"].as_i64(),
            again["mcp_servers"].as_i64(),
        ),
        (
            Some(0),
            Some(0),
            Some(0),
            Some(0),
            Some(0),
            Some(0),
            Some(0),
            Some(0),
            Some(0)
        ),
        "{again}"
    );
    assert_eq!(
        again["added"],
        json!({
            "providers": 0,
            "roles": 0,
            "mcp_servers": 0,
            "modules": 0,
            "tools": 0,
            "skills": 0,
            "memories": 0,
            "jobs": 0,
            "sessions": 0,
        }),
        "{again}"
    );
    assert_eq!(
        again["skipped_existing"],
        json!({
            "providers": 1,
            "roles": 1,
            "mcp_servers": 1,
            "modules": 1,
            "tools": 1,
            "skills": 1,
            "memories": 1,
            "jobs": 1,
            "sessions": 1,
        }),
        "{again}"
    );
    let skill = store.get_skill("travel-guide").unwrap().unwrap();
    assert_eq!((skill.version, skill.enabled), (2, true));
    assert!(!store.get_memory(&memory.id).unwrap().unwrap().enabled);
    assert_eq!(store.list_jobs().unwrap().len(), 2);
    assert_eq!(store.list_sessions().unwrap().len(), 2);

    let mut normalized = bundle.clone();
    let normalized_session = &mut normalized["sessions"][0];
    assert!(normalized_session
        .as_object_mut()
        .unwrap()
        .remove("model")
        .is_some());
    assert!(normalized_session
        .as_object_mut()
        .unwrap()
        .remove("reasoning_effort")
        .is_some());
    normalized_session["checkpoints"]
        .as_array_mut()
        .unwrap()
        .reverse();
    normalized_session["messages"][0]["seq"] = json!(100);
    normalized_session["messages"][1]["seq"] = json!(200);
    normalized_session["plan_history"][0]["anchor_seq"] = json!(100);
    normalized_session["plan_history"][1]["anchor_seq"] = json!(200);
    normalized_session["plan_state"] = json!({
        "run_id":"detached-run",
        "updated_at":"2026-02-01T00:00:00Z",
        "run_status":"failed",
        "previous_task":true,
        "origin":"copied",
    });
    normalized_session["plan_history"][0]["state"] = normalized_session["plan_state"].clone();
    normalized_session["plan_history"][1]["state"] = normalized_session["plan_state"].clone();
    let normalized_again = b
        .handle(Request::ImportAll {
            bundle: normalized.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        normalized_again["added"]["sessions"], 0,
        "{normalized_again}"
    );
    assert_eq!(
        normalized_again["skipped_existing"]["sessions"], 1,
        "{normalized_again}"
    );
    let imported_session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.name == "History")
        .unwrap();
    let attached_message = store
        .list_messages(&imported_session.id)
        .unwrap()
        .into_iter()
        .find(|message| message.content == "read the attached note")
        .unwrap();
    let attachment_meta: serde_json::Value =
        serde_json::from_str(attached_message.meta.as_deref().unwrap()).unwrap();
    assert!(attachment_meta["attachments"][0]["imported"]
        .as_bool()
        .unwrap());
    assert!(attachment_meta["attachments"][0].get("path").is_none());

    store
        .append_message(&imported_session.id, "user", "local continuation")
        .unwrap();
    store
        .set_session_model(
            &imported_session.id,
            Some("local-model".into()),
            Some("high".into()),
        )
        .unwrap();
    store
        .rename_session(&imported_session.id, "Locally evolved")
        .unwrap();
    store
        .save_checkpoint(&imported_session.id, "local checkpoint", 3)
        .unwrap();
    let evolved_again = b
        .handle(Request::ImportAll {
            bundle: normalized.clone(),
        })
        .await
        .unwrap();
    assert_eq!(evolved_again["added"]["sessions"], 0, "{evolved_again}");
    assert_eq!(
        evolved_again["skipped_existing"]["sessions"], 1,
        "{evolved_again}"
    );
    let evolved = store.get_session(&imported_session.id).unwrap().unwrap();
    assert_eq!(evolved.name, "Locally evolved");
    assert_eq!(evolved.model.as_deref(), Some("local-model"));
    assert_eq!(evolved.reasoning_effort.as_deref(), Some("high"));
    assert!(store
        .list_messages(&imported_session.id)
        .unwrap()
        .iter()
        .any(|message| message.content == "local continuation"));
    assert_eq!(
        store.session_plan(&imported_session.id).unwrap()[0].content,
        "Review attachment"
    );
    assert!(!store.get_memory(&memory.id).unwrap().unwrap().enabled);

    let mut revised_plan = normalized.clone();
    let revised_session = &mut revised_plan["sessions"][0];
    revised_session["plan"][0]["content"] = json!("Review and summarize the attachment");
    revised_session["plan_history"][1]["items"][0]["content"] =
        json!("Review and summarize the attachment");
    let plan_import = b
        .handle(Request::ImportAll {
            bundle: revised_plan.clone(),
        })
        .await
        .unwrap();
    assert_eq!(plan_import["added"]["sessions"], 1, "{plan_import}");
    assert_eq!(store.list_sessions().unwrap().len(), 3);
    let revised_imported = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.name == "History")
        .unwrap();
    assert_eq!(
        store.session_plan(&revised_imported.id).unwrap()[0].content,
        "Review and summarize the attachment"
    );
    assert_eq!(
        store.session_plan(&imported_session.id).unwrap()[0].content,
        "Review attachment"
    );
    let revised_again = b
        .handle(Request::ImportAll {
            bundle: revised_plan,
        })
        .await
        .unwrap();
    assert_eq!(revised_again["added"]["sessions"], 0, "{revised_again}");
    assert_eq!(
        revised_again["skipped_existing"]["sessions"], 1,
        "{revised_again}"
    );

    let mut mislabeled_skill = bundle.clone();
    mislabeled_skill["skills"][0]["name"] = json!("trip-helper");
    let skill_collision = b
        .handle(Request::ImportAll {
            bundle: mislabeled_skill,
        })
        .await
        .unwrap();
    assert_eq!(skill_collision["added"]["skills"], 0, "{skill_collision}");
    assert_eq!(
        skill_collision["skipped_existing"]["skills"], 1,
        "{skill_collision}"
    );
    let skill = store.get_skill("travel-guide").unwrap().unwrap();
    assert_eq!((skill.version, skill.enabled), (2, true));
    assert!(!store.get_memory(&memory.id).unwrap().unwrap().enabled);
}

/// A provider key whose provider was removed from the config by hand is
/// still deleted, and an unreadable config is reported (#1080).
#[test]
fn orphaned_named_secrets_are_deleted_and_bad_configs_reported() {
    let _env = credential_env::EnvGuard::isolated();
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-secrets-{short}"));
    let paths = formless_core::Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-secrets-{short}"),
    };
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    formless_core::config::save_provider_key(&paths, "gone", "sk-test").unwrap();
    let file = paths.config_dir.join("provider-key-gone.json");
    assert!(file.exists());
    // Not in the config (never was, like one removed by hand).
    let warnings = formless_core::config::delete_secrets(&paths);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!file.exists(), "the orphaned key file is gone");
    assert!(formless_core::config::provider_key(&paths, "gone").is_none());

    std::fs::write(paths.config_file(), "[provider\nbroken").unwrap();
    let warnings = formless_core::config::delete_secrets(&paths);
    assert!(
        warnings.iter().any(|w| w.contains("could not be read")),
        "{warnings:?}"
    );
    let _ = std::fs::remove_dir_all(base);
}

/// `*_FILE` keys: BOM and whitespace trimmed, unusable files named, and
/// named providers read theirs too (#1083).
#[test]
fn key_files_are_trimmed_named_and_work_for_named_providers() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::config::key_from_file_env;
    let dir = std::env::temp_dir().join(format!("fl-keyfile-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("key");
    std::fs::write(&good, "\u{feff}  sk-file-key \n").unwrap();
    _env.set("FORMLESS_TEST_KEYFILE_GOOD", &good);
    assert_eq!(
        key_from_file_env("FORMLESS_TEST_KEYFILE_GOOD"),
        Some(Ok("sk-file-key".into()))
    );
    let empty = dir.join("empty");
    std::fs::write(&empty, " \n").unwrap();
    _env.set("FORMLESS_TEST_KEYFILE_EMPTY", &empty);
    let e = key_from_file_env("FORMLESS_TEST_KEYFILE_EMPTY")
        .unwrap()
        .unwrap_err();
    assert!(
        e.contains("FORMLESS_TEST_KEYFILE_EMPTY") && e.contains("empty"),
        "{e}"
    );
    _env.set("FORMLESS_TEST_KEYFILE_MISSING", dir.join("nope"));
    let e = key_from_file_env("FORMLESS_TEST_KEYFILE_MISSING")
        .unwrap()
        .unwrap_err();
    assert!(e.contains("can't be read"), "{e}");
    assert_eq!(key_from_file_env("FORMLESS_TEST_KEYFILE_UNSET"), None);

    let paths = formless_core::Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: "keyfile".into(),
    };
    _env.set("FORMLESS_API_KEY_FILETESTPROV_FILE", &good);
    assert_eq!(
        formless_core::config::provider_key(&paths, "filetestprov").as_deref(),
        Some("sk-file-key")
    );
    let _ = std::fs::remove_dir_all(dir);
}
