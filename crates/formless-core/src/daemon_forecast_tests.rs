use super::*;
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
    let current = d
        .with_reader(|st| crate::forecast::estimate_store(st, &cfg, &input))
        .unwrap();
    CLIENT
        .scope((9, "fixture".into()), async {
            d.check_forecast_consent(&current).unwrap();
            assert!(d.check_forecast_consent(&current).is_err());
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
