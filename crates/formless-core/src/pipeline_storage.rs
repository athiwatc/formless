//! Small durable non-text policy markers; Session prompts are never modified.
use super::*;
use crate::pipeline_policy::PolicyMarker;
impl Store {
    pub(crate) fn record_pipeline_policy(&self, run: &str, marker: &PolicyMarker) -> Result<()> {
        if let Some(previous) = self.pipeline_policy_marker(run)? {
            anyhow::ensure!(previous == *marker, "Run pipeline marker cannot change");
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO audit(id,actor,action,detail,created_at) VALUES(?1,'runtime','run.pipeline',?2,?3)",
            params![format!("pipeline:{run}"), serde_json::to_string(marker)?, now()],
        )?;
        Ok(())
    }
    /// Observed usage and its known price floor, never an invented zero total
    /// when admitted attempts lack a complete provider usage record.
    pub(crate) fn pipeline_spend(&self, run: &str, cfg: &crate::Config) -> Result<Value> {
        let attempts: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(expected),0) FROM forecast_request_counts WHERE run_id=?1",
            params![run],
            |r| r.get(0),
        )?;
        let (records, reported, cost_basis): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),COALESCE(SUM(usage_complete),0),COALESCE(SUM(CASE WHEN cost IS NOT NULL OR usage_complete=1 THEN 1 ELSE 0 END),0) FROM run_usage WHERE run_id=?1",
            params![run], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        )?;
        let (rows, _) = self.usage_rows(None, Some(run))?;
        let mut unpriced = Vec::new();
        let mut uses_local_prices = false;
        let costs: Vec<_> = rows
            .iter()
            .map(|(provider, model, p, c, cache, write, usd, unknown)| {
                let cost =
                    cfg.cost_of_known(provider, model, *p, *c, *cache, *write, *usd, *unknown);
                if cost.is_none() {
                    unpriced.push(format!("{provider}/{model}"));
                }
                if usd.is_none() && cost.is_some() {
                    uses_local_prices = true;
                }
                cost
            })
            .collect();
        let spend: crate::budget::Spend = costs.into_iter().collect();
        let complete = (attempts == 0 && records == 0)
            || (attempts > 0 && attempts == records && records == reported && spend.complete);
        let spend_complete = (attempts == 0 && records == 0)
            || (attempts > 0 && attempts == records && records == cost_basis && spend.complete);
        Ok(
            json!({"known_usd":spend.known,"total_usd":spend_complete.then_some(spend.known),"usage_complete":complete,"spend_complete":spend_complete,"admitted_attempts":attempts,"usage_records":records,"uses_local_prices":uses_local_prices,"unpriced":unpriced}),
        )
    }
    pub(crate) fn pipeline_policy_marker(&self, run: &str) -> Result<Option<PolicyMarker>> {
        let row: Option<String> = self.conn.query_row(
            "SELECT CASE WHEN length(CAST(detail AS BLOB))<=4096 THEN detail ELSE '' END FROM audit WHERE id=?1 AND actor='runtime' AND action='run.pipeline'",
            params![format!("pipeline:{run}")], |row| row.get(0),
        ).optional()?;
        row.map(|text| {
            serde_json::from_str(&text)
                .context("Run-only pipeline metadata is corrupt; submit a new pipeline Run")
        })
        .transpose()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spend_never_invents_zero_for_missing_usage_and_preserves_actual_reported_dollars() {
        let st = Store::open_in_memory().unwrap();
        let session = st.create_session(None).unwrap();
        let run = st.create_run(&session.id).unwrap();
        let cfg = crate::Config::default();
        let empty = st.pipeline_spend(&run, &cfg).unwrap();
        assert_eq!(empty["total_usd"].as_f64(), Some(0.0));
        st.record_forecast_request(&run, "run").unwrap();
        let missing = st.pipeline_spend(&run, &cfg).unwrap();
        assert_eq!(missing["known_usd"].as_f64(), Some(0.0));
        assert!(missing["total_usd"].is_null());
        assert_eq!(missing["spend_complete"], false);
        st.record_usage_full(
            &session.id,
            &run,
            "default",
            "unpriced",
            &crate::provider::Usage {
                cost: Some(0.2),
                reported: 0,
                ..Default::default()
            },
            "run",
        )
        .unwrap();
        let reported = st.pipeline_spend(&run, &cfg).unwrap();
        assert_eq!(reported["total_usd"].as_f64(), Some(0.2));
        assert_eq!(reported["spend_complete"], true);
        assert_eq!(
            reported["usage_complete"], false,
            "USD cost can be known while token usage is not"
        );
        st.record_forecast_request(&run, "schema-repair").unwrap();
        let partial = st.pipeline_spend(&run, &cfg).unwrap();
        assert_eq!(partial["known_usd"].as_f64(), Some(0.2));
        assert!(partial["total_usd"].is_null());
    }
    #[test]
    fn marker_survives_owners_and_corrupt_marker_never_becomes_ordinary_defaults() {
        let st = Store::open_in_memory().unwrap();
        let session = st.create_session(None).unwrap();
        let run = st.create_run(&session.id).unwrap();
        let marker = PolicyMarker {
            digest: "digest".into(),
            max_usd: Some(0.0),
            system_hash: Some("system-digest".into()),
            schema_hash: None,
        };
        st.record_pipeline_policy(&run, &marker).unwrap();
        st.set_run_status(&run, "paused").unwrap();
        assert_eq!(st.pipeline_policy_marker(&run).unwrap(), Some(marker));
        assert!(st.pipeline_policy_marker("ordinary").unwrap().is_none());
        st.conn
            .execute(
                "UPDATE audit SET detail='corrupt' WHERE id=?1",
                params![format!("pipeline:{run}")],
            )
            .unwrap();
        assert!(st.pipeline_policy_marker(&run).is_err());
    }
}
