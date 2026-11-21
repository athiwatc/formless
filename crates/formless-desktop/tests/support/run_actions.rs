use super::*;
use formless_desktop::client::{Command as Wire, Update};
use formless_desktop::run_actions::{Context as ContextUsage, Kind, Owner, Target};
use serde_json::{json, Value};

fn value(session: &str, running: bool) -> Value {
    json!({"session":{"id":session,"cwd":null},"running":running,"latest_run":{"session_id":session,"run_id":"run-actions-r","status":if running { "running" } else { "completed" }},"messages":[{"id":"last-user","session_id":session,"role":"user","content":"Original user message 😀","seq":1,"created_at":"now"}]})
}
fn select(shell: &Entity<Shell>, session: &str, window: &mut Window, cx: &mut App) {
    window.render_frame(cx);
    window.click(session_row_id(session), cx);
    let generation = shell.read(cx).client_generation();
    shell.update(cx, |s, cx| {
        s.apply_client_update(
            Update::Snapshot {
                generation,
                snapshot: replies::session_snapshot(session, &value(session, false)).unwrap(),
            },
            window,
            cx,
        )
    });
    shell.update(cx, |s, _| s.take_client_commands());
}
fn open_action(
    shell: &Entity<Shell>,
    kind: Kind,
    window: &mut Window,
    cx: &mut App,
) -> (Owner, uuid::Uuid) {
    shell.update(cx, |s, cx| {
        s.run_command(Command::RunAction(kind), window, cx)
    });
    let commands = shell.update(cx, |s, _| s.take_client_commands());
    commands
        .into_iter()
        .find_map(|c| {
            if let Wire::ReadRunAction { owner, operation } = c {
                Some((owner, operation))
            } else {
                None
            }
        })
        .expect("fresh canonical read, not a mutation")
}

#[gpui_kit::test]
fn cwd_review_shows_the_frozen_destination_and_original_typed_path(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        let (owner, operation) = open_action(&shell, Kind::Cwd, window, cx);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActionTarget {
                    owner: owner.clone(),
                    operation,
                    result: Ok(
                        Target::decode(&owner.session, &value(&owner.session, false)).unwrap(),
                    ),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.press("shift-tab", cx);
        window.input("link", cx);
        window.click("run-action-prepare", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands
            .iter()
            .any(|c| matches!(c, Wire::PrepareRunAction(d) if d.action.text == "link")));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActionReview {
                    owner,
                    operation,
                    quote: None,
                    reviewed_text: Some("/canonical/destination".into()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("run-action-frozen")
            .value()
            .unwrap()
            .contains("/canonical/destination"));
        assert!(window
            .find("run-action-typed")
            .value()
            .unwrap()
            .contains("\"link\""));
        assert_eq!(window.find("run-action-confirm").disabled(), None);
    });
}
#[gpui_kit::test]
fn every_run_action_control_reviews_exact_target_and_requires_explicit_confirmation(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        for kind in Kind::ALL {
            let (owner, operation) = open_action(&shell, kind, window, cx);
            window.render_frame(cx);
            assert_eq!(window.find("run-action-confirm").disabled(), Some(true));
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::RunActionTarget {
                        owner: owner.clone(),
                        operation,
                        result: Ok(Target::decode(
                            &owner.session,
                            &value(&owner.session, kind == Kind::ProviderRetry),
                        )
                        .unwrap()),
                    },
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("run-action-heading").value(),
                Some(kind.label())
            );
            if kind == Kind::Compact {
                let help = window.find("run-action-help").value().unwrap().to_string();
                assert!(help.contains("Invokes the provider"));
                assert!(help.contains("No compaction cost estimate"));
            }
            window.click("run-action-prepare", cx);
            let commands = shell.update(cx, |s, _| s.take_client_commands());
            let d = commands
                .into_iter()
                .find_map(|c| {
                    if let Wire::PrepareRunAction(d) = c {
                        Some(d)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(d.action.kind, kind);
            assert!(d.reserved());
            if kind == Kind::Edit {
                assert_eq!(d.action.text, "Original user message 😀");
            }
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::RunActionReview {
                        owner: owner.clone(),
                        operation,
                        reviewed_text: None,
                        quote: matches!(kind, Kind::Edit | Kind::Retry)
                            .then(|| fixture::run_forecast(&owner.session, true)),
                    },
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            assert_eq!(window.find("run-action-confirm").disabled(), None);
            window.click("run-action-confirm", cx);
            assert!(shell
                .update(cx, |s, _| s.take_client_commands())
                .iter()
                .any(|c| matches!(c,Wire::ConfirmRunAction(op) if *op == operation)));
            shell.update(cx, |s, cx| {
                s.apply_client_update(Update::RunActionReady { owner, operation }, window, cx)
            });
            assert!(shell
                .update(cx, |s, _| s.take_client_commands())
                .iter()
                .any(|c| matches!(c,Wire::CommitRunAction(op) if *op == operation)));
            assert!(d.before_write());
            let receipt = match kind {
                Kind::Edit => {
                    json!({"session_id":d.action.owner.session,"id":"last-user","content":d.action.text})
                }
                Kind::Retry => json!({"model":null,"original_session":null}),
                Kind::Good => json!({"run_id":"run-actions-r","rating":"good"}),
                Kind::Bad => json!({"run_id":"run-actions-r","rating":"bad"}),
                Kind::ProviderRetry => json!({"retrying":true}),
                Kind::Compact => json!({"started":true,"up_to":1}),
                Kind::Delete => json!({"deleted":true,"undo_secs":30}),
                Kind::Cwd => json!({"id":d.action.owner.session,"cwd":null}),
            };
            d.finish(Ok(receipt), true);
            window.render_frame(cx);
            window.click("run-action-close", cx);
            window.render_frame(cx);
            assert!(window.try_find("run-action-dialog").is_none());
        }
    });
}

#[gpui_kit::test]
fn cancelled_stale_or_disconnected_action_reviews_cannot_commit(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Keep my unsent draft", cx);
        let (owner, operation) = open_action(&shell, Kind::Delete, window, cx);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActionTarget {
                    owner: owner.clone(),
                    operation: uuid::Uuid::new_v4(),
                    result: Ok(
                        Target::decode(&owner.session, &value(&owner.session, false)).unwrap(),
                    ),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("run-action-prepare").disabled(), Some(true));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActionTarget {
                    owner: owner.clone(),
                    operation,
                    result: Ok(
                        Target::decode(&owner.session, &value(&owner.session, false)).unwrap(),
                    ),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click("run-action-prepare", cx);
        let d = shell
            .update(cx, |s, _| s.take_client_commands())
            .into_iter()
            .find_map(|c| {
                if let Wire::PrepareRunAction(d) = c {
                    Some(d)
                } else {
                    None
                }
            })
            .unwrap();
        window.render_frame(cx);
        window.press("escape", cx);
        assert!(!d.reserved());
        let (new_owner, new_op) = open_action(&shell, Kind::Delete, window, cx);
        assert_ne!(operation, new_op);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActionReview {
                    owner,
                    operation,
                    quote: None,
                    reviewed_text: None,
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::RunActionTarget {
                    owner: new_owner.clone(),
                    operation: new_op,
                    result: Ok(Target::decode(
                        &new_owner.session,
                        &value(&new_owner.session, false),
                    )
                    .unwrap()),
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(window.find("run-action-confirm").disabled(), Some(true));
        assert_eq!(window.find("run-action-prepare").disabled(), Some(true));
        window.click("run-action-close", cx);
        window.render_frame(cx);
        assert!(shell
            .read(cx)
            .composer()
            .read(cx)
            .value()
            .contains("Keep my unsent draft"));
        assert!(!shell
            .update(cx, |s, _| s.take_client_commands())
            .iter()
            .any(|c| matches!(c, Wire::CommitRunAction(_))));
    });
}

#[gpui_kit::test]
fn delete_undo_is_explicit_expires_and_is_cleared_on_disconnect(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        let (owner, _) = open_action(&shell, Kind::Delete, window, cx);
        window.render_frame(cx);
        window.click("run-action-close", cx);
        let ticket = formless_desktop::run_actions::DeleteUndo {
            owner,
            operation: uuid::Uuid::new_v4(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
        };
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::DeleteUndo(ticket.clone()), window, cx)
        });
        window.render_frame(cx);
        window.click("session-delete-undo", cx);
        assert!(shell
            .update(cx, |s, _| s.take_client_commands())
            .iter()
            .any(|c| matches!(c, Wire::UndoDelete(op) if *op == ticket.operation)));
        window.render_frame(cx);
        assert!(window.try_find("session-delete-undo").is_none());
        let mut expired = ticket.clone();
        expired.deadline = std::time::Instant::now();
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::DeleteUndo(expired), window, cx)
        });
        window.render_frame(cx);
        assert!(window.try_find("session-delete-undo").is_none());
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::DeleteUndo(ticket), window, cx);
            s.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert!(window.try_find("session-delete-undo").is_none());
    });
}
#[gpui_kit::test]
fn measured_run_context_survives_estimates_errors_and_resets_for_next_run(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        let (owner, _) = open_action(&shell, Kind::Cwd, window, cx);
        window.render_frame(cx);
        window.click("run-action-close", cx);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActivity {
                    owner: owner.clone(),
                    event: formless_protocol::Event::RunUsage {
                        session_id: owner.session.clone(),
                        run_id: "r".into(),
                        usage: formless_protocol::UsageInfo {
                            context_used: 75,
                            context_tokens: 100,
                            ..Default::default()
                        },
                    },
                },
                window,
                cx,
            )
        });
        for result in [
            Ok(ContextUsage {
                used: 12,
                limit: 100,
                measured: false,
            }),
            Err("unavailable".into()),
        ] {
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::LiveContext {
                        owner: owner.clone(),
                        result,
                    },
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            let label = window.find("status-context").value().unwrap().to_string();
            assert!(label.contains("75%") && label.contains("measured"));
        }
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActivity {
                    owner: owner.clone(),
                    event: formless_protocol::Event::CompactionNotice {
                        session_id: owner.session.clone(),
                        summary_preview: "Compacted history".into(),
                    },
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::LiveContext {
                    owner: owner.clone(),
                    result: Err("unavailable".into()),
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::LiveContext {
                    owner: owner.clone(),
                    result: Ok(ContextUsage {
                        used: 18,
                        limit: 100,
                        measured: false,
                    }),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        let label = window.find("status-context").value().unwrap().to_string();
        assert!(label.contains("18%") && label.contains("estimated"));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActivity {
                    owner: owner.clone(),
                    event: formless_protocol::Event::RunStarted {
                        session_id: owner.session.clone(),
                        run_id: "next".into(),
                    },
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::LiveContext {
                    owner,
                    result: Ok(ContextUsage {
                        used: 12,
                        limit: 100,
                        measured: false,
                    }),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        let label = window.find("status-context").value().unwrap().to_string();
        assert!(label.contains("12%") && label.contains("estimated"));
    });
}
#[gpui_kit::test]
fn context_provider_and_compaction_notices_are_live_and_selection_guarded(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        select(&shell, &sessions[0].id, window, cx);
        let (owner, _) = open_action(&shell, Kind::Compact, window, cx);
        window.render_frame(cx);
        window.click("run-action-close", cx);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::LiveContext {
                    owner: owner.clone(),
                    result: Ok(ContextUsage {
                        used: 50,
                        limit: 100,
                        measured: false,
                    }),
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::RunActivity {
                    owner: owner.clone(),
                    event: formless_protocol::Event::ProviderStalled {
                        session_id: owner.session.clone(),
                        run_id: "r".into(),
                        idle_secs: 120,
                    },
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert!(window
            .find("status-context")
            .value()
            .unwrap()
            .contains("50%"));
        assert!(window
            .find("provider-activity")
            .value()
            .unwrap()
            .contains("stalled"));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActivity {
                    owner: owner.clone(),
                    event: formless_protocol::Event::ProviderRetrying {
                        session_id: owner.session.clone(),
                        run_id: "r".into(),
                        attempt: 1,
                        max_attempts: 3,
                        wait_secs: 2,
                        reason: "temporary error".into(),
                    },
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("provider-activity")
            .value()
            .unwrap()
            .contains("1/3"));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::RunActivity {
                    owner: owner.clone(),
                    event: formless_protocol::Event::CompactionNotice {
                        session_id: owner.session.clone(),
                        summary_preview: "Checkpoint decisions".into(),
                    },
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("live-run-usage")
            .value()
            .unwrap()
            .contains("Checkpoint decisions"));
        assert!(window.try_find("status-context").is_none());
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::LiveContext {
                    owner: owner.clone(),
                    result: Ok(ContextUsage {
                        used: 50,
                        limit: 100,
                        measured: false,
                    }),
                },
                window,
                cx,
            )
        });
        let mut stale = owner.clone();
        stale.generation += 1;
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::LiveContext {
                    owner: stale,
                    result: Ok(ContextUsage {
                        used: 99,
                        limit: 100,
                        measured: true,
                    }),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("status-context")
            .value()
            .unwrap()
            .contains("50%"));
        select(&shell, &sessions[1].id, window, cx);
        window.render_frame(cx);
        assert!(window.try_find("status-context").is_none());
        assert!(window.try_find("provider-activity").is_none());
    });
}
