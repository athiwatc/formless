#[test]
fn job_time_past_and_present_are_rejected() {
    let now = local(2026, 10, 21, 10, 0);
    for at in ["2025-10-21T10:00:00Z", "2025-10-21 10:00", "in 0m"] {
        let error = parse_at(at, now).expect_err(at).to_string();
        assert!(error.contains("future"), "{at}: {error}");
    }
    assert!(parse_at(&now.to_rfc3339(), now).is_err());
}

#[test]
fn job_time_huge_offsets_return_errors_instead_of_panicking() {
    let now = local(2026, 10, 21, 10, 0);
    for input in [
        "in 9999999999d",
        "in 9223372036854775807h",
        "in 9223372036854775807m",
        "in 999999999999999999999999999d",
        "in 367d",
    ] {
        let result = std::panic::catch_unwind(|| parse_at(input, now));
        assert!(result.is_ok(), "panicked for {input}");
        assert!(result.unwrap().is_err(), "accepted {input}");
    }
    assert_eq!(
        parse_at("in 366d", now).unwrap(),
        now + chrono::Duration::days(366)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn job_time_user_and_agent_entry_points_reject_bad_times_and_blank_prompts() {
    use formless_protocol::Request;
    let env = credential_env::EnvGuard::isolated();
    let (paths, store, daemon) = job_daemon(&env, "job-times", "http://127.0.0.1:9");
    let sid = store.create_session(None).unwrap().id;
    for at in [
        "2020-01-01T10:00:00Z",
        "2020-01-01 10:00",
        "in 0m",
        "in 9999999999d",
    ] {
        let user = daemon
            .handle(Request::AddJob {
                prompt: "test".into(),
                session_id: None,
                cron: None,
                at: Some(at.into()),
                overlap: None,
                deliver: None,
            })
            .await;
        assert!(user.is_err(), "user accepted {at}");
        let agent = daemon
            .call_tool_direct(
                &sid,
                "schedule_run",
                serde_json::json!({"prompt":"test","at":at}),
            )
            .await;
        assert!(
            agent.starts_with("error: schedule_run failed:"),
            "agent accepted {at}: {agent}"
        );
        assert!(
            store.list_jobs().unwrap().is_empty(),
            "failed request saved a job"
        );
    }
    for prompt in ["", "  \n\t"] {
        let user = daemon
            .handle(Request::AddJob {
                prompt: prompt.into(),
                session_id: None,
                cron: Some("@daily".into()),
                at: None,
                overlap: None,
                deliver: None,
            })
            .await
            .unwrap_err();
        assert!(user.to_string().contains("prompt required"), "{user}");
        let agent = daemon
            .call_tool_direct(
                &sid,
                "schedule_run",
                serde_json::json!({"prompt":prompt,"at":"in 1m"}),
            )
            .await;
        assert!(agent.contains("prompt required"), "{agent}");
        assert!(store.list_jobs().unwrap().is_empty());
    }

    // User-facing enabling follows the same transition policy as the store.
    let cron = store
        .create_job(
            "pending approval",
            None,
            Some("0 3 * * *"),
            "2020-01-01T03:00:00Z",
            false,
            "agent",
        )
        .unwrap();
    daemon
        .handle(Request::SetJobEnabled {
            id: cron.id.clone(),
            enabled: true,
        })
        .await
        .unwrap();
    let refreshed = store.get_job(&cron.id).unwrap().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(refreshed.next_run.as_deref().unwrap()).unwrap()
            > chrono::Utc::now()
    );
    let once = store
        .create_job("expired", None, None, "2020-01-01T03:00:00Z", false, "user")
        .unwrap();
    let error = daemon
        .handle(Request::SetJobEnabled {
            id: once.id.clone(),
            enabled: true,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("expired"), "{error}");
    assert!(!store.get_job(&once.id).unwrap().unwrap().enabled);
    drain_daemon(daemon).await;
    drop(store);
    std::fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap();
}

#[test]
fn job_time_enabling_an_unapproved_cron_discards_stale_creation_time() {
    let store = Store::open_in_memory().unwrap();
    let job = store
        .create_job(
            "nightly",
            None,
            Some("0 3 * * *"),
            "2020-01-01T03:00:00Z",
            false,
            "agent",
        )
        .unwrap();
    let before = chrono::Utc::now();
    assert!(store.set_job_enabled(&job.id, true).unwrap());
    let enabled = store.get_job(&job.id).unwrap().unwrap();
    assert!(enabled.enabled && enabled.approved);
    let next = chrono::DateTime::parse_from_rfc3339(enabled.next_run.as_deref().unwrap()).unwrap();
    assert!(next > before, "stale creation time retained: {next}");
    assert!(store.due_jobs(before).unwrap().is_empty());
}

#[test]
fn job_time_expired_one_shot_stays_disabled_with_an_explicit_error() {
    let store = Store::open_in_memory().unwrap();
    let job = store
        .create_job("once", None, None, "2020-01-01T03:00:00Z", false, "user")
        .unwrap();
    let error = store
        .set_job_enabled(&job.id, true)
        .expect_err("expired one-shot enabled")
        .to_string();
    assert!(
        error.contains("expired") && error.contains("run"),
        "{error}"
    );
    let after = store.get_job(&job.id).unwrap().unwrap();
    assert!(!after.enabled && !after.approved);
    assert_eq!(after.next_run, job.next_run);
    assert!(store.due_jobs(chrono::Utc::now()).unwrap().is_empty());
}
