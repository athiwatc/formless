use rusqlite::Connection;
use std::path::Path;

pub const ENCODINGS: [&str; 3] = ["UTF-8", "UTF-16le", "UTF-16be"];

pub fn create_database(path: &Path, encoding: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "encoding", encoding).unwrap();
    // Creating the first table persists the encoding in the database header.
    conn.execute_batch(
        "CREATE TABLE encoding_fixture (unused INTEGER); DROP TABLE encoding_fixture;",
    )
    .unwrap();
    drop(conn);
    let conn = Connection::open(path).unwrap();
    let actual: String = conn
        .pragma_query_value(None, "encoding", |row| row.get(0))
        .unwrap();
    assert_eq!(actual, encoding);
}
