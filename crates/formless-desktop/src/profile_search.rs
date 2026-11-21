//! Explicit, read-only search of persisted messages in the connected Profile.
//! Replies belong to their query/operation/connection, never a Session snapshot.
use std::collections::HashSet;

use formless_protocol::SearchHit;

pub const QUERY_CHARS: usize = 256;
pub const LIMIT: usize = 50;

pub fn query(text: &str) -> Result<&str, &'static str> {
    let text = text.trim();
    if text.is_empty() || text.chars().take(QUERY_CHARS + 1).count() > QUERY_CHARS {
        return Err("Use a nonempty query of at most 256 characters.");
    }
    Ok(text)
}

pub fn decode(value: serde_json::Value) -> Result<Vec<SearchHit>, String> {
    if value.as_array().is_none_or(|hits| hits.len() > LIMIT) {
        return Err("Expected at most the requested 50 search results.".into());
    }
    let hits: Vec<SearchHit> = serde_json::from_value(value)
        .map_err(|_| "The daemon returned malformed search results.".to_string())?;
    let mut identities = HashSet::new();
    for hit in &hits {
        if hit.session_id.is_empty()
            || hit.session_id.len() > 256
            || hit.message_id.is_empty()
            || hit.message_id.len() > 256
            || hit.session_name.len() > 16 * 1024
            || hit.snippet.len() > 16 * 1024
            || hit.role.len() > 64
            || hit.created_at.len() > 128
            || hit.seq < 0
            || !identities.insert((&hit.session_id, &hit.message_id))
        {
            return Err("Search result identity or size is unsupported; use Formless TUI.".into());
        }
    }
    Ok(hits)
}

#[derive(Debug)]
struct Pending {
    connection: u64,
    operation: uuid::Uuid,
    query: String,
}

#[derive(Debug)]
pub struct State {
    pub revision: u64,
    pub hits: Vec<SearchHit>,
    pub selected: Option<usize>,
    pub notice: String,
    query: String,
    connection: Option<u64>,
    pending: Option<Pending>,
    valid: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: 0, hits: Vec::new(), selected: None,
            notice: "Search persisted messages in this Profile. Results are snippets, not full messages.".into(),
            query: String::new(), connection: None, pending: None, valid: false,
        }
    }
}

impl State {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn retire(&mut self, notice: &str) {
        self.revision = self.revision.wrapping_add(1);
        self.valid = false;
        self.connection = None;
        self.pending = None;
        self.notice = notice.into();
    }

    pub fn changed(&mut self, text: &str) {
        let basis = self
            .pending
            .as_ref()
            .map_or(self.query.as_str(), |p| &p.query);
        if (self.valid || self.pending.is_some()) && basis != text.trim() {
            self.retire("Query changed. Search again before opening a result.");
        }
    }

    pub fn start(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        text: &str,
    ) -> Result<String, &'static str> {
        let text = match query(text) {
            Ok(text) => text,
            Err(error) => {
                self.retire(error);
                return Err(error);
            }
        };
        self.retire("Searching persisted messages; previous results are unavailable until this reply arrives.");
        self.pending = Some(Pending {
            connection,
            operation,
            query: text.into(),
        });
        Ok(text.into())
    }

    pub fn receive(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        text: &str,
        result: Result<Vec<SearchHit>, String>,
    ) -> bool {
        if !self.pending.as_ref().is_some_and(|p| {
            p.connection == connection && p.operation == operation && p.query == text
        }) {
            return false;
        }
        self.pending = None;
        self.revision = self.revision.wrapping_add(1);
        match result {
            Ok(hits) => {
                self.query = text.into();
                self.connection = Some(connection);
                self.valid = true;
                self.selected = (!hits.is_empty()).then_some(0);
                self.notice=format!("{} matches returned · up to 50 · Persisted Profile messages, including archived Sessions. Open Session does not jump to the message.",hits.len());
                self.hits = hits;
            }
            Err(error) => self.retire(&format!(
                "Search failed: {}. Search again to retry.",
                crate::questions::preview(&error, 500)
            )),
        }
        true
    }

    pub fn current(&self, connection: u64, text: &str, revision: u64) -> bool {
        self.valid
            && self.pending.is_none()
            && self.connection == Some(connection)
            && self.query == text.trim()
            && self.revision == revision
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        self.selected = Some(match self.selected {
            Some(index) => index.saturating_add_signed(delta).min(self.hits.len() - 1),
            None if delta < 0 => self.hits.len() - 1,
            None => 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hits() -> Vec<SearchHit> {
        decode(json!([{"session_id":"s","session_name":"Session ไทย","message_id":"m","seq":1,"role":"assistant","snippet":"Found 😀","created_at":"now"}])).unwrap()
    }

    #[test]
    fn query_and_reply_bounds_reject_ambiguous_or_unsupported_results() {
        assert_eq!(query(" ไทย 😀 "), Ok("ไทย 😀"));
        assert!(query("  ").is_err());
        assert!(query(&"😀".repeat(257)).is_err());
        let hit = hits().remove(0);
        assert!(decode(serde_json::to_value(vec![hit.clone(); 51]).unwrap()).is_err());
        assert!(decode(serde_json::to_value(vec![hit.clone(); 2]).unwrap()).is_err());
        assert!(decode(serde_json::Value::Null).is_err());
        let mut bad = hit;
        bad.message_id.clear();
        assert!(decode(serde_json::to_value(vec![bad]).unwrap()).is_err());
    }

    #[test]
    fn delayed_or_foreign_replies_never_own_a_replacement_query() {
        let mut state = State::default();
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        state.start(1, a, "a").unwrap();
        state.start(1, b, "b").unwrap();
        assert!(!state.receive(1, a, "a", Ok(hits())));
        assert!(!state.receive(2, b, "b", Ok(hits())));
        assert!(!state.receive(1, b, "a", Ok(hits())));
        assert!(state.receive(1, b, "b", Ok(hits())));
        let version = state.revision;
        assert!(state.current(1, " b ", version));
        assert!(!state.current(2, "b", version));
        assert!(!state.current(1, "a", version));
        state.changed("a");
        assert!(!state.current(1, "b", version));
        assert!(!state.receive(1, b, "b", Ok(hits())));
    }

    #[test]
    fn disconnect_error_and_new_submission_retire_all_previous_row_callbacks() {
        let mut state = State::default();
        let a = uuid::Uuid::new_v4();
        state.start(0, a, "a").unwrap();
        state.receive(0, a, "a", Ok(hits()));
        let old = state.revision;
        state.retire("Disconnected");
        assert!(!state.current(0, "a", old));
        assert_eq!(state.hits.len(), 1);
        let b = uuid::Uuid::new_v4();
        state.start(1, b, "a").unwrap();
        assert!(!state.current(1, "a", state.revision));
        state.receive(1, b, "a", Err("offline".into()));
        assert!(!state.pending());
        assert!(state.notice.contains("Search again"));
        let c = uuid::Uuid::new_v4();
        state.start(1, c, "a").unwrap();
        state.receive(1, c, "a", Ok(Vec::new()));
        assert_eq!(state.selected, None);
        assert!(state.current(1, "a", state.revision));
    }
}
