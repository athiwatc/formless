//! Exact owned Run facts for terminal recaps. Historical gaps are never replaced
//! by another Run or an unbounded Session-history timestamp fallback.
use super::*;

const MAX_STEER_METADATA: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct RecapRunFacts {
    pub status: String,
    pub steps: i64,
    pub started_at: chrono::DateTime<chrono::FixedOffset>,
    pub ended_at: chrono::DateTime<chrono::FixedOffset>,
    pub start_message_seq: Option<i64>,
    pub end_message_seq: Option<i64>,
}

impl RecapRunFacts {
    pub(crate) fn duration_ms(&self) -> i64 {
        (self.ended_at - self.started_at).num_milliseconds()
    }
}

impl Store {
    pub(crate) fn recap_run_facts(
        &self,
        session: &str,
        run: &str,
    ) -> Result<Option<RecapRunFacts>> {
        let row: Option<(String,i64,String,String,Option<i64>,Option<i64>)> = self.conn.query_row(
            "SELECT r.status,r.steps,r.created_at,r.updated_at,r.start_message_seq,r.end_message_seq
             FROM runs r JOIN sessions s ON s.id=r.session_id
             WHERE r.id=?1 AND r.session_id=?2 AND s.deleted_at IS NULL",
            params![run,session],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        ).optional()?;
        let Some((status, steps, start, end, start_message_seq, end_message_seq)) = row else {
            return Ok(None);
        };
        if !matches!(
            status.as_str(),
            "completed" | "paused" | "interrupted" | "failed" | "abandoned"
        ) || steps < 0
        {
            return Ok(None);
        }
        let (Ok(started_at), Ok(ended_at)) = (
            chrono::DateTime::parse_from_rfc3339(&start),
            chrono::DateTime::parse_from_rfc3339(&end),
        ) else {
            return Ok(None);
        };
        if ended_at < started_at
            || start_message_seq.is_some_and(|seq| seq < 0)
            || end_message_seq.is_some_and(|seq| seq < 0)
            || matches!((start_message_seq,end_message_seq),(Some(start),Some(end)) if end<start)
        {
            return Ok(None);
        }
        Ok(Some(RecapRunFacts {
            status,
            steps,
            started_at,
            ended_at,
            start_message_seq,
            end_message_seq,
        }))
    }

    pub(crate) fn run_steering_count(&self, session: &str, run: &str) -> Result<Option<i64>> {
        let Some(facts) = self.recap_run_facts(session, run)? else {
            return Ok(None);
        };
        // Sequence limits resolve timestamp ties for current records. Legacy
        // records retain only bounds actually present; timestamps are still
        // parsed exactly, including offsets and sub-millisecond fractions.
        let mut statement = self.ps("SELECT created_at,CASE WHEN length(CAST(meta AS BLOB))<=?4 THEN meta ELSE NULL END,
            COALESCE(length(CAST(meta AS BLOB)),0) FROM messages
            WHERE session_id=?1 AND role='user' AND (?2 IS NULL OR seq>?2) AND (?3 IS NULL OR seq<=?3) ORDER BY seq")?;
        let mut rows = statement.query(params![
            session,
            facts.start_message_seq,
            facts.end_message_seq,
            MAX_STEER_METADATA as i64
        ])?;
        let mut count = 0i64;
        while let Some(row) = rows.next()? {
            let at: String = row.get(0)?;
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(&at) else {
                continue;
            };
            if at < facts.started_at || at > facts.ended_at {
                continue;
            }
            let bytes: i64 = row.get(2)?;
            if bytes > MAX_STEER_METADATA as i64 {
                anyhow::bail!("Steering metadata exceeds the bounded recap limit");
            }
            let meta: Option<String> = row.get(1)?;
            let meta = meta
                .as_deref()
                .and_then(|meta| serde_json::from_str::<Value>(meta).ok());
            if meta
                .as_ref()
                .and_then(|meta| meta.get("steer"))
                .is_some_and(Value::is_object)
            {
                count = count
                    .checked_add(1)
                    .context("Steering recap count overflow")?;
            }
        }
        Ok(Some(count))
    }

    pub(crate) fn run_unfinished_calls(&self, session: &str, run: &str) -> Result<Option<i64>> {
        if self.recap_run_facts(session, run)?.is_none() {
            return Ok(None);
        }
        Ok(Some(self.conn.query_row(
            "SELECT COUNT(*) FROM invocations WHERE run_id=?1 AND status IN ('running','pending')",
            params![run],
            |row| row.get(0),
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Store, String, String) {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        let run = store.create_run(&session).unwrap();
        store.ex("UPDATE runs SET steps=5,status='completed',created_at='2026-11-02T12:00:00Z',updated_at='2026-11-02T12:00:10Z',start_message_seq=0,end_message_seq=NULL WHERE id=?1",params![run]).unwrap();
        (store, session, run)
    }
    fn message(store: &Store, session: &str, seq: i64, at: &str, meta: &str) {
        store.ex("INSERT INTO messages(id,session_id,seq,role,content,created_at,meta) VALUES(?1,?2,?3,'user','fixture',?4,?5)",
            params![uuid::Uuid::new_v4().to_string(),session,seq,at,meta]).unwrap();
    }

    #[test]
    fn exact_owned_terminal_facts_do_not_depend_on_recent_run_pagination() {
        let (store, session, run) = fixture();
        for _ in 0..8 {
            let newer = store.create_run(&session).unwrap();
            store
                .ex(
                    "UPDATE runs SET created_at='2026-11-02T13:00:00Z' WHERE id=?1",
                    params![newer],
                )
                .unwrap();
        }
        assert!(!store
            .run_metrics_for(Some(&session), 5, None)
            .unwrap()
            .iter()
            .any(|row| row.run_id == run));
        let facts = store.recap_run_facts(&session, &run).unwrap().unwrap();
        assert_eq!(facts.status, "completed");
        assert_eq!((facts.steps, facts.duration_ms()), (5, 10_000));
        let other = store.create_session(None).unwrap();
        assert!(store.recap_run_facts(&other.id, &run).unwrap().is_none());
        assert!(store
            .recap_run_facts(&session, "missing")
            .unwrap()
            .is_none());
        store.soft_delete_session(&session).unwrap();
        assert!(store.recap_run_facts(&session, &run).unwrap().is_none());
    }

    #[test]
    fn invalid_bounds_and_nonterminal_rows_do_not_fabricate_a_recap() {
        let (store, session, run) = fixture();
        for status in ["running", "resumed"] {
            store
                .ex(
                    "UPDATE runs SET status=?2 WHERE id=?1",
                    params![run, status],
                )
                .unwrap();
            assert!(store.recap_run_facts(&session, &run).unwrap().is_none());
        }
        for (start, end) in [
            ("unknown", "2026-11-02T12:00:10Z"),
            ("2026-11-02T12:00:10Z", "2026-11-02T12:00:00Z"),
        ] {
            store
                .ex(
                    "UPDATE runs SET status='completed',created_at=?2,updated_at=?3 WHERE id=?1",
                    params![run, start, end],
                )
                .unwrap();
            assert!(store.recap_run_facts(&session, &run).unwrap().is_none());
            assert!(store.run_steering_count(&session, &run).unwrap().is_none());
        }
    }

    #[test]
    fn steering_uses_parsed_exact_timestamps_sequence_bounds_and_real_metadata() {
        let (store, session, run) = fixture();
        let steer = r#"{"steer":{"step":2}}"#;
        message(&store, &session, 1, "2026-11-02T11:59:59.999999999Z", steer);
        message(&store, &session, 2, "2026-11-02T12:00:00Z", steer);
        message(&store, &session, 3, "2026-11-02T13:00:05+01:00", steer);
        message(&store, &session, 4, "2026-11-02T12:00:10Z", steer);
        message(&store, &session, 5, "2026-11-02T12:00:10.000000001Z", steer);
        message(
            &store,
            &session,
            6,
            "2026-11-02T12:00:04Z",
            r#"{"note":"a quoted \"steer\" string"}"#,
        );
        message(
            &store,
            &session,
            7,
            "2026-11-02T12:00:04Z",
            r#"{"steer":false}"#,
        );
        message(&store, &session, 8, "2026-11-02T12:00:04Z", "not json");
        message(&store, &session, 9, "2026-11-02T12:00:10Z", steer);
        store
            .ex(
                "UPDATE runs SET start_message_seq=2,end_message_seq=8 WHERE id=?1",
                params![run],
            )
            .unwrap();
        assert_eq!(store.run_steering_count(&session, &run).unwrap(), Some(2));
    }

    #[test]
    fn nested_terminal_calls_are_counted_and_unfinished_calls_are_separate() {
        let (store, session, run) = fixture();
        let (tool, _) = store
            .create_tool_version(
                "worker",
                "fixture",
                "{}",
                "{}",
                "function run(){return {};}",
                &Capabilities::default(),
                256,
                "agent",
            )
            .unwrap();
        for (parent, status) in [
            (None, "success"),
            (Some("parent"), "success"),
            (Some("parent"), "failure"),
            (Some("script:one"), "pending"),
            (Some("parent"), "running"),
        ] {
            store
                .record_invocation(
                    &tool.id,
                    1,
                    Some(&session),
                    Some(&run),
                    parent,
                    status,
                    "",
                    "{}",
                    "{}",
                    "",
                    1,
                    200,
                )
                .unwrap();
        }
        store
            .record_builtin_call("read_tool", &session, &run, true, "", 1)
            .unwrap();
        let calls = store.run_calls(&run).unwrap();
        assert!(calls.contains(&("worker".into(), 3, 1)));
        assert!(calls.contains(&("read_tool".into(), 1, 0)));
        assert_eq!(calls.iter().map(|(_, count, _)| count).sum::<i64>(), 4);
        assert_eq!(store.run_unfinished_calls(&session, &run).unwrap(), Some(2));
    }

    #[test]
    fn oversized_steering_metadata_fails_instead_of_silently_undercounting() {
        let (store, session, run) = fixture();
        let meta = json!({"steer":{"step":1},"padding":"x".repeat(MAX_STEER_METADATA)}).to_string();
        message(&store, &session, 1, "2026-11-02T12:00:05Z", &meta);
        let error = store.run_steering_count(&session, &run).unwrap_err();
        assert!(
            error.to_string().contains("bounded recap limit"),
            "{error:#}"
        );
    }
}
