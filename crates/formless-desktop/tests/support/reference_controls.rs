use super::*;
use formless_desktop::{
    client::{Command as Wire, Update},
    pending_files::Report,
    reference_detach::{Dispatch, Outcome, Status},
};
use serde_json::{json, Value};
fn activate(w: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..110 {
        settle_frames(w, cx);
        if w.find(id).focused() == Some(true) {
            assert!(w.find(id).visible());
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
