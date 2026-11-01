//! One daemon owns this profile. Serialize its result merges and background
//! reservations; never hold the lock while a provider request is running.
use crate::provider::ModelTest;
use anyhow::{bail, Context, Result};
use formless_runtime::LockExt;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) struct ModelTestCache {
    file: PathBuf,
    active: Mutex<HashSet<String>>,
}
pub(crate) struct Reservation {
    cache: Arc<ModelTestCache>,
    key: String,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.cache.active.locked().remove(&self.key);
    }
}
impl ModelTestCache {
    pub(crate) fn new(file: PathBuf) -> Self {
        Self {
            file,
            active: Mutex::new(HashSet::new()),
        }
    }
    fn key(endpoint: &str, model: &str) -> String {
        format!("{endpoint} {model}")
    }
    fn read_unlocked(&self) -> Result<Map<String, Value>> {
        match std::fs::symlink_metadata(&self.file) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(error).context("inspect model test results"),
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => bail!(
                "model test results must be a regular file: {}",
                self.file.display()
            ),
            Ok(_) => {}
        }
        let bytes = std::fs::read(&self.file).context("read model test results")?;
        serde_json::from_slice(&bytes)
            .context("parse model test results; existing data was preserved")
    }
    pub(crate) fn results(&self) -> Result<Map<String, Value>> {
        let _lock = self.active.locked();
        self.read_unlocked()
    }
    pub(crate) fn reserve(
        self: &Arc<Self>,
        endpoint: &str,
        model: &str,
    ) -> Result<Option<Reservation>> {
        let key = Self::key(endpoint, model);
        let mut active = self.active.locked();
        if active.contains(&key) || self.read_unlocked()?.contains_key(&key) {
            return Ok(None);
        }
        active.insert(key.clone());
        Ok(Some(Reservation {
            cache: self.clone(),
            key,
        }))
    }
    pub(crate) fn remember(&self, endpoint: &str, test: &ModelTest) -> Result<()> {
        self.remember_with_writer(endpoint, test, |file, contents| {
            crate::config::write_secret_file(file, contents)?;
            #[cfg(unix)]
            if let Some(parent) = file.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        })
    }
    fn remember_with_writer(
        &self,
        endpoint: &str,
        test: &ModelTest,
        write: impl FnOnce(&Path, &str) -> Result<()>,
    ) -> Result<()> {
        let _lock = self.active.locked();
        let mut all = self.read_unlocked()?;
        all.insert(
            Self::key(endpoint, &test.model),
            serde_json::to_value(test)?,
        );
        write(&self.file, &serde_json::to_string_pretty(&all)?)
            .context("atomically save model test results")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("fl-model-test-cache-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn cache(&self) -> Arc<ModelTestCache> {
            Arc::new(ModelTestCache::new(self.0.join("model_tests.json")))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn result(model: &str) -> ModelTest {
        ModelTest {
            model: model.into(),
            ok: true,
            ..Default::default()
        }
    }
    #[test]
    fn reservations_deduplicate_only_the_same_key_and_release_on_drop() {
        let f = Fixture::new();
        let cache = f.cache();
        let first = cache.reserve("https://one", "model").unwrap().unwrap();
        assert!(cache.reserve("https://one", "model").unwrap().is_none());
        let other = cache.reserve("https://two", "model").unwrap().unwrap();
        drop(first);
        assert!(cache.reserve("https://one", "model").unwrap().is_some());
        cache.remember("https://one", &result("model")).unwrap();
        assert!(cache.reserve("https://one", "model").unwrap().is_none());
        drop(other);
    }
    #[test]
    fn atomic_results_preserve_other_keys_and_old_file_on_writer_failure() {
        let f = Fixture::new();
        let cache = f.cache();
        cache.remember("https://one", &result("first")).unwrap();
        let old = std::fs::read(&cache.file).unwrap();
        assert!(cache
            .remember_with_writer("https://two", &result("second"), |_, _| bail!(
                "private injected failure before replacement"
            ))
            .is_err());
        assert_eq!(std::fs::read(&cache.file).unwrap(), old);
        cache.remember("https://two", &result("second")).unwrap();
        let all = cache.results().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key("https://one first") && all.contains_key("https://two second"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&cache.file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&cache.file, b"{broken fixture").unwrap();
        assert!(cache.remember("https://three", &result("third")).is_err());
        assert_eq!(std::fs::read(&cache.file).unwrap(), b"{broken fixture");
    }
    #[test]
    fn concurrent_merges_keep_both_results_when_a_writer_is_in_progress() {
        let f = Fixture::new();
        let cache = f.cache();
        let (loaded_tx, loaded_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = cache.clone();
        let writer = std::thread::spawn(move || {
            first.remember_with_writer("one", &result("first"), |path, body| {
                loaded_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                crate::config::write_secret_file(path, body)
            })
        });
        loaded_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(
            cache.active.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        let (called_tx, called_rx) = std::sync::mpsc::channel();
        let second = cache.clone();
        let next = std::thread::spawn(move || {
            called_tx.send(()).unwrap();
            second.remember("two", &result("second"))
        });
        called_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        release_tx.send(()).unwrap();
        writer.join().unwrap().unwrap();
        next.join().unwrap().unwrap();
        let all = cache.results().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key("one first") && all.contains_key("two second"));
    }
}
