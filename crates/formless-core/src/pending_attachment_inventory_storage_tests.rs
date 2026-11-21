use super::*;
#[test]
fn pending_inventory_storage_is_complete_ordered_and_does_not_consume() {
    let st = Store::open_in_memory().unwrap();
    let session = st.create_session(None).unwrap().id;
    let rows = [
        json!({"id":"a","name":"ไทย😀","bytes":5,"path":"/does/not/exist"}),
        json!({"reference":true,"name":"folder","path":"/also/not/present","dir":true,"bytes":0}),
    ];
    for row in &rows {
        st.add_pending_attachment(&session, row).unwrap();
    }
    assert_eq!(st.pending_attachment_inventory(&session).unwrap(), rows);
    assert_eq!(st.pending_attachments(&session).unwrap(), rows);
    assert!(st.pending_attachment_inventory("other").unwrap().is_empty());
}
#[test]
fn pending_inventory_storage_never_returns_a_partial_snapshot_for_corruption_or_overflow() {
    for mode in 0..4 {
        let st = Store::open_in_memory().unwrap();
        let sid = st.create_session(None).unwrap().id;
        st.add_pending_attachment(&sid, &json!({"id":"kept","name":"valid","bytes":1}))
            .unwrap();
        match mode {0=>{st.conn.execute("INSERT INTO pending_attachments(session_id,att,created_at) VALUES(?1,'invalid JSON','now')",[&sid]).unwrap();},1=>st.add_pending_attachment(&sid,&Value::Null).unwrap(),2=>st.add_pending_attachment(&sid,&json!({"huge":"x".repeat(formless_protocol::PENDING_ATTACHMENT_METADATA_BYTES+1)})).unwrap(),_=>{for n in 0..formless_protocol::PENDING_ATTACHMENT_MAX_ENTRIES{st.add_pending_attachment(&sid,&json!({"id":format!("id-{n}")})).unwrap();}}}
        assert!(st.pending_attachment_inventory(&sid).is_err(), "{mode}");
        let count: i64 = st
            .conn
            .query_row(
                "SELECT count(*) FROM pending_attachments WHERE session_id=?1",
                [&sid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count >= 2);
    }
    let st = Store::open_in_memory().unwrap();
    let sid = st.create_session(None).unwrap().id;
    for n in 0..16 {
        st.add_pending_attachment(&sid, &json!({"id":n,"data":"x".repeat(5000)}))
            .unwrap();
    }
    assert!(st.pending_attachment_inventory(&sid).is_err());
}
