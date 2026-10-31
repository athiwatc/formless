//! Process credential environment isolation for tests. Each test binary has
//! one lock. Keep the guard in the test body until its async work has finished;
//! helpers must borrow it rather than acquiring another (non-reentrant) guard.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

static CREDENTIAL_ENV: Mutex<()> = Mutex::new(());

fn credential(key: &OsStr) -> bool {
    let key = key.to_string_lossy().to_ascii_uppercase();
    key == "FORMLESS_API_KEY"
        || key.starts_with("FORMLESS_API_KEY_")
        || key == "FORMLESS_OAUTH_ACCESS_TOKEN"
        || key == "FORMLESS_OAUTH_CLIENT_SECRET"
}

fn clear_credentials() {
    for (key, _) in std::env::vars_os().filter(|(key, _)| credential(key)) {
        std::env::remove_var(key);
    }
}

/// Serializes credential readers and writers, removes inherited overrides
/// (including named-provider and *_FILE keys), and restores on normal exit or
/// panic. Poison recovery is intentional: a failed test must not break siblings.
#[allow(dead_code)] // Different test binaries use different guard operations.
pub struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    credentials: Vec<(OsString, OsString)>,
    touched: RefCell<BTreeMap<OsString, Option<OsString>>>,
}

#[allow(dead_code)]
impl EnvGuard {
    pub fn isolated() -> Self {
        let lock = CREDENTIAL_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let credentials = std::env::vars_os()
            .filter(|(key, _)| credential(key))
            .collect();
        clear_credentials();
        Self {
            _lock: lock,
            credentials,
            touched: RefCell::new(BTreeMap::new()),
        }
    }

    pub fn set(&self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let key = key.as_ref();
        if !credential(key) {
            self.touched
                .borrow_mut()
                .entry(key.to_os_string())
                .or_insert_with(|| std::env::var_os(key));
        }
        std::env::set_var(key, value);
    }

    pub fn remove(&self, key: impl AsRef<OsStr>) {
        let key = key.as_ref();
        if !credential(key) {
            self.touched
                .borrow_mut()
                .entry(key.to_os_string())
                .or_insert_with(|| std::env::var_os(key));
        }
        std::env::remove_var(key);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        clear_credentials();
        for (key, value) in &self.credentials {
            std::env::set_var(key, value);
        }
        for (key, value) in self.touched.get_mut() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
