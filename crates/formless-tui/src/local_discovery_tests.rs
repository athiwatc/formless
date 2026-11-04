fn local_discovery_env() -> credential_env::EnvGuard {
    let env = credential_env::EnvGuard::isolated();
    for preset in formless_core::presets::PRESETS {
        if let Some(key) = preset.key_env {
            env.remove(key);
        }
    }
    env
}

fn local_discovery_offer(models: usize) -> formless_core::local_servers::LocalServer {
    formless_core::local_servers::LocalServer {
        preset_id: "ollama",
        base_url: "http://[::1]:0/prefix/v1".into(),
        models,
    }
}

#[test]
fn local_discovery_first_paint_does_not_wait_for_the_worker_and_keeps_ipv6_url() {
    let _env = local_discovery_env();
    let mut app = test_app();
    let mut cfg = Config::default();
    cfg.provider.base_url.clear();
    let (release, held) = std::sync::mpsc::channel();
    let started = Instant::now();
    let w = Wizard::start_with_detector(&cfg, &app.paths, move || {
        held.recv_timeout(Duration::from_secs(5)).unwrap();
        vec![local_discovery_offer(1)]
    });
    assert!(w.local_pending && w.step == WizardStep::PickPreset);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| draw_wizard(f, &w, false)).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "first paint waited for a held discovery worker"
    );
    assert!(!w.status.contains("running on this machine"));
    assert!(!app.paths.config_file().exists());
    app.wizard = Some(w);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.wizard.as_ref().unwrap().local_pending {
        assert!(Instant::now() < deadline);
        app.wizard_poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.wizard.as_ref().unwrap().draft_changed.is_none());
    app.flush_wizard_draft(true);
    assert!(!formless_core::setup_draft::file(&app.paths).exists());
    let w = app.wizard.as_mut().unwrap();
    assert!(w.status.contains("Local model API") && w.status.contains("1 model"));
    w.choose_preset();
    assert_eq!(w.base_url, "http://[::1]:0/prefix/v1");
    assert_eq!(w.credential_endpoint, w.base_url);
    assert_eq!(w.preset.unwrap().id, "ollama");
}

#[test]
fn local_discovery_empty_models_show_pull_guidance_in_the_rendered_offer() {
    let _env = local_discovery_env();
    let mut app = test_app();
    let mut cfg = Config::default();
    cfg.provider.base_url.clear();
    let mut w = Wizard::new(&cfg);
    w.local_pending = true;
    w.job_tx
        .send(WizardJob::Local(
            w.generation,
            vec![local_discovery_offer(0)].into(),
        ))
        .unwrap();
    app.wizard = Some(w);
    app.wizard_poll();
    let w = app.wizard.as_ref().unwrap();
    assert!(w.status.contains("ollama pull MODEL"));
    assert!(!w.status.contains("Press Enter"));
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| draw_wizard(f, w, false)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("ollama pull MODEL"), "{text}");
}

#[test]
fn local_discovery_late_results_cannot_override_user_choice_or_a_later_step() {
    let _env = local_discovery_env();
    for scenario in 0..3 {
        let later_step = scenario == 1;
        let mut app = test_app();
        let mut cfg = Config::default();
        cfg.provider.base_url.clear();
        let mut w = Wizard::new(&cfg);
        w.local_pending = true;
        let generation = w.generation;
        let tx = w.job_tx.clone();
        app.wizard = Some(w);
        if later_step {
            app.wizard.as_mut().unwrap().step = WizardStep::ApiKey;
        } else if scenario == 2 {
            app.wizard.as_mut().unwrap().generation += 1;
        } else {
            app.wizard_step_key(key(KeyCode::Down));
        }
        let expected = app.wizard.as_ref().unwrap().preset_idx;
        let status = app.wizard.as_ref().unwrap().status.clone();
        tx.send(WizardJob::Local(
            generation,
            vec![local_discovery_offer(1)].into(),
        ))
        .unwrap();
        app.wizard_poll();
        assert_eq!(app.wizard.as_ref().unwrap().preset_idx, expected);
        assert_eq!(app.wizard.as_ref().unwrap().status, status);
        if later_step {
            assert_eq!(app.wizard.as_ref().unwrap().step, WizardStep::ApiKey);
        }
    }
}

#[test]
fn local_discovery_usable_endpoint_is_preferred_to_an_empty_one() {
    let _env = local_discovery_env();
    let mut app = test_app();
    let mut cfg = Config::default();
    cfg.provider.base_url.clear();
    let mut w = Wizard::new(&cfg);
    w.local_pending = true;
    let available = formless_core::local_servers::LocalServer {
        preset_id: "lmstudio",
        base_url: "http://127.0.0.1:0/v1".into(),
        models: 2,
    };
    w.job_tx
        .send(WizardJob::Local(
            w.generation,
            vec![local_discovery_offer(0), available.clone()].into(),
        ))
        .unwrap();
    app.wizard = Some(w);
    app.wizard_poll();
    assert_eq!(
        app.wizard.as_ref().unwrap().local_offer.as_ref(),
        Some(&available)
    );
    assert!(app.wizard.as_ref().unwrap().status.contains("2 model"));
}

#[test]
fn local_discovery_remote_ollama_host_guidance_reaches_the_rendered_picker_without_a_draft() {
    let env = local_discovery_env();
    env.set("OLLAMA_HOST", "remote-private.example:11500");
    let mut app = test_app();
    let mut cfg = Config::default();
    cfg.provider.base_url.clear();
    app.wizard = Some(Wizard::start(&cfg, &app.paths));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.wizard.as_ref().unwrap().local_pending {
        assert!(Instant::now() < deadline);
        app.wizard_poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    let w = app.wizard.as_ref().unwrap();
    assert!(
        w.status.contains("OLLAMA_HOST points at a remote host"),
        "{}",
        w.status
    );
    assert!(w.status.contains("manually"));
    assert!(!w.status.contains("remote-private.example"));
    assert!(w
        .local_offer
        .as_ref()
        .is_none_or(|s| s.preset_id != "ollama"));
    assert!(w.draft_changed.is_none());
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| draw_wizard(f, w, false)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("OLLAMA_HOST"), "{text}");
    app.flush_wizard_draft(true);
    assert!(!formless_core::setup_draft::file(&app.paths).exists());
    assert!(!app.paths.config_file().exists());
}

#[test]
fn local_discovery_guidance_does_not_override_a_newer_user_choice() {
    let _env = local_discovery_env();
    let mut app = test_app();
    let mut cfg = Config::default();
    cfg.provider.base_url.clear();
    let mut w = Wizard::new(&cfg);
    w.local_pending = true;
    let tx = w.job_tx.clone();
    let generation = w.generation;
    app.wizard = Some(w);
    app.wizard_step_key(key(KeyCode::Down));
    let status = app.wizard.as_ref().unwrap().status.clone();
    tx.send(WizardJob::Local(
        generation,
        formless_core::local_servers::Discovery {
            servers: Vec::new(),
            hint: Some("stale OLLAMA_HOST guidance".into()),
        },
    ))
    .unwrap();
    app.wizard_poll();
    assert_eq!(app.wizard.as_ref().unwrap().status, status);
}
