//! Admission for a profile's model work. One bounded lane reserves the entire
//! remaining headroom instead of pretending an uncapped response has a known
//! price. Completed usage must be committed before its lease is dropped.
use crate::{Config, Paths, Store};
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Known nonnegative charges remain a spending floor even when another group
/// has no price. This is not a complete displayed total or another usage write.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Spend {
    pub known: f64,
    pub complete: bool,
}

impl FromIterator<Option<f64>> for Spend {
    fn from_iter<T: IntoIterator<Item = Option<f64>>>(costs: T) -> Self {
        let mut spend = Self {
            known: 0.0,
            complete: true,
        };
        for cost in costs {
            match cost.filter(|cost| cost.is_finite() && *cost >= 0.0) {
                Some(cost) => {
                    let sum = spend.known + cost;
                    if !sum.is_finite() {
                        spend.complete = false;
                    }
                    spend.known = sum.min(f64::MAX);
                }
                None => spend.complete = false,
            }
        }
        spend
    }
}

#[derive(Default)]
pub(crate) struct Admission {
    lane: Arc<tokio::sync::Mutex<()>>,
    accounting_failed: AtomicBool,
    #[cfg(test)]
    pub entered: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub changed: tokio::sync::Notify,
}

impl Admission {
    pub fn accounting_failed(&self) {
        self.accounting_failed.store(true, Ordering::SeqCst);
    }

    pub async fn enter(
        &self,
        store: &Mutex<Store>,
        paths: &Paths,
        run: &str,
        waived: bool,
    ) -> Result<Option<tokio::sync::OwnedMutexGuard<()>>> {
        if self.accounting_failed.load(Ordering::SeqCst) {
            return Err(anyhow!("budget: usage accounting failed; repair storage and restart before sending more model requests"));
        }
        // Read-only effective configuration: no daemon/config mutex is held
        // while waiting, and no store lock is held while reading settings.
        let cfg = Config::load(paths)?;
        let capped = [cfg.limits.run_usd, cfg.limits.daily_usd]
            .into_iter()
            .flatten()
            .any(|n| n > 0.0);
        if !capped || waived {
            return Ok(None);
        }
        #[cfg(test)]
        {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_one();
        }
        let lease = self.lane.clone().lock_owned().await;
        if self.accounting_failed.load(Ordering::SeqCst) {
            return Err(anyhow!("budget: usage accounting failed; restart after repairing storage before sending more model requests"));
        }
        // A queued request must not retain the pre-wait limit or price table.
        // SetSetting reports applies=now, including while another call is held.
        let cfg = Config::load(paths)?;
        let st = store
            .lock()
            .map_err(|_| anyhow!("budget: usage store is unavailable"))?;
        if let Some(max) = cfg.limits.run_usd.filter(|n| *n > 0.0 && !run.is_empty()) {
            let (rows, _) = st.usage_rows(None, Some(run))?;
            if rows.iter().any(|row| row.7) {
                return Err(anyhow!("budget: native media cost is unknown for this Run; strict run_usd cannot authorize more spending"));
            }
            let cost: Spend = rows
                .iter()
                .map(|(provider, model, p, c, cache, write, reported, unknown)| {
                    cfg.cost_of_known(provider, model, *p, *c, *cache, *write, *reported, *unknown)
                })
                .collect();
            refuse("run_usd", cost, max)?;
        }
        if let Some(max) = cfg.limits.daily_usd.filter(|n| *n > 0.0) {
            let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let rows = st.usage_grouped(Some(&day), "model")?;
            if rows.iter().any(|row| row.8) {
                return Err(anyhow!("budget: native media cost is unknown today; strict daily_usd cannot authorize more spending"));
            }
            let cost: Spend = rows
                .iter()
                .map(
                    |(_, provider, model, p, c, cache, write, reported, unknown)| {
                        cfg.cost_of_known(
                            provider, model, *p, *c, *cache, *write, *reported, *unknown,
                        )
                    },
                )
                .collect();
            refuse("daily_usd", cost, max)?;
        }
        Ok(Some(lease))
    }
}

fn refuse(name: &str, used: Spend, max: f64) -> Result<()> {
    if used.known >= max {
        return Err(anyhow!("budget: limits.{name} reached before model dispatch. The Run is paused: /resume continues, /abandon stops."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reported_usd_precedes_prices_and_both_known_sources_contribute_to_the_floor() {
        let mut cfg = Config::default();
        cfg.provider.prices.insert(
            "priced".into(),
            crate::config::ModelPrice {
                input: 1000.0,
                output: 1000.0,
                cached_input: None,
                cache_write_input: None,
                retrieved_at: None,
            },
        );
        let priced = cfg.cost_of_known("default", "priced", 2000, 0, 0, 0, None, false);
        let actual = cfg.cost_of_known(
            "default",
            "priced",
            9_000_000,
            9_000_000,
            0,
            0,
            Some(0.25),
            true,
        );
        let missing = cfg.cost_of_known("default", "missing", 7, 5, 0, 0, None, false);
        assert_eq!(priced, Some(2.0));
        assert_eq!(actual, Some(0.25));
        assert_eq!(missing, None);
        let spend: Spend = [priced, actual, missing].into_iter().collect();
        assert_eq!(spend.known, 2.25);
        assert!(!spend.complete);
        assert!(refuse("daily_usd", spend, 2.0).is_err());
    }
    #[test]
    fn unknown_groups_cannot_erase_known_nonnegative_spend() {
        let spend: Spend = [Some(2.0), None].into_iter().collect();
        assert_eq!(spend.known, 2.0);
        assert!(!spend.complete);
        assert!(refuse("daily_usd", spend, 1.0).is_err());
        assert!(refuse("run_usd", [None].into_iter().collect(), 1.0).is_ok());
        assert!(refuse("run_usd", [Some(0.25), None].into_iter().collect(), 1.0).is_ok());
        let invalid: Spend = [Some(2.0), Some(-100.0), Some(f64::NAN), Some(f64::INFINITY)]
            .into_iter()
            .collect();
        assert_eq!(invalid.known, 2.0);
        assert!(!invalid.complete);
        let overflow: Spend = [Some(f64::MAX), Some(f64::MAX), None].into_iter().collect();
        assert_eq!(overflow.known, f64::MAX);
    }
}
