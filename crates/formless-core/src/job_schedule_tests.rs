use super::Store;
use chrono::{Local, TimeZone};

fn time(month: u32, day: u32, hour: u32) -> chrono::DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, month, day, hour, 0, 0)
        .single()
        .unwrap()
}

#[test]
fn job_enable_approval_and_resume_recompute_the_next_future_slot() {
    let store = Store::open_in_memory().unwrap();
    let job = store
        .create_job(
            "nightly",
            None,
            Some("0 3 * * *"),
            &time(10, 1, 3).to_rfc3339(),
            false,
            "agent",
        )
        .unwrap();
    let enabled_at = time(10, 21, 12);
    store.set_job_enabled_at(&job.id, true, enabled_at).unwrap();
    let enabled = store.get_job(&job.id).unwrap().unwrap();
    assert!(enabled.enabled && enabled.approved);
    assert_eq!(enabled.next_run, Some(time(10, 22, 3).to_rfc3339()));
    assert!(store
        .due_jobs(enabled_at.with_timezone(&chrono::Utc))
        .unwrap()
        .is_empty());

    store
        .set_job_enabled_at(&job.id, false, time(10, 21, 13))
        .unwrap();
    let disabled = store.get_job(&job.id).unwrap().unwrap();
    assert!(!disabled.enabled && disabled.approved);
    assert_eq!(disabled.next_run, enabled.next_run);
    let resumed_at = time(11, 21, 12);
    store.set_job_enabled_at(&job.id, true, resumed_at).unwrap();
    let resumed = store.get_job(&job.id).unwrap().unwrap();
    assert_eq!(resumed.next_run, Some(time(11, 22, 3).to_rfc3339()));
    assert!(store
        .due_jobs(resumed_at.with_timezone(&chrono::Utc))
        .unwrap()
        .is_empty());
}

#[test]
fn job_enable_is_idempotent_and_keeps_enabled_downtime_catch_up() {
    let store = Store::open_in_memory().unwrap();
    let due = time(10, 1, 3).to_rfc3339();
    for cron in [Some("0 3 * * *"), None] {
        let job = store
            .create_job("missed while daemon down", None, cron, &due, true, "user")
            .unwrap();
        store
            .set_job_enabled_at(&job.id, true, time(10, 21, 12))
            .unwrap();
        assert_eq!(
            store.get_job(&job.id).unwrap().unwrap().next_run,
            Some(due.clone())
        );
        assert!(store
            .due_jobs(time(10, 21, 12).with_timezone(&chrono::Utc))
            .unwrap()
            .iter()
            .any(|j| j.id == job.id));
    }
}

#[test]
fn job_enable_preserves_future_one_shot_and_rejects_expired_or_completed() {
    let store = Store::open_in_memory().unwrap();
    let future = time(10, 22, 3).to_rfc3339();
    let job = store
        .create_job("once", None, None, &future, false, "user")
        .unwrap();
    store
        .set_job_enabled_at(&job.id, true, time(10, 21, 12))
        .unwrap();
    assert_eq!(
        store.get_job(&job.id).unwrap().unwrap().next_run,
        Some(future)
    );
    store
        .set_job_enabled_at(&job.id, false, time(10, 21, 13))
        .unwrap();
    let before = store.get_job(&job.id).unwrap().unwrap();
    let error = store
        .set_job_enabled_at(&job.id, true, time(10, 22, 3))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("expired") && error.contains("jobs run"),
        "{error}"
    );
    let after = store.get_job(&job.id).unwrap().unwrap();
    assert!(!after.enabled && after.approved == before.approved);
    assert_eq!(after.next_run, before.next_run);
    store.job_ran(&job.id, "completed", None).unwrap();
    assert!(store
        .set_job_enabled_at(&job.id, true, time(10, 23, 12))
        .unwrap_err()
        .to_string()
        .contains("completed"));
    assert!(store.get_job(&job.id).unwrap().unwrap().next_run.is_none());
}

#[test]
fn job_enable_bad_cron_rolls_back_and_missing_id_returns_false() {
    let store = Store::open_in_memory().unwrap();
    let job = store
        .create_job(
            "damaged old schedule",
            None,
            Some("invalid"),
            &time(10, 1, 3).to_rfc3339(),
            false,
            "agent",
        )
        .unwrap();
    assert!(store
        .set_job_enabled_at(&job.id, true, time(10, 21, 12))
        .is_err());
    let after = store.get_job(&job.id).unwrap().unwrap();
    assert!(!after.enabled && !after.approved);
    assert_eq!(after.next_run, job.next_run);
    assert!(store
        .set_job_enabled_at(&job.id, false, time(10, 21, 12))
        .unwrap());
    for enabled in [true, false] {
        assert!(!store
            .set_job_enabled_at("missing", enabled, time(10, 21, 12))
            .unwrap());
    }
}

#[test]
fn job_enable_reads_the_clock_inside_the_writer_transaction() {
    let store = Store::open_in_memory().unwrap();
    let job = store
        .create_job(
            "nightly",
            None,
            Some("0 3 * * *"),
            &time(10, 1, 3).to_rfc3339(),
            false,
            "agent",
        )
        .unwrap();
    store
        .set_job_enabled_with_clock(&job.id, true, || {
            assert!(
                !store.conn.is_autocommit(),
                "clock read before writer boundary"
            );
            time(10, 21, 12)
        })
        .unwrap();
    assert_eq!(
        store.get_job(&job.id).unwrap().unwrap().next_run,
        Some(time(10, 22, 3).to_rfc3339())
    );
}
