use super::*;
use formless_desktop::client::{Command as Wire, Update};
use formless_desktop::run_actions::{Kind, Target};
use serde_json::json;

fn draft(shell: &Entity<Shell>, text: &str, window: &mut Window, cx: &mut App) {
    let input = shell.read(cx).composer().clone();
    input.update(cx, |input, cx| {
        input.set_value(text, window, cx);
        input.focus(window, cx);
    });
    window.render_frame(cx);
}

fn select(shell: &Entity<Shell>, session: &str, window: &mut Window, cx: &mut App) {
    window.render_frame(cx);
    window.click(session_row_id(session), cx);
    let generation = shell.read(cx).client_generation();
    shell.update(cx, |s, cx| {
        s.apply_client_update(
            Update::Snapshot {
                generation,
                snapshot: replies::session_snapshot(
                    session,
                    &json!({"session":{"id":session},"running":false,"messages":[]}),
                )
                .unwrap(),
            },
            window,
            cx,
        );
        s.take_client_commands();
    });
}

#[gpui_kit::test]
fn slash_popup_filters_navigates_completes_and_dismisses(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    step(cx, handle, |window, cx| {
        draft(&shell, "/", window, cx);
        assert!(window.find("slash-popup").visible());
        assert_eq!(window.find("slash-row-0").selected(), Some(true));
        window.press("up", cx);
        window.render_frame(cx);
        assert_eq!(window.find("slash-row-0").selected(), Some(true));
        window.press("down", cx);
        window.render_frame(cx);
        assert_eq!(window.find("slash-row-1").selected(), Some(true));
        window.press("tab", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "/new");
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        draft(&shell, "/cmp", window, cx);
        assert_eq!(window.find("slash-row-0").label(), Some("/compact [FOCUS]"));
        window.press("tab", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "/compact ");
        window.render_frame(cx);
        assert!(window.try_find("slash-popup").is_none());
        draft(&shell, "/set", window, cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("slash-popup").is_none());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "/set");
        draft(&shell, "/sett", window, cx);
        assert!(window.find("slash-popup").visible());
        window.press("enter", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Settings);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[gpui_kit::test]
fn slash_selection_stops_at_last_and_scrolls_into_view(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    step(cx, handle, |window, cx| {
        draft(&shell, "/", window, cx);
        for _ in 0..40 {
            window.press("down", cx);
            window.render_frame(cx);
        }
        let last = format!("slash-row-{}", formless_desktop::slash::COMMANDS.len() - 1);
        assert_eq!(window.find(last.clone()).selected(), Some(true));
        let popup = window.find("slash-popup").bounds();
        let row = window.find(last).bounds();
        assert!(
            row.top() >= popup.top() && row.bottom() <= popup.bottom(),
            "row {row:?}, popup {popup:?}"
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "/");
    });
}

#[gpui_kit::test]
fn slash_mouse_accepts_and_shift_enter_remains_a_newline(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    step(cx, handle, |window, cx| {
        draft(&shell, "/sett", window, cx);
        window.press("shift-enter", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert!(shell.read(cx).draft(cx).contains('\n'));
        draft(&shell, "/sett", window, cx);
        window.click("slash-row-0", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Settings);
    });
}

#[gpui_kit::test]
fn slash_errors_keep_the_draft_and_never_send(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    for (text, notice) in [
        (
            "/setings",
            "Unknown command /setings — did you mean /settings?",
        ),
        ("/cancel", "/cancel isn't available in the desktop app yet"),
        ("/settings extra", "usage: /settings"),
        ("/retry --model", "usage: /retry [--model MODEL]"),
        ("/stop", commands::NO_RUN),
        ("/new", "Wait for a ready Profile connection"),
        ("/usage", "Choose a connected Session in Chat first"),
        ("/step", "Step mode is not available for this Session"),
        ("/compact focus", "Open a connected Session in Chat first"),
    ] {
        step(cx, handle, |window, cx| {
            draft(&shell, text, window, cx);
        });
        cx.run_until_parked();
        step(cx, handle, |window, cx| {
            window.press("escape", cx);
            window.render_frame(cx);
            window.press("enter", cx);
        });
        cx.run_until_parked();
        step(cx, handle, |_, cx| {
            assert_eq!(shell.read(cx).draft(cx).as_ref(), text);
            assert_eq!(shell.read(cx).composer_notice(), Some(notice));
            assert!(shell.read(cx).last_intent().is_none());
        });
    }
}

#[gpui_kit::test]
fn slash_compact_prefills_the_existing_canonical_review(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        draft(&shell, "/compact keep the tests ไทย", window, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.press("enter", cx);
    });
    step(cx, handle, |window, cx| {
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let (owner, operation) = commands
            .into_iter()
            .find_map(|c| match c {
                Wire::ReadRunAction { owner, operation } => Some((owner, operation)),
                _ => None,
            })
            .expect("slash opens a fresh canonical read, never a direct mutation");
        let value = json!({"session":{"id":owner.session,"cwd":null},"running":false,
            "latest_run":{"session_id":owner.session,"run_id":"r","status":"completed"},
            "messages":[{"id":"u","session_id":owner.session,"role":"user","content":"original","seq":1,"created_at":"now"}]});
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActionTarget {
                    owner: owner.clone(),
                    operation,
                    result: Ok(Target::decode(&owner.session, &value).unwrap()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.find("run-action-dialog").visible());
        window.click("run-action-prepare", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(c, Wire::PrepareRunAction(d) if d.action.kind == Kind::Compact && d.action.text == "keep the tests ไทย")));
        assert!(!commands
            .iter()
            .any(|c| matches!(c, Wire::CommitRunAction(_))));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
    });
}

#[gpui_kit::test]
fn slash_search_prefills_and_uses_existing_profile_search(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        draft(&shell, "/search exact query ไทย", window, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).destination(), Destination::ProfileSearch);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands
            .iter()
            .any(|c| matches!(c, Wire::SearchProfile { query, .. } if query == "exact query ไทย")));
    });
}

#[gpui_kit::test]
fn slash_literal_escape_uses_the_exact_unescaped_review_draft(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        draft(&shell, "//settings", window, cx);
        assert!(window.try_find("slash-popup").is_none());
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "//settings");
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(c, Wire::Review { intent: formless_desktop::model::Intent::SendMessage { content, .. }, draft_basis, .. } if content == "/settings" && draft_basis == "/settings")));
    });
}

#[gpui_kit::test]
fn slash_popup_is_hidden_and_keys_are_not_taken_during_ime(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    step(cx, handle, |window, cx| {
        draft(&shell, "/set", window, cx);
        let composer = shell.read(cx).composer().clone();
        let bounds = window.find_by_label("Message").bounds();
        TestInput::new(ElementInputHandler::new(bounds, composer.clone()), window).compose_text(
            Some(0..4),
            "/settings",
            None,
            cx,
        );
        shell.update(cx, |s, cx| {
            s.run_command(Command::Send, window, cx);
        });
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(shell.read(cx).composer_notice(), Some(commands::COMPOSING));
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("slash-popup").is_none());
    });
}

#[gpui_kit::test]
fn slash_bare_opens_help_not_a_new_session(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    step(cx, handle, |window, cx| {
        draft(&shell, "/", window, cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.find("palette").visible());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[gpui_kit::test]
fn slash_disabled_fuzzy_selection_keeps_original_spelling(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    step(cx, handle, |window, cx| {
        draft(&shell, "/sto", window, cx);
        window.press("enter", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "/sto");
        assert_eq!(shell.read(cx).composer_notice(), Some(commands::NO_RUN));
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[gpui_kit::test]
fn slash_changed_filter_resets_selection_and_scroll(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 3, 4);
    step(cx, handle, |window, cx| {
        draft(&shell, "/", window, cx);
        for _ in 0..40 {
            window.press("down", cx);
            window.render_frame(cx);
        }
        draft(&shell, "/sett", window, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        draft(&shell, "/", window, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("slash-row-0").selected(), Some(true));
        let popup = window.find("slash-popup").bounds();
        let row = window.find("slash-row-0").bounds();
        assert!(row.top() >= popup.top() && row.bottom() <= popup.bottom());
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.find("palette").visible());
    });
}

#[gpui_kit::test]
fn slash_metadata_arguments_prefill_but_never_save_without_review(cx: &mut TestAppContext) {
    use formless_desktop::session_edit::{Instructions, Metadata};
    for (text, expected) in [
        ("/rename Revised ไทย", Some("Revised ไทย")),
        ("/rename", Some("Confirmed name")),
        ("/instructions new instructions", Some("new instructions")),
        ("/instructions clear", Some("")),
        ("/instructions", Some("existing instructions")),
        ("/tag one two", Some("one\ntwo")),
        ("/pin", None),
        ("/export", Some("Confirmed name")),
    ] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        let sessions = daemon_view(cx, handle, &shell);
        step(cx, handle, |window, cx| {
            select(&shell, &sessions[0].id, window, cx);
            draft(&shell, text, window, cx);
        });
        cx.run_until_parked();
        step(cx, handle, |window, cx| {
            window.press("enter", cx);
        });
        step(cx, handle, |window, cx| {
            let commands = shell.update(cx, |s, _| s.take_client_commands());
            let [Wire::ReadSessionDetails {
                connection,
                operation,
                session_id,
            }] = commands.as_slice()
            else {
                panic!("expected a native metadata read, not a save");
            };
            let mut metadata = Metadata::from_session(&sessions[0]).unwrap();
            metadata.name = "Confirmed name".into();
            metadata.instructions = Instructions::Text("existing instructions".into());
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::SessionDetails {
                        connection: *connection,
                        operation: *operation,
                        session_id: session_id.clone(),
                        result: Ok(metadata),
                    },
                    window,
                    cx,
                )
            });
            settle_frames(window, cx);
            if let Some(expected) = expected {
                assert_eq!(
                    shell
                        .read(cx)
                        .session_edit_input()
                        .unwrap()
                        .read(cx)
                        .value()
                        .as_ref(),
                    expected,
                    "{text}"
                );
            } else {
                assert!(window
                    .find("session-edit-pin-value")
                    .value()
                    .unwrap()
                    .contains("Save will"));
            }
            assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
            assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        });
    }
}

#[gpui_kit::test]
fn slash_usage_and_step_use_native_read_and_sampled_toggle(cx: &mut TestAppContext) {
    for text in ["/usage", "/step"] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        let sessions = daemon_view(cx, handle, &shell);
        step(cx, handle, |window, cx| {
            let id = &sessions[0].id;
            select(&shell, id, window, cx);
            let generation = shell.read(cx).client_generation();
            shell.update(cx, |s, cx| s.apply_client_update(Update::Snapshot {
                generation,
                snapshot: replies::session_snapshot(id, &json!({"session":{"id":id},"messages":[],"running":false,"step_mode":false,"pending_steps":{"prompts":[],"more":false}})).unwrap(),
            }, window, cx));
            draft(&shell, text, window, cx);
        });
        cx.run_until_parked();
        step(cx, handle, |window, cx| {
            window.press("enter", cx);
        });
        step(cx, handle, |_, cx| {
            let commands = shell.update(cx, |s, _| s.take_client_commands());
            if text == "/usage" {
                assert!(
                    matches!(commands.as_slice(), [Wire::ReadSessionUsage { target, .. }] if target.session_id == sessions[0].id)
                );
            } else {
                assert!(
                    matches!(commands.as_slice(), [Wire::EnableStepMode(d)] if d.action().session_id == sessions[0].id && d.action().on)
                );
            }
            assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        });
    }
}
