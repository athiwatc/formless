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

fn status_app() -> App {
    let mut app = crate::tests::test_app();
    app.paths.profile = "default".into();
    app.sessions.push(SessionInfo {
        forked_from: None,
        fork_point_seq: None,
        id: "s1".into(),
        name: "sample".into(),
        model: None,
        reasoning_effort: None,
        created_at: String::new(),
        updated_at: String::new(),
        cwd: None,
        message_count: 0,
        preview: None,
        last_run_status: None,
        archived: false,
        tags: Vec::new(),
        pinned: false,
        instructions: None,
    });
    app.context_fill.insert("s1".into(), (91, 100));
    app
}

#[test]
fn narrow_status_shows_context_beside_activity_with_long_identity() {
    let mut app = status_app();
    app.sessions[0].name = "界e\u{301}".repeat(40);
    app.model_label = "long-model-name".repeat(40);
    let text = screen(
        |frame| draw_status(frame, Rect::new(0, 0, 30, 1), &app),
        30,
        1,
    );
    assert!(text.contains("● ready"), "{text}");
    assert!(text.contains("ctx 91%"), "{text}");
}

#[test]
fn narrow_status_reserves_context_using_rendered_grapheme_widths() {
    let mut app = status_app();
    app.color = false;
    app.sessions[0].name = "لا".repeat(15);
    app.model_label.clear();
    let context = ui::status_text(false, SemanticTone::Bad, "ctx 91%");
    let nominal = format!(" ● ready │ {} │ {context}", app.sessions[0].name);
    let width = u16::try_from(text::width(&nominal)).unwrap();
    let text = screen(
        |frame| draw_status(frame, Rect::new(0, 0, width, 1), &app),
        width,
        1,
    );
    assert!(text.contains("● ready"), "{text}");
    assert!(text.contains(&context), "{text}");
    assert!(text.contains('…'), "{text}");
}

#[test]
fn ordinary_status_truncates_session_and_model_on_complete_graphemes() {
    let mut app = status_app();
    app.color = false;
    let long = "界e\u{301}".repeat(40);
    for (name, model) in [(&*long, "test-model"), ("sample", &*long)] {
        app.sessions[0].name = name.into();
        app.model_label = model.into();
        let spans = status_left(&app, 45);
        let field = spans
            .iter()
            .find(|span| span.content.ends_with('…'))
            .expect("long identity should be clipped");
        let prefix = field.content.strip_suffix('…').unwrap();
        assert!(long.starts_with(prefix));
        assert!(prefix.starts_with("界e\u{301}"));
        assert!(!prefix.ends_with('e'), "split grapheme: {prefix:?}");
        assert!(spans.iter().map(status_width).sum::<usize>() <= 45);
    }
}

#[test]
fn ordinary_status_fits_tiny_rows_and_keeps_context_warning_style() {
    let mut app = status_app();
    app.color = false;
    for (pct, tone, style) in [
        (69, None, ui::dim(false)),
        (70, Some(SemanticTone::Warn), fg(false, Color::Yellow)),
        (89, Some(SemanticTone::Warn), fg(false, Color::Yellow)),
        (90, Some(SemanticTone::Bad), bold_fg(false, Color::Red)),
    ] {
        app.context_fill.insert("s1".into(), (pct, 100));
        let label = format!("ctx {pct}%");
        let content = tone.map_or_else(
            || label.clone(),
            |tone| ui::status_text(false, tone, &label),
        );
        let context = Span::styled(content, style);
        for width in 0..=25 {
            let spans = status_left(&app, width);
            assert!(
                spans.iter().map(status_width).sum::<usize>() <= width,
                "width={width}"
            );
            if width >= context.width() {
                assert!(spans.contains(&context), "width={width}, pct={pct}");
            }
            if width >= context.width() + 2 {
                assert!(
                    spans.iter().any(|span| span.content.starts_with('●')),
                    "width={width}"
                );
            }
        }
    }
}

#[test]
fn wide_status_keeps_field_order_and_hint_selection() {
    let mut app = status_app();
    app.color = false;
    app.paths.profile = "scratch".into();
    app.sessions[0].cwd = Some("/tmp/project".into());
    let status = format!(
        " profile: scratch │ ● ready │ sample {} │ test-model │ {}",
        short_dir("/tmp/project"),
        ui::status_text(false, SemanticTone::Bad, "ctx 91%")
    );
    let width: u16 = 150;
    let text = screen(
        |frame| draw_status(frame, Rect::new(0, 0, width, 1), &app),
        width,
        1,
    );
    assert!(text.starts_with(&status), "{text}");
    let hints = keys::line(app.view, usize::from(width) - text::width(&status) - 3);
    assert!(!hints.is_empty());
    assert!(text.trim_end().ends_with(&hints), "{text}");
}

#[test]
fn pending_approval_status_keeps_its_existing_fields() {
    let mut app = status_app();
    app.on_event(Event::PromotionApprovalRequested {
        promotion_id: "status-promotion".into(),
        tool_name: "fixture".into(),
        reason: "fixture".into(),
        capabilities: vec![],
        memory_mb: 64,
    });
    let wide = status_left(&app, 150);
    assert!(wide
        .iter()
        .any(|span| span.content == "⚠ 1 awaiting approval"));
    assert_eq!(status_left(&app, 15), wide);
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
    let text = screen(
        |frame| draw_settings_form(frame, &form, false, true),
        120,
        30,
    );
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
