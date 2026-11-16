use super::*;
use formless_desktop::approvals::{
    fixture_inventory, fixture_multiple, Inventory, Outcome, Status,
};
use formless_desktop::client::{Command as Wire, Update};
use serde_json::json;
fn setup(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, _| s.take_client_commands());
        shell.update(cx, |s, cx| {
            assert!(s.run_command(Command::Approvals, window, cx).is_none())
        });
    });
    (handle, shell)
}
fn ready(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    value: serde_json::Value,
) {
    step(cx, handle, |window, cx| {
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert_eq!(commands.len(), 1);
        let Wire::ReadApprovals {
            connection,
            operation,
        } = &commands[0]
        else {
            panic!("expected explicit approval read")
        };
        let (connection, operation) = (*connection, *operation);
        let client = shell.read(cx).live_stream_instance().unwrap();
        let result = Inventory::from_list(&value).map_err(str::to_string);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::ApprovalInventory {
                    client,
                    connection,
                    operation,
                    revision: 0,
                    result,
                },
                window,
                cx,
            )
        });
        settle_frames(window, cx);
    });
}
#[gpui_kit::test]
fn approval_review_defaults_to_close_and_preserves_the_exact_composer(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    let input = step(cx, handle, |window, cx| {
        shell.update(cx, |s, _| s.take_client_commands());
        let input = shell.read(cx).composer().clone();
        input.update(cx, |i, cx| i.set_value("Unsent ไทย 😀", window, cx));
        shell.update(cx, |s, cx| {
            s.run_command(Command::Approvals, window, cx);
        });
        input
    });
    ready(cx, handle, &shell, fixture_inventory());
    step(cx, handle, |window, cx| {
        assert_eq!(window.find("approval-review-close").focused(), Some(true));
        assert_eq!(shell.read(cx).composer(), &input);
        assert_eq!(input.read(cx).value().as_ref(), "Unsent ไทย 😀");
        window.press("enter", cx);
        settle_frames(window, cx);
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert!(window.try_find("approval-review-dialog").is_none());
        assert_eq!(input.read(cx).value().as_ref(), "Unsent ไทย 😀");
    });
}
#[gpui_kit::test]
fn approval_allow_and_deny_are_explicit_once_and_spent_callbacks_never_repeat(
    cx: &mut TestAppContext,
) {
    for allow in [false, true] {
        let (handle, shell) = setup(cx);
        ready(cx, handle, &shell, fixture_inventory());
        step(cx, handle, |window, cx| {
            let (revision, prompt) = shell.read(cx).approval_target().unwrap();
            window.click(
                if allow {
                    "approval-allow-once"
                } else {
                    "approval-deny"
                },
                cx,
            );
            let mut commands = shell.update(cx, |s, _| s.take_client_commands());
            assert_eq!(commands.len(), 1);
            let Wire::DecideApproval(d) = commands.remove(0) else {
                panic!("expected approval decision")
            };
            assert_eq!(d.action().allow, allow);
            assert_eq!(&d.action().prompt, &prompt);
            shell.update(cx, |s, cx| {
                s.submit_approval(revision, prompt.clone(), !allow, window, cx)
            });
            assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
            shell.update(cx, |s, cx| {
                s.finish_recorded_approval(&d, json!({"resolved":true,"audit_recorded":false}), cx)
            });
            assert!(matches!(
                shell.read(cx).approval_review_status(),
                Status::Finished {
                    outcome: Outcome::Accepted {
                        audit_recorded: false
                    },
                    ..
                }
            ));
            window.click("approval-reload", cx);
            shell.update(cx, |s, cx| {
                s.submit_approval(revision, prompt, allow, window, cx)
            });
            let commands = shell.update(cx, |s, _| s.take_client_commands());
            assert_eq!(commands.len(), 1);
            assert!(matches!(commands[0], Wire::ReadApprovals { .. }));
        });
    }
}
#[gpui_kit::test]
fn approval_unsupported_and_unrenderable_evidence_cannot_be_granted(cx: &mut TestAppContext) {
    for huge in [false, true] {
        let (handle, shell) = setup(cx);
        let mut v = fixture_inventory();
        if huge {
            v[0]["context"]["call"]["arguments"] =
                json!({"text":format!("a{}","\u{0301}".repeat(4200))});
        } else {
            v[0]["context"]["call"]["truncated"] = json!(true);
        }
        ready(cx, handle, &shell, v);
        step(cx, handle, |window, cx| {
            assert_eq!(window.find("approval-allow-once").disabled(), Some(true));
            window.click("approval-allow-once", cx);
            assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        });
    }
}
#[gpui_kit::test]
fn approval_revision_and_profile_replacement_reject_stale_read_and_decision_callbacks(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = setup(cx);
    ready(cx, handle, &shell, fixture_inventory());
    step(cx, handle, |window, cx| {
        let (revision, prompt) = shell.read(cx).approval_target().unwrap();
        let client = shell.read(cx).live_stream_instance().unwrap();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::ApprovalInvalidated {
                    client,
                    connection: 0,
                    revision: 1,
                },
                window,
                cx,
            )
        });
        shell.update(cx, |s, cx| {
            s.submit_approval(revision, prompt.clone(), true, window, cx)
        });
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        shell.update(cx, |s, cx| s.connect_recording_daemon(cx));
        shell.update(cx, |s, cx| {
            s.submit_approval(revision, prompt, true, window, cx)
        });
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
#[gpui_kit::test]
fn approval_controls_and_evidence_are_keyboard_reachable_at_200_percent(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    ready(cx, handle, &shell, fixture_inventory());
    cx.simulate_window_resize(handle.into(), MINIMUM);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, window, cx));
        for _ in 0..20 {
            settle_frames(window, cx);
            if window.find("approval-evidence").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("approval-evidence").focused(), Some(true));
        window.press("end", cx);
        window.press("home", cx);
        for _ in 0..20 {
            settle_frames(window, cx);
            if window.find("approval-deny").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("approval-deny").focused(), Some(true));
        assert!(window.find("approval-deny").visible());
        window.press("space", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert_eq!(commands.len(), 1);
        assert!(matches!(&commands[0],Wire::DecideApproval(d)if !d.action().allow));
    });
}

fn activate(window: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..40 {
        settle_frames(window, cx);
        if window.find(id).focused() == Some(true) {
            assert!(window.find(id).visible());
            window.press("space", cx);
            return;
        }
        window.press("tab", cx);
    }
    panic!("approval control not keyboard reachable: {id}");
}
#[gpui_kit::test]
fn multiple_approval_selection_retires_cached_decisions_and_never_approves_by_navigation(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    ready(cx, h, &s, fixture_multiple());
    step(cx, h, |w, cx| {
        let old = s.read(cx).approval_target().unwrap();
        let composer = s.read(cx).composer().clone();
        let draft = composer.read(cx).value().to_string();
        activate(w, cx, "approval-next");
        let current = s.read(cx).approval_target().unwrap();
        assert_eq!(current.1.id(), "approval-fixture-2");
        assert_ne!(old.0, current.0);
        assert_eq!(w.find("approval-review-close").focused(), Some(true));
        assert!(w
            .find("approval-selection")
            .value()
            .unwrap()
            .contains("Request 2 of 2"));
        activate(w, cx, "approval-previous");
        let revisited = s.read(cx).approval_target().unwrap();
        assert_eq!(revisited.1, old.1);
        assert_ne!(revisited.0, old.0);
        s.update(cx, |s, cx| {
            s.submit_approval(old.0, old.1.clone(), true, w, cx)
        });
        s.update(cx, |s, cx| s.select_presented_approval(old.0, 1, w, cx));
        assert_eq!(
            s.read(cx).approval_target().unwrap().1.id(),
            "approval-fixture"
        );
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        activate(w, cx, "approval-next");
        let current = s.read(cx).approval_target().unwrap();
        s.update(cx, |s, cx| s.submit_approval(old.0, old.1, true, w, cx));
        s.update(cx, |s, cx| s.select_presented_approval(old.0, 0, w, cx));
        assert_eq!(
            s.read(cx).approval_target().unwrap().1.id(),
            "approval-fixture-2"
        );
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        activate(w, cx, "approval-deny");
        let mut commands = s.update(cx, |s, _| s.take_client_commands());
        assert_eq!(commands.len(), 1);
        let Wire::DecideApproval(dispatch) = commands.remove(0) else {
            panic!("one selected decision")
        };
        assert_eq!(dispatch.action().prompt.id(), "approval-fixture-2");
        assert!(!dispatch.action().allow);
        s.update(cx, |s, cx| s.select_presented_approval(current.0, 0, w, cx));
        assert_eq!(
            s.read(cx).approval_target().unwrap().1.id(),
            "approval-fixture-2"
        );
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert_eq!(composer.read(cx).value().as_ref(), draft);
    });
}
#[gpui_kit::test]
fn multiple_approval_navigation_is_keyboard_reachable_at_200_percent_and_ime_guarded(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    let mut value = fixture_multiple();
    for n in 3..=formless_desktop::approvals::MAX_APPROVALS {
        let mut row = value[0].clone();
        row["approval_id"] = json!(format!("approval-fixture-{n}"));
        row["session_id"] = json!(format!("session-{n}"));
        value.as_array_mut().unwrap().push(row);
    }
    ready(cx, h, &s, value);
    cx.simulate_window_resize(h.into(), MINIMUM);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, w, cx));
        for n in 2..=formless_desktop::approvals::MAX_APPROVALS {
            activate(w, cx, "approval-next");
            assert_eq!(
                s.read(cx).approval_target().unwrap().1.id(),
                format!("approval-fixture-{n}")
            );
            assert_eq!(w.find("approval-review-close").focused(), Some(true));
        }
        assert_eq!(w.find("approval-next").disabled(), Some(true));
        let (rev, prompt) = s.read(cx).approval_target().unwrap();
        let input = s.read(cx).composer().clone();
        input.update(cx, |i, cx| {
            i.replace_and_mark_text_in_range(None, "かな", None, w, cx)
        });
        s.update(cx, |s, cx| s.select_presented_approval(rev, 0, w, cx));
        assert_eq!(s.read(cx).approval_target().unwrap().1, prompt);
        input.update(cx, |i, cx| i.unmark_text(w, cx));
        activate(w, cx, "approval-previous");
        assert_eq!(
            s.read(cx).approval_target().unwrap().1.id(),
            "approval-fixture-15"
        );
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn multiple_approval_unknown_receipt_blocks_other_requests_even_after_explicit_reload(
    cx: &mut TestAppContext,
) {
    let (h, s) = setup(cx);
    ready(cx, h, &s, fixture_multiple());
    step(cx, h, |w, cx| {
        activate(w, cx, "approval-allow-once");
        let mut commands = s.update(cx, |s, _| s.take_client_commands());
        let Wire::DecideApproval(d) = commands.remove(0) else {
            panic!("decision")
        };
        s.update(cx, |s, cx| s.finish_recorded_approval(&d, json!({}), cx));
        assert!(matches!(
            s.read(cx).approval_review_status(),
            Status::Finished {
                outcome: Outcome::Unknown,
                ..
            }
        ));
        activate(w, cx, "approval-reload");
    });
    ready(cx, h, &s, fixture_multiple());
    step(cx, h, |w, cx| {
        let (rev, _) = s.read(cx).approval_target().unwrap();
        s.update(cx, |s, cx| s.select_presented_approval(rev, 1, w, cx));
        assert_eq!(
            s.read(cx).approval_target().unwrap().1.id(),
            "approval-fixture"
        );
        assert_eq!(w.find("approval-next").disabled(), Some(true));
        assert_eq!(w.find("approval-allow-once").disabled(), Some(true));
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn multiple_approval_profile_replacement_closes_review_and_refuses_callbacks_when_ids_repeat(
    cx: &mut TestAppContext,
) {
    for intervening_frame in [false, true] {
        let (h, s) = setup(cx);
        ready(cx, h, &s, fixture_multiple());
        let old = step(cx, h, |_, cx| s.read(cx).approval_target().unwrap());
        step(cx, h, |w, cx| {
            s.update(cx, |s, cx| s.connect_recording_daemon(cx));
            if intervening_frame {
                settle_frames(w, cx);
                assert!(w.try_find("approval-review-dialog").is_none());
                assert!(
                    s.read(cx).focused_section(w, cx).is_some(),
                    "closed review must return focus to the page"
                );
            } else {
                assert!(s.read(cx).approval_target().is_none());
            }
            s.update(cx, |s, cx| {
                s.submit_approval(old.0, old.1.clone(), true, w, cx)
            });
            s.update(cx, |s, cx| s.select_presented_approval(old.0, 1, w, cx));
            assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
            s.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::Connection {
                        ready: true,
                        notice: "Explicit new Profile fixture connection".into(),
                    },
                    w,
                    cx,
                )
            });
            s.update(cx, |s, cx| {
                assert!(s.run_command(Command::Approvals, w, cx).is_none());
            });
        });
        ready(cx, h, &s, fixture_multiple());
        step(cx, h, |w, cx| {
            let current = s.read(cx).approval_target().unwrap();
            assert_ne!(w.find("approval-allow-once").disabled(), Some(true));
            assert_eq!(
                w.find("approval-review-close").focused(),
                Some(true),
                "a fresh review must focus its new Close handle after Profile replacement"
            );
            assert_ne!(current.0, old.0);
            assert_eq!(current.1, old.1);
            s.update(cx, |s, cx| {
                s.submit_approval(old.0, old.1.clone(), true, w, cx)
            });
            assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
            activate(w, cx, "approval-deny");
            let commands = s.update(cx, |s, _| s.take_client_commands());
            assert!(
                matches!(&commands[..],[Wire::DecideApproval(d)]if !d.action().allow&&d.action().prompt==current.1)
            );
        });
    }
}
