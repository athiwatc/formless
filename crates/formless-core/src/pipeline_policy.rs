//! Ownership for live pipeline policies. Weak registry slots are bounded and
//! never keep private Run-only instructions alive after their real owners end.
use crate::pipeline::CompiledRunOptions;
use anyhow::{ensure, Result};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

pub(crate) const MAX_POLICIES: usize = 32;
pub(crate) struct RunPolicy {
    pub options: CompiledRunOptions,
    repair_claimed: AtomicBool,
}
impl RunPolicy {
    pub fn new(options: CompiledRunOptions) -> Self {
        Self {
            options,
            repair_claimed: AtomicBool::new(false),
        }
    }
    pub fn claim_repair(&self) -> bool {
        self.repair_claimed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    pub fn digest(&self) -> String {
        crate::forecast::hash(self.options.options())
    }
    pub fn marker(&self) -> PolicyMarker {
        let options = self.options.options();
        PolicyMarker {
            digest: self.digest(),
            max_usd: options.max_usd,
            system_hash: options.system.as_ref().map(crate::forecast::hash),
            schema_hash: options.schema.as_ref().map(crate::forecast::hash),
        }
    }
}

/// Only immutable non-text evidence survives a daemon restart. A marker with
/// no live registry owner must fail closed; it is never a default-policy Run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct PolicyMarker {
    pub digest: String,
    pub max_usd: Option<f64>,
    pub system_hash: Option<String>,
    pub schema_hash: Option<String>,
}

#[derive(Default)]
pub(crate) struct Registry {
    entries: BTreeMap<String, Weak<RunPolicy>>,
}
impl Registry {
    pub fn prune(&mut self) {
        self.entries.retain(|_, policy| policy.strong_count() > 0);
    }
    pub fn ensure_capacity(&mut self) -> Result<()> {
        self.prune();
        ensure!(self.entries.len() < MAX_POLICIES, "too many live pipeline Run policies; wait for their background work before starting another");
        Ok(())
    }
    pub fn insert(&mut self, run: &str, policy: &Arc<RunPolicy>) -> Result<()> {
        ensure!(!run.is_empty(), "pipeline policy requires an actual Run id");
        if let Some(previous) = self.lookup(run) {
            ensure!(
                previous.digest() == policy.digest(),
                "Run pipeline policy cannot be changed"
            );
            return Ok(());
        }
        self.ensure_capacity()?;
        self.entries.insert(run.into(), Arc::downgrade(policy));
        Ok(())
    }
    pub fn lookup(&self, run: &str) -> Option<Arc<RunPolicy>> {
        self.entries.get(run).and_then(Weak::upgrade)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_protocol::RunOptions;
    fn policy(max: f64) -> Arc<RunPolicy> {
        Arc::new(RunPolicy::new(
            CompiledRunOptions::compile(RunOptions {
                max_usd: Some(max),
                ..Default::default()
            })
            .unwrap(),
        ))
    }
    #[test]
    fn background_and_physical_owners_keep_the_original_run_policy_live() {
        let mut registry = Registry::default();
        let root = policy(1.0);
        registry.insert("old-run", &root).unwrap();
        let background = root.clone();
        drop(root); // Main Run terminal cleanup is not policy cleanup.
        let request_lease = registry.lookup("old-run").unwrap();
        drop(background);
        registry.prune();
        assert_eq!(
            registry
                .lookup("old-run")
                .unwrap()
                .options
                .options()
                .max_usd,
            Some(1.0)
        );
        assert!(registry.lookup("new-run").is_none());
        drop(request_lease);
        registry.prune();
        assert!(registry.lookup("old-run").is_none());
    }
    #[test]
    fn live_policies_are_not_evicted_and_dead_slots_can_be_reused() {
        let mut registry = Registry::default();
        let mut owners = Vec::new();
        for i in 0..MAX_POLICIES {
            let owner = policy(i as f64);
            registry.insert(&format!("run-{i}"), &owner).unwrap();
            owners.push(owner);
        }
        assert!(registry.insert("overflow", &policy(9.0)).is_err());
        assert!(registry.lookup("run-0").is_some());
        owners.remove(0);
        registry.insert("replacement", &policy(3.0)).unwrap();
        assert!(registry.lookup("run-0").is_none());
    }
    #[test]
    fn repair_is_claimed_once_and_markers_never_duplicate_raw_prompt_or_schema() {
        let owner = Arc::new(RunPolicy::new(
            CompiledRunOptions::compile(RunOptions {
                system: Some("private-only instruction".into()),
                schema: Some(
                    serde_json::json!({"type":"string","description":"private schema annotation"}),
                ),
                max_usd: Some(0.0),
                ..Default::default()
            })
            .unwrap(),
        ));
        let concurrent = owner.clone();
        assert!(owner.claim_repair());
        assert!(!concurrent.claim_repair());
        let marker = serde_json::to_string(&owner.marker()).unwrap();
        assert!(!marker.contains("private-only"));
        assert!(!marker.contains("private schema"));
        assert_eq!(owner.marker().max_usd, Some(0.0));
        let mut registry = Registry::default();
        registry.insert("one", &owner).unwrap();
        assert!(registry.insert("one", &policy(5.0)).is_err());
    }
}
