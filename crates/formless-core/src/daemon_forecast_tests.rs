use super::*;
use std::time::Duration;
struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn setup(env: &crate::credential_env::EnvGuard) -> (Fixture, Paths, Arc<Daemon>, String, Config) {
    env.set("FORMLESS_API_KEY", "forecast-fixture");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.remove("FORMLESS_SECURE");
    // hyper-util matches literal IPs against IP entries, not hostname wildcards.
    env.set("NO_PROXY", "127.0.0.1,localhost");
    env.set("no_proxy", "127.0.0.1,localhost");
    let root =
        Fixture(std::env::temp_dir().join(format!("fl-forecast-gate-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "model".into();
    cfg.provider
        .model_context_tokens
        .insert("model".into(), 100_000);
    cfg.provider.prices.insert(
        "model".into(),
        crate::config::ModelPrice {
            input: 100.0,
            output: 100.0,
            ..Default::default()
        },
    );
    cfg.limits.forecast_confirm_usd = Some(0.0);
    cfg.provider.max_retries = 0;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    let sid = st.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let input = ForecastInput {
        session_id: Some(sid.clone()),
        draft: "task".into(),
        ..Default::default()
    };
    let f = crate::forecast::estimate_store(&st, &cfg, &input).unwrap();
    let run = st.create_run(&sid).unwrap();
    st.record_forecast(&run, &f).unwrap();
    st.bump_run_steps(&run).unwrap();
    st.record_forecast_request(&run, "run").unwrap();
    st.record_usage_full(
        &sid,
        &run,
        "default",
        "model",
        &crate::provider::Usage {
            prompt_tokens: 100,
            completion_tokens: 100,
            reported: 3,
            ..Default::default()
        },
        "run",
    )
    .unwrap();
    st.set_run_status(&run, "completed").unwrap();
    drop(st);
    (root, paths, daemon, sid, cfg)
}
#[tokio::test]
async fn bypass_stale_forecast_wrong_connection_and_expired_consent_fail_before_writes() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, _cfg) = setup(&env);
    let st = Store::open(&paths.db_file()).unwrap();
    let before = st.run_metrics(100).unwrap().len();
    let request = || Request::SendMessage {
        session_id: sid.clone(),
        content: "task".into(),
        mode: SendMode::Send,
        client_msg_id: Some("denied-intent".into()),
    };
    let result = CLIENT
        .scope((1, "fixture".into()), d.handle(request()))
        .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("forecast confirmation required"));
    assert!(st.list_messages(&sid).unwrap().is_empty());
    assert_eq!(st.run_metrics(100).unwrap().len(), before);
    let input = ForecastInput {
        session_id: Some(sid.clone()),
        draft: "task".into(),
        ..Default::default()
    };
    let f: RunForecast = serde_json::from_value(
        CLIENT
            .scope(
                (1, "fixture".into()),
                d.handle(Request::ForecastRun {
                    for_send: false,
                    input: input.clone(),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(f.confirmation_required);
    let id = f.confirmation_id.unwrap();
    assert!(CLIENT
        .scope(
            (2, "other".into()),
            d.handle(Request::ConfirmForecast { id: id.clone() })
        )
        .await
        .is_err());
    CLIENT
        .scope(
            (1, "fixture".into()),
            d.handle(Request::ConfirmForecast { id: id.clone() }),
        )
        .await
        .unwrap();
    // A changed attachment is a changed confirmed input. Nothing is consumed.
    let path = paths.data_dir.join("held.txt");
    std::fs::write(&path, b"held").unwrap();
    st.add_pending_attachment(
        &sid,
        &json!({"id":"held","path":path,"name":"held.txt","mime":"text/plain","bytes":4}),
    )
    .unwrap();
    let fresh_intent = Request::SendMessage {
        client_msg_id: Some("new-intent".into()),
        session_id: sid.clone(),
        content: "task".into(),
        mode: SendMode::Send,
    };
    assert!(CLIENT
        .scope((1, "fixture".into()), d.handle(fresh_intent))
        .await
        .is_err());
    assert_eq!(st.pending_attachments(&sid).unwrap().len(), 1);
    assert!(st.list_messages(&sid).unwrap().is_empty());
    assert_eq!(st.run_metrics(100).unwrap().len(), before);
    // Confirming an already stale preview is rejected too.
    assert!(CLIENT
        .scope(
            (1, "fixture".into()),
            d.handle(Request::ConfirmForecast { id })
        )
        .await
        .is_err());
    let f: RunForecast = serde_json::from_value(
        CLIENT
            .scope(
                (1, "fixture".into()),
                d.handle(Request::ForecastRun {
                    for_send: false,
                    input,
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let id = f.confirmation_id.unwrap();
    d.forecast_consents.locked().get_mut(&id).unwrap().at =
        std::time::Instant::now() - std::time::Duration::from_secs(301);
    assert!(CLIENT
        .scope(
            (1, "fixture".into()),
            d.handle(Request::ConfirmForecast { id })
        )
        .await
        .is_err());
    assert_eq!(st.recorded_forecasts().unwrap().len(), 1);
}
#[tokio::test]
async fn exact_consent_is_one_shot_and_cached_denial_is_never_replayed_as_approval() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, _paths, d, sid, cfg) = setup(&env);
    let input = ForecastInput {
        session_id: Some(sid.clone()),
        draft: "task".into(),
        ..Default::default()
    };
    let denied = || Request::SendMessage {
        session_id: sid.clone(),
        content: "task".into(),
        mode: SendMode::Send,
        client_msg_id: Some("same-failed-key".into()),
    };
    assert!(CLIENT
        .scope((9, "fixture".into()), d.handle(denied()))
        .await
        .is_err());
    let f: RunForecast = serde_json::from_value(
        CLIENT
            .scope(
                (9, "fixture".into()),
                d.handle(Request::ForecastRun {
                    for_send: false,
                    input: input.clone(),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let id = f.confirmation_id.unwrap();
    CLIENT
        .scope(
            (9, "fixture".into()),
            d.handle(Request::ConfirmForecast { id }),
        )
        .await
        .unwrap();
    assert!(
        CLIENT
            .scope((9, "fixture".into()), d.handle(denied()))
            .await
            .is_err(),
        "cached refusal cannot become a successful send"
    );
    CLIENT
        .scope((9, "fixture".into()), async {
            d.with_store(|st| {
                d.consented_forecast(st, &cfg, &input).unwrap();
                assert!(d.consented_forecast(st, &cfg, &input).is_err());
                Ok(())
            })
            .unwrap();
        })
        .await;
    assert!(!d.runs.locked().contains_key(&sid));
}

#[tokio::test]
async fn admission_persists_immutable_snapshot_before_any_provider_work() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, cfg) = setup(&env);
    let st = Store::open(&paths.db_file()).unwrap();
    let forecast = d
        .with_reader(|st| {
            crate::forecast::estimate_store(
                st,
                &cfg,
                &ForecastInput {
                    session_id: Some(sid.clone()),
                    draft: "admitted task".into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    d.start_run_forecast(sid.clone(), Some(forecast.clone()))
        .unwrap();
    let run = d.runs.locked().get(&sid).unwrap().run_id.clone();
    let recorded = st.recorded_forecasts().unwrap();
    let (_, stored) = recorded.iter().find(|(id, _)| id == &run).unwrap();
    assert_eq!(stored, &forecast);
    // Cancel before yielding to the spawned Run, then wait for its teardown
    // while credential isolation remains owned by this test.
    d.runs.locked().get(&sid).unwrap().cancel.cancel();
    let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while d.runs.locked().contains_key(&sid) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(settled.is_ok(), "cancelled fixture Run did not settle");
    assert!(st.record_forecast(&run, &forecast).is_err());
    drop(st);
    drop(d);
    let reopened = Store::open(&paths.db_file()).unwrap();
    assert!(reopened
        .recorded_forecasts()
        .unwrap()
        .iter()
        .any(|(id, f)| id == &run && f == &forecast));
    let rows = crate::forecast::metrics(&reopened, &cfg).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["run_id"] == run)
        .unwrap();
    assert_eq!(row["calibration_eligible"], false);
}

async fn cancel_fixture_run(d: &Arc<Daemon>, sid: &str) {
    if let Some(ctl) = d.runs.locked().get(sid) {
        ctl.cancel.cancel();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while d.runs.locked().contains_key(sid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("forecast fixture Run teardown");
}

#[tokio::test]
async fn prospective_edit_retry_and_resume_require_consent_before_any_mutation() {
    let env = crate::credential_env::EnvGuard::isolated();
    for operation in 0..3 {
        let (_root, paths, d, sid, _cfg) = setup(&env);
        let st = Store::open(&paths.db_file()).unwrap();
        let user = st.append_message(&sid, "user", "original").unwrap();
        st.append_message(&sid, "assistant", "answer").unwrap();
        if operation == 2 {
            st.append_message_meta(&sid,"assistant","",Some(json!({"tool_calls":[{"id":"unfinished","type":"function","function":{"name":"never_replay","arguments":"{}"}}]}).to_string())).unwrap();
        }
        let paused = st.create_run(&sid).unwrap();
        st.set_run_status(&paused, "paused").unwrap();
        if operation == 2 {
            st.set_run_error(&paused, "budget: fixture").unwrap();
        }
        let before_messages = serde_json::to_value(st.list_messages(&sid).unwrap()).unwrap();
        let before_sessions = st.list_sessions().unwrap().len();
        let before_runs = st.run_metrics(100).unwrap().len();
        let request = match operation {
            0 => Request::EditMessage {
                session_id: sid.clone(),
                message_id: user.id.clone(),
                content: "edited".into(),
            },
            1 => Request::RetryRun {
                session_id: sid.clone(),
                expected_run_id: None,
                model: None,
                keep_original: true,
            },
            _ => Request::ResumeRun {
                session_id: sid.clone(),
            },
        };
        let refused = CLIENT
            .scope((71, "fixture".into()), d.handle(request.clone()))
            .await;
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("forecast confirmation required"),
            "operation {operation}"
        );
        assert_eq!(
            serde_json::to_value(st.list_messages(&sid).unwrap()).unwrap(),
            before_messages
        );
        assert_eq!(
            st.list_sessions().unwrap().len(),
            before_sessions,
            "denied retry cannot fork"
        );
        assert_eq!(st.run_metrics(100).unwrap().len(), before_runs);
        assert_eq!(st.last_run(&sid).unwrap().unwrap().1, "paused");
        assert!(!d.budget_waived.locked().contains(&sid));
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: if operation == 0 {
                "edited".into()
            } else {
                String::new()
            },
            edit_message_id: (operation == 0).then(|| user.id.clone()),
            retry: operation == 1,
            resume: operation == 2,
            ..Default::default()
        };
        let forecast: RunForecast = serde_json::from_value(
            CLIENT
                .scope(
                    (71, "fixture".into()),
                    d.handle(Request::ForecastRun {
                        input,
                        for_send: true,
                    }),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(forecast.confirmation_required);
        CLIENT
            .scope(
                (71, "fixture".into()),
                d.handle(Request::ConfirmForecast {
                    id: forecast.confirmation_id.clone().unwrap(),
                }),
            )
            .await
            .unwrap();
        CLIENT
            .scope((71, "fixture".into()), d.handle(request))
            .await
            .unwrap();
        let id = d.runs.locked().get(&sid).unwrap().run_id.clone();
        let stored = st
            .recorded_forecasts()
            .unwrap()
            .into_iter()
            .find(|(r, _)| r == &id)
            .unwrap()
            .1;
        assert_eq!(stored.fingerprint, forecast.fingerprint);
        assert_eq!(stored.basis.model, "model");
        if operation == 0 {
            assert_eq!(
                st.list_messages(&sid).unwrap().last().unwrap().content,
                "edited"
            );
        }
        if operation == 1 {
            assert_eq!(st.list_sessions().unwrap().len(), before_sessions + 1);
        }
        if operation == 2 {
            let messages = st.list_messages(&sid).unwrap();
            assert!(messages
                .iter()
                .any(|m| m.content.contains("previous run was paused")));
            assert!(messages
                .iter()
                .any(|m| m.role == "tool" && m.content.contains("not retried")));
            assert!(st.dangling_tool_calls(&sid).unwrap().is_empty());
            assert!(d.budget_waived.locked().contains(&sid));
        }
        cancel_fixture_run(&d, &sid).await;
    }
}

#[tokio::test]
async fn retry_override_is_the_immutable_forecast_and_dispatch_model() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, mut cfg) = setup(&env);
    cfg.limits.forecast_confirm_usd = None;
    cfg.provider
        .model_context_tokens
        .insert("override".into(), 100_000);
    cfg.save(&paths).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    st.append_message(&sid, "user", "original").unwrap();
    d.handle(Request::RetryRun {
        session_id: sid.clone(),
        expected_run_id: None,
        model: Some("override".into()),
        keep_original: false,
    })
    .await
    .unwrap();
    let run = d.runs.locked().get(&sid).unwrap().run_id.clone();
    let f = st
        .recorded_forecasts()
        .unwrap()
        .into_iter()
        .find(|(r, _)| r == &run)
        .unwrap()
        .1;
    assert_eq!(f.basis.model, "override");
    assert_eq!(
        d.run_admissions.locked().get(&run).unwrap().requested,
        "override"
    );
    cancel_fixture_run(&d, &sid).await;
}

#[tokio::test]
async fn retry_run_preserves_steer_provenance_when_content_is_unchanged() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, mut cfg) = setup(&env);
    cfg.limits.forecast_confirm_usd = None;
    cfg.save(&paths).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    let user = st
        .append_message_meta(
            &sid,
            "user",
            "original",
            Some(
                json!({
                    "review_context":{"path":"fixture.rs"},
                    "steer":{"run_id":"source-run","step":2,"context_included":true}
                })
                .to_string(),
            ),
        )
        .unwrap();
    d.handle(Request::RetryRun {
        session_id: sid.clone(),
        expected_run_id: None,
        model: None,
        keep_original: false,
    })
    .await
    .unwrap();
    let retried = st.get_message(&sid, &user.id).unwrap().unwrap();
    let meta: Value = serde_json::from_str(retried.meta.as_deref().unwrap()).unwrap();
    assert_eq!(meta["review_context"]["path"], "fixture.rs");
    assert_eq!(meta["steer"]["run_id"], "source-run");
    assert_eq!(meta["steer"]["step"], 2);
    assert_eq!(meta["steer"]["context_included"], true);
    cancel_fixture_run(&d, &sid).await;
}

#[tokio::test]
async fn retry_expected_run_rejects_a_stale_selection_without_editing_or_forking() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, mut cfg) = setup(&env);
    cfg.limits.forecast_confirm_usd = None;
    cfg.save(&paths).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    let old = st.create_run(&sid).unwrap();
    st.set_run_status(&old, "completed").unwrap();
    let latest = st.create_run(&sid).unwrap();
    st.set_run_status(&latest, "completed").unwrap();
    st.append_message(&sid, "user", "second question").unwrap();
    st.append_message(&sid, "assistant", "current answer")
        .unwrap();
    let error = d
        .handle(Request::RetryRun {
            session_id: sid.clone(),
            expected_run_id: Some(old),
            model: Some("override".into()),
            keep_original: true,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("the latest Run changed"));
    assert_eq!(st.last_run(&sid).unwrap().unwrap().0, latest);
    assert_eq!(st.list_sessions().unwrap().len(), 1);
    assert_eq!(
        st.list_messages(&sid).unwrap().last().unwrap().content,
        "current answer"
    );
    assert!(d.runs.locked().is_empty());
}

#[tokio::test]
async fn racing_attachment_waits_for_admission_and_cannot_enter_the_confirmed_request() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, _cfg) = setup(&env);
    let input = ForecastInput {
        session_id: Some(sid.clone()),
        draft: "task".into(),
        ..Default::default()
    };
    let f: RunForecast = serde_json::from_value(
        CLIENT
            .scope(
                (81, "fixture".into()),
                d.handle(Request::ForecastRun {
                    input,
                    for_send: true,
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    CLIENT
        .scope(
            (81, "fixture".into()),
            d.handle(Request::ConfirmForecast {
                id: f.confirmation_id.clone().unwrap(),
            }),
        )
        .await
        .unwrap();
    let late = paths.data_dir.join("late.txt");
    std::fs::write(&late, b"late").unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(0);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let done_rx = StdMutex::new(done_rx);
    let attempt_rx = StdMutex::new(attempt_rx);
    *d.forecast_admission_test.locked() = Some(Arc::new(move || {
        entered_tx.send(()).unwrap();
        attempt_rx
            .locked()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(
            matches!(
                done_rx.locked().recv_timeout(Duration::from_millis(30)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "racing Store mutation passed the admission boundary"
        );
    }));
    let other = d.clone();
    let target = sid.clone();
    let writer = std::thread::spawn(move || {
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        attempt_tx.send(()).unwrap();
        other.with_store(|st|st.add_pending_attachment(&target,&json!({"id":"late","name":"late.txt","mime":"text/plain","path":late,"bytes":4}))).unwrap();
        done_tx.send(()).unwrap();
    });
    CLIENT
        .scope(
            (81, "fixture".into()),
            d.handle(Request::SendMessage {
                session_id: sid.clone(),
                content: "task".into(),
                mode: SendMode::Send,
                client_msg_id: None,
            }),
        )
        .await
        .unwrap();
    writer.join().unwrap();
    *d.forecast_admission_test.locked() = None;
    let run = d.runs.locked().get(&sid).unwrap().run_id.clone();
    let first = d.run_admissions.locked().get(&run).unwrap().clone();
    assert!(
        !serde_json::to_string(&first.first.locked().as_ref().unwrap().0)
            .unwrap()
            .contains("late.txt")
    );
    let st = Store::open(&paths.db_file()).unwrap();
    assert_eq!(st.pending_attachments(&sid).unwrap()[0]["name"], "late.txt");
    let admitted = st
        .recorded_forecasts()
        .unwrap()
        .into_iter()
        .find(|(r, _)| r == &run)
        .unwrap()
        .1;
    assert_eq!(admitted.fingerprint, f.fingerprint);
    assert!(!st
        .list_messages(&sid)
        .unwrap()
        .last()
        .unwrap()
        .meta
        .as_deref()
        .unwrap_or("")
        .contains("late.txt"));
    cancel_fixture_run(&d, &sid).await;
}

#[tokio::test]
async fn preview_makes_zero_requests_and_completed_run_records_exact_wire_usage_coverage() {
    use axum::response::IntoResponse;
    struct Server(Option<tokio::task::JoinHandle<()>>);
    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(t) = &self.0 {
                t.abort();
            }
        }
    }
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, mut cfg) = setup(&env);
    let seen = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let recorded = seen.clone();
    let app=axum::Router::new().route("/chat/completions",axum::routing::post(move|axum::Json(body):axum::Json<Value>|{
        let recorded=recorded.clone();async move{
            let raw=crate::tokens::request(&body) as u64;
            recorded.locked().push(body);
            format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":{"content":"The task is complete."}}]}),json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":raw,"completion_tokens":5}})).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.save(&paths).unwrap();
    let mut server = Server(Some(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })));
    let input = ForecastInput {
        session_id: Some(sid.clone()),
        draft: "task".into(),
        ..Default::default()
    };
    let preview: RunForecast = serde_json::from_value(
        CLIENT
            .scope(
                (91, "fixture".into()),
                d.handle(Request::ForecastRun {
                    input,
                    for_send: true,
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(seen.locked().is_empty(), "preview called a provider");
    assert_eq!(
        preview.comparable_runs, 0,
        "other endpoint history cannot price this endpoint"
    );
    CLIENT
        .scope(
            (91, "fixture".into()),
            d.handle(Request::SendMessage {
                session_id: sid.clone(),
                content: "task".into(),
                mode: SendMode::Send,
                client_msg_id: None,
            }),
        )
        .await
        .unwrap();
    let run = d.runs.locked().get(&sid).unwrap().run_id.clone();
    tokio::time::timeout(Duration::from_secs(10), async {
        while d.runs.locked().contains_key(&sid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("mock Run completion");
    let st = Store::open(&paths.db_file()).unwrap();
    assert_eq!(
        seen.locked().len(),
        1,
        "mock call missing: status={:?}, error={:?}",
        st.run_status(&run).unwrap(),
        st.run_error(&run).unwrap()
    );
    assert_eq!(
        preview.basis.prompt_tokens,
        crate::tokens::request(&seen.locked()[0])
    );
    assert_eq!(st.last_run(&sid).unwrap().unwrap().1, "completed");
    assert!(st.forecast_usage_complete(&run).unwrap());
    let rows = crate::forecast::metrics(&st, &cfg).unwrap();
    let actual = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] == run)
        .unwrap();
    assert_eq!(
        actual["actual"]["tokens"].as_f64(),
        Some((preview.basis.prompt_tokens + 5) as f64)
    );
    assert_eq!(actual["calibration_eligible"], true);
    let following = d
        .with_reader(|st| {
            d.forecast_estimate(
                st,
                &cfg,
                &ForecastInput {
                    session_id: Some(sid.clone()),
                    draft: "task".into(),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(following.comparable_runs, 1);
    assert_eq!(following.comparables[0].run_id, run);
    if let Some(task) = server.0.take() {
        task.abort();
        let _ = task.await;
    }
}

#[tokio::test]
async fn priced_comparison_refuses_before_real_forks_messages_runs_or_provider_calls() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, paths, d, sid, mut cfg) = setup(&env);
    let endpoint = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    endpoint.set_nonblocking(true).unwrap();
    cfg.provider.base_url = format!("http://{}", endpoint.local_addr().unwrap());
    cfg.save(&paths).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    st.set_session_cwd(&sid, Some(paths.data_dir.to_str().unwrap()))
        .unwrap();
    st.append_message(&sid, "user", "previous task").unwrap();
    let basis = crate::forecast::estimate_store(
        &st,
        &cfg,
        &ForecastInput {
            session_id: Some(sid.clone()),
            draft: "task".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let prior = st.create_run(&sid).unwrap();
    st.record_forecast(&prior, &basis).unwrap();
    st.bump_run_steps(&prior).unwrap();
    st.record_forecast_request(&prior, "run").unwrap();
    st.record_usage_full(
        &sid,
        &prior,
        "default",
        "model",
        &crate::provider::Usage {
            prompt_tokens: 100,
            completion_tokens: 100,
            reported: 3,
            ..Default::default()
        },
        "run",
    )
    .unwrap();
    st.set_run_status(&prior, "completed").unwrap();
    let sessions = serde_json::to_value(st.list_sessions().unwrap()).unwrap();
    let messages = serde_json::to_value(st.list_messages(&sid).unwrap()).unwrap();
    let runs = serde_json::to_value(st.run_metrics(100).unwrap()).unwrap();
    let recorded = st.recorded_forecasts().unwrap();
    let error = d
        .handle(Request::CompareModels {
            session_id: sid.clone(),
            models: vec!["model".into(), "another".into()],
            prompt: Some("task".into()),
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Batch forecast approval is not supported")
            && error.contains("individual Session"),
        "{error}"
    );
    assert_eq!(
        serde_json::to_value(st.list_sessions().unwrap()).unwrap(),
        sessions
    );
    assert_eq!(
        serde_json::to_value(st.list_messages(&sid).unwrap()).unwrap(),
        messages
    );
    assert_eq!(
        serde_json::to_value(st.run_metrics(100).unwrap()).unwrap(),
        runs
    );
    assert_eq!(st.recorded_forecasts().unwrap(), recorded);
    assert!(d.runs.locked().is_empty() && d.comparisons.locked().is_empty());
    assert!(matches!(endpoint.accept(),Err(e)if e.kind()==std::io::ErrorKind::WouldBlock));
}

#[test]
fn production_request_future_is_bounded_for_default_worker_stacks() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, _paths, d, _sid, _cfg) = setup(&env);
    let request = d.handle(Request::Ping);
    let bytes = std::mem::size_of_val(&request);
    assert!(bytes<=16*1024,"public dispatcher future grew to {bytes} bytes; connection scopes must not inline the entire protocol state machine");
}

#[tokio::test]
async fn admitted_native_sources_survive_later_file_changes_and_initial_model_omission() {
    use axum::{extract::OriginalUri, response::IntoResponse, routing::post, Json, Router};
    use base64::Engine;
    let env = crate::credential_env::EnvGuard::isolated();
    for style in ["chat", "responses", "anthropic"] {
        let (_root, paths, d, sid, mut cfg) = setup(&env);
        let seen = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let capture = seen.clone();
        let handler = move |OriginalUri(uri): OriginalUri, Json(body): Json<Value>| {
            capture.locked().push(body.clone());
            async move {
                if body["model"] == "model" {
                    return (axum::http::StatusCode::NOT_FOUND, "model not found").into_response();
                }
                let events = if uri.path() == "/messages" {
                    vec![
                        json!({"type":"message_start","message":{"usage":{"input_tokens":10}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
                        json!({"type":"message_stop"}),
                    ]
                } else if uri.path() == "/responses" {
                    vec![
                        json!({"type":"response.output_text.delta","delta":"ok"}),
                        json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":10,"output_tokens":2}}}),
                    ]
                } else {
                    vec![
                        json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
                    ]
                };
                let mut text = events
                    .into_iter()
                    .map(|event| format!("data: {event}\n\n"))
                    .collect::<String>();
                if uri.path() == "/chat/completions" {
                    text.push_str("data: [DONE]\n\n");
                }
                ([("content-type", "text/event-stream")], text).into_response()
            }
        };
        let app = Router::new()
            .route("/chat/completions", post(handler.clone()))
            .route("/responses", post(handler.clone()))
            .route("/messages", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
        cfg.provider.api_style = style.into();
        cfg.provider.pdf_input = Some(false);
        cfg.provider
            .model_pdf_input
            .insert("native-fallback".into(), true);
        cfg.provider.fallback_models = vec!["native-fallback".into()];
        cfg.limits.forecast_confirm_usd = None;
        cfg.save(&paths).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap()
        });
        let original = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
        d.handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "admitted.pdf".into(),
            data_b64: base64::engine::general_purpose::STANDARD.encode(original),
            mime: Some("application/pdf".into()),
        })
        .await
        .unwrap();
        let path = d
            .with_reader(|st| {
                Ok(st.pending_attachments(&sid)?[0]["path"]
                    .as_str()
                    .unwrap()
                    .to_string())
            })
            .unwrap();
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: "analyze admitted source".into(),
            ..Default::default()
        };
        let (_, run, admission) = d
            .admit_action(&sid, &cfg, &input, |st| {
                Daemon::append_user_message_store(st, &sid, &input.draft, None)
            })
            .unwrap();
        let admitted_json =
            serde_json::to_string(&admission.first.locked().as_ref().unwrap().0).unwrap();
        assert!(
            admitted_json.contains("_attachments"),
            "admission retains canonical markers"
        );
        assert!(!admitted_json.contains("data:application/pdf"));
        let mut changed = original.to_vec();
        *changed.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &changed).unwrap();
        d.launch_prepared_run(sid.clone(), run.clone(), admission)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while d.runs.locked().contains_key(&sid) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("admitted fixture must settle");
        assert_eq!(
            d.with_reader(|st| st.run_status(&run)).unwrap().as_deref(),
            Some("completed")
        );
        let captured = seen.locked();
        assert_eq!(
            captured.len(),
            2,
            "no extra probe/file/transcription calls: {captured:?}"
        );
        assert!(!crate::tokens::has_native_media(&captured[0]));
        assert!(crate::tokens::has_native_media(&captured[1]));
        let native = &captured[1];
        let content = if style == "responses" {
            &native["input"]
        } else {
            &native["messages"]
        };
        let file = content
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().into_iter().flatten())
            .find(|p| matches!(p["type"].as_str(), Some("file" | "input_file" | "document")))
            .unwrap();
        let encoded = if style == "anthropic" {
            file["source"]["data"].as_str().unwrap()
        } else if style == "responses" {
            file["file_data"]
                .as_str()
                .unwrap()
                .strip_prefix("data:application/pdf;base64,")
                .unwrap()
        } else {
            file["file"]["file_data"]
                .as_str()
                .unwrap()
                .strip_prefix("data:application/pdf;base64,")
                .unwrap()
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            original,
            "dispatch must use admitted bytes, not changed on-disk bytes"
        );
        assert_eq!(
            d.usage_info(None, Some(&run), &cfg).unwrap().cost,
            None,
            "native actual cost remains unknown"
        );
        drop(captured);
        assert_eq!(std::fs::read(&path).unwrap(), changed);
        drop(d);
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }
}
