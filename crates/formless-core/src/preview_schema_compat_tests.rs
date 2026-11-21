//! Owned fixtures only: schema-67 preview compatibility and future-stamp refusal.
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
fn preview_schema67_preserves_existing_history_and_metadata_without_new_tables_or_version_bump() {
    assert_eq!(SCHEMA_VERSION, 67);
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
        67
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
        67
    );
}
#[test]
fn preview_schema67_refuses_future_version_stamps_without_downgrading_or_replacing_rows() {
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
            error.contains(&format!("database schema v{version} is newer")),
            "{error}"
        );
        assert!(error.contains("v67"));
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
