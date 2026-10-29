//! Pushing models to build Tools instead of refusing (#237, SPEC §5.2).

use axum::routing::post;
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

/// A provider that answers with each of `answers` in turn (the last one
/// repeats), recording request bodies.
async fn mock(answers: Vec<&'static str>) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                let answers = answers.clone();
                async move {
                    let n = {
                        let mut s = seen.lock().unwrap();
                        s.push(body);
                        s.len()
                    };
                    let answer = answers[(n - 1).min(answers.len() - 1)];
                    let sse = format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        serde_json::json!({"choices": [{"delta": {"content": answer}}]})
                    );
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

async fn run_once(model: &str, nudge: bool, answers: Vec<&'static str>) -> (Seen, Store, String) {
    std::env::set_var("FORMLESS_API_KEY", "k");
    let (base, seen) = mock(answers).await;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-nudge-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-nudge-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = model.into();
    cfg.provider.max_retries = 1;
    cfg.provider.tool_use_nudge = nudge;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    daemon
        .handle(Request::SendMessage {
            session_id: sid.clone(),
            content: "count the TODOs in ~/project".into(),
            mode: formless_protocol::SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap();
    for _ in 0..200 {
        if store
            .last_run(&sid)
            .unwrap()
            .is_some_and(|(_, s)| s != "running")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    (seen, store, sid)
}

/// The system prompt's text, sent as a string or (with prompt-cache
/// breakpoints, for Claude models) as content parts.
fn system(body: &serde_json::Value) -> String {
    let content = &body["messages"][0]["content"];
    content
        .as_str()
        .or_else(|| content[0]["text"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_is_nudged_once_towards_building_a_tool() {
    let (seen, store, sid) = run_once(
        "gpt-4.1",
        true,
        vec![
            "I can't access files on your computer.",
            "I still can't access it.",
        ],
    )
    .await;
    let bodies = seen.lock().unwrap().clone();
    // The refusal went back with a nudge; the second refusal is accepted.
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(bodies[1].to_string().contains("create_tool, test it"));
    assert!(system(&bodies[0]).contains("Tool use rule:"));
    let msgs = store.list_messages(&sid).unwrap();
    let assistant: Vec<&str> = msgs
        .iter()
        .filter(|m| m.role == "assistant")
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(assistant, vec!["I still can't access it."]);
}

#[tokio::test(flavor = "multi_thread")]
async fn other_families_and_the_off_switch_are_left_alone() {
    let (seen, _, _) = run_once("claude-sonnet-4", true, vec!["Here you go."]).await;
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1);
    let sys = system(&bodies[0]);
    assert!(!sys.contains("Tool use rule:"));
    assert!(sys.contains("never ask the user to do a step a Tool could do"));

    let (seen, _, _) = run_once("gpt-4.1", false, vec!["I can't access files."]).await;
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "no nudge when off");
    assert!(!system(&bodies[0]).contains("Tool use rule:"));
}
