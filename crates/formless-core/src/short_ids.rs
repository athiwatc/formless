use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

const SHORT_ID_ATTEMPTS: usize = 8;

/// Insert an ID that keeps the user-facing prefix and eight hexadecimal digits.
/// Each insert ignores only a conflict on the ID and reports how many rows it wrote.
pub(super) fn insert_short_id(
    prefix: char,
    kind: &str,
    mut generate: impl FnMut() -> Uuid,
    mut insert: impl FnMut(&str) -> Result<usize>,
) -> Result<String> {
    for _ in 0..SHORT_ID_ATTEMPTS {
        let candidate = generate().simple();
        let mut buffer = Uuid::encode_buffer();
        let id = format!("{prefix}{}", &candidate.encode_lower(&mut buffer)[..8]);
        if insert(&id)? == 1 {
            return Ok(id);
        }
    }
    bail!("could not allocate a unique {kind} ID after {SHORT_ID_ATTEMPTS} attempts")
}

/// Move applied memory proposals into the memory store as part of migration 18.
pub(super) fn migrate_applied_memories(
    conn: &Connection,
    mut generate: impl FnMut() -> Uuid,
) -> Result<()> {
    let mut proposals = conn.prepare(
        "SELECT id, trim(content), lower(trim(content)), COALESCE(applied_at, created_at)
         FROM prompt_proposals WHERE kind='memory' AND status='applied' ORDER BY rowid",
    )?;
    let mut matching_note =
        conn.prepare("SELECT 1 FROM memories WHERE scope='global' AND dedup_key=?1")?;
    let mut save_note = conn.prepare(
        "INSERT INTO memories(id,text,dedup_key,scope,author,enabled,created_at,updated_at,proposal_id)
         VALUES(?1,?2,?3,'global','optimiser',1,?4,?4,?5) ON CONFLICT(id) DO NOTHING",
    )?;
    let mut rows = proposals.query([])?;
    while let Some(row) = rows.next()? {
        let (proposal, text, key, time): (String, String, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        if matching_note
            .query_row([&key], |_| Ok(()))
            .optional()?
            .is_some()
        {
            continue;
        }
        insert_short_id('m', "migrated memory", &mut generate, |id| {
            Ok(save_note.execute(params![id, text, key, time, proposal])?)
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SCHEMA_V17;

    const COLLISION: Uuid = Uuid::from_u128(0x1234_5678_0000_0000_0000_0000_0000_0000);
    const FREE: Uuid = Uuid::from_u128(0x8765_4321_0000_0000_0000_0000_0000_0000);

    #[test]
    fn retries_the_primary_key_without_overwriting_and_bounds_attempts() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE jobs(id TEXT PRIMARY KEY, prompt TEXT NOT NULL UNIQUE);
             INSERT INTO jobs VALUES('j12345678','original');",
        )
        .unwrap();
        let mut ids = [COLLISION, FREE].into_iter();
        let insert = |id: &str| -> Result<usize> {
            Ok(conn.execute(
                "INSERT INTO jobs VALUES(?1,'new') ON CONFLICT(id) DO NOTHING",
                [id],
            )?)
        };
        let id = insert_short_id('j', "job", || ids.next().unwrap(), insert).unwrap();
        assert_eq!(id, "j87654321");
        let original: String = conn
            .query_row("SELECT prompt FROM jobs WHERE id='j12345678'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(original, "original");

        let mut attempts = 0;
        let err = insert_short_id(
            'j',
            "job",
            || {
                attempts += 1;
                COLLISION
            },
            insert,
        )
        .unwrap_err();
        assert_eq!(attempts, SHORT_ID_ATTEMPTS);
        assert!(err
            .to_string()
            .contains("could not allocate a unique job ID"));

        let err = insert_short_id('j', "job", || Uuid::nil(), insert).unwrap_err();
        let sqlite = err.downcast_ref::<rusqlite::Error>().unwrap();
        assert_eq!(
            sqlite.sqlite_error().unwrap().extended_code,
            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        );
    }

    #[test]
    fn migration_retries_an_id_collision_and_preserves_memory_deduplication() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_V17).unwrap();
        conn.execute_batch(
            "ALTER TABLE memories ADD COLUMN proposal_id TEXT;
             INSERT INTO memories(id,text,dedup_key,created_at,updated_at) VALUES('m12345678','Existing note','existing note','t0','t0');
             CREATE TABLE prompt_proposals(id TEXT PRIMARY KEY, content TEXT NOT NULL, kind TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, applied_at TEXT);
             INSERT INTO prompt_proposals VALUES
               ('p1',' Existing note ','memory','applied','t1',NULL),
               ('p2',' New note ','memory','applied','t1','t2'),
               ('p3',' NEW NOTE ','memory','applied','t1','t3'),
               ('p4','Pending note','memory','pending','t1',NULL);",
        ).unwrap();
        let tx = conn.transaction().unwrap();
        let mut ids = [COLLISION, FREE].into_iter();
        migrate_applied_memories(&tx, || ids.next().unwrap()).unwrap();
        tx.commit().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let migrated: (String, String, String, String) = conn
            .query_row(
                "SELECT id, text, proposal_id, created_at FROM memories WHERE author='optimiser'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            migrated,
            (
                "m87654321".into(),
                "New note".into(),
                "p2".into(),
                "t2".into()
            )
        );
    }
}
