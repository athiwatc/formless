//! Owned fixtures for schema-67 upgrades and unsupported preview refusal.
use super::*;
struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fl-preview-schema-{}", uuid::Uuid::new_v4()));
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
#[test]
fn upgrade_schema67_preserves_existing_history_and_reference_metadata() {
    let dir = Directory::new();
    let file = dir.0.join("schema67.db");
    let conn = schema_at(67).unwrap();
    conn.execute_batch("PRAGMA user_version=67; INSERT INTO sessions(id,name,created_at,updated_at) VALUES('known','Existing ไทย😀','2026-11-16T00:00:00Z','2026-11-16T00:00:00Z'); INSERT INTO messages(id,session_id,seq,role,content,created_at) VALUES('message','known',1,'assistant','Persisted 漢字 ไทย😀','2026-11-16T00:00:00Z');").unwrap();
    let row = json!({"reference":true,"name":"keep","path":"/never-opened","dir":true,"bytes":0});
    conn.execute(
        "INSERT INTO pending_attachments(session_id,att,created_at) VALUES('known',?1,'2026-11-16T00:00:00Z')",
        [row.to_string()],
    )
    .unwrap();
    conn.execute("VACUUM INTO ?1", [file.to_str().unwrap()])
        .unwrap();
    drop(conn);
    let st = Store::open(&file).unwrap();
    assert_eq!(
        st.conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(
        st.get_session("known").unwrap().unwrap().name,
        "Existing ไทย😀"
    );
    assert_eq!(
        st.list_messages("known").unwrap()[0].content,
        "Persisted 漢字 ไทย😀"
    );
    assert_eq!(
        st.pending_attachment_inventory("known").unwrap(),
        vec![row.clone()]
    );
    st.rename_session("known", "Reviewed preview name").unwrap();
    st.detach_reviewed_reference("known", &[row], 0).unwrap();
    assert!(st.pending_attachment_inventory("known").unwrap().is_empty());
    assert_eq!(st.list_messages("known").unwrap().len(), 1);
    for table in ["reviewed_reference_successes", "database_identity"] {
        assert_eq!(
            st.conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    drop(st);
    let st = Store::open(&file).unwrap();
    assert_eq!(
        st.get_session("known").unwrap().unwrap().name,
        "Reviewed preview name"
    );
    assert_eq!(
        st.conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
}
#[test]
fn unsupported_preview_stamps_are_refused_without_replacing_rows() {
    // Refusal probes, not genuine prototype69 recovery fixtures. No claim that
    // these establish an approved downgrade/restore procedure.
    for version in [68, 69] {
        let dir = Directory::new();
        let file = dir.0.join("future.db");
        let conn = Connection::open(&file).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE future_only(payload TEXT NOT NULL); INSERT INTO future_only VALUES('preserve unknown future data');").unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
        drop(conn);
        let error = match Store::open(&file) {
            Ok(_) => panic!("future schema accepted"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(&format!(
                "database schema v{version} is an unsupported preview schema"
            )),
            "{error}"
        );
        assert!(error.contains(&format!("v{SCHEMA_VERSION}")));
        let conn =
            Connection::open_with_flags(&file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            version
        );
        assert_eq!(
            conn.query_row("SELECT payload FROM future_only", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "preserve unknown future data"
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}

#[test]
fn queue_upgrade_reorder_rolls_back_on_error_and_survives_reopening() {
    let dir = Directory::new();
    let file = dir.0.join("queued.db");
    let conn = schema_at(67).unwrap();
    conn.execute_batch(
        "PRAGMA user_version=67;
         INSERT INTO sessions(id,name,created_at,updated_at) VALUES('s','S','t','t');
         INSERT INTO queued_messages(id,session_id,content,created_at) VALUES(3,'s','first','t'),(9,'s','second','u');",
    ).unwrap();
    conn.execute("VACUUM INTO ?1", [file.to_str().unwrap()])
        .unwrap();
    let st = Store::open(&file).unwrap();
    let original = st.queued_items("s").unwrap();
    assert_eq!(
        original
            .iter()
            .map(|m| (m.id, m.content.as_str(), m.revision))
            .collect::<Vec<_>>(),
        [(3, "first", Some(0)), (9, "second", Some(0))]
    );
    st.conn.execute_batch("CREATE TRIGGER reject_queue_update BEFORE UPDATE OF position ON queued_messages WHEN old.id=9 BEGIN SELECT RAISE(ABORT, 'held'); END;").unwrap();
    assert!(st.move_queued("s", 9, true).is_err());
    drop(st);
    let st = Store::open(&file).unwrap();
    assert_eq!(st.queued_items("s").unwrap(), original);
    st.conn
        .execute_batch("DROP TRIGGER reject_queue_update")
        .unwrap();
    assert_eq!(st.move_queued("s", 9, true).unwrap(), Some(9));
    assert!(!st.edit_queued_if_current("s", 9, 0, "stale move").unwrap());
    assert!(st
        .edit_queued_if_current("s", 9, 1, "second edited")
        .unwrap());
    assert!(!st.edit_queued_if_current("s", 9, 1, "stale edit").unwrap());
    st.enqueue_message("s", "third").unwrap();
    drop(st);
    let st = Store::open(&file).unwrap();
    let queued = st.queued_items("s").unwrap();
    assert_eq!(
        queued
            .iter()
            .map(|m| (m.id, m.content.as_str(), m.revision))
            .collect::<Vec<_>>(),
        [
            (9, "second edited", Some(2)),
            (3, "first", Some(1)),
            (10, "third", Some(0))
        ]
    );
    assert_eq!(
        st.pop_queued_message("s").unwrap().as_deref(),
        Some("second edited")
    );
    assert!(!st
        .edit_queued_if_current("s", 9, 2, "left the queue")
        .unwrap());
}
