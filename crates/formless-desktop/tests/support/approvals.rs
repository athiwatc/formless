use super::*;
use formless_desktop::approvals::{fixture_inventory, Outcome, Prompt, Status};
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
        let result = Prompt::from_list(&value).map_err(str::to_string);
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
