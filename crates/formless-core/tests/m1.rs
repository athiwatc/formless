//! M1 acceptance tests (SPEC §18 M1, §19): storage/compaction round-trip,
//! provider against a mock OpenAI-compatible server, daemon end-to-end.

#[path = "support/env.rs"]
mod credential_env;
#[path = "support/historical.rs"]
mod historical;

use axum::{
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use formless_core::config::{Config, ProviderCfg};
use formless_core::provider::{probe, Provider};
use formless_core::storage::{build_context, provider_message, Store};
use formless_core::Paths;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn temp_paths(tag: &str) -> Paths {
    // Short path: macOS Unix socket paths are limited to ~104 chars.
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

#[test]
fn storage_resume_is_byte_identical_and_compaction_flags() {
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("storage");
    let session_id;
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(Some("Test".into())).unwrap();
        session_id = s.id.clone();
        for i in 0..8 {
            store
                .append_message(
                    &s.id,
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("message {i} {}", "x".repeat(40)),
                )
                .unwrap();
        }
        store.save_checkpoint(&s.id, "earlier summary", 2).unwrap();
        let run = store.create_run(&s.id).unwrap();
        store.set_run_status(&run, "running").unwrap();
    }
    // Reopen: orphan runs interrupted, messages identical.
    let store = Store::open(&paths.db_file()).unwrap();
    store.mark_orphan_runs_interrupted().unwrap();
    let msgs = store.list_messages(&session_id).unwrap();
    assert_eq!(msgs.len(), 8);
    assert_eq!(msgs[3].content, format!("message 3 {}", "x".repeat(40)));
    let (ctx, needs, up_to) = build_context(&store, &session_id, "sys", 60, 0.8).unwrap();
    assert!(ctx
        .iter()
        .any(|v| v.to_string().contains("earlier summary")));
    assert!(needs, "tiny context must request compaction");
    assert!(up_to > 2);
    let (_ctx, needs2, _) = build_context(&store, &session_id, "sys", 1_000_000, 0.8).unwrap();
    assert!(!needs2);
}

#[test]
fn session_search_handles_multilingual_substrings_and_preserves_english_prefixes() {
    let store = Store::open_in_memory().unwrap();
    let session = store
        .create_session(Some("multilingual search".into()))
        .unwrap();
    let chinese = store
        .append_message(&session.id, "user", "我喜欢这个文件，请帮我总结")
        .unwrap();
    let thai_news = store.append_message(&session.id, "user", "ข่าววันนี้").unwrap();
    let thai_white = store
        .append_message(&session.id, "user", "เสื้อสีขาว")
        .unwrap();
    let cafe = store
        .append_message(&session.id, "user", "CAFÉ menu")
        .unwrap();
    let fixing = store
        .append_message(&session.id, "user", "fixing the bug")
        .unwrap();

    for (query, expected) in [
        ("文件", &chinese.id),
        ("总结", &chinese.id),
        ("ข่าว", &thai_news.id),
        ("café", &cafe.id),
        ("fix", &fixing.id),
    ] {
        let hits = store.search_sessions(query, 20).unwrap();
        assert_eq!(hits.len(), 1, "query {query:?}: {hits:?}");
        assert_eq!(&hits[0].message_id, expected, "query {query:?}");
    }
    assert!(store
        .search_sessions("ข่าว", 20)
        .unwrap()
        .iter()
        .all(|hit| hit.message_id != thai_white.id));

    let canonical = store.search_sessions("cafe\u{301}", 20).unwrap();
    assert_eq!(canonical.len(), 1);
    assert_eq!(canonical[0].message_id, cafe.id);

    let accented_mixed = store
        .append_message(&session.id, "user", "cafe 我喜欢这个文件")
        .unwrap();
    let accented_mixed_hits = store.search_sessions("café 文件", 20).unwrap();
    assert_eq!(accented_mixed_hits.len(), 1);
    assert_eq!(accented_mixed_hits[0].message_id, accented_mixed.id);

    let mixed = store
        .append_message(&session.id, "user", "fixing notes about 文件")
        .unwrap();
    let mixed_hits = store.search_sessions("fi 文件", 20).unwrap();
    assert_eq!(mixed_hits.len(), 1);
    assert_eq!(mixed_hits[0].message_id, mixed.id);
}

#[test]
fn emoji_search_does_not_match_messages_by_variation_selector_alone() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(Some("emoji search".into())).unwrap();
    store.append_message(&session.id, "user", "sun ☀️").unwrap();
    let heart = store
        .append_message(&session.id, "user", "heart ❤️")
        .unwrap();

    let hits = store.search_sessions("❤️", 20).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].message_id, heart.id);
}

#[test]
fn thai_search_does_not_end_before_a_combining_tone_mark() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(Some("Thai search".into())).unwrap();
    store.append_message(&session.id, "user", "ที่").unwrap();
    let unmarked = store.append_message(&session.id, "user", "ที").unwrap();

    let hits = store.search_sessions("ที", 20).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].message_id, unmarked.id);
}

#[test]
fn thai_search_allows_substrings_after_completed_combining_marks() {
    let store = Store::open_in_memory().unwrap();
    let session = store
        .create_session(Some("Thai substring search".into()))
        .unwrap();
    let message = store.append_message(&session.id, "user", "มีขาว").unwrap();
    let marked_message = store.append_message(&session.id, "user", "ที่นี่").unwrap();

    let hits = store.search_sessions("ขาว", 20).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].message_id, message.id);

    let hits = store.search_sessions("นี่", 20).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].message_id, marked_message.id);
}

#[test]
fn mixed_accented_latin_unicode_terms_preserve_non_latin_marks() {
    let store = Store::open_in_memory().unwrap();
    let session = store
        .create_session(Some("mixed Unicode search".into()))
        .unwrap();
    let chinese = store
        .append_message(&session.id, "user", "cafe文件")
        .unwrap();
    store.append_message(&session.id, "user", "cafeที่").unwrap();
    let thai_unmarked = store.append_message(&session.id, "user", "cafeที").unwrap();

    let chinese_hits = store.search_sessions("café文件", 20).unwrap();
    assert_eq!(chinese_hits.len(), 1, "{chinese_hits:?}");
    assert_eq!(chinese_hits[0].message_id, chinese.id);

    let thai_hits = store.search_sessions("caféที", 20).unwrap();
    assert_eq!(thai_hits.len(), 1, "{thai_hits:?}");
    assert_eq!(thai_hits[0].message_id, thai_unmarked.id);
}

#[test]
fn unicode_search_index_backfills_existing_messages() {
    let paths = temp_paths("unicode-index-backfill");
    let session_id;
    let message_id;
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap();
        session_id = session.id;
        let message = store
            .append_message(&session_id, "user", "historical 文件")
            .unwrap();
        message_id = message.id;
    }
    let connection = rusqlite::Connection::open(paths.db_file()).unwrap();
    connection
        .execute_batch("DROP TABLE messages_unicode_fts; PRAGMA user_version=78;")
        .unwrap();
    drop(connection);

    let store = Store::open(&paths.db_file()).unwrap();
    let hits = store.search_sessions("文件", 20).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, session_id);
    assert_eq!(hits[0].message_id, message_id);
}

#[test]
fn unicode_search_index_tracks_edits_deletions_order_and_limits() {
    let paths = temp_paths("unicode-index-order");
    let (session_id, first, second, third) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap();
        let first = store
            .append_message(&session.id, "user", "文件 旧标记")
            .unwrap();
        let second = store
            .append_message(&session.id, "user", "文件 删除标记")
            .unwrap();
        let third = store
            .append_message(&session.id, "user", "文件 最新标记")
            .unwrap();
        (session.id, first, second, third)
    };
    let connection = rusqlite::Connection::open(paths.db_file()).unwrap();
    connection
        .execute(
            "UPDATE messages SET created_at='2025-01-01T00:00:00Z' WHERE session_id=?1",
            [&session_id],
        )
        .unwrap();
    drop(connection);
    let store = Store::open(&paths.db_file()).unwrap();

    let limited = store.search_sessions("文件", 2).unwrap();
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].message_id, third.id);
    assert_eq!(limited[1].message_id, second.id);

    store
        .edit_message(&session_id, &first.id, "文件 新标记")
        .unwrap();
    for query in ["旧标记", "删除标记", "最新标记"] {
        assert!(
            store.search_sessions(query, 20).unwrap().is_empty(),
            "{query}"
        );
    }
    let updated = store.search_sessions("新标记", 20).unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].message_id, first.id);
}

#[test]
fn unicode_search_index_clears_session_deletion_cascades() {
    let paths = temp_paths("unicode-index-session-delete");
    let (session_id, rowid) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap();
        store
            .append_message(&session.id, "user", "session 文件 content")
            .unwrap();
        let connection = rusqlite::Connection::open(paths.db_file()).unwrap();
        let rowid = connection
            .query_row(
                "SELECT rowid FROM messages WHERE session_id=?1",
                [&session.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        drop(connection);

        assert!(store.delete_session(&session.id).unwrap());
        (session.id, rowid)
    };

    let store = Store::open(&paths.db_file()).unwrap();
    assert!(store.search_sessions("文件", 20).unwrap().is_empty());
    let connection = rusqlite::Connection::open(paths.db_file()).unwrap();
    let indexed_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM messages_unicode_fts WHERE rowid=?1",
            [rowid],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(indexed_rows, 0, "session {session_id}");
}

#[test]
fn unicode_search_index_fork_writes_are_transactional() {
    let paths = temp_paths("unicode-index-fork");
    let (source_id, fork_id, source_messages, fork_messages) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let source = store.create_session(None).unwrap();
        let first = store
            .append_message(&source.id, "user", "first 文件")
            .unwrap();
        let second = store
            .append_message(&source.id, "assistant", "second 文件")
            .unwrap();
        let fork = store.fork_session(&source.id, None, None).unwrap().unwrap();
        let hits = store.search_sessions("文件", 20).unwrap();
        assert_eq!(hits.len(), 4);
        let fork_messages = hits
            .iter()
            .filter(|hit| hit.session_id == fork.id)
            .map(|hit| hit.message_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(fork_messages.len(), 2);
        (source.id, fork.id, vec![first.id, second.id], fork_messages)
    };

    let connection = rusqlite::Connection::open(paths.db_file()).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_partial_fork BEFORE INSERT ON messages
             WHEN NEW.seq=2 AND NEW.session_id NOT IN ('{source_id}','{fork_id}')
             BEGIN SELECT RAISE(ABORT,'controlled fork failure'); END;"
        ))
        .unwrap();
    drop(connection);

    let store = Store::open(&paths.db_file()).unwrap();
    assert!(store.fork_session(&source_id, None, None).is_err());
    let hits = store.search_sessions("文件", 20).unwrap();
    assert_eq!(hits.len(), 4);
    for message_id in source_messages.into_iter().chain(fork_messages) {
        assert!(hits.iter().any(|hit| hit.message_id == message_id));
    }

    let connection = rusqlite::Connection::open(paths.db_file()).unwrap();
    let orphaned_index_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM messages_unicode_fts u
             WHERE NOT EXISTS (SELECT 1 FROM messages m WHERE m.rowid=u.rowid)",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(orphaned_index_rows, 0);
}

#[test]
fn audit_search_matches_unicode_case_without_dropping_marks() {
    let store = Store::open_in_memory().unwrap();
    store.audit("user", "search.note", "CAFÉ menu").unwrap();
    store.audit("user", "search.note", "เสื้อสีขาว").unwrap();

    let cafe = store.list_audit(Some("café"), None, 20).unwrap();
    assert_eq!(cafe.len(), 1);
    assert_eq!(cafe[0].detail, "CAFÉ menu");

    let thai = store.list_audit(Some("ข่าว"), None, 20).unwrap();
    assert!(thai.is_empty());
}

/// A nameless tool call stored before #540 was fixed is left out of the
/// context with its result, so strict endpoints accept the Session again (#541).
#[test]
fn context_drops_stored_tool_calls_without_a_name() {
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("nameless");
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap();
    let call = |id: &str, name: &str| json!({"id": id, "type": "function", "function": {"name": name, "arguments": "{}"}});
    let tool_result = |id: &str, name: &str| json!({"tool_call_id": id, "name": name}).to_string();
    store.append_message(&s.id, "user", "count lines").unwrap();
    // Phantom alongside a real call: only the phantom goes.
    let meta = json!({"tool_calls": [call("call_0", ""), call("call_1", "find_tools")]});
    store
        .append_message_meta(&s.id, "assistant", "", Some(meta.to_string()))
        .unwrap();
    store
        .append_message_meta(
            &s.id,
            "tool",
            "error: invalid JSON",
            Some(tool_result("call_0", "")),
        )
        .unwrap();
    store
        .append_message_meta(
            &s.id,
            "tool",
            "{\"tools\":[]}",
            Some(tool_result("call_1", "find_tools")),
        )
        .unwrap();
    // Only a phantom: the turn keeps its text, or goes when it has none.
    let only = json!({"tool_calls": [call("call_2", " ")]}).to_string();
    store
        .append_message_meta(&s.id, "assistant", "Let me look.", Some(only.clone()))
        .unwrap();
    store
        .append_message_meta(&s.id, "tool", "error", Some(tool_result("call_2", "")))
        .unwrap();
    let only = only.replace("call_2", "call_3");
    store
        .append_message_meta(&s.id, "assistant", "", Some(only))
        .unwrap();
    store
        .append_message_meta(&s.id, "tool", "error", Some(tool_result("call_3", "")))
        .unwrap();
    store.append_message(&s.id, "user", "again").unwrap();

    let (ctx, _, _) = build_context(&store, &s.id, "sys", 1_000_000, 0.8).unwrap();
    let roles: Vec<&str> = ctx.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        ["system", "user", "assistant", "tool", "assistant", "user"]
    );
    assert_eq!(ctx[2]["tool_calls"].as_array().unwrap().len(), 1);
    assert_eq!(ctx[2]["tool_calls"][0]["function"]["name"], "find_tools");
    assert_eq!(ctx[3]["tool_call_id"], "call_1");
    assert_eq!(
        ctx[4],
        json!({"role": "assistant", "content": "Let me look."})
    );
}

#[test]
fn context_has_one_leading_system_message() {
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("onesystem");
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap();
    store.append_message(&s.id, "user", "old").unwrap();
    store
        .append_message(&s.id, "assistant", "old reply")
        .unwrap();
    store.save_checkpoint(&s.id, "earlier summary", 2).unwrap();
    store.append_message(&s.id, "user", "question").unwrap();
    store
        .append_message(&s.id, "system-note", "joins the user turn")
        .unwrap();
    store.append_message(&s.id, "assistant", "answer").unwrap();
    store
        .append_message(&s.id, "system-note", "stands alone")
        .unwrap();
    let (ctx, _, _) = build_context(&store, &s.id, "sys", 1_000_000, 0.8).unwrap();
    let roles: Vec<&str> = ctx.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    let system = ctx[0]["content"].as_str().unwrap();
    let trusted_and_summary = system
        .strip_prefix(formless_core::provenance::HIERARCHY)
        .and_then(|rest| rest.strip_prefix("\n\n"))
        .expect("mandatory hierarchy leads the sole system message");
    let (trusted, summary) = trusted_and_summary
        .split_once("\n\nSummary of earlier conversation (data):\n")
        .expect("earlier summary is explicitly separated as data");
    assert_eq!(trusted, "sys", "custom system instructions remain exact");
    let summary: serde_json::Value = serde_json::from_str(summary).unwrap();
    assert_eq!(
        summary,
        json!({
            "formless_data":1,
            "source_tool":"conversation_summary",
            "origins":[],
            "note":formless_core::provenance::DATA_NOTE,
            "content":"earlier summary"
        })
    );
    assert_eq!(
        ctx[1]["content"],
        "question\n\n[harness note] joins the user turn"
    );
    assert_eq!(ctx[3]["content"], "[harness note] stands alone");
}

#[test]
fn provider_message_echoes_reasoning_only_on_tool_call_turns() {
    let _env = credential_env::EnvGuard::isolated();
    let meta = json!({
        "tool_calls": [{"id": "1", "type": "function", "function": {"name": "t", "arguments": "{}"}}],
        "reasoning_content": "because reasons"
    });
    let m = formless_protocol::MessageInfo {
        generated_by: None,
        id: "m1".into(),
        session_id: "s1".into(),
        seq: 1,
        role: "assistant".into(),
        content: String::new(),
        created_at: String::new(),
        meta: Some(meta.to_string()),
    };
    let v = provider_message(&m);
    assert_eq!(v["reasoning_content"], "because reasons");
    assert_eq!(v["content"], Value::Null, "reasoning is never the answer");
}

#[test]
fn fork_search_and_edit_message() {
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("forksearch");
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(Some("Orig".into())).unwrap();
    let m1 = store
        .append_message(&s.id, "user", "alpha needle one")
        .unwrap();
    store.append_message(&s.id, "assistant", "reply").unwrap();
    store
        .append_message(&s.id, "user", "second Needle")
        .unwrap();
    store.save_checkpoint(&s.id, "sum", 3).unwrap();

    // Fork a prefix; edits in fork/original stay independent.
    let fork = store.fork_session(&s.id, None, Some(2)).unwrap().unwrap();
    assert_eq!(fork.name, "Orig (fork)");
    assert_eq!(store.list_messages(&fork.id).unwrap().len(), 2);
    let full = store
        .fork_session(&s.id, Some("Copy".into()), None)
        .unwrap()
        .unwrap();
    assert_eq!(store.list_messages(&full.id).unwrap().len(), 3);
    assert!(store.fork_session("missing", None, None).unwrap().is_none());

    // Search across sessions, case-insensitive, with snippets.
    let hits = store.search_sessions("NEEDLE", 50).unwrap();
    assert!(hits.len() >= 3, "hits across orig+forks: {hits:?}");
    assert!(hits
        .iter()
        .all(|h| h.snippet.to_lowercase().contains("needle")));
    assert!(store.search_sessions("  ", 50).unwrap().is_empty());

    // Edit-and-retry: later messages and covering checkpoint are dropped.
    let edited = store
        .edit_message(&s.id, &m1.id, "edited text")
        .unwrap()
        .unwrap();
    assert_eq!(edited.content, "edited text");
    let after = store.list_messages(&s.id).unwrap();
    assert_eq!(after.len(), 1);
    assert!(store.latest_checkpoint(&s.id).unwrap().is_none());
    assert!(store.edit_message(&s.id, &m1.id, "x").unwrap().is_some());
    let a = store
        .append_message(&s.id, "assistant", "not editable")
        .unwrap();
    assert!(store.edit_message(&s.id, &a.id, "x").unwrap().is_none());
}

struct MockState {
    effort_rejections: AtomicUsize,
}

async fn mock_server() -> String {
    let state = Arc::new(MockState {
        effort_rejections: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/models", get(|| async { Json(json!({"data": [{"id": "mock-1"}, {"id": "mock-2"}]})) }))
        .route("/chat/completions", post({
            let state = state.clone();
            move |Json(body): Json<Value>| {
                let state = state.clone();
                async move {
                    if body.get("reasoning_effort").is_some() && state.effort_rejections.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "unknown parameter reasoning_effort"}))).into_response();
                    }
                    if body["tool_choice"].as_str() == Some("required") {
                        return Json(json!({
                            "choices": [{"message": {"tool_calls": [{"id": "1", "type": "function", "function": {"name": "probe_ping", "arguments": "{}"}}]}}]
                        })).into_response();
                    }
                    let user = body["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
                    if body["stream"].as_bool().unwrap_or(false) {
                        let sse = format!(
                            "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            json!({"choices": [{"delta": {"reasoning_content": "pondering "}}]}),
                            json!({"choices": [{"delta": {"content": "echo: "}}]}),
                            json!({"choices": [{"delta": {"content": user}}]})
                        );
                        return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], sse).into_response();
                    }
                    Json(json!({"choices": [{"message": {"content": format!("summary of: {user}")}}]})).into_response()
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn mock_cfg(base: &str) -> ProviderCfg {
    ProviderCfg {
        base_url: base.to_string(),
        model: "mock-1".into(),
        max_retries: 2,
        ..Default::default()
    }
}

#[tokio::test]
async fn provider_models_probe_stream_and_effort_retry() {
    let _env = credential_env::EnvGuard::isolated();
    let base = mock_server().await;
    let cfg = mock_cfg(&base);
    let p = Provider::new(cfg.clone(), "k".into()).unwrap();
    assert_eq!(p.list_models().await.unwrap(), vec!["mock-1", "mock-2"]);

    let probe = probe(&cfg, "k", "mock-1").await.unwrap();
    assert!(probe.tool_calling);

    let mut deltas = String::new();
    let mut reasoning_deltas = String::new();
    let out = p
        .chat_stream(
            &[serde_json::json!({"role": "user", "content": "hello"})],
            &[],
            "mock-1",
            Some("high"),
            &CancellationToken::new(),
            |kind, d| match kind {
                formless_core::provider::DeltaKind::Content => deltas.push_str(d),
                formless_core::provider::DeltaKind::Reasoning => reasoning_deltas.push_str(d),
                formless_core::provider::DeltaKind::Restart
                | formless_core::provider::DeltaKind::Bytes => {}
            },
        )
        .await
        .unwrap();
    assert_eq!(out.content, "echo: hello");
    assert_eq!(deltas, "echo: hello");
    assert_eq!(out.reasoning, "pondering ");
    assert_eq!(reasoning_deltas, "pondering ");
    assert!(
        out.dropped_reasoning_effort,
        "first request with effort must be rejected then retried without"
    );
}

/// Thinking-mode endpoints (e.g. DeepSeek reasoner) reject any forced
/// `tool_choice` with 400 "Thinking mode does not support this tool_choice",
/// yet still call tools when `tool_choice` is omitted. The probe must not
/// turn that into a false "no tool-calling" failure.
async fn mock_thinking_server() -> String {
    let app = Router::new()
        .route("/models", get(|| async { Json(json!({"data": [{"id": "seek-1"}]})) }))
        .route("/chat/completions", post(|Json(body): Json<Value>| async move {
            if body.get("tool_choice").is_some() {
                return (axum::http::StatusCode::BAD_REQUEST, Json(json!({
                    "error": {"message": "Thinking mode does not support this tool_choice", "type": "invalid_request_error", "param": null, "code": "invalid_request_error"}
                }))).into_response();
            }
            Json(json!({
                "choices": [{"message": {"tool_calls": [{"id": "1", "type": "function", "function": {"name": "probe_ping", "arguments": "{}"}}]}}]
            })).into_response()
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn probe_retries_without_tool_choice_when_thinking_mode_rejects_it() {
    let _env = credential_env::EnvGuard::isolated();
    let base = mock_thinking_server().await;
    let cfg = mock_cfg(&base);
    let probe = probe(&cfg, "k", "seek-1").await.unwrap();
    assert!(probe.tool_calling);
    assert_eq!(probe.models, vec!["seek-1"]);
}

#[tokio::test]
async fn probe_does_not_retry_on_other_provider_errors() {
    let _env = credential_env::EnvGuard::isolated();
    // A permanent 400 unrelated to tool_choice must fail fast, not be
    // masked by the thinking-mode fallback (which would succeed here).
    let app = Router::new()
        .route("/models", get(|| async { Json(json!({"data": [{"id": "seek-1"}]})) }))
        .route("/chat/completions", post(|Json(body): Json<Value>| async move {
            if body.get("tool_choice").is_some() {
                return (axum::http::StatusCode::BAD_REQUEST, Json(json!({
                    "error": {"message": "model not found", "type": "invalid_request_error"}
                }))).into_response();
            }
            Json(json!({
                "choices": [{"message": {"tool_calls": [{"id": "1", "type": "function", "function": {"name": "probe_ping", "arguments": "{}"}}]}}]
            })).into_response()
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cfg = mock_cfg(&format!("http://{addr}"));
    assert!(probe(&cfg, "k", "seek-1").await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_end_to_end_run_persists_and_two_clients_share() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::daemon::Daemon;
    use formless_protocol::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    _env.set("FORMLESS_API_KEY", "test-key");
    let base = mock_server().await;
    _env.set("FORMLESS_BASE_URL", &base);
    _env.set("FORMLESS_MODEL", "mock-1");
    let paths = temp_paths("daemon");
    let mut cfg = Config {
        provider: mock_cfg(&base),
        ..Default::default()
    };
    cfg.daemon.idle_grace_secs = 3600; // no idle-close during test
    cfg.save(&paths).unwrap();

    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    // Wait for socket.
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(formless_core::daemon::daemon_reachable(&paths));

    // Client A creates a session and runs a message to completion.
    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();

    // Create session.
    let session_id: String = {
        let env = RequestEnvelope {
            id: 1,
            protocol: PROTOCOL_VERSION,
            request: Request::CreateSession {
                name: Some("E2E".into()),
            },
        };
        write_half
            .write_all(serde_json::to_string(&env).unwrap().as_bytes())
            .await
            .unwrap();
        write_half.write_all(b"\n").await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let line = tokio::time::timeout_at(deadline, lines.next_line())
                .await
                .expect("CreateSession response did not arrive within 10 seconds")
                .unwrap()
                .expect("daemon closed before CreateSession response");
            if let Ok(ServerMessage::Response { id: 1, result, .. }) =
                serde_json::from_str::<ServerMessage>(&line)
            {
                break result.unwrap()["id"].as_str().unwrap().to_string();
            }
        }
    };
    // Send message; collect until RunFinished.
    let env = RequestEnvelope {
        id: 2,
        protocol: PROTOCOL_VERSION,
        request: Request::SendMessage {
            session_id: session_id.clone(),
            content: "ping".into(),
            mode: SendMode::Send,
            client_msg_id: None,
        },
    };
    write_half
        .write_all(serde_json::to_string(&env).unwrap().as_bytes())
        .await
        .unwrap();
    write_half.write_all(b"\n").await.unwrap();
    let mut deltas = String::new();
    let mut finished = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while let Some(line) = tokio::time::timeout_at(deadline, lines.next_line())
        .await
        .expect("RunFinished event did not arrive within 10 seconds")
        .unwrap()
    {
        if let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) {
            match ev {
                Event::MessageDelta { delta, .. } => deltas.push_str(&delta),
                Event::RunFinished { status, .. } => {
                    assert_eq!(status, "completed");
                    finished = true;
                    break;
                }
                Event::RunPaused { error, .. } => panic!("run paused: {error}"),
                _ => {}
            }
        }
    }
    assert!(finished);
    assert_eq!(deltas, "echo: ping");

    // Client B (second TUI equivalent) sees the same session + transcript.
    let stream_b = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (rb, mut wb) = tokio::io::split(stream_b);
    let mut lines_b = BufReader::new(rb).lines();
    let env = RequestEnvelope {
        id: 9,
        protocol: PROTOCOL_VERSION,
        request: Request::GetSession { id: session_id },
    };
    wb.write_all(serde_json::to_string(&env).unwrap().as_bytes())
        .await
        .unwrap();
    wb.write_all(b"\n").await.unwrap();
    let mut got = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while let Some(line) = tokio::time::timeout_at(deadline, lines_b.next_line())
        .await
        .expect("GetSession response did not arrive within 10 seconds")
        .unwrap()
    {
        if let Ok(ServerMessage::Response { id: 9, result, .. }) =
            serde_json::from_str::<ServerMessage>(&line)
        {
            let msgs = result.unwrap()["messages"].as_array().cloned().unwrap();
            assert_eq!(msgs.len(), 2);
            assert_eq!(msgs[1]["content"], "echo: ping");
            got = true;
            break;
        }
    }
    assert!(got);
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_idle_closes_after_grace() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::daemon::Daemon;
    let paths = temp_paths("idle");
    let mut cfg = Config::default();
    cfg.daemon.idle_grace_secs = 1;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    // Daemon with no clients and no runs must shut itself down (SPEC §9.1).
    let done = tokio::time::timeout(std::time::Duration::from_secs(15), server).await;
    assert!(done.is_ok(), "daemon did not idle-close");
    assert!(!paths.socket_path().exists(), "socket must be cleaned up");
}

#[test]
fn migrating_an_older_database_takes_a_backup_first() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::storage::SCHEMA_VERSION;
    let paths = temp_paths("migrate");
    let db = paths.db_file();
    {
        let old = historical::create(&db, 3);
        old.execute_batch(
            "INSERT INTO sessions(id,name,created_at,updated_at)
             VALUES('historical-session','keep me','2020-01-01','2020-01-01');",
        )
        .unwrap();
    }
    let user_version = |c: &rusqlite::Connection| -> i64 {
        c.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    };
    let store = Store::open(&db).unwrap();
    assert_eq!(store.list_sessions().unwrap()[0].name, "keep me");
    let backups: Vec<_> = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains(".db.v3-"))
        .collect();
    assert_eq!(backups.len(), 1, "one backup of the v3 database");
    let backup = rusqlite::Connection::open(&backups[0]).unwrap();
    assert_eq!(user_version(&backup), 3);
    let n: i64 = backup
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "backup holds the data");
    assert_eq!(
        user_version(&rusqlite::Connection::open(&db).unwrap()),
        SCHEMA_VERSION
    );

    // Up to date: no new backup. Newer than this build: refuse.
    drop(store);
    Store::open(&db).unwrap();
    let count = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .path()
                .to_string_lossy()
                .contains(".bak")
        })
        .count();
    assert_eq!(count, 1);
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(&format!("PRAGMA user_version={};", SCHEMA_VERSION + 1))
        .unwrap();
    let err = Store::open(&db).err().unwrap().to_string();
    assert!(err.contains("newer than this build"), "{err}");
}

#[test]
fn queued_messages_survive_reopening_the_store() {
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("queue");
    let id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(None).unwrap();
        store.enqueue_message(&s.id, "first").unwrap();
        store.enqueue_message(&s.id, "second").unwrap();
        s.id
    };
    // A daemon restart reopens the database.
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(
        store.list_queued_messages(&id).unwrap(),
        ["first", "second"]
    );
    assert_eq!(
        store.pop_queued_message(&id).unwrap().as_deref(),
        Some("first")
    );
    assert_eq!(store.list_queued_messages(&id).unwrap(), ["second"]);
    store.clear_queued_messages(&id).unwrap();
    assert_eq!(store.pop_queued_message(&id).unwrap(), None);
}

#[test]
fn context_cache_matches_a_full_build_through_every_change() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::storage::ContextCache;
    let paths = temp_paths("ctxcache");
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap();
    let mut cache = ContextCache::default();
    let check = |cache: &mut ContextCache, prompt: &str, label: &str| {
        let cached = cache.build(&store, &s.id, prompt, 200, 0.8).unwrap();
        let full = build_context(&store, &s.id, prompt, 200, 0.8).unwrap();
        assert_eq!(cached, full, "after {label}");
    };
    check(&mut cache, "sys", "nothing");
    let first = store.append_message(&s.id, "user", "hello").unwrap();
    check(&mut cache, "sys", "a user message");
    store
        .append_message(&s.id, "system-note", "merged")
        .unwrap();
    check(&mut cache, "sys", "a note joining the user turn");
    for i in 0..6 {
        store
            .append_message(&s.id, "assistant", &format!("reply {i} {}", "x".repeat(80)))
            .unwrap();
        store
            .append_message(&s.id, "user", &format!("more {i}"))
            .unwrap();
        check(&mut cache, "sys", "growth past the compaction trigger");
    }
    let (_, needs, up_to) = cache.build(&store, &s.id, "sys", 200, 0.8).unwrap();
    assert!(needs);
    store.save_checkpoint(&s.id, "the summary", up_to).unwrap();
    check(&mut cache, "sys", "a checkpoint");
    check(&mut cache, "new prompt", "a system prompt change");
    store.edit_message(&s.id, &first.id, "edited").unwrap();
    check(&mut cache, "new prompt", "an edit that truncates");
    store
        .append_message(&s.id, "assistant", "after edit")
        .unwrap();
    check(&mut cache, "new prompt", "appending after the edit");
}

#[test]
fn reader_connections_see_writes_and_cannot_write() {
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("reader");
    let store = Store::open(&paths.db_file()).unwrap();
    let reader = Store::open_reader(&paths.db_file()).unwrap();
    let s = store.create_session(Some("seen".into())).unwrap();
    store.append_message(&s.id, "user", "hi").unwrap();
    assert_eq!(reader.list_sessions().unwrap()[0].name, "seen");
    assert_eq!(reader.list_messages(&s.id).unwrap().len(), 1);
    assert!(
        reader.create_session(None).is_err(),
        "query_only connections refuse writes"
    );
}

#[test]
fn concurrent_appends_get_distinct_ordered_seqs() {
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("seqs");
    let session = Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    // Two connections (as a second writer would have) appending at once.
    let writers: Vec<_> = (0..2)
        .map(|w| {
            let (db, session) = (paths.db_file(), session.clone());
            std::thread::spawn(move || {
                let store = Store::open(&db).unwrap();
                for i in 0..50 {
                    store
                        .append_message(&session, "user", &format!("{w}-{i}"))
                        .unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    let seqs: Vec<i64> = Store::open(&paths.db_file())
        .unwrap()
        .list_messages(&session)
        .unwrap()
        .iter()
        .map(|m| m.seq)
        .collect();
    assert_eq!(seqs, (1..=100).collect::<Vec<i64>>());
}

#[test]
fn real_prompt_size_drives_compaction_until_the_next_checkpoint() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::provider::Usage;
    use formless_core::storage::ContextCache;
    let paths = temp_paths("usage");
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap();
    for i in 0..8 {
        store
            .append_message(&s.id, "user", &format!("short {i}"))
            .unwrap();
    }
    let mut cache = ContextCache::default();
    let built = cache.build(&store, &s.id, "sys", 1000, 0.8).unwrap();
    assert!(!built.1, "the estimate alone is far under the limit");
    let usage = Usage {
        prompt_tokens: 950,
        completion_tokens: 10,
        ..Default::default()
    };
    store.record_usage(&s.id, "run1", "m", &usage).unwrap();
    store.record_usage(&s.id, "run1", "m", &usage).unwrap();
    assert_eq!(store.run_usage("run1").unwrap().prompt_tokens, 1900);
    let known = store.last_prompt_tokens(&s.id).unwrap();
    assert_eq!(known, Some(950));
    let (_, needs, up_to) = cache.with_known_tokens(built, known, 1000, 0.8);
    assert!(needs, "the provider says the context is nearly full");
    // After compaction the old count no longer applies.
    std::thread::sleep(std::time::Duration::from_millis(5));
    store.save_checkpoint(&s.id, "summary", up_to).unwrap();
    assert_eq!(store.last_prompt_tokens(&s.id).unwrap(), None);
}

#[test]
fn compaction_cuts_only_at_turn_boundaries() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::storage::ContextCache;
    let paths = temp_paths("turncut");
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap();
    let big = "x".repeat(400);
    let mut user_seqs = Vec::new();
    for turn in 0..4 {
        user_seqs.push(
            store
                .append_message(&s.id, "user", &format!("q{turn} {big}"))
                .unwrap()
                .seq,
        );
        let calls = serde_json::json!({"tool_calls": [{"id": format!("c{turn}"), "type": "function",
            "function": {"name": "t", "arguments": "{}"}}]});
        store
            .append_message_meta(&s.id, "assistant", "", Some(calls.to_string()))
            .unwrap();
        store
            .append_message_meta(
                &s.id,
                "tool",
                &big,
                Some(serde_json::json!({"tool_call_id": format!("c{turn}")}).to_string()),
            )
            .unwrap();
        store.append_message(&s.id, "assistant", &big).unwrap();
    }
    let (_, needs, cut) = ContextCache::default()
        .build(&store, &s.id, "sys", 500, 0.8)
        .unwrap();
    assert!(needs);
    // The cut is the last message of a turn: the next one is a user turn.
    assert!(
        user_seqs.contains(&(cut + 1)),
        "cut at {cut}, turns start at {user_seqs:?}"
    );
}

/// The audit view shows decisions by default; per-call approvals-off
/// records are opt-in and capped (#848).
#[test]
fn audit_hides_and_caps_unasked_records() {
    let _env = credential_env::EnvGuard::isolated();
    let store = formless_core::Store::open_in_memory().unwrap();
    store.audit("user", "tool.approve", "fetch: net").unwrap();
    for i in 0..(formless_core::storage::UNASKED_KEEP + 5) {
        store
            .audit("approvals-off", "tool.run_unasked", &format!("t{i}: net"))
            .unwrap();
    }
    let default = store.list_audit(None, None, 500).unwrap();
    assert_eq!(default.len(), 1, "only the decision");
    assert_eq!(default[0].action, "tool.approve");
    let unasked = store.list_audit(Some("run_unasked"), None, 500).unwrap();
    assert_eq!(unasked.len(), 500);
    let all = store.list_audit(Some("*"), None, 500).unwrap();
    assert_eq!(all.len(), 500);
    let without = store.list_audit(Some("-approve"), None, 500).unwrap();
    assert!(without.iter().all(|e| e.action != "tool.approve"));
    let kept: i64 = store
        .recent_audit(Some("tool.run_unasked"), 100_000)
        .unwrap()
        .len() as i64;
    assert_eq!(kept, formless_core::storage::UNASKED_KEEP, "capped");
}
