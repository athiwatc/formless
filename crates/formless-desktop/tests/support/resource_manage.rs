use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_protocol::Request;
use serde_json::json;

#[gpui_kit::test]
fn management_receipts_never_replace_the_global_footer_status(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Notice("Connected".into()), window, cx);
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
            s.apply_client_update(
                Update::ManagementNotice {
                    destination: Destination::Memory,
                    notice: "Done: Delete".into(),
                },
                window,
                cx,
            );
        });
        settle_frames(window, cx);
        assert_eq!(
            window.find("resource-result").value().as_deref(),
            Some("Done: Delete")
        );
        assert_eq!(shell.read(cx).client_status_notice(), "Connected");
        shell.update(cx, |s, cx| {
            s.run_command(Command::GoTo(Destination::Jobs), window, cx);
            s.apply_client_update(
                Update::ManagementNotice {
                    destination: Destination::Memory,
                    notice: "Could not Delete: late rejection".into(),
                },
                window,
                cx,
            );
        });
        settle_frames(window, cx);
        assert_eq!(window.find("resource-result").value().as_deref(), Some(""));
        assert_eq!(shell.read(cx).client_status_notice(), "Connected");
    });
}

#[gpui_kit::test]
fn resource_secondary_summaries_and_bounded_field_values(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        for (destination, value, expected) in [
            (
                Destination::Mcp,
                json!([{"name":"local","transport":"stdio","target":"/bin/demo"}]),
                "stdio · /bin/demo",
            ),
            (
                Destination::Jobs,
                json!([{"id":"j","prompt":"Review","cron":"0 9 * * 1-5","next_run":"2030-01-01T09:00:00Z"}]),
                "0 9 * * 1-5 · Next: 2030-01-01T09:00:00Z",
            ),
            (
                Destination::Memory,
                json!([{"id":"m","scope":"project","text":format!("Line one\ncolon: value {}", "界".repeat(700))}]),
                "Scope: project",
            ),
        ] {
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::Page {
                        destination,
                        result: Ok(value),
                    },
                    window,
                    cx,
                );
                s.run_command(Command::GoTo(destination), window, cx);
            });
            settle_frames(window, cx);
            assert_eq!(
                window
                    .find(("resource-row-secondary", 0usize))
                    .value()
                    .as_deref(),
                Some(expected)
            );
        }
        assert!(window
            .find("resource-detail-truncated")
            .value()
            .unwrap()
            .contains("500 characters"));
        let value = window
            .find(("resource-field-value", 2usize))
            .value()
            .unwrap()
            .to_owned();
        assert!(value.starts_with("Line one\ncolon: value "));
        assert!(value.chars().count() > 400 && value.chars().count() <= 500);
        assert_eq!(
            window
                .find(("resource-field-key", 2usize))
                .value()
                .as_deref(),
            Some("text")
        );
        assert!(window.try_find(("resource-field-key", 3usize)).is_none());
    });
}

#[gpui_kit::test]
fn resource_jobs_narrow_form_and_actions_stay_inside_without_collapsing_the_list(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(560.), px(780.)), 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Page { destination: Destination::Jobs, result: Ok(json!([{"id":"j","prompt":"Review project status","enabled":true,"approved":false}])) }, window, cx);
            s.run_command(Command::GoTo(Destination::Jobs), window, cx);
        });
        settle_frames(window, cx);
        for id in [
            "resource-text-field",
            "resource-schedule-field",
            "resource-add",
            "resource-selected",
        ] {
            let bounds = window.find(id).bounds();
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= px(560.),
                "{id}: {bounds:?}"
            );
        }
        let row = window.find(("resource-row", 0usize)).bounds();
        let title = window.find("resource-selected").bounds();
        assert!(title.top() - row.top() >= px(48.));
        assert!(title.top() - row.top() <= px(100.));
        assert_eq!(
            window
                .find(("resource-field-key", 0usize))
                .value()
                .as_deref(),
            Some("ID")
        );
        assert_eq!(
            window
                .find(("resource-field-value", 0usize))
                .value()
                .as_deref(),
            Some("j")
        );
        assert!(window
            .find(("resource-row-secondary", 0usize))
            .value()
            .unwrap()
            .contains("Next: not scheduled"));
        let primary = window.find(("resource-row-label", 0usize)).bounds();
        assert!(primary.left() - row.left() < px(15.));
        for slot in 1..4usize {
            let bounds = window.find(("resource-action", slot)).bounds();
            assert!(bounds.left() >= px(0.) && bounds.right() <= px(560.));
        }
        let mut reached = [false; 4];
        for _ in 0..32 {
            window.press("tab", cx);
            settle_frames(window, cx);
            if window.find("resource-add").focused() == Some(true) {
                reached[0] = true;
                let bounds = window.find("resource-add").bounds();
                assert!(bounds.top() >= px(0.) && bounds.bottom() <= px(780.));
            }
            for slot in 1..4usize {
                if window.find(("resource-action", slot)).focused() == Some(true) {
                    reached[slot] = true;
                    let bounds = window.find(("resource-action", slot)).bounds();
                    assert!(bounds.top() >= px(0.) && bounds.bottom() <= px(780.));
                }
            }
        }
        assert!(reached.into_iter().all(|reached| reached));
    });
}

#[gpui_kit::test]
fn resource_management_confirms_cancels_and_invalidates_on_selection_refresh_and_navigation(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let value = json!([{"id":"a","text":"Memory A","enabled":true},{"id":"b","text":"Memory B","enabled":false}]);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Page {
                    destination: Destination::Memory,
                    result: Ok(value.clone()),
                },
                window,
                cx,
            );
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
            s.take_client_commands();
        });
        window.render_frame(cx);
        window.click(("resource-action", 2usize), cx);
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_some());
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.click("resource-action-cancel", cx);
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Conversation, window, cx)
        });
        window.press("down", cx);
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Page {
                    destination: Destination::Memory,
                    result: Ok(value.clone()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        shell.update(cx, |s, cx| {
            s.run_command(Command::GoTo(Destination::Tools), window, cx);
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
            s.take_client_commands();
        });
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        window.render_frame(cx);
        window.click(("resource-action", 2usize), cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(c, WireCommand::Manage { destination: Destination::Memory, request: Request::DeleteMemory { id }, .. } if id == "a")));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Offline".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click(("resource-action", 1usize), cx);
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn rejected_skill_and_job_adds_keep_their_drafts_until_success(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        for (destination, text, schedule) in [
            (Destination::Skills, "/missing/skill", ""),
            (
                Destination::Jobs,
                "Review the project",
                "unparseable schedule",
            ),
        ] {
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::Page {
                        destination,
                        result: Ok(json!([])),
                    },
                    window,
                    cx,
                );
                s.run_command(Command::GoTo(destination), window, cx);
                s.take_client_commands();
            });
            settle_frames(window, cx);
            window.click("resource-text-field", cx);
            window.input(text, cx);
            if !schedule.is_empty() {
                window.click("resource-schedule-field", cx);
                window.input(schedule, cx);
            }
            settle_frames(window, cx);
            window.click("resource-add", cx);
            let first = shell.update(cx, |s, _| s.take_client_commands());
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::ManagementNotice {
                        destination,
                        notice: "Could not add: rejected".into(),
                    },
                    window,
                    cx,
                );
            });
            settle_frames(window, cx);
            window.click("resource-add", cx);
            let retry = shell.update(cx, |s, _| s.take_client_commands());
            let requests = |commands: Vec<WireCommand>| {
                commands
                    .into_iter()
                    .filter_map(|command| {
                        if let WireCommand::Manage { request, .. } = command {
                            Some(serde_json::to_value(request).unwrap())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let first = requests(first);
            assert_eq!(first.len(), 1);
            assert_eq!(requests(retry), first);
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::ResourceAdded {
                        destination,
                        text: text.into(),
                        schedule: schedule.into(),
                    },
                    window,
                    cx,
                );
            });
            settle_frames(window, cx);
            window.click("resource-add", cx);
            assert!(requests(shell.update(cx, |s, _| s.take_client_commands())).is_empty());
            window.render_frame(cx);
            assert!(window
                .find("resource-validation-error")
                .value()
                .unwrap()
                .contains("must not be empty"));
        }
    });
}

#[gpui_kit::test]
fn resource_add_form_validates_submits_clears_and_shows_receipts(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Page {
                    destination: Destination::Memory,
                    result: Ok(json!([])),
                },
                window,
                cx,
            );
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
            s.take_client_commands();
        });
        window.render_frame(cx);
        window.click("resource-add", cx);
        window.render_frame(cx);
        assert!(window
            .find("resource-validation-error")
            .value()
            .unwrap()
            .contains("must not be empty"));
        window.click("resource-text-field", cx);
        window.input("  Remember this  ", cx);
        window.render_frame(cx);
        window.click("resource-add", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(c, WireCommand::Manage { destination: Destination::Memory, request: Request::UpsertMemory { id: None, text: Some(text), .. }, .. } if text == "Remember this")));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::ManagementNotice {
                    destination: Destination::Memory,
                    notice: "Could not add memory: rejected".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        window.click("resource-add", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(c, WireCommand::Manage { request: Request::UpsertMemory { text: Some(text), .. }, .. } if text == "Remember this")));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::ResourceAdded {
                    destination: Destination::Memory,
                    text: "Remember this".into(),
                    schedule: String::new(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        window.click("resource-add", cx);
        window.render_frame(cx);
        assert!(window
            .find("resource-validation-error")
            .value()
            .unwrap()
            .contains("must not be empty"));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::ManagementNotice {
                    destination: Destination::Memory,
                    notice: "Done: add memory".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("resource-result").value().as_deref(),
            Some("Done: add memory")
        );
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Notice("Connected · provisional live text".into()),
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("resource-result").value().as_deref(),
            Some("Done: add memory")
        );
        shell.update(cx, |s, cx| {
            s.run_command(Command::GoTo(Destination::Jobs), window, cx);
            s.apply_client_update(
                Update::ManagementNotice {
                    destination: Destination::Memory,
                    notice: "Late memory receipt".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(window.find("resource-result").value().as_deref(), Some(""));
        shell.update(cx, |s, cx| {
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
        });
        window.render_frame(cx);
        assert_eq!(window.find("resource-result").value().as_deref(), Some(""));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::ManagementNotice {
                    destination: Destination::Memory,
                    notice: "Could not add memory: permission denied".into(),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("resource-result").value().as_deref(),
            Some("Could not add memory: permission denied")
        );
    });
}
