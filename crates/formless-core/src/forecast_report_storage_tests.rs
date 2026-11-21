use super::*;
use crate::forecast::{estimate_store, metrics};
use formless_protocol::{ForecastInput, ForecastRange, RunForecast};
use rusqlite::{ffi, Connection};
use serde_json::{json, Value};
use std::cell::Cell;
use std::ffi::{c_int, c_uint, c_void, CStr};

const DATE: &str = "2026-03-01T00:00:00Z";

fn forecast(store: &Store, sid: &str, cfg: &Config) -> RunForecast {
    let mut f = estimate_store(
        store,
        cfg,
        &ForecastInput {
            session_id: Some(sid.into()),
            draft: "Synthetic forecast report".into(),
            ..Default::default()
        },
    )
    .unwrap();
    f.steps = Some(ForecastRange { p50: 1., p90: 4. });
    f.tokens = Some(ForecastRange {
        p50: 101.,
        p90: 300.,
    });
    f.cost_usd = Some(ForecastRange { p50: 0.01, p90: 1. });
    f.duration_ms = Some(ForecastRange {
        p50: 20.,
        p90: 300.,
    });
    f
}

fn config() -> Config {
    let mut cfg = Config::default();
    cfg.provider.model = "model".into();
    cfg.provider.prices.insert(
        "model".into(),
        crate::config::ModelPrice {
            input: 10.,
            output: 20.,
            cached_input: Some(1.),
            cache_write_input: Some(15.),
            ..Default::default()
        },
    );
    cfg
}

fn insert_run(store: &Store, sid: &str, id: &str) {
    store.conn.execute(
        "INSERT INTO runs(id,session_id,created_at,updated_at,status,steps) VALUES(?1,?2,?3,?3,'completed',1)",
        params![id, sid, DATE],
    ).unwrap();
}

fn usage(
    store: &Store,
    sid: &str,
    id: &str,
    identity: (&str, &str, &str),
    prompt: i64,
    price: (Option<f64>, bool),
) {
    let (provider, model, source) = identity;
    let (cost, unknown) = price;
    store.record_forecast_request(id, source).unwrap();
    store.conn.execute(
        "INSERT INTO run_usage(session_id,run_id,model,provider,source,prompt_tokens,completion_tokens,
            reasoning_tokens,cached_tokens,cache_write_tokens,cost,cost_unknown,usage_complete,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,20,0,30,10,?7,?8,1,?9)",
        params![sid,id,model,provider,source,prompt,cost,unknown,DATE],
    ).unwrap();
}

fn assert_parity(store: &Store, cfg: &Config) -> Value {
    let old = legacy_metrics(store, cfg).unwrap();
    let new = metrics(store, cfg).unwrap();
    assert_eq!(
        serde_json::to_vec(&new).unwrap(),
        serde_json::to_vec(&old).unwrap()
    );
    new
}

fn row<'a>(report: &'a Value, id: &str) -> &'a Value {
    report
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] == id)
        .unwrap()
}

#[test]
fn forecast_report_matches_exact_identities_dates_cost_and_completeness() {
    let store = Store::open_in_memory().unwrap();
    let cfg = config();
    let sid = store.create_session(None).unwrap().id;
    let f = forecast(&store, &sid, &cfg);
    let main = ("default", "model", "run");
    let nested = ("default", "model", "compaction");
    for id in [
        "z\0界",
        "z",
        "cost order",
        "known",
        "unpriced",
        "incomplete",
        "short main",
        "paused",
        "empty",
    ] {
        insert_run(&store, &sid, id);
        store.record_forecast(id, &f).unwrap();
    }
    usage(&store, &sid, "z\0界", main, 100, (None, false));
    usage(&store, &sid, "z", main, 101, (None, true));
    usage(&store, &sid, "known", main, 100, (None, false));
    usage(&store, &sid, "known", nested, 2000, (Some(0.1), true));
    usage(
        &store,
        &sid,
        "unpriced",
        ("a-provider", "missing", "run"),
        100,
        (None, false),
    );
    usage(&store, &sid, "incomplete", main, 100, (None, false));
    store.record_forecast_request("incomplete", "tool").unwrap();
    usage(&store, &sid, "short main", main, 100, (None, false));
    usage(&store, &sid, "short main", nested, 2000, (Some(0.1), true));
    usage(&store, &sid, "paused", main, 100, (None, false));

    // A different floating-point fold order changes the exact result.
    let largest = 9_007_199_254_740_992.0;
    let late = ("z-provider", "chosen", "run");
    let early = ("a-provider", "fallback", "run");
    for (identity, cost) in [(late, largest), (early, 1.0), (late, 1.0)] {
        usage(
            &store,
            &sid,
            "cost order",
            identity,
            100,
            (Some(cost), true),
        );
    }
    store.conn.execute_batch(
        "UPDATE runs SET created_at='2026-03-01T07:00:00+07:00', updated_at='2026-03-01T00:00:00.125Z' WHERE id='known';
         UPDATE runs SET updated_at='2026-02-28T23:59:59Z' WHERE id='unpriced';
         UPDATE runs SET created_at='invalid date' WHERE id='z';
         UPDATE runs SET steps=2 WHERE id='short main';
         UPDATE runs SET status='paused' WHERE id='paused';",
    ).unwrap();
    let report = assert_parity(&store, &cfg);
    assert_eq!(row(&report, "z\0界")["actual"]["tokens"], 120.0);
    assert_eq!(row(&report, "z")["actual"]["tokens"], 121.0);
    assert!(row(&report, "z")["actual"]["cost_usd"].is_null());
    assert_eq!(row(&report, "z")["actual"]["duration_ms"], 0);
    let known = row(&report, "known");
    assert_eq!(known["actual"]["duration_ms"], 125);
    assert_eq!(known["actual"]["peak_main_prompt_tokens"], 100);
    assert_eq!(known["actual"]["tokens"], 2140.0);
    assert!(known["actual"]["cost_usd"].as_f64().unwrap() > 0.1);
    assert_eq!(known["actual"]["basis_match"], true);
    assert_eq!(known["calibration_eligible"], true);
    assert_eq!(row(&report, "unpriced")["actual"]["duration_ms"], 0);
    assert!(row(&report, "unpriced")["actual"]["cost_usd"].is_null());
    for id in ["incomplete", "short main", "empty"] {
        assert_eq!(row(&report, id)["actual"]["usage_complete"], false);
        assert!(row(&report, id)["actual"]["tokens"].is_null());
        assert!(row(&report, id)["token_error"].is_null());
    }
    assert_eq!(row(&report, "paused")["actual"]["usage_complete"], true);
    assert!(row(&report, "paused")["comparison"]["steps"]["error"].is_null());
    let order = row(&report, "cost order");
    assert_eq!(
        order["actual"]["cost_usd"].as_f64().unwrap().to_bits(),
        largest.to_bits()
    );
    assert_eq!(
        order["actual"]["served_main"],
        json!([
            {"provider":"z-provider","model":"chosen"},
            {"provider":"a-provider","model":"fallback"}
        ])
    );
    assert_eq!(order["actual"]["basis_match"], false);
    assert_eq!(order["calibration_eligible"], false);

    store
        .conn
        .execute(
            "UPDATE run_usage SET usage_complete=0 WHERE run_id='known' AND source='compaction'",
            [],
        )
        .unwrap();
    let report = assert_parity(&store, &cfg);
    assert_eq!(row(&report, "known")["actual"]["usage_complete"], false);
    assert!(row(&report, "known")["actual"]["cost_usd"].is_null());
}

// SQLite trace handles come from this test's private connection and are
// removed before the stack-owned callback context is dropped.
fn statements(store: &Store, action: impl FnOnce() -> Value) -> (Value, (usize, usize)) {
    #[derive(Default)]
    struct Counts {
        queries: Cell<usize>,
        transaction: Cell<usize>,
    }
    unsafe extern "C" fn trace(
        _: c_uint,
        state: *mut c_void,
        stmt: *mut c_void,
        _: *mut c_void,
    ) -> c_int {
        let state = unsafe { &*state.cast::<Counts>() };
        let sql = unsafe { CStr::from_ptr(ffi::sqlite3_sql(stmt.cast())) }.to_bytes();
        let count = if sql.starts_with(b"SELECT") || sql.starts_with(b"WITH") {
            &state.queries
        } else {
            &state.transaction
        };
        count.set(count.get() + 1);
        0
    }
    struct Trace<'a>(&'a Connection);
    impl Drop for Trace<'_> {
        fn drop(&mut self) {
            unsafe {
                ffi::sqlite3_trace_v2(self.0.handle(), 0, None, std::ptr::null_mut());
            }
        }
    }
    let counts = Counts::default();
    assert_eq!(
        unsafe {
            ffi::sqlite3_trace_v2(
                store.conn.handle(),
                ffi::SQLITE_TRACE_STMT as c_uint,
                Some(trace),
                (&counts as *const Counts).cast_mut().cast(),
            )
        },
        ffi::SQLITE_OK
    );
    let _trace = Trace(&store.conn);
    let value = action();
    (value, (counts.queries.get(), counts.transaction.get()))
}

#[test]
fn forecast_report_retains_both_windows_and_measures_statements() {
    let store = Store::open_in_memory().unwrap();
    let cfg = config();
    let sid = store.create_session(None).unwrap().id;
    let other = store.create_session(None).unwrap().id;
    let f = forecast(&store, &sid, &cfg);
    let measure = |matched: usize| {
        let (old, old_statements) = statements(&store, || legacy_metrics(&store, &cfg).unwrap());
        let (new, new_statements) = statements(&store, || metrics(&store, &cfg).unwrap());
        assert_eq!(
            serde_json::to_vec(&old).unwrap(),
            serde_json::to_vec(&new).unwrap()
        );
        assert_eq!(old_statements, (3 + 5 * matched, 0));
        assert_eq!(new_statements, (if matched == 0 { 2 } else { 4 }, 2));
        println!("{matched} matched Runs: legacy {old_statements:?}, batched {new_statements:?} (queries, transaction statements)");
        new
    };
    assert_eq!(measure(0), json!([]));

    // All 2,001 share a creation timestamp. Rowid, not lexical ID or Session,
    // decides the exact global 2,000-Run membership.
    insert_run(&store, &sid, "outside");
    insert_run(&store, &other, "boundary");
    for n in 0..1799 {
        insert_run(&store, &sid, &format!("unquoted-{n}"));
    }
    let ids: Vec<_> = (0..200).rev().map(|n| format!("quoted-{n:03}")).collect();
    for (i, id) in ids.iter().enumerate() {
        insert_run(&store, &sid, id);
        store.record_forecast(id, &f).unwrap();
        usage(
            &store,
            &sid,
            id,
            ("default", "model", "run"),
            100,
            (None, false),
        );
        if i == 0 {
            assert_eq!(measure(1)[0]["run_id"], *id);
        }
    }
    let report = measure(200);
    assert_eq!(report.as_array().unwrap().len(), 200);
    for (stored, id) in report.as_array().unwrap().iter().zip(ids.iter().rev()) {
        assert_eq!(stored["run_id"], *id);
    }
    // Forecast rowid deliberately conflicts with Run rowid. The outside Run
    // occupies a slot in the 200 forecasts but does not get a report row.
    for (id, session) in [("boundary", &other), ("outside", &sid)] {
        store.record_forecast(id, &f).unwrap();
        usage(
            &store,
            session,
            id,
            ("default", "model", "run"),
            100,
            (None, false),
        );
    }
    let before = store.run_metrics_for(Some(&other), 1, Some(DATE)).unwrap();
    assert_eq!(before[0].run_id, "boundary");
    assert!(store
        .run_metrics_for(Some(&other), 1, Some("2027-01-01"))
        .unwrap()
        .is_empty());
    let report = measure(199);
    let reported: Vec<_> = report
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        reported,
        std::iter::once("boundary")
            .chain(ids[2..].iter().rev().map(String::as_str))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        store.run_metrics_for(Some(&other), 1, Some(DATE)).unwrap(),
        before
    );
    assert!(store
        .forecast_metric_inputs(&["not a Run"; 201], &cfg)
        .is_err());
}

#[test]
fn forecast_report_ignores_unrelated_damage_and_rejects_consumed_bad_rows() {
    let store = Store::open_in_memory().unwrap();
    let cfg = config();
    let sid = store.create_session(None).unwrap().id;
    let f = forecast(&store, &sid, &cfg);
    insert_run(&store, &sid, "good");
    insert_run(&store, &sid, "other");
    store.record_forecast("good", &f).unwrap();
    usage(
        &store,
        &sid,
        "good",
        ("default", "model", "run"),
        100,
        (None, false),
    );
    let baseline = assert_parity(&store, &cfg);
    store
        .conn
        .execute("UPDATE runs SET rating=x'ff' WHERE id='other'", [])
        .unwrap();
    assert!(legacy_metrics(&store, &cfg).is_err());
    assert_eq!(metrics(&store, &cfg).unwrap(), baseline);

    store
        .conn
        .execute_batch(
            "UPDATE runs SET rating='' WHERE id='other';
                              UPDATE runs SET rating=x'ff' WHERE id='good';",
        )
        .unwrap();
    assert!(legacy_metrics(&store, &cfg).is_err());
    assert!(metrics(&store, &cfg).is_err());
    store
        .conn
        .execute_batch(
            "UPDATE runs SET rating='' WHERE id='good';
                              UPDATE run_usage SET usage_complete=x'ff' WHERE run_id='good';",
        )
        .unwrap();
    assert!(legacy_metrics(&store, &cfg).is_err());
    assert!(metrics(&store, &cfg).is_err());
    usage(
        &store,
        &sid,
        "good",
        ("default", "model", "run"),
        100,
        (None, false),
    );
    // Opposite infinite REAL flags are malformed, but SQLite returns a NULL
    // SUM. The legacy COALESCE makes this incomplete rather than an error.
    store
        .conn
        .execute(
            "UPDATE run_usage SET usage_complete=CASE
                        WHEN rowid=(SELECT MIN(rowid) FROM run_usage WHERE run_id='good')
                        THEN 1e999 ELSE -1e999 END WHERE run_id='good'",
            [],
        )
        .unwrap();
    let report = assert_parity(&store, &cfg);
    assert_eq!(row(&report, "good")["actual"]["usage_complete"], false);

    store
        .conn
        .execute(
            "INSERT INTO run_forecasts VALUES('other','invalid JSON')",
            [],
        )
        .unwrap();
    assert!(legacy_metrics(&store, &cfg).is_err());
    assert!(metrics(&store, &cfg).is_err());
    assert!(store.conn.is_autocommit());
}

#[test]
fn forecast_report_queries_use_indexes_for_matched_runs_and_usage() {
    let store = Store::open_in_memory().unwrap();
    let explain = |sql: &str, params: &[&str]| -> Vec<String> {
        store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(rusqlite::params_from_iter(params), |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let run = explain(RUN_FACTS_SQL, &["[\"one\",\"two\"]"]);
    let usage = explain(USAGE_SQL, &["[\"one\",\"two\"]"]);
    let legacy_usage = explain(
        "SELECT provider,model,prompt_tokens,completion_tokens FROM run_usage WHERE run_id=?1",
        &["one"],
    );
    let legacy_distinct = explain(
        "SELECT DISTINCT provider,model FROM run_usage WHERE run_id=?1 AND source='run'",
        &["one"],
    );
    println!("SQLite {}\nRun facts: {run:?}\nUsage: {usage:?}\nLegacy usage: {legacy_usage:?}\nLegacy distinct: {legacy_distinct:?}", rusqlite::version());
    assert!(run.iter().any(|s| s.contains("MATERIALIZE recent")));
    assert!(run
        .iter()
        .any(|s| s.contains("SEARCH r USING INDEX sqlite_autoindex_runs_1")));
    assert!(run
        .iter()
        .any(|s| s.contains("SEARCH f USING INDEX sqlite_autoindex_forecast_request_counts_1")));
    assert!(run
        .iter()
        .any(|s| s.contains("SEARCH u USING INDEX idx_run_usage_run")));
    for plan in [&usage, &legacy_usage, &legacy_distinct] {
        assert!(plan.iter().any(|s| s.contains("idx_run_usage_run")));
        assert!(!plan.iter().any(|s| s.contains("FOR ORDER BY")));
    }
}

// Preserve the pre-batching query and price fold as the differential oracle.
fn legacy_metrics(st: &Store, cfg: &Config) -> Result<Value> {
    let runs = st.run_metrics(2000)?;
    let mut rows = Vec::new();
    for (id, f) in st.recorded_forecasts_limited(200)? {
        let Some(run) = runs.iter().find(|r| r.run_id == id) else {
            continue;
        };
        let (cost, main_peak, _) = legacy_usage(st, &id, cfg)?;
        let served = st.forecast_models(&id)?;
        let quoted_provider = f
            .basis
            .provider_key
            .rsplit_once(':')
            .map(|(name, _)| name)
            .unwrap_or(crate::config::DEFAULT_PROVIDER);
        let basis_match = !served.is_empty()
            && served
                .iter()
                .all(|(provider, model)| provider == quoted_provider && model == &f.basis.model);
        let served_main: Vec<_> = served
            .into_iter()
            .map(|(provider, model)| json!({"provider":provider,"model":model}))
            .collect();
        let complete = st.forecast_usage_complete(&id)?;
        let actual = (run.prompt_tokens + run.completion_tokens) as f64;
        let finished = run.status == "completed" && complete;
        let error = f
            .tokens
            .as_ref()
            .filter(|_| finished)
            .map(|r| actual - r.p50);
        let coverage = f
            .tokens
            .as_ref()
            .filter(|_| finished)
            .map(|r| actual >= r.p50 && actual <= r.p90);
        let comparison = |range: &Option<ForecastRange>, actual: Option<f64>| match (
            range,
            actual.filter(|_| finished),
        ) {
            (Some(r), Some(actual)) => {
                json!({"error":actual-r.p50,"covered_p50_p90":actual>=r.p50 && actual<=r.p90})
            }
            _ => json!({"error":null,"covered_p50_p90":null}),
        };
        let comparisons = json!({"steps":comparison(&f.steps,Some(run.steps as f64)),"tokens":comparison(&f.tokens,complete.then_some(actual)),"cost_usd":comparison(&f.cost_usd,cost.filter(|_|complete)),"duration_ms":comparison(&f.duration_ms,Some(run.duration_ms as f64))});
        rows.push(json!({"run_id":id,"model":f.basis.model,"role":f.basis.role,"status":run.status,"comparable_runs":f.comparable_runs,"forecast":f,"actual":{"steps":run.steps,"tokens":complete.then_some(actual),"cost_usd":cost.filter(|_|complete),"usage_complete":complete,"peak_main_prompt_tokens":main_peak,"served_main":served_main,"basis_match":basis_match,"duration_ms":run.duration_ms},"comparison":comparisons,"token_error":error,"token_covered_p50_p90":coverage,"calibration_eligible":finished && basis_match && main_peak>0}));
    }
    Ok(json!(rows))
}
fn legacy_usage(st: &Store, run_id: &str, cfg: &Config) -> Result<(Option<f64>, u64, u64)> {
    let mut stmt = st.ps("SELECT provider,model,prompt_tokens,completion_tokens,cached_tokens,cache_write_tokens,cost,source,cost_unknown FROM run_usage WHERE run_id=?1")?;
    let rows = stmt.query_map(params![run_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, Option<f64>>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, bool>(8)?,
        ))
    })?;
    let (mut cost, mut peak, mut cached) = (Some(0.0), 0, 0);
    let mut any = false;
    for row in rows {
        let (p, m, input, output, cache, write, reported, source, unknown) = row?;
        any = true;
        // Only the main Session request drives its compaction threshold;
        // nested llm/sub-agent/summarizer contexts have separate windows.
        if source == "run" {
            peak = peak.max(input.max(0) as u64);
        }
        cached += cache.max(0) as u64;
        cost = cost
            .zip(cfg.cost_of_known(&p, &m, input, output, cache, write, reported, unknown))
            .map(|(a, b)| a + b);
    }
    Ok((cost.filter(|_| any), peak, cached))
}
