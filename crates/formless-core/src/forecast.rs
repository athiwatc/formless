//! Read-only local forecasts. No provider factory, credentials or HTTP requests.
use crate::{config::ProviderCfg, provider::Provider, storage::Store, Config, Paths};
use anyhow::{Context, Result};
use formless_protocol::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn hash(value: &impl serde::Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).unwrap_or_default())
    )
}

/// Shared with live Session snapshots; callers decide whether to persist it.
pub fn effective_prompt(st: &Store, cfg: &Config) -> Result<String> {
    let mut prompt = crate::provenance::mandatory_system(
        &st.effective_system_prompt(crate::provider::SYSTEM_PROMPT)?,
    );
    let skills: Vec<_> = st
        .list_skills()?
        .into_iter()
        .filter(|s| s.enabled && !s.retired)
        .map(|s| format!("- {}: {}", s.name, s.description))
        .collect();
    if !skills.is_empty() {
        prompt.push_str("\n\nSkills (load one with load_skill before a task it covers):\n");
        prompt.push_str(&crate::provenance::frame(
            "skill_index",
            &[],
            &skills.join("\n"),
        ));
    }
    if !cfg.roles.is_empty() {
        prompt.push_str("\n\nRoles (pass {role: name} to agents.run or llm.complete from a Tool with the agent/llm Capability):");
        for (name, r) in &cfg.roles {
            prompt.push_str(&format!(
                "\n- {name}: {} [{} · {}]",
                if r.description.is_empty() {
                    "(no description)"
                } else {
                    &r.description
                },
                r.provider
                    .as_deref()
                    .unwrap_or(crate::config::DEFAULT_PROVIDER),
                if r.model.is_empty() {
                    "provider default"
                } else {
                    &r.model
                }
            ));
        }
    }
    prompt.push_str(&crate::memory::prompt_block(&st.list_memories()?));
    Ok(prompt)
}

pub fn fresh_prompt(
    st: &Store,
    cfg: &Config,
    session: Option<&SessionInfo>,
    model: &str,
    cwd: Option<&str>,
) -> Result<String> {
    let mut prompt = effective_prompt(st, cfg)?;
    if let Some(cwd) = cwd {
        prompt.push_str(&format!("\n\nWorking directory: {cwd}. Paths the user gives without a leading / are relative to it; pass absolute paths to Tools."));
    }
    if let Some(extra) = cfg
        .provider
        .tool_use_nudge
        .then(|| crate::provider::model_family_addendum(model))
        .flatten()
    {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    if let Some(instructions) = session.and_then(|s| s.instructions.as_deref()) {
        prompt.push_str(&format!(
            "\n\nStanding instructions for this Session, from the user:\n{instructions}"
        ));
    }
    Ok(prompt)
}

pub fn attachment(name: String, bytes: &[u8]) -> ForecastAttachment {
    let name = crate::daemon::attachment_name(&name);
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    ForecastAttachment {
        mime: crate::daemon::resolve_mime(None, &crate::daemon::cap_name(&name, 120), bytes),
        name,
        bytes: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        reference: None,
        directory: false,
        width: dims.map(|d| d.0),
        height: dims.map(|d| d.1),
    }
}

/// A display-only reference, never resolved or read by the forecast RPC.
pub fn reference(path: &std::path::Path, directory: bool, bytes: u64) -> ForecastAttachment {
    let value = path.to_string_lossy().into_owned();
    ForecastAttachment {
        name: crate::daemon::attachment_name(
            &path.file_name().unwrap_or_default().to_string_lossy(),
        ),
        bytes,
        sha256: hash(&(json!(value), json!(directory), json!(bytes))),
        reference: Some(value),
        directory,
        ..Default::default()
    }
}

/// A tiny local image header carrying dimensions, only for estimator input.
/// Pixels are never decoded or dispatched: the shared counter reads dimensions.
fn dimension_image(dims: Option<(u32, u32)>) -> String {
    use base64::Engine;
    let Some((w, h)) = dims else {
        return "data:image/png;base64,".into();
    };
    let mut png=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=").unwrap();
    png[16..20].copy_from_slice(&w.to_be_bytes());
    png[20..24].copy_from_slice(&h.to_be_bytes());
    let mut crc = 0xffff_ffffu32;
    for b in &png[12..29] {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    png[29..33].copy_from_slice(&(!crc).to_be_bytes());
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

pub fn estimate(paths: &Paths, input: &ForecastInput) -> Result<RunForecast> {
    let st = match Store::open_forecast_snapshot(paths)? {
        Some(snapshot) => snapshot,
        None => Store::open_in_memory()?,
    };
    // The private snapshot retains its configuration namespace freeze while quoting.
    let cfg = Config::load(paths)?; // Configuration only, never provider_key/auth.
    estimate_store(&st, &cfg, input)
}

fn range(mut values: Vec<f64>) -> Option<ForecastRange> {
    values.retain(|v| v.is_finite() && *v >= 0.0);
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let q = |p: f64| {
        values[((values.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)]
    };
    Some(ForecastRange {
        p50: q(0.5),
        p90: q(0.9),
    })
}

pub fn estimate_store(st: &Store, cfg: &Config, input: &ForecastInput) -> Result<RunForecast> {
    estimate_store_with_window(st, cfg, input, None, None)
}

pub(crate) fn estimate_store_with_window(
    st: &Store,
    cfg: &Config,
    input: &ForecastInput,
    cached: Option<&dyn Fn(&str, &ProviderCfg, &str) -> Option<u32>>,
    selected: Option<&str>,
) -> Result<RunForecast> {
    if usize::from(input.edit_message_id.is_some())
        + usize::from(input.resume)
        + usize::from(input.retry)
        > 1
    {
        anyhow::bail!("choose one forecast operation");
    }
    if input.edit_message_id.is_some() || input.resume || input.retry {
        let session = input
            .session_id
            .as_deref()
            .context("rerun forecast requires a Session")?;
        let copy = st.preview_copy()?;
        if let Some(edit) = &input.edit_message_id {
            copy.edit_message(session, edit, &input.draft)?
                .context("user message not found")?;
        } else if input.retry {
            let user = copy
                .list_messages(session)?
                .into_iter()
                .rev()
                .find(|m| m.role == "user")
                .context("nothing to retry")?;
            copy.edit_message(session, &user.id, &user.content)?
                .context("user message not found")?;
        } else {
            let (id, status) = copy
                .last_run(session)?
                .filter(|(_, s)| s == "paused" || s == "interrupted")
                .context("nothing to resume")?;
            copy.set_run_status(&id, "resumed")?;
            copy.append_message(session, "system-note", &resume_note(&status))?;
        }
        let projected = ForecastInput {
            draft: String::new(),
            edit_message_id: None,
            resume: false,
            retry: false,
            attachments: Vec::new(),
            ..input.clone()
        };
        let mut result = estimate_store_with_window(&copy, cfg, &projected, cached, selected)?;
        result.fingerprint = hash(&(
            result.fingerprint,
            &input.edit_message_id,
            input.resume,
            input.retry,
        ));
        return Ok(result);
    }
    let session = input
        .session_id
        .as_deref()
        .map(|id| st.get_session(id))
        .transpose()?
        .flatten();
    if input.session_id.is_some() && session.is_none() {
        anyhow::bail!("no such Session for forecast");
    }
    let wanted = input
        .model
        .clone()
        .or_else(|| session.as_ref().and_then(|s| s.model.clone()))
        .unwrap_or_else(|| cfg.provider.model.clone());
    let wanted = cfg.provider.resolve_alias(&wanted);
    let selected_role = wanted.strip_prefix("role:");
    let role = selected_role
        .map(|r| format!("role:{r}"))
        .unwrap_or_else(|| "main".into());
    let rcfg = selected_role.map(|r| cfg.role(r)).transpose()?;
    let name = rcfg
        .and_then(|r| r.provider.as_deref())
        .unwrap_or(crate::config::DEFAULT_PROVIDER);
    let pcfg = cfg.provider_cfg(Some(name))?;
    let mut model = pcfg.resolve_alias(rcfg.map_or(wanted.as_str(), |r| {
        if r.model.is_empty() {
            pcfg.model.as_str()
        } else {
            r.model.as_str()
        }
    }));
    if ProviderCfg::is_auto_model(&model) {
        if let Some(r) = rcfg.filter(|r| !r.candidates.is_empty()) {
            let candidates: Vec<_> = r.candidates.iter().map(|m| pcfg.resolve_alias(m)).collect();
            model = crate::routing::select_task_model(
                &candidates,
                &st.task_stats(selected_role.unwrap())?,
                cfg.tools.task_min_samples,
                cfg.tools.task_quality_bar,
                false,
            )
            .context("no model candidates")?;
        } else {
            model = crate::routing::select_model(&pcfg, &st.list_model_stats()?, false)
                .context("no model candidates")?;
        }
    }
    if let Some(selected) = selected {
        model = pcfg.resolve_alias(selected);
    }
    let (window, window_source) = if let Some(window) = pcfg.model_context_tokens.get(&model) {
        (*window, "configured")
    } else if let Some(window) = cached.and_then(|f| f(name, &pcfg, &model)) {
        (window, "cached_endpoint")
    } else {
        (pcfg.context_tokens, "default")
    };
    let cwd = input
        .cwd
        .as_deref()
        .or_else(|| session.as_ref().and_then(|s| s.cwd.as_deref()));
    let defs = crate::tools::preview_tool_definitions(
        st,
        input.session_id.as_deref(),
        cfg.tools.inline_limit,
    )?;
    let prompt = match input
        .session_id
        .as_deref()
        .map(|id| st.session_system_prompt(id))
        .transpose()?
        .flatten()
    {
        Some(p) => p,
        None => fresh_prompt(st, cfg, session.as_ref(), &model, cwd)?,
    };
    let mut ctx = if let Some(id) = input.session_id.as_deref() {
        crate::storage::build_context(st, id, &prompt, window, cfg.compaction.fraction())?.0
    } else {
        vec![json!({"role":"system","content":prompt})]
    };
    let context_hash = hash(&ctx);
    let held = input
        .session_id
        .as_deref()
        .map(|id| st.pending_attachments(id))
        .transpose()?
        .unwrap_or_default();
    let mut logical = Vec::new();
    let mut normalized = held.clone();
    for a in &held {
        let bytes = (a["reference"] != true)
            .then(|| a["path"].as_str().and_then(|p| std::fs::read(p).ok()))
            .flatten();
        let mut descriptor = if let Some(bytes) = bytes {
            attachment(
                a["full_name"]
                    .as_str()
                    .or_else(|| a["name"].as_str())
                    .unwrap_or_default()
                    .into(),
                &bytes,
            )
        } else {
            ForecastAttachment {
                name: a["name"].as_str().unwrap_or_default().into(),
                mime: a["mime"].as_str().unwrap_or_default().into(),
                bytes: a["bytes"].as_u64().unwrap_or(0),
                ..Default::default()
            }
        };
        if let Some(mime) = a["mime"].as_str() {
            descriptor.mime = mime.into();
        }
        if a["reference"] == true {
            descriptor.sha256 = hash(&(a["path"].clone(), a["dir"].clone(), a["bytes"].clone()));
            descriptor.reference = a["path"].as_str().map(str::to_string);
            descriptor.directory = a["dir"] == true;
        }
        logical.push(descriptor);
    }
    logical.extend(input.attachments.clone());
    for a in &mut normalized {
        a["id"] = json!("00000000-0000-0000-0000-000000000000");
    }
    for a in &input.attachments {
        if let Some(path) = &a.reference {
            normalized.push(json!({"reference":true,"name":a.name,"path":path,"dir":a.directory,"bytes":a.bytes}));
        } else {
            normalized.push(json!({"id":"00000000-0000-0000-0000-000000000000","name":crate::daemon::cap_name(&a.name,120),"mime":a.mime,"bytes":a.bytes}));
        }
    }
    if !input.draft.is_empty() {
        let message = MessageInfo {
            id: "forecast".into(),
            session_id: input.session_id.clone().unwrap_or_default(),
            seq: 0,
            role: "user".into(),
            content: input.draft.clone(),
            created_at: String::new(),
            meta: Some(json!({"attachments":normalized}).to_string()),
        };
        ctx.push(crate::storage::provider_message(&message));
    }
    // A keyless Provider only formats local context; this engine never calls a
    // provider factory or sends through its explicitly proxy-free HTTP client.
    let provider = Provider::with_key(
        pcfg.clone(),
        String::new(),
        reqwest::Client::builder().no_proxy().build()?,
    )
    .with_name(name);
    let key = crate::tokens::provider_key(name, &pcfg);
    let ratio = st.token_ratio(&key, &model)?;
    let mut ctx = provider.prepare_attachments(&ctx, &model);
    if !input.draft.is_empty() && provider.sends_images(&model) {
        if let Some(message) = ctx.last_mut() {
            let mut parts = message["content"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![json!({"type":"text","text":message["content"]})]);
            for a in input
                .attachments
                .iter()
                .filter(|a| a.reference.is_none() && crate::daemon::is_raster_image(&a.mime))
            {
                parts.push(json!({"type":"image_url","image_url":{"url":dimension_image(a.width.zip(a.height).map(|(w,h)|crate::provider::resized_image_dimensions(w,h,pcfg.image_max_px))),"detail":pcfg.image_detail}}));
            }
            message["content"] = json!(parts);
        }
    }
    let prompt_tokens =
        crate::tokens::calibrated(provider.prompt_tokens(&ctx, &defs, &model), ratio);
    let basis = ForecastBasis {
        provider_key: key,
        model: model.clone(),
        role: role.clone(),
        reasoning_effort: if let Some(r) = rcfg {
            r.reasoning_effort.clone().or(pcfg.reasoning_effort.clone())
        } else {
            session
                .as_ref()
                .and_then(|s| s.reasoning_effort.clone())
                .or(pcfg.reasoning_effort.clone())
        },
        session_id: input.session_id.clone(),
        cwd_hash: hash(&cwd),
        tools_hash: hash(&defs),
        prompt_tokens,
    };
    let recorded = st.recorded_forecasts()?;
    let runs = st.run_metrics(2000)?;
    let mut steps = Vec::new();
    let mut tokens = Vec::new();
    let mut costs = Vec::new();
    let mut durations = Vec::new();
    let mut caches = Vec::new();
    let mut crossings = 0;
    let mut comparables = Vec::new();
    let threshold = window as f64 * cfg.compaction.fraction();
    // Bound quote size and preview work while retaining the exact sample ids.
    // recorded_forecasts is newest-first; N always names this selected sample.
    for (id, old) in &recorded {
        let b = &old.basis;
        let same_location = b.session_id.is_some() && b.session_id == basis.session_id
            || (basis.cwd_hash != hash(&Option::<String>::None) && b.cwd_hash == basis.cwd_hash);
        let scale = prompt_tokens as f64 / b.prompt_tokens.max(1) as f64;
        if b.provider_key != basis.provider_key
            || b.model != model
            || b.role != role
            || b.reasoning_effort != basis.reasoning_effort
            || b.tools_hash != basis.tools_hash
            || !same_location
            || !(0.5..=2.0).contains(&scale)
        {
            continue;
        }
        let Some(run) = runs
            .iter()
            .find(|r| &r.run_id == id && r.status == "completed")
        else {
            continue;
        };
        let served = st.forecast_models(id)?;
        if served.is_empty()
            || served
                .iter()
                .any(|(provider, model)| provider != name || model != &basis.model)
        {
            continue;
        }
        let (cost, peak, cached) = st.forecast_usage(id, cfg)?;
        if peak == 0 || !st.forecast_usage_complete(id)? {
            continue;
        }
        comparables.push(ForecastComparable {
            run_id: id.clone(),
            prompt_tokens: b.prompt_tokens,
            steps: run.steps,
            tokens: run.prompt_tokens + run.completion_tokens,
            duration_ms: run.duration_ms,
            cost_usd: cost,
            cached_fraction: (run.prompt_tokens > 0)
                .then_some(cached as f64 / run.prompt_tokens.max(1) as f64),
            peak_prompt_tokens: peak,
        });
        steps.push(run.steps as f64);
        tokens.push((run.prompt_tokens + run.completion_tokens) as f64 * scale);
        durations.push(run.duration_ms as f64);
        if let Some(c) = cost {
            costs.push(c * scale);
        }
        if peak as f64 * scale > threshold {
            crossings += 1;
        }
        if run.prompt_tokens > 0 {
            caches.push(cached as f64 / run.prompt_tokens as f64);
        }
        if comparables.len() == 200 {
            break;
        }
    }
    let n = steps.len();
    let unpriced = pcfg.price_for(&model).is_none();
    let cost_usd = if unpriced || costs.len() != n {
        None
    } else {
        range(costs)
    };
    let will_compact = prompt_tokens as f64 > threshold;
    let confirmation_required = cfg
        .limits
        .forecast_confirm_usd
        .zip(cost_usd.as_ref())
        .is_some_and(|(limit, r)| r.p90 > limit);
    let mut warnings = vec![
        "Local calibrated heuristic; empirical ranges are forecasts, not limits or guarantees"
            .into(),
        "Legacy Runs without frozen inputs excluded; interrupted/unknown outcomes excluded; at most 200 newest comparable observations from the last 2000 recorded Runs".into(),
    ];
    if window_source == "default" {
        warnings.push("Endpoint context window is not cached; compaction risk uses configured/default window and may differ at dispatch".into());
    }
    warnings.push("Crossing the compaction trigger also requires a complete older turn; the flag is not a guarantee".into());
    if !logical.is_empty() {
        warnings.push("Attachment token counts use local dimensions/detail and reference metadata; unsupported media are uncertain".into());
    }
    if n > 0 && n < 10 {
        warnings.push(format!("Small empirical sample (N={n}); p50/p90 are observed quantiles, not statistical confidence bounds"));
    }
    if n == 0 {
        warnings.push(
            "Cache behaviour and future compaction risk unknown without comparable history".into(),
        );
        warnings.push("No comparable history yet: prompt-size-only estimate".into());
    }
    if unpriced {
        warnings.push("Unpriced model: no dollar forecast".into());
    } else if n > 0 && cost_usd.is_none() {
        warnings.push("Incomplete historical prices: no dollar forecast".into());
    }
    if ProviderCfg::is_auto_model(&wanted)
        || rcfg.is_some_and(|r| ProviderCfg::is_auto_model(&r.model))
    {
        warnings.push("Auto-model estimate uses local deterministic selection; routing/exploration/fallback may choose differently".into());
    }
    let mut f = RunForecast {
        fingerprint: String::new(),
        basis,
        comparable_runs: n,
        comparables,
        steps: range(steps),
        tokens: range(tokens),
        cost_usd,
        duration_ms: range(durations),
        context_tokens: window,
        context_source: window_source.into(),
        will_compact,
        compaction_probability: if will_compact {
            Some(1.0)
        } else if n > 0 {
            Some(crossings as f64 / n as f64)
        } else {
            None
        },
        cached_fraction: range(caches),
        unpriced,
        warnings,
        confirmation_required,
        confirmation_id: None,
        hint: String::new(),
    };
    let history_revision: Vec<_> = runs
        .iter()
        .map(|r| {
            (
                &r.run_id,
                &r.status,
                if r.status == "completed" {
                    Some((r.steps, r.prompt_tokens, r.completion_tokens, r.duration_ms))
                } else {
                    None
                },
            )
        })
        .collect();
    f.fingerprint = hash(&(
        context_hash,
        &input.draft,
        session
            .as_ref()
            .map(|s| (&s.model, &s.reasoning_effort, &s.cwd, &s.instructions)),
        &logical,
        &cfg,
        &recorded,
        &history_revision,
        ratio,
        &f,
    ));
    f.hint = format!(
        "{} prompt tokens · {}{}{}",
        prompt_tokens,
        match &f.cost_usd {
            Some(c) => format!("~${:.2}–{:.2} · ", c.p50, c.p90),
            None => String::new(),
        },
        match &f.steps {
            Some(s) => format!("~{:.0} steps · {n} comparable Runs ({model}/{role})", s.p50),
            None => format!("no history yet ({model}/{role})"),
        },
        if will_compact {
            " · will compact"
        } else if unpriced {
            " · unpriced"
        } else {
            ""
        }
    );
    Ok(f)
}

pub fn resume_note(status: &str) -> String {
    format!("The previous run was {status} before it finished. Continue the user's task from where it stopped; do not repeat work already done above.")
}

pub fn describe(f: &RunForecast) -> String {
    let show = |r: &Option<ForecastRange>, unit: &str| {
        r.as_ref()
            .map(|r| format!("p50 {:.2} / p90 {:.2} {unit}", r.p50, r.p90))
            .unwrap_or_else(|| "unknown (no comparable history or price)".into())
    };
    format!("{}\nSteps: {}\nTokens: {}\nCost: {}\nDuration: {}\nCached fraction: {}\nCompaction probability: {}\n{}",f.hint,show(&f.steps,"steps"),show(&f.tokens,"tokens"),show(&f.cost_usd,"USD"),show(&f.duration_ms,"ms"),show(&f.cached_fraction,""),f.compaction_probability.map_or("unknown".into(),|p|format!("{:.0}%",100.0*p)),f.warnings.join("\n"))
}

pub fn metrics(st: &Store, cfg: &Config) -> Result<Value> {
    let runs = st.run_metrics(2000)?;
    let mut rows = Vec::new();
    for (id, f) in st.recorded_forecasts()? {
        let Some(run) = runs.iter().find(|r| r.run_id == id) else {
            continue;
        };
        let (cost, _, _) = st.forecast_usage(&id, cfg)?;
        let complete = st.forecast_usage_complete(&id)?;
        let actual = (run.prompt_tokens + run.completion_tokens) as f64;
        let finished = run.status == "completed" && complete;
        let error = f
            .tokens
            .as_ref()
            .filter(|_| finished)
            .map(|r| actual - r.p50);
        let coverage = f
            .tokens
            .as_ref()
            .filter(|_| finished)
            .map(|r| actual >= r.p50 && actual <= r.p90);
        let comparison = |range: &Option<ForecastRange>, actual: Option<f64>| match (
            range,
            actual.filter(|_| finished),
        ) {
            (Some(r), Some(actual)) => {
                json!({"error":actual-r.p50,"covered_p50_p90":actual>=r.p50 && actual<=r.p90})
            }
            _ => json!({"error":null,"covered_p50_p90":null}),
        };
        let comparisons = json!({"steps":comparison(&f.steps,Some(run.steps as f64)),"tokens":comparison(&f.tokens,complete.then_some(actual)),"cost_usd":comparison(&f.cost_usd,cost.filter(|_|complete)),"duration_ms":comparison(&f.duration_ms,Some(run.duration_ms as f64))});
        rows.push(json!({"run_id":id,"model":f.basis.model,"role":f.basis.role,"status":run.status,"comparable_runs":f.comparable_runs,"forecast":f,"actual":{"steps":run.steps,"tokens":complete.then_some(actual),"cost_usd":cost.filter(|_|complete),"usage_complete":complete,"duration_ms":run.duration_ms},"comparison":comparisons,"token_error":error,"token_covered_p50_p90":coverage,"calibration_eligible":finished}));
    }
    Ok(json!(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        let mut c = Config::default();
        c.provider.model = "model".into();
        c.provider.base_url = "http://127.0.0.1:9".into();
        c.provider
            .model_context_tokens
            .insert("model".into(), 100_000);
        c.provider.prices.insert(
            "model".into(),
            crate::config::ModelPrice {
                input: 10.0,
                output: 20.0,
                ..Default::default()
            },
        );
        c.naming.enabled = false;
        c
    }
    fn completed(st: &Store, cfg: &Config, f: &RunForecast, steps: i64, status: &str) -> String {
        let id = st
            .create_run(f.basis.session_id.as_deref().unwrap())
            .unwrap();
        st.record_forecast(&id, f).unwrap();
        for step in 0..steps {
            st.bump_run_steps(&id).unwrap();
            st.record_forecast_request(&id, "run").unwrap();
            st.record_usage_full(
                f.basis.session_id.as_deref().unwrap(),
                &id,
                "default",
                "model",
                &crate::provider::Usage {
                    prompt_tokens: 100,
                    completion_tokens: if step == 0 { 20 } else { 0 },
                    cached_tokens: 50,
                    reported: 3,
                    ..Default::default()
                },
                "run",
            )
            .unwrap();
        }
        st.set_run_status(&id, status).unwrap();
        assert!(st.forecast_usage(&id, cfg).unwrap().0.is_some());
        id
    }
    #[test]
    fn previews_are_read_only_and_without_history_invent_no_ranges() {
        let st = Store::open_in_memory().unwrap();
        let c = config();
        let sid = st.create_session(None).unwrap().id;
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: "你好 inspect JSON".into(),
            ..Default::default()
        };
        let f = estimate_store(&st, &c, &input).unwrap();
        assert!(f.basis.prompt_tokens > 0);
        assert_eq!(f.comparable_runs, 0);
        assert!(
            f.steps.is_none()
                && f.tokens.is_none()
                && f.cost_usd.is_none()
                && f.duration_ms.is_none()
        );
        assert!(!st.session_has_filler(&sid).unwrap());
        assert!(st.session_system_prompt(&sid).unwrap().is_none());
        assert!(st.list_messages(&sid).unwrap().is_empty());
        assert!(st.run_metrics(10).unwrap().is_empty());
        assert!(st.pending_attachments(&sid).unwrap().is_empty());
        assert_eq!(f, estimate_store(&st, &c, &input).unwrap());
        let legacy = st.create_run(&sid).unwrap();
        st.set_run_status(&legacy, "completed").unwrap();
        assert_eq!(estimate_store(&st, &c, &input).unwrap().comparable_runs, 0);
    }
    #[test]
    fn comparable_quantiles_unknown_prices_outcomes_and_metrics_are_truthful() {
        let st = Store::open_in_memory().unwrap();
        let mut c = config();
        let sid = st.create_session(None).unwrap().id;
        let input = ForecastInput {
            session_id: Some(sid),
            draft: "a task".into(),
            ..Default::default()
        };
        let first = estimate_store(&st, &c, &input).unwrap();
        for steps in [1, 2, 8, 10] {
            completed(&st, &c, &first, steps, "completed");
        }
        completed(&st, &c, &first, 100, "paused");
        let f = estimate_store(&st, &c, &input).unwrap();
        assert_eq!(f.comparable_runs, 4);
        assert_eq!(f.steps.as_ref().unwrap().p50, 2.0);
        assert_eq!(f.steps.as_ref().unwrap().p90, 10.0);
        assert_eq!(f.tokens.as_ref().unwrap().p50, 220.0);
        assert_eq!(f.tokens.as_ref().unwrap().p90, 1020.0);
        assert!(f.cost_usd.as_ref().unwrap().p90 > 0.0);
        assert!(f.cached_fraction.is_some());
        let id = completed(&st, &c, &f, 3, "completed");
        assert!(
            st.record_forecast(&id, &first).is_err(),
            "admission snapshots are immutable"
        );
        let rows = metrics(&st, &c).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["run_id"] == id)
            .unwrap();
        assert_eq!(row["token_error"], 100.0);
        assert_eq!(row["comparison"]["tokens"]["error"], 100.0);
        assert_eq!(row["comparison"]["steps"]["error"], 1.0);
        assert_eq!(row["forecast"]["comparables"].as_array().unwrap().len(), 4);
        assert_eq!(row["token_covered_p50_p90"], true);
        let paused = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "paused")
            .unwrap();
        assert_eq!(paused["token_error"], Value::Null);
        assert_eq!(paused["calibration_eligible"], false);
        c.provider.prices.clear();
        let f = estimate_store(&st, &c, &input).unwrap();
        assert!(f.unpriced && f.cost_usd.is_none() && f.steps.is_some());
    }
    #[test]
    fn unrelated_model_tools_location_and_compaction_are_not_hidden() {
        let st = Store::open_in_memory().unwrap();
        let mut c = config();
        let sid = st.create_session(None).unwrap().id;
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: "task".into(),
            ..Default::default()
        };
        let f = estimate_store(&st, &c, &input).unwrap();
        let mut foreign = f.clone();
        foreign.basis.tools_hash = "other snapshot".into();
        completed(&st, &c, &foreign, 2, "completed");
        assert_eq!(estimate_store(&st, &c, &input).unwrap().comparable_runs, 0);
        completed(&st, &c, &f, 2, "completed");
        let other = st.create_session(None).unwrap().id;
        let other = ForecastInput {
            session_id: Some(other),
            draft: "task".into(),
            ..Default::default()
        };
        assert_eq!(
            estimate_store(&st, &c, &other).unwrap().comparable_runs,
            0,
            "unknown cwd does not make other Sessions comparable"
        );
        c.provider.model_context_tokens.insert("model".into(), 1);
        let f = estimate_store(&st, &c, &input).unwrap();
        assert!(f.will_compact);
        assert_eq!(f.compaction_probability, Some(1.0));
    }
    #[test]
    fn byte_bound_attachment_previews_equal_uploaded_input_and_do_not_read_rpc_paths() {
        let root = std::env::temp_dir().join(format!("fl-forecast-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let st = Store::open_in_memory().unwrap();
        let mut c = config();
        c.provider.vision = Some(true);
        let sid = st.create_session(None).unwrap().id;
        let path = root.join("real.png");
        image::DynamicImage::new_rgb8(512, 512).save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let descriptor = attachment("[real].png".into(), &bytes);
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: "inspect this".into(),
            attachments: vec![descriptor.clone()],
            ..Default::default()
        };
        let preview = estimate_store(&st, &c, &input).unwrap();
        st.add_pending_attachment(&sid,&json!({"id":uuid::Uuid::new_v4().to_string(),"name":descriptor.name,"mime":descriptor.mime,"path":path,"bytes":bytes.len()})).unwrap();
        let uploaded = estimate_store(
            &st,
            &c,
            &ForecastInput {
                attachments: vec![],
                ..input.clone()
            },
        )
        .unwrap();
        assert_eq!(
            preview.fingerprint, uploaded.fingerprint,
            "upload nonce does not change confirmed semantic inputs"
        );
        assert_eq!(preview.basis.prompt_tokens, uploaded.basis.prompt_tokens);
        let forged:ForecastAttachment=serde_json::from_value(json!({"name":"rpc","mime":"image/png","bytes":10,"width":null,"height":null,"path":"/private/not/a/preview/file"})).unwrap();
        assert!(forged.sha256.is_empty());
        assert!(estimate_store(
            &st,
            &c,
            &ForecastInput {
                attachments: vec![forged],
                ..input.clone()
            }
        )
        .is_ok());
        let changed = ForecastInput {
            draft: "changed".into(),
            attachments: vec![],
            ..input
        };
        assert_ne!(
            uploaded.fingerprint,
            estimate_store(&st, &c, &changed).unwrap().fingerprint
        );
        drop(st);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_or_partial_usage_is_unknown_in_metrics_and_never_a_comparable() {
        let st = Store::open_in_memory().unwrap();
        let c = config();
        let sid = st.create_session(None).unwrap().id;
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: "task".into(),
            ..Default::default()
        };
        let f = estimate_store(&st, &c, &input).unwrap();
        for reported in [0, 1, 2] {
            let run = st.create_run(&sid).unwrap();
            st.record_forecast(&run, &f).unwrap();
            st.bump_run_steps(&run).unwrap();
            st.record_forecast_request(&run, "run").unwrap();
            if reported != 0 {
                st.record_usage_full(
                    &sid,
                    &run,
                    "default",
                    "model",
                    &crate::provider::Usage {
                        prompt_tokens: 100,
                        completion_tokens: 20,
                        reported,
                        ..Default::default()
                    },
                    "run",
                )
                .unwrap();
            }
            st.set_run_status(&run, "completed").unwrap();
            let rows = metrics(&st, &c).unwrap();
            let row = rows
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["run_id"] == run)
                .unwrap();
            assert_eq!(row["actual"]["tokens"], Value::Null);
            assert_eq!(row["actual"]["cost_usd"], Value::Null);
            assert_eq!(row["calibration_eligible"], false);
            assert_eq!(row["token_error"], Value::Null);
            assert_eq!(estimate_store(&st, &c, &input).unwrap().comparable_runs, 0);
        }
    }

    #[test]
    fn quote_binds_session_calibration_and_newly_completed_history() {
        let st = Store::open_in_memory().unwrap();
        let c = config();
        let sid = st.create_session(None).unwrap().id;
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: "task".into(),
            ..Default::default()
        };
        let first = estimate_store(&st, &c, &input).unwrap();
        let foreign = ForecastInput {
            session_id: Some(st.create_session(None).unwrap().id),
            ..input.clone()
        };
        assert_ne!(
            first.fingerprint,
            estimate_store(&st, &c, &foreign).unwrap().fingerprint
        );
        st.set_session_model(&sid, None, Some("high".into()))
            .unwrap();
        let effort = estimate_store(&st, &c, &input).unwrap();
        assert_ne!(first.fingerprint, effort.fingerprint);
        assert_eq!(effort.basis.reasoning_effort.as_deref(), Some("high"));
        st.set_session_model(&sid, None, None).unwrap();
        st.observe_prompt_tokens(&first.basis.provider_key, "model", 100, 400)
            .unwrap();
        let calibrated = estimate_store(&st, &c, &input).unwrap();
        assert!(calibrated.basis.prompt_tokens > first.basis.prompt_tokens);
        assert_ne!(first.fingerprint, calibrated.fingerprint);
        let id = completed(&st, &c, &calibrated, 2, "running");
        let running = estimate_store(&st, &c, &input).unwrap();
        st.set_run_status(&id, "completed").unwrap();
        let done = estimate_store(&st, &c, &input).unwrap();
        assert_ne!(running.fingerprint, done.fingerprint);
        assert_eq!(done.comparable_runs, 1);
    }

    #[test]
    fn role_named_main_and_local_cached_windows_match_actual_selection() {
        let st = Store::open_in_memory().unwrap();
        let mut c = config();
        let mut named = c.provider.clone();
        named.base_url = "http://127.0.0.1:8".into();
        named.model = "named-model".into();
        named.model_context_tokens.clear();
        c.providers.insert("named".into(), named.clone());
        c.roles.insert(
            "main".into(),
            crate::config::RoleCfg {
                provider: Some("named".into()),
                model: "named-model".into(),
                ..Default::default()
            },
        );
        let input = ForecastInput {
            model: Some("role:main".into()),
            draft: "task".into(),
            ..Default::default()
        };
        let f = estimate_store_with_window(
            &st,
            &c,
            &input,
            Some(&|name, _, model| {
                assert_eq!((name, model), ("named", "named-model"));
                Some(1)
            }),
            None,
        )
        .unwrap();
        assert_eq!(f.basis.role, "role:main");
        assert_eq!(f.basis.model, "named-model");
        assert_eq!(
            f.basis.provider_key,
            crate::tokens::provider_key("named", &named)
        );
        assert_eq!(f.context_source, "cached_endpoint");
        assert!(f.will_compact);
        let offline = estimate_store(&st, &c, &input).unwrap();
        assert_eq!(offline.context_source, "default");
        assert!(offline.warnings.iter().any(|w| w.contains("window")));
        c.providers
            .get_mut("named")
            .unwrap()
            .model_context_tokens
            .insert("named-model".into(), 100_000);
        let configured = estimate_store_with_window(
            &st,
            &c,
            &input,
            Some(&|_, _, _| panic!("explicit configured window wins")),
            None,
        )
        .unwrap();
        assert_eq!(configured.context_source, "configured");
    }

    #[test]
    fn prospective_resume_edit_and_retry_never_mutate_source_and_are_stable() {
        let st = Store::open_in_memory().unwrap();
        let c = config();
        let sid = st.create_session(None).unwrap().id;
        let user = st.append_message(&sid, "user", "original").unwrap();
        st.append_message(&sid, "assistant", "answer").unwrap();
        let run = st.create_run(&sid).unwrap();
        st.set_run_status(&run, "paused").unwrap();
        let before = st.list_messages(&sid).unwrap();
        for operation in 0..3 {
            let input = ForecastInput {
                session_id: Some(sid.clone()),
                draft: if operation == 0 {
                    "edited".into()
                } else {
                    String::new()
                },
                edit_message_id: (operation == 0).then(|| user.id.clone()),
                retry: operation == 1,
                resume: operation == 2,
                ..Default::default()
            };
            let first = estimate_store(&st, &c, &input).unwrap();
            let second = estimate_store(&st, &c, &input).unwrap();
            assert_eq!(
                first.fingerprint, second.fingerprint,
                "prospective operation {operation}"
            );
            assert_eq!(
                serde_json::to_value(st.list_messages(&sid).unwrap()).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            assert_eq!(st.last_run(&sid).unwrap().unwrap().1, "paused");
        }
    }
    #[test]
    fn attachment_preview_matches_resize_prompt_tools_and_reference_semantics() {
        let root =
            std::env::temp_dir().join(format!("fl-forecast-geometry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("wide.png");
        image::DynamicImage::new_rgb8(4096, 1024)
            .save(&path)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        for prompt_tools in [false, true] {
            let st = Store::open_in_memory().unwrap();
            let mut c = config();
            c.provider.vision = Some(true);
            c.provider.image_max_px = 256;
            if prompt_tools {
                c.provider.tool_mode = "prompt".into();
            }
            let sid = st.create_session(None).unwrap().id;
            let input = ForecastInput {
                session_id: Some(sid.clone()),
                draft: "inspect image".into(),
                attachments: vec![attachment("wide.png".into(), &bytes)],
                ..Default::default()
            };
            let preview = estimate_store(&st, &c, &input).unwrap();
            st.add_pending_attachment(&sid,&json!({"id":uuid::Uuid::new_v4().to_string(),"path":path,"name":"wide.png","mime":"image/png","bytes":bytes.len()})).unwrap();
            let uploaded = estimate_store(
                &st,
                &c,
                &ForecastInput {
                    attachments: vec![],
                    ..input
                },
            )
            .unwrap();
            assert_eq!(
                preview.basis.prompt_tokens, uploaded.basis.prompt_tokens,
                "prompt_tools={prompt_tools}"
            );
            assert_eq!(preview.fingerprint, uploaded.fingerprint);
        }
        let st = Store::open_in_memory().unwrap();
        let c = config();
        let sid = st.create_session(None).unwrap().id;
        // Arbitrary reference metadata cannot cause a local read. It is the
        // exact text that an independently validated AttachPath would supply.
        let absent = root.join("not-a-real-directory");
        let descriptor = reference(&absent, true, 0);
        let input = ForecastInput {
            session_id: Some(sid.clone()),
            draft: "inspect reference".into(),
            attachments: vec![descriptor],
            ..Default::default()
        };
        let preview = estimate_store(&st, &c, &input).unwrap();
        st.add_pending_attachment(&sid,&json!({"reference":true,"name":"not-a-real-directory","path":absent,"dir":true,"bytes":0})).unwrap();
        let uploaded = estimate_store(
            &st,
            &c,
            &ForecastInput {
                attachments: vec![],
                ..input
            },
        )
        .unwrap();
        assert_eq!(preview.fingerprint, uploaded.fingerprint);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn owned_offline_quote_releases_source_freeze_before_any_output() {
        let env = crate::credential_env::EnvGuard::isolated();
        for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
            env.remove(key);
        }
        let root = std::env::temp_dir().join(format!("fl-forecast-owned-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        config().save(&paths).unwrap();
        let st = Store::open(&paths.db_file()).unwrap();
        let sid = st.create_session(None).unwrap().id;
        drop(st);
        let quote = estimate(
            &paths,
            &ForecastInput {
                session_id: Some(sid),
                draft: "task".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let lease = crate::profile_lock::ProfileLock::hierarchy_shared(&paths)
            .expect("owned quote cannot retain source maintenance during output");
        assert!(!serde_json::to_string_pretty(&quote).unwrap().is_empty());
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }
}
