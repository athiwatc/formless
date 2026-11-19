use super::*;
use formless_desktop::{
    client::{Command as Wire, Update},
    pending_files::Report,
    reference_detach::{Dispatch, Outcome, Status},
    session_edit::Metadata,
};
use serde_json::{json, Value};
fn activate(w: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..110 {
        settle_frames(w, cx);
        if w.find(id).focused() == Some(true) {
            assert!(
                w.find(id).visible(),
                "{id} is focused at {:?}",
                w.find(id).bounds()
            );
            w.press("space", cx);
            return;
        }
        w.press("tab", cx);
    }
    panic!("unreachable reference control {id}");
}
fn setup(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>, String) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, h, &s);
    super::session_edit_tests::show(cx, h, &s, &sessions[1]);
    step(cx, h, |_, cx| {
        s.update(cx, |s, _| s.take_client_commands());
    });
    (h, s, sessions[1].id.clone())
}
fn value(id: &str, n: usize) -> Value {
    json!({"session_id":id,"attachments":(0..n).map(|index|json!({"reference":true,"name":format!("ref-{index} ไทย😀"),"path":format!("/not/read/{index}"),"dir":true,"bytes":0,"future":{"kept":true}})).collect::<Vec<_>>()})
}
fn read(cx: &mut TestAppContext, h: WindowHandle<Root>, s: &Entity<Shell>, id: &str, raw: Value) {
    let op = step(cx, h, |w, cx| {
        activate(w, cx, "pending-files-read");
        let commands = s.update(cx, |s, _| s.take_client_commands());
        let [Wire::ReadPendingAttachments { operation, .. }] = commands.as_slice() else {
            panic!("explicit read only")
        };
        *operation
    });
    step(cx, h, |w, cx| {
        let instance = s.read(cx).pending_files_instance();
        s.update(cx, |s, cx| {
            s.apply_client_update(
                Update::PendingAttachments {
                    client: instance,
                    connection: 0,
                    operation: op,
                    session_id: id.into(),
                    result: Report::decode(id, raw),
                },
                w,
                cx,
            )
        });
        settle_frames(w, cx);
    });
}
fn confirm(cx: &mut TestAppContext, h: WindowHandle<Root>, s: &Entity<Shell>) -> Dispatch {
    step(cx, h, |w, cx| {
        activate(w, cx, "reference-confirm");
        let commands = s.update(cx, |s, _| s.take_client_commands());
        let [Wire::DetachReference(d)] = commands.as_slice() else {
            panic!(
                "expected one confirmed removal, got {} commands",
                commands.len()
            )
        };
        d.clone()
    })
}

fn load_chat_details(
    window: &mut Window,
    cx: &mut App,
    shell: &Entity<Shell>,
    session: &formless_protocol::SessionInfo,
) {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    let [Wire::ReadSessionDetails {
        connection,
        operation,
        session_id,
    }] = commands.as_slice()
    else {
        panic!("opening Pending files must only read Session details");
    };
    assert_eq!(session_id, &session.id);
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::SessionDetails {
                connection: *connection,
                operation: *operation,
                session_id: session.id.clone(),
                result: Ok(Metadata::from_session(session).unwrap()),
            },
            window,
            cx,
        );
    });
    settle_frames(window, cx);
}

#[gpui_kit::test]
fn chat_pending_files_keeps_reads_and_explicit_removal_with_their_sessions(
    cx: &mut TestAppContext,
) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, h, &s);
    let first = &sessions[0];
    let second = &sessions[1];
    let (instance, operation) = step(cx, h, |w, cx| {
        w.click(session_row_id(&first.id), cx);
        s.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, w, cx);
            s.take_client_commands();
        });
        w.input("First draft ไทย😀", cx);
        w.click("chat-pending-files", cx);
        load_chat_details(w, cx, &s, first);
        assert!(w.find("pending-files-read").visible());
        activate(w, cx, "pending-files-read");
        let form = s.read(cx).session_edit_revision().unwrap();
        s.update(cx, |s, cx| s.read_presented_pending_files(form, w, cx));
        let commands = s.update(cx, |s, _| s.take_client_commands());
        let [Wire::ReadPendingAttachments {
            operation,
            session_id,
            ..
        }] = commands.as_slice()
        else {
            panic!("repeated activation must leave exactly one explicit inventory read");
        };
        assert_eq!(session_id, &first.id);
        let owner = (s.read(cx).pending_files_instance(), *operation);
        w.press("escape", cx);
        settle_frames(w, cx);
        assert_eq!(s.read(cx).draft(cx).as_ref(), "First draft ไทย😀");
        owner
    });

    step(cx, h, |w, cx| {
        w.click(session_row_id(&second.id), cx);
        s.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, w, cx);
            s.take_client_commands();
        });
        w.input("Second draft 漢字", cx);
        w.press(palette_key(), cx);
        w.input("review pending files", cx);
        w.render_frame(cx);
        assert_eq!(palette_titles(w), ["Review pending files"]);
        w.press("enter", cx);
        load_chat_details(w, cx, &s, second);
        s.update(cx, |s, cx| {
            s.apply_client_update(
                Update::PendingAttachments {
                    client: instance,
                    connection: 0,
                    operation,
                    session_id: first.id.clone(),
                    result: Report::decode(&first.id, value(&first.id, 1)),
                },
                w,
                cx,
            );
        });
        assert_eq!(s.read(cx).pending_files_count(), None);
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });

    let mut inventory = value(&second.id, 2);
    inventory["attachments"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "stored",
            "name": "notes.txt",
            "path": "/not/read/stored",
            "mime": "text/plain",
            "bytes": 19,
        }));
    read(cx, h, &s, &second.id, inventory.clone());
    step(cx, h, |w, cx| {
        assert!(w.try_find("reference-review-2").is_none());
        activate(w, cx, "reference-review-0");
        activate(w, cx, "reference-cancel");
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        activate(w, cx, "reference-review-1");
        assert_eq!(w.find("reference-cancel").focused(), Some(true));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
    let removal = confirm(cx, h, &s);
    assert_eq!(removal.action().session_id, second.id);
    assert_eq!(removal.action().index, 1);
    assert_eq!(
        serde_json::from_str::<Value>(&removal.action().report.copy()).unwrap(),
        inventory
    );
    step(cx, h, |w, cx| {
        let form = s.read(cx).session_edit_revision().unwrap();
        s.update(cx, |s, cx| {
            s.confirm_presented_reference(form, removal.action().operation, w, cx);
        });
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        s.update(cx, |s, cx| {
            s.finish_reference_for_test(
                &removal,
                Ok(json!({
                    "removed": true,
                    "session_id": second.id,
                    "index": 1,
                    "operation_id": removal.action().operation.to_string(),
                })),
                true,
            );
            s.poll_client(w, cx);
        });
        assert_eq!(s.read(cx).pending_files_count(), None);
        w.press("escape", cx);
        settle_frames(w, cx);
        assert_eq!(s.read(cx).draft(cx).as_ref(), "Second draft 漢字");
        assert!(s.read(cx).last_intent().is_none());
        w.click(session_row_id(&first.id), cx);
        assert_eq!(s.read(cx).draft(cx).as_ref(), "First draft ไทย😀");
    });
}

#[gpui_kit::test]
fn chat_pending_files_button_remains_keyboard_reachable_at_200_percent(cx: &mut TestAppContext) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, h, &s);
    step(cx, h, |w, cx| {
        w.click(session_row_id(&sessions[0].id), cx);
        s.update(cx, |s, _| s.take_client_commands());
    });
    cx.simulate_window_resize(h.into(), MINIMUM);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, w, cx));
        activate(w, cx, "chat-pending-files");
        load_chat_details(w, cx, &s, &sessions[0]);
        activate(w, cx, "pending-files-read");
        let commands = s.update(cx, |s, _| s.take_client_commands());
        assert!(matches!(
            commands.as_slice(),
            [Wire::ReadPendingAttachments { session_id, .. }] if session_id == &sessions[0].id
        ));
        w.press("escape", cx);
        settle_frames(w, cx);
        assert_eq!(w.find("chat-pending-files").focused(), Some(true));
        s.update(cx, |s, cx| s.focus_section(PageSection::Composer, w, cx));
        settle_frames(w, cx);
        assert_composer_in_view(w, MINIMUM, "after closing Pending files");
    });
}

#[gpui_kit::test]
fn native_reference_selection_requires_cancel_first_confirmation_and_retires_the_old_snapshot(
    cx: &mut TestAppContext,
) {
    let (h, s, id) = setup(cx);
    let mut raw = value(&id, 3);
    raw["attachments"][2] = raw["attachments"][1].clone();
    read(cx, h, &s, &id, raw.clone());
    step(cx, h, |w, cx| {
        activate(w, cx, "reference-review-2");
        assert_eq!(w.find("reference-cancel").focused(), Some(true));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert!(w
            .find("reference-removal-target")
            .value()
            .unwrap()
            .contains("reference 3"));
    });
    let d = confirm(cx, h, &s);
    assert_eq!(d.action().index, 2);
    assert_eq!(d.action().session_id, id);
    assert_eq!(
        serde_json::from_str::<Value>(&d.action().report.copy()).unwrap(),
        raw
    );
    step(cx, h, |w, cx| {
        let form = s.read(cx).session_edit_revision().unwrap();
        s.update(cx, |s, cx| {
            s.confirm_presented_reference(form, d.action().operation, w, cx)
        });
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        s.update(cx,|s,cx|{s.finish_reference_for_test(&d,Ok(json!({"removed":true,"session_id":id,"index":2,"operation_id":d.action().operation.to_string()})),true);s.poll_client(w,cx);});
        settle_frames(w, cx);
        assert_eq!(s.read(cx).pending_files_count(), None);
        assert!(w
            .find("pending-files-notice")
            .value()
            .unwrap()
            .contains("One reviewed reference"));
        assert!(matches!(
            s.read(cx).reference_status(),
            Status::Finished {
                outcome: Outcome::Removed,
                ..
            }
        ));
    });
}
#[gpui_kit::test]
fn native_reference_stale_confirm_cancel_and_reload_never_retarget_a_new_review(
    cx: &mut TestAppContext,
) {
    let (h, s, id) = setup(cx);
    read(cx, h, &s, &id, value(&id, 2));
    let (form, old) = step(cx, h, |w, cx| {
        activate(w, cx, "reference-review-0");
        (
            s.read(cx).session_edit_revision().unwrap(),
            s.read(cx).reference_review_operation().unwrap(),
        )
    });
    step(cx, h, |w, cx| {
        activate(w, cx, "reference-review-1");
        let current = s.read(cx).reference_review_operation().unwrap();
        assert_ne!(old, current);
        s.update(cx, |s, cx| {
            s.confirm_presented_reference(form, old, w, cx);
            s.cancel_presented_reference(form, old, w, cx);
        });
        assert_eq!(s.read(cx).reference_review_operation(), Some(current));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        activate(w, cx, "reference-cancel");
        assert_eq!(s.read(cx).reference_review_operation(), None);
        assert_eq!(w.find("pending-files-read").focused(), Some(true));
        activate(w, cx, "reference-review-0");
        let obsolete = s.read(cx).reference_review_operation().unwrap();
        activate(w, cx, "session-edit-reload");
        s.update(cx, |s, cx| {
            s.confirm_presented_reference(form, obsolete, w, cx)
        });
        assert!(s
            .update(cx, |s, _| s.take_client_commands())
            .iter()
            .all(|c| !matches!(c, Wire::DetachReference(_))));
    });
}
#[gpui_kit::test]
fn native_reference_unknown_survives_empty_fresh_reads_and_blocks_new_mutation(
    cx: &mut TestAppContext,
) {
    let (h, s, id) = setup(cx);
    read(cx, h, &s, &id, value(&id, 1));
    step(cx, h, |w, cx| activate(w, cx, "reference-review-0"));
    let d = confirm(cx, h, &s);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| {
            s.finish_reference_for_test(&d, Err("could fail after commit".into()), false);
            s.poll_client(w, cx);
        });
        settle_frames(w, cx);
        assert!(matches!(
            s.read(cx).reference_status(),
            Status::Finished {
                outcome: Outcome::Unknown,
                ..
            }
        ));
        assert_eq!(s.read(cx).pending_files_count(), None);
    });
    read(cx, h, &s, &id, json!({"session_id":id,"attachments":[]}));
    assert!(matches!(
        step(cx, h, |_, cx| s.read(cx).reference_status()),
        Status::Finished {
            outcome: Outcome::Unknown,
            ..
        }
    ));
    read(cx, h, &s, &id, value(&id, 1));
    step(cx, h, |w, cx| {
        settle_frames(w, cx);
        assert_eq!(w.find("reference-review-0").disabled(), Some(true));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert!(w
            .find("reference-removal-status")
            .value()
            .unwrap()
            .contains("outcome is unknown"));
    });
}
#[gpui_kit::test]
fn native_reference_all_positions_are_reachable_at_minimum_window_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (h, s, id) = setup(cx);
    read(cx, h, &s, &id, value(&id, 64));
    cx.simulate_window_resize(h.into(), MINIMUM);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, w, cx));
        for _ in 1..8 {
            activate(w, cx, "pending-files-next");
        }
        activate(w, cx, "reference-review-7");
        assert_eq!(w.find("reference-cancel").focused(), Some(true));
        assert!(w
            .find("reference-removal-target")
            .value()
            .unwrap()
            .contains("/not/read/63"));
    });
    let d = confirm(cx, h, &s);
    assert_eq!(d.action().index, 63);
    assert_eq!(d.action().report.rows.len(), 64);
}

#[gpui_kit::test]
fn native_reference_stored_rows_and_composition_never_authorize_removal(cx: &mut TestAppContext) {
    let (h, s, id) = setup(cx);
    let mut raw = value(&id, 2);
    raw["attachments"][0] = json!({"id":"stored","name":"keep.txt","mime":"text/plain","path":"/not/read/stored","bytes":1});
    read(cx, h, &s, &id, raw);
    step(cx, h, |w, cx| {
        assert!(w.try_find("reference-review-0").is_none());
        activate(w, cx, "reference-review-1");
        let form = s.read(cx).session_edit_revision().unwrap();
        let operation = s.read(cx).reference_review_operation().unwrap();
        let input = s.read(cx).session_edit_input().unwrap();
        let bounds = w.find_by_label("Session name").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input.clone()), w)
            .compose_text(None, "かな", None, cx);
        s.update(cx, |s, cx| {
            s.confirm_presented_reference(form, operation, w, cx)
        });
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert_eq!(s.read(cx).reference_review_operation(), Some(operation));
        input.update(cx, |input, cx| input.unmark_text(w, cx));
        s.update(cx, |s, cx| {
            s.cancel_presented_reference(form, operation, w, cx)
        });
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn native_reference_profile_replacement_keeps_uncertain_notice_and_rejects_old_controls(
    cx: &mut TestAppContext,
) {
    let (h, s, id) = setup(cx);
    read(cx, h, &s, &id, value(&id, 1));
    step(cx, h, |w, cx| activate(w, cx, "reference-review-0"));
    let d = confirm(cx, h, &s);
    step(cx, h, |w, cx| {
        let form = s.read(cx).session_edit_revision().unwrap();
        s.update(cx, |s, cx| {
            s.finish_reference_for_test(&d, Err("lost acknowledgement".into()), false);
            s.poll_client(w, cx);
            s.connect_recording_daemon(cx);
            s.confirm_presented_reference(form, d.action().operation, w, cx);
        });
        settle_frames(w, cx);
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert!(w
            .find("session-edit-notice")
            .value()
            .unwrap()
            .contains("reference-removal outcome is unknown"));
    });
}
