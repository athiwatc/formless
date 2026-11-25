//! Offline golden scenarios for multi-step orchestrator behavior.
//!
//! Run with `cargo test -p formless-core --test agent_scenarios --locked -- --nocapture`
//! to collect one JSON summary line per scenario.
//! `steps` counts completed or truncated model replies; 429s and dropped streams
//! are excluded from all counters. `tool_calls` includes malformed calls. Token
//! values estimate normalized prompt-body/output-delta JSON bytes divided by 4,
//! rounded up; machine paths, timestamps and IDs are normalized first. These
//! estimates are not tokenizer measurements.
mod common;

use common::Scenario;
use formless_protocol::{Request, SendMode};
use serde_json::{json, Value};

fn assert_summary(scenario: &str, actual: Value, expected: Value) {
    let actual = json!({"scenario":scenario,"summary":actual});
    let expected = json!({"scenario":scenario,"summary":expected});
    println!("offline-agent-scenario={actual}");
    assert_eq!(
        actual,
        expected,
        "offline scenario summary changed\nexpected JSON:\n{}\nactual JSON:\n{}",
        serde_json::to_string_pretty(&expected).unwrap(),
        serde_json::to_string_pretty(&actual).unwrap()
    );
}

fn find_tools_call(body: &Value, query: &str) -> bool {
    body["tools"].as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool["function"]["name"] == "find_tools")
    }) && body["messages"].to_string().contains(query)
}

fn assert_tool_feedback(body: &Value, case: &str, tool_call_id: &str, expected: &[&str]) {
    let message = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .expect("invalid arguments are returned as tool-role feedback");
    assert_eq!(
        message["tool_call_id"].as_str(),
        Some(tool_call_id),
        "{case} feedback referenced the wrong tool call: {}",
        serde_json::to_string_pretty(message).unwrap()
    );
    let contains_text = |expected: &str| {
        let mut pending = message["content"]
            .as_str()
            .map(str::to_owned)
            .into_iter()
            .collect::<Vec<_>>();
        while let Some(content) = pending.pop() {
            if content.contains(expected) {
                return true;
            }
            if let Ok(value) = serde_json::from_str::<Value>(&content) {
                for key in ["content", "error", "message", "note"] {
                    if let Some(inner) = value[key].as_str() {
                        pending.push(inner.to_owned());
                    }
                }
            }
        }
        false
    };
    assert!(
        expected.iter().all(|expected| contains_text(expected)),
        "{case} error was not fed back in the last tool message: {}",
        serde_json::to_string_pretty(message).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn creates_a_tool_and_reuses_it_in_a_fresh_session() {
    let mut scenario = Scenario::new(0).await;
    let first = scenario.session.clone();
    let first_run = scenario
        .send(
            &first,
            "Create a greet Tool and make it reusable in another Session.",
            SendMode::Send,
        )
        .await;
    scenario
        .next_call()
        .await
        .tool_call(
            "create_tool",
            json!({
                "name":"greet",
                "description":"Greet one person by name",
                "input_schema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]},
                "output_schema":{"type":"string"},
                "implementation_js":"function run(input) { return 'Hello, ' + input.name + '!'; }",
                "capabilities":[]
            }),
        );
    scenario.next_call().await.text("The greet Tool is ready.");
    scenario.finish(&first, &first_run).await;

    let created = scenario.store.get_tool_by_name("greet").unwrap().unwrap().0;
    let first_transcript =
        serde_json::to_string(&scenario.store.list_messages(&first).unwrap()).unwrap();
    assert!(first_transcript.contains("The greet Tool is ready."));

    let fresh = scenario.new_session();
    let fresh_run = scenario
        .send(
            &fresh,
            "Use the existing greet Tool for Ada.",
            SendMode::Send,
        )
        .await;
    let lookup = scenario.next_call().await;
    assert!(find_tools_call(&lookup.body, "greet"));
    assert!(
        lookup.body["tools"]
            .as_array()
            .is_some_and(|tools| { tools.iter().any(|tool| tool["function"]["name"] == "greet") }),
        "the initial small-registry listing already advertises the stored Tool"
    );
    lookup.tool_call("find_tools", json!({"query":"greet"}));

    let use_tool = scenario.next_call().await;
    let find_tools_result_envelope = use_tool.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| serde_json::from_str::<Value>(message["content"].as_str()?).ok())
        .find(|result| result["source_tool"] == "find_tools")
        .expect("the next prompt includes the find_tools result");
    let find_tools_result: Value =
        serde_json::from_str(find_tools_result_envelope["content"].as_str().unwrap()).unwrap();
    let returned_greet = find_tools_result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "greet")
        .expect("find_tools returns the persisted greet Tool");
    assert!(
        returned_greet["description"]
            .as_str()
            .is_some_and(|description| description == created.description.as_str()),
        "find_tools returns the stored Tool description"
    );
    assert!(
        scenario
            .store
            .session_tool_ids(&fresh)
            .unwrap()
            .contains(&created.id),
        "find_tools loads the persisted Tool identity into the fresh Session"
    );
    use_tool.tool_call("greet", json!({"name":"Ada"}));
    let finish = scenario.next_call().await;
    assert!(finish.body["messages"].to_string().contains("Hello, Ada!"));
    finish.text("Hello, Ada!");
    scenario.finish(&fresh, &fresh_run).await;

    let messages = scenario.store.list_messages(&fresh).unwrap();
    let transcript = serde_json::to_string(&messages).unwrap();
    assert!(transcript.contains("Hello, Ada!"));
    let reused = scenario.store.get_tool_by_name("greet").unwrap().unwrap().0;
    assert_eq!(
        reused.id, created.id,
        "the fresh Session reused the same Tool"
    );
    assert_summary(
        "tool-create-fresh-session-reuse",
        scenario.provider.summary(),
        json!({
            "steps":5,
            "tool_calls":3,
            "tokens":{"prompt":32927,"completion":178}
        }),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_rate_limit_and_dropped_stream_then_repairs_malformed_arguments() {
    let mut scenario = Scenario::new(3).await;
    let session = scenario.session.clone();
    let mut events = scenario.watch_events(&session).await;
    let run_id = scenario
        .send(
            &session,
            "Search for a matching Tool, then answer.",
            SendMode::Send,
        )
        .await;

    let first_attempt = scenario.next_call().await;
    first_attempt.too_many_requests();
    let dropped_attempt = scenario.next_call().await;
    let initial_messages = dropped_attempt.body["messages"].clone();
    let release_drop = dropped_attempt.drop_after(json!({"content":"uncommitted partial answer"}));
    events
        .wait_for_message_delta(&session, &run_id, "uncommitted partial answer")
        .await;
    release_drop
        .send(())
        .expect("scripted stream is waiting for the emitted partial delta");
    let malformed = scenario.next_call().await;
    assert_eq!(malformed.body["messages"], initial_messages);
    let malformed_tool_call_id = malformed.tool_call_id();
    malformed.malformed_tool_call("find_tools", "{");

    let correction = scenario.next_call().await;
    assert_tool_feedback(
        &correction.body,
        "malformed argument",
        &malformed_tool_call_id,
        &[
            "invalid JSON arguments for find_tools",
            "Retry with valid JSON.",
        ],
    );
    let truncated_tool_call_id = correction.tool_call_id();
    correction.truncated_tool_call("find_tools", "{\"query\":");
    let repair = scenario.next_call().await;
    assert_tool_feedback(
        &repair.body,
        "truncated argument",
        &truncated_tool_call_id,
        &[
            "find_tools call was cut off at the output token limit",
            "arguments are incomplete and it was not run",
        ],
    );
    repair.tool_call(
        "find_tools",
        json!({"query":"no matching offline scenario Tool"}),
    );
    let final_step = scenario.next_call().await;
    let actual_tool_envelope = final_step.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| serde_json::from_str::<Value>(message["content"].as_str()?).ok())
        .find(|result| {
            result["source_tool"] == "find_tools"
                && result["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("No matching Tool; create one."))
        });
    let actual_tool_envelope =
        actual_tool_envelope.expect("the repaired find_tools result should be in the next prompt");
    let actual_tool_result: Value =
        serde_json::from_str(actual_tool_envelope["content"].as_str().unwrap()).unwrap();
    assert_eq!(actual_tool_result["tools"], json!([]));
    assert_eq!(actual_tool_result["note"], "No matching Tool; create one.");
    final_step.text("Recovered and found no matching Tool.");
    scenario.finish(&session, &run_id).await;
    events.shutdown().await;

    let messages = scenario.store.list_messages(&session).unwrap();
    let transcript = serde_json::to_string(&messages).unwrap();
    assert!(transcript.contains("Recovered and found no matching Tool."));
    assert!(
        !transcript.contains("uncommitted partial answer"),
        "the dropped stream must not leak into the durable transcript"
    );
    assert_summary(
        "rate-limit-drop-argument-recovery",
        scenario.provider.summary(),
        json!({
            "steps":4,
            "tool_calls":3,
            "tokens":{"prompt":26224,"completion":100}
        }),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn applies_a_steer_to_a_later_step_of_the_active_run() {
    let mut scenario = Scenario::new(0).await;
    let session = scenario.session.clone();
    let run_id = scenario
        .send(
            &session,
            "Find a useful Tool and report back.",
            SendMode::Send,
        )
        .await;
    scenario
        .next_call()
        .await
        .tool_call("find_tools", json!({"query":"steering scenario"}));

    let second_step = scenario.next_call().await;
    let steer = "For the rest of this Run, stay concise and do not create a Tool.";
    scenario.send(&session, steer, SendMode::Steer).await;
    second_step.tool_call("find_tools", json!({"query":"concise response"}));

    let later_step = scenario.next_call().await;
    assert!(later_step.body["messages"].to_string().contains(steer));
    later_step.text("I followed the updated direction.");
    scenario.finish(&session, &run_id).await;

    let transcript =
        serde_json::to_string(&scenario.store.list_messages(&session).unwrap()).unwrap();
    assert!(transcript.contains(steer));
    assert!(transcript.contains("I followed the updated direction."));
    assert_summary(
        "mid-run-steering",
        scenario.provider.summary(),
        json!({
            "steps":3,
            "tool_calls":2,
            "tokens":{"prompt":19537,"completion":76}
        }),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn compacts_a_session_and_continues_with_the_checkpoint() {
    let mut scenario = Scenario::new(0).await;
    let session = scenario.session.clone();
    scenario
        .store
        .append_message(
            &session,
            "user",
            "We chose Postgres for the project database.",
        )
        .unwrap();
    scenario
        .store
        .append_message(&session, "assistant", "Postgres is the choice.")
        .unwrap();

    let daemon = scenario.daemon.clone();
    let compact_session = session.clone();
    let compact = tokio::spawn(async move {
        daemon
            .handle(Request::CompactSession {
                session_id: compact_session,
                focus: Some("the database decision".into()),
            })
            .await
    });
    let request = scenario.next_call().await;
    assert!(request.body.to_string().contains("the database decision"));
    request.text("The project database decision is Postgres.");
    assert_eq!(compact.await.unwrap().unwrap()["started"], true);

    let (summary, _) = scenario.wait_for_checkpoint(&session).await;
    assert!(summary.contains("Postgres"));
    let run_id = scenario
        .send(
            &session,
            "Does the database decision change our migration plan?",
            SendMode::Send,
        )
        .await;
    let follow_up = scenario.next_call().await;
    assert!(follow_up
        .body
        .to_string()
        .contains("The project database decision is Postgres."));
    follow_up.text("No, the migration plan remains unchanged.");
    scenario.finish(&session, &run_id).await;

    let transcript =
        serde_json::to_string(&scenario.store.list_messages(&session).unwrap()).unwrap();
    assert!(transcript.contains("No, the migration plan remains unchanged."));
    assert!(scenario
        .store
        .latest_checkpoint(&session)
        .unwrap()
        .is_some());
    assert_summary(
        "compaction-continuation",
        scenario.provider.summary(),
        json!({
            "steps":2,
            "tool_calls":0,
            "tokens":{"prompt":6825,"completion":28}
        }),
    );
}
