//! Exhaustive small causal schedules for the actual barrier staging/admission API.
//! The oracle is eligibility at the producer cutoff, independent of receipt time.
use super::*;
use formless_protocol::{TranscriptAttemptClose, TranscriptAttemptEvent};
use serde_json::json;
use std::collections::BTreeSet;

const EPOCH: &str = "791c9df1-3cd3-4a9a-9833-077048f95327";
const A: &str = "ไทย 😀 ";
const B: &str = "漢字\n";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
enum Action {
    Read,
    Start,
    EmitA,
    EmitB,
    Sample,
    Reply,
    ReceiveStart,
    ReceiveA,
    ReceiveB,
}
const ACTIONS: [Action; 9] = [
    Action::Read,
    Action::Start,
    Action::EmitA,
    Action::EmitB,
    Action::Sample,
    Action::Reply,
    Action::ReceiveStart,
    Action::ReceiveA,
    Action::ReceiveB,
];
fn bit(action: Action) -> u16 {
    1 << action as usize
}
fn predecessors(action: Action) -> u16 {
    match action {
        Action::Read | Action::Start => 0,
        Action::EmitA | Action::ReceiveStart => bit(Action::Start),
        Action::EmitB => bit(Action::EmitA),
        Action::Sample => bit(Action::Read),
        Action::Reply => bit(Action::Sample),
        Action::ReceiveA => bit(Action::EmitA) | bit(Action::ReceiveStart),
        Action::ReceiveB => bit(Action::EmitB) | bit(Action::ReceiveA),
    }
}
fn schedules(done: u16, order: &mut Vec<Action>, visit: &mut impl FnMut(&[Action])) {
    if order.len() == ACTIONS.len() {
        visit(order);
        return;
    }
    for action in ACTIONS {
        if done & bit(action) == 0 && done & predecessors(action) == predecessors(action) {
            order.push(action);
            schedules(done | bit(action), order, visit);
            order.pop();
        }
    }
}
fn frame(cursor: u64, change: Phase) -> Event {
    Event::TranscriptAttempt {
        update: TranscriptAttemptEvent {
            epoch: EPOCH.into(),
            cursor,
            session_id: "s".into(),
            run_id: "r".into(),
            turn_id: "turn".into(),
            attempt_id: "attempt".into(),
            change,
        },
    }
}
fn snapshot() -> serde_json::Value {
    json!({"session":{"id":"s"},"messages":[{"id":"m","session_id":"s","seq":4,"role":"user","content":"Question","created_at":"now"}],"running":true})
}
fn consume(gate: &mut Gate, tag: &Attachment, event: &Event, appended: &mut String) {
    match gate.event(tag, event) {
        Change::Append(text) => appended.push_str(text),
        Change::Ignore | Change::Started => {}
        other => panic!("valid causal schedule unexpectedly changed admission: {other:?}"),
    }
}
struct Result {
    text: String,
    cutoff: u64,
    staged: usize,
}
fn run(order: &[Action], retire_at: Option<usize>) -> Result {
    let mut tag = Attachment::default();
    let mut pending = Some(Pending::new(tag.clone(), "s").unwrap());
    let mut gate = None;
    let mut emitted: [Option<Event>; 3] = [None, None, None];
    let mut clock = 0;
    let mut cutoff = None;
    let mut read = None;
    let mut text = String::new();
    let mut staged = 0;
    for (index, action) in order.iter().enumerate() {
        if retire_at == Some(index) {
            tag = Attachment::default();
            // The view discards its provisional content on owner retirement.
            text.clear();
        }
        match action {
            Action::Read => read = Some(snapshot()),
            Action::Start | Action::EmitA | Action::EmitB => {
                clock += 1;
                let (at, change) = match action {
                    Action::Start => (0, Phase::Started { base_seq: 4 }),
                    Action::EmitA => (
                        1,
                        Phase::Delta {
                            offset: 0,
                            text: A.into(),
                        },
                    ),
                    Action::EmitB => (
                        2,
                        Phase::Delta {
                            offset: A.len() as u64,
                            text: B.into(),
                        },
                    ),
                    _ => unreachable!(),
                };
                emitted[at] = Some(frame(clock, change));
            }
            Action::Sample => cutoff = Some(clock),
            Action::Reply => {
                let reply = json!({"snapshot":read.as_ref().unwrap(),"barrier":{"epoch":EPOCH,"cursor":cutoff.unwrap()},"mode":"future_attempts_only_v1"});
                let loaded = pending.take().unwrap().finish(&tag, &reply);
                if retire_at.is_some_and(|cut| cut <= index) {
                    assert!(
                        loaded.is_err(),
                        "retired barrier installed: {order:?} cut={retire_at:?}"
                    );
                } else {
                    let mut loaded = loaded.unwrap();
                    assert_eq!(loaded.snapshot.last_seq, Some(4));
                    for event in &loaded.buffered {
                        consume(&mut loaded.gate, &tag, event, &mut text);
                    }
                    gate = Some(loaded.gate);
                }
            }
            Action::ReceiveStart | Action::ReceiveA | Action::ReceiveB => {
                let at = match action {
                    Action::ReceiveStart => 0,
                    Action::ReceiveA => 1,
                    _ => 2,
                };
                let event = emitted[at].as_ref().unwrap();
                if let Some(gate) = &mut gate {
                    consume(gate, &tag, event, &mut text);
                } else if let Some(pending) = &mut pending {
                    staged += 1;
                    pending.observe(&tag, event).unwrap();
                }
            }
        }
    }
    if retire_at == Some(order.len()) {
        text.clear();
    }
    Result {
        text,
        cutoff: cutoff.unwrap(),
        staged,
    }
}
#[test]
fn all_causal_cutoff_and_receipt_schedules_admit_exactly_the_eligible_future_prefix() {
    let mut count = 0;
    let mut coverage = BTreeSet::new();
    schedules(0, &mut Vec::new(), &mut |order| {
        count += 1;
        let result = run(order, None);
        let eligible = order.iter().position(|a| *a == Action::Start).unwrap()
            > order.iter().position(|a| *a == Action::Sample).unwrap();
        assert_eq!(
            result.text,
            if eligible {
                format!("{A}{B}")
            } else {
                String::new()
            },
            "schedule {order:?}"
        );
        assert_eq!(eligible, result.cutoff == 0);
        coverage.insert((result.cutoff, result.staged));
    });
    assert_eq!(
        coverage,
        (0..=3)
            .flat_map(|cutoff| (0..=3).map(move |staged| (cutoff, staged)))
            .collect()
    );
    assert!(count > 100);
    eprintln!(
        "barrier_causal_schedules {{\"schedules\":{count},\"cutoff_staging_cells\":{}}}",
        coverage.len()
    );
}
#[test]
fn every_owner_retirement_cut_stays_inert_across_all_causal_schedules() {
    let mut count = 0;
    schedules(0, &mut Vec::new(), &mut |order| {
        for cut in 0..=order.len() {
            let result = run(order, Some(cut));
            assert!(
                result.text.is_empty(),
                "retired text escaped: {order:?} cut={cut}"
            );
            count += 1;
        }
    });
    assert!(count > 1000);
    eprintln!("barrier_retirement_schedules {{\"retirement_cuts\":{count}}}");
}
#[test]
fn all_wire_permutations_expose_only_the_ordered_prefix_then_stay_retired() {
    let frames = [
        frame(11, Phase::Started { base_seq: 4 }),
        frame(
            12,
            Phase::Delta {
                offset: 0,
                text: A.into(),
            },
        ),
        frame(
            13,
            Phase::Delta {
                offset: A.len() as u64,
                text: B.into(),
            },
        ),
        frame(
            14,
            Phase::Closed {
                bytes: (A.len() + B.len()) as u64,
                reason: TranscriptAttemptClose::Returned,
            },
        ),
    ];
    let snapshot = crate::replies::session_snapshot("s", &snapshot()).unwrap();
    let mut count = 0;
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if order.iter().copied().collect::<BTreeSet<_>>().len() != 4 {
                        continue;
                    }
                    let tag = Attachment::default();
                    let mut gate = Gate::new(
                        tag.clone(),
                        &snapshot,
                        TranscriptBarrier {
                            epoch: EPOCH.into(),
                            cursor: 10,
                        },
                    )
                    .unwrap();
                    let mut text = String::new();
                    for index in order {
                        let retired = gate.retired();
                        let change = gate.event(&tag, &frames[index]);
                        if retired {
                            assert_eq!(change, Change::Ignore);
                        }
                        if let Change::Append(chunk) = change {
                            text.push_str(chunk);
                        }
                    }
                    let expected = if order[..2] == [0, 1] {
                        if order[2] == 2 {
                            format!("{A}{B}")
                        } else {
                            A.into()
                        }
                    } else {
                        String::new()
                    };
                    assert_eq!(text, expected, "wire order {order:?}");
                    assert!(gate.retired());
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 24);
    eprintln!("barrier_wire_permutations {{\"wire_orders\":{count}}}");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
enum CanonicalAction {
    Read,
    Commit,
    Publish,
    Start,
    Emit,
    Sample,
    Reply,
    ReceiveCanonical,
    ReceiveStart,
    ReceiveDelta,
}
const CANONICAL_ACTIONS: [CanonicalAction; 10] = [
    CanonicalAction::Read,
    CanonicalAction::Commit,
    CanonicalAction::Publish,
    CanonicalAction::Start,
    CanonicalAction::Emit,
    CanonicalAction::Sample,
    CanonicalAction::Reply,
    CanonicalAction::ReceiveCanonical,
    CanonicalAction::ReceiveStart,
    CanonicalAction::ReceiveDelta,
];
fn canonical_orders(
    done: u16,
    order: &mut Vec<CanonicalAction>,
    visit: &mut impl FnMut(&[CanonicalAction]),
) {
    use CanonicalAction as C;
    let bit = |action: C| 1u16 << action as usize;
    if order.len() == CANONICAL_ACTIONS.len() {
        visit(order);
        return;
    }
    for action in CANONICAL_ACTIONS {
        let required = match action {
            C::Read | C::Commit => 0,
            C::Publish | C::Start => bit(C::Commit),
            C::Emit | C::ReceiveStart => bit(C::Start),
            C::Sample => bit(C::Read),
            C::Reply => bit(C::Sample),
            C::ReceiveCanonical => bit(C::Publish),
            C::ReceiveDelta => bit(C::Emit) | bit(C::ReceiveStart),
        };
        if done & bit(action) == 0 && done & required == required {
            order.push(action);
            canonical_orders(done | bit(action), order, visit);
            order.pop();
        }
    }
}
#[test]
fn canonical_storage_and_publication_interleavings_never_attach_text_to_the_old_baseline() {
    use CanonicalAction as C;
    let mut count = 0;
    let mut appending = 0;
    let mut reloading = 0;
    let mut coverage = BTreeSet::new();
    canonical_orders(0, &mut Vec::new(), &mut |order| {
        // The real subscriber serializes events in publication order, while
        // storage, producer sampling and response receipt may interleave.
        let published = order
            .iter()
            .filter_map(|a| match a {
                C::Publish => Some(0),
                C::Start => Some(1),
                C::Emit => Some(2),
                _ => None,
            })
            .collect::<Vec<_>>();
        let received = order
            .iter()
            .filter_map(|a| match a {
                C::ReceiveCanonical => Some(0),
                C::ReceiveStart => Some(1),
                C::ReceiveDelta => Some(2),
                _ => None,
            })
            .collect::<Vec<_>>();
        if published != received {
            return;
        }
        count += 1;
        let eligible = order.iter().position(|a| *a == C::Start).unwrap()
            > order.iter().position(|a| *a == C::Sample).unwrap();
        let tag = Attachment::default();
        let mut pending = Some(Pending::new(tag.clone(), "s").unwrap());
        let mut gate = None;
        let mut committed = false;
        let mut sampled = None;
        let mut cutoff = 0;
        let mut clock = 0;
        let mut start = None;
        let mut delta = None;
        let mut visible = None;
        let mut did_append = false;
        let mut did_reload = false;
        let canonical = Event::MessageAppended {
            session_id: "s".into(),
            message: formless_protocol::MessageInfo {
                generated_by: None,
                id: "next".into(),
                session_id: "s".into(),
                seq: 5,
                role: "user".into(),
                content: "Followup".into(),
                created_at: "now".into(),
                meta: None,
            },
        };
        let mut consume = |gate: &mut Gate, event: &Event, visible: &mut Option<i64>| {
            let retired = gate.retired();
            let change = gate.event(&tag, event);
            if retired {
                assert_eq!(change, Change::Ignore);
            }
            match change {
                Change::Append(text) => {
                    assert!(eligible, "pre-cutoff attempt admitted: {order:?}");
                    assert_eq!(
                        *visible,
                        Some(5),
                        "live text attached to stale canonical context: {order:?}"
                    );
                    assert_eq!(text, A);
                    assert!(!did_append, "duplicate append: {order:?}");
                    did_append = true;
                }
                Change::Canonical(message) => *visible = Some(message.seq),
                Change::Reload => did_reload = true,
                Change::Ignore | Change::Started => {}
            }
        };
        for action in order {
            match action {
                C::Commit => committed = true,
                C::Read => {
                    let mut value = snapshot();
                    if committed {
                        if let Event::MessageAppended { message, .. } = &canonical {
                            value["messages"]
                                .as_array_mut()
                                .unwrap()
                                .push(serde_json::to_value(message).unwrap());
                        }
                    }
                    sampled = Some(value);
                }
                C::Start => {
                    clock += 1;
                    start = Some(frame(clock, Phase::Started { base_seq: 5 }));
                }
                C::Emit => {
                    clock += 1;
                    delta = Some(frame(
                        clock,
                        Phase::Delta {
                            offset: 0,
                            text: A.into(),
                        },
                    ));
                }
                C::Sample => cutoff = clock,
                C::Publish => {}
                C::Reply => {
                    let value = sampled.as_ref().unwrap();
                    visible = Some(if value["messages"].as_array().unwrap().len() == 2 {
                        5
                    } else {
                        4
                    });
                    coverage.insert((visible.unwrap(), cutoff));
                    let mut loaded=pending.take().unwrap().finish(&tag,&json!({"snapshot":value,"barrier":{"epoch":EPOCH,"cursor":cutoff},"mode":"future_attempts_only_v1"})).unwrap();
                    for event in &loaded.buffered {
                        consume(&mut loaded.gate, event, &mut visible);
                    }
                    gate = Some(loaded.gate);
                }
                C::ReceiveCanonical | C::ReceiveStart | C::ReceiveDelta => {
                    let event = match action {
                        C::ReceiveCanonical => &canonical,
                        C::ReceiveStart => start.as_ref().unwrap(),
                        _ => delta.as_ref().unwrap(),
                    };
                    if let Some(gate) = &mut gate {
                        consume(gate, event, &mut visible);
                    } else {
                        pending.as_mut().unwrap().observe(&tag, event).unwrap();
                    }
                }
            }
        }
        appending += usize::from(did_append);
        reloading += usize::from(did_reload);
    });
    assert!(count > 1000);
    assert!(appending > 0 && reloading > 0);
    assert_eq!(
        coverage,
        (4..=5)
            .flat_map(|seq| (0..=2).map(move |cursor| (seq, cursor)))
            .collect()
    );
    eprintln!("barrier_canonical_schedules {{\"schedules\":{count},\"appending\":{appending},\"reloading\":{reloading},\"snapshot_cutoff_cells\":{}}}",coverage.len());
}
