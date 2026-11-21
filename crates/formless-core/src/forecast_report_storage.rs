//! Read-only facts for forecast accuracy. Keep the legacy report's windows
//! and price every usage row in Rust, in the existing Run index's row order.
use super::{run_duration_ms, Store};
use crate::Config;
use anyhow::{ensure, Result};
use rusqlite::{params, Row};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct ForecastMetricInput {
    pub(crate) status: String,
    pub(crate) steps: i64,
    pub(crate) duration_ms: i64,
    pub(crate) prompt_tokens: i64,
    pub(crate) completion_tokens: i64,
    pub(crate) cost: Option<f64>,
    pub(crate) main_peak: u64,
    pub(crate) served: Vec<(String, String)>,
    pub(crate) complete: bool,
}

pub(super) struct UsageRow {
    provider: String,
    model: String,
    input: i64,
    output: i64,
    cache: i64,
    write: i64,
    reported: Option<f64>,
    source: String,
    unknown: bool,
}

impl UsageRow {
    pub(super) fn read(row: &Row<'_>, i: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            provider: row.get(i)?,
            model: row.get(i + 1)?,
            input: row.get(i + 2)?,
            output: row.get(i + 3)?,
            cache: row.get(i + 4)?,
            write: row.get(i + 5)?,
            reported: row.get(i + 6)?,
            source: row.get(i + 7)?,
            unknown: row.get(i + 8)?,
        })
    }
}

#[derive(Default)]
pub(super) struct UsageTotals {
    cost: Option<f64>,
    any: bool,
    pub(super) peak: u64,
    pub(super) cached: u64,
}

impl UsageTotals {
    pub(super) fn add(&mut self, row: &UsageRow, cfg: &Config) {
        // A nested request never sets the parent Session's context peak.
        if row.source == "run" {
            self.peak = self.peak.max(row.input.max(0) as u64);
        }
        self.cached += row.cache.max(0) as u64;
        let cost = if self.any { self.cost } else { Some(0.0) };
        self.cost = cost
            .zip(cfg.cost_of_known(
                &row.provider,
                &row.model,
                row.input,
                row.output,
                row.cache,
                row.write,
                row.reported,
                row.unknown,
            ))
            .map(|(previous, price)| previous + price);
        self.any = true;
    }

    pub(super) fn cost(&self) -> Option<f64> {
        self.cost
    }
}

pub(super) fn covered_requests(expected: i64, rows: i64, known: i64) -> bool {
    expected > 0 && expected == rows && expected == known
}

// There is no index on global creation time. Keep this narrow top-2,000
// selection, then filter IDs before decoding or fetching their Run facts.
const RUN_FACTS_SQL: &str = "WITH recent AS MATERIALIZED (
        SELECT id FROM runs ORDER BY created_at DESC, rowid DESC LIMIT 2000
    )
    SELECT r.id, r.status, r.steps, r.created_at, r.updated_at,
       (SELECT json_array(COALESCE(SUM(u.prompt_tokens),0), COALESCE(SUM(u.completion_tokens),0),
                          COUNT(*), COALESCE(SUM(u.usage_complete),0))
        FROM run_usage u WHERE u.run_id=r.id),
       (SELECT json_array(COALESCE(SUM(f.expected),0),
                          COALESCE(SUM(CASE WHEN f.source='run' THEN f.expected ELSE 0 END),0))
        FROM forecast_request_counts f WHERE f.run_id=r.id),
       r.session_id, r.followup, r.rating
    FROM recent CROSS JOIN runs r ON r.id=recent.id
    WHERE recent.id IN (SELECT value FROM json_each(?1))";

const USAGE_SQL: &str = "SELECT u.run_id,u.provider,u.model,u.prompt_tokens,u.completion_tokens,
           u.cached_tokens,u.cache_write_tokens,u.cost,u.source,u.cost_unknown
    FROM run_usage u WHERE u.run_id IN (SELECT value FROM json_each(?1))
    ORDER BY u.run_id,u.rowid";

impl Store {
    pub(crate) fn forecast_metric_inputs(
        &self,
        ids: &[&str],
        cfg: &Config,
    ) -> Result<HashMap<String, ForecastMetricInput>> {
        ensure!(
            ids.len() <= 200,
            "forecast report window exceeds 200 records"
        );
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut inputs = HashMap::with_capacity(ids.len());
        let mut stmt = self.ps(RUN_FACTS_SQL)?;
        let mut rows = stmt.query(params![serde_json::to_string(ids)?])?;
        while let Some(row) = rows.next()? {
            let created = row.get_ref(3)?.as_str()?;
            let updated = row.get_ref(4)?.as_str()?;
            // Keep SQLite's SUM and NULL handling on each indexed aggregate,
            // including legacy type errors and malformed flags that coalesce.
            let (prompt_tokens, completion_tokens, recorded, known) =
                serde_json::from_str(row.get_ref(5)?.as_str()?)?;
            let (expected, main): (i64, i64) = serde_json::from_str(row.get_ref(6)?.as_str()?)?;
            let steps = row.get(2)?;
            let input = ForecastMetricInput {
                status: row.get(1)?,
                steps,
                duration_ms: run_duration_ms(created, updated),
                prompt_tokens,
                completion_tokens,
                complete: covered_requests(expected, recorded, known) && main >= steps,
                ..Default::default()
            };
            // Retain legacy type/UTF-8 errors on selected Runs. Unrelated
            // Runs contribute their window position without decoding fields.
            for column in 7..=9 {
                row.get_ref(column)?.as_str()?;
            }
            inputs.insert(row.get::<_, String>(0)?, input);
        }
        if inputs.is_empty() {
            return Ok(HashMap::new());
        }

        // Stream indexed usage rows, retaining only report facts per Run.
        let ids = serde_json::to_string(&inputs.keys().collect::<Vec<_>>())?;
        let mut stmt = self.ps(USAGE_SQL)?;
        let mut rows = stmt.query(params![ids])?;
        let mut current = String::new();
        let mut usage = UsageTotals::default();
        let mut seen = HashSet::new();
        while let Some(row) = rows.next()? {
            let id = row.get_ref(0)?.as_str()?;
            if current != id {
                if let Some(input) = inputs.get_mut(&current) {
                    finish_usage(input, &usage);
                }
                current = id.to_owned();
                usage = UsageTotals::default();
                seen.clear();
            }
            let details = UsageRow::read(row, 1)?;
            usage.add(&details, cfg);
            if details.source == "run"
                && seen.insert((details.provider.clone(), details.model.clone()))
            {
                inputs
                    .get_mut(id)
                    .expect("query only returns requested Run IDs")
                    .served
                    .push((details.provider, details.model));
            }
        }
        if let Some(input) = inputs.get_mut(&current) {
            finish_usage(input, &usage);
        }
        Ok(inputs)
    }
}

fn finish_usage(input: &mut ForecastMetricInput, usage: &UsageTotals) {
    input.cost = usage.cost();
    input.main_peak = usage.peak;
}

#[cfg(test)]
#[path = "forecast_report_storage_tests.rs"]
mod tests;
