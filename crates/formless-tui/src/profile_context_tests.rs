use super::*;

fn screen(mut render: impl FnMut(&mut Frame), width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                + "\n"
        })
        .collect()
}

#[test]
fn wizard_and_status_identify_profile_and_cache_alternate_setup_hints() {
    let root = std::env::temp_dir().join(format!("fl-profile-ui-{}", uuid::Uuid::new_v4()));
    struct Owned(std::path::PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _owned = Owned(root.clone());
    let base = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let work = base.named("work").unwrap();
    let mut saved = Config::default();
    saved.provider.base_url = "http://127.0.0.1:9/v1".into();
    saved.provider.model = "private-profile-model".into();
    saved.save(&work).unwrap();
    let scratch = base.named("scratch").unwrap();
    let wizard = Wizard::new_for_profile(&Config::default(), &scratch);
    // Rendering uses the already captured hint, even if disk state changes.
    std::fs::remove_file(work.config_file()).unwrap();
    for _ in 0..3 {
        let text = screen(|frame| draw_wizard(frame, &wizard, false), 120, 40);
        assert!(text.contains("profile: scratch"), "{text}");
        assert!(text.contains("formless --profile work"), "{text}");
        assert!(text.contains("formless profiles list"), "{text}");
    }
    let mut app = crate::tests::test_app();
    app.paths = scratch;
    let text = screen(
        |frame| draw_status(frame, Rect::new(0, 0, 120, 1), &app),
        120,
        1,
    );
    assert!(text.contains("scratch"), "{text}");
}

#[test]
fn edited_role_shows_preserved_advanced_settings_in_the_actual_form() {
    let role = serde_json::json!({"provider":"default", "model":"auto", "description":"Primary role", "candidates":["fast","precise"], "reasoning_effort":"high", "max_turns":7, "system":"Private system content"});
    let form = settings::Form::role(Some("primary"), Some(&role));
    let text = screen(|frame| draw_settings_form(frame, &form, false), 120, 30);
    for label in [
        "candidates",
        "fast",
        "reasoning_effort",
        "max_turns",
        "system",
        "config.toml",
    ] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    assert!(!text.contains("Private system content"));
}
