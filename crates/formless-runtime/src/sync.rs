//! Locks that survive a panic (#717): a panic while a `Mutex` is held
//! poisons it, and with `lock().unwrap()` every later request touching it
//! panics too, so one bug becomes a dead daemon. `locked()` takes the
//! lock anyway (the data is whatever the panicking holder left, which
//! each field's users already tolerate) and reports where, once per site.

use std::sync::{Mutex, MutexGuard};

pub trait LockExt<T> {
    /// The lock, recovered from poison (reported once per call site).
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    #[track_caller]
    fn locked(&self) -> MutexGuard<'_, T> {
        let at = std::panic::Location::caller();
        self.lock().unwrap_or_else(|poisoned| {
            report_poison(at);
            poisoned.into_inner()
        })
    }
}

fn report_poison(at: &'static std::panic::Location<'static>) {
    static SEEN: Mutex<Vec<(&str, u32)>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let key = (at.file(), at.line());
    if !seen.contains(&key) {
        seen.push(key);
        eprintln!(
            "formless: recovered a lock poisoned by an earlier panic at {}:{}",
            at.file(),
            at.line()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let m = std::sync::Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("while holding it");
        })
        .join();
        assert!(m.is_poisoned());
        *m.locked() += 1;
        assert_eq!(*m.locked(), 2);
    }
}
