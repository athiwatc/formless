use super::*;
use anyhow::bail;

const TABLES: &[&str] = &[
    "topics",
    "topic_source_messages",
    "topic_features",
    "topic_attempts",
    "topic_repo_locks",
];
const TOPICS:&str="id,name,parent_topic_id,workspace,workspace_kind,base_revision,outline,source_session_id,topic_session_id,status,redirect_message_id,rejection_reason,created_at,updated_at";
const SOURCES: &str =
    "topic_id,source_session_id,message_id,source_seq,source_content,source_content_hash,moved_at";
const FEATURES:&str="id,topic_id,source_message_id,source_seq,request,status,active_attempt_id,outputs,attempt_count,detail,created_at,updated_at";
const ATTEMPTS:&str="id,topic_id,feature_id,step_title,access,harness_kind,model,acp_agent,inputs,outputs,summary,status,conversation_session_id,capability_hash,worktree_path,outputs_dir,harness_session_id,child_pid,detail,created_at,updated_at";

pub(super) fn migrate(c: &Connection) -> Result<()> {
    let columns = c
        .prepare("PRAGMA table_info(topics)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if columns.is_empty() {
        return migrate_v71(c);
    }
    let v1 = columns.iter().any(|name| name == "repo");
    if !v1 && !columns.iter().any(|name| name == "workspace_kind") {
        bail!("unrecognized legacy topic schema");
    }
    if !v1 {
        add_column(
            c,
            "topics",
            "parent_topic_id TEXT REFERENCES topics(id) ON DELETE CASCADE",
        )?;
        add_column(c, "topic_attempts", "acp_agent TEXT")?;
    }
    for table in TABLES {
        c.execute_batch(&format!(
            "CREATE TEMP TABLE formless_v72_{table} AS SELECT * FROM {table}"
        ))?;
    }
    if v1 {
        c.execute_batch("CREATE TEMP TABLE formless_v72_reviews AS SELECT * FROM topic_reviews")?;
    }
    c.execute_batch(
        "DROP TABLE IF EXISTS topic_reviews;
        DROP TABLE topic_attempts;DROP TABLE topic_features;DROP TABLE topic_source_messages;
        DROP TABLE topic_repo_locks;DROP TABLE topics;",
    )?;
    c.execute_batch(SCHEMA_V71)?;
    if v1 {
        c.execute_batch(&format!("INSERT INTO topics({TOPICS})
            SELECT id,name,NULL,repo,'git',base_revision,'[]',source_session_id,topic_session_id,
            CASE WHEN status='running' THEN 'needs_user' ELSE status END,
            redirect_message_id,rejection_reason,created_at,updated_at FROM formless_v72_topics;
            INSERT INTO topic_features({FEATURES})
            SELECT id,topic_id,source_message_id,source_seq,request,
            CASE WHEN status IN ('ready','failed') THEN status ELSE 'pending' END,NULL,
            CASE WHEN ready_revision IS NOT NULL THEN json_array(json_object('kind','git_revision','revision',ready_revision)) ELSE '[]' END,
            (SELECT COUNT(*) FROM formless_v72_topic_attempts a WHERE a.feature_id=f.id),
            detail,created_at,updated_at FROM formless_v72_topic_features f;
            INSERT INTO topic_attempts({ATTEMPTS})
            SELECT a.id,a.topic_id,a.feature_id,CASE WHEN role='build' THEN 'Legacy build' ELSE 'Legacy review' END,
            CASE WHEN role='build' THEN 'write' ELSE 'read_only' END,a.harness_kind,
            CASE WHEN role='build' THEN t.builder_model ELSE t.reviewer_model END,NULL,
            CASE WHEN COALESCE(a.base_revision,a.revision) IS NOT NULL THEN
              json_array(json_object('kind','git_revision','revision',CASE WHEN role='review' THEN COALESCE(a.revision,a.base_revision) ELSE COALESCE(a.base_revision,a.revision) END))
              ELSE '[]' END,
            CASE WHEN role='build' AND a.revision IS NOT NULL THEN json_array(json_object('kind','git_revision','revision',a.revision)) ELSE '[]' END,
            COALESCE(test_report,a.detail,'Legacy harness attempt') ||
              COALESCE((SELECT char(10)||group_concat(outcome||': '||detail,char(10)) FROM formless_v72_reviews r WHERE r.attempt_id=a.id),''),
            CASE WHEN a.status IN ('awaiting_review','approved','changes_requested','succeeded') THEN 'succeeded'
              WHEN a.status IN ('failed','abandoned','cancelled') THEN a.status ELSE 'abandoned' END,
            conversation_session_id,capability_hash,worktree_path,NULL,harness_session_id,NULL,
            CASE WHEN a.status IN ('building','reviewing') THEN 'Legacy running attempt abandoned during v72 migration' ELSE a.detail END,
            a.created_at,a.updated_at FROM formless_v72_topic_attempts a JOIN formless_v72_topics t ON t.id=a.topic_id;
            INSERT INTO topic_repo_locks(workspace,topic_id,acquired_at) SELECT repo,topic_id,acquired_at FROM formless_v72_topic_repo_locks;"))?;
        c.execute_batch("DROP TABLE formless_v72_reviews")?;
    } else {
        for (table, columns) in [
            ("topics", TOPICS),
            ("topic_features", FEATURES),
            ("topic_attempts", ATTEMPTS),
            ("topic_repo_locks", "workspace,topic_id,acquired_at"),
        ] {
            c.execute_batch(&format!(
                "INSERT INTO {table}({columns}) SELECT {columns} FROM formless_v72_{table}"
            ))?;
        }
    }
    c.execute_batch(&format!("INSERT INTO topic_source_messages({SOURCES}) SELECT {SOURCES} FROM formless_v72_topic_source_messages"))?;
    for table in TABLES {
        c.execute_batch(&format!("DROP TABLE formless_v72_{table}"))?;
    }
    Ok(())
}
