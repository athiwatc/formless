//! Genuine historical schema prefixes, embedded for offline/shallow-checkout
//! tests. This helper never opens the current Store to manufacture old state.

use rusqlite::Connection;
use std::path::Path;

const PREFIX: &str = include_str!("../fixtures/historical-upgrades/prefix-through-46.sql");

pub fn create(file: &Path, checkpoint: i64) -> Connection {
    assert!((1..=46).contains(&checkpoint));
    assert!(
        !file.exists(),
        "historical fixture must start with no database"
    );
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let conn = Connection::open(file).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    let mut applied = 0;
    for section in PREFIX.split("\n-- migration ").skip(1) {
        let (version, sql) = section.split_once('\n').unwrap();
        let version: i64 = version.parse().unwrap();
        assert_eq!(version, applied + 1, "historical steps must be ordered");
        if version > checkpoint {
            break;
        }
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute_batch(sql).unwrap();
        tx.execute_batch(&format!("PRAGMA user_version={version};"))
            .unwrap();
        tx.commit().unwrap();
        applied = version;
    }
    assert_eq!(applied, checkpoint);
    assert_old_shape(&conn, checkpoint);
    conn
}

pub fn version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn column(conn: &Connection, table: &str, name: &str) -> bool {
    conn.prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .any(|column| column.unwrap() == name)
}

pub fn assert_old_shape(conn: &Connection, checkpoint: i64) {
    assert_eq!(version(conn), checkpoint);
    assert_eq!(column(conn, "messages", "meta"), checkpoint >= 2);
    assert_eq!(table(conn, "tools"), checkpoint >= 2);
    assert_eq!(table(conn, "memories"), checkpoint >= 17);
    assert_eq!(column(conn, "memories", "proposal_id"), checkpoint >= 18);
    assert_eq!(table(conn, "tool_kv"), checkpoint >= 23);
    assert_eq!(table(conn, "messages_fts"), checkpoint >= 24);
    assert_eq!(column(conn, "sessions", "message_count"), checkpoint >= 39);
    assert_eq!(column(conn, "run_usage", "cost"), checkpoint >= 46);
    assert!(!table(conn, "denials"), "v47 must not already exist");
    assert!(!column(conn, "approvals", "code_hash"));
    assert!(!column(conn, "sessions", "name_source"));
}
