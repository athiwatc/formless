//! Correlated routing receipts. No timestamp join to the billing ledger and no
//! fabricated identity/cost backfill for historical model_events/task_stats.
use super::*;
use crate::{
    routing_observation::{Identity, Outcome, Record},
    routing_policy::{usage_usd, Stat},
    routing_recorder::Receipt,
    routing_task_observation::{Event, TaskOutcome, TaskRecord},
};
use std::collections::BTreeMap;

pub(super) const SCHEMA: &str = "
CREATE TABLE routing_calls (
 id TEXT PRIMARY KEY, finished_at_ms INTEGER NOT NULL,
 provider TEXT, endpoint TEXT, model TEXT, payload TEXT NOT NULL);
CREATE INDEX routing_calls_window ON routing_calls(provider,endpoint,finished_at_ms);
CREATE TABLE routing_tasks (
 id TEXT PRIMARY KEY, finished_at_ms INTEGER NOT NULL, role TEXT,
 provider TEXT, endpoint TEXT, model TEXT, final INTEGER NOT NULL CHECK(final IN(0,1)),
 payload TEXT NOT NULL);
CREATE INDEX routing_tasks_window ON routing_tasks(role,provider,endpoint,finished_at_ms);
CREATE TABLE routing_call_tasks (
 call_id TEXT NOT NULL, task_id TEXT NOT NULL, PRIMARY KEY(call_id,task_id));
CREATE INDEX routing_calls_for_task ON routing_call_tasks(task_id,call_id);
";
fn eligible(record: &Record) -> Option<(&Identity, bool)> {
    if record.truncated {
        return None;
    }
    match &record.outcome {
        Outcome::Success(identity) => Some((identity, true)),
        Outcome::ContractFailure(identity) => Some((identity, false)),
        Outcome::Neutral(_) => None,
    }
}
#[derive(Default)]
struct Cost {
    usd: Option<f64>,
    tokens: Option<f64>,
}
impl Cost {
    fn zero() -> Self {
        Self {
            usd: Some(0.0),
            tokens: Some(0.0),
        }
    }
    fn add_call(&mut self, record: &Record, cfg: &crate::Config) {
        if record.truncated {
            self.usd = None;
            self.tokens = None;
        }
        for attempt in record.attempts.iter().filter(|a| a.sent) {
            let usage = attempt.usage.as_ref();
            let usd = usage.filter(|_| attempt.response_complete).and_then(|u| {
                // Reported money survives endpoint removal; token repricing
                // needs the same current endpoint identity, not just a name.
                if let Some(cost) = u.cost.filter(|c| c.is_finite() && *c >= 0.0) {
                    return Some(cost);
                }
                let pcfg = cfg.provider_cfg(Some(&attempt.identity.provider)).ok()?;
                (crate::tokens::provider_key(&attempt.identity.provider, &pcfg)
                    == attempt.identity.endpoint)
                    .then(|| usage_usd(cfg, &attempt.identity.provider, &attempt.identity.model, u))
                    .flatten()
            });
            self.usd = self
                .usd
                .zip(usd)
                .map(|(a, b)| a + b)
                .filter(|n| n.is_finite());
            let tokens = usage
                .filter(|u| attempt.response_complete && u.reported & 3 == 3)
                .and_then(|u| u.prompt_tokens.checked_add(u.completion_tokens))
                .map(|n| n as f64);
            self.tokens = self
                .tokens
                .zip(tokens)
                .map(|(a, b)| a + b)
                .filter(|n| n.is_finite());
        }
    }
}
fn accumulate(
    stats: &mut BTreeMap<String, Stat>,
    identity: &Identity,
    good: bool,
    ms: Option<u64>,
    cost: Cost,
) {
    let stat = stats.entry(identity.model.clone()).or_insert_with(|| Stat {
        model: identity.model.clone(),
        usd: Some(0.0),
        total_tokens: Some(0.0),
        ..Default::default()
    });
    stat.samples += 1;
    stat.good += i64::from(good);
    stat.total_ms += ms.unwrap_or_default() as f64;
    stat.total_tokens = stat.total_tokens.zip(cost.tokens).map(|(a, b)| a + b);
    stat.usd = stat.usd.zip(cost.usd).map(|(a, b)| a + b);
}
impl Store {
    pub(crate) fn routing_auto_choice(
        &self,
        cfg: &crate::Config,
        provider: &str,
        complex: bool,
    ) -> Result<Option<crate::routing_policy::Choice>> {
        let pcfg = cfg.provider_cfg(Some(provider))?;
        let now = chrono::Utc::now();
        let stats = self.routing_call_stats(cfg, provider, now)?;
        let mut choice = crate::routing_policy::select_auto(&pcfg, &stats, complex)?;
        if let Some(choice) = &mut choice {
            choice.explanation["provider"] = serde_json::json!(provider);
            choice.explanation["window_days"] = serde_json::json!(pcfg.routing_window_days);
            choice.explanation["as_of"] = serde_json::json!(now.to_rfc3339());
        }
        Ok(choice)
    }
    pub(crate) fn routing_role_choice(
        &self,
        cfg: &crate::Config,
        role: &str,
        provider: &str,
        candidates: &[String],
        explore: bool,
    ) -> Result<Option<crate::routing_policy::Choice>> {
        let pcfg = cfg.provider_cfg(Some(provider))?;
        let now = chrono::Utc::now();
        let stats = self.routing_task_stats(cfg, role, provider, now)?;
        let mut choice = crate::routing_policy::select_task(
            candidates,
            &stats,
            cfg.tools.task_min_samples,
            cfg.tools.task_quality_bar,
            explore,
        )?;
        if let Some(choice) = &mut choice {
            choice.explanation["provider"] = serde_json::json!(provider);
            choice.explanation["window_days"] = serde_json::json!(pcfg.routing_window_days);
            choice.explanation["as_of"] = serde_json::json!(now.to_rfc3339());
        }
        Ok(choice)
    }
    #[cfg(test)]
    pub(crate) fn routing_task_final_persisted(&self, task_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM routing_tasks WHERE id=?1 AND final=1)",
            params![task_id],
            |row| row.get(0),
        )?)
    }

    /// Called with a detached serialized batch. Callbacks never hold this Store
    /// or execute SQL; an error leaves the caller's exact receipts retryable.
    pub(crate) fn record_routing(&self, receipts: &[Receipt]) -> Result<()> {
        self.transaction(|st| {
            for receipt in receipts {
                match receipt {
                    Receipt::Call(record)=> {
                        let identity=eligible(record).map(|(id,_)|id);
                        st.ex("INSERT OR IGNORE INTO routing_calls(id,finished_at_ms,provider,endpoint,model,payload) VALUES(?1,?2,?3,?4,?5,?6)",params![record.id,record.finished_at_ms,identity.map(|i|&i.provider),identity.map(|i|&i.endpoint),identity.map(|i|&i.model),serde_json::to_string(record)?])?;
                        for task_id in &record.task_ids {
                            st.ex("INSERT OR IGNORE INTO routing_call_tasks(call_id,task_id) VALUES(?1,?2)",params![record.id,task_id])?;
                        }
                    }
                    Receipt::Task(event)=>{
                        let (record,finalized)=match event{Event::Closed(r)=>(r,false),Event::Final(r)=>(r,true)};
                        let identity=record.responsible.as_ref();
                        st.ex("INSERT INTO routing_tasks(id,finished_at_ms,role,provider,endpoint,model,final,payload) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET finished_at_ms=excluded.finished_at_ms,role=excluded.role,provider=excluded.provider,endpoint=excluded.endpoint,model=excluded.model,final=excluded.final,payload=excluded.payload WHERE routing_tasks.final=0 AND excluded.final=1",params![record.id,record.finished_at_ms,record.role,identity.map(|i|&i.provider),identity.map(|i|&i.endpoint),identity.map(|i|&i.model),finalized,serde_json::to_string(record)?])?;
                    }
                }
            }
            Ok(())
        })
    }
    pub(crate) fn routing_call_stats(
        &self,
        cfg: &crate::Config,
        provider: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Stat>> {
        let pcfg = cfg.provider_cfg(Some(provider))?;
        let since =
            crate::routing_policy::window_start(now, pcfg.routing_window_days)?.timestamp_millis();
        let endpoint = crate::tokens::provider_key(provider, &pcfg);
        let mut query=self.ps("SELECT payload FROM routing_calls WHERE provider=?1 AND endpoint=?2 AND finished_at_ms>=?3 AND finished_at_ms<=?4 ORDER BY id")?;
        let mut rows = query.query(params![provider, endpoint, since, now.timestamp_millis()])?;
        let mut stats = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let record: Record = serde_json::from_str(&row.get::<_, String>(0)?)?;
            let Some((identity, good)) = eligible(&record) else {
                continue;
            };
            // Unstopped logical clocks are not zero-latency successes.
            if record.elapsed_ms.is_none() {
                continue;
            }
            let mut cost = Cost::zero();
            cost.add_call(&record, cfg);
            accumulate(&mut stats, identity, good, record.elapsed_ms, cost);
        }
        Ok(stats.into_values().collect())
    }
    pub(crate) fn routing_task_stats(
        &self,
        cfg: &crate::Config,
        role: &str,
        provider: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Stat>> {
        let pcfg = cfg.provider_cfg(Some(provider))?;
        let since =
            crate::routing_policy::window_start(now, pcfg.routing_window_days)?.timestamp_millis();
        let endpoint = crate::tokens::provider_key(provider, &pcfg);
        let mut query=self.ps("SELECT r.payload,t.outcome,run.rating,run.followup FROM routing_tasks r JOIN task_stats t ON t.id=r.id LEFT JOIN runs run ON run.id=t.run_id WHERE r.role=?1 AND r.provider=?2 AND r.endpoint=?3 AND r.final=1 AND r.finished_at_ms>=?4 AND r.finished_at_ms<=?5 ORDER BY r.id")?;
        let mut rows = query.query(params![
            role,
            provider,
            endpoint,
            since,
            now.timestamp_millis()
        ])?;
        let mut stats = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let record: TaskRecord = serde_json::from_str(&row.get::<_, String>(0)?)?;
            if record.mixed || !record.capture_complete || record.elapsed_ms.is_none() {
                continue;
            }
            let Some(identity) = &record.responsible else {
                continue;
            };
            let mut good = match record.outcome {
                TaskOutcome::Completed => true,
                TaskOutcome::ContractFailure => false,
                TaskOutcome::Neutral(_) => continue,
            };
            let feedback: Option<String> = row.get(1)?;
            let rating: Option<String> = row.get(2)?;
            let followup: Option<String> = row.get(3)?;
            if rating.as_deref() == Some("bad")
                || matches!(followup.as_deref(), Some("edited" | "corrected"))
                || matches!(
                    feedback.as_deref(),
                    Some("respawned" | "rated_bad" | "edited" | "corrected")
                )
            {
                good = false;
            }
            let mut calls=self.ps("SELECT r.payload FROM routing_call_tasks t JOIN routing_calls r ON r.id=t.call_id WHERE t.task_id=?1 ORDER BY r.id")?;
            let mut call_rows = calls.query(params![record.id])?;
            let mut cost = Cost::zero();
            let mut count = 0u64;
            while let Some(call) = call_rows.next()? {
                let call: Record = serde_json::from_str(&call.get::<_, String>(0)?)?;
                count = count.saturating_add(1);
                cost.add_call(&call, cfg);
            }
            // Missing/dropped receipts cannot be turned into a cheap task.
            if count == 0 || count != record.expected_calls {
                continue;
            }
            accumulate(&mut stats, identity, good, record.elapsed_ms, cost);
        }
        Ok(stats.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::Usage,
        routing_observation::{AttemptRecord, Neutral},
    };
    fn identity(cfg: &crate::Config, model: &str) -> Identity {
        Identity::new("default", &cfg.provider, model)
    }
    fn record(cfg: &crate::Config, model: &str, cost: Option<f64>, time: i64) -> Record {
        let id = identity(cfg, model);
        Record {
            id: uuid::Uuid::new_v4().to_string(),
            outcome: Outcome::Success(id.clone()),
            attempts: vec![AttemptRecord {
                id: uuid::Uuid::new_v4().to_string(),
                identity: id,
                sent: true,
                status: Some(200),
                neutral: None,
                usage: Some(Usage {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    reported: 3,
                    cost,
                    ..Default::default()
                }),
                response_complete: true,
            }],
            task_ids: vec![],
            resource_only: false,
            truncated: false,
            started_at_ms: time - 50,
            finished_at_ms: time,
            elapsed_ms: Some(50),
        }
    }
    fn task_record(record: &Record, id: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            role: Some("worker".into()),
            parent_ids: vec![],
            outcome: TaskOutcome::Completed,
            responsible: eligible(record).map(|(id, _)| id.clone()),
            mixed: false,
            expected_calls: 1,
            capture_complete: true,
            started_at_ms: record.started_at_ms,
            finished_at_ms: record.finished_at_ms,
            elapsed_ms: Some(100),
        }
    }
    fn legacy(st: &Store, id: &str) {
        st.record_task_with_id(
            id,
            "worker",
            "fixture-actual",
            "",
            "",
            "",
            100,
            5,
            1,
            "completed",
        )
        .unwrap();
    }
    #[test]
    fn window_and_endpoint_scope_do_not_backfill_legacy_rows_or_count_neutral_spend() {
        let st = Store::open_in_memory().unwrap();
        let cfg = crate::Config::default();
        let now = chrono::Utc::now();
        st.record_model_outcome("legacy", true, 1).unwrap();
        let good = record(&cfg, "served", Some(0.2), now.timestamp_millis());
        let mut old = good.clone();
        old.id = "old".into();
        old.finished_at_ms = (now - chrono::Duration::days(31)).timestamp_millis();
        let mut other = good.clone();
        other.id = "other-endpoint".into();
        if let Outcome::Success(id) = &mut other.outcome {
            id.endpoint = "different".into();
        }
        let mut neutral = good.clone();
        neutral.id = "neutral".into();
        neutral.outcome = Outcome::Neutral(Neutral::Auth);
        st.record_routing(&[
            Receipt::Call(good.clone()),
            Receipt::Call(old),
            Receipt::Call(other),
            Receipt::Call(neutral),
        ])
        .unwrap();
        let stats = st.routing_call_stats(&cfg, "default", now).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].model, "served");
        assert_eq!(stats[0].samples, 1);
        assert_eq!(stats[0].usd, Some(0.2));
        let mut changed = cfg.clone();
        changed.provider.base_url = "https://other.invalid".into();
        assert!(st
            .routing_call_stats(&changed, "default", now)
            .unwrap()
            .is_empty());
        let mut invalid = cfg;
        invalid.provider.routing_window_days = 0;
        assert!(st.routing_call_stats(&invalid, "default", now).is_err());
    }
    #[test]
    fn eligible_fallback_uses_all_attempt_spend_unknown_is_not_free_and_unsent_is_not_billed() {
        let st = Store::open_in_memory().unwrap();
        let cfg = crate::Config::default();
        let now = chrono::Utc::now();
        let mut call = record(&cfg, "served", Some(0.006), now.timestamp_millis());
        let mut failed = call.attempts[0].clone();
        failed.id = "failed".into();
        failed.identity.model = "requested".into();
        failed.status = Some(404);
        failed.neutral = Some(Neutral::Unavailable);
        failed.usage.as_mut().unwrap().cost = Some(0.004);
        call.attempts.insert(0, failed);
        let mut unsent = call.attempts[0].clone();
        unsent.id = "unsent".into();
        unsent.sent = false;
        unsent.usage = None;
        call.attempts.push(unsent);
        st.record_routing(&[Receipt::Call(call.clone()), Receipt::Call(call.clone())])
            .unwrap();
        let stats = st.routing_call_stats(&cfg, "default", now).unwrap();
        assert_eq!(stats[0].samples, 1);
        assert!((stats[0].usd.unwrap() - 0.01).abs() < 1e-12);
        assert_eq!(stats[0].total_tokens, Some(10.0));
        let mut partial = call;
        partial.id = "partial".into();
        partial.attempts[0].response_complete = false;
        st.record_routing(&[Receipt::Call(partial)]).unwrap();
        let stats = st.routing_call_stats(&cfg, "default", now).unwrap();
        assert_eq!(stats[0].samples, 2);
        assert_eq!(stats[0].usd, None);
        assert_eq!(stats[0].total_tokens, None);
    }
    #[test]
    fn exact_task_links_require_finalization_and_all_receipts_then_follow_exact_feedback_id() {
        let st = Store::open_in_memory().unwrap();
        let cfg = crate::Config::default();
        let now = chrono::Utc::now();
        let mut call = record(&cfg, "actual", Some(0.1), now.timestamp_millis());
        call.task_ids = vec!["task-exact".into()];
        let task = task_record(&call, "task-exact");
        legacy(&st, "task-exact");
        st.record_routing(&[
            Receipt::Task(Event::Closed(task.clone())),
            Receipt::Call(call.clone()),
        ])
        .unwrap();
        assert!(st
            .routing_task_stats(&cfg, "worker", "default", now)
            .unwrap()
            .is_empty());
        st.record_routing(&[Receipt::Task(Event::Final(task.clone()))])
            .unwrap();
        assert_eq!(
            st.routing_task_stats(&cfg, "worker", "default", now)
                .unwrap()[0]
                .good,
            1
        );
        // An unrelated legacy task at the same timestamp/model cannot rate it.
        st.ex("INSERT INTO task_stats(id,role,model,task_hash,ms,tokens,turns,outcome,created_at) VALUES(?1,'worker','actual','',1,1,1,'rated_bad',?2)",params!["unrelated",now.to_rfc3339()]).unwrap();
        assert_eq!(
            st.routing_task_stats(&cfg, "worker", "default", now)
                .unwrap()[0]
                .good,
            1
        );
        st.ex(
            "UPDATE task_stats SET outcome='rated_bad' WHERE id='task-exact'",
            [],
        )
        .unwrap();
        assert_eq!(
            st.routing_task_stats(&cfg, "worker", "default", now)
                .unwrap()[0]
                .good,
            0
        );
        // A duplicate Final cannot rewrite immutable serving/outcome evidence.
        let mut altered = task.clone();
        altered.responsible.as_mut().unwrap().model = "forged".into();
        st.record_routing(&[Receipt::Task(Event::Final(altered))])
            .unwrap();
        assert_eq!(
            st.routing_task_stats(&cfg, "worker", "default", now)
                .unwrap()[0]
                .model,
            "actual"
        );
        // A delayed retry of Closed never overwrites a finalized receipt.
        st.record_routing(&[Receipt::Task(Event::Closed(task))])
            .unwrap();
        assert_eq!(
            st.routing_task_stats(&cfg, "worker", "default", now)
                .unwrap()[0]
                .samples,
            1
        );
        let mut missing = call.clone();
        missing.id = "another-call".into();
        missing.task_ids = vec!["late-task".into()];
        let late = task_record(&missing, "late-task");
        legacy(&st, "late-task");
        st.record_routing(&[Receipt::Task(Event::Final(late))])
            .unwrap();
        assert_eq!(
            st.routing_task_stats(&cfg, "worker", "default", now)
                .unwrap()[0]
                .samples,
            1
        );
        st.record_routing(&[Receipt::Call(missing)]).unwrap();
        assert_eq!(
            st.routing_task_stats(&cfg, "worker", "default", now)
                .unwrap()[0]
                .samples,
            2
        );
    }
    #[test]
    fn task_cost_includes_incidental_models_without_relabeling_responsible_identity() {
        let st = Store::open_in_memory().unwrap();
        let cfg = crate::Config::default();
        let now = chrono::Utc::now();
        let mut responsible = record(&cfg, "agent", Some(0.1), now.timestamp_millis());
        responsible.task_ids = vec!["parent".into()];
        let mut resource = record(&cfg, "tool-model", Some(0.3), now.timestamp_millis());
        resource.task_ids = vec!["child".into(), "parent".into()];
        resource.resource_only = true;
        resource.outcome = Outcome::Neutral(Neutral::Unknown);
        let mut parent = task_record(&responsible, "parent");
        legacy(&st, "parent");
        parent.expected_calls = 2;
        st.record_routing(&[
            Receipt::Call(responsible),
            Receipt::Call(resource),
            Receipt::Task(Event::Final(parent)),
        ])
        .unwrap();
        let stats = st
            .routing_task_stats(&cfg, "worker", "default", now)
            .unwrap();
        assert_eq!(stats[0].model, "agent");
        assert!((stats[0].usd.unwrap() - 0.4).abs() < 1e-12);
    }
    #[test]
    fn transaction_failure_keeps_no_partial_receipts_and_raw_usage_roundtrips() {
        let st = Store::open_in_memory().unwrap();
        let cfg = crate::Config::default();
        let time = chrono::Utc::now().timestamp_millis();
        let mut call = record(&cfg, "actual", Some(0.0), time);
        let usage = call.attempts[0].usage.as_mut().unwrap();
        usage.cost_unknown = true;
        usage.reported = 1;
        usage.cache_write_tokens = 9;
        usage.reasoning_tokens = 5;
        st.conn.execute_batch("CREATE TRIGGER refuse_routing BEFORE INSERT ON routing_call_tasks BEGIN SELECT RAISE(ABORT,'private failure'); END;").unwrap();
        call.task_ids = vec!["private-task".into()];
        assert!(st.record_routing(&[Receipt::Call(call.clone())]).is_err());
        assert_eq!(
            st.conn
                .query_row("SELECT COUNT(*) FROM routing_calls", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        st.conn
            .execute_batch("DROP TRIGGER refuse_routing")
            .unwrap();
        st.record_routing(&[Receipt::Call(call)]).unwrap();
        let raw: String = st
            .conn
            .query_row("SELECT payload FROM routing_calls", [], |r| r.get(0))
            .unwrap();
        let receipt: Record = serde_json::from_str(&raw).unwrap();
        let u = receipt.attempts[0].usage.unwrap();
        assert!(u.cost_unknown);
        assert_eq!(u.reported, 1);
        assert_eq!(u.cache_write_tokens, 9);
        assert_eq!(u.reasoning_tokens, 5);
        assert_eq!(u.cost, Some(0.0));
    }
    #[test]
    fn both_live_choice_helpers_use_correlated_usd_and_the_same_recent_window() {
        let st = Store::open_in_memory().unwrap();
        let mut cfg = crate::Config::default();
        cfg.provider.routing_models = vec!["premium".into(), "cheap".into()];
        cfg.provider.routing_min_samples = 1;
        cfg.tools.task_min_samples = 1;
        let now = chrono::Utc::now();
        for (model, cost, time) in [
            ("premium", 0.1, now.timestamp_millis()),
            (
                "cheap",
                0.01,
                (now - chrono::Duration::days(2)).timestamp_millis(),
            ),
        ] {
            let mut call = record(&cfg, model, Some(cost), time);
            let task_id = format!("task-{model}");
            call.task_ids = vec![task_id.clone()];
            let task = task_record(&call, &task_id);
            legacy(&st, &task_id);
            st.record_routing(&[Receipt::Call(call), Receipt::Task(Event::Final(task))])
                .unwrap();
        }
        let main = st
            .routing_auto_choice(&cfg, "default", false)
            .unwrap()
            .unwrap();
        let role = st
            .routing_role_choice(
                &cfg,
                "worker",
                "default",
                &cfg.provider.routing_models,
                false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(main.model, "cheap");
        assert_eq!(role.model, "cheap");
        assert_eq!(main.reason, crate::routing_policy::Reason::KnownUsd);
        assert_eq!(role.reason, crate::routing_policy::Reason::KnownUsd);
        assert_eq!(main.explanation["window_days"], 30);
        assert_eq!(role.explanation["window_days"], 30);
        cfg.provider.routing_window_days = 1;
        let exploring = st
            .routing_auto_choice(&cfg, "default", false)
            .unwrap()
            .unwrap();
        assert_eq!(exploring.model, "cheap");
        assert_eq!(exploring.reason, crate::routing_policy::Reason::Explore);
        let expired = exploring.explanation["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["model"] == "cheap")
            .unwrap();
        assert_eq!(
            expired["samples"], 0,
            "expired cost must not qualify or survive the window"
        );
        assert_eq!(
            st.routing_role_choice(
                &cfg,
                "worker",
                "default",
                &cfg.provider.routing_models,
                false
            )
            .unwrap()
            .unwrap()
            .model,
            "premium"
        );
    }
}

#[cfg(test)]
mod feedback_race_tests {
    use super::*;
    #[test]
    fn bad_run_rating_before_exact_task_metadata_is_not_lost_or_borrowed() {
        for feedback in ["bad", "corrected"] {
            let st = Store::open_in_memory().unwrap();
            let cfg = crate::Config::default();
            let sid = st.create_session(None).unwrap().id;
            let run = st.create_run(&sid).unwrap();
            st.set_run_status(&run, "completed").unwrap();
            if feedback == "bad" {
                st.rate_run(&sid, Some(&run), "bad", None).unwrap();
            } else {
                st.mark_last_run_followup(&sid, "corrected").unwrap();
            }
            let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = records.clone();
            let call = crate::routing_observation::Call::new(std::sync::Arc::new(move |r| {
                sink.lock().unwrap().push(r)
            }));
            let actual = Identity::new("default", &cfg.provider, "actual");
            let attempt = call.attempt(actual.clone());
            attempt.sent();
            attempt.status(200);
            attempt.returned();
            attempt.settle(
                Some(crate::provider::Usage {
                    cost: Some(0.1),
                    reported: 3,
                    ..Default::default()
                }),
                true,
            );
            call.finish(Outcome::Success(actual.clone()));
            drop(attempt);
            drop(call);
            let mut call = records.lock().unwrap().pop().unwrap();
            call.task_ids = vec!["task-exact".into()];
            let task = TaskRecord {
                id: "task-exact".into(),
                role: Some("worker".into()),
                parent_ids: vec![],
                outcome: TaskOutcome::Completed,
                responsible: Some(actual),
                mixed: false,
                expected_calls: 1,
                capture_complete: true,
                started_at_ms: call.started_at_ms,
                finished_at_ms: call.finished_at_ms,
                elapsed_ms: Some(1),
            };
            st.record_routing(&[Receipt::Call(call), Receipt::Task(Event::Final(task))])
                .unwrap();
            assert!(
                st.routing_task_stats(&cfg, "worker", "default", chrono::Utc::now())
                    .unwrap()
                    .is_empty(),
                "missing exact metadata stays unscored"
            );
            st.record_task_with_id(
                "task-exact",
                "worker",
                "actual",
                &sid,
                &run,
                "",
                1,
                0,
                1,
                "completed",
            )
            .unwrap();
            let stats = st
                .routing_task_stats(&cfg, "worker", "default", chrono::Utc::now())
                .unwrap();
            assert_eq!(stats[0].samples, 1);
            assert_eq!(
                stats[0].good, 0,
                "durable negative feedback predates row insertion"
            );
        }
    }
}
