//! Headless GPUI Kit tests of the Phase 0 shell.
//!
//! These drive the real `Shell` view in the kit's test window: real hit
//! testing, key dispatch, focus and layout. They do not prove pixels, OS IME
//! behaviour, a screen reader, or anything about a Daemon Core.
//!
//! The test window has no real fonts. Its text system gives every character
//! the same width (0.6 em), so text here wraps and sizes by character count.
//! Layout logic is real; glyph widths and where real text breaks are not.
//! Anything that depends on those is measured in `tests/render.rs`, which
//! runs on the platform text system.

#[path = "support/production_overlay.rs"]
mod production_overlay_tests;

#[path = "support/attachments.rs"]
mod attachment_tests;

use std::time::{Duration, Instant};

#[path = "support/archive.rs"]
mod archive_tests;
#[path = "support/comparison.rs"]
mod comparison_tests;
#[path = "support/exports.rs"]
mod export_tests;
#[path = "support/instructions.rs"]
mod instruction_tests;
#[path = "support/invocations.rs"]
mod invocation_tests;
#[path = "support/preferences.rs"]
mod preference_tests;
#[path = "support/profile_search.rs"]
mod profile_search_tests;
#[path = "support/questions.rs"]
mod question_tests;
#[path = "support/reading_positions.rs"]
mod reading_position_tests;
#[path = "support/run_actions.rs"]
mod run_action_tests;
#[path = "support/run_inspection.rs"]
mod run_inspection_tests;
#[path = "support/session_edit.rs"]
mod session_edit_tests;
#[path = "support/session_fork.rs"]
mod session_fork_tests;
#[path = "support/usage.rs"]
mod usage_tests;

use formless_desktop::commands::{self, Command};
use formless_desktop::fixture;
use formless_desktop::memory::{limits, IntentKind, IntentRecord};
use formless_desktop::model::{tokens, BlockKind, Destination, Fit, TextScale};
use formless_desktop::replies;
use formless_desktop::shell::{
    self, directory_row_id, search_row_id, session_row_id, PageSection, Shell,
};
use formless_desktop::timeline::{self, TimelineBlock};
use formless_protocol::SendMode;
use gpui_kit::base::test_support::texts;
use gpui_kit::base::Root;
use gpui_kit::component::Theme;
use gpui_kit::test::{TestInput, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::EntityInputHandler as _;
use gpui_kit::{
    point, px, size, App, AppContext as _, Bounds, ClipboardItem, ElementInputHandler, Entity,
    InputEvent as _, MouseMoveEvent, Pixels, Point, Role, ScrollDelta, ScrollEvent, Size,
    TestAppContext, Window, WindowBounds, WindowHandle, WindowOptions,
};

const WIDE: Size<Pixels> = Size {
    width: px(1440.),
    height: px(900.),
};

// Tests of the serialized view boundary. The recording client performs no
// I/O or acknowledgements; real transport is exercised by tests/daemon.rs.
fn daemon_view(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
) -> Vec<formless_protocol::SessionInfo> {
    let sessions = fixture::sessions(3);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.connect_recording_daemon(cx);
            shell.apply_client_update(
                formless_desktop::client::Update::Connection {
                    ready: true,
                    notice: "Connected fixture boundary".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(
                formless_desktop::client::Update::Sessions(sessions.clone()),
                window,
                cx,
            );
        });
    });
    sessions
}

#[gpui_kit::test]
fn desktop_theme_keeps_primary_tokens_in_sync_in_both_modes(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |window, cx| {
        for dark in [false, true] {
            let theme = Theme::global(cx);
            assert_eq!(theme.is_dark(), dark);
            assert_eq!(theme.table_head, theme.muted);
            assert_eq!(theme.table_head_foreground, theme.foreground);
            assert_eq!(theme.tokens.button_primary.color, theme.primary);
            assert_eq!(theme.tokens.button_primary_hover.color, theme.primary_hover);
            shell.update(cx, |shell, cx| {
                shell.run_command(Command::ToggleTheme, window, cx)
            });
        }
    });
}

#[gpui_kit::test]
fn sidebar_empty_state_creates_only_when_connected(cx: &mut TestAppContext) {
    use formless_desktop::client::{Command as WireCommand, Update};
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("sessions-empty-title").visible());
        assert_eq!(window.find("sidebar-new-session").disabled(), Some(true));
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        window.render_frame(cx);
        assert_eq!(window.find("sidebar-new-session").disabled(), Some(true));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "Connected".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("sidebar-new-session").disabled(), None);
        window.click("sidebar-new-session", cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], WireCommand::CreateSession));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("sidebar-new-session").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn daemon_view_revalidates_queued_snapshot_generations_and_disables_stale_sends(
    cx: &mut TestAppContext,
) {
    use formless_desktop::client::Update;
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let generation = shell.read(cx).client_generation();
        let snapshot = || {
            replies::session_snapshot(&id, &serde_json::json!({"session":{"id":id}, "running":false,"messages":[typed_message(&id, 1, "assistant", "canonical desktop")]})).unwrap()
        };
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation: generation - 1,
                    snapshot: snapshot(),
                },
                window,
                cx,
            )
        });
        assert_eq!(shell.read(cx).block_count(), 0);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: snapshot(),
                },
                window,
                cx,
            )
        });
        assert_eq!(shell.read(cx).block_count(), 1);
        window.render_frame(cx);
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.input("unsent ไทย 😀", cx);
        window.render_frame(cx);
        assert_eq!(window.find("composer-primary").disabled(), None);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SnapshotError {
                    generation,
                    session_id: id.clone(),
                    error: "stale Session fixture".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent ไทย 😀");
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: snapshot(),
                },
                window,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected; unknown Run state".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        assert_eq!(shell.read(cx).block_count(), 1);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent ไทย 😀");
    });
}

fn daemon_quote(
    shell: &Entity<Shell>,
    cx: &App,
    required: bool,
) -> Box<formless_desktop::forecast::Prepared> {
    let state = shell.read(cx);
    let session_id = state.sidebar().active_id().unwrap().to_string();
    Box::new(formless_desktop::forecast::Prepared {
        client: state.client_identity().unwrap().0,
        connection: state.client_identity().unwrap().1,
        attachments: Some(
            formless_desktop::pending_files::Report::decode(
                &session_id,
                serde_json::json!({"session_id":session_id,"attachments":[]}),
            )
            .unwrap(),
        ),
        id: uuid::Uuid::new_v4(),
        epoch: state.client_epoch().unwrap(),
        generation: state.client_generation(),
        quote: fixture::run_forecast(&session_id, required),
        session_id,
        label: "Send",
        draft_basis: state.draft(cx).to_string(),
    })
}

#[gpui_kit::test]
fn native_forecast_requires_ack_and_current_draft_before_commit(cx: &mut TestAppContext) {
    use formless_desktop::client::{Command as WireCommand, Update};
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = sessions[0].id.clone();
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: replies::session_snapshot(
                        &id,
                        &serde_json::json!({"session":{"id":id},"running":false,"messages":[]}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            shell.focus_section(PageSection::Composer, window, cx);
            shell.take_client_commands();
        });
        window.input("confirm this ไทย 😀", cx);
        window.render_frame(cx);
        window.click("composer-primary", cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(commands.iter().any(|command| matches!(command, WireCommand::Review { intent: formless_desktop::model::Intent::SendMessage { content, .. }, draft_basis, .. } if content == "confirm this ไทย 😀" && content == draft_basis)));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, WireCommand::Intent(_))));
        let prepared = daemon_quote(&shell, cx, true);
        let action = prepared.id;
        let epoch = prepared.epoch;
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Prepared(prepared.clone()), window, cx)
        });
        window.render_frame(cx);
        assert!(shell.read(cx).background_inert(window));
        assert_eq!(window.find("forecast-cancel").focused(), Some(true));
        window.press("f6", cx);
        window.render_frame(cx);
        assert_eq!(window.find("forecast-cancel").focused(), Some(true));
        window.click("forecast-copy", cx);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &cx.read_from_clipboard().unwrap().text().unwrap()
            )
            .unwrap(),
            serde_json::from_str::<serde_json::Value>(&prepared.details()).unwrap()
        );
        window.click("forecast-confirm", cx);
        window.render_frame(cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(commands
            .iter()
            .any(|command| matches!(command, WireCommand::ConfirmPrepared(got) if *got == action)));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, WireCommand::CommitPrepared(_))));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Prepared(prepared.clone()), window, cx);
            shell.apply_client_update(
                Update::Action {
                    session_id: id.clone(),
                    result: Ok(()),
                },
                window,
                cx,
            );
            shell.apply_client_update(
                Update::PreparationReady {
                    id: uuid::Uuid::new_v4(),
                    epoch,
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(window.find("forecast-confirm").disabled(), Some(true));
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::PreparationReady { id: action, epoch }, window, cx)
        });
        window.render_frame(cx);
        assert!(window.try_find("forecast-dialog").is_none());
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::CommitPrepared(got) if *got == action)));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "confirm this ไทย 😀");
    });
}

#[gpui_kit::test]
fn native_forecast_cancel_and_late_replies_cannot_commit_changed_input(cx: &mut TestAppContext) {
    use formless_desktop::client::{Command as WireCommand, Update};
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let old = step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = sessions[0].id.clone();
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: replies::session_snapshot(
                        &id,
                        &serde_json::json!({"session":{"id":id},"running":false,"messages":[]}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            shell.focus_section(PageSection::Composer, window, cx);
        });
        window.input("keep me", cx);
        window.render_frame(cx);
        let old = daemon_quote(&shell, cx, true);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Prepared(old.clone()), window, cx)
        });
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("forecast-dialog").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_ne!(shell.read(cx).client_epoch(), Some(old.epoch));
        old
    });
    cx.run_until_parked();
    let current = step(cx, handle, |window, cx| {
        let current = daemon_quote(&shell, cx, true);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Prepared(current.clone()), window, cx);
            shell.apply_client_update(Update::Prepared(old.clone()), window, cx);
            shell.apply_client_update(
                Update::PreparationReady {
                    id: old.id,
                    epoch: old.epoch,
                },
                window,
                cx,
            );
            shell.take_client_commands();
        });
        current
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("forecast-dialog").is_some(),
            "old updates must not cancel a newer dialog"
        );
        shell.update(cx, |shell, cx| {
            let composer = shell.composer().clone();
            composer.update(cx, |composer, cx| {
                composer.set_value("changed before observer", window, cx)
            });
            shell.apply_client_update(
                Update::PreparationReady {
                    id: current.id,
                    epoch: current.epoch,
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::CommitPrepared(_))));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "changed before observer");
    });
}

#[gpui_kit::test]
fn native_forecast_rechecks_marked_text_before_quote_or_consent_commit(cx: &mut TestAppContext) {
    use formless_desktop::client::{Command as WireCommand, Update};
    for consent_ack in [false, true] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        let sessions = daemon_view(cx, handle, &shell);
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            window.click(session_row_id(&sessions[0].id), cx);
            let id = sessions[0].id.clone();
            let generation = shell.read(cx).client_generation();
            shell.update(cx, |shell, cx| {
                shell.apply_client_update(
                    Update::Snapshot {
                        generation,
                        snapshot: replies::session_snapshot(
                            &id,
                            &serde_json::json!({"session":{"id":id},"running":false,"messages":[]}),
                        )
                        .unwrap(),
                    },
                    window,
                    cx,
                );
                shell.focus_section(PageSection::Composer, window, cx);
                shell.take_client_commands();
            });
            window.input("かな", cx);
            window.render_frame(cx);
            let prepared = daemon_quote(&shell, cx, consent_ack);
            if consent_ack {
                shell.update(cx, |shell, cx| {
                    shell.apply_client_update(Update::Prepared(prepared.clone()), window, cx)
                });
                window.render_frame(cx);
                window.click("forecast-confirm", cx);
            }
            // Direct input-handler delivery tests the ordering before the
            // composer's deferred observer; it is not OS IME acceptance.
            let bounds = window.find_by_label("Message").bounds();
            let composer = shell.read(cx).composer().clone();
            TestInput::new(ElementInputHandler::new(bounds, composer), window).compose_text(
                Some(0..2),
                "かな",
                None,
                cx,
            );
            assert_eq!(shell.read(cx).draft(cx).as_ref(), "かな");
            shell.update(cx, |shell, cx| {
                if consent_ack {
                    shell.apply_client_update(
                        Update::PreparationReady {
                            id: prepared.id,
                            epoch: prepared.epoch,
                        },
                        window,
                        cx,
                    );
                } else {
                    shell.apply_client_update(Update::Prepared(prepared), window, cx);
                }
            });
            assert!(!shell
                .update(cx, |shell, _| shell.take_client_commands())
                .iter()
                .any(|command| matches!(command, WireCommand::CommitPrepared(_))));
        });
    }
}

#[gpui_kit::test]
fn recovery_uses_current_snapshot_retains_draft_and_rejects_stale_commands(
    cx: &mut TestAppContext,
) {
    use formless_desktop::client::Update;
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = sessions[0].id.clone();
        let generation = shell.read(cx).client_generation();
        let recovery = || {
            replies::session_snapshot(&id, &serde_json::json!({
            "session":{"id":id}, "running":false, "messages":[],
            "unfinished_run":{"run_id":"interrupted-run","status":"interrupted","error":"Stopped before completion"}
        })).unwrap()
        };
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: recovery(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("run-resume").label(), Some("Resume Run"));
        assert_eq!(window.find("run-resume").disabled(), None);
        window.click_at("region-composer", point(px(40.), px(220.)), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("keep this draft ไทย", cx);
        let draft = shell.read(cx).draft(cx).to_string();
        assert_eq!(draft, "keep this draft ไทย");
        window.render_frame(cx);
        window.click("run-resume", cx);
        assert_eq!(
            shell.read(cx).last_intent().unwrap().kind,
            IntentKind::Resume
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SnapshotError {
                    generation,
                    session_id: id.clone(),
                    error: "disconnected".into(),
                },
                window,
                cx,
            )
        });
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(
                Command::ResumeRun,
                window,
                cx
            )),
            Some(commands::NO_RECOVERY)
        );
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(
                Command::AbandonRun,
                window,
                cx
            )),
            Some(commands::NO_RECOVERY)
        );
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: recovery(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click("run-abandon", cx);
        assert_eq!(
            shell.read(cx).last_intent().unwrap().kind,
            IntentKind::Abandon
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        window.click(session_row_id(&sessions[1].id), cx);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: recovery(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(
            window.try_find("run-recovery").is_none(),
            "a retired Session cannot restore recovery controls"
        );
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(
                Command::ResumeRun,
                window,
                cx
            )),
            Some(commands::NO_RECOVERY)
        );
    });
}

#[gpui_kit::test]
fn daemon_resources_are_keyboard_browsable_and_copy_exact_json(cx: &mut TestAppContext) {
    use formless_desktop::client::Update;
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let value = serde_json::json!([{"id":"first","name":"Tool A"},{"id":"second","name":"Tool B","description":"ไทย 😀"}]);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Tools,
                    result: Ok(value.clone()),
                },
                window,
                cx,
            );
            shell.run_command(Command::GoTo(Destination::Tools), window, cx);
            shell.focus_section(PageSection::Conversation, window, cx);
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("region-conversation").label(),
            Some("Tools · daemon data")
        );
        window.press("down", cx);
        window.render_frame(cx);
        assert_eq!(window.find("resource-selected").value(), Some("Tool B"));
        window.click("copy-resource", cx);
        let copied = cx.read_from_clipboard().unwrap().text().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&copied).unwrap(),
            value[1]
        );
        for _ in 0..3 {
            shell.update(cx, |shell, cx| {
                shell.run_command(Command::GoTo(Destination::Settings), window, cx);
                shell.run_command(Command::GoTo(Destination::Tools), window, cx);
            });
            window.render_frame(cx);
            assert!(window.find("resource-back").visible());
        }
    });
}

#[gpui_kit::test]
fn the_fitted_footer_wraps_status_text_at_200_percent(cx: &mut TestAppContext) {
    let size = size(px(1100.), px(720.));
    let (handle, shell) = open(cx, size, 4, 120);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).fit(window), Fit::Whole);
        let footer = window.find("status").bounds();
        let fixture = window.find("status-fixture").bounds();
        let detail = window.find("status-detail").bounds();
        for id in ["status-fixture", "status-intent", "status-detail"] {
            let bounds = window.find(id).bounds();
            assert!(
                bounds.left() >= footer.left() && bounds.right() <= footer.right(),
                "{id}: {bounds:?}"
            );
            assert!(
                bounds.top() >= footer.top() && bounds.bottom() <= footer.bottom(),
                "{id}: {bounds:?}"
            );
        }
        assert!(detail.top() >= fixture.bottom());
        assert!(footer.bottom() <= size.height);
        assert!(window.find_by_label("Message").visible());
    });
}

#[gpui_kit::test]
fn daemon_resource_error_and_status_reflow_at_200_percent(cx: &mut TestAppContext) {
    use formless_desktop::client::Update;
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx);
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Metrics,
                    result: Err("Metrics unavailable".into()),
                },
                window,
                cx,
            );
            shell.run_command(Command::GoTo(Destination::Metrics), window, cx);
            shell.focus_section(PageSection::Conversation, window, cx);
        });
        settle_frames(window, cx);
        assert!(window
            .find("resource-notice")
            .value()
            .unwrap()
            .contains("Metrics unavailable"));
        for id in ["region-conversation", "resource-notice", "resource-detail"] {
            assert!(window.find(id).bounds().size.width <= px(480.), "{id}");
        }
        for id in ["refresh-resource", "resource-back"] {
            let bounds = window.find(id).bounds();
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= px(480.),
                "{id}: {bounds:?}"
            );
        }
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Status, window, cx)
        });
        settle_frames(window, cx);
        assert!(shell.read(cx).page_metrics().is_inside(PageSection::Status));
    });
}

#[gpui_kit::test]
fn daemon_attachment_clears_a_previously_populated_virtual_sidebar(cx: &mut TestAppContext) {
    use formless_desktop::client::{Endpoint, Update};
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.scroll(
            "session-list",
            ScrollDelta::Pixels(point(px(0.), px(-4000.))),
            cx,
        );
        window.render_frame(cx);
        shell.update(cx, |shell, cx| {
            shell.connect_daemon(
                Endpoint(std::env::temp_dir().join(format!("absent-{}", uuid::Uuid::new_v4()))),
                cx,
            )
        });
        settle_frames(window, cx);
        assert!(shell.read(cx).sidebar().rows().is_empty());
        assert_eq!(window.find("session-count").value(), Some("Sessions (0)"));
        let sessions = fixture::sessions(3);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx)
        });
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).sidebar().rows().len(), 3);
        assert!(sessions
            .iter()
            .any(|session| window.try_find(session_row_id(&session.id)).is_some()));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(Vec::new()), window, cx)
        });
        settle_frames(window, cx);
        assert_eq!(window.find("session-count").value(), Some("Sessions (0)"));
    });
}

#[gpui_kit::test]
fn daemon_attention_survives_navigation_and_rejects_retired_updates(cx: &mut TestAppContext) {
    use formless_desktop::client::Update;
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx);
            shell.apply_client_update(
                Update::Attention {
                    generation,
                    pending: true,
                },
                window,
                cx,
            );
            shell.run_command(Command::GoTo(Destination::Tools), window, cx);
            shell.apply_client_update(
                Update::Attention {
                    generation: generation + 1,
                    pending: false,
                },
                window,
                cx,
            );
            shell.focus_section(PageSection::Status, window, cx);
        });
        settle_frames(window, cx);
        assert!(window
            .find("status-pending-attention")
            .value()
            .unwrap()
            .contains("Formless TUI"));
        assert!(shell.read(cx).page_metrics().is_inside(PageSection::Status));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Attention {
                    generation,
                    pending: false,
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("status-pending-attention").is_none());
    });
}

fn open(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    sessions: usize,
    blocks: usize,
) -> (WindowHandle<Root>, Entity<Shell>) {
    open_with(cx, window_size, sessions, blocks, true)
}

/// `with_running: false` leaves every fixture Session idle, which changes
/// how the sidebar groups and orders them.
fn open_with(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    sessions: usize,
    blocks: usize,
    with_running: bool,
) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);
        let sessions = fixture::sessions(sessions);
        let running = if with_running {
            fixture::running(&sessions)
        } else {
            Default::default()
        };
        let blocks = fixture::timeline(blocks);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: window_size,
            })),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx))
        })
        .expect("open test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    })
}

#[cfg(target_os = "macos")]
const TOGGLE_SIDEBAR: &str = "cmd-b";
#[cfg(not(target_os = "macos"))]
const TOGGLE_SIDEBAR: &str = "ctrl-b";
#[cfg(target_os = "macos")]
const TOGGLE_INSPECTOR: &str = "cmd-alt-b";
#[cfg(not(target_os = "macos"))]
const TOGGLE_INSPECTOR: &str = "ctrl-alt-b";

/// One window update. Events a control emits (a composer Enter, for one) are
/// delivered when the update ends, so an assertion about their effect belongs
/// in the next step.
fn step<R>(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    update: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(handle.into(), |_, window, cx| update(window, cx))
        .expect("shell window is open")
}

#[gpui_kit::test]
fn wide_window_docks_three_panes_at_their_token_widths(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let sidebar = window.find("region-sidebar").bounds();
        let conversation = window.find("region-conversation").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert_eq!(sidebar.size.width, px(tokens::SIDEBAR_DEFAULT));
        assert_eq!(inspector.size.width, px(tokens::INSPECTOR_DEFAULT));
        assert_eq!(sidebar.left(), px(0.));
        assert_eq!(conversation.left(), sidebar.right());
        assert_eq!(inspector.left(), conversation.right());
        assert_eq!(inspector.right(), px(1440.));
        assert!(window.find("header").visible());
        assert!(window.find("status").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn narrowing_hides_the_inspector_until_asked_then_collapses_the_sidebar(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);

    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Narrowing alone puts nothing over the conversation or its controls.
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.find("region-sidebar").visible());
        assert_eq!(
            window.find("region-conversation").bounds().right(),
            px(1100.)
        );

        // Asked for, it comes up as an overlay: the conversation keeps its
        // width and the inspector covers its right edge.
        window.click("toggle-inspector", cx);
        let conversation = window.find("region-conversation").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert_eq!(conversation.right(), px(1100.));
        assert_eq!(inspector.right(), px(1100.));
        assert!(inspector.left() < conversation.right());
        // The control that dismisses it stays outside the overlay.
        let toggle = window.find("toggle-inspector").bounds();
        assert!(toggle.bottom() <= inspector.top());
        window.click("toggle-inspector", cx);
        assert!(window.try_find("region-inspector").is_none());
    });

    cx.simulate_window_resize(handle.into(), size(px(720.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find("region-conversation").bounds().left(), px(0.));
        assert!(window.find("composer-primary").visible());
    });

    // Widening restores the docked inspector: that preference never changed.
    cx.simulate_window_resize(handle.into(), WIDE);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("region-inspector").visible());
        assert!(window.find("region-sidebar").visible());
    });
}

#[gpui_kit::test]
fn composer_takes_thai_cjk_and_emoji_and_keeps_the_draft_after_a_recorded_send(
    cx: &mut TestAppContext,
) {
    let text = "สวัสดีครับ 日本語のテスト 한국어 👩🏽‍💻";
    let draft = format!("{text}\nsecond line");
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The composer has focus from the start, so typing needs no click.
        let composer = window.find_by_label("Message");
        assert_eq!(composer.focused(), Some(true));
        assert_eq!(composer.role(), Some(Role::MultilineTextInput));
        // Nothing to send yet.
        window.press("enter", cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).last_intent(), None);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");

        window.input(text, cx);
        assert_eq!(window.find_by_label("Message").value(), Some(text));
        window.press("shift-enter", cx);
        window.input("second line", cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        assert_eq!(shell.read(cx).last_intent(), None);
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(
            shell.read(cx).last_intent(),
            // The record has the sizes of what was sent and none of its text.
            Some(&IntentRecord {
                kind: IntentKind::Send(SendMode::Send),
                // The pinned fixture Session sorts first and opens at start.
                session_id: "fixture-session-3".to_string(),
                content_bytes: draft.len(),
                content_chars: draft.chars().count(),
            })
        );
        // Nothing acknowledged the request, so the draft is still there.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
    });
}

#[gpui_kit::test]
fn a_running_session_offers_steer_queue_and_a_separate_stop(cx: &mut TestAppContext) {
    #[cfg(target_os = "macos")]
    const SECONDARY_ENTER: &str = "cmd-enter";
    #[cfg(not(target_os = "macos"))]
    const SECONDARY_ENTER: &str = "ctrl-enter";

    let (handle, shell) = open(cx, WIDE, 200, 40);
    let last_mode = |cx: &App| match shell.read(cx).last_intent().map(|record| record.kind) {
        Some(IntentKind::Send(mode)) => Some(mode),
        _ => None,
    };
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Idle Session: Send only, Stop present but unavailable.
        assert_eq!(window.find("composer-primary").label(), Some("Send"));
        assert!(window.try_find("composer-secondary").is_none());
        let stop = window.find("composer-stop").bounds();
        window.click("composer-stop", cx);
        assert_eq!(shell.read(cx).last_intent(), None);

        // Fixture Session 7 is running.
        window.click(session_row_id("fixture-session-7"), cx);
        assert_eq!(window.find("composer-primary").label(), Some("Steer"));
        assert_eq!(window.find("composer-secondary").label(), Some("Queue"));
        // Stop did not move when the primary action changed.
        assert_eq!(window.find("composer-stop").bounds(), stop);

        window.click("composer-stop", cx);
        assert_eq!(
            shell.read(cx).last_intent(),
            Some(&IntentRecord {
                kind: IntentKind::Stop,
                session_id: "fixture-session-7".to_string(),
                content_bytes: 0,
                content_chars: 0,
            })
        );

        // With no draft the send controls do nothing.
        window.click("composer-primary", cx);
        window.click("composer-secondary", cx);
        assert_eq!(last_mode(cx), None);

        window.click_at("region-composer", gpui_kit::point(px(40.), px(20.)), cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.input("แก้ทิศทาง", cx);
        window.click("composer-secondary", cx);
        assert_eq!(last_mode(cx), Some(SendMode::Queue));
        window.click("composer-primary", cx);
        assert_eq!(last_mode(cx), Some(SendMode::Steer));

        window.click_at("region-composer", gpui_kit::point(px(40.), px(20.)), cx);
        window.press(SECONDARY_ENTER, cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(last_mode(cx), Some(SendMode::Queue));
        window.press("enter", cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(last_mode(cx), Some(SendMode::Steer));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "แก้ทิศทาง");
    });
}

#[gpui_kit::test]
fn f6_moves_between_regions_and_skips_a_closed_panel(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let focused = |window: &gpui_kit::Window, id: &'static str| window.find(id).focused();

        // Composer -> Context -> Sessions -> Conversation -> Composer.
        window.press("f6", cx);
        assert_eq!(focused(window, "region-inspector"), Some(true));
        window.press("f6", cx);
        assert_eq!(focused(window, "region-sidebar"), Some(true));
        window.press("f6", cx);
        assert_eq!(focused(window, "region-conversation"), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.press("shift-f6", cx);
        assert_eq!(focused(window, "region-conversation"), Some(true));

        // Closing the inspector removes it from the cycle.
        window.click("toggle-inspector", cx);
        assert!(window.try_find("region-inspector").is_none());
        window.press("shift-f6", cx);
        assert_eq!(focused(window, "region-sidebar"), Some(true));
        window.press("shift-f6", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn closing_a_focused_panel_returns_focus_to_the_composer(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("f6", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn arrows_preview_a_session_and_only_enter_opens_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("unsent draft", cx);
        window.press("f6", cx);
        window.press("f6", cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);

        window.press("down", cx);
        window.press("down", cx);
        assert_eq!(shell.read(cx).sidebar().selected_index(), Some(2));
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        // Previewing did not touch the draft or steal focus.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent draft");
        assert_eq!(window.find("region-sidebar").focused(), Some(true));

        window.press("enter", cx);
        let second = shell.read(cx).sidebar().rows()[2].id.clone();
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(second.as_str()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        assert_eq!(window.find("region-sidebar").focused(), Some(true));

        // Going back restores the first Session's draft.
        window.press("up", cx);
        window.press("up", cx);
        window.press("enter", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent draft");
    })
    .unwrap();
}

#[gpui_kit::test]
fn ten_thousand_sessions_and_blocks_materialize_only_what_is_near_the_viewport(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 10_000, 10_000);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let rows = shell.read(cx).sidebar().rows().len();
        assert!(rows > 9_000, "archived Sessions are filtered: {rows}");

        let observed = window.elements();
        let count = |name: &str| {
            observed
                .iter()
                .filter(|element| {
                    element
                        .path()
                        .last()
                        .is_some_and(|id| format!("{id:?}").contains(name))
                })
                .count()
        };
        let session_rows = count("session-row");
        let blocks = count("block");
        // Rows are identified by Session id, not by position.
        let row_id = |index: usize| session_row_id(&shell.read(cx).sidebar().rows()[index].id);
        assert!(window.find(row_id(0)).visible());
        assert!(window.try_find(row_id(5_000)).is_none());
        assert!(
            (1..=60).contains(&session_rows),
            "session rows: {session_rows}"
        );

        // The timeline follows its tail: the newest block is there, the
        // oldest was never built.
        assert!(window.find(("block", 10_000usize)).visible());
        assert!(window.try_find(("block", 1usize)).is_none());
        assert!((1..=60).contains(&blocks), "timeline blocks: {blocks}");
    })
    .unwrap();
}

#[gpui_kit::test]
fn markdown_paints_thai_cjk_and_right_to_left_text(cx: &mut TestAppContext) {
    // Tall enough for all eight fixture bodies at once.
    let (handle, _) = open(cx, size(px(1440.), px(2400.)), 20, 8);
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let painted: String = texts(window)
            .iter()
            .filter(|text| text.visible())
            .map(|text| text.text().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "ช่วยสรุปค่าใช้จ่ายของเดือนนี้ให้หน่อย",
            "了解しました。以下に要点をまとめます。",
            "收入",
            "items.iter().sum() // 合計 / ผลรวม",
            "تقرير.pdf",
            "👩🏽‍💻",
        ] {
            assert!(
                painted.contains(expected),
                "{expected:?} was not painted:\n{painted}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_long_unspaced_thai_paragraph_wraps_inside_a_narrow_conversation(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, size(px(520.), px(2400.)), 20, 8);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let conversation = window.find("region-conversation").bounds();
        assert_eq!(conversation.size.width, px(520.));
        let painted = texts(window);
        let paragraph = painted
            .iter()
            .find(|text| text.text().starts_with("ข้อความอ้างอิงภาษาไทย"))
            .expect("the Thai quote was painted");
        let one_line = painted
            .iter()
            .find(|text| text.text() == "block 8")
            .expect("a short single-line Markdown paragraph was painted")
            .bounds()
            .size
            .height;
        // It breaks onto further lines instead of overflowing or clipping.
        // Where it breaks is not asserted: GPUI 0.3.12 has no Thai
        // word-break model, so the break is not at a word boundary.
        assert!(paragraph.bounds().size.height > one_line * 1.5);
        assert!(paragraph.bounds().left() >= conversation.left());
        assert!(paragraph.bounds().right() <= conversation.right());
        assert!(paragraph.visible());
    });
}

#[gpui_kit::test]
fn a_session_row_keeps_its_element_id_at_a_different_position(cx: &mut TestAppContext) {
    // The same Sessions listed twice: with Session 7 running it sorts into
    // the Active group, idle it stays in Recent. The row is at a different
    // position in each list and is found under the same element id.
    let mut position = |with_running: bool| {
        // Tall enough that the row is materialized in either list.
        let (handle, shell) = open_with(cx, size(px(1440.), px(2400.)), 200, 8, with_running);
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            let index = shell
                .read(cx)
                .sidebar()
                .rows()
                .iter()
                .position(|row| row.id == "fixture-session-7")
                .expect("fixture session 7");
            let top = window
                .find(session_row_id("fixture-session-7"))
                .bounds()
                .top();
            (index, top)
        })
    };
    let (running_index, running_top) = position(true);
    let (idle_index, idle_top) = position(false);
    assert_ne!(running_index, idle_index);
    assert_ne!(running_top, idle_top);
}

#[gpui_kit::test]
fn the_inspector_overlay_is_modal_traps_focus_and_restores_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft under the overlay", cx);
        // Put focus somewhere other than the default before opening.
        window.press("shift-f6", cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.press(TOGGLE_INSPECTOR, cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert!(window.find("overlay-backdrop").visible());

        // F6 and Tab cannot leave the overlay.
        for key in ["f6", "shift-f6", "tab", "shift-tab", "tab"] {
            window.press(key, cx);
            assert_eq!(
                window.find("region-inspector").focused(),
                Some(true),
                "{key} left the overlay"
            );
        }

        // Send sits under the overlay panel: a click there reaches the panel,
        // not the button.
        let send = window.find("composer-primary").bounds();
        let inspector = window.find("region-inspector").bounds();
        assert!(send.right() > inspector.left());
        window.click("composer-primary", cx);
        assert_eq!(shell.read(cx).last_intent(), None);
        assert!(window.find("region-inspector").visible());

        // Escape dismisses it and focus goes back where it was.
        window.press("escape", cx);
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft under the overlay");

        // With the overlay gone the same click works.
        window.click("composer-primary", cx);
        assert!(shell.read(cx).last_intent().is_some());
    });
}

#[gpui_kit::test]
fn a_click_on_the_backdrop_dismisses_the_overlay_and_nothing_else(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.find("region-inspector").visible());
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);

        // The backdrop's top-left corner lies over the first Session row.
        window.click_at("overlay-backdrop", gpui_kit::point(px(40.), px(60.)), cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn the_sessions_toggle_opens_a_modal_sidebar_when_too_narrow_to_dock(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(720.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-sidebar").is_none());
        window.input("keep me", cx);

        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.focused(), Some(true));
        assert_eq!(sidebar.bounds().left(), px(0.));
        // It lies over the conversation, which keeps the full width.
        assert_eq!(window.find("region-conversation").bounds().left(), px(0.));
        assert!(window.find("overlay-backdrop").visible());

        // Arrows preview inside the overlay; Escape closes without opening.
        let opened = shell.read(cx).sidebar().active_id().map(str::to_string);
        window.press("down", cx);
        window.press("escape", cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert_eq!(shell.read(cx).sidebar().active_id(), opened.as_deref());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));

        // Enter opens the selected Session, closes the overlay and returns
        // focus to the composer it came from.
        window.press(TOGGLE_SIDEBAR, cx);
        window.press("down", cx);
        window.press("enter", cx);
        let selected = shell.read(cx).sidebar().rows()[2].id.clone();
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(selected.as_str())
        );
        assert!(window.try_find("region-sidebar").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");

        // The header toggle works too, and closes what it opened.
        window.click("toggle-sidebar", cx);
        assert!(window.find("region-sidebar").visible());
        window.click("toggle-sidebar", cx);
        assert!(window.try_find("region-sidebar").is_none());
    });
}

#[gpui_kit::test]
fn widening_docks_an_open_overlay_and_narrowing_again_does_not_reopen_it(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        assert!(window.find("overlay-backdrop").visible());
    });

    cx.simulate_window_resize(handle.into(), WIDE);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Docked now: no backdrop, and focus is back in a visible region.
        assert!(window.try_find("overlay-backdrop").is_none());
        assert!(window.find("region-inspector").visible());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });

    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
    });
}

#[gpui_kit::test]
fn other_destinations_show_a_scaffold_notice_instead_of_the_conversation(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("still here", cx);

        window.click("destination-Tools", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Tools);
        // No conversation, no composer: nothing that looks like a Tools page.
        assert!(window.try_find("region-composer").is_none());
        assert!(window.try_find("composer-primary").is_none());
        assert!(window.try_find(("block", 40usize)).is_none());
        assert!(window.find("back-to-chat").visible());
        // Focus left the composer for the region that replaced it.
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.draw(cx).clear(cx);
        let painted: Vec<String> = texts(window)
            .iter()
            .filter(|text| text.visible())
            .map(|text| text.text().to_string())
            .collect();
        assert!(
            painted.iter().any(|text| text == "Tools: not built"),
            "{painted:?}"
        );
        assert!(painted.iter().any(|text| text.contains(
            "This Phase 0 scaffold has Chat, fixture Sessions and loaded-text Search only"
        )));

        // The region cycle skips the composer that is not there.
        window.press("f6", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        window.press("f6", cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));

        window.click("back-to-chat", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert!(window.find("region-composer").visible());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "still here");
    });
}

/// Press Tab until `id` holds keyboard focus.
fn tab_to(window: &mut Window, id: &'static str, cx: &mut App) {
    for _ in 0..60 {
        if window.find(id).focused() == Some(true) {
            return;
        }
        window.press("tab", cx);
    }
    panic!("Tab never reached {id}");
}

// The pinned GPUI gives an element an accessibility node only if it has an
// id and a role. Read from a real macOS window, the platform tree held the
// buttons and the composer and none of the shell's text: no Session, no
// message, no status. These check what the shell now hands the platform
// adapter. They read the kit's test tree, not the platform's; the real
// window was read separately (see the findings).

#[gpui_kit::test]
fn the_shells_text_and_rows_are_in_the_accessibility_tree(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let rows = shell.read(cx).sidebar().rows().to_vec();
        let active = &rows[0];

        // The header names the open Session, whole, as a heading.
        let title = window.find("header-title");
        assert_eq!(title.role(), Some(Role::Heading));
        assert_eq!(title.label(), Some(active.title.as_str()));
        assert_eq!(
            window.find("header-profile").value(),
            Some("Fixture Profile /")
        );

        // The Sessions list is a list of options. Each is named by its
        // title, read with its status, knows whether it is selected and
        // where it is among all of them, and can be pressed.
        let list = window.find("session-list");
        assert_eq!(list.role(), Some(Role::ListBox));
        assert_eq!(list.label(), Some("Sessions"));
        assert_eq!(
            window.find("session-count").value(),
            Some(format!("Sessions ({})", rows.len()).as_str())
        );
        let first = window.find(session_row_id(&active.id));
        assert_eq!(first.role(), Some(Role::ListBoxOption));
        assert_eq!(first.label(), Some(active.title.as_str()));
        let value = first.value().expect("a row has a value").to_string();
        assert!(value.starts_with(&active.title), "{value}");
        assert!(value.ends_with("· Open"), "{value}");
        assert_eq!(first.selected(), Some(true));
        let second = window.find(session_row_id(&rows[1].id));
        assert_eq!(second.selected(), Some(false));
        assert!(!second.value().unwrap().contains("Open"));

        // The conversation is the main landmark; its heading line is text.
        let conversation = window.find("region-conversation");
        assert_eq!(conversation.role(), Some(Role::Main));
        assert_eq!(conversation.label(), Some("Conversation"));
        assert_eq!(window.find("block-count").value(), Some("40 blocks"));
        assert_eq!(window.find("run-status").value(), Some("Run: Idle"));

        // Each block on screen is an article named by who it is from, and
        // holds the text its row lays out.
        let block = window.find(("block", 40usize));
        assert_eq!(block.role(), Some(Role::Article));
        assert_eq!(block.label(), Some("Run recap"));
        let text = window.find(("block-text", 40usize));
        assert_eq!(text.role(), Some(Role::Label));
        assert_eq!(
            text.value(),
            Some(
                // The text a reader sees, not its markdown source.
                "Done. 3 files changed, 2 Tools created, 1 Invocation still pending.\nblock 40"
            )
        );

        // The Context panel's details, each as one line.
        assert_eq!(window.find("inspector-title").value(), Some("Context"));
        assert_eq!(
            window.find("inspector-session").value(),
            Some(format!("Session: {}", active.title).as_str())
        );
        assert_eq!(
            window.find("inspector-session-id").value(),
            Some(format!("Session id: {}", active.id).as_str())
        );
        assert_eq!(window.find("inspector-run").value(), Some("Run: Idle"));
        assert!(window
            .find("inspector-last-message")
            .value()
            .is_some_and(|value| value.starts_with("Last message: ")));

        // The status line is a status region holding its three lines.
        let status = window.find("status");
        assert_eq!(status.role(), Some(Role::Status));
        assert_eq!(status.label(), Some("Status"));
        assert_eq!(
            window.find("status-fixture").value(),
            Some("Synthetic fixture: no Daemon Core connection")
        );
        assert_eq!(
            window.find("status-intent").value(),
            Some("No intent recorded")
        );
        assert_eq!(
            window.find("status-detail").value(),
            Some("Text 100% · Chat · inspector docked")
        );

        // A value alone is not enough: an element without a role is not
        // in the platform's tree at all, whatever else it carries.
        for id in [
            "header-profile",
            "session-count",
            "block-count",
            "run-status",
            "inspector-title",
            "inspector-session",
            "inspector-session-id",
            "inspector-run",
            "inspector-last-message",
            "status-fixture",
            "status-intent",
            "status-detail",
        ] {
            assert_eq!(window.find(id).role(), Some(Role::Label), "{id}");
        }

        // What the status line says changes with what was done.
        window.input("hello", cx);
        window.click("composer-primary", cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("status-intent").value(),
            Some("Recorded Send (5 characters), not sent")
        );

        // Pressing another Session's row opens it, and everything that
        // names the open Session follows.
        let rows = shell.read(cx).sidebar().rows().to_vec();
        window.click(session_row_id(&rows[1].id), cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("header-title").label(),
            Some(rows[1].title.as_str())
        );
        assert_eq!(
            window.find("inspector-session").value(),
            Some(format!("Session: {}", rows[1].title).as_str())
        );
        assert_eq!(
            window.find(session_row_id(&rows[1].id)).selected(),
            Some(true)
        );
    });
}

#[gpui_kit::test]
fn a_notice_and_a_cut_block_say_what_they_say_to_assistive_technology(cx: &mut TestAppContext) {
    let mut blocks = fixture::timeline(8);
    blocks.push(fixture::huge_block(9, 256 * 1024));
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(1200.)), blocks);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // A row that shows less than its block: the line that says so is
        // the name of the group its controls are in, and the row's text is
        // the bounded window, not the whole block.
        let line = window.find("block-9-top-window");
        assert_eq!(line.role(), Some(Role::Group));
        let name = line.label().expect("the line is named").to_string();
        assert!(name.starts_with("Showing "), "{name}");
        assert!(name.contains("after it is not shown."), "{name}");
        let text = window.find(("block-text", 9usize));
        let value = text.value().expect("the row has text");
        assert!(
            value.len() <= timeline::PREVIEW_MAX_BYTES,
            "{}",
            value.len()
        );
        // The window as plain text: the fence and its opening line are
        // markdown, not words.
        let preview = shell.read(cx).last_block_preview();
        assert!(preview.starts_with("```text\n"));
        assert_eq!(value, timeline::plain_text(&preview));
        assert!(!value.contains("```"));

        // A refusal is text too.
        let paste = "x".repeat(limits::COMPOSER_MAX_BYTES + 1);
        cx.write_to_clipboard(ClipboardItem::new_string(paste));
        window.press(&chord("v"), cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let notice = window.find("composer-notice");
        assert_eq!(notice.role(), Some(Role::Label));
        assert_eq!(
            notice.value(),
            shell.read(cx).composer_notice(),
            "the notice on screen and the one read are the same"
        );
        assert!(notice
            .value()
            .is_some_and(|value| value.starts_with("Paste not added")));
    });
}

#[gpui_kit::test]
fn a_rows_accessible_text_keeps_what_the_markdown_view_draws(cx: &mut TestAppContext) {
    // Drawn by the kit's own markdown view: list numbers from a list's
    // start, and task boxes in place of numbers. The accessible text has to
    // carry the same.
    let blocks = vec![
        TimelineBlock::new(1, BlockKind::Assistant, "7. seven\n8. eight".into()),
        TimelineBlock::new(2, BlockKind::Assistant, "- [x] done\n- [ ] to do".into()),
    ];
    let (handle, _) = open_blocks(cx, size(px(1440.), px(1200.)), blocks);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        // The test window records the items' text but not the list
        // markers or the boxes (the view draws those apart from its text),
        // so the markers are taken from the kit's source
        // (`render_list_item_row`), not from paint.
        let painted = painted(window);
        for text in ["seven", "eight", "done", "to do"] {
            assert!(painted.iter().any(|painted| painted == text), "{painted:?}");
        }
        assert_eq!(
            window.find(("block-text", 1usize)).value(),
            Some("7. seven\n8. eight")
        );
        assert_eq!(
            window.find(("block-text", 2usize)).value(),
            Some("☑ done\n☐ to do")
        );
    });
}

#[gpui_kit::test]
fn every_place_focus_can_rest_on_a_reflowed_page_has_an_accessibility_node(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 200, 40);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::from_percent(200), window, cx)
        });
        window.render_frame(cx);
        // In a real window, focus on the status line or on the
        // conversation used to be on nothing the platform could name.
        for (id, role, label) in [
            ("status", Role::Status, "Status"),
            ("region-conversation", Role::Main, "Conversation"),
        ] {
            let element = window.find(id);
            assert_eq!(element.role(), Some(role), "{id}");
            assert_eq!(element.label(), Some(label), "{id}");
        }
        tab_to(window, "status", cx);
        assert_eq!(window.find("status").focused(), Some(true));
        // The stacked status lines are still there to be read.
        assert_eq!(
            window.find("status-detail").value(),
            Some("Text 200% · Chat · inspector hidden")
        );
        // A destination with no view: the notice is the landmark's name
        // and description.
        window.press(TOGGLE_SIDEBAR, cx);
        window.render_frame(cx);
        window.click("destination-Tools", cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let notice = window.find("region-conversation");
        assert_eq!(notice.role(), Some(Role::Main));
        assert_eq!(notice.label(), Some("Tools: not built"));
        assert!(notice.description().is_some_and(
            |text| text.contains("has Chat, fixture Sessions and loaded-text Search only")
        ));
    });
}

// While a dialog is open, everything else is taken out of the accessibility
// tree (AccessKit's hidden flag, inherited). The kit's test tree does not
// show that flag, so these check the shell's own account of it and that
// nothing else about the shell changes; the real window's platform tree was
// read before and after (see the findings).

#[gpui_kit::test]
fn the_background_is_inert_exactly_while_a_dialog_is_open(cx: &mut TestAppContext) {
    // 1100 px: the sidebar docks and the Context panel is an overlay, so a
    // docked panel sits behind the dialog.
    let (handle, shell) = open(cx, size(px(1100.), px(900.)), 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft", cx);
        // The parts that are taken out each have a node of their own.
        assert_eq!(window.find("header").role(), Some(Role::Banner));
        assert_eq!(window.find("header").label(), Some("Header"));
        assert_eq!(window.find("region-composer").role(), Some(Role::Group));
        assert_eq!(window.find("region-composer").label(), Some("Composer"));
        assert_eq!(window.find("region-sidebar").role(), Some(Role::Navigation));
        assert!(!shell.read(cx).background_inert(window));

        window.press(TOGGLE_INSPECTOR, cx);
        window.render_frame(cx);
        assert!(shell.read(cx).background_inert(window));
        let dialog = window.find("region-inspector");
        assert_eq!(dialog.role(), Some(Role::Dialog));
        assert_eq!(dialog.focused(), Some(true));
        // The docked sidebar is still laid out and drawn behind it.
        assert!(window.find("region-sidebar").visible());

        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!shell.read(cx).background_inert(window));
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft");

        // The header's toggle still closes the dialog with the pointer: the
        // flag changes what assistive technology is given, not input.
        window.press(TOGGLE_INSPECTOR, cx);
        window.render_frame(cx);
        assert!(shell.read(cx).background_inert(window));
        window.click("toggle-inspector", cx);
        window.render_frame(cx);
        assert!(window.try_find("region-inspector").is_none());
        assert!(!shell.read(cx).background_inert(window));
    });

    // A reflowed page: both dialogs, each closed with Escape, each giving
    // focus back to the composer, in view.
    let (small, small_shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, small, &small_shell, 200);
    step(cx, small, |window, cx| {
        settle_frames(window, cx);
        window.input("draft", cx);
        for (keys, dialog) in [
            (TOGGLE_SIDEBAR, "region-sidebar"),
            (TOGGLE_INSPECTOR, "region-inspector"),
        ] {
            assert!(!small_shell.read(cx).background_inert(window), "{dialog}");
            window.press(keys, cx);
            settle_frames(window, cx);
            assert!(small_shell.read(cx).background_inert(window), "{dialog}");
            assert_eq!(window.find(dialog).role(), Some(Role::Dialog));
            assert_eq!(window.find(dialog).focused(), Some(true));
            window.press("escape", cx);
            settle_frames(window, cx);
            assert!(!small_shell.read(cx).background_inert(window), "{dialog}");
            assert_composer_in_view(window, MINIMUM, dialog);
        }
        assert_eq!(small_shell.read(cx).draft(cx).as_ref(), "draft");
    });
}

#[gpui_kit::test]
fn panels_expose_landmark_roles_docked_and_named_dialogs_as_overlays(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.role(), Some(Role::Navigation));
        assert_eq!(sidebar.label(), Some("Sessions"));
        assert_eq!(sidebar.description(), None);
        let inspector = window.find("region-inspector");
        assert_eq!(inspector.role(), Some(Role::Complementary));
        assert_eq!(inspector.label(), Some("Context"));
    });

    cx.simulate_window_resize(handle.into(), size(px(720.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(TOGGLE_INSPECTOR, cx);
        let inspector = window.find("region-inspector");
        assert_eq!(inspector.role(), Some(Role::Dialog));
        assert_eq!(inspector.label(), Some("Context"));
        assert!(inspector
            .description()
            .is_some_and(|text| text.contains("Escape")));
        // Exactly one dialog is exposed while an overlay is up.
        assert_eq!(window.find_all_by_role(Role::Dialog).len(), 1);

        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar");
        assert_eq!(sidebar.role(), Some(Role::Dialog));
        assert_eq!(sidebar.label(), Some("Sessions"));
        assert_eq!(window.find_all_by_role(Role::Dialog).len(), 1);

        window.press("escape", cx);
        assert!(window.find_all_by_role(Role::Dialog).is_empty());
    });
}

#[gpui_kit::test]
fn dismissing_an_overlay_returns_focus_to_the_header_control_that_opened_it(
    cx: &mut TestAppContext,
) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "toggle-inspector", cx);
        // Activate the focused toggle from the keyboard.
        window.press("enter", cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert_eq!(window.find("toggle-inspector").focused(), Some(false));

        window.press("escape", cx);
        assert!(window.try_find("region-inspector").is_none());
        // Back on the very button, not on the composer or a neighbour.
        assert_eq!(window.find("toggle-inspector").focused(), Some(true));
        assert_eq!(window.find("toggle-sidebar").focused(), Some(false));
        assert_eq!(window.find_by_label("Message").focused(), Some(false));

        // The same button closes it again, and focus stays on that button.
        window.press("enter", cx);
        assert!(window.find("region-inspector").visible());
        window.click_at("overlay-backdrop", gpui_kit::point(px(40.), px(60.)), cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find("toggle-inspector").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn dismissing_an_overlay_returns_focus_to_the_control_inside_a_region(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("destination-Tools", cx);
    });
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "back-to-chat", cx);

        window.press(TOGGLE_INSPECTOR, cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert_eq!(window.find("back-to-chat").focused(), Some(false));
        window.press("escape", cx);
        // `focused()` on a region means "contains focus", so the button, a
        // leaf, is what shows that focus came back to the control itself.
        assert_eq!(window.find("back-to-chat").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn focus_falls_back_when_the_originating_control_is_gone(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(720.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Opened from the composer, then a destination without a composer is
        // chosen inside the overlay: the composer cannot take focus back.
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.press(TOGGLE_SIDEBAR, cx);
        window.click("destination-Metrics", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Metrics);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-composer").is_none());
        assert_eq!(window.find("region-conversation").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn a_session_chosen_in_the_overlay_returns_focus_to_the_new_composer(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(720.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        let before = shell.read(cx).composer().entity_id();
        // Opened from the composer; choosing a Session inside the overlay
        // swaps in a fresh composer before the overlay closes, so the
        // control focus was taken from no longer exists.
        window.press(TOGGLE_SIDEBAR, cx);
        window.click(session_row_id("fixture-session-7"), cx);
        let composer = shell.read(cx).composer().clone();
        assert_ne!(composer.entity_id(), before);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(gpui_kit::Focusable::focus_handle(&composer, cx).is_focused(window));
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

// ---- Memory policy and stress -------------------------------------------

#[cfg(target_os = "macos")]
const COMMAND: &str = "cmd";
#[cfg(not(target_os = "macos"))]
const COMMAND: &str = "ctrl";

// GPUI Kit binds Redo to a different key on macOS.
#[cfg(target_os = "macos")]
const REDO_SHORTCUT: &str = "cmd-shift-z";
#[cfg(not(target_os = "macos"))]
const REDO_SHORTCUT: &str = "ctrl-y";

fn chord(key: &str) -> String {
    format!("{COMMAND}-{key}")
}

fn open_blocks(
    cx: &mut TestAppContext,
    window_size: Size<Pixels>,
    blocks: Vec<TimelineBlock>,
) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);
        let sessions = fixture::sessions(20);
        let running = fixture::running(&sessions);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: window_size,
            })),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Shell::new(&sessions, &running, blocks, window, cx))
        })
        .expect("open test window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    })
}

/// Lines of text adding up to exactly `bytes` bytes.
fn text_of(bytes: usize) -> String {
    let line = "the quick brown fox jumps over the lazy dog, again and again and again.\n";
    let mut text = line.repeat(bytes / line.len());
    text.push_str(&"x".repeat(bytes - text.len()));
    text
}

fn painted(window: &Window) -> Vec<String> {
    texts(window)
        .iter()
        .map(|text| text.text().to_string())
        .collect()
}

#[gpui_kit::test]
fn a_five_megabyte_block_shows_a_bounded_preview_and_typing_still_works(cx: &mut TestAppContext) {
    const FIVE_MIB: usize = 5 * 1024 * 1024;
    let typed = "ยังพิมพ์ได้ still typing 12345";

    // The same typing with no large block, to compare against.
    let (baseline, _) = open_blocks(cx, size(px(1440.), px(2400.)), fixture::timeline(9));
    cx.run_until_parked();
    let baseline_typing = step(cx, baseline, |window, cx| {
        window.draw(cx).clear(cx);
        let typing = Instant::now();
        window.input(typed, cx);
        typing.elapsed()
    });

    let mut blocks = fixture::timeline(8);
    blocks.push(fixture::huge_block(9, FIVE_MIB));
    assert!(blocks[8].total_bytes() >= FIVE_MIB);

    let started = Instant::now();
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(2400.)), blocks);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let first_draw = started.elapsed();

        // The timeline is at its tail, so the line below the block's text
        // is the one on screen.
        assert!(window.find("block-9-bottom-window").visible());
        let painted = painted(window);
        let notice = painted
            .iter()
            .find(|text| text.starts_with("Showing "))
            .unwrap_or_else(|| panic!("no window line painted"));
        assert!(notice.contains("of 5.0 MiB."), "{notice}");
        assert!(notice.contains("after it is not shown."), "{notice}");
        assert!(!notice.contains("before it"), "{notice}");
        // The start of the output was laid out; lines past the preview were
        // never given to the text layout.
        assert!(painted
            .iter()
            .any(|text| text.contains("00000001 output line")));
        assert!(painted
            .iter()
            .any(|text| text.contains("00000390 output line")));
        assert!(!painted
            .iter()
            .any(|text| text.contains("00000450 output line")));
        let laid_out: usize = painted.iter().map(String::len).sum();
        assert!(
            laid_out < 4 * limits::BLOCK_PREVIEW_MAX_BYTES,
            "{laid_out} bytes of text laid out for the whole window"
        );

        // The composer is not held up by the block above it.
        let typing = Instant::now();
        window.input(typed, cx);
        let typing = typing.elapsed();
        assert_eq!(shell.read(cx).draft(cx).as_ref(), typed);

        println!(
            "EVIDENCE five_mib_block: open_and_first_draw_ms={} typing_ms={} \
             baseline_typing_ms={} for {} keystrokes (each a full frame), \
             painted_text_bytes={laid_out}",
            first_draw.as_millis(),
            typing.as_millis(),
            baseline_typing.as_millis(),
            typed.chars().count()
        );
        // Generous: this guards against a freeze, it is not a budget.
        assert!(first_draw < Duration::from_secs(20));
        assert!(typing < Duration::from_secs(20));
    });
}

#[gpui_kit::test]
fn a_five_megabyte_paste_is_refused_whole_and_explained(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("keep", cx);
    });
    cx.write_to_clipboard(ClipboardItem::new_string(text_of(5 * 1024 * 1024)));
    let pasting = Instant::now();
    step(cx, handle, |window, cx| window.press(&chord("v"), cx));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        println!(
            "EVIDENCE five_mib_paste: refused_in_ms={}",
            pasting.elapsed().as_millis()
        );
        // Nothing of the clipboard went in.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep");
        let notice = shell.read(cx).composer_notice().expect("a reason is shown");
        assert!(notice.starts_with("Paste not added"), "{notice}");
        assert!(
            notice.contains("5.0 MiB") && notice.contains("1.0 MiB"),
            "{notice}"
        );
        assert!(window.find("composer-notice").visible());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));

        // A paste that fits goes in as usual and clears the notice.
        cx.write_to_clipboard(ClipboardItem::new_string(" and this".to_string()));
        window.press(&chord("v"), cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep and this");
        assert_eq!(shell.read(cx).composer_notice(), None);
        assert!(window.try_find("composer-notice").is_none());
    });
}

#[gpui_kit::test]
fn the_composer_never_holds_more_than_its_limit(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let full = text_of(limits::COMPOSER_MAX_BYTES);
    cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(&chord("v"), cx);
    });
    step(cx, handle, |window, cx| {
        // Exactly at the limit is allowed.
        assert_eq!(shell.read(cx).draft(cx).len(), limits::COMPOSER_MAX_BYTES);
        assert_eq!(shell.read(cx).composer_notice(), None);
        // One more character, typed rather than pasted, goes over.
        window.input("ก", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), limits::COMPOSER_MAX_BYTES);
        assert_eq!(draft.as_ref(), full);
        let notice = shell.read(cx).composer_notice().expect("a reason is shown");
        assert!(notice.contains("limit"), "{notice}");
        // The composer was replaced and still has focus.
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn undo_cannot_bring_back_another_sessions_draft(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let rows = |cx: &App| -> Vec<String> {
        shell
            .read(cx)
            .sidebar()
            .rows()
            .iter()
            .take(2)
            .map(|row| row.id.clone())
            .collect()
    };
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("for the first session only", cx);
        // Undo works within one Session's composer.
        window.press(&chord("z"), cx);
    });
    step(cx, handle, |window, cx| {
        assert_ne!(
            shell.read(cx).draft(cx).as_ref(),
            "for the first session only"
        );
        window.press(REDO_SHORTCUT, cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(
            shell.read(cx).draft(cx).as_ref(),
            "for the first session only"
        );
        let second = rows(cx)[1].clone();
        window.click(session_row_id(&second), cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        // Clicking a row took focus out of the composer; put it back.
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        // Undo in the second Session has nothing of the first to restore.
        window.press(&chord("z"), cx);
        window.press(&chord("z"), cx);
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        let first = rows(cx)[0].clone();
        window.click(session_row_id(&first), cx);
    });
    step(cx, handle, |_, cx| {
        // The draft itself was parked and comes back.
        assert_eq!(
            shell.read(cx).draft(cx).as_ref(),
            "for the first session only"
        );
    });
}

#[gpui_kit::test]
fn large_edits_drop_the_undo_history_once_the_budget_is_spent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let chunk = text_of(limits::COMPOSER_MAX_BYTES);
    cx.write_to_clipboard(ClipboardItem::new_string(chunk.clone()));
    step(cx, handle, |window, cx| window.render_frame(cx));
    // Each round inserts and then removes one composer's worth of text; both
    // count against the budget.
    let mut cleared_after = None;
    for round in 0..6 {
        step(cx, handle, |window, cx| window.press(&chord("v"), cx));
        cx.run_until_parked();
        let cleared = step(cx, handle, |_, cx| {
            assert_eq!(shell.read(cx).draft(cx).as_ref(), chunk, "round {round}");
            shell
                .read(cx)
                .composer_notice()
                .is_some_and(|notice| notice.starts_with("Undo history was cleared"))
        });
        if cleared {
            cleared_after = Some(round);
            break;
        }
        step(cx, handle, |window, cx| {
            window.press(&chord("a"), cx);
            window.press("backspace", cx);
        });
        cx.run_until_parked();
    }
    // Insert 1 MiB, remove 1 MiB, insert 1 MiB, remove 1 MiB, insert 1 MiB:
    // 5 MiB of edits passes the 4 MiB budget on the third paste.
    assert_eq!(cleared_after, Some(2));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        // The history is gone: undo changes nothing, and the text is whole.
        window.press(&chord("z"), cx);
    });
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), chunk);
    });
}

// ---- Over-limit edits: every pathway, whole draft kept ---------------------

#[cfg(target_os = "macos")]
const TO_START: &str = "cmd-up";
#[cfg(not(target_os = "macos"))]
const TO_START: &str = "ctrl-home";

/// A draft of exactly `bytes` bytes that ends in a multi-code-point emoji, so
/// a cut tail or a split character would show.
const TAIL: &str = " the tail must survive 👩🏽‍💻";

fn draft_of(bytes: usize) -> String {
    text_of(bytes - TAIL.len()) + TAIL
}

/// Open the shell and paste `draft` into the composer.
fn open_with_draft(cx: &mut TestAppContext, draft: &str) -> (WindowHandle<Root>, Entity<Shell>) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.write_to_clipboard(ClipboardItem::new_string(draft.to_string()));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(&chord("v"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        assert_eq!(shell.read(cx).composer_notice(), None);
    });
    (handle, shell)
}

/// After a rejected edit: the whole previous draft, a reason on screen, and
/// focus still in the composer.
fn assert_rejected(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    expected: &str,
    pathway: &str,
) {
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), expected.len(), "{pathway}: length");
        assert!(draft.as_ref() == expected, "{pathway}: the draft changed");
        let notice = shell
            .read(cx)
            .composer_notice()
            .unwrap_or_else(|| panic!("{pathway}: no reason shown"))
            .to_string();
        assert!(notice.starts_with("Not added"), "{pathway}: {notice}");
        assert_eq!(
            window.find_by_label("Message").focused(),
            Some(true),
            "{pathway}: focus"
        );
    });
}

#[gpui_kit::test]
fn an_over_limit_insertion_in_the_middle_keeps_the_whole_draft_and_the_caret(
    cx: &mut TestAppContext,
) {
    let full = draft_of(limits::COMPOSER_MAX_BYTES);
    let (handle, shell) = open_with_draft(cx, &full);

    // Typed in the middle, one byte over.
    step(cx, handle, |window, cx| {
        window.press(TO_START, cx);
        for _ in 0..5 {
            window.press("right", cx);
        }
        assert_eq!(shell.read(cx).composer_selection(cx), 5..5);
        window.input("Z", cx);
    });
    assert_rejected(cx, handle, &shell, &full, "typing in the middle");
    step(cx, handle, |_, cx| {
        // The caret is where the insertion was attempted.
        assert_eq!(shell.read(cx).composer_selection(cx), 5..5);
    });

    // Pasted in the middle.
    step(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "pasted in the middle".to_string(),
        ));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &full, "paste in the middle");
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).composer_selection(cx), 5..5);
    });

    // Typed at the very end, after the emoji.
    step(cx, handle, |window, cx| {
        window.press(&chord("a"), cx);
        window.press("right", cx);
        assert_eq!(
            shell.read(cx).composer_selection(cx),
            full.len()..full.len()
        );
        window.input("!", cx);
    });
    assert_rejected(cx, handle, &shell, &full, "typing at the end");
}

#[gpui_kit::test]
fn a_paste_is_judged_on_what_it_replaces(cx: &mut TestAppContext) {
    let full = draft_of(limits::COMPOSER_MAX_BYTES);
    let (handle, shell) = open_with_draft(cx, &full);
    let select_first_ten = |window: &mut Window, cx: &mut App| {
        window.press(TO_START, cx);
        for _ in 0..10 {
            window.press("shift-right", cx);
        }
    };

    // Ten bytes over ten selected bytes: the draft stays at the limit.
    step(cx, handle, |window, cx| {
        select_first_ten(window, cx);
        assert_eq!(shell.read(cx).composer_selection(cx), 0..10);
        cx.write_to_clipboard(ClipboardItem::new_string("0123456789".to_string()));
        window.press(&chord("v"), cx);
    });
    cx.run_until_parked();
    let replaced = format!("0123456789{}", &full[10..]);
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == replaced);
        assert_eq!(shell.read(cx).composer_notice(), None);
    });

    // Eleven bytes over ten selected bytes: one over, refused whole, and the
    // selection it would have replaced is selected again.
    step(cx, handle, |window, cx| {
        select_first_ten(window, cx);
        cx.write_to_clipboard(ClipboardItem::new_string("0123456789X".to_string()));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &replaced, "paste over a selection");
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).composer_selection(cx), 0..10);
    });

    // A clipboard the size of the limit replaces a fully selected draft.
    let other = draft_of(limits::COMPOSER_MAX_BYTES).replace("fox", "cat");
    assert_eq!(other.len(), limits::COMPOSER_MAX_BYTES);
    step(cx, handle, |window, cx| {
        window.press(&chord("a"), cx);
        cx.write_to_clipboard(ClipboardItem::new_string(other.clone()));
        window.press(&chord("v"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == other);
        assert_eq!(shell.read(cx).composer_notice(), None);
    });
}

#[gpui_kit::test]
fn a_multi_code_point_character_is_never_inserted_in_part(cx: &mut TestAppContext) {
    // Four bytes of room; the emoji is one character of fifteen bytes.
    let emoji = "👩🏽‍💻";
    assert_eq!(emoji.len(), 15);
    let nearly = draft_of(limits::COMPOSER_MAX_BYTES - 4);
    let (handle, shell) = open_with_draft(cx, &nearly);

    // Committed in one edit, as an input method or the emoji picker does.
    step(cx, handle, |window, cx| {
        let bounds = window.find_by_label("Message").bounds();
        let composer = shell.read(cx).composer().clone();
        TestInput::new(ElementInputHandler::new(bounds, composer), window).commit_text(emoji, cx);
    });
    assert_rejected(cx, handle, &shell, &nearly, "input method commit");

    // Pasted.
    step(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(emoji.to_string()));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &nearly, "emoji paste");

    // A composition in progress (marked text) is left alone while the input
    // method owns it: twelve bytes where three fit, and no complaint yet.
    let ime = |cx: &mut TestAppContext, act: &dyn Fn(&mut TestInput, &mut App)| {
        step(cx, handle, |window, cx| {
            let bounds = window.find_by_label("Message").bounds();
            let composer = shell.read(cx).composer().clone();
            act(
                &mut TestInput::new(ElementInputHandler::new(bounds, composer), window),
                cx,
            );
        });
        cx.run_until_parked();
    };
    step(cx, handle, |window, cx| window.input("a", cx));
    cx.run_until_parked();
    let nearly = format!("{nearly}a");
    ime(cx, &|input, cx| {
        input.compose_text(None, "にほんご", None, cx)
    });
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).len(), nearly.len() + 12);
        assert_eq!(shell.read(cx).composer_notice(), None);
        // Nothing is handed on while the input method owns the text, over
        // the limit or not: Send is unavailable until the composition ends.
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        window.click("composer-primary", cx);
        assert_eq!(shell.read(cx).last_intent(), None);
    });
    // Ending the composition in place makes its text part of the draft, and
    // the kit gives no signal at all when that happens (no event, no
    // notification). Until the composer next changes, the excess is still
    // there; it can be neither sent nor parked.
    ime(cx, &|input, cx| input.unmark_text(cx));
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).len(), nearly.len() + 12);
        window.click("composer-primary", cx);
        assert_eq!(shell.read(cx).last_intent(), None);
        // Over-limit text is not handed on even in this state.
        assert!(shell
            .read(cx)
            .composer_notice()
            .is_some_and(|notice| notice.starts_with("Not sent: the message is over")));
        // The next change of any kind, here a caret move, is when it is
        // judged, and it is taken back whole.
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.press("left", cx);
    });
    assert_rejected(cx, handle, &shell, &nearly, "composition ended in place");

    // Composed again and committed with different text: same outcome.
    ime(cx, &|input, cx| input.compose_text(None, "かな", None, cx));
    ime(cx, &|input, cx| input.commit_text("仮名", cx));
    assert_rejected(cx, handle, &shell, &nearly, "composition committed");

    // What fits still goes in: three more ASCII bytes fill the draft exactly.
    step(cx, handle, |window, cx| window.input("bcd", cx));
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), limits::COMPOSER_MAX_BYTES);
        assert!(draft.ends_with(&format!("{emoji}abcd")));
    });
}

#[gpui_kit::test]
fn a_rejected_edit_keeps_the_earlier_undo_history_and_cannot_be_redone(cx: &mut TestAppContext) {
    let base = draft_of(limits::COMPOSER_MAX_BYTES - 20);
    let (handle, shell) = open_with_draft(cx, &base);
    // An accepted edit to undo later.
    step(cx, handle, |window, cx| window.input("abc", cx));
    cx.run_until_parked();
    let accepted = format!("{base}abc");
    step(cx, handle, |window, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == accepted);
        // Thirty bytes where seventeen fit.
        cx.write_to_clipboard(ClipboardItem::new_string("x".repeat(30)));
        window.press(&chord("v"), cx);
    });
    assert_rejected(cx, handle, &shell, &accepted, "paste at the end");
    step(cx, handle, |window, cx| {
        // The rejection went through the kit's own undo, so nothing else was
        // lost: the notice does not report a cleared history.
        let notice = shell.read(cx).composer_notice().unwrap_or_default();
        assert!(notice.ends_with("Your text is unchanged."), "{notice}");
        assert!(!notice.contains("undo history"), "{notice}");
        // Undo now takes back the accepted typing, not nothing.
        window.press(&chord("z"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let draft = shell.read(cx).draft(cx);
        assert!(draft.len() < accepted.len(), "undo had nothing to undo");
        assert!(accepted.starts_with(draft.as_ref()));
        // Redo brings the typing back; a second redo would re-apply the
        // rejected paste, and is rejected again.
        window.press(REDO_SHORTCUT, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == accepted);
        window.press(REDO_SHORTCUT, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        let draft = shell.read(cx).draft(cx);
        assert!(draft.len() <= limits::COMPOSER_MAX_BYTES);
        assert!(
            draft.as_ref() == accepted,
            "redo of the rejected paste got through"
        );
    });
}

#[gpui_kit::test]
fn discarding_stored_drafts_is_reported_not_silent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let ids: Vec<String> = step(cx, handle, |window, cx| {
        window.render_frame(cx);
        shell
            .read(cx)
            .sidebar()
            .rows()
            .iter()
            .take(6)
            .map(|row| row.id.clone())
            .collect()
    });
    let draft = draft_of(limits::COMPOSER_MAX_BYTES);
    cx.write_to_clipboard(ClipboardItem::new_string(draft.clone()));
    let stored = limits::DRAFT_MAX_BYTES / limits::COMPOSER_MAX_BYTES;
    assert_eq!(stored, 4);

    // Leave a full-size draft in each of the first five Sessions.
    for (index, id) in ids.iter().enumerate().take(stored + 1) {
        step(cx, handle, |window, cx| {
            window.click(session_row_id(id), cx);
            window.click_at("region-composer", point(px(40.), px(20.)), cx);
            window.press(&chord("v"), cx);
        });
        cx.run_until_parked();
        step(cx, handle, |_, cx| {
            assert_eq!(
                shell.read(cx).draft(cx).len(),
                draft.len(),
                "session {index}"
            );
            // Four parked drafts fit the budget exactly: nothing discarded
            // and nothing reported so far.
            assert_eq!(shell.read(cx).composer_notice(), None, "session {index}");
        });
    }

    // Leaving the fifth parks a fifth megabyte: the oldest draft has to go.
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&ids[stored + 1]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let notice = shell
            .read(cx)
            .composer_notice()
            .expect("the discard is reported");
        assert!(
            notice.starts_with("Unsent drafts for 1 older Session(s) were discarded"),
            "{notice}"
        );
        assert!(
            notice.contains("64 Sessions") && notice.contains("4.0 MiB"),
            "{notice}"
        );
        assert!(window.find("composer-notice").visible());
        assert!(!shell.read(cx).has_stored_draft(&ids[0]));
        for id in &ids[1..=stored] {
            assert!(shell.read(cx).has_stored_draft(id), "{id}");
        }
        // The discarded draft really is gone, and a kept one is intact.
        window.click(session_row_id(&ids[0]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        window.click(session_row_id(&ids[1]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == draft);
    });
}

// ---- Deferred composer work never crosses Sessions ------------------------

/// The ids of the first few Session rows, in sidebar order.
fn first_rows(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
) -> Vec<String> {
    step(cx, handle, |_, cx| {
        shell
            .read(cx)
            .sidebar()
            .rows()
            .iter()
            .take(3)
            .map(|row| row.id.clone())
            .collect()
    })
}

/// Queue a click on a Session row to run after the effects already queued in
/// this update (a composer change, for one) and before anything those
/// effects go on to defer.
fn queue_switch(handle: WindowHandle<Root>, session_id: &str, cx: &mut App) {
    let row = session_row_id(session_id);
    cx.defer(move |cx| {
        cx.update_window(handle.into(), |_, window, cx| window.click(row, cx))
            .expect("shell window is open");
    });
}

#[gpui_kit::test]
fn a_switch_queued_behind_an_over_limit_edit_leaves_the_other_session_alone(
    cx: &mut TestAppContext,
) {
    let full = draft_of(limits::COMPOSER_MAX_BYTES);
    let (handle, shell) = open_with_draft(cx, &full);
    let rows = first_rows(cx, handle, &shell);

    // One batch: an over-limit paste into the first Session, then a switch
    // to the second. The paste's rejection is scheduled when its change is
    // delivered; the switch runs next; the rejection's deferred steps run
    // last, for a composer that no longer exists.
    step(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("over the limit".to_string()));
        window.press(&chord("v"), cx);
        queue_switch(handle, &rows[1], cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(rows[1].as_str()));
        // The second Session's composer has nothing of the first in it.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        assert_eq!(shell.read(cx).composer_selection(cx), 0..0);
        // The over-limit text was not stored; the accepted draft was, and
        // that is said.
        let notice = shell
            .read(cx)
            .composer_notice()
            .expect("the loss is reported");
        assert!(
            notice.starts_with("Text over the 1.0 MiB limit"),
            "{notice}"
        );
        assert!(shell.read(cx).has_stored_draft(&rows[0]));
        // Typing here stays here.
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.input("second", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "second");
        window.click(session_row_id(&rows[0]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        // The first Session's draft is exactly what it was before the paste.
        assert!(shell.read(cx).draft(cx).as_ref() == full);
    });
}

#[gpui_kit::test]
fn a_switch_queued_behind_an_undo_history_reset_leaves_the_other_session_alone(
    cx: &mut TestAppContext,
) {
    let chunk = draft_of(limits::COMPOSER_MAX_BYTES);
    let (handle, shell) = open_with_draft(cx, &chunk);
    let rows = first_rows(cx, handle, &shell);
    // The opening paste, a removal, a paste and a removal: 4 MiB of edits,
    // exactly the budget and not yet over it.
    let select_all_and_delete = |window: &mut Window, cx: &mut App| {
        window.press(&chord("a"), cx);
        window.press("backspace", cx);
    };
    step(cx, handle, |window, cx| select_all_and_delete(window, cx));
    cx.run_until_parked();
    step(cx, handle, |window, cx| window.press(&chord("v"), cx));
    cx.run_until_parked();
    step(cx, handle, |window, cx| select_all_and_delete(window, cx));
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        assert_eq!(shell.read(cx).composer_notice(), None);
    });
    // One batch: the edit that spends the budget, then a switch. The history
    // reset is scheduled first and must not be applied to the second
    // Session's composer.
    step(cx, handle, |window, cx| {
        window.press(&chord("v"), cx);
        queue_switch(handle, &rows[1], cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(rows[1].as_str()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        // No notice about a history that is not this Session's.
        assert_eq!(shell.read(cx).composer_notice(), None);
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.input("second", cx);
        // Undo works in the second Session: its history was not dropped.
        window.press(&chord("z"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert_ne!(shell.read(cx).draft(cx).as_ref(), "second");
    });
}

/// Queue work on the window to run after the effects already queued in this
/// update and before anything those effects go on to defer.
fn queue_on_window(
    handle: WindowHandle<Root>,
    cx: &mut App,
    work: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    cx.defer(move |cx| {
        cx.update_window(handle.into(), |_, window, cx| work(window, cx))
            .expect("shell window is open");
    });
}

#[gpui_kit::test]
fn a_switch_while_a_rejection_is_pending_stores_the_accepted_draft_not_what_is_on_screen(
    cx: &mut TestAppContext,
) {
    // Room for two bytes. "a" and "b" are accepted; "c" goes over.
    let base = draft_of(limits::COMPOSER_MAX_BYTES - 2);
    let (handle, shell) = open_with_draft(cx, &base);
    let rows = first_rows(cx, handle, &shell);
    for character in ["a", "b"] {
        step(cx, handle, |window, cx| window.input(character, cx));
        cx.run_until_parked();
    }
    let accepted = format!("{base}ab");
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == accepted);
    });

    // One batch. The third character's rejection is recorded when its change
    // is delivered. Before that rejection is finished, an undo takes the
    // whole typing group out (as the kit's grouped undo does), so the
    // composer holds less than the accepted draft, and the Session is
    // switched in that state.
    let target = rows[1].clone();
    let observed = shell.clone();
    let base_len = base.len();
    step(cx, handle, |window, cx| {
        window.input("c", cx);
        queue_on_window(handle, cx, move |window, cx| {
            window.press(&chord("z"), cx);
            // On screen now: neither the over-limit text nor the accepted
            // draft, and it is under the limit.
            assert_eq!(observed.read(cx).draft(cx).len(), base_len);
            window.click(session_row_id(&target), cx);
        });
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(rows[1].as_str()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        let notice = shell
            .read(cx)
            .composer_notice()
            .expect("the loss is reported");
        assert!(
            notice.starts_with("Text over the 1.0 MiB limit"),
            "{notice}"
        );
        window.click(session_row_id(&rows[0]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        // The accepted draft was stored: both characters that fitted are
        // there, and the one that did not is not.
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), accepted.len());
        assert!(draft.as_ref() == accepted);
    });
}

#[gpui_kit::test]
fn a_stale_rejection_undoes_nothing_in_the_session_switched_to(cx: &mut TestAppContext) {
    let full = draft_of(limits::COMPOSER_MAX_BYTES);
    let (handle, shell) = open_with_draft(cx, &full);
    let rows = first_rows(cx, handle, &shell);
    let typed = "typed in the second session";

    // One batch: an over-limit paste in the first Session; then a switch,
    // and typing in the second Session, which gives its composer focus and
    // an undo history of its own. The first Session's rejection is finished
    // last. It must not send an undo to the composer that has focus now.
    let target = rows[1].clone();
    step(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("over the limit".to_string()));
        window.press(&chord("v"), cx);
        queue_on_window(handle, cx, move |window, cx| {
            window.click(session_row_id(&target), cx);
            window.click_at("region-composer", point(px(40.), px(20.)), cx);
            window.input(typed, cx);
        });
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(rows[1].as_str()));
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        // Nothing of the second Session's typing was undone.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), typed);
        // Its history is its own and still there.
        window.press(&chord("z"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert_ne!(shell.read(cx).draft(cx).as_ref(), typed);
        window.click(session_row_id(&rows[0]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == full);
    });
}

#[gpui_kit::test]
fn typing_across_the_limit_keeps_what_fitted_and_says_the_history_went(cx: &mut TestAppContext) {
    // Room for two more bytes. Three characters are typed one event at a
    // time with no pause between them, which the kit records as one undo
    // step.
    let base = draft_of(limits::COMPOSER_MAX_BYTES - 2);
    let (handle, shell) = open_with_draft(cx, &base);
    for character in ["a", "b", "c"] {
        step(cx, handle, |window, cx| window.input(character, cx));
        cx.run_until_parked();
    }
    let kept = format!("{base}ab");
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // The two characters that fitted are still there. The kit's undo
        // took them along with the third, so the draft was restored the
        // other way, and the notice says what that cost.
        let draft = shell.read(cx).draft(cx);
        assert_eq!(draft.len(), kept.len());
        assert!(draft.as_ref() == kept);
        assert_eq!(
            shell.read(cx).composer_selection(cx),
            kept.len()..kept.len()
        );
        let notice = shell.read(cx).composer_notice().expect("a reason is shown");
        println!("EVIDENCE grouped_typing_rejection notice: {notice}");
        assert!(notice.starts_with("Not added"), "{notice}");
        assert!(
            notice.ends_with("but its undo history had to be cleared."),
            "{notice}"
        );
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        // And it really is gone: undo changes nothing.
        window.press(&chord("z"), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert!(shell.read(cx).draft(cx).as_ref() == kept);
    });
}

#[gpui_kit::test]
fn typing_delivered_as_one_batch_is_judged_and_rejected_as_one_edit(cx: &mut TestAppContext) {
    // The same three characters arriving before the shell is told about any
    // of them: the shell sees one change, from the draft it last accepted to
    // one that is over the limit, and takes all of it back.
    let base = draft_of(limits::COMPOSER_MAX_BYTES - 2);
    let (handle, shell) = open_with_draft(cx, &base);
    step(cx, handle, |window, cx| window.input("abc", cx));
    assert_rejected(cx, handle, &shell, &base, "a batch of typing");
    step(cx, handle, |_, cx| {
        let notice = shell.read(cx).composer_notice().unwrap_or_default();
        assert!(notice.ends_with("Your text is unchanged."), "{notice}");
    });
}

#[gpui_kit::test]
fn switching_sessions_during_an_over_limit_composition_says_what_was_not_kept(
    cx: &mut TestAppContext,
) {
    let nearly = draft_of(limits::COMPOSER_MAX_BYTES - 4);
    let (handle, shell) = open_with_draft(cx, &nearly);
    let rows = first_rows(cx, handle, &shell);
    // Twelve bytes of composition where four fit, still marked.
    step(cx, handle, |window, cx| {
        let bounds = window.find_by_label("Message").bounds();
        let composer = shell.read(cx).composer().clone();
        TestInput::new(ElementInputHandler::new(bounds, composer), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).draft(cx).len(), nearly.len() + 12);
        assert_eq!(shell.read(cx).composer_notice(), None);
        window.click(session_row_id(&rows[1]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        // Nothing was evicted, so this notice is only about the composition.
        let notice = shell
            .read(cx)
            .composer_notice()
            .expect("the loss is reported");
        assert_eq!(
            notice,
            "Text over the 1.0 MiB limit in the Session you left was not kept; its draft was \
             stored without it."
        );
        assert!(window.find("composer-notice").visible());
        window.click(session_row_id(&rows[0]), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        // What was accepted before the composition is what was stored.
        assert!(shell.read(cx).draft(cx).as_ref() == nearly);
    });
}

// ---- Streaming -----------------------------------------------------------

#[gpui_kit::test]
fn a_burst_of_stream_deltas_reaches_the_view_in_one_update(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(1440.), px(1200.)), 20, 40);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find(("block", 40usize)).visible());
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            for index in 0..1_000 {
                shell.push_stream_delta(&format!("คำที่ {index} "), cx);
            }
        });
        // The data is already complete; the view has not been told yet.
        assert_eq!(shell.read(cx).block_count(), 41);
        assert_eq!(shell.read(cx).stream_stats(), (1_001, 0));
    });
    cx.executor().advance_clock(Duration::from_millis(
        2 * formless_desktop::timeline::FLUSH_INTERVAL_MS,
    ));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(shell.read(cx).stream_stats(), (1_001, 1));
        // Following the tail: the streamed block is on screen with its text.
        assert!(window.find(("block", 41usize)).visible());
        assert!(painted(window)
            .iter()
            .any(|text| text.contains("คำที่ 0 ") && text.contains("คำที่ 999")));

        // Finishing shows the final state at once, with no timer involved.
        shell.update(cx, |shell, cx| {
            shell.push_stream_delta("จบ", cx);
            shell.finish_stream(cx);
        });
        assert_eq!(shell.read(cx).stream_stats().1, 2);
    });
    // The kit parses Markdown off the main thread, so the new text is painted
    // a frame after the update, not within it. The timer armed by the last
    // delta also fires here and finds nothing left to do.
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert!(painted(window).iter().any(|text| text.ends_with("จบ")));
        assert_eq!(shell.read(cx).stream_stats().1, 2);
    });
}

#[gpui_kit::test]
fn streaming_does_not_move_older_content_that_is_being_read(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(1440.), px(1200.)), 20, 200);
    cx.run_until_parked();
    let anchor = step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // Scroll well away from the tail.
        window.scroll(
            "region-conversation",
            ScrollDelta::Pixels(point(px(0.), px(4000.))),
            cx,
        );
        window.render_frame(cx);
        assert!(window.try_find(("block", 200usize)).is_none());
        let id = (1..=200usize)
            .rev()
            .find(|id| {
                window
                    .try_find(("block", *id))
                    .is_some_and(|block| block.visible() && block.bounds().top() > px(100.))
            })
            .expect("an older block is on screen");
        (id, window.find(("block", id)).bounds())
    });

    step(cx, handle, |_, cx| {
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            for index in 0..300 {
                shell.push_stream_delta(&format!("streamed paragraph {index}\n\n"), cx);
            }
        });
    });
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).stream_stats(), (301, 1));
        assert_eq!(shell.read(cx).block_count(), 201);
        // The block being read is exactly where it was.
        assert_eq!(window.find(("block", anchor.0)).bounds(), anchor.1);
        // The streamed tail is off screen and was not built.
        assert!(window.try_find(("block", 201usize)).is_none());
    });
}

// ---- Windows onto long blocks ---------------------------------------------

/// Click a control by id if it is on screen, trying the line below the
/// block's text first and then the one above it.
fn click_window_control(window: &mut Window, block: usize, action: &str, cx: &mut App) {
    for position in ["bottom", "top"] {
        let id = gpui_kit::SharedString::from(format!("block-{block}-{position}-{action}"));
        if window
            .try_find(id.clone())
            .is_some_and(|control| control.visible())
        {
            window.click(id, cx);
            return;
        }
    }
    panic!("no {action} control of block {block} is on screen");
}

#[gpui_kit::test]
fn a_five_megabyte_block_can_be_paged_from_either_end_in_bounded_windows(cx: &mut TestAppContext) {
    let mut blocks = fixture::timeline(8);
    blocks.push(fixture::huge_block(9, 5 * 1024 * 1024));
    let total = blocks[8].markdown().len();
    let last_line = blocks[8]
        .markdown()
        .lines()
        .rev()
        .find(|line| line.contains("output line"))
        .expect("the block has output lines")[..8]
        .to_string();
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(1200.)), blocks);
    cx.run_until_parked();
    let laid_out = |window: &mut Window, cx: &mut App| -> Vec<String> {
        window.draw(cx).clear(cx);
        let painted = painted(window);
        let bytes: usize = painted.iter().map(String::len).sum();
        assert!(
            bytes < 4 * limits::BLOCK_PREVIEW_MAX_BYTES,
            "{bytes} bytes laid out"
        );
        painted
    };
    step(cx, handle, |window, cx| {
        let painted = laid_out(window, cx);
        assert!(painted
            .iter()
            .any(|text| text.contains("00000001 output line")));
        assert!(!painted.iter().any(|text| text.contains(&last_line)));
        // Jump to the end.
        click_window_control(window, 9, "end", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let painted = laid_out(window, cx);
        assert!(painted.iter().any(|text| text.contains(&last_line)));
        assert!(!painted
            .iter()
            .any(|text| text.contains("00000001 output line")));
        let notice = painted
            .iter()
            .find(|text| text.starts_with("Showing "))
            .expect("the window line is painted");
        assert!(notice.contains("before it is not shown."), "{notice}");
        assert!(!notice.contains("after it"), "{notice}");
        // One window back, then back to the start.
        click_window_control(window, 9, "earlier", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let painted = laid_out(window, cx);
        assert!(!painted.iter().any(|text| text.contains(&last_line)));
        let notice = painted
            .iter()
            .find(|text| text.starts_with("Showing "))
            .expect("the window line is painted");
        // In the middle: something is missing on both sides, and it says so.
        assert!(
            notice.contains("before it is not shown.") && notice.contains("after it is not shown."),
            "{notice}"
        );
        click_window_control(window, 9, "start", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let painted = laid_out(window, cx);
        assert!(painted
            .iter()
            .any(|text| text.contains("00000001 output line")));
        // Nothing about the block itself changed.
        assert_eq!(shell.read(cx).block_count(), 9);
        let _ = total;
    });
}

#[gpui_kit::test]
fn a_fenced_block_with_an_enormous_opening_line_is_still_a_bounded_row(cx: &mut TestAppContext) {
    // A megabyte of info string on the fence's opening line, then code.
    let code: String = (1..=20_000)
        .map(|line| format!("code line {line:06}\n"))
        .collect();
    let markdown = format!("```{}\n{code}", "x".repeat(1024 * 1024));
    let mut blocks = fixture::timeline(8);
    blocks.push(TimelineBlock::new(9, BlockKind::ToolSummary, markdown));
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(1200.)), blocks);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        click_window_control(window, 9, "end", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        // The row is given a short opening line in place of the real one.
        let row = shell.read(cx).last_block_preview();
        assert!(row.len() <= timeline::PREVIEW_MAX_BYTES, "{}", row.len());
        assert!(row.starts_with("```\ncode line "), "{:?}", &row[..32]);
        let painted = painted(window);
        let bytes: usize = painted.iter().map(String::len).sum();
        assert!(bytes <= 2 * timeline::PREVIEW_MAX_BYTES, "{bytes} bytes");
        assert!(painted.iter().any(|text| text.contains("code line 020000")));
        assert!(!painted.iter().any(|text| text.contains("xxxxxxxx")));
    });
}

#[gpui_kit::test]
fn a_burst_of_deltas_rebuilds_the_streamed_row_once_and_a_frame_shows_the_newest(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(1440.), px(1200.)), 20, 40);
    cx.run_until_parked();
    let flush = |cx: &mut TestAppContext| {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    };
    let delta = |index: usize| format!("ย่อหน้าที่ {index} 👩🏽‍💻 #{index:06}#\n\n");
    // Start the stream and let the list learn of the new row.
    step(cx, handle, |_, cx| {
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            shell.push_stream_delta(&delta(0), cx);
        });
    });
    flush(cx);
    let rebuilds = |cx: &App| shell.read(cx).stream_work().rebuilds;
    let view_updates = |cx: &App| shell.read(cx).stream_stats().1;
    let mut updates_before_burst = 0;
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let before = rebuilds(cx);
        updates_before_burst = view_updates(cx);
        // Enough for several windows, all between two frames.
        const BURST: usize = 5_000;
        shell.update(cx, |shell, cx| {
            for index in 1..=BURST {
                shell.push_stream_delta(&delta(index), cx);
            }
        });
        // Nothing was rebuilt for them.
        assert_eq!(rebuilds(cx), before);
        assert_eq!(shell.read(cx).stream_work().deltas, BURST as u64 + 1);

        // A frame drawn now, before the flush that is on its way, rebuilds
        // the row once and hands the newest text to the row's view.
        window.draw(cx).clear(cx);
        assert_eq!(rebuilds(cx), before + 1);
        // Drawing again rebuilds nothing.
        window.draw(cx).clear(cx);
        assert_eq!(rebuilds(cx), before + 1);
    });
    // The kit lays markdown out off the main thread. Let that finish
    // without letting the clock reach the flush.
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let painted = painted(window);
        let bytes: usize = painted.iter().map(String::len).sum();
        assert!(bytes <= 2 * timeline::PREVIEW_MAX_BYTES, "{bytes} bytes");
        assert!(painted.iter().any(|text| text.contains("#005000#")));
        // The newest text is on show and the flush has still not run.
        assert_eq!(view_updates(cx), updates_before_burst);
    });
    // Neither does the flush: the row is already up to date.
    flush(cx);
    step(cx, handle, |_, cx| {
        // Two rebuilds in all: the first delta's, and the burst's.
        let work = shell.read(cx).stream_work();
        assert_eq!(work.rebuilds, 2, "{work:?}");
        assert!(
            work.bytes_copied as usize <= 2 * timeline::PREVIEW_MAX_BYTES,
            "{work:?}"
        );
        let row = shell.read(cx).last_block_preview();
        assert!(row.len() <= timeline::PREVIEW_MAX_BYTES);
        assert!(
            row.ends_with("#005000#\n\n"),
            "{:?}",
            &row[row.len() - 40..]
        );
        let (_, _, after) = shell.read(cx).last_block_window().unwrap();
        assert_eq!(after, 0);
    });
}

#[gpui_kit::test]
fn a_streamed_block_longer_than_one_window_keeps_showing_its_newest_text(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(1440.), px(1200.)), 20, 40);
    cx.run_until_parked();
    let flush = |cx: &mut TestAppContext| {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    };
    // Stream about three windows' worth: Thai, an emoji sequence, and a
    // marker in every delta.
    let delta = |index: usize| format!("ย่อหน้าที่ {index} 👩🏽‍💻 จบย่อหน้า #{index:06}#\n\n");
    let per_delta = delta(1_000).len();
    let count = 3 * limits::BLOCK_PREVIEW_MAX_BYTES / per_delta;
    step(cx, handle, |_, cx| {
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            for index in 1..=count {
                shell.push_stream_delta(&delta(index), cx);
            }
        });
    });
    flush(cx);
    let newest = |index: usize| format!("จบย่อหน้า #{index:06}#");
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let painted = painted(window);
        // The newest text is on screen, the oldest is not laid out, and the
        // row says how much is missing before it.
        assert!(window.find(("block", 41usize)).visible());
        assert!(painted.iter().any(|text| text.contains(&newest(count))));
        assert!(!painted.iter().any(|text| text.contains(&newest(1))));
        let notice = painted
            .iter()
            .find(|text| text.starts_with("Showing "))
            .expect("the window line is painted");
        assert!(notice.contains("before it is not shown."), "{notice}");
        assert!(!notice.contains("after it"), "{notice}");
        let bytes: usize = painted.iter().map(String::len).sum();
        assert!(
            bytes < 4 * limits::BLOCK_PREVIEW_MAX_BYTES,
            "{bytes} bytes laid out"
        );

        // More arrives: still the newest text.
        shell.update(cx, |shell, cx| {
            for index in count + 1..=count + 50 {
                shell.push_stream_delta(&delta(index), cx);
            }
        });
    });
    flush(cx);
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert!(painted(window)
            .iter()
            .any(|text| text.contains(&newest(count + 50))));
        // The reader goes back to the start of the block...
        click_window_control(window, 41, "start", cx);
    });
    flush(cx);
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert!(painted(window).iter().any(|text| text.contains(&newest(1))));
        // ...and the stream goes on.
        shell.update(cx, |shell, cx| {
            for index in count + 51..=count + 100 {
                shell.push_stream_delta(&delta(index), cx);
            }
        });
    });
    flush(cx);
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        let painted = painted(window);
        // The start stays on show; the row says more is arriving below it.
        assert!(painted.iter().any(|text| text.contains(&newest(1))));
        assert!(!painted
            .iter()
            .any(|text| text.contains(&newest(count + 100))));
        let notice = painted
            .iter()
            .find(|text| text.starts_with("Showing "))
            .expect("the window line is painted");
        assert!(notice.contains("and more is arriving."), "{notice}");
        // "End" brings the newest text back and follows it again.
        click_window_control(window, 41, "end", cx);
        shell.update(cx, |shell, cx| {
            shell.push_stream_delta(&delta(count + 101), cx);
            shell.finish_stream(cx);
        });
    });
    flush(cx);
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert!(painted(window)
            .iter()
            .any(|text| text.contains(&newest(count + 101))));
        assert_eq!(shell.read(cx).block_count(), 41);
    });
}

// ---- Text size -----------------------------------------------------------
//
// These set the shell's own text-size setting and the display scale factor in
// the test window. They are not observations of a macOS accessibility
// setting, and nobody looked at the result.

fn set_text_scale(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    percent: u16,
) {
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::from_percent(percent), window, cx)
        });
        settle_frames(window, cx);
    });
}

const SHELL_PARTS: [&str; 5] = [
    "header",
    "region-sidebar",
    "region-conversation",
    "region-inspector",
    "status",
];

#[gpui_kit::test]
fn at_200_percent_a_window_twice_as_wide_has_the_same_layout_at_twice_the_size(
    cx: &mut TestAppContext,
) {
    let (normal, normal_shell) = open(cx, WIDE, 200, 40);
    let first_row = step(cx, normal, |_, cx| {
        session_row_id(&normal_shell.read(cx).sidebar().rows()[0].id)
    });
    let measure = |cx: &mut TestAppContext, handle: WindowHandle<Root>| {
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            let mut bounds: Vec<Bounds<Pixels>> = SHELL_PARTS
                .iter()
                .map(|id| window.find(*id).bounds())
                .collect();
            bounds.push(window.find(first_row.clone()).bounds());
            let controls =
                ["composer-primary", "toggle-inspector"].map(|id| window.find(id).bounds());
            (bounds, controls)
        })
    };
    let (at_100, controls_100) = measure(cx, normal);

    let (large, large_shell) = open(cx, size(px(2880.), px(1800.)), 200, 40);
    set_text_scale(cx, large, &large_shell, 200);
    let (at_200, controls_200) = measure(cx, large);

    // Twice the size to within one pixel: rem-based sizes round per element.
    let doubled = |big: Pixels, small: Pixels| (big - small * 2.).abs() <= px(1.);
    for (index, (small, big)) in at_100.iter().zip(&at_200).enumerate() {
        let name = SHELL_PARTS
            .get(index)
            .copied()
            .unwrap_or("first session row");
        assert!(
            doubled(big.origin.x, small.origin.x)
                && doubled(big.origin.y, small.origin.y)
                && doubled(big.size.width, small.size.width)
                && doubled(big.size.height, small.size.height),
            "{name}: {small:?} -> {big:?}"
        );
    }
    // Kit controls scale with the text too. How exactly is the kit's
    // business; they must at least have grown clearly in both directions.
    for (small, big) in controls_100.iter().zip(&controls_200) {
        println!(
            "EVIDENCE text_scale_200 control: {:?} -> {:?}",
            small.size, big.size
        );
        assert!(big.size.height >= small.size.height * 1.5);
        assert!(big.size.width >= small.size.width * 1.5);
    }
}

#[gpui_kit::test]
fn at_200_percent_in_the_same_window_the_panels_give_way_and_controls_stay_reachable(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("ข้อความเดิม", cx);
    });
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        // 1440 px at 200% lays out like 720 px: neither panel is docked, and
        // nothing was dropped over the conversation.
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.try_find("region-inspector").is_none());
        assert!(window.try_find("overlay-backdrop").is_none());
        assert_eq!(
            window.find("region-conversation").bounds().size.width,
            px(1440.)
        );
        assert_eq!(window.find("header").bounds().size.height, px(112.));

        // Every control is whole and inside the window, in reading order.
        let viewport = Bounds {
            origin: Point::default(),
            size: WIDE,
        };
        let controls = [
            "toggle-sidebar",
            "toggle-inspector",
            "toggle-theme",
            "composer-stop",
            "composer-primary",
        ];
        for id in controls {
            let control = window.find(id);
            assert!(control.visible(), "{id}");
            let bounds = control.bounds();
            assert!(
                bounds.left() >= viewport.left()
                    && bounds.right() <= viewport.right()
                    && bounds.top() >= viewport.top()
                    && bounds.bottom() <= viewport.bottom(),
                "{id} is not inside the window: {bounds:?}"
            );
        }
        let header: Vec<_> = controls[..3]
            .iter()
            .map(|id| window.find(*id).bounds())
            .collect();
        assert!(header[0].right() <= header[1].left());
        assert!(header[1].right() <= header[2].left());

        // The draft and focus survived the change, and typing still works.
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        window.input(" ที่พิมพ์ต่อ", cx);
        assert_eq!(
            window.find_by_label("Message").value(),
            Some("ข้อความเดิม ที่พิมพ์ต่อ")
        );

        // The sidebar is still one keystroke away, at twice the size.
        window.press(TOGGLE_SIDEBAR, cx);
        let sidebar = window.find("region-sidebar").bounds();
        assert_eq!(sidebar.size.width, px(tokens::SIDEBAR_DEFAULT * 2.));
        let row = session_row_id(&shell.read(cx).sidebar().rows()[0].id);
        assert_eq!(window.find(row).bounds().size.height, px(112.));
        window.press("escape", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

// ---- Windows smaller than the text size needs ------------------------------
//
// The text keeps the size that was asked for. The shell is reflowed: every
// part is as wide as the window, the parts stack, and the shell scrolls up
// and down as one page. The test window has no real fonts, so these check
// the layout and the behaviour; `tests/render.rs` checks the same layout
// with real fonts.

/// 480 x 360: the binary's minimum window size.
const MINIMUM: Size<Pixels> = Size {
    width: px(480.),
    height: px(360.),
};

const REACHABLE_AT_ONCE: [&str; 7] = [
    "toggle-sidebar",
    "toggle-inspector",
    "toggle-theme",
    "composer-stop",
    "composer-primary",
    "region-composer",
    "status",
];

/// The parts of a reflowed page, top to bottom.
const PAGE_PARTS: [&str; 4] = ["header", "region-conversation", "region-composer", "status"];

/// Draw, and go on drawing while the shell asks for another frame, as a
/// real window does. Bringing a part of a reflowed page into view takes a
/// second frame, which the shell asks for.
fn settle_frames(window: &mut Window, cx: &mut App) {
    window.render_frame(cx);
    for _ in 0..4 {
        if window.simulate_next_frame(cx) == 0 {
            break;
        }
        window.render_frame(cx);
    }
}

/// The part of a reflowed page whose middle is in the window, to turn the
/// wheel over.
fn part_under_pointer(window: &Window, window_size: Size<Pixels>) -> &'static str {
    PAGE_PARTS
        .into_iter()
        .rev()
        .find(|id| {
            let middle = window.find(*id).bounds().center().y;
            middle > px(0.) && middle < window_size.height
        })
        .expect("some part of the page has its middle in the window")
}

/// Turn the wheel with the pointer over a part of a reflowed page that
/// does not scroll by itself: whichever of the bands above and below the
/// timeline has more of the window.
fn wheel_page(window: &mut Window, delta: f32, window_size: Size<Pixels>, cx: &mut App) {
    window.render_frame(cx);
    let timeline = window.find("timeline-area").bounds();
    let above = timeline.top().clamp(px(0.), window_size.height);
    let below = timeline.bottom().clamp(px(0.), window_size.height);
    let y = if above > window_size.height - below {
        above / 2.
    } else {
        (below + window_size.height) / 2.
    };
    let position = point(px(24.), y);
    assert!(
        !timeline.contains(&position),
        "no part of the page but the timeline is in the window"
    );
    wheel_at(window, position, delta, cx);
}

/// Turn the wheel with the pointer at a point in the window.
fn wheel_at(window: &mut Window, position: Point<Pixels>, delta: f32, cx: &mut App) {
    window.render_frame(cx);
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: window.modifiers(),
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(
        ScrollEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(0.), px(delta))),
            modifiers: window.modifiers(),
            ..Default::default()
        }
        .to_platform_input(),
        cx,
    );
    settle_frames(window, cx);
}

fn inside_window(window: &Window, id: &'static str, window_size: Size<Pixels>) -> bool {
    let bounds = window.find(id).bounds();
    bounds.left() >= px(0.)
        && bounds.right() <= window_size.width
        && bounds.top() >= px(0.)
        && bounds.bottom() <= window_size.height
}

/// The shell is fitted into its window the way the model says, at the text
/// size that was asked for, with nothing cut off or overlapping.
fn assert_fitted(
    window: &Window,
    shell: &Entity<Shell>,
    window_size: Size<Pixels>,
    cx: &App,
    context: &str,
) {
    let scale = shell.read(cx).text_scale();
    let fit = scale.fit(f32::from(window_size.width), f32::from(window_size.height));
    assert_eq!(shell.read(cx).fit(window), fit, "{context}");
    // The text is the size asked for, in a window of any size.
    assert_eq!(window.rem_size(), px(16. * scale.factor()), "{context}");
    match fit {
        Fit::Whole => {
            for id in REACHABLE_AT_ONCE {
                assert!(
                    inside_window(window, id, window_size),
                    "{id} is not inside the window: {context}"
                );
            }
            assert!(
                window.find("region-composer").bounds().bottom()
                    <= window.find("status").bounds().top(),
                "the status line overlaps the composer: {context}"
            );
            assert!(window.try_find("shell-page").is_none(), "{context}");
        }
        Fit::Reflowed => {
            // Nothing is cut off at either side.
            for id in PAGE_PARTS.into_iter().chain(REACHABLE_AT_ONCE) {
                let bounds = window.find(id).bounds();
                assert!(
                    bounds.left() >= px(0.) && bounds.right() <= window_size.width,
                    "{id} runs {:?}..{:?} in a window {:?} wide: {context}",
                    bounds.left(),
                    bounds.right(),
                    window_size.width
                );
            }
            // The parts stack in order and do not overlap.
            for pair in PAGE_PARTS.windows(2) {
                assert!(
                    window.find(pair[0]).bounds().bottom() <= window.find(pair[1]).bounds().top(),
                    "{} overlaps {}: {context}",
                    pair[0],
                    pair[1]
                );
            }
            // The page's end can be scrolled into the window.
            let page = shell.read(cx).page_metrics();
            let (_, last) = page.sections.last().expect("the page has parts");
            assert!(
                last.bottom() - page.scroll_max <= window_size.height,
                "the status line cannot be scrolled into the window: {context}"
            );
        }
    }
}

/// The composer holds focus and is wholly inside the window.
fn assert_composer_in_view(window: &Window, window_size: Size<Pixels>, context: &str) {
    assert_eq!(
        window.find_by_label("Message").focused(),
        Some(true),
        "{context}"
    );
    for id in ["region-composer", "composer-stop", "composer-primary"] {
        assert!(
            inside_window(window, id, window_size),
            "{id} is at {:?}, not inside the window: {context}",
            window.find(id).bounds()
        );
    }
}

#[gpui_kit::test]
fn a_window_too_small_for_the_text_size_keeps_the_size_and_reflows(cx: &mut TestAppContext) {
    let measured = ["toggle-sidebar", "composer-primary", "composer-stop"];
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_fitted(window, &shell, MINIMUM, cx, "100%");
        window.input("ร่างข้อความ", cx);
    });

    // 200% in a window made for 100%. The text is 200%; it is not given a
    // smaller size to make it fit.
    set_text_scale(cx, handle, &shell, 200);
    let reflowed_sizes = step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).text_scale().percent(), 200);
        assert_eq!(shell.read(cx).fit(window), Fit::Reflowed);
        assert_fitted(window, &shell, MINIMUM, cx, "200% in 480 x 360");
        // The draft and focus are untouched, the composer was brought into
        // view, and typing still works.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "ร่างข้อความ");
        assert_composer_in_view(window, MINIMUM, "after the change to 200%");
        window.input(" ต่อ", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "ร่างข้อความ ต่อ");
        measured.map(|id| window.find(id).bounds().size)
    });

    // The window grows a step at a time. The text never changes size; the
    // shell is whole as soon as the window holds it, and reflowed until
    // then, with the composer in view throughout.
    for (width, height, fit) in [
        (600., 450., Fit::Reflowed),
        (720., 540., Fit::Reflowed),
        (959., 719., Fit::Reflowed),
        (960., 720., Fit::Whole),
        (1440., 900., Fit::Whole),
        (1440., 500., Fit::Reflowed),
        (480., 900., Fit::Reflowed),
    ] {
        let window_size = size(px(width), px(height));
        cx.simulate_window_resize(handle.into(), window_size);
        step(cx, handle, |window, cx| {
            settle_frames(window, cx);
            let context = format!("{width} x {height}");
            assert_eq!(shell.read(cx).text_scale().percent(), 200, "{context}");
            assert_eq!(shell.read(cx).fit(window), fit, "{context}");
            assert_fitted(window, &shell, window_size, cx, &context);
            assert_composer_in_view(window, window_size, &context);
            assert_eq!(
                shell.read(cx).draft(cx).as_ref(),
                "ร่างข้อความ ต่อ",
                "{context}"
            );
            // Wide enough to dock panels, but reflowed: they stay overlays.
            assert!(window.try_find("region-sidebar").is_none() || fit == Fit::Whole);
            // Controls are as tall reflowed as where the shell is whole at
            // the same text size, and no narrower: the text was not shrunk.
            if (width, height) == (960., 720.) {
                for (id, reflowed) in measured.into_iter().zip(reflowed_sizes) {
                    let whole = window.find(id).bounds().size;
                    assert_eq!(reflowed.height, whole.height, "{id}");
                    assert!(reflowed.width >= whole.width, "{id}");
                }
            }
        });
    }

    // And shrinks again, all the way.
    cx.simulate_window_resize(handle.into(), MINIMUM);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_fitted(window, &shell, MINIMUM, cx, "shrunk back to 480 x 360");
        assert_composer_in_view(window, MINIMUM, "shrunk back to 480 x 360");
        window.input("!", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "ร่างข้อความ ต่อ!");
    });
}

#[gpui_kit::test]
fn every_part_of_a_reflowed_page_is_reached_from_the_keyboard(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        // Focus starts in the composer, at the foot of the page: the header
        // is out of view.
        assert_composer_in_view(window, MINIMUM, "at the start");
        assert!(!inside_window(window, "toggle-sidebar", MINIMUM));

        // Tab reaches each header control, and each is in view when it has
        // focus.
        for id in ["toggle-sidebar", "toggle-inspector", "toggle-theme"] {
            tab_to(window, id, cx);
            settle_frames(window, cx);
            assert!(
                inside_window(window, id, MINIMUM),
                "{id} has focus at {:?}",
                window.find(id).bounds()
            );
        }
        assert!(inside_window(window, "header", MINIMUM));

        // The status line holds no control. On a reflowed page it is a tab
        // stop itself, so the keyboard can bring it into view.
        tab_to(window, "status", cx);
        settle_frames(window, cx);
        assert!(inside_window(window, "status", MINIMUM));
        assert_eq!(
            shell.read(cx).focused_section(window, cx),
            Some(PageSection::Status)
        );

        // F6 goes round the regions as it does anywhere, and shows each.
        window.press("f6", cx);
        settle_frames(window, cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        assert!(inside_window(window, "region-conversation", MINIMUM));
        assert!(inside_window(window, "run-status", MINIMUM));
        window.press("f6", cx);
        settle_frames(window, cx);
        assert_composer_in_view(window, MINIMUM, "after F6");
    });

    // Where the shell is whole the status line is not a tab stop: there is
    // nothing to bring into view.
    cx.simulate_window_resize(handle.into(), size(px(1440.), px(900.)));
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).fit(window), Fit::Whole);
        for _ in 0..60 {
            window.press("tab", cx);
            assert_ne!(window.find("status").focused(), Some(true));
        }
    });
}

#[gpui_kit::test]
fn a_reflowed_page_stays_where_it_was_scrolled_until_focus_or_typing_moves_it(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    let scrolled = |cx: &App| shell.read(cx).page_metrics().scrolled;
    let at_composer = step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        window.input("draft", cx);
        settle_frames(window, cx);
        assert_composer_in_view(window, MINIMUM, "at the start");
        let at_composer = scrolled(cx);
        assert!(at_composer > px(0.));

        // The wheel, turned anywhere but over the timeline, back to the top.
        for _ in 0..20 {
            wheel_page(window, 100., MINIMUM, cx);
        }
        assert_eq!(scrolled(cx), px(0.));
        assert!(inside_window(window, "toggle-sidebar", MINIMUM));

        // Nothing brings it back while nothing changes: the user put it
        // there. Focus is still in the composer, which is out of view.
        for _ in 0..3 {
            settle_frames(window, cx);
        }
        assert_eq!(scrolled(cx), px(0.));
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert!(!inside_window(window, "composer-primary", MINIMUM));

        // Typing does: the text goes to the composer, and the composer
        // comes back into view.
        window.input("!", cx);
        at_composer
    });
    // (The shell hears of the change once this batch of input is over.)
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft!");
        assert_composer_in_view(window, MINIMUM, "after typing");
        assert_eq!(scrolled(cx), at_composer);

        // So does asking for the part that already has focus.
        for _ in 0..20 {
            wheel_page(window, 100., MINIMUM, cx);
        }
        assert_eq!(scrolled(cx), px(0.));
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        settle_frames(window, cx);
        assert_composer_in_view(window, MINIMUM, "after asking for the composer");

        // So does moving focus, even within the part that already had it.
        tab_to(window, "toggle-sidebar", cx);
        settle_frames(window, cx);
        assert_eq!(scrolled(cx), px(0.));
        for _ in 0..3 {
            wheel_page(window, -100., MINIMUM, cx);
        }
        assert!(!inside_window(window, "toggle-inspector", MINIMUM));
        tab_to(window, "toggle-inspector", cx);
        settle_frames(window, cx);
        assert!(inside_window(window, "toggle-inspector", MINIMUM));
    });
}

#[gpui_kit::test]
fn the_wheel_over_the_timeline_moves_the_page_once_the_timeline_is_at_its_end(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    cx.run_until_parked();
    let scrolled = |cx: &App| shell.read(cx).page_metrics().scrolled;
    let wheel = |window: &mut Window, delta: f32, cx: &mut App| {
        // The conversation fills the window, so its middle is the timeline.
        window.scroll(
            "region-conversation",
            ScrollDelta::Pixels(point(px(0.), px(delta))),
            cx,
        );
    };
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        settle_frames(window, cx);
        assert!(inside_window(window, "region-conversation", MINIMUM));
        let at_conversation = scrolled(cx);
        let max = shell.read(cx).page_metrics().scroll_max;
        assert!(at_conversation > px(0.) && at_conversation < max);

        // The timeline is at its newest block. Up: the timeline scrolls,
        // the page does not.
        wheel(window, 300., cx);
        assert_eq!(scrolled(cx), at_conversation);
        // Down again: first the timeline takes it, back to its end.
        wheel(window, -100., cx);
        assert_eq!(scrolled(cx), at_conversation);
        // From there on the page does, to its own end, where the status
        // line is. Without this the timeline would be a trap: the kit's
        // list keeps every wheel event that reaches it.
        let mut over_timeline = 0;
        for _ in 0..40 {
            if scrolled(cx) == max {
                break;
            }
            let over = part_under_pointer(window, MINIMUM);
            if over == "region-conversation" {
                over_timeline += 1;
            }
            window.scroll(over, ScrollDelta::Pixels(point(px(0.), px(-100.))), cx);
        }
        assert!(over_timeline > 0, "the wheel was never over the timeline");
        assert_eq!(scrolled(cx), max);
        assert!(inside_window(window, "status", MINIMUM));
    });

    // A timeline with nothing to scroll hands the wheel on in both
    // directions.
    let brief = vec![TimelineBlock::new(1, BlockKind::Assistant, "ok".into())];
    let (short, short_shell) = open_blocks(cx, MINIMUM, brief);
    set_text_scale(cx, short, &short_shell, 200);
    cx.run_until_parked();
    step(cx, short, |window, cx| {
        short_shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        settle_frames(window, cx);
        let at_conversation = short_shell.read(cx).page_metrics().scrolled;
        wheel(window, 60., cx);
        let up = short_shell.read(cx).page_metrics().scrolled;
        assert!(up < at_conversation, "{up:?} {at_conversation:?}");
        wheel(window, -60., cx);
        assert_eq!(
            short_shell.read(cx).page_metrics().scrolled,
            at_conversation
        );
    });
}

#[gpui_kit::test]
fn overlay_panels_fit_a_reflowed_window_and_their_contents_are_reachable(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        window.input("draft", cx);
        settle_frames(window, cx);
        assert_composer_in_view(window, MINIMUM, "at the start");

        // Sessions, opened from the composer with the page scrolled to it.
        // The panel covers the window and no more, whatever is under it.
        window.press(TOGGLE_SIDEBAR, cx);
        settle_frames(window, cx);
        let panel = window.find("region-sidebar");
        assert_eq!(panel.role(), Some(Role::Dialog));
        assert_eq!(panel.focused(), Some(true));
        assert_eq!(
            panel.bounds(),
            Bounds {
                origin: Point::default(),
                size: MINIMUM
            }
        );
        // Its list and its destinations are inside it, and the list works.
        let first = session_row_id(&shell.read(cx).sidebar().rows()[0].id);
        assert!(window.find(first).bounds().right() <= MINIMUM.width);
        for destination in ["Chat", "Tools", "Proposals"] {
            let id = gpui_kit::SharedString::from(format!("destination-{destination}"));
            let bounds = window.find(id).bounds();
            assert!(
                bounds.left() >= px(0.)
                    && bounds.right() <= MINIMUM.width
                    && bounds.bottom() <= MINIMUM.height,
                "{destination}: {bounds:?}"
            );
        }
        window.press("down", cx);
        assert_eq!(shell.read(cx).sidebar().selected_index(), Some(1));

        // Escape gives focus back to the composer, in view.
        window.press("escape", cx);
        settle_frames(window, cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert_composer_in_view(window, MINIMUM, "after Sessions");

        // Context. Its details are taller than the window, so with focus
        // in it the arrow and page keys scroll it.
        window.press(TOGGLE_INSPECTOR, cx);
        settle_frames(window, cx);
        let panel = window.find("region-inspector");
        assert_eq!(panel.role(), Some(Role::Dialog));
        assert_eq!(panel.focused(), Some(true));
        assert_eq!(
            panel.bounds(),
            Bounds {
                origin: Point::default(),
                size: MINIMUM
            }
        );
        assert!(!inside_window(window, "inspector-last-message", MINIMUM));
        let before = window.find("inspector-last-message").bounds().top();
        window.press("down", cx);
        settle_frames(window, cx);
        let after_line = window.find("inspector-last-message").bounds().top();
        assert_eq!(before - after_line, px(80.), "one line at 200%");
        for _ in 0..6 {
            window.press("pagedown", cx);
            settle_frames(window, cx);
        }
        assert!(
            inside_window(window, "inspector-last-message", MINIMUM),
            "{:?}",
            window.find("inspector-last-message").bounds()
        );
        for _ in 0..6 {
            window.press("pageup", cx);
            settle_frames(window, cx);
        }
        assert_eq!(window.find("inspector-last-message").bounds().top(), before);

        window.press("escape", cx);
        settle_frames(window, cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_composer_in_view(window, MINIMUM, "after Context");
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft");
    });
}

#[gpui_kit::test]
fn the_wheel_over_a_modal_panel_never_moves_the_page_under_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    cx.run_until_parked();
    let scrolled = |cx: &App| shell.read(cx).page_metrics().scrolled;
    let middle = point(MINIMUM.width / 2., MINIMUM.height / 2.);
    // With the page under the overlay scrolled to each of its parts. With
    // the conversation in view, the middle of the window is the timeline,
    // at its newest block: the place where a wheel turned downwards is
    // handed on to the page when nothing is in front of it.
    for section in [
        PageSection::Conversation,
        PageSection::Composer,
        PageSection::Header,
    ] {
        step(cx, handle, |window, cx| {
            shell.update(cx, |shell, cx| shell.focus_section(section, window, cx));
            settle_frames(window, cx);
            let before = scrolled(cx);

            // Context. The wheel belongs to the panel: its details scroll
            // and the page under it stays exactly where it was, at the
            // first turn, at the panel's end and well past it, and all the
            // way back past its start.
            window.press(TOGGLE_INSPECTOR, cx);
            settle_frames(window, cx);
            assert_eq!(window.find("region-inspector").focused(), Some(true));
            let detail = window.find("inspector-last-message").bounds().top();
            window.scroll(
                "region-inspector",
                ScrollDelta::Pixels(point(px(0.), px(-100.))),
                cx,
            );
            settle_frames(window, cx);
            assert_eq!(scrolled(cx), before, "modal wheel moved background");
            assert_eq!(
                window.find("inspector-last-message").bounds().top(),
                detail - px(100.),
                "the wheel did not scroll the panel"
            );
            for delta in [-100., 100.] {
                for turn in 0..12 {
                    wheel_at(window, middle, delta, cx);
                    assert_eq!(
                        scrolled(cx),
                        before,
                        "{section:?}: Context, turn {turn} of {delta}"
                    );
                }
            }
            assert_eq!(window.find("inspector-last-message").bounds().top(), detail);
            window.press("escape", cx);
            settle_frames(window, cx);
            assert!(window.try_find("region-inspector").is_none());
            assert_eq!(scrolled(cx), before, "{section:?}: after Context");

            // Sessions. The same, with a list that scrolls a long way.
            window.press(TOGGLE_SIDEBAR, cx);
            settle_frames(window, cx);
            assert_eq!(window.find("region-sidebar").focused(), Some(true));
            let first = session_row_id(&shell.read(cx).sidebar().rows()[0].id);
            let row = window.find(first.clone()).bounds();
            wheel_at(window, row.center(), -100., cx);
            assert_eq!(scrolled(cx), before, "modal wheel moved background");
            assert!(
                window
                    .try_find(first.clone())
                    .is_none_or(|moved| moved.bounds().top() < row.top()),
                "the wheel did not scroll the list"
            );
            // Further up than it went down, to end at the list's start.
            for (delta, turns) in [(-100., 12), (100., 14)] {
                for turn in 0..turns {
                    wheel_at(window, row.center(), delta, cx);
                    assert_eq!(
                        scrolled(cx),
                        before,
                        "{section:?}: Sessions, turn {turn} of {delta}"
                    );
                }
            }
            // Back at its start, and still not handed on from there.
            assert_eq!(window.find(first).bounds().top(), row.top());
            window.press("escape", cx);
            settle_frames(window, cx);
            assert!(window.try_find("region-sidebar").is_none());
            assert_eq!(scrolled(cx), before, "{section:?}: after Sessions");
        });
    }

    // With nothing in front of it the timeline hands the wheel on as
    // before: the overlays coming and going changed nothing about that.
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        settle_frames(window, cx);
        let before = scrolled(cx);
        assert!(window.find("timeline-area").bounds().contains(&middle));
        wheel_at(window, middle, -100., cx);
        assert_eq!(scrolled(cx), before + px(100.));
        // Upwards the timeline has blocks to show, so it takes the wheel.
        wheel_at(window, middle, 60., cx);
        assert_eq!(scrolled(cx), before + px(100.));
    });
}

/// The independent review's test of `61e1958`, as written there apart from
/// formatting. It failed: the page under the Context panel went from 213 px
/// to 313 px.
#[gpui_kit::test]
fn modal_wheel_does_not_scroll_background(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Conversation, window, cx)
        });
        settle_frames(window, cx);
        window.press(TOGGLE_INSPECTOR, cx);
        settle_frames(window, cx);
        let before = shell.read(cx).page_metrics().scrolled;
        window.scroll(
            "region-inspector",
            ScrollDelta::Pixels(point(px(0.), px(-100.))),
            cx,
        );
        settle_frames(window, cx);
        assert_eq!(
            shell.read(cx).page_metrics().scrolled,
            before,
            "modal wheel moved background"
        );
    });
}

#[gpui_kit::test]
fn the_wheel_over_a_modal_backdrop_moves_nothing(cx: &mut TestAppContext) {
    // 700 x 500 at 200%: reflowed, and wider than the Sessions panel, so
    // some of the backdrop shows beside it.
    let window_size = size(px(700.), px(500.));
    let (handle, shell) = open(cx, window_size, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        settle_frames(window, cx);
        let before = shell.read(cx).page_metrics().scrolled;
        window.press(TOGGLE_SIDEBAR, cx);
        settle_frames(window, cx);
        let panel = window.find("region-sidebar").bounds();
        assert!(panel.right() < window_size.width - px(40.), "{panel:?}");
        // Over the backdrop, with the timeline under it.
        let beside = point(panel.right() + px(20.), window_size.height / 2.);
        assert!(window.find("timeline-area").bounds().contains(&beside));
        let first = session_row_id(&shell.read(cx).sidebar().rows()[0].id);
        let row = window.find(first.clone()).bounds().top();
        for delta in [-100., 100.] {
            for _ in 0..6 {
                wheel_at(window, beside, delta, cx);
                assert_eq!(shell.read(cx).page_metrics().scrolled, before);
                assert_eq!(window.find(first.clone()).bounds().top(), row);
            }
        }
        // The overlay is still up and still has focus.
        assert!(window.find("overlay-backdrop").visible());
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn a_wide_but_short_window_opens_its_panels_as_overlays(cx: &mut TestAppContext) {
    // 2000 px at 200% lays out like 1000 px, wide enough to dock the
    // sidebar. But the window is too short for 200%, so it is reflowed, and
    // a reflowed page has no room beside it: the panel is an overlay.
    let window_size = size(px(2000.), px(600.));
    let (handle, shell) = open(cx, window_size, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).fit(window), Fit::Reflowed);
        assert_fitted(window, &shell, window_size, cx, "2000 x 600 at 200%");
        assert!(window.try_find("region-sidebar").is_none());

        window.press(TOGGLE_SIDEBAR, cx);
        settle_frames(window, cx);
        let panel = window.find("region-sidebar");
        assert_eq!(panel.role(), Some(Role::Dialog));
        assert_eq!(panel.focused(), Some(true));
        assert!(window.find("overlay-backdrop").visible());
        assert!(inside_window(window, "region-sidebar", window_size));
        window.press("escape", cx);
        settle_frames(window, cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert_composer_in_view(window, window_size, "after Sessions");

        // Tall enough again: the shell is whole and the sidebar docks.
    });
    let whole = size(px(2000.), px(800.));
    cx.simulate_window_resize(handle.into(), whole);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).fit(window), Fit::Whole);
        assert_eq!(window.find("region-sidebar").role(), Some(Role::Navigation));
        assert!(window.try_find("overlay-backdrop").is_none());
    });
}

#[gpui_kit::test]
fn a_destination_without_a_view_is_reflowed_too(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        window.press(TOGGLE_SIDEBAR, cx);
        settle_frames(window, cx);
        window.click("destination-Tools", cx);
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).destination(), Destination::Tools);
        // No composer here: the page is header, notice, status line.
        assert!(window.try_find("region-composer").is_none());
        let page = shell.read(cx).page_metrics();
        assert_eq!(
            page.sections
                .iter()
                .map(|(part, _)| *part)
                .collect::<Vec<_>>(),
            [
                PageSection::Header,
                PageSection::Conversation,
                PageSection::Status
            ]
        );
        for id in ["header", "region-conversation", "back-to-chat", "status"] {
            let bounds = window.find(id).bounds();
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= MINIMUM.width,
                "{id}: {bounds:?}"
            );
        }
        // The way back is reachable from the keyboard, and in view.
        tab_to(window, "back-to-chat", cx);
        settle_frames(window, cx);
        assert!(inside_window(window, "back-to-chat", MINIMUM));
        tab_to(window, "status", cx);
        settle_frames(window, cx);
        assert!(inside_window(window, "status", MINIMUM));
        tab_to(window, "back-to-chat", cx);
        settle_frames(window, cx);
        window.click("back-to-chat", cx);
    });
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_fitted(window, &shell, MINIMUM, cx, "back in Chat");
    });
}

#[gpui_kit::test]
fn a_stream_into_a_reflowed_window_shows_its_newest_text(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, MINIMUM, 20, 40);
    set_text_scale(cx, handle, &shell, 200);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        window.input("draft", cx);
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            for index in 0..600 {
                shell.push_stream_delta(&format!("คำ #{index:04}# "), cx);
            }
        });
    });
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        // The composer kept focus and its place; the newest text is in the
        // strip of timeline above it.
        assert_composer_in_view(window, MINIMUM, "while streaming");
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft");
        assert!(painted(window).iter().any(|text| text.contains("#0599#")));
        assert_fitted(window, &shell, MINIMUM, cx, "while streaming");
    });
}

#[gpui_kit::test]
fn raising_the_text_size_asks_the_window_to_grow_to_fit_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, MINIMUM, 200, 40);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(window.bounds().size, MINIMUM);
        // The shortcut asks for 125%, which needs 600 x 450 to be whole.
        window.press(&chord("="), cx);
        assert_eq!(shell.read(cx).text_scale().percent(), 125);
        assert_eq!(window.bounds().size, size(px(600.), px(450.)));
        // The test window does not deliver the resize the way a real one
        // does, so the view still has its old size here. Until a resize
        // arrives the shell is reflowed, at 125%.
        settle_frames(window, cx);
        assert_fitted(window, &shell, MINIMUM, cx, "before the resize arrives");
        assert_eq!(shell.read(cx).fit(window), Fit::Reflowed);
        assert_composer_in_view(window, MINIMUM, "before the resize arrives");
    });
    // The resize arrives.
    let grown = size(px(600.), px(450.));
    cx.simulate_window_resize(handle.into(), grown);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).fit(window), Fit::Whole);
        assert_fitted(window, &shell, grown, cx, "after the resize");
        // A window already large enough is left alone.
        window.press(&chord("-"), cx);
        assert_eq!(window.bounds().size, grown);
    });
}

#[gpui_kit::test]
fn repeated_text_size_changes_and_resizes_leave_the_shell_consistent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        window.input("draft", cx);
    });
    // A long, irregular run of size changes with resizes in between and no
    // settling time: the same batch of events, over and over. Many of the
    // combinations are reflowed.
    let sizes = [
        size(px(1440.), px(900.)),
        size(px(700.), px(500.)),
        size(px(480.), px(360.)),
        size(px(2000.), px(1200.)),
        size(px(960.), px(720.)),
        size(px(1440.), px(400.)),
        size(px(480.), px(1000.)),
    ];
    let mut reflowed_rounds = 0;
    for round in 0..56usize {
        step(cx, handle, |window, cx| {
            let keys: &[&str] = match round % 4 {
                0 => &["=", "="],
                1 => &["-"],
                2 => &["=", "=", "="],
                _ => &["0", "="],
            };
            for key in keys {
                window.press(&chord(key), cx);
            }
        });
        let window_size = sizes[round % sizes.len()];
        cx.simulate_window_resize(handle.into(), window_size);
        step(cx, handle, |window, cx| {
            settle_frames(window, cx);
            let context = format!(
                "round {round}: {window_size:?} at {}%",
                shell.read(cx).text_scale().percent()
            );
            assert_fitted(window, &shell, window_size, cx, &context);
            if shell.read(cx).fit(window) == Fit::Reflowed {
                reflowed_rounds += 1;
            }
            // Whatever happened, the draft is whole and the composer has
            // focus and is in view.
            assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft", "{context}");
            assert_composer_in_view(window, window_size, &context);
        });
    }
    assert!(reflowed_rounds >= 20, "{reflowed_rounds} reflowed rounds");
}

#[gpui_kit::test]
fn a_text_size_change_while_an_overlay_is_open_keeps_the_overlay_and_its_focus(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(1100.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft", cx);
        window.press(TOGGLE_INSPECTOR, cx);
        assert_eq!(window.find("region-inspector").focused(), Some(true));
        assert!(window.find("region-sidebar").visible());

        // Larger text: the window now lays out like a narrower one, so the
        // docked sidebar gives way. The overlay stays up with focus in it.
        window.press(&chord("="), cx);
        window.press(&chord("="), cx);
        assert_eq!(shell.read(cx).text_scale().percent(), 150);
        assert_eq!(shell.read(cx).fit(window), Fit::Whole);
        assert!(window.try_find("region-sidebar").is_none());
        assert!(window.find("overlay-backdrop").visible());
        assert_eq!(window.find("region-inspector").focused(), Some(true));

        // Dismissing it still returns focus to where it came from.
        window.press("escape", cx);
        assert!(window.try_find("region-inspector").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft");

        // Back to the default size: the sidebar docks again, and the overlay
        // that was dismissed does not come back.
        window.press(&chord("0"), cx);
        assert!(window.find("region-sidebar").visible());
        assert!(window.try_find("overlay-backdrop").is_none());
    });
}

#[gpui_kit::test]
fn a_text_size_change_in_the_middle_of_a_stream_loses_nothing(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(1440.), px(1200.)), 20, 40);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            for index in 0..200 {
                shell.push_stream_delta(&format!("คำ #{index:04}# "), cx);
            }
        });
        // Before those deltas have reached the view.
        window.press(&chord("="), cx);
        shell.update(cx, |shell, cx| {
            for index in 200..400 {
                shell.push_stream_delta(&format!("คำ #{index:04}# "), cx);
            }
        });
        window.press(&chord("="), cx);
    });
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(shell.read(cx).text_scale().percent(), 150);
        assert_eq!(shell.read(cx).block_count(), 41);
        assert!(window.find(("block", 41usize)).visible());
        let painted = painted(window);
        assert!(painted
            .iter()
            .any(|text| text.contains("#0000#") && text.contains("#0399#")));
    });
}

#[gpui_kit::test]
fn text_size_shortcuts_step_up_to_200_percent_and_reset(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft", cx);
        let mut seen = vec![shell.read(cx).text_scale().percent()];
        for _ in 0..6 {
            window.press(&chord("="), cx);
            seen.push(shell.read(cx).text_scale().percent());
        }
        // It stops at 200% however often the shortcut is pressed.
        assert_eq!(seen, [100, 125, 150, 175, 200, 200, 200]);
        assert_eq!(window.find("header").bounds().size.height, px(112.));

        window.press(&chord("-"), cx);
        assert_eq!(shell.read(cx).text_scale().percent(), 175);
        window.press(&chord("0"), cx);
        assert_eq!(shell.read(cx).text_scale(), TextScale::DEFAULT);
        assert_eq!(window.find("header").bounds().size.height, px(56.));
        // Back to three panes, with the draft and focus untouched.
        assert!(window.find("region-inspector").visible());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft");
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn the_display_scale_factor_does_not_change_the_logical_layout(cx: &mut TestAppContext) {
    let (handle, _) = open(cx, WIDE, 200, 40);
    let measure = |cx: &mut TestAppContext| {
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            SHELL_PARTS.map(|id| window.find(id).bounds())
        })
    };
    let before = measure(cx);
    cx.simulate_window_scale_factor_change(handle.into(), 2.0);
    assert_eq!(measure(cx), before);
    cx.simulate_window_scale_factor_change(handle.into(), 1.0);
    assert_eq!(measure(cx), before);
}

// ---- Thai ----------------------------------------------------------------

#[gpui_kit::test]
fn word_wise_caret_movement_over_thai_is_recorded(cx: &mut TestAppContext) {
    #[cfg(target_os = "macos")]
    const WORD_LEFT: &str = "alt-left";
    #[cfg(not(target_os = "macos"))]
    const WORD_LEFT: &str = "ctrl-left";

    // Seven words, no spaces: ภาษา ไทย ไม่มี ช่อง ว่าง ระหว่าง คำ
    let sentence = "ภาษาไทยไม่มีช่องว่างระหว่างคำ";
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input(sentence, cx);
        assert_eq!(
            shell.read(cx).composer_selection(cx),
            sentence.len()..sentence.len()
        );
        let mut stops = Vec::new();
        for _ in 0..60 {
            window.press(WORD_LEFT, cx);
            let caret = shell.read(cx).composer_selection(cx).start;
            if stops.last() == Some(&caret) {
                break;
            }
            stops.push(caret);
        }
        println!(
            "EVIDENCE thai_word_left: {} presses to cross {} characters in 7 words; stops at \
             byte offsets {stops:?}",
            stops.len(),
            sentence.chars().count()
        );
        // Recorded, not judged: the kit decides where a "word" starts. It must
        // at least reach the start of the text and stop on character
        // boundaries.
        assert_eq!(stops.last(), Some(&0));
        assert!(stops.iter().all(|stop| sentence.is_char_boundary(*stop)));
    });
}

// ---- Command registry -----------------------------------------------------

/// The shell's command state, read the way it is read for running one.
fn command_entry(
    shell: &Entity<Shell>,
    command: Command,
    window: &mut Window,
    cx: &mut App,
) -> commands::Entry {
    shell.update(cx, |shell, cx| {
        commands::entry(command, &shell.command_state(window, cx))
    })
}

#[gpui_kit::test]
fn header_buttons_show_their_registry_entry_and_shortcuts_run_the_same_command(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        for (id, command) in [
            ("toggle-sidebar", Command::ToggleSessions),
            ("toggle-inspector", Command::ToggleContext),
            ("toggle-theme", Command::ToggleTheme),
        ] {
            let entry = command_entry(&shell, command, window, cx);
            assert_eq!(window.find(id).label(), Some(entry.label), "{id}");
        }
        // Each panel's shortcuts and its button run the same command, and
        // the entry says whether the panel is shown.
        for (command, panel, button) in [
            (Command::ToggleSessions, "region-sidebar", "toggle-sidebar"),
            (
                Command::ToggleContext,
                "region-inspector",
                "toggle-inspector",
            ),
        ] {
            let shown = |window: &mut Window, cx: &mut App| {
                window.render_frame(cx);
                let checked = command_entry(&shell, command, window, cx).checked;
                assert_eq!(checked, Some(window.try_find(panel).is_some()), "{panel}");
                checked
            };
            assert_eq!(shown(window, cx), Some(true));
            for key in commands::keys(command) {
                window.press(key, cx);
                assert_eq!(shown(window, cx), Some(false), "{key}");
                window.press(key, cx);
                assert_eq!(shown(window, cx), Some(true), "{key}");
            }
            window.click(button, cx);
            assert_eq!(shown(window, cx), Some(false), "{button}");
            window.click(button, cx);
            assert_eq!(shown(window, cx), Some(true), "{button}");
        }
        // The theme button is a command too.
        let dark = command_entry(&shell, Command::ToggleTheme, window, cx).label;
        window.click("toggle-theme", cx);
        window.render_frame(cx);
        assert_ne!(window.find("toggle-theme").label(), Some(dark));
    });
}

#[gpui_kit::test]
fn every_text_size_shortcut_runs_its_command_and_stops_at_the_limits(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let run = |command, window: &mut Window, cx: &mut App| {
            shell.update(cx, |shell, cx| shell.run_command(command, window, cx))
        };
        assert_eq!(
            run(Command::DecreaseTextSize, window, cx),
            Some(commands::SMALLEST)
        );
        assert_eq!(
            run(Command::ResetTextSize, window, cx),
            Some(commands::DEFAULT_SIZE)
        );
        for key in commands::keys(Command::IncreaseTextSize) {
            let before = shell.read(cx).text_scale();
            window.press(key, cx);
            assert_eq!(shell.read(cx).text_scale(), before.larger(), "{key}");
        }
        while run(Command::IncreaseTextSize, window, cx).is_none() {}
        assert_eq!(shell.read(cx).text_scale(), TextScale::LARGEST);
        assert_eq!(
            run(Command::IncreaseTextSize, window, cx),
            Some(commands::LARGEST)
        );
        window.press(commands::keys(Command::DecreaseTextSize)[0], cx);
        assert_eq!(shell.read(cx).text_scale(), TextScale::LARGEST.smaller());
        window.press(commands::keys(Command::ResetTextSize)[0], cx);
        assert_eq!(shell.read(cx).text_scale(), TextScale::DEFAULT);
    });
}

#[gpui_kit::test]
fn composer_buttons_take_label_and_availability_from_the_registry(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let check = |window: &mut Window, cx: &mut App| {
            window.render_frame(cx);
            for (id, command) in [
                ("composer-primary", Command::Send),
                ("composer-secondary", Command::SendSecondary),
                ("composer-stop", Command::Stop),
            ] {
                let entry = command_entry(&shell, command, window, cx);
                match window.try_find(id) {
                    Some(button) => {
                        assert_eq!(button.label(), Some(entry.label), "{id}");
                        // The kit reports `disabled` only when it is set.
                        assert_eq!(button.disabled() == Some(true), !entry.enabled(), "{id}");
                    }
                    // Only the second action is ever left out, while idle.
                    None => assert_eq!(entry.disabled_reason, Some(commands::NO_SECONDARY)),
                }
            }
        };
        check(window, cx);
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(Command::Stop, window, cx)),
            Some(commands::NO_RUN)
        );
        window.click(session_row_id("fixture-session-7"), cx);
        check(window, cx);
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.input("ok", cx);
        check(window, cx);
        // Somewhere without a composer, none of them can run.
        window.click("destination-Tools", cx);
        for command in [Command::Send, Command::SendSecondary, Command::Stop] {
            assert_eq!(
                shell.update(cx, |shell, cx| shell.run_command(command, window, cx)),
                Some(commands::NO_COMPOSER)
            );
        }
        assert_eq!(shell.read(cx).last_intent(), None);
    });
}

#[gpui_kit::test]
fn nothing_is_recorded_while_an_input_method_is_composing(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let ime = |cx: &mut TestAppContext, act: &dyn Fn(&mut TestInput, &mut App)| {
        step(cx, handle, |window, cx| {
            let bounds = window.find_by_label("Message").bounds();
            let composer = shell.read(cx).composer().clone();
            act(
                &mut TestInput::new(ElementInputHandler::new(bounds, composer), window),
                cx,
            );
        });
        cx.run_until_parked();
    };
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("ส่ง ", cx);
    });
    ime(cx, &|input, cx| {
        input.compose_text(None, "にほんご", None, cx)
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let send = command_entry(&shell, Command::Send, window, cx);
        assert_eq!(send.disabled_reason, Some(commands::COMPOSING));
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        // Enter that reaches the shell mid-composition records nothing and
        // says why; the composed text is left exactly as it is.
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(Command::Send, window, cx)),
            Some(commands::COMPOSING)
        );
        assert_eq!(shell.read(cx).last_intent(), None);
        assert_eq!(
            shell.read(cx).composer_notice(),
            Some("Not sent: Finish composing text first.")
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "ส่ง にほんご");
    });
    ime(cx, &|input, cx| input.commit_text("日本語", cx));
    step(cx, handle, |window, cx| {
        assert_eq!(
            command_entry(&shell, Command::Send, window, cx).disabled_reason,
            None
        );
        window.click("composer-primary", cx);
        let record = shell.read(cx).last_intent().cloned().expect("recorded");
        assert_eq!(record.kind, IntentKind::Send(SendMode::Send));
        assert_eq!(record.content_chars, "ส่ง 日本語".chars().count());
    });
}

// ---- Command palette ------------------------------------------------------

fn row_id(index: usize) -> gpui_kit::SharedString {
    format!("palette-row-{index}").into()
}

fn palette_key() -> &'static str {
    commands::keys(Command::ShowCommands)[0]
}

fn palette_status(window: &Window) -> String {
    window
        .find("palette-status")
        .value()
        .expect("the palette status has a value")
        .to_string()
}

fn palette_titles(window: &Window) -> Vec<String> {
    (0..)
        .map_while(|index| window.try_find(row_id(index)))
        .map(|row| row.label().unwrap_or_default().to_string())
        .collect()
}

#[gpui_kit::test]
fn the_palette_finds_and_runs_a_command_then_hands_focus_back(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert!(!shell.read(cx).background_inert(window));

        window.press(palette_key(), cx);
        window.render_frame(cx);
        assert_eq!(window.find("palette").role(), Some(Role::Dialog));
        assert_eq!(
            window.find_by_label("Search commands").focused(),
            Some(true)
        );
        // Everything else is out of the accessibility tree meanwhile.
        assert!(shell.read(cx).background_inert(window));
        let all = palette_titles(window);
        assert!(all.len() >= 15, "{all:?}");
        assert!(!all.iter().any(|title| title == "Show all commands"));
        assert!(palette_status(window).starts_with(&format!("1 of {} commands: ", all.len())));

        window.input("larger", cx);
        window.render_frame(cx);
        assert_eq!(palette_titles(window), ["Larger text"]);
        assert_eq!(window.find("palette-row-0").selected(), Some(true));
        let shortcut = commands::shortcut_text(commands::keys(Command::IncreaseTextSize)[0]);
        assert_eq!(
            palette_status(window),
            format!("1 of 1 commands: Larger text, {shortcut}.")
        );

        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(shell.read(cx).text_scale(), TextScale::DEFAULT.larger());
        assert!(!shell.read(cx).background_inert(window));
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn arrows_choose_a_row_and_an_unavailable_command_says_why(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(palette_key(), cx);
        window.input("text size", cx);
        window.render_frame(cx);
        assert_eq!(
            palette_titles(window),
            [
                "Larger text",
                "Smaller text",
                "Default text size",
                "Save desktop preferences"
            ]
        );
        // Up at the top stays there; Down moves one row and no further
        // than the last.
        window.press("up", cx);
        window.render_frame(cx);
        assert!(palette_status(window).starts_with("1 of 4 commands: Larger text"));
        window.press("down", cx);
        window.render_frame(cx);
        assert_eq!(window.find("palette-row-1").selected(), Some(true));
        assert_eq!(window.find("palette-row-0").selected(), Some(false));
        assert!(palette_status(window).starts_with("2 of 4 commands: Smaller text"));
        assert!(palette_status(window).ends_with(&format!("unavailable: {}.", commands::SMALLEST)));
        assert_eq!(
            window.find("palette-row-1").description(),
            Some(format!("Unavailable: {}", commands::SMALLEST).as_str())
        );
        for _ in 0..5 {
            window.press("down", cx);
        }
        window.render_frame(cx);
        assert!(palette_status(window).starts_with("4 of 4 commands: Save desktop preferences"));
        window.press("up", cx);
        window.render_frame(cx);
        assert!(palette_status(window).starts_with("3 of 4 commands: Default text size"));

        // Enter on an unavailable command: nothing runs, the palette stays
        // and says why.
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_some());
        assert_eq!(
            palette_status(window),
            format!(
                "Default text size is unavailable: {}.",
                commands::DEFAULT_SIZE
            )
        );
        assert_eq!(shell.read(cx).text_scale(), TextScale::DEFAULT);

        // A click runs a row too.
        window.click("palette-row-0", cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(shell.read(cx).text_scale(), TextScale::DEFAULT.larger());

        // Nothing matches: the status says so and Enter does nothing.
        window.press(palette_key(), cx);
        window.input("zzz", cx);
        window.render_frame(cx);
        assert!(palette_titles(window).is_empty());
        assert_eq!(
            palette_status(window),
            "No command matches \u{201c}zzz\u{201d}."
        );
        window.press("enter", cx);
        assert!(window.try_find("palette").is_some());
    });
}

#[gpui_kit::test]
fn escape_the_backdrop_and_the_shortcut_close_the_palette_onto_the_control_that_opened_it(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "show-commands", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find_by_label("Search commands").focused(),
            Some(true)
        );
        let entry = command_entry(&shell, Command::ShowCommands, window, cx);
        assert_eq!(entry.checked, Some(true));

        window.press("escape", cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(window.find("show-commands").focused(), Some(true));

        window.press("enter", cx);
        window.render_frame(cx);
        window.click_at("palette-backdrop", point(px(20.), px(800.)), cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(window.find("show-commands").focused(), Some(true));

        // The shortcut closes it as well, from inside the search field.
        window.press("enter", cx);
        window.press(palette_key(), cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(window.find("show-commands").focused(), Some(true));
        assert_eq!(
            command_entry(&shell, Command::ShowCommands, window, cx).checked,
            Some(false)
        );
    });
}

#[gpui_kit::test]
fn f6_stays_in_the_palette_and_a_panel_overlay_gives_way_to_it(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    cx.simulate_window_resize(handle.into(), size(px(720.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        // A Sessions overlay, opened from the composer.
        window.press(TOGGLE_SIDEBAR, cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        // The palette replaces it: one modal at a time.
        window.press(palette_key(), cx);
        window.render_frame(cx);
        assert!(window.try_find("region-sidebar").is_none());
        assert_eq!(
            window.find_by_label("Search commands").focused(),
            Some(true)
        );
        for key in ["f6", "shift-f6"] {
            window.press(key, cx);
            assert_eq!(
                window.find_by_label("Search commands").focused(),
                Some(true),
                "{key}"
            );
        }
        assert!(shell.read(cx).background_inert(window));
        // Closing goes back to where focus was before either modal.
        window.press("escape", cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));

        // A panel command from the palette opens the overlay as its
        // toggle would, with focus in it, and Escape then returns to the
        // composer.
        window.press(palette_key(), cx);
        window.input("show sessions", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(window.find("region-sidebar").focused(), Some(true));
        window.press("escape", cx);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn sending_from_the_palette_records_the_draft_with_focus_back_in_the_composer(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("สวัสดี", cx);
        window.press(palette_key(), cx);
        window.input("send", cx);
        window.render_frame(cx);
        assert_eq!(
            palette_titles(window).first().map(String::as_str),
            Some("Send")
        );
        window.press("enter", cx);
        let record = shell.read(cx).last_intent().cloned().expect("recorded");
        assert_eq!(record.kind, IntentKind::Send(SendMode::Send));
        assert_eq!(record.content_chars, "สวัสดี".chars().count());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        // The draft stays, as for the Send button.
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "สวัสดี");
    });
}

#[gpui_kit::test]
fn the_palette_search_field_keeps_an_input_method_composition(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press(palette_key(), cx);
        window.render_frame(cx);
    });
    let ime = |cx: &mut TestAppContext, act: &dyn Fn(&mut TestInput, &mut App)| {
        step(cx, handle, |window, cx| {
            let bounds = window.find_by_label("Search commands").bounds();
            let query = shell.read(cx).palette_query().expect("open").clone();
            act(
                &mut TestInput::new(ElementInputHandler::new(bounds, query), window),
                cx,
            );
        });
        cx.run_until_parked();
    };
    let marked = |cx: &mut TestAppContext| {
        step(cx, handle, |window, cx| {
            let query = shell.read(cx).palette_query().expect("open").clone();
            query.update(cx, |query, cx| {
                gpui_kit::EntityInputHandler::marked_text_range(query, window, cx).is_some()
            })
        })
    };
    ime(cx, &|input, cx| {
        input.compose_text(None, "てきすと", None, cx)
    });
    assert!(marked(cx));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let before = palette_status(window);
        // The arrows, Enter and Escape belong to the input method while it
        // composes: nothing is chosen, run or closed.
        window.press("down", cx);
        window.render_frame(cx);
        assert_eq!(palette_status(window), before);
        window.press("enter", cx);
        assert!(window.try_find("palette").is_some());
        assert_eq!(shell.read(cx).text_scale(), TextScale::DEFAULT);
    });
    // Enter ended the composition and kept its text.
    assert!(!marked(cx));
    ime(cx, &|input, cx| input.compose_text(None, "か", None, cx));
    step(cx, handle, |window, cx| {
        window.press("escape", cx);
        assert!(window.try_find("palette").is_some());
    });
    assert!(!marked(cx));
    step(cx, handle, |window, cx| {
        window.press("escape", cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).last_intent(), None);
    });
}

#[gpui_kit::test]
fn at_200_percent_in_the_smallest_window_the_palette_fits_and_every_row_is_reachable(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let smallest = size(px(tokens::MIN_WINDOW_WIDTH), px(tokens::MIN_WINDOW_HEIGHT));
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        });
    });
    cx.simulate_window_resize(handle.into(), smallest);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).fit(window), Fit::Reflowed);
        window.press(palette_key(), cx);
        window.render_frame(cx);
        let viewport = Bounds::new(Point::default(), window.viewport_size());
        let dialog = window.find("palette").bounds();
        assert!(
            dialog.left() >= viewport.left()
                && dialog.right() <= viewport.right()
                && dialog.top() >= viewport.top()
                && dialog.bottom() <= viewport.bottom(),
            "{dialog:?} in {viewport:?}"
        );
        assert!(window.find_by_label("Search commands").bounds().bottom() <= viewport.bottom());
        let count = palette_titles(window).len();
        let list = window.find("palette-list").bounds();
        for index in 0..count {
            if index > 0 {
                window.press("down", cx);
            }
            // The scroll lands on the next frame.
            window.render_frame(cx);
            window.render_frame(cx);
            let row = window.find(row_id(index)).bounds();
            let starts_inside = row.top() >= list.top() - px(1.);
            let fits = row.bottom() <= list.bottom() + px(1.);
            // A row taller than the list (an unavailable command's reason
            // wraps here) starts at the list's top; the rest of it scrolls,
            // and the status line holds its whole text.
            let taller_and_at_top =
                row.size.height > list.size.height && row.top() <= list.top() + px(1.);
            assert!(
                starts_inside && (fits || taller_and_at_top),
                "row {index} {row:?} not shown in {list:?}"
            );
            assert!(palette_status(window).starts_with(&format!("{} of {count} ", index + 1)));
        }
    });
}

// ---- Decoded daemon replies -------------------------------------------------

#[gpui_kit::test]
fn a_decoded_session_reply_becomes_the_open_sessions_timeline_and_no_other(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let message = |session: &str, seq: i64, role: &str, content: &str| {
        serde_json::json!({
            "id": format!("m{seq}"), "session_id": session, "seq": seq, "role": role,
            "content": content, "created_at": "2026-11-14T00:00:00Z",
        })
    };
    let reply = |session: &str| {
        serde_json::json!({
            "session": {"id": session},
            "running": false,
            "pending_events": [],
            "messages": [
                message(session, 10, "user", "สวัสดี **bold**"),
                message(session, 11, "assistant", ""),
                message(session, 12, "assistant", "- [x] done\n- [ ] 日本語"),
                message(session, 13, "system-note", "Model changed"),
            ],
        })
    };
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let open = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let other = shell.read(cx).sidebar().rows()[1].id.clone();
        assert_ne!(open, other);

        // Another Session's reply changes nothing.
        let theirs = replies::session_snapshot(&other, &reply(&other)).unwrap();
        assert_eq!(
            shell.update(cx, |shell, cx| shell.show_session_snapshot(theirs, cx)),
            Err("the snapshot is not for the open Session")
        );
        window.render_frame(cx);
        assert_eq!(window.find("block-count").value(), Some("40 blocks"));

        // A stream in progress is replaced by the whole transcript.
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            shell.push_stream_delta("partial", cx);
        });
        let ours = replies::session_snapshot(&open, &reply(&open)).unwrap();
        assert_eq!(ours.hidden, 1);
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(ours, cx))
            .unwrap();
        window.render_frame(cx);
        assert_eq!(window.find("block-count").value(), Some("3 blocks"));
        let block = |id: usize| window.find(("block", id));
        assert_eq!(block(10).label(), Some("You"));
        assert_eq!(block(12).label(), Some("Formless"));
        assert_eq!(block(13).label(), Some("Note"));
        assert!(window.try_find(("block", 11usize)).is_none());
        // Read as plain text, as any other block.
        assert_eq!(
            window.find(("block-text", 10usize)).value(),
            Some("สวัสดี bold")
        );
        assert_eq!(
            window.find(("block-text", 12usize)).value(),
            Some("☑ done\n☐ 日本語")
        );
    });
}

// ---- Full-output view -------------------------------------------------------

/// Rows of the full-output view that are laid out now.
fn viewer_rows_shown(window: &Window) -> Vec<usize> {
    let shown: Vec<usize> = (0..100_000usize)
        .filter(|index| window.try_find(("viewer-row", *index)).is_some())
        .take(400)
        .collect();
    shown
}

#[gpui_kit::test]
fn the_full_output_view_shows_a_long_block_whole_and_hands_focus_back(cx: &mut TestAppContext) {
    let mut blocks = fixture::timeline(8);
    // 3,000 short lines, then one line of 200 Thai characters.
    let mut text: String = (0..3000).map(|line| format!("line {line}\n")).collect();
    text.push_str(&"ก".repeat(200));
    blocks.push(TimelineBlock::new(9, BlockKind::ToolSummary, text.clone()));
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(1200.)), blocks);
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window
            .find("block-9-top-window")
            .label()
            .unwrap()
            .starts_with("Showing "));
        tab_to(window, "block-9-top-view", cx);
        window.press("enter", cx);
        window.render_frame(cx);

        let dialog = window.find("viewer");
        assert_eq!(dialog.role(), Some(Role::Dialog));
        assert_eq!(dialog.focused(), Some(true));
        assert!(shell.read(cx).background_inert(window));
        let rows = shell.read(cx).viewer_rows().expect("open");
        let summary = window.find("viewer-summary").value().unwrap().to_string();
        assert!(summary.starts_with("Tool block, "), "{summary}");
        assert!(
            summary.contains(&format!(", {rows} rows fitted to the view.")),
            "{summary}"
        );
        // 3,000 lines and the Thai line cut into rows of the view's width.
        assert!(rows > 3000, "{rows}");

        // Only what is on screen is laid out.
        let shown = viewer_rows_shown(window);
        assert_eq!(shown.first(), Some(&0));
        assert!(shown.len() < 200, "{}", shown.len());
        assert_eq!(window.find(("viewer-row", 0usize)).value(), Some("line 0"));

        // End shows the last row; Home the first; Page Down moves on.
        window.press("end", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let last = window
            .find(("viewer-row", rows - 1))
            .value()
            .unwrap()
            .to_string();
        assert!(!last.is_empty() && last.chars().all(|c| c == 'ก'), "{last}");
        window.press("home", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(viewer_rows_shown(window).first(), Some(&0));
        window.press("pagedown", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let first = *viewer_rows_shown(window).first().unwrap();
        assert!(first > 10, "{first}");
        window.press("up", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(viewer_rows_shown(window).first(), Some(&(first - 1)));

        // Copy all puts the whole block on the clipboard and says so.
        window.click("viewer-copy", cx);
        window.render_frame(cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(text.clone())
        );
        assert!(window
            .find("viewer-summary")
            .value()
            .unwrap()
            .starts_with("Copied to the clipboard."));

        // F6 stays inside; Escape closes onto the control that opened it.
        window.press("f6", cx);
        assert_eq!(window.find("viewer").focused(), Some(true));
        window.press("escape", cx);
        assert!(window.try_find("viewer").is_none());
        assert!(!shell.read(cx).background_inert(window));
        assert_eq!(window.find("block-9-top-view").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn a_block_still_arriving_cannot_be_viewed_whole(cx: &mut TestAppContext) {
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(1200.)), fixture::timeline(4));
    step(cx, handle, |_, cx| {
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            shell.push_stream_delta(&text_of(300 * 1024), cx);
        });
    });
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let id = 5u64;
        let button = window.find(gpui_kit::SharedString::from(format!("block-{id}-top-view")));
        assert_eq!(button.disabled(), Some(true));
        assert_eq!(
            shell.update(cx, |shell, cx| shell.view_block(id, window, cx)),
            Err("Available once the block has finished arriving")
        );
        assert!(window.try_find("viewer").is_none());
        // Once it is complete, it can.
        shell.update(cx, |shell, cx| shell.finish_stream(cx));
        assert_eq!(
            shell.update(cx, |shell, cx| shell.view_block(id, window, cx)),
            Ok(())
        );
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_some());
    });
}

#[gpui_kit::test]
fn a_five_megabyte_line_is_viewed_as_short_rows_and_resizing_cuts_it_again(
    cx: &mut TestAppContext,
) {
    let mut blocks = fixture::timeline(2);
    let line = "x".repeat(5 * 1024 * 1024);
    blocks.push(TimelineBlock::new(3, BlockKind::ToolSummary, line.clone()));
    let (handle, shell) = open_blocks(cx, size(px(1440.), px(900.)), blocks);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let started = Instant::now();
        shell
            .update(cx, |shell, cx| shell.view_block(3, window, cx))
            .unwrap();
        window.render_frame(cx);
        let opened = started.elapsed();
        let wide = shell.read(cx).viewer_rows().unwrap();
        let row = window.find(("viewer-row", 0usize)).value().unwrap().len();
        assert_eq!(wide, line.len().div_ceil(row));
        assert!(viewer_rows_shown(window).len() < 200);
        // Bounded work, not a layout of five megabytes.
        assert!(opened < Duration::from_secs(5), "{opened:?}");
    });
    cx.simulate_window_resize(handle.into(), size(px(800.), px(900.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let narrow = shell.read(cx).viewer_rows().unwrap();
        let row = window.find(("viewer-row", 0usize)).value().unwrap().len();
        assert_eq!(narrow, line.len().div_ceil(row));
        assert!(narrow > line.len() / 200, "{narrow}");
        // The dialog stays inside the window.
        let viewport = Bounds::new(Point::default(), window.viewport_size());
        let dialog = window.find("viewer").bounds();
        assert!(dialog.right() <= viewport.right() && dialog.bottom() <= viewport.bottom());
        assert!(window.find("viewer-close").bounds().right() <= viewport.right());
    });
}

#[gpui_kit::test]
fn replacing_a_viewed_source_closes_stale_offsets_and_copy_uses_the_new_revision(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            3,
            BlockKind::ToolSummary,
            "a".repeat(1200),
        )],
    );
    let snapshot = |session: &str, text: &str| {
        replies::session_snapshot(
            session,
            &serde_json::json!({
                "session": {"id": session}, "running": false,
                "messages": [{"id": "m3", "session_id": session, "seq": 3,
                    "role": "tool", "content": text, "created_at": "now"}],
            }),
        )
        .unwrap()
    };
    for replacement in ["ก".to_string(), "漢".repeat(400)] {
        step(cx, handle, |window, cx| {
            let open = shell.read(cx).sidebar().active_id().unwrap().to_string();
            shell
                .update(cx, |shell, cx| {
                    shell.show_session_snapshot(snapshot(&open, &"a".repeat(1200)), cx)
                })
                .unwrap();
            window.render_frame(cx);
            shell
                .update(cx, |shell, cx| shell.view_block(3, window, cx))
                .unwrap();
            window.render_frame(cx);
            assert!(shell.read(cx).viewer_rows().unwrap() > 1);
            window.press("end", cx);
            window.render_frame(cx);
            // A foreign reply must not dismiss or change the current view.
            assert!(shell
                .update(cx, |shell, cx| shell
                    .show_session_snapshot(snapshot("foreign", "not ours"), cx))
                .is_err());
            assert!(shell.read(cx).viewer_rows().is_some());
            // Both shorter text and same-byte-length, different UTF-8 text
            // invalidate the index even with identical sequence id/width.
            shell
                .update(cx, |shell, cx| {
                    shell.show_session_snapshot(snapshot(&open, &replacement), cx)
                })
                .unwrap();
            assert_eq!(shell.read(cx).viewer_rows(), None);
            window.render_frame(cx);
            assert!(window.try_find("viewer").is_none());
            assert!(!shell.read(cx).background_inert(window));
            assert_eq!(window.find_by_label("Message").focused(), Some(true));
            shell
                .update(cx, |shell, cx| shell.view_block(3, window, cx))
                .unwrap();
            window.render_frame(cx);
            window.click("viewer-copy", cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some(replacement.clone())
            );
            window.press("escape", cx);
            window.render_frame(cx);
        });
    }
}

#[gpui_kit::test]
fn an_unrenderable_grapheme_has_an_explanation_and_copy_all_not_clipped_rows(
    cx: &mut TestAppContext,
) {
    let text = "e".to_string() + &"\u{301}".repeat(formless_desktop::full_view::MEASURE_MAX_BYTES);
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::ToolSummary, text.clone())],
    );
    step(cx, handle, |window, cx| {
        shell
            .update(cx, |shell, cx| shell.view_block(1, window, cx))
            .unwrap();
        window.render_frame(cx);
        assert_eq!(shell.read(cx).viewer_rows(), Some(0));
        assert!(window
            .find("viewer-summary")
            .value()
            .unwrap()
            .contains("too large to lay out safely"));
        assert!(window.try_find(("viewer-row", 0usize)).is_none());
        for key in ["end", "home", "pagedown", "pageup", "up", "down"] {
            window.press(key, cx);
        }
        window.render_frame(cx);
        window.click("viewer-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(text.clone())
        );
    });
    cx.simulate_window_resize(handle.into(), size(px(800.), px(700.)));
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).viewer_rows(), Some(0));
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_none());
    });
}

#[gpui_kit::test]
fn a_font_size_change_at_the_same_viewport_reindexes_the_whole_output(cx: &mut TestAppContext) {
    let text = "ab漢字👩🏽‍💻".repeat(100);
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::ToolSummary, text.clone())],
    );
    step(cx, handle, |window, cx| {
        shell
            .update(cx, |shell, cx| shell.view_block(1, window, cx))
            .unwrap();
        window.render_frame(cx);
        let before = shell.read(cx).viewer_rows().unwrap();
        let viewport = window.viewport_size();
        let theme = gpui_kit::component::Theme::global_mut(cx);
        theme.mono_font_size *= 2.;
        window.render_frame(cx);
        assert_eq!(window.viewport_size(), viewport);
        assert!(shell.read(cx).viewer_rows().unwrap() > before);
        window.click("viewer-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(text.clone())
        );
    });
}

#[gpui_kit::test]
fn the_whole_output_view_and_its_error_controls_fit_the_minimum_window_at_200_percent(
    cx: &mut TestAppContext,
) {
    let oversized =
        "e".to_string() + &"\u{301}".repeat(formless_desktop::full_view::MEASURE_MAX_BYTES);
    let blocks = vec![
        TimelineBlock::new(1, BlockKind::ToolSummary, "漢字👩🏽‍💻".repeat(1000)),
        TimelineBlock::new(2, BlockKind::ToolSummary, oversized),
    ];
    let (handle, shell) = open_blocks(cx, size(px(480.), px(360.)), blocks);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::from_percent(200), window, cx)
        });
        for id in [1, 2] {
            shell
                .update(cx, |shell, cx| shell.view_block(id, window, cx))
                .unwrap();
            window.render_frame(cx);
            let viewport = Bounds::new(Point::default(), window.viewport_size());
            for copied in [false, true] {
                if copied {
                    window.click("viewer-copy", cx);
                    window.render_frame(cx);
                }
                for control in ["viewer", "viewer-summary", "viewer-copy", "viewer-close"] {
                    let bounds = window.find(control).bounds();
                    assert!(
                        bounds.left() >= viewport.left()
                            && bounds.top() >= viewport.top()
                            && bounds.right() <= viewport.right()
                            && bounds.bottom() <= viewport.bottom(),
                        "{control} at {bounds:?}, copied={copied}"
                    );
                }
            }
            tab_to(window, "viewer-summary", cx);
            assert_eq!(window.find("viewer-summary").focused(), Some(true));
            for key in ["end", "pageup", "pagedown", "home"] {
                window.press(key, cx);
                window.render_frame(cx);
            }
            tab_to(window, "viewer-close", cx);
            if id == 1 {
                assert!(window.try_find(("viewer-row", 0usize)).is_some());
            }
            window.press("escape", cx);
            window.render_frame(cx);
        }
    });
}

#[gpui_kit::test]
fn the_sessions_directory_filters_and_opens_explicitly_without_losing_drafts(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    let original = step(cx, handle, |_, cx| {
        shell.read(cx).sidebar().active_id().unwrap().to_string()
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft A", cx);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert_eq!(
            window.find("region-conversation").label(),
            Some("Fixture Sessions")
        );
        assert!(window.try_find("region-composer").is_none());
        assert_eq!(
            window.find_by_label("Filter Sessions").focused(),
            Some(true)
        );
        window.input("fixture-session-7", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(original.as_str())
        );
        window.press("down", cx);
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(original.as_str())
        );
        window.press("up", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some("fixture-session-7")
        );
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        window.input("draft B", cx);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        window.press(&chord("a"), cx);
        window.input(&original, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(original.as_str())
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft A");
        assert!(shell.read(cx).has_stored_draft("fixture-session-7"));
        assert_eq!(shell.read(cx).last_intent(), None);
    });
}

#[gpui_kit::test]
fn directory_empty_results_never_open_a_neighbor_and_escape_preserves_the_draft(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("keep me", cx);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        window.input("no such session 123456789", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let before = shell.read(cx).sidebar().active_id().map(str::to_string);
        assert!(window
            .find("directory-status")
            .value()
            .unwrap()
            .starts_with("No matching Sessions"));
        for key in ["up", "down", "enter"] {
            window.press(key, cx);
        }
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert_eq!(shell.read(cx).sidebar().active_id(), before.as_deref());
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep me");
        // A focused Back button must use its own Enter action, not the
        // directory's Enter-to-open binding (even with no chosen result).
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        tab_to(window, "back-to-chat", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(shell.read(cx).sidebar().active_id(), before.as_deref());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep me");
    });
}

#[gpui_kit::test]
fn the_directory_virtualizes_ten_thousand_sessions_and_rejects_an_over_limit_filter(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 10_000, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        let rows = shell.read(cx).sidebar().rows();
        let materialized = rows
            .iter()
            .filter(|row| window.try_find(directory_row_id(&row.id)).is_some())
            .count();
        assert!(materialized > 0 && materialized < 200, "{materialized}");
        assert!(window
            .try_find(directory_row_id("fixture-session-40"))
            .is_none()); // archived
        window.input(&"a".repeat(257), cx);
        assert_eq!(shell.read(cx).directory_query().read(cx).value().len(), 256);
        window.press(&chord("a"), cx);
        window.input(&"a".repeat(256), cx);
        assert_eq!(shell.read(cx).directory_query().read(cx).value().len(), 256);
    });
}

#[gpui_kit::test]
fn directory_enter_and_escape_do_not_activate_or_leave_during_composition(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
    });
    for key in ["enter", "escape"] {
        step(cx, handle, |window, cx| {
            let query = shell.read(cx).directory_query().clone();
            let bounds = window.find_by_label("Filter Sessions").bounds();
            let before = shell.read(cx).sidebar().active_id().map(str::to_string);
            TestInput::new(ElementInputHandler::new(bounds, query), window).compose_text(
                None,
                "にほんご",
                None,
                cx,
            );
            window.press(key, cx);
            window.render_frame(cx);
            assert_eq!(shell.read(cx).destination(), Destination::Sessions);
            assert_eq!(shell.read(cx).sidebar().active_id(), before.as_deref());
            assert_eq!(shell.read(cx).last_intent(), None);
        });
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn decoded_transcripts_are_cleared_instead_of_relabelled_when_a_directory_session_opens(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let empty = serde_json::json!({"session": {"id": id}, "running": false, "messages": []});
        let empty = replies::session_snapshot(&id, &empty).unwrap();
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(empty, cx))
            .unwrap();
        window.render_frame(cx);
        assert_eq!(window.find("block-count").value(), Some("0 blocks"));
        assert!(window
            .find("status-fixture")
            .value()
            .unwrap()
            .starts_with("Decoded snapshot"));
        let reply = serde_json::json!({"session": {"id": id}, "running": false,
            "messages": [{"id": "m1", "session_id": id, "seq": 1, "role": "user", "content": "private old transcript", "created_at": "now"}]});
        let snapshot = replies::session_snapshot(&id, &reply).unwrap();
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(snapshot, cx))
            .unwrap();
        window.render_frame(cx);
        assert_eq!(window.find("block-count").value(), Some("1 blocks"));
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        window.click(directory_row_id("fixture-session-7"), cx);
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some("fixture-session-7")
        );
        assert_eq!(window.find("block-count").value(), Some("0 blocks"));
        assert!(window
            .find("status-fixture")
            .value()
            .unwrap()
            .contains("Transcript not loaded"));
        assert!(window.try_find(("block", 1usize)).is_none());
    });
}

#[gpui_kit::test]
fn the_sessions_directory_filter_and_back_control_fit_at_200_percent_in_the_minimum_window(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 10_000, 40);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::from_percent(200), window, cx);
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx);
        });
        settle_frames(window, cx);
        assert_eq!(
            window.find_by_label("Filter Sessions").focused(),
            Some(true)
        );
        let viewport = Bounds::new(Point::default(), window.viewport_size());
        for query in [
            window.find_by_label("Filter Sessions"),
            window.find("back-to-chat"),
            window.find("directory-status"),
        ] {
            let b = query.bounds();
            assert!(
                b.top() >= viewport.top()
                    && b.bottom() <= viewport.bottom()
                    && b.left() >= viewport.left()
                    && b.right() <= viewport.right(),
                "{b:?}"
            );
        }
        window.press("down", cx);
        window.press("enter", cx);
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
    });
}

#[gpui_kit::test]
fn a_filter_change_keeps_the_preserved_directory_selection_in_view(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 10_000, 40);
    let target = step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        for _ in 0..100 {
            window.press("down", cx);
        }
        window.render_frame(cx);
        let target = shell.read(cx).sidebar().rows()[100].id.clone();
        assert_eq!(
            window.find(directory_row_id(&target)).selected(),
            Some(true)
        );
        window.input("fixture-session-", cx);
        target
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(
            window.find(directory_row_id(&target)).selected(),
            Some(true)
        );
    });
}

#[gpui_kit::test]
fn f6_into_the_directory_keeps_arrow_and_enter_navigation_working(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        let before = shell.read(cx).sidebar().active_id().map(str::to_string);
        for _ in 0..3 {
            window.press("f6", cx);
        }
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        window.press("down", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), before.as_deref());
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_ne!(shell.read(cx).sidebar().active_id(), before.as_deref());
    });
}

#[gpui_kit::test]
fn keyboard_back_from_the_directory_does_not_open_the_selected_neighbor(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let before = shell.read(cx).sidebar().active_id().map(str::to_string);
        window.click("destination-Sessions", cx);
        window.render_frame(cx);
        window.press("down", cx);
        window.render_frame(cx);
        let neighbor = shell.read(cx).sidebar().rows()[1].id.clone();
        assert_ne!(Some(neighbor.as_str()), before.as_deref());
        assert_eq!(
            window.find(directory_row_id(&neighbor)).selected(),
            Some(true)
        );
        tab_to(window, "back-to-chat", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(shell.read(cx).sidebar().active_id(), before.as_deref());
    });
}

#[gpui_kit::test]
fn commands_replace_the_whole_output_modal_and_navigation_cannot_leave_hidden_focus(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            1,
            BlockKind::ToolSummary,
            "line\n".repeat(1000),
        )],
    );
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        shell
            .update(cx, |shell, cx| shell.view_block(1, window, cx))
            .unwrap();
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_some());
        shell.update(cx, |shell, cx| shell.open_palette(window, cx));
        window.render_frame(cx);
        assert_eq!(shell.read(cx).viewer_rows(), None);
        assert!(window.try_find("viewer").is_none());
        assert!(window.try_find("palette").is_some());
        window.input("go sessions", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert!(window.try_find("viewer").is_none());
        assert!(window.try_find("palette").is_none());
        assert_eq!(
            window.find_by_label("Filter Sessions").focused(),
            Some(true)
        );
        assert!(!shell.read(cx).background_inert(window));
        window.press("escape", cx);
        window.render_frame(cx);
        shell
            .update(cx, |shell, cx| shell.view_block(1, window, cx))
            .unwrap();
        window.render_frame(cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Tools), window, cx)
        });
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_none());
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        assert!(!shell.read(cx).background_inert(window));
    });
}

#[gpui_kit::test]
fn a_snapshot_replacing_a_palette_origin_restores_visible_focus_not_an_old_row(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            1,
            BlockKind::ToolSummary,
            "a".repeat(64 * 1024),
        )],
    );
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "block-1-top-view", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        shell.update(cx, |shell, cx| shell.open_palette(window, cx));
        window.render_frame(cx);
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let snapshot = replies::session_snapshot(&id, &serde_json::json!({
            "session": {"id": id}, "running": false,
            "messages": [{"id": "m1", "session_id": id, "seq": 1, "role": "tool", "content": "short", "created_at": "now"}],
        })).unwrap();
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(snapshot, cx))
            .unwrap();
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_none());
        assert!(window.try_find("palette").is_none());
        assert!(window.try_find("block-1-top-view").is_none());
        assert_eq!(window.find_by_label("Message").focused(), Some(true));
        assert!(!shell.read(cx).background_inert(window));
    });
}

#[gpui_kit::test]
fn search_is_reachable_from_commands_and_opens_the_chosen_literal_match_without_losing_drafts(
    cx: &mut TestAppContext,
) {
    let blocks = vec![
        TimelineBlock::new(1, BlockKind::User, "alpha needle".into()),
        TimelineBlock::new(2, BlockKind::ToolSummary, "Needle caps".into()),
        TimelineBlock::new(3, BlockKind::Assistant, "prefix needle second".into()),
    ];
    let (handle, shell) = open_blocks(cx, WIDE, blocks);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("draft kept", cx);
        shell.update(cx, |shell, cx| shell.open_palette(window, cx));
        window.render_frame(cx);
        window.input("go search", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Search);
        assert_eq!(
            window.find_by_label("Search loaded transcript").focused(),
            Some(true)
        );
        assert!(window.try_find("region-composer").is_none());
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .contains("case-sensitive"));
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).search_hits().len(), 2); // not uppercase Needle
        let second = shell.read(cx).search_hits()[1];
        window.press("down", cx);
        window.render_frame(cx);
        assert_eq!(window.find(search_row_id(second)).selected(), Some(true));
        window.press("enter", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_some());
        assert!(window
            .find(("viewer-row", 0usize))
            .value()
            .unwrap()
            .contains("needle"));
        window.click("viewer-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("prefix needle second".into())
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find_by_label("Search loaded transcript").focused(),
            Some(true)
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft kept");
        assert_eq!(shell.read(cx).last_intent(), None);
    });
}

#[gpui_kit::test]
fn a_search_hit_deep_in_a_long_block_is_visible_when_opened(cx: &mut TestAppContext) {
    let text = "x".repeat(2 * 1024 * 1024) + "needle";
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(7, BlockKind::ToolSummary, text)],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).search_hits().len(), 1);
        window.press("enter", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(shell.read(cx).viewer_rows().unwrap() > 200);
        assert!(viewer_rows_shown(window).iter().any(|index| window
            .find(("viewer-row", *index))
            .value()
            .unwrap()
            .contains("needle")));
    });
}

#[gpui_kit::test]
fn search_limits_are_explicit_and_do_not_pretend_to_search_unloaded_or_truncated_text(
    cx: &mut TestAppContext,
) {
    let text = "x".repeat(formless_desktop::search::MAX_SCAN_BYTES + 1024) + "needle";
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(7, BlockKind::ToolSummary, text)],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(shell.read(cx).search_hits().is_empty());
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .starts_with("Partial:"));
        window.press("enter", cx);
        assert_eq!(shell.read(cx).viewer_rows(), None);
        window.press(&chord("a"), cx);
        window.input("xxxx", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).search_hits().len(), 200);
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .contains("first 200 results only"));
    });
}

#[gpui_kit::test]
fn a_search_row_from_an_old_snapshot_cannot_open_a_same_id_replacement(cx: &mut TestAppContext) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            1,
            BlockKind::User,
            "prefix needle old".into(),
        )],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let hit = shell.read(cx).search_hits()[0];
        let revision = shell.read(cx).search_results_revision();
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let snapshot = replies::session_snapshot(&id, &serde_json::json!({"session":{"id":id},"running":false,
            "messages":[{"id":"m1","session_id":id,"seq":1,"role":"user","content":"prefix needle NEW","created_at":"now"}]})).unwrap();
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(snapshot, cx))
            .unwrap();
        // Invoke the action captured by the old row. The kit's click helper
        // repaints first, so it cannot simulate a queued stale-frame action.
        shell.update(cx, |shell, cx| {
            shell.open_search_hit(hit, revision, window, cx)
        });
        assert_eq!(shell.read(cx).viewer_rows(), None);
        window.render_frame(cx);
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .contains("Results changed"));
        window.click(search_row_id(hit), cx);
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_some());
        window.click("viewer-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("prefix needle NEW".into())
        );
    });
}

#[gpui_kit::test]
fn search_distinguishes_unloaded_sessions_from_loaded_empty_snapshots(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 200, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let snapshot = replies::session_snapshot(
            &id,
            &serde_json::json!({"session":{"id":id},"running":false,"messages":[]}),
        )
        .unwrap();
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(snapshot, cx))
            .unwrap();
        window.render_frame(cx);
        window.click(session_row_id("fixture-session-7"), cx);
        window.render_frame(cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .contains("No transcript loaded"));
        assert!(shell.read(cx).search_hits().is_empty());
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let snapshot = replies::session_snapshot(
            &id,
            &serde_json::json!({"session":{"id":id},"running":false,"messages":[]}),
        )
        .unwrap();
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(snapshot, cx))
            .unwrap();
        window.render_frame(cx);
        let status = window.find("search-status").value().unwrap().to_string();
        assert!(status.contains("No case-sensitive matches"));
        assert!(!status.contains("No transcript loaded"));
        window.press("enter", cx);
        assert_eq!(shell.read(cx).viewer_rows(), None);
    });
}

#[gpui_kit::test]
fn search_excludes_live_text_then_refreshes_once_the_stream_finishes(cx: &mut TestAppContext) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::User, "needle".into())],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            shell.push_stream_delta("needle", cx);
            shell.run_command(Command::GoTo(Destination::Search), window, cx);
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).search_hits().len(), 1);
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .contains("Live tail excluded"));
        shell.update(cx, |shell, cx| shell.push_stream_delta("needle", cx));
        window.render_frame(cx);
        assert_eq!(shell.read(cx).search_hits().len(), 1);
        shell.update(cx, |shell, cx| shell.finish_stream(cx));
        window.render_frame(cx);
        assert_eq!(shell.read(cx).search_hits().len(), 3);
        assert!(!window
            .find("search-status")
            .value()
            .unwrap()
            .contains("Live tail excluded"));
    });
}

#[gpui_kit::test]
fn search_composition_does_not_open_a_match_or_navigate_back(cx: &mut TestAppContext) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::User, "にほんご".into())],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
    });
    for key in ["enter", "escape"] {
        step(cx, handle, |window, cx| {
            let query = shell.read(cx).search_query().clone();
            let bounds = window.find_by_label("Search loaded transcript").bounds();
            TestInput::new(ElementInputHandler::new(bounds, query), window).compose_text(
                None,
                "にほんご",
                None,
                cx,
            );
            window.press(key, cx);
            window.render_frame(cx);
            assert_eq!(shell.read(cx).destination(), Destination::Search);
            assert_eq!(shell.read(cx).viewer_rows(), None);
            assert_eq!(shell.read(cx).last_intent(), None);
        });
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn repeated_search_entry_and_keyboard_back_restore_the_prior_destination_without_opening_a_match(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            1,
            BlockKind::User,
            "needle needle".into(),
        )],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
        });
        window.render_frame(cx);
        window.input("invoice", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx);
            shell.run_command(Command::GoTo(Destination::Search), window, cx);
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.render_frame(cx);
        tab_to(window, "search-back", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert_eq!(
            shell.read(cx).directory_query().read(cx).value().as_ref(),
            "invoice"
        );
        assert_eq!(
            window.find_by_label("Filter Sessions").focused(),
            Some(true)
        );
        assert_eq!(shell.read(cx).viewer_rows(), None);
        window.press("escape", cx);
        window.render_frame(cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
    });
}

#[gpui_kit::test]
fn search_controls_and_a_match_fit_the_minimum_window_at_200_percent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 200, 40);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::from_percent(200), window, cx);
            shell.run_command(Command::GoTo(Destination::Search), window, cx);
        });
        settle_frames(window, cx);
        window.input("ข้อความ", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        let viewport = Bounds::new(Point::default(), window.viewport_size());
        let hit = shell.read(cx).search_hits()[0];
        for bounds in [
            window.find_by_label("Search loaded transcript").bounds(),
            window.find("search-back").bounds(),
            window.find("search-status").bounds(),
            window.find(search_row_id(hit)).bounds(),
        ] {
            assert!(
                bounds.top() >= viewport.top()
                    && bounds.bottom() <= viewport.bottom()
                    && bounds.left() >= viewport.left()
                    && bounds.right() <= viewport.right(),
                "{bounds:?}"
            );
        }
        window.press("f6", cx);
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_some());
    });
}

#[gpui_kit::test]
fn an_old_search_row_cannot_activate_after_the_query_changes_before_repaint(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::User, "alpha beta".into())],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("alpha", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let old = shell.read(cx).search_hits()[0];
        let revision = shell.read(cx).search_results_revision();
        window.press(&chord("a"), cx);
        window.input("beta", cx);
        shell.update(cx, |shell, cx| {
            shell.open_search_hit(old, revision, window, cx)
        });
        assert_eq!(shell.read(cx).viewer_rows(), None);
        window.render_frame(cx);
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .contains("changed"));
    });
}

#[gpui_kit::test]
fn a_queued_search_activation_cannot_stack_a_viewer_over_commands(cx: &mut TestAppContext) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::User, "needle".into())],
    );
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let hit = shell.read(cx).search_hits()[0];
        let revision = shell.read(cx).search_results_revision();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ShowCommands, window, cx);
            shell.open_search_hit(hit, revision, window, cx);
        });
        window.render_frame(cx);
        assert!(window.try_find("palette").is_some());
        assert_eq!(shell.read(cx).viewer_rows(), None);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_none());
        assert_eq!(shell.read(cx).viewer_rows(), None);
        window.click(search_row_id(hit), cx);
        window.render_frame(cx);
        assert!(window.try_find("viewer").is_some());
    });
}

// ---- Trusted typed-event view boundary (no transport) --------------------

fn event_snapshot(id: &str, text: &str) -> replies::SessionSnapshot {
    replies::session_snapshot(
        id,
        &serde_json::json!({
            "session":{"id":id}, "running":false, "messages":[{
                "id":"m1", "session_id":id, "seq":1, "role":"user",
                "content":text, "created_at":"now"
            }]
        }),
    )
    .unwrap()
}

fn load_event_snapshot(shell: &Entity<Shell>, text: &str, cx: &mut App) -> String {
    let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
    shell
        .update(cx, |shell, cx| {
            shell.show_session_snapshot(event_snapshot(&id, text), cx)
        })
        .unwrap();
    id
}

fn typed_delta(id: &str, text: &str) -> formless_protocol::Event {
    formless_protocol::Event::MessageDelta {
        session_id: id.into(),
        run_id: "r".into(),
        delta: text.into(),
    }
}

fn typed_message(id: &str, seq: i64, role: &str, text: &str) -> formless_protocol::MessageInfo {
    formless_protocol::MessageInfo {
        id: format!("m{seq}"),
        session_id: id.into(),
        seq,
        role: role.into(),
        content: text.into(),
        created_at: "now".into(),
        meta: None,
    }
}

fn typed_complete(id: &str, seq: i64, text: &str) -> formless_protocol::Event {
    formless_protocol::Event::MessageComplete {
        session_id: id.into(),
        run_id: "r".into(),
        message: typed_message(id, seq, "assistant", text),
    }
}

fn typed_gap(id: &str) -> formless_protocol::Event {
    formless_protocol::Event::DeltasDropped {
        session_id: id.into(),
        run_id: "r".into(),
        kinds: vec![formless_protocol::DroppedEventKind::MessageDelta],
        handle: None,
        model: None,
    }
}

#[gpui_kit::test]
fn typed_transcript_updates_replace_the_live_tail_without_changing_run_or_draft(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 20, 40);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("keep this draft", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let id = load_event_snapshot(&shell, "question", cx);
        window.render_frame(cx);
        let run = window.find("run-status").value().unwrap().to_string();
        shell.update(cx, |shell, cx| {
            for _ in 0..100 {
                shell
                    .apply_transcript_event(&typed_delta(&id, "partial "), cx)
                    .unwrap();
            }
            shell
                .apply_transcript_event(
                    &formless_protocol::Event::MessageAppended {
                        session_id: id.clone(),
                        message: typed_message(&id, 2, "tool", "tool result"),
                    },
                    cx,
                )
                .unwrap();
            assert_eq!(shell.block_count(), 3);
            shell
                .apply_transcript_event(&typed_complete(&id, 3, "canonical answer"), cx)
                .unwrap();
        });
        window.render_frame(cx);
        assert_eq!(shell.read(cx).block_count(), 3);
        assert_eq!(shell.read(cx).last_block_preview(), "canonical answer");
        assert!(window.try_find(("block", usize::MAX)).is_none());
        assert_eq!(
            window.find(("block-text", 2usize)).value(),
            Some("tool result")
        );
        assert_eq!(
            window.find(("block-text", 3usize)).value(),
            Some("canonical answer")
        );
        assert_eq!(window.find("run-status").value(), Some(run.as_str()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep this draft");
        assert_eq!(shell.read(cx).last_intent(), None);
        shell
            .update(cx, |shell, cx| shell.view_block(3, window, cx))
            .unwrap();
        window.render_frame(cx);
        window.click("viewer-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("canonical answer".into())
        );
    });
}

#[gpui_kit::test]
fn typed_transcript_events_need_a_decoded_source_and_foreign_events_are_ignored(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 20, 40);
    step(cx, handle, |window, cx| {
        let fixture_count = shell.read(cx).block_count();
        assert!(shell
            .update(cx, |shell, cx| shell
                .apply_transcript_event(&typed_delta("foreign", "text"), cx))
            .is_err());
        assert_eq!(shell.read(cx).block_count(), fixture_count);
        let id = load_event_snapshot(&shell, "question", cx);
        let before = shell.read(cx).stream_stats();
        let change = shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(&typed_delta("foreign", "not ours"), cx)
            })
            .unwrap();
        assert_eq!(change, formless_desktop::transcript::Change::default());
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(
                    &formless_protocol::Event::RunStarted {
                        session_id: id.clone(),
                        run_id: "r".into(),
                    },
                    cx,
                )
            })
            .unwrap();
        assert_eq!(shell.read(cx).block_count(), 1);
        assert_eq!(shell.read(cx).stream_stats(), before);
        assert_eq!(shell.read(cx).transcript_reload_needed(), None);
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(
                    &formless_protocol::Event::MessageAppended {
                        session_id: id,
                        message: typed_message("foreign", 2, "user", "wrong nested Session"),
                    },
                    cx,
                )
            })
            .unwrap();
        window.render_frame(cx);
        assert_eq!(shell.read(cx).block_count(), 1);
        assert!(window
            .find("transcript-reload")
            .value()
            .unwrap()
            .contains("could not be reconciled"));
    });
}

#[gpui_kit::test]
fn typed_transcript_gap_notices_stay_visible_until_a_trusted_snapshot_replaces_them(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 20, 40);
    step(cx, handle, |window, cx| {
        let id = load_event_snapshot(&shell, "needle question", cx);
        shell
            .update(cx, |shell, cx| shell.view_block(1, window, cx))
            .unwrap();
        shell.update(cx, |shell, cx| {
            shell
                .apply_transcript_event(&typed_delta(&id, "missed partial"), cx)
                .unwrap();
            shell.apply_transcript_event(&typed_gap(&id), cx).unwrap();
            let frozen_count = shell.block_count();
            assert!(
                !shell
                    .apply_transcript_event(&typed_delta(&id, "unsafe suffix"), cx)
                    .unwrap()
                    .timeline_changed
            );
            assert_eq!(shell.block_count(), frozen_count);
            shell
                .apply_transcript_event(
                    &formless_protocol::Event::MessagesEdited {
                        session_id: id.clone(),
                    },
                    cx,
                )
                .unwrap();
            shell
                .apply_transcript_event(&typed_complete(&id, 2, "needle canonical"), cx)
                .unwrap();
        });
        window.render_frame(cx);
        assert_eq!(shell.read(cx).viewer_rows(), None);
        assert_eq!(
            shell.read(cx).transcript_reload_needed(),
            Some(&formless_desktop::transcript::ReloadReason::DeltasDropped)
        );
        assert!(window
            .find("transcript-reload")
            .value()
            .unwrap()
            .contains("live text was missed"));
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .starts_with("Reload needed:"));
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window
            .find("viewer-summary")
            .value()
            .unwrap()
            .starts_with("Reload needed:"));
        assert!(shell
            .update(cx, |shell, cx| shell
                .show_session_snapshot(event_snapshot("foreign", "wrong"), cx))
            .is_err());
        assert!(shell.read(cx).transcript_reload_needed().is_some());
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        shell
            .update(cx, |shell, cx| {
                shell.show_session_snapshot(event_snapshot(&id, "fresh needle"), cx)
            })
            .unwrap();
        window.render_frame(cx);
        assert_eq!(shell.read(cx).transcript_reload_needed(), None);
        assert_eq!(shell.read(cx).viewer_rows(), None);
        assert!(!window
            .find("search-status")
            .value()
            .unwrap()
            .contains("Reload needed:"));
    });
}

#[gpui_kit::test]
fn typed_transcript_search_refreshes_at_source_boundaries_not_per_delta(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 20, 40);
    step(cx, handle, |window, cx| {
        load_event_snapshot(&shell, "needle question", cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Search), window, cx)
        });
        window.render_frame(cx);
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        assert_eq!(shell.read(cx).search_hits().len(), 1);
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(&typed_delta(&id, "needle"), cx)
            })
            .unwrap();
        let revision = shell.read(cx).search_results_revision();
        let hit = shell.read(cx).search_hits()[0];
        shell.update(cx, |shell, cx| {
            for _ in 0..100 {
                shell
                    .apply_transcript_event(&typed_delta(&id, " needle"), cx)
                    .unwrap();
            }
        });
        assert_eq!(shell.read(cx).search_results_revision(), revision);
        assert_eq!(shell.read(cx).search_hits().len(), 1);
        window.render_frame(cx);
        assert!(window
            .find("search-status")
            .value()
            .unwrap()
            .contains("Live tail excluded"));
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(&typed_complete(&id, 2, "needle final"), cx)
            })
            .unwrap();
        assert_eq!(shell.read(cx).search_hits().len(), 2);
        shell.update(cx, |shell, cx| {
            shell.open_search_hit(hit, revision, window, cx)
        });
        assert_eq!(shell.read(cx).viewer_rows(), None);
        window.render_frame(cx);
        assert!(!window
            .find("search-status")
            .value()
            .unwrap()
            .contains("Live tail excluded"));
    });
}

#[gpui_kit::test]
fn typed_transcript_reducer_is_dropped_when_the_open_session_changes(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 20, 40);
    step(cx, handle, |window, cx| {
        let id = load_event_snapshot(&shell, "old Session", cx);
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(&typed_gap(&id), cx)
            })
            .unwrap();
        window.render_frame(cx);
        let next = shell.read(cx).sidebar().rows()[1].id.clone();
        assert_ne!(id, next);
        window.click(session_row_id(&next), cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(next.as_str()));
        assert_eq!(shell.read(cx).block_count(), 0);
        assert_eq!(shell.read(cx).transcript_reload_needed(), None);
        assert!(window.try_find("transcript-reload").is_none());
        assert!(shell
            .update(cx, |shell, cx| shell
                .apply_transcript_event(&typed_delta(&id, "late old text"), cx))
            .is_err());
        shell.update(cx, |shell, cx| {
            shell.begin_stream(cx);
            shell.push_stream_delta("late synthetic producer", cx);
            shell.finish_stream(cx);
        });
        assert_eq!(shell.read(cx).block_count(), 0);
    });
}

#[gpui_kit::test]
fn typed_transcript_reload_notice_fits_reflow_and_fixture_streams_cannot_bypass_the_reducer(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 20, 40);
    step(cx, handle, |window, cx| {
        let id = load_event_snapshot(&shell, "unchanged source", cx);
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::from_percent(200), window, cx);
            shell.begin_stream(cx);
            shell.push_stream_delta("untyped corruption", cx);
            shell.finish_stream(cx);
            shell.apply_transcript_event(&typed_gap(&id), cx).unwrap();
        });
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).block_count(), 1);
        assert_eq!(shell.read(cx).last_block_preview(), "unchanged source");
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        settle_frames(window, cx);
        let notice = window.find("transcript-reload");
        let region = window.find("region-conversation").bounds();
        assert!(
            notice.bounds().left() >= region.left() && notice.bounds().right() <= region.right()
        );
        let viewport = Bounds::new(Point::default(), window.viewport_size());
        assert!(
            notice.bounds().top() >= viewport.top()
                && notice.bounds().bottom() <= viewport.bottom(),
            "{:?}",
            notice.bounds()
        );
        assert!(notice.bounds().size.height > px(0.));
        assert_eq!(
            notice.value(),
            Some(formless_desktop::transcript::ReloadReason::DeltasDropped.notice())
        );
        assert_eq!(shell.read(cx).last_intent(), None);
    });
}

#[gpui_kit::test]
fn typed_transcript_structural_updates_preserve_a_scrolled_up_reading_anchor(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 20, 40);
    step(cx, handle, |window, cx| {
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let messages: Vec<_> = (1..=100)
            .map(|seq| {
                typed_message(
                    &id,
                    seq,
                    "user",
                    &format!("message {seq}\nline two\nline three\nline four"),
                )
            })
            .collect();
        let snapshot = replies::session_snapshot(
            &id,
            &serde_json::json!({"session":{"id":id},"running":false,"messages":messages}),
        )
        .unwrap();
        shell
            .update(cx, |shell, cx| shell.show_session_snapshot(snapshot, cx))
            .unwrap();
        settle_frames(window, cx);
        // Wheel movement is clamped per event. Use two gestures so setup
        // is well away from the tail even if the composer changes height.
        for _ in 0..2 {
            window.scroll(
                "timeline-area",
                ScrollDelta::Pixels(point(px(0.), px(1600.))),
                cx,
            );
            settle_frames(window, cx);
        }
        let area = window.find("timeline-area").bounds();
        let reading = (1..=100usize)
            .find(|seq| {
                window.try_find(("block", *seq)).is_some_and(|row| {
                    row.bounds().top() >= area.top() && row.bounds().bottom() <= area.bottom()
                })
            })
            .expect("a visible older row");
        assert!(
            reading < 90,
            "the test must scroll away from the tail: {reading}"
        );
        let top = window.find(("block", reading)).bounds().top();
        shell.update(cx, |shell, cx| {
            shell
                .apply_transcript_event(&typed_delta(&id, "partial"), cx)
                .unwrap();
            // The delta has not reached the virtual list yet.
            shell
                .apply_transcript_event(
                    &formless_protocol::Event::MessageAppended {
                        session_id: id.clone(),
                        message: typed_message(&id, 101, "tool", "interleaved tool"),
                    },
                    cx,
                )
                .unwrap();
            shell
                .apply_transcript_event(&typed_complete(&id, 102, "canonical"), cx)
                .unwrap();
        });
        settle_frames(window, cx);
        assert_eq!(window.find(("block", reading)).bounds().top(), top);
        assert_eq!(shell.read(cx).block_count(), 102);
    });
}

// ---- Exact source copying --------------------------------------------------

#[gpui_kit::test]
fn block_copy_source_keeps_markdown_unicode_whitespace_and_the_complete_previewed_block(
    cx: &mut TestAppContext,
) {
    let text = format!(
        "# ไทย 日本語 👩‍💻\r\n```rust\r\n\tlet x = 1;  \r\n```\r\n{}",
        "tail  \n".repeat(6000)
    );
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::Assistant, text.clone())],
    );
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("block-1-top-window").is_some());
        let control = window.find("block-1-copy-source");
        assert_eq!(control.label(), Some("Copy complete block source"));
        tab_to(window, "block-1-copy-source", cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(text.clone())
        );
        assert_eq!(window.find("block-1-copy-source").focused(), Some(true));
        assert!(window
            .find("block-copy-notice")
            .value()
            .unwrap()
            .contains("complete block source"));
        assert!(shell.read(cx).viewer_rows().is_none());
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[gpui_kit::test]
fn block_copy_source_rejects_stale_replacements_modals_reload_and_live_tail(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::User, "old".into())],
    );
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()));
        let stale = shell.read(cx).block_copy_revision();
        let id = load_event_snapshot(&shell, "new", cx);
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_block_source(1, stale, window, cx)));
        let current = shell.read(cx).block_copy_revision();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Tools), window, cx)
        });
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_block_source(1, current, window, cx)));
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Chat), window, cx)
        });
        window.press(palette_key(), cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_some());
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_block_source(1, current, window, cx)));
        window.press("escape", cx);
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(&typed_gap(&id), cx)
            })
            .unwrap();
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_block_source(1, current, window, cx)));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("sentinel".into())
        );
        load_event_snapshot(&shell, "restored", cx);
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(&typed_delta(&id, "live"), cx)
            })
            .unwrap();
        let current = shell.read(cx).block_copy_revision();
        // The reducer reserves this ID outside the persisted sequence range.
        let tail = u64::MAX;
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_block_source(tail, current, window, cx)));
        assert!(shell.update(cx, |shell, cx| shell
            .copy_block_source(1, current, window, cx)));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("restored".into())
        );
    });
}

#[gpui_kit::test]
fn block_copy_source_enforces_a_whole_one_mib_limit(cx: &mut TestAppContext) {
    let accepted = "ก".repeat(349525) + "x"; // Exactly 1 MiB, valid UTF-8.
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            1,
            BlockKind::ToolSummary,
            accepted.clone(),
        )],
    );
    step(cx, handle, |window, cx| {
        let revision = shell.read(cx).block_copy_revision();
        assert!(shell.update(cx, |shell, cx| shell
            .copy_block_source(1, revision, window, cx)));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(accepted.clone())
        );
        load_event_snapshot(&shell, &(accepted.clone() + "x"), cx);
        let revision = shell.read(cx).block_copy_revision();
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_block_source(1, revision, window, cx)));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(accepted.clone())
        );
        window.render_frame(cx);
        assert_eq!(window.find("block-1-copy-source").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn block_copy_source_is_keyboard_reachable_at_minimum_size_and_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            1,
            BlockKind::User,
            "# ไทย 日本語\n\tcode  ".into(),
        )],
    );
    set_text_scale(cx, handle, &shell, 200);
    cx.simulate_window_resize(handle.into(), size(px(480.), px(360.)));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        tab_to(window, "block-1-copy-source", cx);
        window.render_frame(cx);
        let viewport = Bounds::new(Point::default(), window.viewport_size());
        let button = window.find("block-1-copy-source");
        assert_eq!(button.focused(), Some(true));
        assert!(button.bounds().left() >= viewport.left());
        assert!(button.bounds().right() <= viewport.right());
        assert!(button.bounds().top() >= viewport.top());
        assert!(button.bounds().bottom() <= viewport.bottom());
        window.press("enter", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("# ไทย 日本語\n\tcode  ".into())
        );
    });
}

#[gpui_kit::test]
fn resource_copy_details_rejects_queued_selection_refresh_navigation_and_modal_changes(
    cx: &mut TestAppContext,
) {
    use formless_desktop::client::Update;
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let initial =
            serde_json::json!([{"id":"a","text":"ไทย\r\n\t  "},{"id":"b","text":"private B"}]);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Tools,
                    result: Ok(initial.clone()),
                },
                window,
                cx,
            );
            shell.run_command(Command::GoTo(Destination::Tools), window, cx);
            shell.focus_section(PageSection::Conversation, window, cx);
        });
        window.render_frame(cx);
        let first = shell.read(cx).resource_copy_target().unwrap();
        cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()));
        window.press("down", cx);
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(first, window, cx)));
        let second = shell.read(cx).resource_copy_target().unwrap();
        assert!(shell.update(cx, |shell, cx| shell
            .copy_resource_details(second, window, cx)));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &cx.read_from_clipboard().unwrap().text().unwrap()
            )
            .unwrap(),
            initial[1]
        );
        cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Tools,
                    result: Ok(initial.clone()),
                },
                window,
                cx,
            )
        });
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(first, window, cx)));
        let fresh = shell.read(cx).resource_copy_target().unwrap();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Settings,
                    result: Ok(serde_json::json!({"config":{"text":"private settings"}})),
                },
                window,
                cx,
            );
            shell.run_command(Command::GoTo(Destination::Settings), window, cx);
        });
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(fresh, window, cx)));
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Tools), window, cx)
        });
        window.press(palette_key(), cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_some());
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(fresh, window, cx)));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("sentinel".into())
        );
        window.press("escape", cx);
        window.render_frame(cx);
        // The current actual control still copies the complete original JSON.
        window.click("copy-resource", cx);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &cx.read_from_clipboard().unwrap().text().unwrap()
            )
            .unwrap(),
            initial[0]
        );
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Tools,
                    result: Err("unavailable".into()),
                },
                window,
                cx,
            )
        });
        assert!(shell.read(cx).resource_copy_target().is_none());
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(fresh, window, cx)));
        // Changing Profile clears old resource identities, including a same destination.
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        assert!(shell.read(cx).resource_copy_target().is_none());
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(fresh, window, cx)));
    });
}

#[gpui_kit::test]
fn native_find_shortcut_opens_loaded_search_and_refocuses_without_losing_drafts_or_selection(
    cx: &mut TestAppContext,
) {
    let find = commands::keys(Command::GoTo(Destination::Search))[0];
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(
            1,
            BlockKind::User,
            "needle needle".into(),
        )],
    );
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("unsent f", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        window.press(find, cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Search);
        assert_eq!(
            window.find_by_label("Search loaded transcript").focused(),
            Some(true)
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent f");
        assert!(
            command_entry(&shell, Command::GoTo(Destination::Search), window, cx)
                .shortcut()
                .is_some()
        );
        window.input("needle", cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).search_hits().len(), 2);
        window.press("down", cx);
        window.render_frame(cx);
        let chosen = shell.read(cx).search_hits()[1];
        assert_eq!(window.find(search_row_id(chosen)).selected(), Some(true));
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        window.press(find, cx);
        window.render_frame(cx);
        assert_eq!(
            window.find_by_label("Search loaded transcript").focused(),
            Some(true)
        );
        assert_eq!(
            shell.read(cx).search_query().read(cx).value().as_ref(),
            "needle"
        );
        assert_eq!(window.find(search_row_id(chosen)).selected(), Some(true));
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unsent f");
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[gpui_kit::test]
fn native_find_shortcut_keeps_modals_and_composition_owned_by_the_current_control(
    cx: &mut TestAppContext,
) {
    let find = commands::keys(Command::GoTo(Destination::Search))[0];
    let (handle, shell) = open_blocks(
        cx,
        WIDE,
        vec![TimelineBlock::new(1, BlockKind::User, "にほんご".into())],
    );
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let composer = shell.read(cx).composer().clone();
        let bounds = window.find_by_label("Message").bounds();
        TestInput::new(ElementInputHandler::new(bounds, composer.clone()), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        window.press(find, cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        composer.update(cx, |input, cx| input.unmark_text(window, cx));
        window.press(find, cx);
        window.render_frame(cx);
        let query = shell.read(cx).search_query().clone();
        let bounds = window.find_by_label("Search loaded transcript").bounds();
        TestInput::new(ElementInputHandler::new(bounds, query.clone()), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        window.press(find, cx);
        assert!(query.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        query.update(cx, |input, cx| input.unmark_text(window, cx));
        window.press(palette_key(), cx);
        window.render_frame(cx);
        window.press(find, cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_some());
        window.press("escape", cx);
        shell
            .update(cx, |shell, cx| shell.view_block(1, window, cx))
            .unwrap();
        window.render_frame(cx);
        window.press(find, cx);
        assert!(shell.read(cx).viewer_rows().is_some());
        window.press("escape", cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
        });
        window.render_frame(cx);
        let query = shell.read(cx).directory_query().clone();
        let bounds = window.find_by_label("Filter Sessions").bounds();
        TestInput::new(ElementInputHandler::new(bounds, query.clone()), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        window.press(find, cx);
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert!(query.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[gpui_kit::test]
fn composer_escape_cancels_replaced_preedit_without_an_undo_entry(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 4, 8);
    let draft = "👩🏽‍💻 left ไทย right";
    let selection = "👩🏽‍💻 left ".len().."👩🏽‍💻 left ไทย".len();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input(draft, cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let composer = shell.read(cx).composer().clone();
        composer.update(cx, |input, cx| {
            input.set_selected_range_with_direction(selection.clone(), true, cx);
        });
        let bounds = window.find_by_label("Message").bounds();
        let mut input = TestInput::new(ElementInputHandler::new(bounds, composer.clone()), window);
        input.compose_text(None, "´", None, cx);
        input.compose_text(None, "にほん", None, cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "👩🏽‍💻 left にほん right");
        window.press("escape", cx);
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_none()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        assert_eq!(composer.read(cx).selected_range(), selection);
        assert!(composer.read(cx).is_selection_reversed());
        assert!(gpui_kit::Focusable::focus_handle(&composer, cx).is_focused(window));
        window.press("escape", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
        assert!(shell.read(cx).last_intent().is_none());
        window.press(&chord("z"), cx);
        assert!(shell.read(cx).draft(cx).is_empty());
        window.press(REDO_SHORTCUT, cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), draft);
    });
}

#[gpui_kit::test]
fn composer_send_key_choice_keeps_exact_draft_and_survives_rebuilds(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 20, 4);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("first ไทย", cx);
        let composer = shell.read(cx).composer().clone();
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(
                Command::ToggleEnterSends,
                window,
                cx
            )),
            None
        );
        assert_eq!(shell.read(cx).composer().entity_id(), composer.entity_id());
        window.render_frame(cx);
        assert!(window
            .find("composer-send-keys")
            .value()
            .unwrap()
            .contains("Enter / Shift+Enter: newline"));
        window.press("enter", cx);
        window.press("shift-enter", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "first ไทย\n\n");
        assert!(shell.read(cx).last_intent().is_none());
        window.press(commands::composer_submit_key(), cx);
        assert_eq!(
            shell.read(cx).last_intent().unwrap().kind,
            IntentKind::Send(SendMode::Send)
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "first ไทย\n\n");
        let first = shell.read(cx).sidebar().active_id().unwrap().to_string();
        window.click(session_row_id("fixture-session-7"), cx);
        window.render_frame(cx);
        assert!(
            !shell
                .update(cx, |shell, cx| shell.command_state(window, cx))
                .enter_sends
        );
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.input("steer", cx);
        window.press(commands::composer_submit_key(), cx);
        assert_eq!(
            shell.read(cx).last_intent().unwrap().kind,
            IntentKind::Send(SendMode::Steer)
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "steer");
        window.click("composer-secondary", cx);
        assert_eq!(
            shell.read(cx).last_intent().unwrap().kind,
            IntentKind::Send(SendMode::Queue)
        );
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ToggleEnterSends, window, cx)
        });
        window.render_frame(cx);
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.press(commands::composer_submit_key(), cx);
        assert_eq!(
            shell.read(cx).last_intent().unwrap().kind,
            IntentKind::Send(SendMode::Queue)
        );
        window.click(session_row_id(&first), cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "first ไทย\n\n");
        assert!(
            shell
                .update(cx, |shell, cx| shell.command_state(window, cx))
                .enter_sends
        );
    });
}

#[gpui_kit::test]
fn composer_send_key_choice_refuses_composition_and_modal_changes(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 20, 4);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let composer = shell.read(cx).composer().clone();
        let bounds = window.find_by_label("Message").bounds();
        TestInput::new(ElementInputHandler::new(bounds, composer.clone()), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(
                Command::ToggleEnterSends,
                window,
                cx
            )),
            Some(commands::COMPOSING)
        );
        window.press(commands::composer_submit_key(), cx);
        assert!(shell.read(cx).last_intent().is_none());
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        composer.update(cx, |input, cx| input.unmark_text(window, cx));
        window.press(palette_key(), cx);
        window.render_frame(cx);
        assert!(shell
            .update(cx, |shell, cx| shell.run_command(
                Command::ToggleEnterSends,
                window,
                cx
            ))
            .is_some());
        assert!(
            shell
                .update(cx, |shell, cx| shell.command_state(window, cx))
                .enter_sends
        );
        window.input("Use Enter for newlines", cx);
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_none());
        assert!(
            !shell
                .update(cx, |shell, cx| shell.command_state(window, cx))
                .enter_sends
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "にほんご");
    });
}

#[gpui_kit::test]
fn composer_send_key_choice_uses_one_existing_forecast_request_without_optimistic_send(
    cx: &mut TestAppContext,
) {
    use formless_desktop::client::{Command as WireCommand, Update};
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = sessions[0].id.clone();
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: replies::session_snapshot(
                        &id,
                        &serde_json::json!({"session":{"id":id},"running":false,"messages":[]}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            shell.run_command(Command::ToggleEnterSends, window, cx);
            shell.focus_section(PageSection::Composer, window, cx);
            shell.take_client_commands();
        });
        window.render_frame(cx);
        window.input("line", cx);
        window.press("enter", cx);
        window.input("next ไทย", cx);
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .all(|c| !matches!(
                c,
                WireCommand::Review { .. }
                    | WireCommand::Intent(_)
                    | WireCommand::CommitPrepared(_)
            )));
        window.press(commands::composer_submit_key(), cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        let reviews = commands
            .iter()
            .filter(|c| matches!(c, WireCommand::Review { .. }))
            .collect::<Vec<_>>();
        assert_eq!(
            reviews.len(),
            1,
            "modified Enter must prepare exactly one action"
        );
        assert!(
            matches!(reviews[0], WireCommand::Review { intent: formless_desktop::model::Intent::SendMessage { session_id, content, mode: SendMode::Send }, draft_basis, .. } if session_id == &id && content == "line\nnext ไทย" && content == draft_basis)
        );
        assert!(!commands
            .iter()
            .any(|c| matches!(c, WireCommand::Intent(_) | WireCommand::CommitPrepared(_))));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "line\nnext ไทย");
        assert_eq!(shell.read(cx).block_count(), 0);
    });
}

#[gpui_kit::test]
fn composer_send_key_choice_preserves_selection_and_existing_undo_redo(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 20, 4);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.input("original", cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ToggleEnterSends, window, cx)
        });
        window.press(&chord("z"), cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        window.press(REDO_SHORTCUT, cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "original");
        window.press("shift-left", cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ToggleEnterSends, window, cx)
        });
        window.input("X", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "originaX");
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[path = "support/reader.rs"]
mod reader_tests;

#[path = "support/steps.rs"]
mod step_inventory_tests;

#[path = "support/native_step.rs"]
mod native_step_tests;

#[cfg(feature = "daemon-tests")]
#[path = "support/v1_end_to_end.rs"]
mod v1_end_to_end;

#[path = "support/step_enable.rs"]
mod step_enable;

#[path = "support/approvals.rs"]
mod approval_tests;

#[cfg(feature = "daemon-tests")]
#[path = "support/approval_end_to_end.rs"]
mod approval_end_to_end;

#[cfg(feature = "daemon-tests")]
#[path = "../../formless-core/tests/support/env.rs"]
mod credential_env;

#[path = "support/session_model.rs"]
mod session_model_tests;

#[path = "support/model_catalog.rs"]
mod model_catalog_tests;

#[path = "support/pending_files.rs"]
mod pending_files_tests;

#[path = "support/native_send.rs"]
mod native_send_tests;

#[path = "support/reference_controls.rs"]
mod reference_control_tests;
