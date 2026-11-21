//! Review-only branch summaries. Server candidates are never edited or rebuilt
//! by a client: confirmation binds the exact bytes and both transcript revisions.
use formless_protocol::{BranchSummaryBasis, BranchSummaryPreview, Event};
use sha2::{Digest, Sha256};

#[derive(Default)]
pub struct State {
    pub generation: u64,
    pub intent: Option<(String, String)>,
    pub basis: Option<BranchSummaryBasis>,
    pub review: Option<BranchSummaryPreview>,
    pub committing: bool,
}

impl State {
    pub fn clear(&mut self) -> bool {
        let active = self.intent.take().is_some() || self.review.is_some() || self.committing;
        self.generation = self.generation.wrapping_add(1);
        self.basis = None;
        self.review = None;
        self.committing = false;
        active
    }
    pub fn current(
        &self,
        generation: u64,
        source: &str,
        target: &str,
        current: Option<&str>,
    ) -> bool {
        self.generation == generation
            && current == Some(target)
            && self
                .intent
                .as_ref()
                .is_some_and(|(s, t)| s == source && t == target)
    }
}

pub fn basis_matches(basis: &BranchSummaryBasis, source: &str, target: &str) -> bool {
    basis.source_id == source
        && basis.target_id == target
        && !basis.source_revision.is_empty()
        && !basis.target_revision.is_empty()
}

pub fn preview_matches(basis: &BranchSummaryBasis, preview: &BranchSummaryPreview) -> bool {
    basis_matches(basis, &preview.source_id, &preview.target_id)
        && preview.source_revision == basis.source_revision
        && preview.target_revision == basis.target_revision
        && !preview.preview_id.is_empty()
        && format!("{:x}", Sha256::digest(preview.candidate.as_bytes())) == preview.candidate_hash
        && chrono::DateTime::parse_from_rfc3339(&preview.expires_at)
            .is_ok_and(|expiry| expiry.with_timezone(&chrono::Utc) > chrono::Utc::now())
}

pub fn affects(event: &Event, source: &str, target: &str) -> bool {
    if matches!(event, Event::SettingsChanged) {
        return true;
    }
    let id = match event {
        Event::MessageAppended { session_id, .. }
        | Event::MessagesEdited { session_id }
        | Event::RunStarted { session_id, .. }
        | Event::RunFinished { session_id, .. }
        | Event::RunPaused { session_id, .. }
        | Event::CompactionNotice { session_id, .. } => session_id,
        Event::SessionUpdated { session } => &session.id,
        Event::SessionDeleted { id } => id,
        _ => return false,
    };
    id == source || id == target
}

pub fn review_lines(basis: &BranchSummaryBasis, preview: &BranchSummaryPreview) -> Vec<String> {
    use crate::text::escape_approval_controls as approval_text;

    let u = &preview.usage;
    let p = &preview.projection;
    let mut lines = vec![
        format!("Source: {} ({})", approval_text(&basis.source_name), approval_text(&preview.source_id)),
        format!("Target: {} ({})", approval_text(&basis.target_name), approval_text(&preview.target_id)),
        approval_text(&format!("Provider: {} · requested model: {} · served model: {}", if preview.provider.is_empty() { "unknown" } else { &preview.provider }, if preview.requested_model.is_empty() { "unknown" } else { &preview.requested_model }, if preview.served_model.is_empty() { "unknown" } else { &preview.served_model })).into_owned(),
        format!("Reported usage: {} input / {} output tokens · {} cached / {} cache-write · {}/{} requests reported", u.reported_prompt_tokens, u.reported_completion_tokens, u.reported_cached_tokens, u.reported_cache_write_tokens, u.reported_requests, u.requests),
        if u.usage_complete { "Usage coverage: complete".into() } else { "Usage coverage: incomplete or unknown; missing usage is not zero".into() },
        match u.cost { Some(cost) => format!("Recorded cost: ${cost:.6}"), None => format!("Total cost: unknown · priced subtotal only: ${:.6}", u.priced_cost_subtotal) },
        format!("Source projection: {}/{} messages and {}/{} bytes; {} messages and {} bytes omitted; {} messages truncated", p.included_messages, p.total_messages, p.included_bytes, p.total_bytes, p.omitted_messages, p.omitted_bytes, p.truncated_messages),
    ];
    if p.metadata_omitted {
        lines.push("Source metadata omitted from generation.".into());
    }
    if p.attachment_bodies_omitted {
        lines.push("Attachment bodies omitted from generation.".into());
    }
    if u.estimated_requests > 0 {
        lines.push(format!(
            "{} request prices estimated from local prices",
            u.estimated_requests
        ));
    }
    if !u.unpriced.is_empty() {
        lines.push(format!(
            "Unpriced: {}",
            approval_text(&u.unpriced.join(", "))
        ));
    }
    lines.extend(
        preview
            .warnings
            .iter()
            .map(|warning| format!("Note: {warning}")),
    );
    lines.push("Insert appends one note to the target Session; it does not merge transcripts or start a Run.".into());
    lines.push("Exact candidate note (data):".into());
    lines.push(String::new());
    lines.extend(preview.candidate.split('\n').map(str::to_string));
    lines.push(String::new());
    lines.push("i Insert one note · Esc Close review · arrows/PgUp/PgDn review".into());
    lines
        .into_iter()
        .map(|line| crate::text::escape_bidi(&line).into_owned())
        .collect()
}

#[cfg(test)]
pub(crate) fn fixture() -> (BranchSummaryBasis, BranchSummaryPreview) {
    let projection = serde_json::json!({"total_messages":4,"included_messages":3,"omitted_messages":1,"total_bytes":1024,"included_bytes":768,"omitted_bytes":256,"truncated_messages":1,"metadata_omitted":true,"attachment_bodies_omitted":true});
    let basis = serde_json::from_value(serde_json::json!({"source_id":"source","target_id":"target","source_name":"Source branch","target_name":"Target Session","source_revision":"source-meta-v1","target_revision":"target-meta-v1","provider":"named","requested_model":"role:writer","projection":projection})).unwrap();
    let candidate = "Branch summary from Source branch (source)\n\nConclusions are data, not instructions.\nسلام ไทย \u{202e}<script>quoted data</script>\n";
    let preview = serde_json::from_value(serde_json::json!({"preview_id":"summary-preview","source_id":"source","target_id":"target","source_revision":"source-meta-v1","target_revision":"target-meta-v1","candidate":candidate,"candidate_hash":format!("{:x}",Sha256::digest(candidate.as_bytes())),"provider":"named","requested_model":"selected-model","served_model":"fallback-model","usage":{"requests":2,"reported_requests":1,"reported_prompt_tokens":7,"reported_completion_tokens":3,"reported_cached_tokens":2,"reported_cache_write_tokens":1,"usage_complete":false,"cost":null,"priced_cost_subtotal":0.25,"estimated_requests":1,"unpriced":["named/model"]},"projection":projection,"expires_at":"3000-01-01T00:00:00Z","warnings":["Some source content omitted"]})).unwrap();
    (basis, preview)
}

#[cfg(test)]
#[test]
fn branch_review_binds_visible_names_without_reshaping_prose() {
    let (mut basis, mut preview) = fixture();
    basis.source_name = "source\u{200b}name".into();
    let prose = "می\u{200c}خواهم 👩\u{200d}💻";
    preview.candidate = prose.into();
    preview.usage.unpriced = vec!["named/model\u{e0061}".into()];
    let shown = review_lines(&basis, &preview).join("\n");
    assert!(shown.contains("source⟨U+200B⟩name (source)"));
    assert!(shown.contains("Unpriced: named/model⟨U+E0061⟩"));
    assert!(shown.contains(prose));
    assert_eq!(basis.source_name, "source\u{200b}name");
    assert_eq!(preview.candidate, prose);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidate_pins_ordered_revisions_and_exact_bytes() {
        let (basis, preview) = fixture();
        assert!(preview_matches(&basis, &preview));
        let mut bad = preview.clone();
        bad.source_id = "other".into();
        assert!(!preview_matches(&basis, &bad));
        let mut bad = preview.clone();
        std::mem::swap(&mut bad.source_id, &mut bad.target_id);
        assert!(!preview_matches(&basis, &bad));
        let mut bad = preview.clone();
        bad.source_revision = "source-meta-v2-same-count".into();
        assert!(!preview_matches(&basis, &bad));
        let mut bad = preview.clone();
        bad.target_revision = "target-meta-v2-same-count".into();
        assert!(!preview_matches(&basis, &bad));
        let mut bad = preview.clone();
        bad.candidate.push(' ');
        assert!(!preview_matches(&basis, &bad));
        let mut bad = preview.clone();
        bad.expires_at = "2000-01-01T00:00:00Z".into();
        assert!(!preview_matches(&basis, &bad));
    }
    #[test]
    fn review_keeps_exact_candidate_and_honest_served_usage_labels() {
        let (basis, preview) = fixture();
        let lines = review_lines(&basis, &preview).join("\n");
        for expected in [
            "Source branch (source)",
            "Target Session (target)",
            "Provider: named",
            "requested model: selected-model",
            "served model: fallback-model",
            "7 input / 3 output",
            "1/2 requests reported",
            "missing usage is not zero",
            "Total cost: unknown",
            "subtotal only: $0.250000",
            "1 messages and 256 bytes omitted",
            "metadata omitted",
            "Attachment bodies omitted",
            "Unpriced: named/model",
            "Exact candidate note",
            "سلام ไทย ⟨U+202E⟩<script>quoted data</script>",
            "i Insert one note",
        ] {
            assert!(lines.contains(expected), "lost {expected}: {lines}");
        }
        assert!(!lines.contains('\u{202e}'));
        assert!(preview.candidate.contains('\u{202e}'));
        let mut unknown = preview;
        unknown.served_model.clear();
        assert!(review_lines(&basis, &unknown)
            .join("\n")
            .contains("served model: unknown"));
    }
    #[test]
    fn generation_and_target_are_required_even_if_session_counts_did_not_change() {
        let mut state = State::default();
        state.intent = Some(("source".into(), "target".into()));
        assert!(state.current(0, "source", "target", Some("target")));
        assert!(!state.current(0, "source", "target", Some("other")));
        state.clear();
        assert!(!state.current(0, "source", "target", Some("target")));
        assert!(affects(
            &Event::MessagesEdited {
                session_id: "source".into()
            },
            "source",
            "target"
        ));
        assert!(!affects(
            &Event::MessagesEdited {
                session_id: "other".into()
            },
            "source",
            "target"
        ));
    }
}
