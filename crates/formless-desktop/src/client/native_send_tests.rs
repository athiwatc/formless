use super::*;
use formless_protocol::SendMode;
use serde_json::json;
fn engine() -> Engine {
    Engine {
        ready: true,
        reviewed_send_available: true,
        instance: uuid::Uuid::new_v4(),
        connection: 4,
        epoch: 7,
        generation: 3,
        active: Some("a".into()),
        ..Default::default()
    }
}
fn intent(mode: SendMode) -> Intent {
    Intent::SendMessage {
        session_id: "a".into(),
        content: "exact ไทย😀".into(),
        mode,
    }
}
fn review(e: &mut Engine) {
    e.command(Command::Review {
        intent: intent(SendMode::Send),
        draft_basis: "exact ไทย😀".into(),
        epoch: e.epoch,
    });
}
fn reply(e: &mut Engine, id: u64, value: Value) {
    e.receive(ServerMessage::ok(id, value)).unwrap();
}
fn inventory() -> Value {
    json!({"session_id":"a","attachments":[{"id":"one","name":"private ไทย😀","path":"/not/read","mime":"text/plain","bytes":5,"future":{"kept":true}},{"reference":true,"name":"folder","path":"/private/reference","dir":true,"bytes":0}]})
}
fn prepare(e: &mut Engine, consent: bool) -> uuid::Uuid {
    review(e);
    let read = e.outgoing.pop_front().unwrap();
    assert!(matches!(
        read.request,
        Request::ListPendingAttachments { .. }
    ));
    reply(e, read.id, inventory());
    let forecast = e.outgoing.pop_front().unwrap();
    assert!(matches!(
        forecast.request,
        Request::ForecastRun { for_send: true, .. }
    ));
    reply(
        e,
        forecast.id,
        serde_json::to_value(crate::fixture::run_forecast("a", consent)).unwrap(),
    );
    e.prepared.as_ref().unwrap().id
}
#[test]
fn native_send_requires_exact_draft_active_session_capabilities_and_rejects_raw_send_and_queue() {
    let mut e = engine();
    e.command(Command::Intent(intent(SendMode::Send)));
    assert!(e.outgoing.is_empty());
    e.command(Command::Review {
        intent: intent(SendMode::Queue),
        draft_basis: "exact ไทย😀".into(),
        epoch: e.epoch,
    });
    assert!(e.outgoing.is_empty());
    for reason in 0..4 {
        let mut e = engine();
        let mut basis = "exact ไทย😀".to_string();
        match reason {
            0 => basis.push('!'),
            1 => e.active = Some("other".into()),
            2 => e.reviewed_send_available = false,
            _ => e.epoch += 1,
        }
        e.command(Command::Review {
            intent: intent(SendMode::Send),
            draft_basis: basis,
            epoch: 7,
        });
        assert!(e.outgoing.is_empty());
    }
}
#[test]
fn native_send_never_truncates_an_over_limit_draft_to_make_it_reviewable() {
    let mut e = engine();
    let content = "ไทย".repeat(crate::forecast::MAX_REVIEW_DRAFT_BYTES);
    e.command(Command::Review {
        intent: Intent::SendMessage {
            session_id: "a".into(),
            content: content.clone(),
            mode: SendMode::Send,
        },
        draft_basis: content,
        epoch: e.epoch,
    });
    assert!(e.outgoing.is_empty());
    assert!(e.prepared.is_none());
}

#[test]
fn native_send_never_forecasts_or_sends_from_unavailable_incomplete_or_foreign_inventory() {
    for mode in 0..5 {
        let mut e = engine();
        review(&mut e);
        let read = e.outgoing.pop_front().unwrap();
        let mut raw = inventory();
        match mode {
            0 => raw["session_id"] = json!("other"),
            1 => raw["attachments"] = Value::Null,
            2 => raw["attachments"][0]["path"] = Value::Null,
            3 => {
                raw["attachments"][0]["future"] =
                    json!("x".repeat(crate::pending_files::MAX_BYTES + 1))
            }
            _ => {
                e.receive(ServerMessage::err(read.id, "unsupported".into()))
                    .unwrap();
                assert!(e.outgoing.is_empty());
                continue;
            }
        }
        reply(&mut e, read.id, raw);
        assert!(e.outgoing.is_empty());
        assert!(e.prepared.is_none());
        assert!(e
            .updates
            .iter()
            .any(|u| matches!(u, Update::PreparationFailed { .. })));
    }
}
#[test]
fn native_send_confirmation_binds_complete_metadata_and_one_key_without_legacy_fallback() {
    for consent in [false, true] {
        let mut e = engine();
        let id = prepare(&mut e, consent);
        e.command(Command::CommitPrepared(id));
        assert!(e.outgoing.is_empty());
        e.command(Command::ConfirmPrepared(id));
        e.command(Command::ConfirmPrepared(id));
        if consent {
            let ack = e.outgoing.pop_front().unwrap();
            assert!(matches!(ack.request, Request::ConfirmForecast { .. }));
            reply(&mut e, ack.id, json!({"confirmed":true}));
        }
        assert!(
            e.outgoing.is_empty(),
            "consent/read replies never send themselves"
        );
        e.command(Command::CommitPrepared(id));
        e.command(Command::CommitPrepared(id));
        assert_eq!(e.outgoing.len(), 1);
        let Request::SendReviewedMessage {
            expected_attachments,
            client_msg_id,
            content,
            ..
        } = &e.outgoing[0].request
        else {
            panic!("reviewed operation required");
        };
        assert_eq!(
            expected_attachments,
            &inventory()["attachments"].as_array().unwrap().clone()
        );
        assert_eq!(client_msg_id, &id.to_string());
        assert_eq!(content, "exact ไทย😀");
    }
}
#[test]
fn native_send_all_owners_retire_unwritten_work_but_cannot_discard_a_written_receipt() {
    for stage in 0..4 {
        for change in 0..7 {
            let mut e = engine();
            if stage == 0 {
                review(&mut e);
            } else {
                let id = prepare(&mut e, false);
                if stage >= 2 {
                    e.command(Command::ConfirmPrepared(id));
                    e.command(Command::CommitPrepared(id));
                }
                if stage == 3 {
                    for p in e.pending.values_mut() {
                        if let Pending::ReviewedAction { sent, .. } = p {
                            *sent = true;
                        }
                    }
                }
            }
            match change {
                0 => e.epoch += 1,
                1 => e.generation += 1,
                2 => e.instance = uuid::Uuid::new_v4(),
                3 => e.connection += 1,
                4 => e.active = Some("other".into()),
                5 => e.status_revision += 1,
                _ => e.ready = false,
            }
            e.sync_epoch(e.epoch);
            if stage == 3 {
                assert!(e
                    .pending
                    .values()
                    .any(|p| matches!(p, Pending::ReviewedAction { .. })));
            } else {
                assert!(e.outgoing.is_empty());
                assert!(e.prepared.is_none());
                assert!(e.pending.is_empty());
            }
        }
    }
}
#[test]
fn native_send_requires_mode_checks_before_requesting_a_review() {
    for supports_mode in [false, true] {
        let mut e = Engine::default();
        e.begin();
        let ping = e.outgoing.pop_front().unwrap();
        let mut features = vec![
            "pending_attachment_inventory_v1",
            "reviewed_attachment_send_v1",
        ];
        if supports_mode {
            features.push("reviewed_send_mode_v1");
        }
        reply(
            &mut e,
            ping.id,
            json!({"min_protocol":1,"max_protocol":1,"features":features}),
        );
        let subscribe = e.outgoing.pop_front().unwrap();
        reply(&mut e, subscribe.id, json!({"subscribed":true}));
        e.active = Some("a".into());
        review(&mut e);
        if supports_mode {
            assert!(matches!(
                e.outgoing.pop_front().unwrap().request,
                Request::ListPendingAttachments { .. }
            ));
        } else {
            assert!(e.outgoing.is_empty());
            assert!(e.updates.iter().any(|u|matches!(u, Update::PreparationFailed {error,..} if error.contains("mode checks"))));
        }
    }
}

#[test]
fn native_send_requires_a_matching_well_formed_run_receipt_to_clear_unknown() {
    for value in [
        json!({}),
        json!({"started":true}),
        json!({"started":true,"steered":true,"run_id":"r"}),
        json!({"queued":true,"run_id":"r"}),
        json!({"steered":true,"started":"bad","run_id":"r"}),
        json!({"started":true,"run_id":"r\n"}),
    ] {
        for mode in [SendMode::Send, SendMode::Steer, SendMode::Queue] {
            assert!(!send_review::acknowledged(mode, &value));
        }
    }
    let started = json!({"started":true,"run_id":"r"});
    assert!(send_review::acknowledged(SendMode::Send, &started));
    assert!(!send_review::acknowledged(SendMode::Steer, &started));
    let steered = json!({"steered":true,"run_id":"r","future":true});
    assert!(send_review::acknowledged(SendMode::Steer, &steered));
    assert!(!send_review::acknowledged(SendMode::Send, &steered));
    assert!(!send_review::acknowledged(SendMode::Queue, &steered));
}
