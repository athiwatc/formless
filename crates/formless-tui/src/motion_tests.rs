#[cfg(unix)]
#[test]
fn settings_changed_live_motion_updates_cursor_and_spinner() {
    const CHILD: &str = "FORMLESS_MOTION_TEST_CHILD";
    let mode = std::env::var(CHILD).ok();
    if !matches!(mode.as_deref(), Some("normal" | "forced")) {
        // Isolate environment overrides and global theme/i18n updates from
        // other tests; no process-wide environment mutation in the parent.
        for mode in ["normal", "forced"] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "tests::settings_changed_live_motion_updates_cursor_and_spinner",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env_remove("REDUCED_MOTION")
                .env_remove("FORMLESS_REDUCED_MOTION");
            if mode == "forced" {
                child.env("FORMLESS_REDUCED_MOTION", "1");
            }
            let output = child.output().unwrap();
            assert!(output.status.success(), "{mode}: {output:?}");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
                "{mode} must execute the live event test: {output:?}"
            );
        }
        return;
    }
    let forced = mode.as_deref() == Some("forced");
    let mut app = test_app();
    let mut config = Config::default();
    let mut cursor = MotionCursor::default();
    let mut bytes = Vec::new();
    let mut expected = Vec::new();
    let mut last = None;
    for setting in [true, false, false, true, false, true] {
        config.tui.animations = setting;
        config.save(&app.paths).unwrap();
        app.on_event(Event::SettingsChanged);
        let moving = setting && !forced;
        assert_eq!(app.animations, moving);
        assert_eq!(app.spinner(), if moving { SPINNER[0] } else { "●" });
        // This is the same effect synchronization called by the running TUI
        // after draining daemon events and before drawing its next frame.
        cursor.sync(&mut bytes, app.animations).unwrap();
        if last != Some(moving) {
            expected.extend_from_slice(if moving { b"\x1b[0 q" } else { b"\x1b[6 q" });
            last = Some(moving);
        }
        assert_eq!(bytes, expected);
    }
    let _ = std::fs::remove_dir_all(app.paths.config_dir.parent().unwrap());
}

#[cfg(unix)]
#[test]
fn motion_cursor_retries_failed_writes_without_repeating_unchanged_frames() {
    struct Refuse;
    impl Write for Refuse {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut cursor = MotionCursor::default();
    assert!(cursor.sync(&mut Refuse, false).is_err());
    let mut bytes = Vec::new();
    cursor.sync(&mut bytes, false).unwrap();
    assert_eq!(bytes, b"\x1b[6 q");
    cursor.sync(&mut bytes, false).unwrap();
    assert_eq!(bytes, b"\x1b[6 q");
    cursor.sync(&mut bytes, true).unwrap();
    assert_eq!(bytes, b"\x1b[6 q\x1b[0 q");
}
