//! Scheduled jobs (SPEC §9.7): cron, one-shot times, approval, running.

#[path = "support/env.rs"]
mod credential_env;

use chrono::{Local, TimeZone};
use formless_core::jobs::{parse_at, Cron};
use formless_core::{Paths, Store};
use std::sync::Arc;

fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<Local> {
    Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

#[test]
fn cron_next_occurrences() {
    let daily3 = Cron::parse("0 3 * * *").unwrap();
    assert_eq!(
        daily3.next_after(local(2026, 10, 21, 2, 59)),
        Some(local(2026, 10, 21, 3, 0))
    );
    assert_eq!(
        daily3.next_after(local(2026, 10, 21, 3, 0)),
        Some(local(2026, 10, 22, 3, 0))
    );
    let every15 = Cron::parse("*/15 * * * *").unwrap();
    assert_eq!(
        every15.next_after(local(2026, 10, 21, 10, 1)),
        Some(local(2026, 10, 21, 10, 15))
    );
    // 2026-10-21 is a Wednesday; next Monday 09:30 is the 26th.
    let monday = Cron::parse("30 9 * * 1").unwrap();
    assert_eq!(
        monday.next_after(local(2026, 10, 21, 12, 0)),
        Some(local(2026, 10, 26, 9, 30))
    );
    let weekdays = Cron::parse("0 18 * * 1-5").unwrap();
    assert_eq!(
        weekdays.next_after(local(2026, 10, 23, 19, 0)),
        Some(local(2026, 10, 26, 18, 0))
    );
    assert_eq!(
        Cron::parse("@daily").unwrap(),
        Cron::parse("0 0 * * *").unwrap()
    );
    assert_eq!(
        Cron::parse("0 0 * * 7").unwrap(),
        Cron::parse("0 0 * * 0").unwrap()
    );
    for bad in [
        "* * * *",
        "60 * * * *",
        "0 24 * * *",
        "*/0 * * * *",
        "a b c d e",
    ] {
        assert!(Cron::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn one_shot_times() {
    let now = local(2026, 10, 21, 10, 0);
    assert_eq!(
        parse_at("in 30m", now).unwrap(),
        local(2026, 10, 21, 10, 30)
    );
    assert_eq!(parse_at("2h", now).unwrap(), local(2026, 10, 21, 12, 0));
    assert_eq!(parse_at("1 day", now).unwrap(), local(2026, 10, 22, 10, 0));
    assert_eq!(
        parse_at("2026-10-22 08:15", now).unwrap(),
        local(2026, 10, 22, 8, 15)
    );
    assert!(parse_at("tomorrow-ish", now).is_err());
}

#[test]
fn job_store_due_and_lifecycle() {
    let store = Store::open_in_memory().unwrap();
    let past = (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let due = store
        .create_job("check build", None, None, &past, true, "user")
        .unwrap();
    store
        .create_job("later", None, None, &future, true, "user")
        .unwrap();
    let unapproved = store
        .create_job("nightly", None, Some("0 3 * * *"), &past, false, "agent")
        .unwrap();
    let ids: Vec<String> = store
        .due_jobs(chrono::Utc::now())
        .unwrap()
        .into_iter()
        .map(|j| j.id)
        .collect();
    assert_eq!(ids, vec![due.id.clone()], "only enabled, approved and due");
    assert!(store.has_active_jobs().unwrap());
    store.job_ran(&due.id, "started", None).unwrap();
    let after = store.get_job(&due.id).unwrap().unwrap();
    assert!(!after.enabled && after.next_run.is_none(), "one-shot ends");
    store.set_job_enabled(&unapproved.id, true).unwrap();
    assert!(
        store.get_job(&unapproved.id).unwrap().unwrap().approved,
        "enabling approves"
    );
    assert!(store.delete_job(&unapproved.id).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_run_and_the_scheduler() {
    let env = credential_env::EnvGuard::isolated();
    let (endpoint, requested, release, server) = held_provider(false).await;
    let (paths, store, daemon) = job_daemon(&env, "jobs", &endpoint);
    let sid = store.create_session(None).unwrap().id;

    // Recurring with nobody to approve: saved, waiting for the user.
    let rec: serde_json::Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &sid,
                "schedule_run",
                serde_json::json!({"prompt": "nightly cleanup", "cron": "0 3 * * *"}),
            )
            .await,
    )
    .unwrap();
    assert!(
        rec["status"].as_str().unwrap().starts_with("awaiting"),
        "{rec}"
    );
    // One-shot: scheduled at once.
    let once: serde_json::Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &sid,
                "schedule_run",
                serde_json::json!({"prompt": "re-check the build", "at": "in 1m"}),
            )
            .await,
    )
    .unwrap();
    assert_eq!(once["status"], "scheduled");
    let bad = daemon
        .call_tool_direct(
            &sid,
            "schedule_run",
            serde_json::json!({"prompt": "x", "cron": "61 * * * *"}),
        )
        .await;
    assert!(bad.starts_with("error:"), "{bad}");

    // A due one-shot job starts a Run in a new Session and ends.
    let past = (chrono::Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
    let job = store
        .create_job("say hi", None, None, &past, true, "user")
        .unwrap();
    daemon.run_due_jobs().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), requested)
        .await
        .expect("scheduled Run never requested its mock provider")
        .unwrap();
    let ran = store.get_job(&job.id).unwrap().unwrap();
    assert!(
        ran.last_status.starts_with("started in session"),
        "{}",
        ran.last_status
    );
    assert!(!ran.enabled);
    assert!(store
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.name == "Job: say hi"));
    // The response stays held until the transient started/disabled/Session
    // assertions have observed a genuinely in-flight Run.
    release.notify_one();
    let runs = completed_job(&store, &job.id, false).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "completed", "{runs:?}");
    drain_daemon(daemon).await;
    stop_provider(server).await;
    drop(store);
    std::fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap();
}

/// The caller owns credential isolation through all awaited Run/delivery work.
/// Each fixture uses an explicit local endpoint, never the default provider.
fn job_daemon(
    env: &credential_env::EnvGuard,
    tag: &str,
    endpoint: &str,
) -> (Paths, Store, Arc<formless_core::daemon::Daemon>) {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    let mut cfg = formless_core::Config::default();
    env.set("FORMLESS_API_KEY", "mock-jobs-key");
    env.set("FORMLESS_BASE_URL", endpoint);
    env.set("FORMLESS_MODEL", "m");
    cfg.provider.base_url = endpoint.into();
    cfg.provider.model = "m".into();
    cfg.provider.max_retries = 0;
    cfg.provider.fallback_models.clear();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    (paths, store, daemon)
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_english_schedules_run_now_history_and_the_keep_alive_warning() {
    use formless_protocol::Request;
    let env = credential_env::EnvGuard::isolated();
    let (endpoint, requested, release, server) = held_provider(true).await;
    let (paths, store, daemon) = job_daemon(&env, "jobs2", &endpoint);
    let preview = daemon
        .handle(Request::PreviewSchedule {
            schedule: "every weekday at 9am".into(),
        })
        .await
        .unwrap();
    assert_eq!(preview["cron"], "0 9 * * 1-5");
    assert_eq!(preview["next"].as_array().unwrap().len(), 3);

    let out = std::env::temp_dir().join(format!("fl-deliver-{}.md", uuid::Uuid::new_v4()));
    let job = daemon
        .handle(Request::AddJob {
            prompt: "say hi".into(),
            session_id: None,
            cron: Some("every 2 hours".into()),
            at: None,
            overlap: None,
            deliver: Some(serde_json::json!({"file": out.display().to_string()})),
        })
        .await
        .unwrap();
    assert_eq!(job["cron"], "0 */2 * * *");
    assert_eq!(job["overlap"], "skip");
    assert!(job["warning"]
        .as_str()
        .unwrap()
        .contains("keep_alive_for_jobs"));
    let id = job["id"].as_str().unwrap().to_string();

    // Run now: a firing in the history; the schedule is unchanged.
    let before = store.get_job(&id).unwrap().unwrap().next_run;
    let r = daemon
        .handle(Request::RunJobNow { id: id.clone() })
        .await
        .unwrap();
    assert!(r["status"].as_str().unwrap().starts_with("started"), "{r}");
    assert_eq!(store.get_job(&id).unwrap().unwrap().next_run, before);
    tokio::time::timeout(std::time::Duration::from_secs(10), requested)
        .await
        .expect("manual Run never requested its mock provider")
        .unwrap();
    // Firing again while it still runs is skipped (overlap = skip).
    let again = daemon
        .handle(Request::RunJobNow { id: id.clone() })
        .await
        .unwrap();
    assert!(
        again["status"].as_str().unwrap().starts_with("skipped"),
        "{again}"
    );
    // Only now return an intentional local 503: failure/history/delivery
    // assertions remain separate from the in-flight overlap assertion.
    release.notify_one();
    let runs = completed_job(&store, &id, true).await;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs.iter().any(|r| r.status == "skipped"));
    let fired = runs.iter().find(|r| r.status != "skipped").unwrap();
    assert_eq!(fired.trigger, "manual");
    assert_ne!(fired.status, "running", "{fired:?}");
    assert_eq!(fired.status, "paused", "the intentional 503 pauses the Run");
    assert!(fired.detail.contains("appended"), "{fired:?}");
    assert!(std::fs::read_to_string(&out).unwrap().contains("say hi"));
    std::fs::remove_file(out).unwrap();
    drain_daemon(daemon).await;
    stop_provider(server).await;
    drop(store);
    std::fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap();
}

/// A request-arrival handshake proves that the Run reached this local provider.
/// The response is held until the test releases it after its in-flight checks.
async fn held_provider(
    fail: bool,
) -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{response::IntoResponse, routing::post, Router};
    let (requested, receiver) = tokio::sync::oneshot::channel();
    let requested = Arc::new(std::sync::Mutex::new(Some(requested)));
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    let app = Router::new().route("/chat/completions", post(move || {
        let requested = requested.clone();
        let gate = gate.clone();
        async move {
            requested.lock().unwrap().take().expect("unexpected provider retry").send(()).unwrap();
            gate.notified().await;
            if fail {
                return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "intentional mock failure").into_response();
            }
            let event = serde_json::json!({"choices":[{"delta":{"content":"job completed"},"finish_reason":"stop"}]});
            axum::response::Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(format!("data: {event}\n\ndata: [DONE]\n\n")))
                .unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (endpoint, receiver, release, server)
}

async fn completed_job(
    store: &Store,
    id: &str,
    delivered: bool,
) -> Vec<formless_protocol::JobRunInfo> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let runs = store.job_runs(id, 10).unwrap();
            if runs.iter().any(|r| {
                r.status != "running"
                    && r.status != "skipped"
                    && (!delivered || r.detail.contains("appended"))
            }) {
                return runs;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("job did not reach its terminal history/delivery state")
}

async fn drain_daemon(daemon: Arc<formless_core::daemon::Daemon>) {
    let pending = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while pending.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("job tasks outlived credential isolation");
}

async fn stop_provider(server: tokio::task::JoinHandle<()>) {
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock provider failed: {error}");
    }
}

#[test]
fn missed_firings_are_counted() {
    let due = local(2026, 10, 21, 3, 0);
    let now = local(2026, 10, 21, 9, 30);
    assert_eq!(formless_core::jobs::missed_since("0 * * * *", due, now), 6);
    assert_eq!(formless_core::jobs::missed_since("0 3 * * *", due, now), 0);
}
