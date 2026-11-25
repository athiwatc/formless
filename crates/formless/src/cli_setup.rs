use anyhow::{bail, Context, Result};
use formless_core::{config, presets, Config, Paths};
use std::io::{self, BufRead, IsTerminal, Write};

pub(super) fn run(paths: &Paths) -> Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stderr = io::stderr();
    let mut output = stderr.lock();
    run_with(
        paths,
        &mut input,
        &mut output,
        read_hidden_line,
        formless_core::provider::list_models_blocking,
        formless_core::provider::probe_blocking,
    )
}

fn run_with<R, W, H, L, P>(
    paths: &Paths,
    input: &mut R,
    output: &mut W,
    mut read_secret: H,
    mut list_models: L,
    mut probe: P,
) -> Result<()>
where
    R: BufRead,
    W: Write,
    H: FnMut(&mut R, &mut W, &str) -> Result<String>,
    L: FnMut(&formless_core::config::ProviderCfg, &str) -> Result<Vec<String>>,
    P: FnMut(
        &formless_core::config::ProviderCfg,
        &str,
        &str,
    ) -> Result<formless_core::provider::ProbeResult>,
{
    let mut cfg = Config::load_file_only(paths)?;
    if cfg.provider.is_configured() {
        bail!(
            "profile '{}' already has a configured provider; use its settings or the TUI setup wizard",
            paths.profile
        );
    }

    writeln!(output, "First-time setup for profile '{}'.", paths.profile)?;
    writeln!(output, "Choose a provider preset:")?;
    for (index, preset) in presets::PRESETS.iter().enumerate() {
        writeln!(output, "  {}. {} ({})", index + 1, preset.name, preset.id)?;
    }
    let choice = prompt_line(input, output, "Provider number or ID")?;
    let preset = select_preset(&choice)?;

    let (api_key, save_key) = if preset.local {
        (String::new(), None)
    } else {
        let env_hint = preset.key_env.unwrap_or("FORMLESS_API_KEY");
        let entered = read_secret(
            input,
            output,
            &format!(
                "API key for {} (hidden; Enter uses ${env_hint} or $FORMLESS_API_KEY if set)",
                preset.name
            ),
        )?;
        let entered = entered.trim().to_owned();
        if entered.chars().any(char::is_whitespace) {
            bail!("API key must not contain internal whitespace");
        }
        let runtime_override = runtime_api_key_override();
        let key = if !entered.is_empty() {
            if let Some((source, override_key)) = &runtime_override {
                if override_key != &entered {
                    writeln!(
                        output,
                        "Note: {source} is set in your environment and overrides the saved key at runtime until unset."
                    )?;
                }
            }
            (entered.clone(), Some(entered))
        } else if let Some((_, key)) = runtime_override {
            (key, None)
        } else if let Some((_, key)) = presets::env_key(preset) {
            (key.clone(), Some(key))
        } else {
            (String::new(), None)
        };
        if key.0.is_empty() {
            bail!(
                "an API key is required for {}; setup was not saved",
                preset.name
            );
        }
        key
    };

    cfg.provider.base_url = preset.base_url.to_owned();
    cfg.provider.preset = Some(preset.id.to_owned());
    cfg.provider.api_style = preset.api_style.to_owned();
    cfg.provider.model = preset.default_model.to_owned();

    let models = match list_models(&cfg.provider, &api_key) {
        Ok(models) => models,
        Err(_) => {
            writeln!(output, "Could not list models; enter a model ID manually.")?;
            Vec::new()
        }
    };
    if models.is_empty() {
        writeln!(
            output,
            "No models were listed; type a model ID to continue."
        )?;
    } else {
        writeln!(output, "Available models:")?;
        for (index, model) in models.iter().enumerate() {
            writeln!(output, "  {}. {}", index + 1, crate::table::cell_text(model))?;
        }
    }
    let selection = prompt_line(
        input,
        output,
        "Model number or model ID (Enter for preset default)",
    )?;
    let model = select_model(&selection, &models, &preset.default_model)?;
    cfg.provider.model = model.clone();

    writeln!(output, "Probing {}...", crate::table::cell_text(&model))?;
    let verdict = match probe(&cfg.provider, &api_key, &model) {
        Ok(verdict) if verdict.tool_calling => verdict,
        Ok(_) => {
            writeln!(
                output,
                "Probe verdict: failed (model does not support tool-calling)."
            )?;
            bail!("setup was not saved because the model cannot run agent Tools");
        }
        Err(error) => {
            writeln!(
                output,
                "Probe verdict: failed ({}).",
                crate::table::cell_text(&format!("{error:#}"))
            )?;
            bail!("setup was not saved");
        }
    };
    writeln!(
        output,
        "Probe verdict: passed (streaming: {}; tool-calling: yes{}).",
        if verdict.streaming { "yes" } else { "no" },
        if verdict.prompt_fallback {
            "; prompt fallback"
        } else {
            ""
        }
    )?;
    formless_core::profile_setup::save_verified_provider(
        paths,
        &mut cfg,
        verdict.prompt_fallback,
        save_key.as_deref(),
    )?;
    let open = formless_core::profile_setup::open_command(&paths.profile);
    writeln!(
        output,
        "Setup saved. Start chatting with `{open} chat` or run `{open} run TASK`."
    )?;
    Ok(())
}

fn prompt_line<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &str,
) -> Result<String> {
    write!(output, "{prompt}: ")?;
    output.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        bail!("setup cancelled: no input received");
    }
    Ok(line.trim().to_owned())
}

fn read_hidden_line<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &str,
) -> Result<String> {
    write!(output, "{prompt}: ")?;
    output.flush()?;
    if !io::stdin().is_terminal() {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            bail!("setup cancelled: no input received");
        }
        return Ok(line.trim().to_owned());
    }

    crossterm::terminal::enable_raw_mode().context("hide terminal echo for the API key")?;
    let secret = (|| -> Result<String> {
        let mut secret = String::new();
        loop {
            if let crossterm::event::Event::Key(event) = crossterm::event::read()? {
                use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
                if event.kind != KeyEventKind::Press {
                    continue;
                }
                match event.code {
                    KeyCode::Enter => break,
                    KeyCode::Backspace => {
                        secret.pop();
                    }
                    KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                        bail!("setup cancelled")
                    }
                    KeyCode::Char(character) => secret.push(character),
                    _ => {}
                }
            }
        }
        Ok(secret)
    })();
    let restored = crossterm::terminal::disable_raw_mode();
    writeln!(output)?;
    restored.context("restore terminal after API key entry")?;
    secret
}

fn runtime_api_key_override() -> Option<(&'static str, String)> {
    if let Ok(value) = std::env::var("FORMLESS_API_KEY") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(("FORMLESS_API_KEY", trimmed.to_owned()));
        }
    }
    if let Ok(path) = std::env::var("FORMLESS_API_KEY_FILE") {
        let path = path.trim();
        if !path.is_empty() {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let trimmed = contents.trim();
                if !trimmed.is_empty() {
                    return Some(("FORMLESS_API_KEY_FILE", trimmed.to_owned()));
                }
            }
        }
    }
    None
}

fn select_preset(input: &str) -> Result<&'static presets::Preset> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(&presets::PRESETS[0]);
    }
    if let Ok(index) = input.parse::<usize>() {
        return index
            .checked_sub(1)
            .and_then(|index| presets::PRESETS.get(index))
            .with_context(|| {
                format!(
                    "provider choice must be 1 through {}",
                    presets::PRESETS.len()
                )
            });
    }
    presets::find(input).with_context(|| format!("unknown provider preset '{input}'"))
}

fn select_model(input: &str, models: &[String], default: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        if !default.trim().is_empty() {
            return Ok(default.trim().to_owned());
        }
        bail!("a model ID is required");
    }
    if let Ok(index) = input.parse::<usize>() {
        if let Some(model) = index.checked_sub(1).and_then(|index| models.get(index)) {
            return Ok(model.clone());
        }
    }
    Ok(input.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_core::provider::ProbeResult;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn setup_picks_numbered_preset_and_model_probes_then_saves_without_echoing_key() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::from_dirs(root.path().join("data"), root.path().join("config")).unwrap();
        let mut input = io::Cursor::new(b"1\nprivate-setup-test-key\n2\n".to_vec());
        let mut output = Vec::new();
        let listed = AtomicBool::new(false);
        let probed = AtomicBool::new(false);

        run_with(
            &paths,
            &mut input,
            &mut output,
            prompt_line,
            |provider, key| {
                assert_eq!(provider.base_url, "https://api.openai.com/v1");
                assert_eq!(key, "private-setup-test-key");
                listed.store(true, Ordering::SeqCst);
                Ok(vec!["gpt-5".into(), "custom-model".into()])
            },
            |provider, key, model| {
                assert_eq!(provider.preset.as_deref(), Some("openai"));
                assert_eq!(provider.api_style, "chat");
                assert_eq!(key, "private-setup-test-key");
                assert_eq!(model, "custom-model");
                probed.store(true, Ordering::SeqCst);
                Ok(ProbeResult {
                    models: vec!["gpt-5".into(), "custom-model".into()],
                    streaming: true,
                    tool_calling: true,
                    prompt_fallback: false,
                })
            },
        )
        .unwrap();

        let cfg = Config::load_file_only(&paths).unwrap();
        assert!(listed.load(Ordering::SeqCst));
        assert!(probed.load(Ordering::SeqCst));
        assert_eq!(cfg.provider.preset.as_deref(), Some("openai"));
        assert_eq!(cfg.provider.model, "custom-model");
        assert_eq!(
            config::read_secret_file(&paths.key_fallback_file()).as_deref(),
            Some("private-setup-test-key")
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("1. OpenAI"));
        assert!(output.contains("2. custom-model"));
        assert!(output.contains("Probe verdict: passed"));
        assert!(!output.contains("private-setup-test-key"));
    }

    #[test]
    fn failed_probe_reports_verdict_without_saving_provider_or_key() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::from_dirs(root.path().join("data"), root.path().join("config")).unwrap();
        let mut input = io::Cursor::new(b"1\nprivate-setup-test-key\n1\n".to_vec());
        let mut output = Vec::new();

        let result = run_with(
            &paths,
            &mut input,
            &mut output,
            prompt_line,
            |_, _| Ok(vec!["gpt-5".into()]),
            |_, _, _| Err(anyhow::anyhow!("fixture probe failure")),
        );

        assert!(result.is_err());
        assert!(!paths.config_file().exists());
        assert!(config::read_secret_file(&paths.key_fallback_file()).is_none());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Probe verdict: failed (fixture probe failure)"));
        assert!(!output.contains("private-setup-test-key"));
    }

    #[test]
    fn model_listing_escapes_terminal_control_sequences() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::from_dirs(root.path().join("data"), root.path().join("config"))
            .unwrap()
            .named("setup-control-test")
            .unwrap();
        let mut input = io::Cursor::new(b"1\nprivate-setup-test-key\n1\n".to_vec());
        let mut output = Vec::new();
        let malicious = "spoof\u{1b}[2J\n2. fake\u{202e}";

        let result = run_with(
            &paths,
            &mut input,
            &mut output,
            prompt_line,
            |_, _| Ok(vec![malicious.into()]),
            |_, _, _| Err(anyhow::anyhow!("bad\u{1b}[31merr")),
        );

        assert!(result.is_err());
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains('\u{1b}'), "{output:?}");
        assert!(!output.contains('\u{202e}'), "{output:?}");
        assert!(output.contains("\\u{1b}[2J"), "{output:?}");
        assert!(output.contains("\\u{202e}"), "{output:?}");
    }
}
