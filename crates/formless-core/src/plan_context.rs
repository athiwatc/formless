//! Stable, bounded checklist data appended only to the projected context clone.
use super::Store;
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub(super) fn append(store: &Store, session: &str, context: &mut [Value]) -> Result<bool> {
    let (items, mut state) = store.session_plan_snapshot(session)?;
    if items.is_empty() {
        return Ok(false);
    }
    let omitted_items = items.len().saturating_sub(crate::tools::MAX_PLAN_ITEMS);
    let mut truncated = omitted_items != 0;
    let items: Vec<_> = items
        .into_iter()
        .take(crate::tools::MAX_PLAN_ITEMS)
        .map(|mut item| {
            truncated |= item.content.chars().count() > crate::tools::MAX_PLAN_TEXT
                || item.status.chars().count() > 32;
            item.content = item
                .content
                .chars()
                .take(crate::tools::MAX_PLAN_TEXT)
                .collect();
            item.status = item.status.chars().take(32).collect();
            item
        })
        .collect();
    // Legacy/imported metadata is data too; a malformed historic value cannot
    // turn a small checklist into an unbounded system-message projection.
    if let Some(state) = &mut state {
        for value in [
            &mut state.run_id,
            &mut state.updated_at,
            &mut state.run_status,
        ] {
            if let Some(value) = value {
                truncated |= value.chars().count() > 128;
                *value = value.chars().take(128).collect();
            }
        }
        truncated |= state.origin.chars().count() > 32;
        state.origin = state.origin.chars().take(32).collect();
    }
    let data =
        json!({"items":items,"state":state,"omitted_items":omitted_items,"truncated":truncated});
    let frame = crate::provenance::frame("session_plan", &[], &data.to_string());
    let first = context
        .first_mut()
        .context("Session context has no leading system message")?;
    if first["role"] != "system" {
        anyhow::bail!("Session context must have one leading system message");
    }
    let base = first["content"]
        .as_str()
        .context("Session system message must contain text")?;
    first["content"] = json!(format!("{base}\n\nRecorded Session checklist snapshot (data; resume only when relevant to the current user task):\n{frame}"));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{build_context, ContextCache};
    use formless_protocol::PlanItem;
    fn items(text: &str) -> Vec<PlanItem> {
        vec![PlanItem {
            content: text.into(),
            status: "in_progress".into(),
        }]
    }

    #[test]
    fn cache_reprojects_authoritative_plan_without_mutating_base_or_prompt() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        store
            .set_session_system_prompt(&session, "private static prompt")
            .unwrap();
        store.append_message(&session, "user", "task").unwrap();
        let run = store.create_run(&session).unwrap();
        store
            .set_run_plan(&session, &run, &items("first step"))
            .unwrap();
        let mut cache = ContextCache::default();
        let first = cache
            .build(&store, &session, "private static prompt", 100_000, 0.8)
            .unwrap()
            .0;
        assert_eq!(
            first
                .iter()
                .filter(|message| message["role"] == "system")
                .count(),
            1
        );
        assert!(first[0]["content"].as_str().unwrap().contains("first step"));
        assert!(first[0]["content"]
            .as_str()
            .unwrap()
            .contains(crate::provenance::DATA_NOTE));
        assert!(!cache.ctx[0]["content"]
            .as_str()
            .unwrap()
            .contains("first step"));
        let repeated = cache
            .build(&store, &session, "private static prompt", 100_000, 0.8)
            .unwrap()
            .0;
        assert_eq!(
            first, repeated,
            "unchanged plans must produce identical request prefixes"
        );
        store
            .set_run_plan(&session, &run, &items("next step"))
            .unwrap();
        let next = cache
            .build(&store, &session, "private static prompt", 100_000, 0.8)
            .unwrap()
            .0;
        assert!(next[0]["content"].as_str().unwrap().contains("next step"));
        assert!(!next[0]["content"].as_str().unwrap().contains("first step"));
        assert_eq!(
            store.session_system_prompt(&session).unwrap().as_deref(),
            Some("private static prompt")
        );
        store.set_run_plan(&session, &run, &[]).unwrap();
        let clear = cache
            .build(&store, &session, "private static prompt", 100_000, 0.8)
            .unwrap()
            .0;
        assert!(!clear[0]["content"]
            .as_str()
            .unwrap()
            .contains("Recorded Session checklist"));
    }

    #[test]
    fn plan_survives_checkpoint_projection_and_counts_toward_context_budget() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        for i in 0..8 {
            store
                .append_message(
                    &session,
                    if i % 2 == 0 { "user" } else { "assistant" },
                    "small message",
                )
                .unwrap();
        }
        let run = store.create_run(&session).unwrap();
        let before = build_context(&store, &session, "system", 2000, 0.5).unwrap();
        assert!(!before.1);
        let plan: Vec<_> = (0..20)
            .map(|_| PlanItem {
                content: "x".repeat(200),
                status: "pending".into(),
            })
            .collect();
        store.set_run_plan(&session, &run, &plan).unwrap();
        let after = build_context(&store, &session, "system", 2000, 0.5).unwrap();
        assert!(
            after.1,
            "plan bytes must participate in the context/compaction budget"
        );
        store
            .save_checkpoint(&session, "Earlier task summary", 4)
            .unwrap();
        let checkpoint = build_context(&store, &session, "system", 100_000, 0.8)
            .unwrap()
            .0;
        assert!(checkpoint[0]["content"]
            .as_str()
            .unwrap()
            .contains("Earlier task summary"));
        assert!(checkpoint[0]["content"]
            .as_str()
            .unwrap()
            .contains("Recorded Session checklist"));
        assert_eq!(
            checkpoint
                .iter()
                .filter(|message| message["role"] == "system")
                .count(),
            1
        );
    }

    #[test]
    fn legacy_plan_projection_is_bounded_and_framed_as_data() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        // Historical SQLite payloads bypassed current validation. Projection is
        // bounded independently without rewriting or inventing their authority.
        let plan: Vec<_> = (0..40)
            .map(|_| PlanItem {
                content: "日".repeat(500),
                status: "pending".into(),
            })
            .collect();
        store
            .conn
            .execute(
                "UPDATE sessions SET plan=?2 WHERE id=?1",
                rusqlite::params![session, serde_json::to_string(&plan).unwrap()],
            )
            .unwrap();
        let context = build_context(&store, &session, "system", 100_000, 0.8)
            .unwrap()
            .0;
        let text = context[0]["content"].as_str().unwrap();
        assert!(text.len() < 32 * 1024);
        let frame: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        let data: Value = serde_json::from_str(frame["content"].as_str().unwrap()).unwrap();
        assert_eq!(data["omitted_items"], 20);
        assert_eq!(data["state"]["origin"], "legacy");
        assert_eq!(
            store.session_plan(&session).unwrap().len(),
            40,
            "projection does not silently migrate legacy data"
        );
    }
}
