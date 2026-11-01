//! Exact Run completion metadata is independent of recent-list pagination.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_primary_role_records_exact_run_after_fifty_one_newer_runs() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env, Mode::Held).await;
    f.cfg.roles.insert(
        "worker".into(),
        crate::config::RoleCfg {
            model: "requested".into(),
            ..Default::default()
        },
    );
    f.cfg.save(&f.daemon().paths).unwrap();
    f.daemon()
        .with_store(|st| st.set_session_model(&f.sid, Some("role:worker".into()), None))
        .unwrap();
    f.send().await;
    tokio::time::timeout(Duration::from_secs(10), f.nested.notified())
        .await
        .expect("private role request did not reach held HTTP peer");
    let run_id = f.daemon().runs.locked().get(&f.sid).unwrap().run_id.clone();
    f.daemon()
        .with_store(|st| {
            let other = st.create_session(None)?.id;
            for _ in 0..51 {
                let newer = st.create_run(&other)?;
                st.set_run_status(&newer, "completed")?;
            }
            let recent = st.run_metrics(50)?;
            assert_eq!(recent.len(), 50);
            assert!(
                recent.iter().all(|m| m.run_id != run_id),
                "fixture must actually displace the held Run from the legacy page"
            );
            let exact = st.run_metric(&run_id)?.expect("exact held Run");
            assert_eq!(exact.run_id, run_id);
            assert_eq!(exact.session_id, f.sid);
            assert_eq!(exact.status, "running");
            assert!(
                st.run_metric(&run_id[..run_id.len() - 1])?.is_none(),
                "the private accessor must not resolve ID prefixes"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    f.release.add_permits(1);
    f.idle().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while Arc::strong_count(f.daemon()) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("private held Run retained producer ownership");
    f.daemon().flush_routing().unwrap();
    let stats = f
        .daemon()
        .with_reader(|st| st.routing_task_stats(&f.cfg, "worker", "default", chrono::Utc::now()))
        .unwrap();
    assert_eq!(
        stats.len(),
        1,
        "the displaced Run must retain exact task metadata"
    );
    assert_eq!(stats[0].model, "requested");
    assert_eq!(stats[0].samples, 1);
    assert_eq!(stats[0].good, 1);
    assert_eq!(stats[0].total_tokens, Some(5.0));
    assert!((stats[0].usd.unwrap() - 0.1).abs() < 1e-12);
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    f.shutdown().await;
}
