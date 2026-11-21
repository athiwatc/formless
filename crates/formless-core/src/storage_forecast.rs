//! The estimate path reads a basis from the bounded forecast history. Only
//! selected comparable Runs need full forecast and usage records.
use super::*;
use formless_protocol::{ForecastBasis, ForecastComparable, RunForecast};

const SELECT_COMPARABLES: &str = "
    WITH recent_forecasts AS MATERIALIZED (
        SELECT rowid, run_id, json_extract(forecast, '$.basis') AS basis
        FROM run_forecasts ORDER BY rowid DESC LIMIT 2000
    ), recent_runs AS MATERIALIZED (
        SELECT id, status, steps, created_at, updated_at
        FROM runs ORDER BY created_at DESC, rowid DESC LIMIT 2000
    ), comparable_runs AS MATERIALIZED (
        SELECT f.rowid, f.run_id, r.steps, r.created_at, r.updated_at
        FROM recent_forecasts f JOIN recent_runs r ON r.id=f.run_id
        WHERE r.status='completed'
          AND json_extract(f.basis, '$.provider_key')=?1
          AND json_extract(f.basis, '$.model')=?2
          AND json_extract(f.basis, '$.role')=?3
          AND json_extract(f.basis, '$.reasoning_effort') IS ?4
          AND json_extract(f.basis, '$.tools_hash')=?5
          AND (json_extract(f.basis, '$.session_id')=?6 OR json_extract(f.basis, '$.cwd_hash')=?7)
          AND CAST(?8 AS REAL) / MAX(1, json_extract(f.basis, '$.prompt_tokens')) BETWEEN 0.5 AND 2.0
          AND EXISTS (
              SELECT 1
              FROM (
                  SELECT COUNT(*) AS rows, SUM(usage_complete) AS covered,
                      MAX(CASE WHEN source='run' THEN prompt_tokens ELSE 0 END) AS peak,
                      SUM(source='run' AND (provider<>?9 OR model<>?2)) AS wrong_model
                  FROM run_usage WHERE run_id=f.run_id
              ) u
              CROSS JOIN (
                  SELECT SUM(expected) AS expected,
                      SUM(CASE WHEN source='run' THEN expected ELSE 0 END) AS main
                  FROM forecast_request_counts WHERE run_id=f.run_id
              ) c
              WHERE u.rows=c.expected AND u.covered=c.expected
                AND u.peak>0 AND u.wrong_model=0 AND c.main>=r.steps
          )
        ORDER BY f.rowid DESC LIMIT 50
    )
    SELECT c.run_id, c.steps, c.created_at, c.updated_at, f.forecast
    FROM comparable_runs c JOIN run_forecasts f ON f.rowid=c.rowid
    ORDER BY c.rowid DESC
";

const USAGE_COLUMNS: &str = "
    u.provider,u.model,u.prompt_tokens,u.completion_tokens,u.cached_tokens,
    u.cache_write_tokens,u.cost,u.source,u.cost_unknown
";

struct ComparableUsage {
    cost: Option<f64>,
    peak: u64,
    cached: u64,
    prompt_tokens: i128,
    completion_tokens: i128,
    any: bool,
}
impl Default for ComparableUsage {
    fn default() -> Self {
        Self {
            cost: Some(0.0),
            peak: 0,
            cached: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            any: false,
        }
    }
}
impl ComparableUsage {
    fn add(&mut self, row: &rusqlite::Row<'_>, cfg: &crate::Config) -> Result<()> {
        let provider: String = row.get(0)?;
        let model: String = row.get(1)?;
        let input: i64 = row.get(2)?;
        let output: i64 = row.get(3)?;
        let cached: i64 = row.get(4)?;
        let write = row.get(5)?;
        let reported = row.get(6)?;
        let source: String = row.get(7)?;
        let unknown = row.get(8)?;
        self.any = true;
        self.prompt_tokens += i128::from(input);
        self.completion_tokens += i128::from(output);
        self.cached += cached.max(0) as u64;
        // Nested contexts have their own windows and token peaks.
        if source == "run" {
            self.peak = self.peak.max(input.max(0) as u64);
        }
        self.cost = self
            .cost
            .zip(cfg.cost_of_known(
                &provider, &model, input, output, cached, write, reported, unknown,
            ))
            .map(|(a, b)| a + b);
        Ok(())
    }
}

impl Store {
    pub(super) fn has_forecast_history(&self) -> Result<bool> {
        Ok(self.ps("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='run_forecasts')")?
            .query_row([], |row| row.get(0))?)
    }

    pub fn forecast_usage(
        &self,
        run_id: &str,
        cfg: &crate::Config,
    ) -> Result<(Option<f64>, u64, u64)> {
        let sql = format!("SELECT {USAGE_COLUMNS} FROM run_usage u WHERE u.run_id=?1");
        let mut statement = self.ps(&sql)?;
        let mut rows = statement.query([run_id])?;
        let mut usage = ComparableUsage::default();
        while let Some(row) = rows.next()? {
            usage.add(row, cfg)?;
        }
        Ok((usage.cost.filter(|_| usage.any), usage.peak, usage.cached))
    }

    pub(crate) fn forecast_comparables(
        &self,
        basis: &ForecastBasis,
        provider: &str,
        share_cwd: bool,
        cfg: &crate::Config,
    ) -> Result<Vec<ForecastComparable>> {
        self.read_transaction(|store| {
            if !store.has_forecast_history()? {
                return Ok(Vec::new());
            }
            let mut statement = store.ps(SELECT_COMPARABLES)?;
            let mut rows = statement.query(params![
                basis.provider_key,
                basis.model,
                basis.role,
                basis.reasoning_effort,
                basis.tools_hash,
                basis.session_id,
                share_cwd.then_some(&basis.cwd_hash),
                basis.prompt_tokens as f64,
                provider,
            ])?;
            let mut selected = Vec::new();
            while let Some(row) = rows.next()? {
                let snapshot: RunForecast = serde_json::from_str(row.get_ref(4)?.as_str()?)?;
                let created: String = row.get(2)?;
                let updated: String = row.get(3)?;
                selected.push(ForecastComparable {
                    run_id: row.get(0)?,
                    prompt_tokens: snapshot.basis.prompt_tokens,
                    steps: row.get(1)?,
                    tokens: 0,
                    duration_ms: super::run_duration_ms(&created, &updated),
                    cost_usd: None,
                    cached_fraction: None,
                    peak_prompt_tokens: 0,
                });
            }
            if selected.is_empty() {
                return Ok(selected);
            }
            let ids: Vec<_> = selected.iter().map(|item| &item.run_id).collect();
            let ids = serde_json::to_string(&ids)?;
            let sql = format!(
                "SELECT {USAGE_COLUMNS}, w.key FROM json_each(?1) AS w
                 CROSS JOIN run_usage u WHERE u.run_id=w.value ORDER BY w.key, u.rowid"
            );
            let mut statement = store.ps(&sql)?;
            let mut rows = statement.query([ids])?;
            let mut usage: Vec<_> = (0..selected.len())
                .map(|_| ComparableUsage::default())
                .collect();
            while let Some(row) = rows.next()? {
                let index: usize = row.get(9)?;
                usage[index].add(row, cfg)?;
            }
            for (item, usage) in selected.iter_mut().zip(usage) {
                let prompt = i64::try_from(usage.prompt_tokens)?;
                let completion = i64::try_from(usage.completion_tokens)?;
                item.tokens = prompt
                    .checked_add(completion)
                    .context("Comparable token total overflow")?;
                item.cost_usd = usage.cost.filter(|_| usage.any);
                item.cached_fraction =
                    (prompt > 0).then_some(usage.cached as f64 / prompt.max(1) as f64);
                item.peak_prompt_tokens = usage.peak;
            }
            Ok(selected)
        })
    }
}

#[cfg(test)]
impl Store {
    pub(crate) fn count_forecast_statements<T>(&self, action: impl FnOnce() -> T) -> (T, usize) {
        use rusqlite::ffi::{sqlite3, sqlite3_trace_v2, SQLITE_TRACE_STMT};
        use std::{cell::Cell, ffi::c_void};

        unsafe extern "C" fn count(
            _: u32,
            context: *mut c_void,
            _: *mut c_void,
            _: *mut c_void,
        ) -> i32 {
            // SAFETY: the trace is synchronous and its guard clears the
            // callback before the stack-local Cell can be dropped.
            let calls = unsafe { &*context.cast::<Cell<usize>>() };
            calls.set(calls.get() + 1);
            0
        }

        struct Trace(*mut sqlite3);
        impl Drop for Trace {
            fn drop(&mut self) {
                // SAFETY: the Store still owns this SQLite connection.
                unsafe { sqlite3_trace_v2(self.0, 0, None, std::ptr::null_mut()) };
            }
        }

        let calls = Cell::new(0);
        // SAFETY: the Store owns the connection throughout the trace, and
        // the context pointer refers to the local Cell for this whole scope.
        let db = unsafe { self.conn.handle() };
        assert_eq!(
            unsafe {
                sqlite3_trace_v2(
                    db,
                    SQLITE_TRACE_STMT as u32,
                    Some(count),
                    std::ptr::from_ref(&calls).cast_mut().cast(),
                )
            },
            0
        );
        let _trace = Trace(db);
        (action(), calls.get())
    }
}
