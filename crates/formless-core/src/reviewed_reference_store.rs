//! Exact row removal under the same write boundary as inventory comparison.
use super::*;

impl Store {
    /// No reference-target I/O, artifact removal or reference-count mutation.
    pub(crate) fn detach_reviewed_reference(
        &self,
        session: &str,
        expected: &[Value],
        index: u32,
    ) -> Result<()> {
        self.transaction(|st| {
            st.get_session(session)?
                .filter(|_| !st.session_deleted(session).unwrap_or(true))
                .context("session not found")?;
            let current = st.pending_attachment_inventory(session)?;
            if current != expected {
                return Err(anyhow!("pending attachments changed; review the complete inventory again before removing a reference"));
            }
            let selected = current.get(index as usize).context("reference index is out of range")?;
            let text = |key: &str, max| selected[key].as_str().is_some_and(|s| !s.is_empty() && s.len() <= max);
            if selected["reference"] != true
                || !text("name", 512)
                || !text("path", 4096)
                || !selected["dir"].is_boolean()
                || selected["bytes"].as_u64().is_none()
            {
                return Err(anyhow!("selected row is not a supported pending reference; stored files require Formless TUI"));
            }
            let rowid: i64 = st.conn.query_row(
                "SELECT rowid FROM pending_attachments WHERE session_id=?1 ORDER BY rowid LIMIT 1 OFFSET ?2",
                params![session, i64::from(index)], |row| row.get(0),
            )?;
            if st.ex("DELETE FROM pending_attachments WHERE session_id=?1 AND rowid=?2", params![session, rowid])? != 1 {
                return Err(anyhow!("reviewed reference disappeared before removal"));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "reviewed_reference_store_tests.rs"]
mod tests;
