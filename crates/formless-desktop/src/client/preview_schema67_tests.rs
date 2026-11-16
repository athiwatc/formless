//! Real preview Client/Schema67 Core framing with no listener or Provider.
use super::*;
use crate::reference_detach::{Action as Removal, Ledger, Outcome, Status};
use serde_json::json;
struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fl-preview-pair-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn until(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
async fn update(rx: &mut Receiver<Update>, predicate: impl Fn(&Update) -> bool + Send) -> Update {
    tokio::time::timeout(Duration::from_secs(5), async move {
        loop {
            while let Ok(u) = rx.try_recv() {
                if predicate(&u) {
                    return u;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap()
}
async fn inventory(
    tx: &queue::Sender<Command>,
    rx: &mut Receiver<Update>,
    session: &str,
) -> Arc<crate::pending_files::Report> {
    let operation = uuid::Uuid::new_v4();
    tx.send(Command::ReadPendingAttachments {
        connection: 0,
        operation,
        session_id: session.into(),
    })
    .await
    .unwrap();
    match update(
        rx,
        |u| matches!(u,Update::PendingAttachments{operation:found,..}if *found==operation),
    )
    .await
    {
        Update::PendingAttachments {
            result: Ok(report), ..
        } => Arc::new(report),
        other => panic!("unexpected inventory {other:?}"),
    }
}
fn removal(
    client: uuid::Uuid,
    session: &str,
    report: Arc<crate::pending_files::Report>,
) -> Removal {
    Removal {
        client,
        connection: 0,
        epoch: 0,
        form: uuid::Uuid::new_v4(),
        read: uuid::Uuid::new_v4(),
        operation: uuid::Uuid::new_v4(),
        session_id: session.into(),
        index: 1,
        report,
    }
}
#[tokio::test]
async fn preview_schema67_real_core_browses_edits_reviews_and_keeps_lost_reference_unknown_on_reconnect(
) {
    assert_eq!(formless_core::storage::SCHEMA_VERSION, 67);
    let _env = credential_env::EnvGuard::isolated();
    let dir = Directory::new();
    let paths = formless_core::Paths::from_dirs(dir.0.join("data"), dir.0.join("config")).unwrap();
    let mut config = formless_core::Config::default();
    config.provider.auto_test_models = false;
    config.naming.enabled = false;
    config.skills.auto_create = false;
    config.save(&paths).unwrap();
    let st = formless_core::Store::open(&paths.db_file()).unwrap();
    let sid = st.create_session(Some("Before preview".into())).unwrap().id;
    st.append_message(
        &sid,
        "assistant",
        "Canonical ไทย 漢字 😀\n exact whitespace ",
    )
    .unwrap();
    let marker = dir.0.join("never-delete-reference.txt");
    std::fs::write(&marker, b"unchanged reference target").unwrap();
    let row = json!({"reference":true,"name":"keep ไทย😀","path":marker.to_str().unwrap(),"dir":false,"bytes":26,"future":{"preserve":true}});
    for _ in 0..3 {
        st.add_pending_attachment(&sid, &row).unwrap();
    }
    drop(st);
    let client = uuid::Uuid::new_v4();
    let ledger = Arc::new(Ledger::default());
    let mut retained = Retained {
        instance: client,
        ..Default::default()
    };
    let mut lost = None;
    for phase in 0..2 {
        let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
        let (stream, server) = tokio::io::duplex(128 * 1024);
        let (tx, mut commands) = queue::channel(QUEUE);
        let (updates, mut rx) = mpsc::sync_channel(QUEUE);
        let (stop, mut cancelled) = watch::channel(false);
        let ready = Arc::new(AtomicBool::new(false));
        let epoch = AtomicU64::new(0);
        let connection = AtomicU64::new(0);
        let driver = tokio::spawn({
            let ledger = ledger.clone();
            let ready = ready.clone();
            let sid = sid.clone();
            let lost = lost.clone();
            async move {
                until(|| ready.load(Ordering::Acquire)).await;
                if phase == 0 {
                    tx.send(Command::Select {
                        session_id: Some(sid.clone()),
                        generation: 1,
                    })
                    .await
                    .unwrap();
                    let snapshot=update(&mut rx,|u|matches!(u,Update::Snapshot{generation:1,snapshot}if snapshot.session_id==sid)).await;
                    match snapshot {
                        Update::Snapshot { snapshot, .. } => assert!(snapshot
                            .blocks
                            .iter()
                            .any(|b| b.markdown() == "Canonical ไทย 漢字 😀\n exact whitespace ")),
                        other => panic!("{other:?}"),
                    }
                    let operation = uuid::Uuid::new_v4();
                    tx.send(Command::EditSession {
                        connection: 0,
                        operation,
                        session_id: sid.clone(),
                        change: session_edit::Change::Name("Preview reviewed ไทย😀".into()),
                    })
                    .await
                    .unwrap();
                    let edited=update(&mut rx,|u|matches!(u,Update::SessionEdited{operation:found,..}if *found==operation)).await;
                    assert!(matches!(
                        edited,
                        Update::SessionEdited {
                            outcome: session_edit::Outcome::Applied(_),
                            ..
                        }
                    ));
                    let report = inventory(&tx, &mut rx, &sid).await;
                    assert_eq!(report.expected().len(), 3);
                    let first = ledger.reserve(removal(client, &sid, report)).unwrap();
                    tx.send(Command::DetachReference(first)).await.unwrap();
                    until(|| {
                        matches!(
                            ledger.status(),
                            Status::Finished {
                                outcome: Outcome::Removed,
                                ..
                            }
                        )
                    })
                    .await;
                    let report = inventory(&tx, &mut rx, &sid).await;
                    assert_eq!(report.expected().len(), 2);
                    let a = removal(client, &sid, report);
                    tx.send(Command::DetachReference(ledger.reserve(a.clone()).unwrap()))
                        .await
                        .unwrap();
                    (tx, stop, Some(a))
                } else {
                    assert!(matches!(
                        ledger.status(),
                        Status::Finished {
                            outcome: Outcome::Unknown,
                            ..
                        }
                    ));
                    assert!(ledger.reserve(lost.unwrap()).is_err());
                    tx.send(Command::Intent(Intent::ResumeRun {
                        session_id: sid.clone(),
                    }))
                    .await
                    .unwrap();
                    update(
                        &mut rx,
                        |u| matches!(u,Update::Notice(n)if n.contains("unknown")),
                    )
                    .await;
                    let report = inventory(&tx, &mut rx, &sid).await;
                    assert_eq!(report.expected().len(), 1);
                    assert!(matches!(
                        ledger.status(),
                        Status::Finished {
                            outcome: Outcome::Unknown,
                            ..
                        }
                    ));
                    stop.send(true).unwrap();
                    (tx, stop, None)
                }
            }
        });
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(server);
            let mut removals = 0;
            let mut renames = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let req: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let remove = matches!(req.request, Request::DetachReviewedReference { .. });
                match &req.request {
                    Request::Ping
                    | Request::Subscribe { .. }
                    | Request::ListSessions
                    | Request::GetSession { .. }
                    | Request::ListPendingAttachments { .. } => {}
                    Request::RenameSession { .. } => {
                        assert_eq!(phase, 0);
                        renames += 1;
                    }
                    Request::DetachReviewedReference { .. } => {
                        assert_eq!(phase, 0);
                        removals += 1;
                    }
                    other => panic!("unexpected/replayed/Provider-starting request {other:?}"),
                };
                let value = if let Request::Subscribe { deltas, .. } = &req.request {
                    assert!(!deltas);
                    json!({"subscribed":true})
                } else {
                    daemon.handle(req.request).await.unwrap()
                };
                if remove && removals == 2 {
                    assert_eq!(value["removed"], true);
                    break;
                }
                let mut bytes = serde_json::to_vec(&ServerMessage::ok(req.id, value)).unwrap();
                bytes.push(b'\n');
                reader.get_mut().write_all(&bytes).await.unwrap();
            }
            (removals, renames)
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            connected(
                stream,
                &mut commands,
                &updates,
                &mut cancelled,
                &mut retained,
                ConnectionState {
                    ready: &ready,
                    epoch: &epoch,
                    connection: &connection,
                },
            ),
        )
        .await
        .unwrap();
        if phase == 0 {
            assert!(result.is_err());
            assert_eq!(retained.uncertain.len(), 1);
        } else {
            assert!(result.is_ok());
            assert_eq!(retained.uncertain.len(), 1);
        }
        let (_sender, _stop, action) = driver.await.unwrap();
        if phase == 0 {
            lost = action;
        }
        assert_eq!(
            peer.await.unwrap(),
            if phase == 0 { (2, 1) } else { (0, 0) }
        );
    }
    let st = formless_core::Store::open_reader(&paths.db_file()).unwrap();
    assert_eq!(
        st.get_session(&sid).unwrap().unwrap().name,
        "Preview reviewed ไทย😀"
    );
    assert_eq!(st.list_messages(&sid).unwrap().len(), 1);
    assert_eq!(st.pending_attachment_inventory(&sid).unwrap(), vec![row]);
    assert_eq!(st.queued_count(&sid).unwrap(), 0);
    drop(st);
    // Owned fixture writer only; query-only readers cannot VACUUM INTO.
    let st = formless_core::Store::open(&paths.db_file()).unwrap();
    let backup = dir.0.join("schema67-backup.db");
    st.backup_into(&backup).unwrap();
    drop(st);
    let report = formless_core::backup_verification::verify(&paths, &backup).unwrap();
    assert_eq!(report.schema_version, 67);
    assert!(!report.tables.contains_key("database_identity"));
    assert!(!report.tables.contains_key("reviewed_reference_successes"));
    assert_eq!(
        std::fs::read(marker).unwrap(),
        b"unchanged reference target"
    );
}
