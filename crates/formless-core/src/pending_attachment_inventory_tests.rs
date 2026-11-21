use super::*;
#[tokio::test]
async fn pending_inventory_daemon_is_scoped_read_only_and_reports_unknown_or_deleted_sessions() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_SECURE");
    let root = std::env::temp_dir().join(format!("fl-pending-{}", uuid::Uuid::new_v4().simple()));
    let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    let mut config = Config::default();
    config.provider.auto_test_models = false;
    config.naming.enabled = false;
    config.skills.auto_create = false;
    config.save(&paths).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    let sid = st.create_session(None).unwrap().id;
    let hidden = st.create_session(None).unwrap().id;
    let record = json!({"id":"a","name":"Private metadata only","path":"/does/not/exist","bytes":8,"future":{"kept":true}});
    st.add_pending_attachment(&sid, &record).unwrap();
    st.add_pending_attachment(&hidden, &json!({"id":"hidden"}))
        .unwrap();
    drop(st);
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let read = || Request::ListPendingAttachments {
        session_id: sid.clone(),
    };
    let value = daemon.handle(read()).await.unwrap();
    assert_eq!(
        value,
        json!({"session_id":sid,"attachments":[record.clone()]})
    );
    assert_eq!(daemon.handle(read()).await.unwrap(), value);
    assert!(daemon
        .handle(Request::ListPendingAttachments {
            session_id: "missing".into()
        })
        .await
        .is_err());
    daemon
        .with_store(|st| {
            assert_eq!(st.pending_attachments(&sid)?, vec![record]);
            st.soft_delete_session(&sid)?;
            Ok(())
        })
        .unwrap();
    assert!(daemon.handle(read()).await.is_err());
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
