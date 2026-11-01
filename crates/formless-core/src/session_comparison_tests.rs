use super::*;
use crate::provider::Usage;

#[allow(clippy::too_many_arguments)]
fn usage(
    st: &Store,
    session: &str,
    run: &str,
    provider: &str,
    model: &str,
    source: &str,
    prompt: u64,
    cost: Option<f64>,
) {
    st.record_forecast_request(run, source).unwrap();
    st.record_usage_full(
        session,
        run,
        provider,
        model,
        &Usage {
            prompt_tokens: prompt,
            completion_tokens: 1,
            cost,
            reported: 3,
            ..Default::default()
        },
        source,
    )
    .unwrap();
}
fn paid_turn(st: &Store, session: &str, content: &str, cost: f64) -> String {
    st.append_message(session, "user", "new task").unwrap();
    let run = st.create_run(session).unwrap();
    usage(st, session, &run, "default", "model", "run", 10, Some(cost));
    st.append_message(session, "assistant", content).unwrap();
    st.set_run_status(&run, "completed").unwrap();
    run
}

#[test]
fn related_comparison_diff_and_cost_exclude_shared_runs_and_copied_history() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(Some("Parent".into())).unwrap();
    paid_turn(&st, &parent.id, "shared response", 7.0);
    let child = st.fork_session(&parent.id, None, None).unwrap().unwrap();
    paid_turn(&st, &parent.id, "left response", 0.2);
    paid_turn(&st, &child.id, "right response", 0.3);
    let result = st
        .compare_sessions(&parent.id, &child.id, &crate::Config::default())
        .unwrap();
    assert_eq!(result.common_ancestor.as_deref(), Some(parent.id.as_str()));
    assert_eq!(result.fork_point_seq, Some(2));
    assert_eq!(result.diff_from_seq, 2);
    assert!(!result.prefix_changed);
    assert!(result.messages.iter().all(|row| row
        .left
        .as_ref()
        .or(row.right.as_ref())
        .unwrap()
        .seq
        > 2));
    assert!(result.messages.iter().any(
        |row| row.kind == "left_only" && row.left.as_ref().unwrap().content == "left response"
    ));
    assert!(result
        .messages
        .iter()
        .any(|row| row.kind == "right_only"
            && row.right.as_ref().unwrap().content == "right response"));
    assert_eq!(result.left.usage.requests, 1);
    assert_eq!(result.left.usage.cost, Some(0.2));
    assert_eq!(result.right.usage.requests, 1);
    assert_eq!(result.right.usage.cost, Some(0.3));
    assert!(result.left.usage.usage_complete && result.right.usage.usage_complete);
    assert_eq!(
        st.list_sessions().unwrap().len(),
        2,
        "comparison cannot create paid model-comparison forks"
    );
}

#[test]
fn deleted_parent_and_nested_siblings_keep_recorded_lineage_without_copying_costs() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    paid_turn(&st, &parent.id, "original", 8.0);
    let a = st.fork_session(&parent.id, None, Some(1)).unwrap().unwrap();
    let b = st.fork_session(&parent.id, None, None).unwrap().unwrap();
    st.append_message(&a.id, "assistant", "branch a").unwrap();
    let nested = st.fork_session(&a.id, None, None).unwrap().unwrap();
    st.delete_session(&parent.id).unwrap();
    let result = st
        .compare_sessions(&nested.id, &b.id, &crate::Config::default())
        .unwrap();
    assert_eq!(result.common_ancestor.as_deref(), Some(parent.id.as_str()));
    assert_eq!(result.fork_point_seq, Some(1));
    assert!(result
        .warnings
        .iter()
        .any(|warning| warning.contains("deleted")));
    assert_eq!(result.left.usage.cost, Some(0.0));
    assert_eq!(result.right.usage.cost, Some(0.0));
    assert_eq!(
        result.right.usage.requests, 0,
        "copied paid response is not a second request"
    );
}

#[test]
fn edited_prefix_and_unknown_roots_compare_full_history_with_explicit_warnings() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    let original = st
        .append_message(&parent.id, "user", "original prompt")
        .unwrap();
    let child = st.fork_session(&parent.id, None, None).unwrap().unwrap();
    st.edit_message(&parent.id, &original.id, "changed prompt")
        .unwrap();
    let result = st
        .compare_sessions(&parent.id, &child.id, &crate::Config::default())
        .unwrap();
    assert!(result.prefix_changed);
    assert_eq!(result.diff_from_seq, 0);
    assert!(result.messages.iter().any(|row| row
        .left
        .as_ref()
        .is_some_and(|m| m.content == "changed prompt")));
    let imported = st
        .import_session(
            "Imported",
            None,
            None,
            &[("user".into(), "unknown past".into(), None)],
            &[],
        )
        .unwrap();
    let unknown = st
        .compare_sessions(&imported.id, &child.id, &crate::Config::default())
        .unwrap();
    assert_eq!(unknown.common_ancestor, None);
    assert_eq!(unknown.fork_point_seq, None);
    assert!(unknown.left.usage.cost.is_none());
    assert!(!unknown.left.usage.usage_complete);
    assert!(unknown
        .warnings
        .iter()
        .any(|warning| warning.contains("No recorded common ancestor")));
}

#[test]
fn straddling_legacy_and_missing_request_coverage_never_become_exact_zero_bills() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    st.append_message(&parent.id, "user", "prompt copied while Run is in flight")
        .unwrap();
    let run = st.create_run(&parent.id).unwrap();
    usage(
        &st,
        &parent.id,
        &run,
        "default",
        "model",
        "run",
        40,
        Some(0.4),
    );
    let child = st.fork_session(&parent.id, None, None).unwrap().unwrap();
    st.append_message(&parent.id, "assistant", "response after fork")
        .unwrap();
    st.set_run_status(&run, "completed").unwrap();
    let result = st
        .compare_sessions(&parent.id, &child.id, &crate::Config::default())
        .unwrap();
    assert_eq!(result.left.usage.unallocated_requests, 1);
    assert_eq!(result.left.usage.unallocated_prompt_tokens, 40);
    assert_eq!(result.left.usage.unknown_runs, 1);
    assert_eq!(result.left.usage.cost, None);
    assert_eq!(result.left.usage.known_records_cost_range, Some([0.0, 0.4]));
    assert_eq!(result.right.usage.cost, Some(0.0));
    st.ex("UPDATE runs SET start_message_id=NULL,start_message_seq=NULL,end_message_id=NULL,end_message_seq=NULL WHERE id=?1",params![run]).unwrap();
    let legacy = st
        .compare_sessions(&parent.id, &child.id, &crate::Config::default())
        .unwrap();
    assert_eq!(legacy.left.usage.cost, None);
    assert_eq!(legacy.left.usage.unallocated_requests, 1);
    let extra = paid_turn(&st, &child.id, "known work", 0.1);
    st.record_forecast_request(&extra, "run").unwrap(); // observed usage is missing one physical response
    let incomplete = st
        .compare_sessions(&parent.id, &child.id, &crate::Config::default())
        .unwrap();
    assert_eq!(incomplete.right.usage.recorded_prompt_tokens, 10);
    assert_eq!(incomplete.right.usage.priced_cost_subtotal, 0.1);
    assert_eq!(incomplete.right.usage.cost, None);
    assert!(!incomplete.right.usage.usage_complete);
    assert!(incomplete
        .right
        .usage
        .warnings
        .iter()
        .any(|warning| warning.contains("not an upper bound")));
}

#[test]
fn physical_rows_keep_reported_cost_precedence_named_provider_and_nested_model_prices() {
    let st = Store::open_in_memory().unwrap();
    let a = st.create_session(None).unwrap();
    let b = st.create_session(None).unwrap();
    let mut cfg = crate::Config::default();
    let price = |input| crate::config::ModelPrice {
        input,
        output: 0.0,
        cached_input: None,
        cache_write_input: None,
        retrieved_at: None,
    };
    cfg.provider.prices.insert("model".into(), price(1.0));
    let mut served = cfg.provider.clone();
    served.prices.clear();
    served.prices.insert("fallback".into(), price(3.0));
    cfg.providers.insert("served".into(), served);
    st.append_message(&a.id, "user", "accounting fixture")
        .unwrap();
    let run = st.create_run(&a.id).unwrap();
    usage(
        &st,
        &a.id,
        &run,
        "default",
        "model",
        "run",
        1_000_000,
        Some(0.7),
    );
    usage(&st, &a.id, &run, "default", "model", "run", 1_000_000, None);
    usage(
        &st, &a.id, &run, "served", "fallback", "llm", 1_000_000, None,
    );
    st.append_message(&a.id, "assistant", "done").unwrap();
    st.set_run_status(&run, "completed").unwrap();
    let result = st.compare_sessions(&a.id, &b.id, &cfg).unwrap();
    assert!(
        (result.left.usage.cost.unwrap() - 4.7).abs() < 1e-9,
        "reported0.7 + currentdefault1 + actualservedfallback3"
    );
    assert_eq!(result.left.usage.requests, 3);
    assert_eq!(result.left.usage.estimated_requests, 2);
    assert_eq!(result.left.usage.recorded_prompt_tokens, 3_000_000);
    usage(
        &st,
        &a.id,
        &run,
        "removed-provider",
        "model",
        "agent",
        1,
        None,
    );
    let unpriced = st.compare_sessions(&a.id, &b.id, &cfg).unwrap();
    assert_eq!(
        unpriced.left.usage.cost, None,
        "removed provider cannot silently borrow the default price"
    );
    assert!(unpriced
        .left
        .usage
        .unpriced
        .contains(&"removed-provider/model".into()));
}

#[test]
fn comparison_rejects_hidden_sessions_and_cycles_without_mutating_history() {
    let st = Store::open_in_memory().unwrap();
    let a = st.create_session(None).unwrap();
    let b = st.create_session(None).unwrap();
    st.soft_delete_session(&b.id).unwrap();
    assert!(st
        .compare_sessions(&a.id, &b.id, &crate::Config::default())
        .is_err());
    st.restore_session(&b.id).unwrap();
    st.ex(
        "UPDATE sessions SET forked_from=id,fork_point_seq=0 WHERE id=?1",
        params![a.id],
    )
    .unwrap();
    assert!(st
        .compare_sessions(&a.id, &b.id, &crate::Config::default())
        .unwrap_err()
        .to_string()
        .contains("cyclic"));
    assert!(st.list_messages(&a.id).unwrap().is_empty());
}

#[test]
fn large_message_histories_fall_back_without_omission_or_invented_unchanged_rows() {
    let st = Store::open_in_memory().unwrap();
    let a = st.create_session(None).unwrap();
    let b = st.create_session(None).unwrap();
    st.transaction(|st| {
        for _ in 0..1001 {
            st.append_message(&a.id, "assistant", "identical repeated text")?;
            st.append_message(&b.id, "assistant", "identical repeated text")?;
        }
        Ok(())
    })
    .unwrap();
    let result = st
        .compare_sessions_for_rpc(&a.id, &b.id, &crate::Config::default())
        .unwrap();
    assert_eq!(result.messages.len(), 2002);
    assert_eq!(
        result
            .messages
            .iter()
            .filter(|row| row.kind == "left_only")
            .count(),
        1001
    );
    assert_eq!(
        result
            .messages
            .iter()
            .filter(|row| row.kind == "right_only")
            .count(),
        1001
    );
    assert!(result.messages.iter().all(|row| row.kind != "unchanged"));
    assert_eq!(
        result.messages.last().unwrap().right.as_ref().unwrap().seq,
        1001
    );
    assert!(result
        .warnings
        .iter()
        .any(|warning| warning.contains("without quadratic")
            && warning.contains("no messages are omitted")));
}

#[test]
fn repeated_large_bodies_align_exactly_but_rpc_budget_fails_with_a_typed_error() {
    let st = Store::open_in_memory().unwrap();
    let a = st.create_session(None).unwrap();
    let b = st.create_session(None).unwrap();
    let body = "large repeated payload 界\n".repeat(3000);
    for _ in 0..32 {
        st.append_message(&a.id, "assistant", &body).unwrap();
        st.append_message(&b.id, "assistant", &body).unwrap();
    }
    let result = st
        .compare_sessions(&a.id, &b.id, &crate::Config::default())
        .unwrap();
    assert_eq!(result.messages.len(), 32);
    assert!(result.messages.iter().all(|row| row.kind == "unchanged"
        && row.left.as_ref().unwrap().content == body
        && row.right.as_ref().unwrap().content == body));
    let error = st
        .compare_sessions_for_rpc(&a.id, &b.id, &crate::Config::default())
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionComparisonFailure>(),
        Some(SessionComparisonFailure::ReplyTooLarge { .. })
    ));
    assert_eq!(st.list_messages(&a.id).unwrap().len(), 32);
}

#[test]
fn final_rpc_budget_also_covers_metadata_and_missing_sessions_stay_typed() {
    let st = Store::open_in_memory().unwrap();
    let a = st.create_session(Some("A".repeat(2000))).unwrap();
    let b = st.create_session(None).unwrap();
    let error = st
        .compare_sessions_with_budget(&a.id, &b.id, &crate::Config::default(), Some(1024))
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionComparisonFailure>(),
        Some(SessionComparisonFailure::ReplyTooLarge { limit: 1024 })
    ));
    let error = st
        .compare_sessions_for_rpc("not-found", &b.id, &crate::Config::default())
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionComparisonFailure>(),
        Some(SessionComparisonFailure::NotFound)
    ));
}
