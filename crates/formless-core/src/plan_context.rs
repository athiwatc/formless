//! Stable, bounded checklist data appended only to the projected context clone.
use super::Store;
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub(super) fn append(store: &Store, session: &str, context: &mut [Value]) -> Result<bool> {
    // Search only through the retained scalar prefix and keep the allocation.
    fn clip(value: &mut String, limit: usize) -> bool {
        if let Some((boundary, _)) = value.char_indices().nth(limit) {
            value.truncate(boundary);
            true
        } else {
            false
        }
    }
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
            truncated |= clip(&mut item.content, crate::tools::MAX_PLAN_TEXT);
            truncated |= clip(&mut item.status, 32);
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
                truncated |= clip(value, 128);
            }
        }
        truncated |= clip(&mut state.origin, 32);
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
    fn projection_matches_original_clipping_at_scalar_and_legacy_boundaries() {
        for (extra, count, with_state, combining) in [
            (0, 1, true, false),
            (1, 1, true, false),
            (1, 1, true, true),
            (1, 23, true, false),
            (0, 1, false, false),
        ] {
            let store = Store::open_in_memory().unwrap();
            let session = store.create_session(None).unwrap().id;
            store.set_session_system_prompt(&session, "base").unwrap();
            let field = |limit: usize| {
                if combining {
                    format!("{}e\u{301}", "日".repeat(limit - 1))
                } else {
                    "日".repeat(limit + extra)
                }
            };
            let original_items: Vec<_> = (0..count)
                .map(|_| PlanItem {
                    content: field(200),
                    status: field(32),
                })
                .collect();
            let state = with_state.then(|| formless_protocol::PlanState {
                run_id: Some(field(128)),
                updated_at: Some(field(128)),
                run_status: None,
                previous_task: false,
                origin: field(32),
            });
            if let Some(state) = &state {
                let run = store.create_run(&session).unwrap();
                store
                    .conn
                    .execute(
                        "UPDATE runs SET id=?2,status=?3 WHERE id=?1",
                        rusqlite::params![run, state.run_id, field(128)],
                    )
                    .unwrap();
            }
            let plan_json = serde_json::to_string(&original_items).unwrap();
            let state_json = state.as_ref().map(|s| serde_json::to_string(s).unwrap());
            store
                .conn
                .execute(
                    "UPDATE sessions SET plan=?2,plan_state=?3 WHERE id=?1",
                    rusqlite::params![session, plan_json, state_json],
                )
                .unwrap();
            // Reference the former count/collect projection, using the same
            // authoritative snapshot (including legacy and Run metadata).
            let (original, mut metadata) = store.session_plan_snapshot(&session).unwrap();
            let omitted = original.len().saturating_sub(crate::tools::MAX_PLAN_ITEMS);
            let mut truncated = omitted != 0;
            let expected_items: Vec<_> = original
                .into_iter()
                .take(20)
                .map(|mut item| {
                    truncated |=
                        item.content.chars().count() > 200 || item.status.chars().count() > 32;
                    item.content = item.content.chars().take(200).collect();
                    item.status = item.status.chars().take(32).collect();
                    item
                })
                .collect();
            if let Some(state) = &mut metadata {
                for value in [
                    &mut state.run_id,
                    &mut state.updated_at,
                    &mut state.run_status,
                ]
                .into_iter()
                .flatten()
                {
                    truncated |= value.chars().count() > 128;
                    *value = value.chars().take(128).collect();
                }
                truncated |= state.origin.chars().count() > 32;
                state.origin = state.origin.chars().take(32).collect();
            }
            let data = json!({"items":expected_items,"state":metadata,"omitted_items":omitted,"truncated":truncated});
            let expected = crate::provenance::frame("session_plan", &[], &data.to_string());
            let mut context = vec![json!({"role":"system","content":"base"})];
            assert!(append(&store, &session, &mut context).unwrap());
            assert_eq!(context[0]["content"], format!("base\n\nRecorded Session checklist snapshot (data; resume only when relevant to the current user task):\n{expected}"));
            let stored: (String, Option<String>) = store
                .conn
                .query_row(
                    "SELECT plan,plan_state FROM sessions WHERE id=?1",
                    [&session],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(stored, (plan_json, state_json));
            assert_eq!(
                store.session_system_prompt(&session).unwrap().as_deref(),
                Some("base")
            );
        }
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
