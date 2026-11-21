use super::*;

const LEGACY_V71: &str = r#"
CREATE TABLE profile_metadata (
  key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE topics (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, repo TEXT NOT NULL,
  base_revision TEXT NOT NULL, source_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  topic_session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE, status TEXT NOT NULL,
  builder_kind TEXT NOT NULL, builder_model TEXT, reviewer_kind TEXT NOT NULL,
  reviewer_model TEXT, redirect_message_id TEXT, rejection_reason TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE INDEX idx_topics_status_updated ON topics(status, updated_at DESC);
CREATE TABLE topic_repo_locks (
  repo TEXT PRIMARY KEY, topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
  acquired_at TEXT NOT NULL);
CREATE TABLE topic_source_messages (
  topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
  source_session_id TEXT NOT NULL, message_id TEXT NOT NULL,
  source_seq INTEGER NOT NULL, source_content TEXT NOT NULL, source_content_hash TEXT NOT NULL, moved_at TEXT,
  PRIMARY KEY(topic_id, message_id));
CREATE TABLE topic_features (
  id TEXT PRIMARY KEY, topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
  source_message_id TEXT NOT NULL, source_seq INTEGER NOT NULL, request TEXT NOT NULL,
  status TEXT NOT NULL, active_attempt_id TEXT, ready_revision TEXT,
  review_round INTEGER NOT NULL DEFAULT 0, detail TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE INDEX idx_topic_features_status ON topic_features(topic_id, status, source_seq);
CREATE TABLE topic_attempts (
  id TEXT PRIMARY KEY, topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
  feature_id TEXT NOT NULL REFERENCES topic_features(id) ON DELETE CASCADE,
  round INTEGER NOT NULL, role TEXT NOT NULL, harness_kind TEXT NOT NULL,
  status TEXT NOT NULL, conversation_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  capability_hash TEXT NOT NULL, worktree_path TEXT, base_revision TEXT,
  revision TEXT, test_report TEXT, harness_session_id TEXT, child_pid INTEGER,
  detail TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE INDEX idx_topic_attempts_feature ON topic_attempts(feature_id, created_at);
CREATE UNIQUE INDEX idx_topic_active_review_attempt_revision
  ON topic_attempts(feature_id, revision)
  WHERE role='review' AND status='reviewing' AND revision IS NOT NULL;
CREATE TABLE topic_reviews (
  id TEXT PRIMARY KEY, feature_id TEXT NOT NULL REFERENCES topic_features(id) ON DELETE CASCADE,
  attempt_id TEXT NOT NULL REFERENCES topic_attempts(id) ON DELETE CASCADE, revision TEXT NOT NULL,
  outcome TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
  UNIQUE(feature_id, revision));
CREATE INDEX idx_topic_reviews_attempt ON topic_reviews(attempt_id);"#;

fn legacy_file(v1: bool) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("fl-topic-v72-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("formless.db");
    let c = Connection::open(&file).unwrap();
    for (version, step) in MIGRATIONS {
        if version <= 70 {
            step(&c).unwrap();
        }
    }
    for column in [
        "parent_session_id TEXT",
        "session_kind TEXT NOT NULL DEFAULT 'chat'",
        "topic_id TEXT",
        "topic_attempt_id TEXT",
        "harness_kind TEXT",
    ] {
        add_column(&c, "sessions", column).unwrap();
    }
    if v1 {
        c.execute_batch(LEGACY_V71).unwrap();
    } else {
        c.execute_batch(&SCHEMA_V71.replacen(
            "source_session_id TEXT NOT NULL,",
            "source_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,",
            1,
        ))
        .unwrap();
    }
    c.execute_batch("INSERT INTO sessions(id,name,created_at,updated_at) VALUES
        ('source','Source','2026','2026'),('chat','Topic','2026','2026'),('attempt-chat','Attempt','2026','2026');
        UPDATE sessions SET session_kind='topic',parent_session_id='source',topic_id='legacy' WHERE id='chat';").unwrap();
    let revision = "a".repeat(40);
    if v1 {
        c.execute("INSERT INTO topics(id,name,repo,base_revision,source_session_id,topic_session_id,status,builder_kind,reviewer_kind,created_at,updated_at)
            VALUES('legacy','Legacy','/legacy/repo',?1,'source','chat','running','claude','codex','2026','2026')",[&revision]).unwrap();
        c.execute_batch("INSERT INTO topic_features(id,topic_id,source_message_id,source_seq,request,status,active_attempt_id,created_at,updated_at)
            VALUES('feature','legacy','message',1,'Legacy request','building','attempt','2026','2026');
            INSERT INTO topic_attempts(id,topic_id,feature_id,round,role,harness_kind,status,conversation_session_id,capability_hash,created_at,updated_at)
            VALUES('attempt','legacy','feature',1,'build','claude','building','attempt-chat','capability-hash','2026','2026');").unwrap();
    } else {
        c.execute("INSERT INTO topics(id,name,workspace,workspace_kind,base_revision,source_session_id,topic_session_id,status,created_at,updated_at)
            VALUES('legacy','Legacy','/legacy/repo','git',?1,'source','chat','approved','2026','2026')",[&revision]).unwrap();
        c.execute_batch("INSERT INTO topic_features(id,topic_id,source_message_id,source_seq,request,status,created_at,updated_at)
            VALUES('feature','legacy','message',1,'Legacy request','pending','2026','2026');").unwrap();
    }
    c.execute_batch("INSERT INTO topic_source_messages(topic_id,source_session_id,message_id,source_seq,source_content,source_content_hash,moved_at)
        VALUES('legacy','source','message',1,'Legacy request','hash','2026');PRAGMA user_version=71;").unwrap();
    (root, file)
}

#[test]
fn topic_schema_v72_upgrades_v1_v71_and_lists_topics_without_losing_conversations() {
    let (root, file) = legacy_file(true);
    let store = Store::open(&file).unwrap();
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    let topics = store.list_topics().unwrap();
    assert_eq!(topics.len(), 1);
    let topic = &topics[0];
    assert_eq!(topic.workspace.as_deref(), Some(Path::new("/legacy/repo")));
    assert_eq!(topic.status, formless_protocol::TopicStatus::NeedsUser);
    assert_eq!(topic.features[0].attempt_count, 1);
    assert_eq!(
        topic.attempts[0].status,
        formless_protocol::TopicAttemptStatus::Abandoned
    );
    assert_eq!(
        topic.attempts[0].access,
        formless_protocol::StepAccess::Write
    );
    assert!(store.get_session("attempt-chat").unwrap().is_some());
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM topic_source_messages", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    store.delete_session("source").unwrap();
    assert_eq!(store.list_topics().unwrap().len(), 1);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn topic_schema_v72_preserves_v2_v71_topics_and_relaxes_source_fk() {
    let (root, file) = legacy_file(false);
    let store = Store::open(&file).unwrap();
    assert_eq!(store.list_topics().unwrap().len(), 1);
    store.delete_session("source").unwrap();
    assert_eq!(store.list_topics().unwrap().len(), 1);
    assert_eq!(
        store
            .get_topic("legacy")
            .unwrap()
            .unwrap()
            .source_session_id,
        "source"
    );
    drop(store);
    let reopened = Store::open(&file).unwrap();
    assert!(
        !reopened
            .get_topic("legacy")
            .unwrap()
            .unwrap()
            .allow_external_context
    );
    reopened.set_topic_context_access("legacy", true).unwrap();
    drop(reopened);
    let reopened = Store::open(&file).unwrap();
    assert!(
        reopened
            .get_topic("legacy")
            .unwrap()
            .unwrap()
            .allow_external_context
    );
    assert_eq!(reopened.list_topics().unwrap().len(), 1);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn topic_schema_v72_preserves_legacy_parent_links_and_cascades() {
    for source_is_parent in [true, false] {
        let (root, file) = legacy_file(true);
        let c = Connection::open(&file).unwrap();
        c.execute_batch("INSERT INTO sessions(id,name,created_at,updated_at,parent_session_id,session_kind,topic_id)
            VALUES('child-chat','Child','2026','2026','chat','topic','child');").unwrap();
        c.execute("INSERT INTO topics(id,name,repo,base_revision,source_session_id,topic_session_id,status,builder_kind,reviewer_kind,created_at,updated_at)
            SELECT 'child','Child',repo,base_revision,?1,'child-chat','approved',builder_kind,reviewer_kind,created_at,updated_at FROM topics WHERE id='legacy'",
            [if source_is_parent {"chat"}else{"source"}]).unwrap();
        drop(c);
        let store = Store::open(&file).unwrap();
        assert_eq!(
            store
                .get_topic("child")
                .unwrap()
                .unwrap()
                .parent_topic_id
                .as_deref(),
            Some("legacy")
        );
        store.delete_session("chat").unwrap();
        assert!(store.get_topic("child").unwrap().is_none());
        assert!(store.get_session("child-chat").unwrap().is_none());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
