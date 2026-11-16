//! Metadata compare-and-append boundary. No file paths or bodies are opened here.
use super::*;

pub(super) fn validate(session: &str, expected: &[Value]) -> Result<()> {
    if session.is_empty() || session.len() > 256 || session.chars().any(char::is_control) {
        return Err(RequestFailure::invalid("invalid Session identity").into());
    }
    if expected.len() > PENDING_ATTACHMENT_MAX_ENTRIES || expected.iter().any(|v| !v.is_object()) {
        return Err(RequestFailure::invalid(
            "reviewed attachment metadata exceeds bounds or is invalid",
        )
        .into());
    }
    // Bound retained request metadata after decoding, without allocating another
    // complete serialization. Stored whitespace is not part of JSON equality.
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > PENDING_ATTACHMENT_METADATA_BYTES {
                return Err(std::io::Error::other("reviewed metadata limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    for record in expected {
        serde_json::to_writer(&mut count, record)
            .map_err(|_| RequestFailure::invalid("reviewed attachment metadata exceeds bounds"))?;
    }
    Ok(())
}

/// Must run inside the same IMMEDIATE transaction as append_user_message_store.
pub(super) fn check(st: &Store, session: &str, expected: Option<&[Value]>) -> Result<()> {
    if let Some(expected) = expected {
        st.get_session(session)?
            .filter(|_| !st.session_deleted(session).unwrap_or(true))
            .context(RequestFailure::not_found("session not found"))?;
        let current = st.pending_attachment_inventory(session)?;
        if current != expected {
            return Err(RequestFailure::invalid(
                "pending attachments changed; review the complete inventory again before sending",
            )
            .into());
        }
    }
    Ok(())
}
