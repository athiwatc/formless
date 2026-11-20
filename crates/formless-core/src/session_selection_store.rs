//! Compare and write the reviewed model/effort pair under one existing DB lock.
use super::*;
use formless_protocol::{SessionModelSelection, SESSION_EFFORT_MAX_BYTES, SESSION_MODEL_MAX_BYTES};
fn validate(pair: &SessionModelSelection, desired: bool) -> Result<()> {
    for (value, limit) in [
        (&pair.model, SESSION_MODEL_MAX_BYTES),
        (&pair.reasoning_effort, SESSION_EFFORT_MAX_BYTES),
    ] {
        if value.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > limit
                || s.trim() != s
                || s.chars().any(|c| {
                    c.is_control()
                        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
        }) {
            return Err(anyhow!("model/effort pair is invalid or exceeds bounds"));
        }
    }
    if pair.model.as_deref().is_some_and(|s| {
        s.strip_prefix("role:").is_some_and(|r| r.trim().is_empty())
            || (desired && s.eq_ignore_ascii_case("auto"))
    }) {
        return Err(anyhow!(
            "choose an explicit model or Role, or inherit defaults"
        ));
    }
    Ok(())
}
impl Store {
    pub(crate) fn compare_set_session_model(
        &self,
        id: &str,
        expected: &SessionModelSelection,
        desired: &SessionModelSelection,
    ) -> Result<SessionInfo> {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(anyhow!("invalid Session identity"));
        }
        validate(expected, false)?;
        validate(desired, true)?;
        self.transaction(|st| {
            let current = st
                .get_session(id)?
                .filter(|_| !st.session_deleted(id).unwrap_or(true))
                .context("Session is unavailable")?;
            if current.model != expected.model || current.reasoning_effort != expected.reasoning_effort {
                return Err(anyhow!("Session model/effort changed; reload the complete pair before saving"));
            }
            if expected == desired {
                return Ok(current);
            }
            // IMMEDIATE transaction serializes external SQLite writers as well
            // as daemon clients. Compare both nullable values in this same write
            // statement too; unrelated metadata changes are preserved.
            let changed=st.ex("UPDATE sessions SET model=?2,reasoning_effort=?3,updated_at=?4 WHERE id=?1 AND deleted_at IS NULL AND model IS ?5 AND reasoning_effort IS ?6",params![id,desired.model,desired.reasoning_effort,now(),expected.model,expected.reasoning_effort])?;
            if changed != 1 {
                return Err(anyhow!("Session model/effort changed; nothing was saved"));
            }
            st.get_session(id)?.context("Session is unavailable")
        })
    }
}
#[cfg(test)]
#[path = "session_selection_store_tests.rs"]
mod tests;
