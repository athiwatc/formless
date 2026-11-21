use super::*;
use formless_desktop::preferences::{Settings, Store};
use gpui_kit::component::ThemeMode;
use std::path::PathBuf;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("formless-shell-prefs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self) -> PathBuf {
        self.0.join("view.json")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[gpui_kit::test]
fn preferences_attachment_preserves_theme_without_saved_settings(cx: &mut TestAppContext) {
    let directory = Directory::new();
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |window, cx| {
        for dark in [true, false] {
            Theme::change(
                if dark {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                },
                Some(window),
                cx,
            );
            for corrupt in [false, true] {
                let path = directory.0.join(format!("{dark}-{corrupt}.json"));
                if corrupt {
                    std::fs::write(&path, b"invalid preferences").unwrap();
                }
                shell.update(cx, |shell, cx| {
                    shell.attach_preferences(Store::open(path.clone()), window, cx)
                });
                assert_eq!(Theme::global(cx).is_dark(), dark);
                assert_eq!(path.exists(), corrupt);
            }
        }
    });
}

#[gpui_kit::test]
fn saved_theme_wins_over_initial_and_later_system_sync(cx: &mut TestAppContext) {
    let directory = Directory::new();
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |window, cx| {
        for dark in [true, false] {
            let path = directory.0.join(format!("{dark}.json"));
            let settings = Settings::for_view(TextScale::DEFAULT, dark, true, true, true);
            Store::open(path.clone()).save(settings).unwrap();
            Theme::change(
                if dark {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                },
                Some(window),
                cx,
            );
            shell.update(cx, |shell, cx| {
                shell.attach_preferences(Store::open(path), window, cx);
            });
            assert_eq!(Theme::global(cx).is_dark(), dark);
            shell.update(cx, |shell, cx| shell.sync_system_appearance(window, cx));
            assert_eq!(Theme::global(cx).is_dark(), dark);
        }
    });
}

#[gpui_kit::test]
fn preferences_save_is_explicit_and_reload_preserves_draft_selection_and_composer(
    cx: &mut TestAppContext,
) {
    let directory = Directory::new();
    let path = directory.file();
    let (handle, shell) = open(cx, WIDE, 20, 4);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_preferences(Store::open(path.clone()), window, cx)
        });
        window.render_frame(cx);
        window.input("private draft canary", cx);
        window.press("shift-left", cx);
        let selection = shell.read(cx).composer_selection(cx);
        let composer = shell.read(cx).composer().clone();
        shell.update(cx, |shell, cx| {
            for command in [
                Command::ToggleTheme,
                Command::ToggleEnterSends,
                Command::ToggleContext,
                Command::ToggleSessions,
                Command::IncreaseTextSize,
            ] {
                assert_eq!(shell.run_command(command, window, cx), None);
            }
        });
        assert!(!path.exists());
        let expected = shell.read(cx).view_preferences(cx);
        assert_eq!(
            expected,
            Settings::for_view(TextScale::from_percent(125), true, false, false, false)
        );
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::SaveDesktopPreferences, window, cx)
        });
        assert_eq!(Store::open(path.clone()).settings(), &expected);
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("private draft canary"));
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ToggleTheme, window, cx);
            shell.run_command(Command::ToggleEnterSends, window, cx);
            shell.run_command(Command::ResetTextSize, window, cx);
            shell.run_command(Command::ReloadDesktopPreferences, window, cx);
        });
        assert_eq!(shell.read(cx).view_preferences(cx), expected);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "private draft canary");
        assert_eq!(shell.read(cx).composer_selection(cx), selection);
        assert_eq!(shell.read(cx).composer().entity_id(), composer.entity_id());
        assert!(shell.read(cx).last_intent().is_none());
        window.render_frame(cx);
        assert!(window
            .find("preferences-notice")
            .value()
            .unwrap()
            .contains("saved only"));
    });
}

#[gpui_kit::test]
fn preferences_conflicts_corruption_and_explicit_recovery_never_overwrite_or_clear_drafts(
    cx: &mut TestAppContext,
) {
    let directory = Directory::new();
    let path = directory.file();
    let (handle, shell) = open(cx, WIDE, 20, 4);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_preferences(Store::open(path.clone()), window, cx)
        });
        window.render_frame(cx);
        window.input("retained", cx);
        let mut other = Store::open(path.clone());
        let chosen = Settings::for_view(TextScale::from_percent(150), true, false, true, false);
        other.save(chosen.clone()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::SaveDesktopPreferences, window, cx)
        });
        window.render_frame(cx);
        assert!(window
            .find("preferences-notice")
            .value()
            .unwrap()
            .contains("changed"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ReloadDesktopPreferences, window, cx)
        });
        assert_eq!(shell.read(cx).view_preferences(cx), chosen);
        std::fs::write(&path, b"private broken canary").unwrap();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ReloadDesktopPreferences, window, cx)
        });
        assert_eq!(shell.read(cx).view_preferences(cx), chosen);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::SaveDesktopPreferences, window, cx)
        });
        assert_eq!(std::fs::read(&path).unwrap(), b"private broken canary");
        window.render_frame(cx);
        assert!(!window
            .find("preferences-notice")
            .value()
            .unwrap()
            .contains("private broken canary"));
        std::fs::remove_file(&path).unwrap();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ReloadDesktopPreferences, window, cx)
        });
        assert_eq!(shell.read(cx).view_preferences(cx), Settings::default());
        assert!(!path.exists());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "retained");
        assert!(shell.read(cx).last_intent().is_none());
    });
}

#[gpui_kit::test]
fn preferences_reload_keeps_active_composition_and_modal_ownership(cx: &mut TestAppContext) {
    let directory = Directory::new();
    let path = directory.file();
    let (handle, shell) = open(cx, WIDE, 20, 4);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_preferences(Store::open(path.clone()), window, cx)
        });
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
                Command::ReloadDesktopPreferences,
                window,
                cx
            )),
            Some(commands::COMPOSING)
        );
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        composer.update(cx, |input, cx| input.unmark_text(window, cx));
        window.press(palette_key(), cx);
        window.render_frame(cx);
        assert!(shell
            .update(cx, |shell, cx| shell.run_command(
                Command::SaveDesktopPreferences,
                window,
                cx
            ))
            .is_some());
        assert!(!path.exists());
        window.input("Save desktop preferences", cx);
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(path.exists());
        assert!(window.try_find("palette").is_none());
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
        });
        window.render_frame(cx);
        let query = shell.read(cx).directory_query().clone();
        let bounds = window.find_by_label("Filter Sessions").bounds();
        TestInput::new(ElementInputHandler::new(bounds, query.clone()), window).compose_text(
            None,
            "ไทย",
            None,
            cx,
        );
        assert_eq!(
            shell.update(cx, |shell, cx| shell.run_command(
                Command::ReloadDesktopPreferences,
                window,
                cx
            )),
            Some(commands::COMPOSING)
        );
        assert!(query.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "にほんご");
    });
}

#[gpui_kit::test]
fn preferences_controls_and_status_are_reachable_at_two_hundred_percent(cx: &mut TestAppContext) {
    let directory = Directory::new();
    let path = directory.file();
    let mut store = Store::open(path.clone());
    store
        .save(Settings::for_view(
            TextScale::LARGEST,
            true,
            true,
            true,
            false,
        ))
        .unwrap();
    let (handle, shell) = open(cx, WIDE, 20, 4);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_preferences(Store::open(path.clone()), window, cx)
        });
    });
    cx.simulate_window_resize(handle.into(), size(px(480.), px(360.)));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).text_scale(), TextScale::LARGEST);
        assert!(!shell.read(cx).background_inert(window));
        window.press(palette_key(), cx);
        window.render_frame(cx);
        window.input("Save desktop preferences", cx);
        window.render_frame(cx);
        assert_eq!(
            palette_titles(window),
            ["Save desktop preferences", "Reload desktop preferences"]
        );
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.try_find("palette").is_none());
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Status, window, cx)
        });
        settle_frames(window, cx);
        // A long status may exceed one screen at 200%. Both ends remain
        // keyboard reachable rather than requiring the whole prose at once.
        window.press("home", cx);
        window.render_frame(cx);
        let notice = window.find("preferences-notice");
        assert!(notice.value().unwrap().contains("preferences saved"));
        assert!(notice.bounds().size.width <= px(480.));
        assert!(notice.bounds().top() >= px(0.) && notice.bounds().top() < px(360.));
        window.press("pagedown", cx);
        window.render_frame(cx);
        window.press("end", cx);
        window.render_frame(cx);
        let last = window.find("status-detail").bounds();
        assert!(
            last.bottom() <= px(360.) && last.bottom() > px(0.),
            "{last:?}"
        );
        window.press("home", cx);
        window.render_frame(cx);
        assert!(window.find("preferences-notice").bounds().top() >= px(0.));
    });
}

#[gpui_kit::test]
fn preferences_reload_retires_pending_preparation_without_replaying_actions(
    cx: &mut TestAppContext,
) {
    use formless_desktop::client::{Command as WireCommand, Update};
    let directory = Directory::new();
    let path = directory.file();
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_preferences(Store::open(path.clone()), window, cx)
        });
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
        window.render_frame(cx);
        window.input("keep pending draft", cx);
        window.click("composer-primary", cx);
        let prepared = daemon_quote(&shell, cx, true);
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|c| matches!(c, WireCommand::Review { .. })));
        Store::open(path.clone())
            .save(Settings::for_view(
                TextScale::from_percent(125),
                true,
                false,
                false,
                false,
            ))
            .unwrap();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ReloadDesktopPreferences, window, cx)
        });
        assert_ne!(shell.read(cx).client_epoch(), Some(prepared.epoch));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Prepared(prepared), window, cx)
        });
        window.render_frame(cx);
        assert!(window.try_find("forecast-dialog").is_none());
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|c| matches!(c, WireCommand::Intent(_) | WireCommand::CommitPrepared(_))));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "keep pending draft");
        assert_eq!(shell.read(cx).block_count(), 0);
    });
}
