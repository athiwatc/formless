//! Admission for a profile's model work. One bounded lane reserves the entire
//! remaining headroom instead of pretending an uncapped response has a known
//! price. Completed usage must be committed before its lease is dropped.
use crate::{Config, Paths, Store};
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
            let cost: Option<f64> = rows
                .iter()
                .map(|(provider, model, p, c, cache, write, reported)| {
                    cfg.cost_of(provider, model, *p, *c, *cache, *write, *reported)
                })
                .sum();
            refuse("run_usd", cost, max)?;
        }
        if let Some(max) = cfg.limits.daily_usd.filter(|n| *n > 0.0) {
            let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let rows = st.usage_grouped(Some(&day), "model")?;
            let cost: Option<f64> = rows
                .iter()
                .map(|(_, provider, model, p, c, cache, write, reported)| {
                    cfg.cost_of(provider, model, *p, *c, *cache, *write, *reported)
                })
                .sum();
            refuse("daily_usd", cost, max)?;
        }
        Ok(Some(lease))
    }
}

fn refuse(name: &str, used: Option<f64>, max: f64) -> Result<()> {
    if used.is_some_and(|used| used >= max) {
        return Err(anyhow!("budget: limits.{name} reached before model dispatch. The Run is paused: /resume continues, /abandon stops."));
    }
    Ok(())
}
