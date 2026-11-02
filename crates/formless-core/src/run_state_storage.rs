//! Owned durable Run recaps and message-bound plan revisions. Legacy plan data
//! remains readable without inventing a Run owner or a historical boundary.
use super::*;
use formless_protocol::{PlanItem, PlanState, RecordedRunRecap, RunRecap};

pub(super) const SCHEMA: &str = r#"
CREATE TABLE session_plan_revisions (
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 anchor_seq INTEGER NOT NULL CHECK(anchor_seq>=0),
 anchor_message_id TEXT REFERENCES messages(id) ON DELETE CASCADE,
 items TEXT NOT NULL, state TEXT NOT NULL, created_at TEXT NOT NULL,
 PRIMARY KEY(session_id,anchor_seq)
);
CREATE TABLE run_recaps (
 run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 recap TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE INDEX idx_run_recaps_session ON run_recaps(session_id);
CREATE TRIGGER immutable_run_recap BEFORE UPDATE ON run_recaps
 BEGIN SELECT RAISE(ABORT,'Recorded Run recap is immutable'); END;
"#;
const MAX_PLAN_BYTES: usize = 64 * 1024;
const MAX_RECAP_BYTES: usize = 64 * 1024;

fn terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "paused" | "interrupted" | "failed" | "abandoned"
    )
}
fn plan_json(items: &[PlanItem]) -> Result<String> {
    if items.len() > crate::tools::MAX_PLAN_ITEMS {
        anyhow::bail!("at most {} plan steps", crate::tools::MAX_PLAN_ITEMS);
    }
    if items
        .iter()
        .filter(|item| item.status == "in_progress")
        .count()
        > 1
    {
        anyhow::bail!("only one plan step may be in_progress");
    }
    for item in items {
        if !matches!(
            item.status.as_str(),
            "pending" | "in_progress" | "completed"
        ) {
            anyhow::bail!("invalid plan step status");
        }
        if item.content.len() > crate::tools::MAX_PLAN_TEXT * 4
            || item.content.chars().count() > crate::tools::MAX_PLAN_TEXT
            || item.content.trim().is_empty()
        {
            anyhow::bail!(
                "plan step content must be nonempty and at most {} characters",
                crate::tools::MAX_PLAN_TEXT
            );
        }
    }
    let json = serde_json::to_string(items)?;
    if json.len() > MAX_PLAN_BYTES {
        anyhow::bail!("plan exceeds the 64 KiB storage limit");
    }
    Ok(json)
}
fn portable_plan(value: &Value) -> Result<Vec<PlanItem>> {
    let values = value.as_array().context("plan items must be an array")?;
    if values.len() > crate::tools::MAX_PLAN_ITEMS {
        anyhow::bail!("too many portable plan items");
    }
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        let content = value["content"]
            .as_str()
            .context("plan step content required")?;
        let status = value["status"]
            .as_str()
            .context("plan step status required")?;
        if content.len() > crate::tools::MAX_PLAN_TEXT * 4 || status.len() > 32 {
            anyhow::bail!("portable plan item exceeds its bounded fields");
        }
        items.push(PlanItem {
            content: content.to_owned(),
            status: status.to_owned(),
        });
    }
    plan_json(&items)?;
    Ok(items)
}

fn detached_state(state: Option<PlanState>, origin: &str, nonempty: bool) -> PlanState {
    PlanState {
        run_id: None,
        updated_at: state.and_then(|state| state.updated_at),
        run_status: None,
        previous_task: nonempty,
        origin: origin.into(),
    }
}
impl Store {
    fn live_plan_session(&self, session: &str) -> Result<()> {
        let live: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND deleted_at IS NULL)",
            params![session],
            |r| r.get(0),
        )?;
        if !live {
            anyhow::bail!("Session not found or deleted");
        }
        Ok(())
    }
    fn owned_state_run(&self, session: &str, run: &str) -> Result<String> {
        self.live_plan_session(session)?;
        self.conn
            .query_row(
                "SELECT status FROM runs WHERE id=?1 AND session_id=?2",
                params![run, session],
                |r| r.get(0),
            )
            .optional()?
            .context("Run does not belong to this Session")
    }
    fn current_plan_run(&self, session: &str, run: &str) -> Result<()> {
        if self.owned_state_run(session, run)? != "running"
            || self.last_run(session)?.as_ref().map(|(id, _)| id.as_str()) != Some(run)
        {
            anyhow::bail!("Only the current owned running Run may update the plan; background or stale Runs must not replace the main plan");
        }
        Ok(())
    }
    fn stored_plan(&self, session: &str) -> Result<(Vec<PlanItem>, Option<PlanState>)> {
        self.live_plan_session(session)?;
        let (items,state):(Option<String>,Option<String>)=self.conn.query_row("SELECT CASE WHEN length(CAST(plan AS BLOB))<=65536 THEN plan ELSE NULL END,CASE WHEN length(CAST(plan_state AS BLOB))<=65536 THEN plan_state ELSE NULL END FROM sessions WHERE id=?1 AND (plan IS NULL OR length(CAST(plan AS BLOB))<=65536) AND (plan_state IS NULL OR length(CAST(plan_state AS BLOB))<=65536)",params![session],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.context("Stored plan exceeds the bounded read limit")?;
        let items = items
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let state = state.as_deref().map(serde_json::from_str).transpose()?;
        Ok((items, state))
    }
    pub fn session_plan_snapshot(
        &self,
        session: &str,
    ) -> Result<(Vec<PlanItem>, Option<PlanState>)> {
        self.read_transaction(|st| {
            let (items, mut state) = st.stored_plan(session)?;
            if state.is_none() && !items.is_empty() {
                state = Some(PlanState {
                    origin: "legacy".into(),
                    previous_task: true,
                    ..Default::default()
                });
            }
            if let Some(state) = state.as_mut() {
                state.run_status = match state.run_id.as_deref() {
                    Some(run) => st
                        .conn
                        .query_row(
                            "SELECT status FROM runs WHERE id=?1 AND session_id=?2",
                            params![run, session],
                            |r| r.get(0),
                        )
                        .optional()?,
                    None => None,
                };
                state.previous_task = match state.run_id.as_deref() {
                    Some(run) => {
                        st.last_run(session)?.as_ref().map(|(id, _)| id.as_str()) != Some(run)
                    }
                    None => !items.is_empty(),
                };
            }
            Ok((items, state))
        })
    }
    pub fn session_plan_state(&self, session: &str) -> Result<Option<PlanState>> {
        self.session_plan_snapshot(session).map(|(_, state)| state)
    }
    fn write_plan_revision(
        &self,
        session: &str,
        items: &[PlanItem],
        state: &PlanState,
    ) -> Result<()> {
        let items = plan_json(items)?;
        let state = serde_json::to_string(state)?;
        if state.len() > MAX_PLAN_BYTES {
            anyhow::bail!("plan metadata exceeds the storage limit");
        }
        let anchor:Option<(String,i64)>=self.conn.query_row("SELECT id,seq FROM messages WHERE session_id=?1 ORDER BY seq DESC,rowid DESC LIMIT 1",params![session],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (anchor_id, anchor_seq) = match anchor {
            Some((id, seq)) => (Some(id), seq),
            None => (None, 0),
        };
        self.ex("INSERT INTO session_plan_revisions(session_id,anchor_seq,anchor_message_id,items,state,created_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(session_id,anchor_seq) DO UPDATE SET anchor_message_id=excluded.anchor_message_id,items=excluded.items,state=excluded.state,created_at=excluded.created_at",params![session,anchor_seq,anchor_id,items,state,now()])?;
        self.ex(
            "UPDATE sessions SET plan=?2,plan_state=?3 WHERE id=?1",
            params![session, items, state],
        )?;
        Ok(())
    }
    pub fn set_run_plan(&self, session: &str, run: &str, items: &[PlanItem]) -> Result<()> {
        plan_json(items)?;
        self.transaction(|st| {
            st.current_plan_run(session, run)?;
            let state = PlanState {
                run_id: Some(run.into()),
                updated_at: Some(now()),
                run_status: Some("running".into()),
                previous_task: false,
                origin: "live".into(),
            };
            let (previous, metadata) = st.stored_plan(session)?;
            if previous == items
                && metadata
                    .as_ref()
                    .is_some_and(|meta| meta.run_id.as_deref() == Some(run))
            {
                return Ok(());
            }
            st.write_plan_revision(session, items, &state)
        })
    }
    /// Legacy/fixture entry point with no Run authority. Live agents use set_run_plan.
    pub(super) fn set_legacy_plan(&self, session: &str, items: &[PlanItem]) -> Result<()> {
        plan_json(items)?;
        self.transaction(|st| {
            st.live_plan_session(session)?;
            if st
                .last_run(session)?
                .is_some_and(|(_, status)| status == "running")
            {
                anyhow::bail!("A live plan requires its current Run owner");
            }
            let state = PlanState {
                updated_at: Some(now()),
                previous_task: !items.is_empty(),
                origin: "legacy".into(),
                ..Default::default()
            };
            st.write_plan_revision(session, items, &state)
        })
    }
    pub fn resume_session_plan(&self, session: &str, old_run: &str, new_run: &str) -> Result<()> {
        self.transaction(|st| {
            st.owned_state_run(session, old_run)?;
            st.current_plan_run(session, new_run)?;
            if old_run == new_run {
                anyhow::bail!("Resume requires a new Run identity");
            }
            let (items, state) = st.stored_plan(session)?;
            if !state
                .as_ref()
                .is_some_and(|state| state.run_id.as_deref() == Some(old_run))
            {
                return Ok(());
            }
            let state = PlanState {
                run_id: Some(new_run.into()),
                updated_at: Some(now()),
                run_status: Some("running".into()),
                previous_task: false,
                origin: "resumed".into(),
            };
            st.write_plan_revision(session, &items, &state)
        })
    }
    /// An edited message is a new branch boundary, not evidence for plans that
    /// were written against its old text or the now-deleted later transcript.
    pub(super) fn truncate_plan_revisions(&self, session: &str, edited_seq: i64) -> Result<()> {
        self.ex(
            "DELETE FROM session_plan_revisions WHERE session_id=?1 AND anchor_seq>=?2",
            params![session, edited_seq],
        )?;
        let row:Option<(String,String)>=self.conn.query_row("SELECT CASE WHEN length(CAST(items AS BLOB))<=65536 THEN items ELSE NULL END,CASE WHEN length(CAST(state AS BLOB))<=65536 THEN state ELSE NULL END FROM session_plan_revisions WHERE session_id=?1 ORDER BY anchor_seq DESC LIMIT 1",params![session],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        match row {
            Some((items, state)) => {
                self.ex(
                    "UPDATE sessions SET plan=?2,plan_state=?3 WHERE id=?1",
                    params![session, items, state],
                )?;
            }
            None => {
                self.ex(
                    "UPDATE sessions SET plan=NULL,plan_state=NULL WHERE id=?1",
                    params![session],
                )?;
            }
        }
        Ok(())
    }
    pub(super) fn copy_plan_at(
        &self,
        source: &str,
        target: &str,
        point: i64,
        complete: bool,
    ) -> Result<()> {
        let mut query=self.ps("SELECT p.anchor_seq,CASE WHEN length(CAST(p.items AS BLOB))<=65536 THEN p.items ELSE NULL END,CASE WHEN length(CAST(p.state AS BLOB))<=65536 THEN p.state ELSE NULL END FROM session_plan_revisions p WHERE p.session_id=?1 AND p.anchor_seq<=?2 AND (p.anchor_seq=0 OR EXISTS(SELECT 1 FROM messages m WHERE m.id=p.anchor_message_id AND m.session_id=p.session_id AND m.seq=p.anchor_seq)) ORDER BY p.anchor_seq")?;
        let rows = query.query_map(params![source, point], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut selected = None;
        for row in rows {
            let (seq, items, state) = row?;
            if items.len() > MAX_PLAN_BYTES || state.len() > MAX_PLAN_BYTES {
                anyhow::bail!("Stored plan revision exceeds the bounded copy limit");
            }
            let parsed: Vec<PlanItem> = serde_json::from_str(&items)?;
            plan_json(&parsed)?;
            let state = detached_state(
                Some(serde_json::from_str(&state)?),
                "copied",
                !parsed.is_empty(),
            );
            let state = serde_json::to_string(&state)?;
            let anchor:Option<String>=self.conn.query_row("SELECT id FROM messages WHERE session_id=?1 AND seq=?2 ORDER BY rowid DESC LIMIT 1",params![target,seq],|r|r.get(0)).optional()?;
            if seq != 0 && anchor.is_none() {
                continue;
            }
            self.ex("INSERT INTO session_plan_revisions(session_id,anchor_seq,anchor_message_id,items,state,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![target,seq,anchor,items,state,now()])?;
            selected = Some((items, state));
        }
        if let Some((items, state)) = selected {
            self.ex(
                "UPDATE sessions SET plan=?2,plan_state=?3 WHERE id=?1",
                params![target, items, state],
            )?;
        } else if complete {
            let (items, state) = self.stored_plan(source)?;
            // Only an actual legacy row has unknown history. New typed state
            // must be copied from its message-bound revisions, never a later field.
            if state.is_none() && !items.is_empty() {
                let state = detached_state(None, "copied", true);
                self.write_plan_revision(target, &items, &state)?;
            }
        }
        Ok(())
    }
    /// Portable plan data contains no local authority. Message sequence numbers
    /// are mapped explicitly by the importer, never inferred from timestamps.
    pub fn session_plan_bundle(&self, session: &str) -> Result<Value> {
        self.read_transaction(|st| {
            let (items,state)=st.session_plan_snapshot(session)?;
            let mut query=st.ps("SELECT anchor_seq,CASE WHEN length(CAST(items AS BLOB))<=65536 THEN items ELSE NULL END,CASE WHEN length(CAST(state AS BLOB))<=65536 THEN state ELSE NULL END FROM session_plan_revisions WHERE session_id=?1 ORDER BY anchor_seq")?;
            let rows=query.query_map(params![session],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?;
            let mut history=Vec::new();
            for row in rows {
                let (seq,items,state)=row?;
                if items.len()>MAX_PLAN_BYTES || state.len()>MAX_PLAN_BYTES {anyhow::bail!("Plan revision exceeds the portable limit");}
                history.push(json!({"anchor_seq":seq,"items":serde_json::from_str::<Vec<PlanItem>>(&items)?,"state":serde_json::from_str::<PlanState>(&state)?}));
            }
            Ok(json!({"plan":items,"plan_state":state,"plan_history":history}))
        })
    }
    pub fn import_session_plan_bundle(
        &self,
        session: &str,
        bundle: &Value,
        source_seqs: &[i64],
    ) -> Result<()> {
        if bundle.get("plan_history").is_some()
            && bundle.get("plan").is_none()
            && bundle.get("plan_state").is_none()
        {
            anyhow::bail!("Plan history requires an explicit current plan or plan_state; history-only imports are ambiguous");
        }
        let items = bundle
            .get("plan")
            .map(portable_plan)
            .transpose()?
            .unwrap_or_default();
        self.transaction(|st| {
            st.live_plan_session(session)?;
            let existing:bool=st.conn.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE session_id=?1) OR EXISTS(SELECT 1 FROM session_plan_revisions WHERE session_id=?1) OR EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND (plan IS NOT NULL OR plan_state IS NOT NULL))",params![session],|r|r.get(0))?;
            if existing {anyhow::bail!("Imported plan may only initialize a new Session without local Run authority");}
            let messages=st.list_messages(session)?;
            if messages.len()!=source_seqs.len() {anyhow::bail!("Imported plan message mapping is incomplete");}
            let mut mapping=std::collections::BTreeMap::new();
            let mut previous=0;
            for (source,message) in source_seqs.iter().zip(&messages) {
                if *source<=previous || mapping.insert(*source,(message.id.clone(),message.seq)).is_some() {anyhow::bail!("Imported message sequence mapping is ambiguous or out of order");}
                previous=*source;
            }
            if let Some(history)=bundle.get("plan_history") {
                let history=history.as_array().context("plan_history must be an array")?;
                if history.len()>messages.len()+1 {anyhow::bail!("Plan history exceeds retained message boundaries");}
                let mut seen=std::collections::BTreeSet::new();
                for revision in history {
                    let source_seq=revision["anchor_seq"].as_i64().context("Plan revision boundary required")?;
                    if !seen.insert(source_seq) {anyhow::bail!("Duplicate plan revision boundary");}
                    let revision_items=portable_plan(&revision["items"])?;
                    let json=plan_json(&revision_items)?;
                    let (anchor,seq)=if source_seq==0 {(None,0)} else {let (id,seq)=mapping.get(&source_seq).context("Plan revision points outside imported messages")?;(Some(id.clone()),*seq)};
                    let state=detached_state(None,"imported",!revision_items.is_empty());
                    st.ex("INSERT INTO session_plan_revisions(session_id,anchor_seq,anchor_message_id,items,state,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![session,seq,anchor,json,serde_json::to_string(&state)?,now()])?;
                }
            }
            if bundle.get("plan").is_some() || bundle.get("plan_state").is_some() {
                let state=detached_state(None,"imported",!items.is_empty());
                st.write_plan_revision(session,&items,&state)?;
            }
            Ok(())
        })
    }
    pub fn record_run_recap(&self, session: &str, run: &str, recap: &RunRecap) -> Result<()> {
        let json = serde_json::to_string(recap)?;
        if json.len() > MAX_RECAP_BYTES {
            anyhow::bail!("Run recap exceeds the 64 KiB storage limit");
        }
        self.transaction(|st| {
            let status = st.owned_state_run(session, run)?;
            if !terminal(&status) || recap.status != status {
                anyhow::bail!("A recorded recap requires the matching owned terminal Run status");
            }
            if recap.steps < 0
                || recap.duration_ms < 0
                || recap.steered < 0
                || recap.unfinished_calls < 0
                || recap
                    .total_calls
                    .is_some_and(|count| count < recap.unfinished_calls)
            {
                anyhow::bail!("Invalid Run recap counters");
            }
            let previous: Option<String> = st
                .conn
                .query_row(
                    "SELECT recap FROM run_recaps WHERE run_id=?1 AND session_id=?2",
                    params![run, session],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(previous) = previous {
                if previous != json {
                    anyhow::bail!("Recorded Run recap is immutable");
                }
                return Ok(());
            }
            st.ex(
                "INSERT INTO run_recaps(run_id,session_id,recap,created_at) VALUES(?1,?2,?3,?4)",
                params![run, session, json, now()],
            )?;
            Ok(())
        })
    }
    pub fn recorded_run_recap(&self, session: &str, run: &str) -> Result<Option<RecordedRunRecap>> {
        self.read_transaction(|st| {
            st.owned_state_run(session,run)?;
            let raw:Option<Option<String>>=st.conn.query_row("SELECT CASE WHEN length(CAST(recap AS BLOB))<=65536 THEN recap ELSE NULL END FROM run_recaps WHERE run_id=?1 AND session_id=?2",params![run,session],|r|r.get(0)).optional()?;
            match raw {
                None=>Ok(None),
                Some(Some(raw))=>Ok(Some(RecordedRunRecap{run_id:run.into(),recap:serde_json::from_str(&raw)?})),
                Some(None)=>Err(anyhow!("Stored Run recap exceeds the bounded read limit")),
            }
        })
    }
    pub fn latest_recorded_run_recap(&self, session: &str) -> Result<Option<RecordedRunRecap>> {
        self.read_transaction(|st| {
            st.live_plan_session(session)?;
            match st.last_run(session)? {
                Some((run, _)) => st.recorded_run_recap(session, &run),
                None => Ok(None),
            }
        })
    }
}

#[cfg(test)]
#[path = "run_state_storage_tests.rs"]
mod tests;
