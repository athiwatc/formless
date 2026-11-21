//! Window-local, bounded message identities for returning to a Session.
//! No transcript, pixel offsets, selection, draft or filesystem persistence.
use std::collections::{HashSet, VecDeque};

pub const MAX_SESSIONS: usize = 64;
pub const MAX_ID_BYTES: usize = 256;

#[derive(Debug, Default)]
pub struct Positions {
    // Oldest use first. The absence of a saved message means follow latest.
    entries: VecDeque<(String, u64)>,
}
impl Positions {
    /// Save a message identity; `None` forgets an older position and follows
    /// latest on return. Invalid Session identities never enter the store.
    pub fn save(&mut self, session: &str, message: Option<u64>) {
        if session.is_empty() || session.len() > MAX_ID_BYTES {
            return;
        }
        self.entries.retain(|(id, _)| id != session);
        if let Some(message) = message {
            if self.entries.len() == MAX_SESSIONS {
                self.entries.pop_front();
            }
            self.entries.push_back((session.into(), message));
        }
    }
    pub fn get(&mut self, session: &str) -> Option<u64> {
        let index = self.entries.iter().position(|(id, _)| id == session)?;
        let entry = self.entries.remove(index).unwrap();
        let message = entry.1;
        self.entries.push_back(entry);
        Some(message)
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    /// Check the current directory once, with a temporary set bounded by
    /// this store rather than a second set of all loaded Sessions.
    pub fn retain_available<'a>(&mut self, available: impl IntoIterator<Item = &'a str>) {
        let mut absent: HashSet<String> = self.entries.iter().map(|(id, _)| id.clone()).collect();
        if absent.is_empty() {
            return;
        }
        for id in available {
            absent.remove(id);
            if absent.is_empty() {
                break;
            }
        }
        self.entries.retain(|(id, _)| !absent.contains(id));
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positions_are_by_session_and_message_and_latest_forgets_old_positions() {
        let mut positions = Positions::default();
        positions.save("A", Some(42));
        positions.save("B", Some(7));
        positions.save("A", Some(89));
        assert_eq!(positions.len(), 2);
        assert_eq!(positions.get("A"), Some(89));
        assert_eq!(positions.get("B"), Some(7));
        positions.save("A", None);
        assert_eq!(positions.get("A"), None);
        assert_eq!(positions.get("B"), Some(7));
        positions.clear();
        assert!(positions.is_empty());
    }
    #[test]
    fn bounded_lru_touches_restored_positions_and_never_retains_invalid_ids() {
        let mut positions = Positions::default();
        for i in 0..MAX_SESSIONS {
            positions.save(&format!("session-{i}"), Some(i as u64));
        }
        assert_eq!(positions.get("session-0"), Some(0));
        positions.save("new", Some(900));
        assert_eq!(positions.len(), MAX_SESSIONS);
        assert_eq!(positions.get("session-1"), None);
        assert_eq!(positions.get("session-0"), Some(0));
        positions.save("", Some(1));
        positions.save(&"😀".repeat(MAX_ID_BYTES), Some(1));
        assert_eq!(positions.len(), MAX_SESSIONS);
        positions.retain_available(["new"]);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions.get("new"), Some(900));
    }
}
