//! #779: upgrade genuinely old shapes and a complete old profile, rather
//! than lowering user_version on a database that already has every column.

#[path = "support/env.rs"]
mod credential_env;
#[path = "support/historical.rs"]
mod historical;

use formless_core::daemon::Daemon;
use formless_core::storage::{newest_backup, SCHEMA_VERSION};
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use rusqlite::{params, Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SESSION: &str = "history-session";
const PREVIEW: &str = "Keep rehearsalcheckpoint invoice text";
const STAMP: &str = "2020-01-01T00:00:00Z";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let fixture =
            Self(std::env::temp_dir().join(format!("fl-history-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir(&fixture.0).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        fixture
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_only(file: &Path) -> Connection {
    Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn seed(conn: &Connection, checkpoint: i64) {
    conn.execute(
        "INSERT INTO sessions(id,name,model,reasoning_effort,created_at,updated_at)
         VALUES(?1,'Historical Session','historical-fixture-model','low',?2,?2)",
        params![SESSION, STAMP],
    )
    .unwrap();
    for (id, seq, role, content) in [
        ("history-first", 1, "user", "An earlier question"),
        ("history-answer", 2, "assistant", "Retained answer bytes"),
        ("history-last", 3, "user", PREVIEW),
    ] {
        conn.execute(
            "INSERT INTO messages(id,session_id,seq,role,content,created_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![id, SESSION, seq, role, content, STAMP],
        )
        .unwrap();
    }
    for (id, status, created) in [
        ("history-run-old", "completed", STAMP),
        ("history-run-new", "paused", "2020-01-02T00:00:00Z"),
    ] {
        conn.execute(
            "INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES(?1,?2,?3,?4,?4)",
            params![id, SESSION, status, created],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO checkpoints(id,session_id,summary,up_to_seq,created_at)
         VALUES('history-checkpoint',?1,'Retained context summary',2,?2)",
        params![SESSION, STAMP],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit(id,actor,action,detail,created_at)
         VALUES('history-audit','user','fixture.seed','Retained audit detail',?1)",
        [STAMP],
    )
    .unwrap();
    if checkpoint >= 3 {
        conn.execute(
            "INSERT INTO tools(id,name,name_lower,description,current_version,created_at,updated_at)
             VALUES('history-tool','history_tool','history_tool','Historical Tool',1,?1,?1)",
            [STAMP],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_versions(id,tool_id,version,input_schema,output_schema,code,created_at)
             VALUES('history-version','history-tool',1,'{}','{}','function run(i) { return i; }',?1)",
            [STAMP],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO invocations(id,tool_id,version,session_id,run_id,status,input,output,duration_ms,created_at)
             VALUES('history-invocation','history-tool',1,?1,'history-run-old','success','{\"n\":7}','{\"n\":7}',12,?2)",
            params![SESSION, STAMP],
        )
        .unwrap();
    }
    if checkpoint >= 17 {
        conn.execute(
            "INSERT INTO memories(id,text,dedup_key,scope,author,created_at,updated_at)
             VALUES('history-memory','Keep the original memory','keep the original memory','global','user',?1,?1)",
            [STAMP],
        )
        .unwrap();
    }
    if checkpoint == 17 {
        // v18 must backfill an already-applied memory proposal; seeding it
        // after replaying v18 would never exercise the migration's SELECT.
        conn.execute(
            "INSERT INTO prompt_proposals(id,kind,content,rationale,status,created_at,applied_at)
             VALUES('history-proposal','memory','  Prefer concise fixture answers  ','fixture','applied',?1,?1)",
            [STAMP],
        )
        .unwrap();
    }
    if checkpoint >= 23 {
        conn.execute(
            "INSERT INTO tool_kv(namespace,key,value,updated_at) VALUES('history-tool','cursor','{\"offset\":7}',?1)",
            [STAMP],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs(id,prompt,session_id,enabled,created_at)
             VALUES('history-job','Retained disabled job',?1,0,?2)",
            params![SESSION, STAMP],
        )
        .unwrap();
    }
    if checkpoint >= 46 {
        conn.execute(
            "INSERT INTO run_usage(session_id,run_id,model,prompt_tokens,completion_tokens,reasoning_tokens,cached_tokens,created_at,cost,cache_write_tokens)
             VALUES(?1,'history-run-old','historical-fixture-model',101,17,3,11,?2,0.25,7)",
            params![SESSION, STAMP],
        )
        .unwrap();
    }
}

fn assert_retained(conn: &Connection, checkpoint: i64) {
    let session: (String, String, String) = conn
        .query_row(
            "SELECT name,model,reasoning_effort FROM sessions WHERE id=?1",
            [SESSION],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        session,
        (
            "Historical Session".into(),
            "historical-fixture-model".into(),
            "low".into()
        )
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1",
            [SESSION],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
    let content: String = conn
        .query_row(
            "SELECT content FROM messages WHERE id='history-answer'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "Retained answer bytes");
    let runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 2);
    let summary: String = conn
        .query_row(
            "SELECT summary FROM checkpoints WHERE id='history-checkpoint'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(summary, "Retained context summary");
    let audit: String = conn
        .query_row(
            "SELECT detail FROM audit WHERE id='history-audit'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audit, "Retained audit detail");
    if checkpoint >= 3 {
        let invocation: (String, String, i64) = conn
            .query_row(
                "SELECT input,output,duration_ms FROM invocations WHERE id='history-invocation'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(invocation, ("{\"n\":7}".into(), "{\"n\":7}".into(), 12));
    }
    if checkpoint >= 17 {
        let memory: String = conn
            .query_row(
                "SELECT text FROM memories WHERE id='history-memory'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(memory, "Keep the original memory");
    }
    if checkpoint >= 23 {
        let cursor: String = conn
            .query_row(
                "SELECT value FROM tool_kv WHERE namespace='history-tool' AND key='cursor'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, "{\"offset\":7}");
        let job: (String, bool) = conn
            .query_row(
                "SELECT prompt,enabled FROM jobs WHERE id='history-job'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(job, ("Retained disabled job".into(), false));
    }
    if checkpoint >= 46 {
        let usage: (i64, i64, i64, i64, f64, i64) = conn
            .query_row("SELECT prompt_tokens,completion_tokens,reasoning_tokens,cached_tokens,cost,cache_write_tokens FROM run_usage WHERE run_id='history-run-old'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))
            .unwrap();
        assert_eq!(usage, (101, 17, 3, 11, 0.25, 7));
    }
}

fn assert_integrity(conn: &Connection) {
    let mut check = conn.prepare("PRAGMA quick_check").unwrap();
    let rows: Vec<String> = check
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, ["ok"]);
    let broken: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
}

fn snapshots(db: &Path) -> Vec<PathBuf> {
    let prefix = format!("{}.v", db.file_name().unwrap().to_string_lossy());
    let mut files: Vec<PathBuf> = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&prefix)
                && path.extension().is_some_and(|suffix| suffix == "bak")
        })
        .collect();
    files.sort();
    files
}

fn rehearse(checkpoint: i64) {
    let fixture = Fixture::new();
    let db = fixture.0.join("formless.db");
    {
        let old = historical::create(&db, checkpoint);
        seed(&old, checkpoint);
        assert_retained(&old, checkpoint);
        assert_integrity(&old);
    }
    let store = Store::open(&db).unwrap();
    let upgraded = read_only(&db);
    assert_eq!(historical::version(&upgraded), SCHEMA_VERSION);
    // Old profiles gain isolated calibration state without rewriting their
    // historical usage or their pre-migration recovery snapshot.
    assert_eq!(store.token_ratio("fixture-provider", "model").unwrap(), 1.0);
    store
        .observe_prompt_tokens("fixture-provider", "model", 100, 300)
        .unwrap();
    assert!((store.token_ratio("fixture-provider", "model").unwrap() - 1.4).abs() < 1e-9);
    assert_retained(&upgraded, checkpoint);
    assert_integrity(&upgraded);
    let session = store.get_session(SESSION).unwrap().unwrap();
    assert_eq!(
        session.forked_from, None,
        "old histories do not invent parents"
    );
    assert_eq!(session.fork_point_seq, None);
    let metadata_columns: i64 = upgraded
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('run_files') WHERE name='before_metadata'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        metadata_columns, 1,
        "real historical DDL gains the nullable metadata column"
    );
    let invented_metadata: i64 = upgraded
        .query_row(
            "SELECT COUNT(*) FROM run_files WHERE before_metadata IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        invented_metadata, 0,
        "an upgrade cannot invent prior file permissions"
    );
    let anchors: (Option<String>, Option<i64>, Option<String>, Option<i64>) = upgraded.query_row("SELECT start_message_id,start_message_seq,end_message_id,end_message_seq FROM runs WHERE id='history-run-old'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(
        anchors,
        (None, None, None, None),
        "old Run boundaries cannot be reconstructed from timestamps"
    );
    assert_eq!(session.message_count, 3);
    assert_eq!(session.preview.as_deref(), Some(PREVIEW));
    assert_eq!(session.last_run_status.as_deref(), Some("paused"));
    let hits = store.search_sessions("rehearsalcheckpoint", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id, "history-last");
    if checkpoint >= 3 {
        let (tool, version) = store.get_tool_by_name("history_tool").unwrap().unwrap();
        assert_eq!(tool.id, "history-tool");
        assert_eq!(version.code, "function run(i) { return i; }");
        let metrics = store.tool_metrics(&tool.id, Some(1)).unwrap();
        assert_eq!((metrics.total, metrics.success, metrics.failure), (1, 1, 0));
    }
    if checkpoint == 17 {
        let migrated: (String, String) = upgraded
            .query_row(
                "SELECT text,author FROM memories WHERE proposal_id='history-proposal'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            migrated,
            ("Prefer concise fixture answers".into(), "optimiser".into())
        );
        let count: i64 = upgraded
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE proposal_id='history-proposal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    if checkpoint >= 46 {
        let attribution: (String, String) = upgraded
            .query_row(
                "SELECT source,provider FROM run_usage WHERE run_id='history-run-old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(attribution, ("run".into(), "".into()));
        let usage = store.run_usage("history-run-old").unwrap();
        assert_eq!((usage.prompt_tokens, usage.cache_write_tokens), (101, 7));
    }
    let before = snapshots(&db);
    assert_eq!(before.len(), 1);
    assert_eq!(newest_backup(&db).as_ref(), before.first());
    {
        // Never open a recovery snapshot with Store: that would upgrade the
        // very old shape this assertion is meant to preserve and inspect.
        let backup = read_only(&before[0]);
        historical::assert_old_shape(&backup, checkpoint);
        assert_retained(&backup, checkpoint);
        assert_integrity(&backup);
    }
    // The migrated FTS index and summary triggers must remain live, not only
    // produce a correct one-time backfill.
    store
        .append_message(SESSION, "user", "postupgrade searchable text")
        .unwrap();
    let updated = store.get_session(SESSION).unwrap().unwrap();
    assert_eq!(updated.message_count, 4);
    assert_eq!(
        updated.preview.as_deref(),
        Some("postupgrade searchable text")
    );
    assert_eq!(store.search_sessions("postupgrade", 10).unwrap().len(), 1);
    drop(upgraded);
    drop(store);
    let reopened = Store::open(&db).unwrap();
    assert_eq!(
        reopened
            .get_session(SESSION)
            .unwrap()
            .unwrap()
            .message_count,
        4
    );
    assert_eq!(
        snapshots(&db),
        before,
        "up-to-date reopen made another snapshot"
    );
}

macro_rules! checkpoint_tests {
    ($($name:ident: $version:literal),+ $(,)?) => {$(
        #[test]
        fn $name() { rehearse($version); }
    )+};
}
checkpoint_tests! {
    genuine_v1_upgrades_with_seeded_history: 1,
    genuine_v3_upgrades_with_seeded_history: 3,
    genuine_v17_backfills_applied_memory: 17,
    genuine_v23_backfills_search: 23,
    genuine_v38_backfills_session_summaries: 38,
    genuine_v46_preserves_usage_through_table_rebuild: 46,
}

#[tokio::test]
async fn whole_v23_profile_answers_status_migrates_consent_and_preserves_key_and_artifact() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    let fixture = Fixture::new();
    let paths = Paths::from_dirs(fixture.0.join("data"), fixture.0.join("config")).unwrap();
    let artifact = paths.data_dir.join("artifacts/legacy.txt");
    let bytes: &[u8] = include_bytes!("fixtures/historical-upgrades/artifact.txt");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, bytes).unwrap();
    {
        let old = historical::create(&paths.db_file(), 23);
        seed(&old, 23);
        old.execute("INSERT INTO artifacts(id,session_id,path,preview,bytes,created_at) VALUES('history-artifact',?1,?2,'Historical artifact',?3,?4)",
            params![SESSION, artifact.to_str().unwrap(), bytes.len() as i64, STAMP]).unwrap();
    }
    let config: &[u8] = include_bytes!("fixtures/historical-upgrades/config-v1.toml");
    std::fs::write(paths.config_file(), config).unwrap();
    let key = "fake-historical-rehearsal-key";
    formless_core::config::write_secret_file(&paths.key_fallback_file(), key).unwrap();
    assert_eq!(
        formless_core::config::api_key_source(&paths).0,
        "fallback file"
    );
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        daemon.handle(Request::Status),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(status["sessions"], 1);
    assert_eq!(status["auth"]["credential_present"], true);
    assert!(status["running_sessions"].as_array().unwrap().is_empty());
    let loaded = Config::load_file_only(&paths).unwrap();
    assert_eq!(loaded.provider.model, "historical-fixture-model");
    assert_eq!(loaded.provider.context_tokens, 8192);
    let persisted_config: toml::Value =
        toml::from_str(&std::fs::read_to_string(paths.config_file()).unwrap()).unwrap();
    assert_eq!(
        persisted_config["provider"]["model"].as_str(),
        Some("historical-fixture-model")
    );
    assert_eq!(
        persisted_config["provider"]["context_tokens"].as_integer(),
        Some(8192)
    );
    assert_eq!(
        persisted_config["optimiser"]["cooldown_secs"].as_integer(),
        Some(3600)
    );
    assert_eq!(
        persisted_config["naming"]["auto_download"].as_bool(),
        Some(false)
    );
    assert_eq!(
        persisted_config["naming"]["auto_download_migration_done"].as_bool(),
        Some(true)
    );
    assert_eq!(
        std::fs::read(paths.key_fallback_file()).unwrap(),
        key.as_bytes()
    );
    assert_eq!(std::fs::read(&artifact).unwrap(), bytes);
    let upgraded = read_only(&paths.db_file());
    assert_eq!(historical::version(&upgraded), SCHEMA_VERSION);
    assert_retained(&upgraded, 23);
    assert_integrity(&upgraded);
    drop(upgraded);
    let store = Store::open(&paths.db_file()).unwrap();
    let saved = store.get_artifact("history-artifact").unwrap().unwrap();
    assert_eq!(
        saved,
        (
            Some(SESSION.into()),
            artifact.to_string_lossy().into_owned(),
            bytes.len() as i64
        )
    );
    assert_eq!(
        store
            .search_sessions("rehearsalcheckpoint", 10)
            .unwrap()
            .len(),
        1
    );
    let backup = read_only(&newest_backup(&paths.db_file()).unwrap());
    historical::assert_old_shape(&backup, 23);
    assert_retained(&backup, 23);
    assert_integrity(&backup);
    let saved_path: String = backup
        .query_row(
            "SELECT path FROM artifacts WHERE id='history-artifact'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(saved_path, artifact.to_string_lossy());
    drop(backup);
    drop(store);
    drop(daemon);
}
