use super::*;
fn reference(tag: &str) -> Value {
    json!({"reference":true,"name":"reference ไทย😀","path":"/does/not/need/to/exist","dir":false,"bytes":8,"future":{"tag":tag}})
}
#[test]
fn reference_detach_removes_only_the_reviewed_position_not_an_id_or_path_match() {
    let st = Store::open_in_memory().unwrap();
    let a = st.create_session(None).unwrap().id;
    let b = st.create_session(None).unwrap().id;
    let artifact = st
        .save_artifact(Some(&b), "/not/opened", "preview", 8)
        .unwrap();
    let mut first = reference("first");
    first["id"] = json!(artifact);
    let stored = json!({"id":artifact,"name":"stored","path":"/does/not/need/to/exist","mime":"text/plain","bytes":8});
    let expected = vec![
        first.clone(),
        stored.clone(),
        first.clone(),
        reference("last"),
    ];
    for row in &expected {
        st.add_pending_attachment(&a, row).unwrap();
    }
    st.add_pending_attachment(&b, &first).unwrap();
    let artifacts = st.list_artifacts().unwrap();
    st.detach_reviewed_reference(&a, &expected, 2).unwrap();
    assert_eq!(
        st.pending_attachment_inventory(&a).unwrap(),
        vec![first.clone(), stored, reference("last")]
    );
    assert_eq!(st.pending_attachment_inventory(&b).unwrap(), vec![first]);
    assert_eq!(
        st.list_artifacts().unwrap(),
        artifacts,
        "metadata id/path collisions must never delete artifact rows"
    );
}
#[test]
fn reference_detach_rejects_stale_invalid_deleted_and_corrupt_inventories_whole() {
    for mode in 0..10 {
        let st = Store::open_in_memory().unwrap();
        let sid = st.create_session(None).unwrap().id;
        st.add_pending_attachment(&sid, &reference("one")).unwrap();
        st.add_pending_attachment(&sid, &reference("two")).unwrap();
        let mut expected = st.pending_attachment_inventory(&sid).unwrap();
        let mut index = 0;
        let mut target = sid.clone();
        match mode {
            0 => expected.reverse(),
            1 => {
                expected.pop();
            }
            2 => expected[0]["future"]["tag"] = json!("changed"),
            3 => index = 2,
            4 => target = "other".into(),
            5 => {
                st.soft_delete_session(&sid).unwrap();
            }
            6 => {
                st.add_pending_attachment(&sid, &Value::Null).unwrap();
            }
            7 => {
                st.conn.execute("INSERT INTO pending_attachments(session_id,att,created_at) VALUES(?1,'corrupt','now')",[&sid]).unwrap();
            }
            8 => {
                st.take_pending_attachments(&sid).unwrap();
                st.add_pending_attachment(
                    &sid,
                    &json!({"reference":false,"id":"stored","path":"/does/not/exist"}),
                )
                .unwrap();
                expected = st.pending_attachment_inventory(&sid).unwrap();
            }
            _ => {
                st.take_pending_attachments(&sid).unwrap();
                let mut row = reference("bad");
                row["dir"] = Value::Null;
                st.add_pending_attachment(&sid, &row).unwrap();
                expected = st.pending_attachment_inventory(&sid).unwrap();
            }
        }
        let before: Vec<(i64, String)> = st
            .conn
            .prepare("SELECT rowid,att FROM pending_attachments ORDER BY rowid")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            st.detach_reviewed_reference(&target, &expected, index)
                .is_err(),
            "mode {mode}"
        );
        let after: Vec<(i64, String)> = st
            .conn
            .prepare("SELECT rowid,att FROM pending_attachments ORDER BY rowid")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(before, after, "mode {mode} changed pending rows on failure");
    }
}
#[test]
fn reference_detach_rolls_back_with_its_transaction_instead_of_leaving_partial_removal() {
    let st = Store::open_in_memory().unwrap();
    let sid = st.create_session(None).unwrap().id;
    let expected = vec![reference("one"), reference("two")];
    for row in &expected {
        st.add_pending_attachment(&sid, row).unwrap();
    }
    let result: Result<()> = st.transaction(|st| {
        st.detach_reviewed_reference(&sid, &expected, 0)?;
        Err(anyhow!("later transaction failure"))
    });
    assert!(result.is_err());
    assert_eq!(st.pending_attachment_inventory(&sid).unwrap(), expected);
    st.conn.execute_batch("CREATE TRIGGER refuse_reference_delete BEFORE DELETE ON pending_attachments BEGIN SELECT RAISE(ABORT,'refused by test'); END;").unwrap();
    assert!(st.detach_reviewed_reference(&sid, &expected, 0).is_err());
    assert_eq!(st.pending_attachment_inventory(&sid).unwrap(), expected);
}
#[test]
fn reference_detach_keeps_a_separate_sqlite_writer_out_of_compare_and_remove() {
    let root = std::env::temp_dir().join(format!("fl-reference-store-{}", uuid::Uuid::new_v4()));
    let path = root.join("private.db");
    let st = Store::open(&path).unwrap();
    let sid = st.create_session(None).unwrap().id;
    let expected = vec![reference("reviewed")];
    st.add_pending_attachment(&sid, &expected[0]).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let other_sid = sid.clone();
    let writer = std::thread::spawn(move || {
        let other = Store::open(&path).unwrap();
        ready_tx.send(()).unwrap();
        go_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        attempt_tx.send(()).unwrap();
        done_tx
            .send(other.add_pending_attachment(&other_sid, &reference("late")))
            .unwrap();
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    st.transaction(|st| {
        go_tx.send(()).unwrap();
        attempt_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        st.detach_reviewed_reference(&sid, &expected, 0)
    })
    .unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    assert_eq!(
        st.pending_attachment_inventory(&sid).unwrap(),
        vec![reference("late")]
    );
    drop(st);
    std::fs::remove_dir_all(root).unwrap();
}
