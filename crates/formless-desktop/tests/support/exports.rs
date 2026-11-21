use super::session_edit_tests::show;
use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::session_export::{Document, Format};
use gpui_kit::EntityInputHandler as _;

fn requested(shell: &Entity<Shell>, cx: &mut App, id: &str, format: Format) -> uuid::Uuid {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    assert!(!commands.iter().any(|command| matches!(
        command,
        WireCommand::Intent(_) | WireCommand::Review { .. } | WireCommand::EditSession { .. }
    )));
    commands
        .into_iter()
        .find_map(|command| match command {
            WireCommand::ExportSession {
                connection,
                operation,
                session_id,
                format: actual,
            } => {
                assert_eq!(connection, 0);
                assert_eq!(session_id, id);
                assert_eq!(actual, format);
                Some(operation)
            }
            _ => None,
        })
        .unwrap()
}
#[allow(clippy::too_many_arguments)] // Explicit adversarial reply identities at the serialized view boundary.
fn reply(
    shell: &Entity<Shell>,
    window: &mut Window,
    cx: &mut App,
    connection: u64,
    operation: uuid::Uuid,
    id: &str,
    format: Format,
    result: Result<Document, String>,
) {
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::SessionExport {
                connection,
                operation,
                session_id: id.into(),
                format,
                result,
            },
            window,
            cx,
        )
    });
}
fn focus_control(shell: &Entity<Shell>, window: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..35 {
        window.render_frame(cx);
        if window.find(id).focused() == Some(true) {
            break;
        }
        window.press("tab", cx);
    }
    for _ in 0..4 {
        window.render_frame(cx);
        window.simulate_next_frame(cx);
    }
    window.render_frame(cx);
    assert_eq!(window.find(id).focused(), Some(true));
    let bounds = window.find(id).bounds();
    assert!(bounds.intersects(&window.find("session-edit-body").bounds()));
    assert!(shell.read(cx).background_inert(window));
}

#[gpui_kit::test]
fn export_is_explicit_frozen_and_exact_without_touching_unsaved_edits(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent composer ไทย 😀", cx);
    });
    show(cx, handle, &shell, &sessions[1]);
    step(cx, handle, |window, cx| {
        shell
            .read(cx)
            .session_edit_input()
            .unwrap()
            .update(cx, |input, cx| {
                input.set_value("Unsaved name ไทย", window, cx)
            });
        focus_control(&shell, window, cx, "session-export-html");
        window.press("enter", cx);
        let operation = requested(&shell, cx, &sessions[1].id, Format::Html);
        window.render_frame(cx);
        assert_eq!(window.find("session-export-html").disabled(), Some(true));
        assert_eq!(window.find("session-export-copy").disabled(), Some(true));
        let text = "<script>not executed ไทย 😀</script>\r\n".repeat(200);
        reply(
            &shell,
            window,
            cx,
            0,
            operation,
            &sessions[0].id,
            Format::Html,
            Ok(Document {
                name: "wrong".into(),
                text: "wrong".into(),
            }),
        );
        reply(
            &shell,
            window,
            cx,
            0,
            uuid::Uuid::new_v4(),
            &sessions[1].id,
            Format::Html,
            Ok(Document {
                name: "wrong".into(),
                text: "wrong".into(),
            }),
        );
        window.render_frame(cx);
        assert_eq!(window.find("session-export-copy").disabled(), Some(true));
        reply(
            &shell,
            window,
            cx,
            0,
            operation,
            &sessions[1].id,
            Format::Html,
            Ok(Document {
                name: "Exported ไทย".into(),
                text: text.clone(),
            }),
        );
        focus_control(&shell, window, cx, "session-export-copy");
        window.press("enter", cx);
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), text);
        assert!(window
            .find("session-export-preview")
            .value()
            .unwrap()
            .contains("Preview truncated"));
        let revision = shell.read(cx).session_export_revision().unwrap();
        focus_control(&shell, window, cx, "session-export-markdown");
        window.press("enter", cx);
        let newer = requested(&shell, cx, &sessions[1].id, Format::Markdown);
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        shell.update(cx, |shell, cx| {
            shell.copy_presented_session_export(&sessions[1].id, 0, revision, cx)
        });
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "unchanged"
        );
        reply(
            &shell,
            window,
            cx,
            0,
            operation,
            &sessions[1].id,
            Format::Html,
            Ok(Document {
                name: "late".into(),
                text: "late".into(),
            }),
        );
        reply(
            &shell,
            window,
            cx,
            0,
            newer,
            &sessions[1].id,
            Format::Markdown,
            Err("fixture rejected read".into()),
        );
        window.render_frame(cx);
        assert_eq!(window.find("session-export-copy").disabled(), Some(true));
        assert!(window
            .find("session-export-notice")
            .value()
            .unwrap()
            .contains("fixture rejected"));
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value(),
            "Unsaved name ไทย"
        );
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "Unsent composer ไทย 😀");
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!shell.read(cx).background_inert(window));
    });
}

#[gpui_kit::test]
fn export_preedit_disconnect_and_closed_panel_never_replay_or_install_late_results(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, &sessions[0]);
    let mut old = uuid::Uuid::nil();
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.replace_and_mark_text_in_range(None, "กำลัง", None, window, cx);
        });
        assert!(input.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        // Click the read action while composition is live; no wire request.
        let revision = shell.read(cx).session_export_revision().unwrap();
        shell.update(cx, |shell, cx| {
            shell.request_presented_session_export(
                &sessions[0].id,
                0,
                revision,
                Format::Json,
                window,
                cx,
            )
        });
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::ExportSession { .. })));
        input.update(cx, |input, cx| input.unmark_text(window, cx));
        focus_control(&shell, window, cx, "session-export-json");
        window.press("enter", cx);
        old = requested(&shell, cx, &sessions[0].id, Format::Json);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "lost".into(),
                },
                window,
                cx,
            )
        });
        reply(
            &shell,
            window,
            cx,
            0,
            old,
            &sessions[0].id,
            Format::Json,
            Ok(Document {
                name: "late".into(),
                text: "late".into(),
            }),
        );
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "restored".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("session-export-copy").disabled(), Some(true));
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.press("escape", cx);
    });
    step(cx, handle, |window, cx| {
        let query = shell.read(cx).directory_query().clone();
        query.update(cx, |query, cx| query.set_value("", window, cx));
    });
    show(cx, handle, &shell, &sessions[1]);
    step(cx, handle, |window, cx| {
        reply(
            &shell,
            window,
            cx,
            0,
            old,
            &sessions[0].id,
            Format::Json,
            Ok(Document {
                name: "old panel".into(),
                text: "old panel".into(),
            }),
        );
        window.render_frame(cx);
        assert_eq!(window.find("session-export-copy").disabled(), Some(true));
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value(),
            sessions[1].name
        );
    });
}

#[gpui_kit::test]
fn export_controls_remain_reachable_at_two_hundred_percent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        })
    });
    show(cx, handle, &shell, &sessions[0]);
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-markdown");
        window.press("enter", cx);
        let operation = requested(&shell, cx, &sessions[0].id, Format::Markdown);
        let text = "Long ไทย 😀 export\n".repeat(500);
        reply(
            &shell,
            window,
            cx,
            0,
            operation,
            &sessions[0].id,
            Format::Markdown,
            Ok(Document {
                name: "name".into(),
                text: text.clone(),
            }),
        );
        focus_control(&shell, window, cx, "session-export-copy");
        let body = window.find("session-edit-body").bounds();
        let button = window.find("session-export-copy").bounds();
        assert!(button.top() >= body.top() && button.bottom() <= body.bottom());
        window.press("enter", cx);
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), text);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!shell.read(cx).background_inert(window));
    });
}

struct SaveDirectory(std::path::PathBuf);
impl SaveDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("formless-shell-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}
impl Drop for SaveDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ready_file_export(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    id: &str,
    format: Format,
    text: &str,
) -> uuid::Uuid {
    step(cx, handle, |window, cx| {
        let revision = shell.read(cx).session_export_revision().unwrap();
        shell.update(cx, |shell, cx| {
            shell.request_presented_session_export(id, 0, revision, format, window, cx)
        });
        let operation = requested(shell, cx, id, format);
        reply(
            shell,
            window,
            cx,
            0,
            operation,
            id,
            format,
            Ok(Document {
                name: "../../private name".into(),
                text: text.into(),
            }),
        );
        window.render_frame(cx);
        shell.read(cx).session_export_revision().unwrap()
    })
}

#[gpui_kit::test]
fn export_save_uses_the_platform_picker_and_writes_complete_exact_source_only_after_choice(
    cx: &mut TestAppContext,
) {
    let directory = SaveDirectory::new();
    let path = directory.file("chosen.html");
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("private composer draft", cx);
    });
    show(cx, handle, &shell, &sessions[1]);
    let text = "<script>not opened ไทย 👩‍💻</script>\r\n\t  \n".repeat(200);
    let revision = ready_file_export(cx, handle, &shell, &sessions[1].id, Format::Html, &text);
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.set_value("unsaved private name", window, cx)
        });
        focus_control(&shell, window, cx, "session-export-save-file");
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("session-export-save-file").disabled(),
            Some(true)
        );
        // A queued duplicate cannot start a second system dialog.
        shell.update(cx, |shell, cx| {
            shell.save_presented_session_export(&sessions[1].id, 0, revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
    cx.run_until_parked();
    assert!(cx.did_prompt_for_new_path());
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
    cx.simulate_new_path_selection(|initial| {
        assert!(initial.is_absolute());
        Some(path.clone())
    });
    cx.run_until_parked();
    assert!(!cx.did_prompt_for_new_path());
    assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window
            .find("session-export-notice")
            .value()
            .unwrap()
            .contains("Saved complete HTML source"));
        assert_eq!(
            window.find("session-export-notice").role(),
            Some(gpui_kit::Role::Status)
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "private composer draft");
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "unsaved private name"
        );
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
}

#[gpui_kit::test]
fn export_save_cancel_and_existing_file_preserve_data_and_stale_buttons_cannot_reopen_picker(
    cx: &mut TestAppContext,
) {
    let directory = SaveDirectory::new();
    let path = directory.file("existing.md");
    std::fs::write(&path, b"existing owner's bytes").unwrap();
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, &sessions[0]);
    let revision = ready_file_export(
        cx,
        handle,
        &shell,
        &sessions[0].id,
        Format::Markdown,
        "new exact\r\n",
    );
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-save-file");
        window.press("enter", cx);
    });
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    assert!(!cx.did_prompt_for_new_path());
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window
            .find("session-export-notice")
            .value()
            .unwrap()
            .contains("canceled"));
        shell.update(cx, |shell, cx| {
            shell.save_presented_session_export(&sessions[0].id, 0, revision, window, cx)
        });
    });
    assert!(!cx.did_prompt_for_new_path());
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-save-file");
        window.press("enter", cx);
    });
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    assert_eq!(std::fs::read(&path).unwrap(), b"existing owner's bytes");
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window
            .find("session-export-notice")
            .value()
            .unwrap()
            .contains("already exists"));
        focus_control(&shell, window, cx, "session-export-copy");
        window.press("enter", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "new exact\r\n"
        );
    });
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[gpui_kit::test]
fn export_save_late_dialog_choices_cannot_write_after_panel_or_connection_replacement(
    cx: &mut TestAppContext,
) {
    let directory = SaveDirectory::new();
    let first = directory.file("old-session.json");
    let second = directory.file("old-connection.md");
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, &sessions[0]);
    ready_file_export(cx, handle, &shell, &sessions[0].id, Format::Json, "{}\n");
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-save-file");
        window.press("enter", cx);
        window.press("escape", cx);
        let query = shell.read(cx).directory_query().clone();
        query.update(cx, |input, cx| input.set_value("", window, cx));
    });
    show(cx, handle, &shell, &sessions[1]);
    cx.simulate_new_path_selection(|_| Some(first.clone()));
    cx.run_until_parked();
    assert!(!first.exists());
    ready_file_export(
        cx,
        handle,
        &shell,
        &sessions[1].id,
        Format::Markdown,
        "second",
    );
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-save-file");
        window.press("enter", cx);
        // Actual connection atomic advances before its UI update is drained.
        assert_eq!(shell.read(cx).retire_recording_question_connection(), 1);
    });
    cx.simulate_new_path_selection(|_| Some(second.clone()));
    cx.run_until_parked();
    assert!(!second.exists());
    assert!(!cx.did_prompt_for_new_path());
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[gpui_kit::test]
fn export_save_respects_preedit_and_is_keyboard_reachable_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let directory = SaveDirectory::new();
    let path = directory.file("small.md");
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        });
    });
    show(cx, handle, &shell, &sessions[0]);
    let revision = ready_file_export(
        cx,
        handle,
        &shell,
        &sessions[0].id,
        Format::Markdown,
        "ไทย\r\n\t  ",
    );
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.replace_and_mark_text_in_range(None, "กำลัง", None, window, cx);
        });
        shell.update(cx, |shell, cx| {
            shell.save_presented_session_export(&sessions[0].id, 0, revision, window, cx)
        });
        assert!(input.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        input.update(cx, |input, cx| input.unmark_text(window, cx));
    });
    assert!(!cx.did_prompt_for_new_path());
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-save-file");
        window.press("enter", cx);
    });
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    assert_eq!(std::fs::read(path).unwrap(), "ไทย\r\n\t  ".as_bytes());
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-save-file");
        let bounds = window.find("session-export-save-file").bounds();
        assert!(bounds.left() >= px(0.) && bounds.right() <= px(480.));
        assert_eq!(
            window.find("session-export-save-file").focused(),
            Some(true)
        );
        window.press("escape", cx);
        assert!(window.try_find("session-edit-dialog").is_none());
    });
}

#[gpui_kit::test]
fn export_save_cannot_cross_profile_attachment_even_when_connection_and_session_ids_repeat(
    cx: &mut TestAppContext,
) {
    let directory = SaveDirectory::new();
    let path = directory.file("old-profile.md");
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, &sessions[0]);
    ready_file_export(
        cx,
        handle,
        &shell,
        &sessions[0].id,
        Format::Markdown,
        "old profile canary",
    );
    step(cx, handle, |window, cx| {
        focus_control(&shell, window, cx, "session-export-save-file");
        window.press("enter", cx);
    });
    assert!(cx.did_prompt_for_new_path());
    let replacement = daemon_view(cx, handle, &shell);
    assert_eq!(sessions[0].id, replacement[0].id);
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    assert!(
        !path.exists(),
        "a new client attachment must retire the old export even if counters repeat"
    );
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("session-export-copy").disabled(), Some(true));
        assert_eq!(
            window.find("session-export-save-file").disabled(),
            Some(true)
        );
    });
}
