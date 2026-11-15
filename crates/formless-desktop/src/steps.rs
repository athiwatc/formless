//! Bounded, read-only Step inventory. Never construct decisions from previews.
use formless_protocol::{PendingSteps, MAX_PENDING_STEPS, STEP_PREVIEW_BYTES};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Inventory {
    #[default]
    Unavailable,
    Unsupported,
    Snapshot(PendingSteps),
}
fn identity(text: &str, limit: usize) -> bool {
    !text.is_empty() && text.len() <= limit && !text.chars().any(char::is_control)
}
impl Inventory {
    pub fn decode(session: &str, value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::Unavailable;
        };
        let Some(rows) = value.get("prompts").and_then(Value::as_array) else {
            return Self::Unsupported;
        };
        if rows.len() > MAX_PENDING_STEPS {
            return Self::Unsupported;
        }
        // Bound fields before cloning/decoding untrusted response values.
        if rows.iter().any(|row| {
            ["session_id", "run_id", "step_id", "tool_name"]
                .iter()
                .any(|key| {
                    row.get(key)
                        .and_then(Value::as_str)
                        .is_none_or(|s| s.len() > 512)
                })
                || row
                    .get("arguments_preview")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s.len() > STEP_PREVIEW_BYTES)
        }) {
            return Self::Unsupported;
        }
        let Ok(snapshot) = serde_json::from_value::<PendingSteps>(value.clone()) else {
            return Self::Unsupported;
        };
        let mut ids = HashSet::new();
        if snapshot.prompts.iter().any(|p| {
            p.session_id != session
                || !identity(&p.session_id, 256)
                || !identity(&p.run_id, 256)
                || !identity(&p.step_id, 256)
                || !identity(&p.tool_name, 512)
                || !ids.insert(&p.step_id)
                || p.count == 0
                || p.index >= p.count
                || p.arguments_bytes < p.arguments_preview.len()
                || (p.arguments_bytes > p.arguments_preview.len()
                    && p.arguments_preview.len() < STEP_PREVIEW_BYTES.saturating_sub(3))
        }) {
            return Self::Unsupported;
        }
        Self::Snapshot(snapshot)
    }
    pub fn summary(&self) -> Option<String> {
        match self {
            Self::Unavailable => None,
            Self::Unsupported => Some("Step inventory could not be read. Use Formless TUI to inspect pending Steps; no native decision was sent.".into()),
            Self::Snapshot(s) if s.prompts.is_empty() && !s.more => None,
            Self::Snapshot(s) => {
                let mut text = "Step prompts at last canonical refresh · read only; may have changed. Use Formless TUI to inspect full arguments and decide.".to_string();
                for p in s.prompts.iter().take(3) {
                    use std::fmt::Write;
                    let preview: String = p.arguments_preview.chars().take(256).map(|c| if c.is_control() { '�' } else { c }).collect();
                    let _ = write!(text,"\nTool {} · call {} of {} · Run {} · Step {}\nArguments preview: {}{} ({} original bytes)",crate::questions::preview(&crate::directory::visual_text(&p.tool_name),120),p.index+1,p.count,p.run_id,p.step_id,preview,if p.arguments_bytes>p.arguments_preview.len() || p.arguments_preview.chars().count()>256 {" … [preview truncated]"} else {""},p.arguments_bytes);
                }
                if s.more || s.prompts.len()>3 {text.push_str("\nMore Step prompts exist; inspect the complete inventory in Formless TUI.");}
                Some(text)
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn value() -> Value {
        json!({"prompts":[{"session_id":"s","run_id":"r","step_id":"p","tool_name":"tool","arguments_preview":"{}","arguments_bytes":2,"index":0,"count":2}],"more":false})
    }
    #[test]
    fn restores_identity_without_an_event_and_preserves_legacy_absence() {
        assert_eq!(Inventory::decode("s", None), Inventory::Unavailable);
        let inventory = Inventory::decode("s", Some(&value()));
        let summary = inventory.summary().unwrap();
        for text in [
            "Step p",
            "Run r",
            "call 1 of 2",
            "last canonical refresh",
            "read only",
        ] {
            assert!(summary.contains(text));
        }
        assert!(
            Inventory::decode("s", Some(&json!({"prompts":[],"more":false})))
                .summary()
                .is_none()
        );
    }
    #[test]
    fn rejects_wrong_session_duplicate_and_invalid_definitions_without_partial_display() {
        let mut wrong = value();
        wrong["prompts"][0]["session_id"] = json!("other");
        let mut duplicate = value();
        duplicate["prompts"]
            .as_array_mut()
            .unwrap()
            .push(value()["prompts"][0].clone());
        let mut index = value();
        index["prompts"][0]["index"] = json!(2);
        let mut short = value();
        short["prompts"][0]["arguments_bytes"] = json!(1);
        let mut unknown = value();
        unknown["prompts"][0]["arguments_bytes"] = json!(100);
        let mut control = value();
        control["prompts"][0]["step_id"] = json!("p\n");
        for bad in [
            wrong,
            duplicate,
            index,
            short,
            unknown,
            control,
            json!(null),
        ] {
            assert_eq!(Inventory::decode("s", Some(&bad)), Inventory::Unsupported);
        }
    }
    #[test]
    fn rejects_oversized_rows_and_previews_and_labels_partial_inventory() {
        let mut bad = value();
        bad["prompts"][0]["arguments_preview"] = json!("x".repeat(STEP_PREVIEW_BYTES + 1));
        assert_eq!(Inventory::decode("s", Some(&bad)), Inventory::Unsupported);
        let bad =
            json!({"prompts":vec![value()["prompts"][0].clone();MAX_PENDING_STEPS+1],"more":true});
        assert_eq!(Inventory::decode("s", Some(&bad)), Inventory::Unsupported);
        let mut partial = value();
        partial["prompts"][0]["arguments_preview"] = json!("ก".repeat(STEP_PREVIEW_BYTES / 3));
        partial["prompts"][0]["arguments_bytes"] = json!(9999);
        partial["more"] = json!(true);
        let text = Inventory::decode("s", Some(&partial)).summary().unwrap();
        assert!(text.contains("preview truncated") && text.contains("More Step prompts"));
    }
}
