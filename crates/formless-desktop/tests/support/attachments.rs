use super::*;
use formless_desktop::client::attachments::{Intake, Operation, Outcome};
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::pending_files::Report;
use serde_json::json;

fn setup(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = sessions[0].id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation: shell.client_generation(),
                    snapshot: replies::session_snapshot(
                        &id,
                        &json!({"session":{"id":id},"running":false,"messages":[]}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            shell.focus_section(PageSection::Composer, window, cx);
            shell.poll_client(window, cx);
        });
        let refresh = queued(&shell, cx).pop().expect("selection refresh");
        assert!(matches!(refresh.intake, Intake::Refresh));
        complete(&shell, refresh, vec![], window, cx);
    });
    (handle, shell)
}

#[gpui_kit::test]
fn transcript_chips_render_after_history_refresh_and_live_user_append(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        let meta = json!({"attachments":[file("image", "sent-image.png", "image/png"), {"name":"project-reference","reference":true,"bytes":4096}, {"name":"retained-name.pdf","mime":"application/pdf","bytes":12,"expired":true}, {"name":"missing.txt","unavailable":true}]}).to_string();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Snapshot { generation:shell.client_generation(), snapshot:replies::session_snapshot(&id, &json!({"session":{"id":id},"running":false,"messages":[{"id":"sent","session_id":id,"seq":1,"role":"user","content":"Review attachments","created_at":"now","meta":meta}]})).unwrap() }, window, cx);
            shell.focus_section(PageSection::Conversation, window, cx);
        });
        window.render_frame(cx);
        assert!(window.find("transcript-attachment-1-0").visible());
        assert!(window.find("transcript-attachment-1-1").visible());
        assert!(window
            .find("transcript-attachment-1-2")
            .label()
            .unwrap()
            .contains("expired"));
        let text = painted(window).join(" ");
        assert!(
            text.contains("sent-image.png")
                && text.contains("expired")
                && text.contains("unavailable")
        );
        let mut message = typed_message(&id, 2, "user", "");
        message.meta =
            Some(json!({"attachments":[file("live", "live-user.png", "image/png")]}).to_string());
        shell.update(cx, |shell, cx| {
            shell
                .apply_transcript_event(
                    &formless_protocol::Event::MessageAppended {
                        session_id: id,
                        message,
                    },
                    cx,
                )
                .unwrap();
        });
        window.render_frame(cx);
        assert!(window.find("transcript-attachment-2-0").visible());
        assert!(painted(window).join(" ").contains("live-user.png"));
    });
}

fn queued(shell: &Entity<Shell>, cx: &mut App) -> Vec<Operation> {
    shell
        .update(cx, |shell, _| shell.take_client_commands())
        .into_iter()
        .filter_map(|command| match command {
            WireCommand::StageAttachments(operation) => Some(operation),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn inventory_preflight_keeps_send_disabled_until_waiting_attachments_finish(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "Reconnected".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(
                Update::Snapshot {
                    generation: shell.client_generation(),
                    snapshot: replies::session_snapshot(
                        &id,
                        &json!({"session":{"id":id},"running":false,"messages":[]}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            shell.focus_section(PageSection::Composer, window, cx);
        });
        window.input("Send with this file", cx);
        shell.update(cx, |shell, cx| {
            shell.attach_paths(vec!["/waiting.txt".into()], cx)
        });
        let refresh = queued(&shell, cx).pop().unwrap();
        assert!(matches!(refresh.intake, Intake::Refresh));
        window.render_frame(cx);
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        complete(&shell, refresh, vec![], window, cx);
        let staged = queued(&shell, cx).pop().unwrap();
        assert!(
            matches!(&staged.intake, Intake::Paths(paths) if paths == &[std::path::PathBuf::from("/waiting.txt")])
        );
        window.render_frame(cx);
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        complete(
            &shell,
            staged,
            vec![file("waiting", "waiting.txt", "text/plain")],
            window,
            cx,
        );
        window.render_frame(cx);
        assert_ne!(window.find("composer-primary").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn successful_attach_clears_only_its_earlier_attachment_notice(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_paths(vec!["/directory".into()], cx)
        });
        let operation = queued(&shell, cx).pop().unwrap();
        let (client, connection) = shell.read(cx).client_identity().unwrap();
        let id = operation.session_id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::AttachmentStaged(Outcome {
                    client,
                    connection,
                    operation: operation.id,
                    session_id: id.clone(),
                    errors: vec!["Cannot attach a directory".into()],
                    pending: Report::decode(&id, json!({"session_id":id,"attachments":[]})),
                }),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("composer-notice")
            .value()
            .unwrap()
            .contains("directory"));
        shell.update(cx, |shell, cx| {
            shell.attach_paths(vec!["/valid".into()], cx)
        });
        let operation = queued(&shell, cx).pop().unwrap();
        complete(
            &shell,
            operation,
            vec![file("valid", "valid.txt", "text/plain")],
            window,
            cx,
        );
        window.render_frame(cx);
        assert!(window.try_find("composer-notice").is_none());
    });
    cx.write_to_clipboard(ClipboardItem::new_string(
        "x".repeat(formless_desktop::memory::limits::COMPOSER_MAX_BYTES + 1),
    ));
    step(cx, handle, |window, cx| window.press(&chord("v"), cx));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let unrelated = window.find("composer-notice").value().unwrap().to_string();
        assert!(unrelated.contains("Paste not added"));
        shell.update(cx, |shell, cx| {
            shell.attach_paths(vec!["/another-valid".into()], cx)
        });
        let operation = queued(&shell, cx).pop().unwrap();
        complete(
            &shell,
            operation,
            vec![file("valid", "valid.txt", "text/plain")],
            window,
            cx,
        );
        window.render_frame(cx);
        assert_eq!(
            window.find("composer-notice").value(),
            Some(unrelated.as_str())
        );
    });
}

#[gpui_kit::test]
fn oversized_clipboard_images_are_rejected_before_copy_or_conversion(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    for format in [gpui_kit::ImageFormat::Png, gpui_kit::ImageFormat::Tiff] {
        cx.write_to_clipboard(ClipboardItem::new_image(&gpui_kit::Image::from_bytes(
            format,
            vec![0; formless_desktop::client::attachments::MAX_FILE_BYTES + 1],
        )));
        step(cx, handle, |window, cx| window.press(&chord("v"), cx));
        cx.run_until_parked();
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            assert!(window
                .find("composer-notice")
                .value()
                .unwrap()
                .contains("larger than 20 MiB"));
            assert!(queued(&shell, cx).is_empty());
            assert!(shell.read(cx).draft(cx).is_empty());
        });
    }
}

#[gpui_kit::test]
fn reconnect_refreshes_pending_chips_for_the_restored_session(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| shell.attach_paths(vec!["/file".into()], cx));
        let operation = queued(&shell, cx).pop().unwrap();
        complete(
            &shell,
            operation,
            vec![file("kept", "kept.txt", "text/plain")],
            window,
            cx,
        );
        let id = shell.read(cx).sidebar().active_id().unwrap().to_string();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "Reconnected".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(
                Update::Snapshot {
                    generation: shell.client_generation(),
                    snapshot: replies::session_snapshot(
                        &id,
                        &json!({"session":{"id":id},"running":false,"messages":[]}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            shell.poll_client(window, cx);
        });
        let operation = queued(&shell, cx)
            .pop()
            .expect("reconnect inventory refresh");
        assert!(matches!(operation.intake, Intake::Refresh));
        complete(
            &shell,
            operation,
            vec![file("kept", "kept.txt", "text/plain")],
            window,
            cx,
        );
        window.render_frame(cx);
        assert!(window.find(("composer-attachment", 0_usize)).visible());
        assert_ne!(window.find("composer-primary").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn composer_batches_reserve_capacity_and_reject_extras(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| shell.attach_paths(vec!["/seed".into()], cx));
        let operation = queued(&shell, cx).pop().unwrap();
        complete(
            &shell,
            operation,
            (0..62)
                .map(|i| file(&i.to_string(), "seed", "text/plain"))
                .collect(),
            window,
            cx,
        );
        shell.update(cx, |shell, cx| {
            shell.attach_paths((0..100).map(|i| format!("/file-{i}").into()).collect(), cx)
        });
        let operations = queued(&shell, cx);
        assert_eq!(operations.len(), 1);
        assert!(matches!(&operations[0].intake, Intake::Paths(paths) if paths.len() == 2));
        shell.update(cx, |shell, cx| {
            shell.attach_paths(vec!["/extra".into()], cx)
        });
        assert!(queued(&shell, cx).is_empty());
        window.render_frame(cx);
        assert!(window
            .find("composer-notice")
            .value()
            .unwrap()
            .contains("not staged"));
    });
}

fn file(id: &str, name: &str, mime: &str) -> serde_json::Value {
    json!({"id":id,"name":name,"mime":mime,"bytes":2048,"path":format!("/artifacts/{id}")})
}

fn complete(
    shell: &Entity<Shell>,
    operation: Operation,
    files: Vec<serde_json::Value>,
    window: &mut Window,
    cx: &mut App,
) {
    let (client, connection) = shell.read(cx).client_identity().unwrap();
    let pending = Report::decode(
        &operation.session_id,
        json!({"session_id":operation.session_id,"attachments":files}),
    )
    .unwrap();
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::AttachmentStaged(Outcome {
                client,
                connection,
                operation: operation.id,
                session_id: operation.session_id,
                errors: vec![],
                pending: Ok(pending),
            }),
            window,
            cx,
        )
    });
}

fn stage(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| {
        shell.attach_paths(
            vec!["/local/design.png".into(), "/local/notes.txt".into()],
            cx,
        )
    });
    window.render_frame(cx);
    assert!(window
        .find(("composer-attachment-uploading", 0_usize))
        .visible());
    assert_eq!(window.find("composer-primary").disabled(), Some(true));
    let operation = queued(shell, cx).pop().unwrap();
    assert!(matches!(operation.intake, Intake::Paths(ref paths) if paths.len() == 2));
    complete(
        shell,
        operation,
        vec![
            file("image-1", "design.png", "image/png"),
            file("text-2", "notes.txt", "text/plain"),
        ],
        window,
        cx,
    );
    window.render_frame(cx);
}

#[gpui_kit::test]
fn composer_attachment_chips_render_after_staging_and_remove_by_artifact_id(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        stage(&shell, window, cx);
        assert!(window.find(("composer-attachment", 0_usize)).visible());
        assert!(window.find(("composer-attachment", 1_usize)).visible());
        assert!(painted(window).join(" ").contains("design.png"));
        window.click(("composer-attachment-remove", 0_usize), cx);
        let operation = queued(&shell, cx).pop().unwrap();
        assert!(matches!(&operation.intake, Intake::Detach { id } if id == "image-1"));
        complete(
            &shell,
            operation,
            vec![file("text-2", "notes.txt", "text/plain")],
            window,
            cx,
        );
        window.render_frame(cx);
        assert!(!painted(window).join(" ").contains("design.png"));
        assert!(painted(window).join(" ").contains("notes.txt"));
    });
}

#[gpui_kit::test]
fn composer_attachment_empty_backspace_removes_last_without_changing_text(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        stage(&shell, window, cx);
        window.input("x", cx);
        window.press("backspace", cx);
        assert!(
            queued(&shell, cx).is_empty(),
            "ordinary text deletion must not detach"
        );
        window.press("backspace", cx);
        let operation = queued(&shell, cx).pop().unwrap();
        assert!(matches!(&operation.intake, Intake::Detach { id } if id == "text-2"));
        complete(
            &shell,
            operation,
            vec![file("image-1", "design.png", "image/png")],
            window,
            cx,
        );
        assert!(shell.read(cx).draft(cx).is_empty());
    });
}

#[gpui_kit::test]
fn composer_attachment_only_send_uses_review_and_success_refreshes_chips(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        stage(&shell, window, cx);
        assert_eq!(window.find("composer-primary").disabled(), None);
        window.click("composer-primary", cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(commands.iter().any(|command| matches!(command, WireCommand::Review { intent: formless_desktop::model::Intent::SendMessage { content, .. }, draft_basis, .. } if content.is_empty() && draft_basis.is_empty())));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, WireCommand::Intent(_))));
        let id = shell.read(cx).sidebar().active_id().unwrap().to_owned();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Action {
                    session_id: id,
                    result: Ok(()),
                },
                window,
                cx,
            )
        });
        let refresh = queued(&shell, cx).pop().unwrap();
        assert!(matches!(refresh.intake, Intake::Refresh));
        complete(&shell, refresh, vec![], window, cx);
        window.render_frame(cx);
        assert!(window.try_find(("composer-attachment", 0_usize)).is_none());
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn composer_attachment_stale_result_ignored_after_session_switch_and_return(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_paths(vec!["old.png".into()], cx)
        });
        let old = queued(&shell, cx).pop().unwrap();
        let old_id = old.session_id.clone();
        let other = shell
            .read(cx)
            .sidebar()
            .rows()
            .iter()
            .find(|row| row.id != old_id)
            .unwrap()
            .id
            .clone();
        window.render_frame(cx);
        window.click(session_row_id(&other), cx);
        complete(
            &shell,
            Operation {
                id: old.id,
                session_id: old.session_id.clone(),
                intake: Intake::Refresh,
            },
            vec![file("stale", "stale.png", "image/png")],
            window,
            cx,
        );
        window.render_frame(cx);
        assert!(window.try_find(("composer-attachment", 0_usize)).is_none());
        window.click(session_row_id(&old_id), cx);
        shell.update(cx, |shell, cx| shell.poll_client(window, cx));
        complete(
            &shell,
            old,
            vec![file("stale", "stale.png", "image/png")],
            window,
            cx,
        );
        window.render_frame(cx);
        assert!(window.try_find(("composer-attachment", 0_usize)).is_none());
        assert!(queued(&shell, cx)
            .iter()
            .any(|operation| matches!(operation.intake, Intake::Refresh)
                && operation.session_id == old_id));
    });
}

#[gpui_kit::test]
fn composer_attachment_upload_errors_become_notice_and_release_send(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_paths(vec!["/directory".into()], cx)
        });
        let operation = queued(&shell, cx).pop().unwrap();
        let (client, connection) = shell.read(cx).client_identity().unwrap();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::AttachmentStaged(Outcome {
                    client,
                    connection,
                    operation: operation.id,
                    session_id: operation.session_id,
                    errors: vec![
                        "Cannot attach directory: select a regular file, not a directory".into(),
                    ],
                    pending: Err("Cannot refresh pending attachments".into()),
                }),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("composer-notice")
            .value()
            .unwrap()
            .contains("not a directory"));
        assert!(window
            .try_find(("composer-attachment-uploading", 0_usize))
            .is_none());
        window.input("ordinary text", cx);
        window.render_frame(cx);
        assert_eq!(window.find("composer-primary").disabled(), None);
    });
}

#[gpui_kit::test]
fn composer_attachment_clipboard_paths_and_image_use_staging_not_text(cx: &mut TestAppContext) {
    let (handle, shell) = setup(cx);
    cx.write_to_clipboard(
        gpui_kit::ClipboardEntry::ExternalPaths(gpui_kit::ExternalPaths(
            ["/one.png".into(), "/two.txt".into()].into_iter().collect(),
        ))
        .into(),
    );
    step(cx, handle, |window, cx| window.press(&chord("v"), cx));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert!(shell.read(cx).draft(cx).is_empty());
        let operation = queued(&shell, cx).pop().unwrap();
        assert!(matches!(&operation.intake, Intake::Paths(paths) if paths.len() == 2));
        complete(&shell, operation, vec![], window, cx);
    });
    cx.write_to_clipboard(ClipboardItem::new_image(&gpui_kit::Image::from_bytes(
        gpui_kit::ImageFormat::Png,
        vec![1, 2, 3],
    )));
    step(cx, handle, |window, cx| window.press(&chord("v"), cx));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let operation = queued(&shell, cx).pop().unwrap();
        assert!(
            matches!(&operation.intake, Intake::Bytes { name, mime, bytes } if name == "pasted-image-1.png" && mime == "image/png" && bytes == &[1, 2, 3])
        );
        complete(&shell, operation, vec![], window, cx);
    });
    cx.write_to_clipboard(ClipboardItem::new_string("unchanged text paste".into()));
    step(cx, handle, |window, cx| window.press(&chord("v"), cx));
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "unchanged text paste");
        assert!(queued(&shell, cx).is_empty());
    });
}
