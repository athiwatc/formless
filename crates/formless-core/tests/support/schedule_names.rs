#[tokio::test(flavor = "multi_thread")]
async fn human_recurring_inputs_reach_user_agent_and_preview_without_running_jobs() {
    use formless_protocol::Request;
    let env = credential_env::EnvGuard::isolated();
    let (paths, store, daemon) = job_daemon(&env, "schedule-names", "http://127.0.0.1:9");
    let sid = store.create_session(None).unwrap().id;
    for schedule in ["0 9 * * MON-FRI", "0 0 1 JAN-MAR *", "@yearly", "@annually"] {
        let user = daemon.handle(Request::AddJob {
            prompt: "future fixture".into(), session_id: None,
            cron: Some(schedule.into()), at: None, overlap: None, deliver: None,
        }).await.unwrap();
        assert_eq!(user["cron"], schedule);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5),
            daemon.call_tool_direct(&sid, "schedule_run", serde_json::json!({
                "prompt":"future fixture", "cron":schedule
            }))).await.expect("schedule parser/approval fallback exceeded fixture deadline");
        let agent: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(agent["status"].as_str().unwrap().starts_with("awaiting"), "{agent}");
        let saved = store.get_job(agent["id"].as_str().unwrap()).unwrap().unwrap();
        assert_eq!(saved.cron.as_deref(), Some(schedule));
        assert!(!saved.enabled && !saved.approved, "existing recurring approval remains intact");
    }
    for (phrase, cron) in [
        ("every weekday at 9 a.m.", "0 9 * * 1-5"),
        ("daily at 9 p.m.", "0 21 * * *"),
        ("every monday at 9:30 a.m.", "30 9 * * 1"),
        ("every year at 9 a.m.", "0 9 1 1 *"),
    ] {
        let preview = daemon.handle(Request::PreviewSchedule { schedule: phrase.into() }).await.unwrap();
        assert_eq!(preview["cron"], cron);
        assert_eq!(preview["next"].as_array().unwrap().len(), 3);
        let saved = daemon.handle(Request::AddJob {
            prompt: "future fixture".into(), session_id: None,
            cron: Some(phrase.into()), at: None, overlap: None, deliver: None,
        }).await.unwrap();
        assert_eq!(saved["cron"], cron);
    }
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert!(store.list_messages(&sid).unwrap().is_empty());
    assert!(store.list_jobs().unwrap().iter().all(|j| j.last_run.is_none()));
    drain_daemon(daemon).await;
    drop(store);
    std::fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_cron_field_diagnostics_survive_both_creation_entry_points() {
    use formless_protocol::Request;
    let env = credential_env::EnvGuard::isolated();
    let (paths, store, daemon) = job_daemon(&env, "schedule-errors", "http://127.0.0.1:9");
    let sid = store.create_session(None).unwrap().id;
    for (schedule, field) in [("0 9 * * MON-NOP", "weekday"), ("0 0 1 JAN-NOP *", "month"), ("0 bad-9 * * *", "hour")] {
        let user = daemon.handle(Request::AddJob {
            prompt: "future fixture".into(), session_id: None,
            cron: Some(schedule.into()), at: None, overlap: None, deliver: None,
        }).await.unwrap_err().to_string();
        let agent = daemon.call_tool_direct(&sid, "schedule_run", serde_json::json!({
            "prompt":"future fixture", "cron":schedule
        })).await;
        assert!(user.contains(field), "{user}");
        assert!(agent.contains(field), "{agent}");
        assert!(store.list_jobs().unwrap().is_empty());
    }
    drain_daemon(daemon).await;
    drop(store);
    std::fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap();
}
