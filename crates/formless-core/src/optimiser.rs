//! Background optimiser (SPEC §8.3–§8.5): trigger scan, Candidate
//! generation, sandboxed replay gate, promotion.

use anyhow::{anyhow, Result};
use formless_protocol::*;
use formless_runtime::{new_overlay, Capabilities, ExecRequest, HostMode, HostOps, Overlay};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::config::Config;
use crate::daemon::Daemon;
use crate::provider::Provider;
use crate::storage::{CandidateData, ReplayItem};
use crate::tools as toolreg;

const PROPOSAL_PROMPT: &str = "You improve the Formless agent's learned guidance. \
Reply with ONLY a JSON object: {\"kind\": \"prompt\" or \"memory\", \"content\": one short imperative guidance or memory note (max 400 chars), \"rationale\": why the signals justify it}. \
Propose exactly one concrete, generalisable lesson from the failure signals. \
No secrets, no user data, no instructions to ignore safety or approvals.";

const OPTIMISER_PROMPT: &str = concat!(
    "You improve a failing JavaScript Tool for the Formless harness. \
Reply with ONLY a JSON object: {\"description\": string, \"input_schema\": object, \"output_schema\": object, \
\"implementation_js\": string defining `function run(input)`, \"capabilities\": string[], \"memory_mb\": integer}. \
Keep capabilities and memory no wider than the current Tool unless the failures prove it necessary. \
If failures are resource/out-of-memory, fix with streaming (fs.readChunks / fetchChunks / emit) first. \
Fix the actual failures without breaking the recorded successes. ",
    crate::provider::host_api_doc!()
);

impl Daemon {
    pub(crate) fn activity(&self, tool_name: &str, text: String) {
        self.broadcast(Event::OptimiserActivity {
            tool_name: tool_name.to_string(),
            text,
        });
    }

    pub(crate) async fn optimiser_loop(self: Arc<Self>) {
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
            }
            let Ok(cfg) = Config::load(&self.paths) else {
                continue;
            };
            if !cfg.provider.is_configured() {
                continue;
            }
            if let Ok(triggered) = self.scan_triggers(&cfg) {
                for id in triggered {
                    if !queue.contains(&id) {
                        queue.push_back(id);
                    }
                }
            }
            // Politeness (§8.3): never run while a Run is active.
            if !self.runs.lock().unwrap().is_empty() {
                continue;
            }
            if let Some(tool_id) = queue.pop_front() {
                self.optimise_tool(&tool_id, &cfg).await;
            }
        }
    }

    fn scan_triggers(&self, cfg: &Config) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for tool in self.with_store(|st| st.list_tools(false))? {
            if tool.needs_attention {
                continue;
            }
            let (sample, rate) =
                self.with_store(|st| st.windowed_success(&tool.id, cfg.metrics.success_window))?;
            if sample >= cfg.metrics.trigger_min_invocations as i64
                && rate
                    .map(|r| r < cfg.metrics.trigger_success_below)
                    .unwrap_or(false)
            {
                out.push(tool.id);
            }
        }
        Ok(out)
    }

    pub(crate) async fn run_optimiser_now(self: Arc<Self>, tool_id: Option<String>) {
        let Ok(cfg) = Config::load(&self.paths) else {
            return;
        };
        if !cfg.provider.is_configured() {
            return;
        }
        match tool_id {
            Some(id) => {
                self.optimise_tool(&id, &cfg).await;
            }
            None => {
                if let Ok(ids) = self.scan_triggers(&cfg) {
                    for id in ids {
                        if !self.runs.lock().unwrap().is_empty() {
                            break;
                        }
                        self.optimise_tool(&id, &cfg).await;
                    }
                }
            }
        }
    }

    pub(crate) async fn generate_and_measure_proposal(self: Arc<Self>) {
        let Ok(cfg) = Config::load(&self.paths) else {
            return;
        };
        if !cfg.provider.is_configured() {
            return;
        }
        self.activity("prompt", "generating prompt/memory proposal...".into());
        if let Err(e) = self.generate_proposal_inner(&cfg).await {
            self.activity("prompt", format!("proposal generation failed: {e}"));
        }
    }

    async fn generate_proposal_inner(self: &Arc<Self>, cfg: &Config) -> Result<()> {
        // Signals: per-Tool metrics + recent recorded failure patterns.
        let mut signals = String::from("Recent signals:\n");
        let mut narrow = Vec::new();
        for tool in self.with_store(|st| st.list_tools(false))? {
            let metrics = self.with_store(|st| st.tool_metrics(&tool.id, None))?;
            if metrics.total == 0 {
                continue;
            }
            signals.push_str(&format!(
                "- {}: calls={} success={}/{:?} failures={} validation={} denied={} sessions={}\n",
                tool.name,
                metrics.total,
                metrics.success,
                metrics.success_rate,
                metrics.failure,
                metrics.validation_failures,
                metrics.denied,
                metrics.sessions
            ));
            if is_narrow(&metrics) {
                narrow.push(tool.name.clone());
            }
            if let Ok(replay) = self.with_store(|st| st.replay_set(&tool.id)) {
                for f in replay.iter().filter(|r| !r.recorded_success).take(4) {
                    signals.push_str(&format!("  failure tag={} error={}\n", f.tag, f.error));
                }
            }
        }
        if !narrow.is_empty() {
            signals.push_str(&format!(
                "\nPossibly over-specific Tools (used repeatedly, but only ever in one session; \
                 likely hardcoded paths/hosts/names instead of input parameters): {}\n",
                narrow.join(", ")
            ));
        }
        if signals.lines().count() <= 1 {
            signals.push_str("(no Tool activity yet; propose a general first-run lesson)\n");
        }
        let applied = self.with_store(|st| st.applied_proposals())?;
        if !applied.is_empty() {
            signals.push_str("\nAlready applied (do not repeat):\n");
            for a in &applied {
                signals.push_str(&format!("- [{}] {}\n", a.kind, a.content));
            }
        }
        let provider = Provider::for_profile(cfg.provider.clone(), self.paths.clone()).await?;
        let model = cfg
            .optimiser
            .model
            .clone()
            .unwrap_or_else(|| cfg.provider.model.clone());
        let model = self.resolve_model(&model, cfg, true)?;
        let effort = cfg
            .optimiser
            .reasoning_effort
            .as_deref()
            .or(cfg.provider.reasoning_effort.as_deref());
        let raw = provider
            .complete(
                &[
                    json!({"role": "system", "content": PROPOSAL_PROMPT}),
                    json!({"role": "user", "content": signals}),
                ],
                &model,
                effort,
            )
            .await?;
        let est_tokens = ((signals.len() + raw.len()) / 4) as i64;
        let (kind, content, rationale) = parse_proposal(&raw)?;
        let proposal =
            self.with_store(|st| st.create_proposal(&kind, &content, &rationale, est_tokens))?;
        self.broadcast(Event::ProposalUpdated {
            proposal: proposal.clone(),
        });
        // Measure: fixed eval suite, baseline vs candidate prompt.
        let baseline_prompt = self.effective_system_prompt();
        let label = if kind == "memory" {
            "Learned memory"
        } else {
            "Learned guidance"
        };
        let candidate_prompt = format!(
            "{baseline_prompt}\n\n{label} (candidate): {}",
            content.trim()
        );
        let (base_score, total) =
            evaluate_prompt(&provider, &baseline_prompt, &model, effort).await?;
        let (cand_score, _) = evaluate_prompt(&provider, &candidate_prompt, &model, effort).await?;
        if let Some(measured) = self
            .with_store(|st| st.update_proposal_eval(&proposal.id, base_score, cand_score, total))?
        {
            self.activity("prompt", format!(
                "proposal measured: candidate {:.0}% vs baseline {:.0}% ({} evals) - awaiting your decision",
                cand_score * 100.0, base_score * 100.0, total
            ));
            self.broadcast(Event::ProposalUpdated { proposal: measured });
        }
        Ok(())
    }

    /// Apply gate: only measured proposals that beat the baseline by the
    /// configured promotion margin (or match a perfect baseline) apply.
    pub(crate) fn apply_proposal_checked(&self, id: &str) -> Result<Option<PromptProposalInfo>> {
        let cfg = Config::load(&self.paths)?;
        let Some(p) = self.with_store(|st| st.get_proposal(id))? else {
            return Ok(None);
        };
        if p.status != "measured" {
            return Err(anyhow!("proposal is not measured yet"));
        }
        let (Some(base), Some(cand)) = (p.baseline_score, p.candidate_score) else {
            return Err(anyhow!("proposal has no eval scores"));
        };
        if !proposal_beats_baseline(base, cand, cfg.optimiser.promotion_margin_points) {
            return Err(anyhow!(
                "candidate score {cand:.2} does not beat baseline {base:.2} by {:.0} points",
                cfg.optimiser.promotion_margin_points
            ));
        }
        self.with_store(|st| st.set_proposal_status(id, "applied"))
    }

    fn guardrails_ok(&self, tool: &ToolInfo, cfg: &Config) -> bool {
        if tool.retired || tool.needs_attention {
            return false;
        }
        let (cooldown_until, _consec) = self
            .with_store(|st| st.get_opt_state(&tool.id))
            .unwrap_or((None, 0));
        if let Some(until) = cooldown_until {
            if until.as_str() > chrono::Utc::now().to_rfc3339().as_str() {
                return false;
            }
        }
        if self
            .with_store(|st| st.attempts_today(&tool.id))
            .unwrap_or(0)
            >= cfg.optimiser.max_attempts_per_day as i64
        {
            return false;
        }
        if let Some(budget) = cfg.optimiser.token_budget {
            if self.with_store(|st| st.attempt_tokens_today()).unwrap_or(0) >= budget as i64 {
                return false;
            }
        }
        true
    }

    fn set_cooldown_now(&self, tool_id: &str, cfg: &Config) {
        let until =
            chrono::Utc::now() + chrono::Duration::seconds(cfg.optimiser.cooldown_secs as i64);
        let _ = self.with_store(|st| st.set_cooldown(tool_id, Some(&until.to_rfc3339())));
    }

    async fn optimise_tool(self: &Arc<Self>, tool_id: &str, cfg: &Config) {
        let Ok(Some((info, ver))) = self.with_store(|st| st.get_tool_current(tool_id)) else {
            return;
        };
        if !self.guardrails_ok(&info, cfg) {
            return;
        }
        self.activity(&info.name, "generating candidate…".into());
        match self.generate_candidate(&info, &ver, cfg).await {
            Ok((cand, est_tokens)) => {
                let attempt_id = match self
                    .with_store(|st| st.create_attempt(tool_id, ver.version, &cand, est_tokens))
                {
                    Ok(id) => id,
                    Err(_) => return,
                };
                // Widening gate (§8.3): never auto-widen capabilities/memory.
                let cur_caps = Capabilities::parse(&ver.capabilities).unwrap_or_default();
                let cand_caps = Capabilities::parse(&cand.capabilities).unwrap_or_default();
                if !cur_caps.covers(&cand_caps) || cand.memory_mb > ver.memory_mb {
                    let _ = self.with_store(|st| {
                        st.update_attempt(
                            &attempt_id,
                            "awaiting_approval",
                            "candidate widens capabilities/memory",
                            None,
                            None,
                            0,
                            0,
                        )
                    });
                    self.request_promotion_approval(&info, &attempt_id, "widening", &cand);
                    return;
                }
                self.replay_and_maybe_promote(attempt_id, cfg).await;
            }
            Err(e) => {
                // Generation failure counts toward give-up (§8.3).
                let empty = CandidateData {
                    code: String::new(),
                    description: String::new(),
                    input_schema: "{}".into(),
                    output_schema: "{}".into(),
                    capabilities: Vec::new(),
                    memory_mb: ver.memory_mb,
                };
                if let Ok(attempt_id) =
                    self.with_store(|st| st.create_attempt(tool_id, ver.version, &empty, 0))
                {
                    self.fail_attempt(
                        &info,
                        &attempt_id,
                        "failed",
                        &format!("generation failed: {e}"),
                        None,
                        None,
                        0,
                        0,
                        cfg,
                    );
                }
            }
        }
    }

    async fn generate_candidate(
        &self,
        info: &ToolInfo,
        ver: &ToolVersionInfo,
        cfg: &Config,
    ) -> Result<(CandidateData, i64)> {
        let replay = self.with_store(|st| st.replay_set(&info.id))?;
        let failures: Vec<&ReplayItem> = replay
            .iter()
            .filter(|r| !r.recorded_success)
            .take(10)
            .collect();
        let successes: Vec<&ReplayItem> = replay
            .iter()
            .filter(|r| r.recorded_success)
            .take(5)
            .collect();
        let resource = replay.iter().any(|r| r.tag == "resource");
        let metrics = self.with_store(|st| st.tool_metrics(&info.id, None))?;
        let mut prompt = format!(
            "Tool `{}` v{} - {}\nCurrent metrics: success_rate={:?} validation_failures={} retry_rate={:?}\nCapabilities: {:?} memory_mb={}\nInput schema: {}\nOutput schema: {}\nCurrent code:\n{}\n\nRecorded failures (redacted):\n",
            info.name, ver.version, info.description, metrics.success_rate, metrics.validation_failures, metrics.retry_rate,
            ver.capabilities, ver.memory_mb, ver.input_schema, ver.output_schema, ver.code
        );
        for f in &failures {
            prompt.push_str(&format!("- input={} error={}\n", f.input, f.error));
        }
        prompt.push_str("\nRecorded successes (must keep working):\n");
        for s in &successes {
            prompt.push_str(&format!("- input={}\n", s.input));
        }
        if resource {
            prompt.push_str(
                "\nNOTE: resource (OOM) failures present - use streaming/chunking first.\n",
            );
        }
        let provider = Provider::for_profile(cfg.provider.clone(), self.paths.clone()).await?;
        let model = cfg
            .optimiser
            .model
            .clone()
            .unwrap_or_else(|| cfg.provider.model.clone());
        let model = self.resolve_model(&model, cfg, true)?;
        let effort = cfg
            .optimiser
            .reasoning_effort
            .as_deref()
            .or(cfg.provider.reasoning_effort.as_deref());
        let raw = provider
            .complete(
                &[
                    json!({"role": "system", "content": OPTIMISER_PROMPT}),
                    json!({"role": "user", "content": prompt}),
                ],
                &model,
                effort,
            )
            .await?;
        let est_tokens = ((prompt.len() + raw.len()) / 4) as i64;
        let cand = parse_candidate(&raw, ver, cfg)?;
        Ok((cand, est_tokens))
    }

    fn request_promotion_approval(
        &self,
        info: &ToolInfo,
        attempt_id: &str,
        kind: &str,
        cand: &CandidateData,
    ) {
        let promotion_id = uuid::Uuid::new_v4().to_string();
        self.promotions.lock().unwrap().insert(
            promotion_id.clone(),
            (attempt_id.to_string(), kind.to_string()),
        );
        self.activity(&info.name, format!("awaiting promotion approval ({kind})"));
        self.broadcast(Event::PromotionApprovalRequested {
            promotion_id,
            tool_name: info.name.clone(),
            reason: kind.to_string(),
            capabilities: cand.capabilities.clone(),
            memory_mb: cand.memory_mb,
        });
    }

    pub(crate) async fn resume_promotion(
        self: Arc<Self>,
        attempt_id: String,
        kind: String,
        approved: bool,
    ) {
        let Ok(cfg) = Config::load(&self.paths) else {
            return;
        };
        let Ok(Some(attempt)) = self.with_store(|st| st.get_attempt(&attempt_id)) else {
            return;
        };
        let Ok(Some((info, _ver))) = self.with_store(|st| st.get_tool_current(&attempt.tool_id))
        else {
            return;
        };
        if !approved {
            self.fail_attempt(
                &info,
                &attempt_id,
                "discarded",
                &format!("user denied promotion approval ({kind})"),
                None,
                None,
                0,
                0,
                &cfg,
            );
            return;
        }
        match kind.as_str() {
            "widening" => self.replay_and_maybe_promote(attempt_id, &cfg).await,
            "replay_limited" => self.promote_attempt(&info, &attempt_id, &cfg),
            _ => {}
        }
    }

    async fn replay_and_maybe_promote(self: &Arc<Self>, attempt_id: String, cfg: &Config) {
        let Ok(Some(attempt)) = self.with_store(|st| st.get_attempt(&attempt_id)) else {
            return;
        };
        let Ok(Some((info, ver))) = self.with_store(|st| st.get_tool_current(&attempt.tool_id))
        else {
            return;
        };
        if info.current_version != attempt.base_version {
            self.fail_attempt(
                &info,
                &attempt_id,
                "discarded",
                "stale: tool changed since candidate generation",
                None,
                None,
                0,
                0,
                cfg,
            );
            return;
        }
        let cand = &attempt.candidate;
        let replay = match self.with_store(|st| st.replay_set(&info.id)) {
            Ok(r) if !r.is_empty() => r,
            _ => {
                self.fail_attempt(
                    &info,
                    &attempt_id,
                    "discarded",
                    "no replay data",
                    None,
                    None,
                    0,
                    0,
                    cfg,
                );
                return;
            }
        };
        self.activity(
            &info.name,
            format!("replaying {} recorded invocations…", replay.len()),
        );
        let cand_caps = Capabilities::parse(&cand.capabilities).unwrap_or_default();
        let cur_caps = Capabilities::parse(&ver.capabilities).unwrap_or_default();
        let total = replay.len() as i64;
        let mut cand_pass = 0i64;
        let mut base_pass = 0i64;
        let mut regressions = 0i64;
        for item in &replay {
            let input: Value = serde_json::from_str(&item.input).unwrap_or(json!({}));
            let cand_ok = self
                .replay_one(
                    &info.name,
                    &cand.code,
                    &cand_caps,
                    cand.memory_mb,
                    &cand.output_schema,
                    input.clone(),
                    Some(cand),
                    0,
                    new_overlay(),
                )
                .await;
            let base_ok = self
                .replay_one(
                    &info.name,
                    &ver.code,
                    &cur_caps,
                    ver.memory_mb,
                    &ver.output_schema,
                    input,
                    None,
                    0,
                    new_overlay(),
                )
                .await;
            if cand_ok {
                cand_pass += 1;
            } else if item.recorded_success {
                regressions += 1;
            }
            if base_ok {
                base_pass += 1;
            }
        }
        let cand_score = cand_pass as f64 / total as f64;
        let base_score = base_pass as f64 / total as f64;
        // Exact integer form of `cand_score - base_score >= margin` so a
        // win of exactly the margin is not lost to float rounding.
        let wins_by_margin =
            (cand_pass - base_pass) * 100 >= cfg.optimiser.promotion_margin_points as i64 * total;
        let detail = format!(
            "candidate {cand_pass}/{total} baseline {base_pass}/{total} regressions {regressions}"
        );
        if wins_by_margin && regressions == 0 {
            if cand_caps.net.is_some() {
                // Replay-limited (§8.4): net tools can only promote by hand.
                let _ = self.with_store(|st| {
                    st.update_attempt(
                        &attempt_id,
                        "awaiting_approval",
                        &detail,
                        Some(cand_score),
                        Some(base_score),
                        total,
                        regressions,
                    )
                });
                self.request_promotion_approval(&info, &attempt_id, "replay_limited", cand);
                return;
            }
            let _ = self.with_store(|st| {
                st.update_attempt(
                    &attempt_id,
                    "promoted",
                    &detail,
                    Some(cand_score),
                    Some(base_score),
                    total,
                    regressions,
                )
            });
            self.promote_attempt_with(&info, &attempt_id, cand, cfg);
        } else {
            self.fail_attempt(
                &info,
                &attempt_id,
                "discarded",
                &detail,
                Some(cand_score),
                Some(base_score),
                total,
                regressions,
                cfg,
            );
        }
    }

    fn promote_attempt(self: &Arc<Self>, info: &ToolInfo, attempt_id: &str, cfg: &Config) {
        let Ok(Some(attempt)) = self.with_store(|st| st.get_attempt(attempt_id)) else {
            return;
        };
        // Replay already passed; keep its scores, just flip the status.
        let cand = attempt.candidate.clone();
        let attempts = self
            .with_store(|st| st.list_attempts(&info.id, 50))
            .unwrap_or_default();
        if let Some(a) = attempts.iter().find(|a| a.id == attempt_id) {
            let _ = self.with_store(|st| {
                st.update_attempt(
                    attempt_id,
                    "promoted",
                    &a.detail,
                    a.candidate_score,
                    a.baseline_score,
                    a.replay_total,
                    a.regressions,
                )
            });
        }
        self.promote_attempt_with(info, attempt_id, &cand, cfg);
    }

    fn promote_attempt_with(
        self: &Arc<Self>,
        info: &ToolInfo,
        _attempt_id: &str,
        cand: &CandidateData,
        cfg: &Config,
    ) {
        let caps = Capabilities::parse(&cand.capabilities).unwrap_or_default();
        match self.with_store(|st| {
            st.create_tool_version(
                &info.name,
                &cand.description,
                &cand.input_schema,
                &cand.output_schema,
                &cand.code,
                &caps,
                cand.memory_mb,
                "optimiser",
            )
        }) {
            Ok((new_info, version)) => {
                let _ = self.with_store(|st| {
                    let _ = st.set_consecutive_failures(&info.id, 0);
                    let _ = st.set_needs_attention(&info.id, false);
                    st.audit(
                        "optimiser",
                        "tool.promote",
                        &format!("{} v{version}", info.name),
                    )
                });
                self.set_cooldown_now(&info.id, cfg);
                self.activity(&info.name, format!("promoted candidate as v{version}"));
                self.broadcast(Event::ToolUpdated { tool: new_info });
            }
            Err(e) => self.activity(&info.name, format!("promotion failed: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn fail_attempt(
        &self,
        info: &ToolInfo,
        attempt_id: &str,
        status: &str,
        detail: &str,
        cand_score: Option<f64>,
        base_score: Option<f64>,
        replay_total: i64,
        regressions: i64,
        cfg: &Config,
    ) {
        let _ = self.with_store(|st| {
            st.update_attempt(
                attempt_id,
                status,
                detail,
                cand_score,
                base_score,
                replay_total,
                regressions,
            )
        });
        let (_, consec) = self
            .with_store(|st| st.get_opt_state(&info.id))
            .unwrap_or((None, 0));
        let consec = consec + 1;
        let _ = self.with_store(|st| st.set_consecutive_failures(&info.id, consec));
        self.set_cooldown_now(&info.id, cfg);
        self.activity(&info.name, format!("candidate {status}: {detail}"));
        let _ = self.with_store(|st| {
            st.audit(
                "optimiser",
                "tool.candidate_discarded",
                &format!("{}: {detail}", info.name),
            )
        });
        if consec >= cfg.optimiser.give_up_after_failures as i64 {
            let _ = self.with_store(|st| st.set_needs_attention(&info.id, true));
            let _ = self.with_store(|st| st.audit("optimiser", "tool.needs_attention", &info.name));
            self.activity(
                &info.name,
                "flagged needs attention after repeated failed candidates".into(),
            );
            if let Ok(Some((t, _))) = self.with_store(|st| st.get_tool_current(&info.id)) {
                self.broadcast(Event::ToolUpdated { tool: t });
            }
        }
    }

    /// Execute once in the replay sandbox, returning the output value.
    /// `override_cand` (with `tool_name` as its name) lets nested self-calls
    /// run the Candidate, not the stored version.
    #[allow(clippy::too_many_arguments)]
    pub fn replay_exec(
        self: &Arc<Self>,
        _tool_name: &str,
        code: &str,
        caps: &Capabilities,
        memory_mb: u32,
        input: Value,
        cand_name: &str,
        override_cand: Option<&CandidateData>,
        depth: usize,
        overlay: Overlay,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<Value, String>> + Send>,
    > {
        let this = self.clone();
        let (code, cand_name) = (code.to_string(), cand_name.to_string());
        let caps = caps.clone();
        let override_cand = override_cand.cloned();
        Box::pin(async move {
            if depth > 8 {
                return Err("tool nesting too deep (possible cycle)".to_string());
            }
            let caps = caps.bind(&input)?;
            let ops_daemon = this.clone();
            let ops = Arc::new(ReplayOps {
                daemon: this,
                depth: depth + 1,
                overlay: overlay.clone(),
                cand_name,
                override_cand,
                handle: tokio::runtime::Handle::current(),
            });
            let req = ExecRequest {
                code,
                input,
                caps,
                memory_mb,
                mode: HostMode::Replay,
                overlay: Some(overlay),
                activity: None,
                modules: ops_daemon
                    .with_store(|st| st.list_module_code())
                    .unwrap_or_default(),
                cancel: Arc::new(AtomicBool::new(false)),
            };
            let ops_dyn: Arc<dyn HostOps> = ops;
            match tokio::task::spawn_blocking(move || formless_runtime::execute(&req, ops_dyn))
                .await
            {
                Ok(Ok(out)) => Ok(out.output),
                Ok(Err(e)) => Err(e.message),
                Err(e) => Err(format!("replay panicked: {e}")),
            }
        })
    }

    /// One replayed invocation as a pass/fail gate (output must validate).
    #[allow(clippy::too_many_arguments)]
    pub fn replay_one(
        self: &Arc<Self>,
        tool_name: &str,
        code: &str,
        caps: &Capabilities,
        memory_mb: u32,
        output_schema: &str,
        input: Value,
        override_cand: Option<&CandidateData>,
        depth: usize,
        overlay: Overlay,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
        let this = self.clone();
        let (tool_name, code, output_schema) = (
            tool_name.to_string(),
            code.to_string(),
            output_schema.to_string(),
        );
        let caps = caps.clone();
        let override_cand = override_cand.cloned();
        Box::pin(async move {
            match this
                .replay_exec(
                    &tool_name,
                    &code,
                    &caps,
                    memory_mb,
                    input,
                    &tool_name,
                    override_cand.as_ref(),
                    depth,
                    overlay,
                )
                .await
            {
                Ok(output) => {
                    let schema: Value = serde_json::from_str(&output_schema).unwrap_or(json!({}));
                    match toolreg::compile_schema(&schema) {
                        Ok(v) => toolreg::validate_against(&v, &output).is_ok(),
                        Err(_) => false,
                    }
                }
                Err(_) => false,
            }
        })
    }
}

struct ReplayOps {
    daemon: Arc<Daemon>,
    depth: usize,
    overlay: Overlay,
    cand_name: String,
    override_cand: Option<CandidateData>,
    handle: tokio::runtime::Handle,
}

impl HostOps for ReplayOps {
    fn call_tool(&self, name: &str, input: Value) -> std::result::Result<Value, String> {
        let daemon = self.daemon.clone();
        let (code, caps, memory_mb, tool_name) = if name == self.cand_name {
            match &self.override_cand {
                Some(c) => (
                    c.code.clone(),
                    Capabilities::parse(&c.capabilities).unwrap_or_default(),
                    c.memory_mb,
                    name.to_string(),
                ),
                None => match daemon.with_store(|st| st.get_tool_by_name(name)) {
                    Ok(Some((info, ver))) if !info.retired => (
                        ver.code.clone(),
                        Capabilities::parse(&ver.capabilities).unwrap_or_default(),
                        ver.memory_mb,
                        info.name.clone(),
                    ),
                    _ => return Err(format!("no tool named '{name}'")),
                },
            }
        } else {
            match daemon.with_store(|st| st.get_tool_by_name(name)) {
                Ok(Some((info, ver))) if !info.retired => (
                    ver.code.clone(),
                    Capabilities::parse(&ver.capabilities).unwrap_or_default(),
                    ver.memory_mb,
                    info.name.clone(),
                ),
                _ => return Err(format!("no tool named '{name}'")),
            }
        };
        let (depth, overlay, cand_name, cand) = (
            self.depth,
            self.overlay.clone(),
            self.cand_name.clone(),
            self.override_cand.clone(),
        );
        self.handle.block_on(async move {
            daemon
                .replay_exec(
                    &tool_name,
                    &code,
                    &caps,
                    memory_mb,
                    input,
                    &cand_name,
                    cand.as_ref(),
                    depth,
                    overlay,
                )
                .await
        })
    }
    fn emit(&self, _chunk: &str) -> std::result::Result<(), String> {
        Ok(())
    }
}

fn parse_candidate(raw: &str, ver: &ToolVersionInfo, cfg: &Config) -> Result<CandidateData> {
    let start = raw
        .find('{')
        .ok_or_else(|| anyhow!("candidate is not JSON"))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| anyhow!("candidate is not JSON"))?;
    let v: Value = serde_json::from_str(&raw[start..=end])?;
    let code = v["implementation_js"]
        .as_str()
        .ok_or_else(|| anyhow!("candidate missing implementation_js"))?
        .to_string();
    formless_runtime::check_syntax(&code).map_err(|e| anyhow!("candidate syntax: {e}"))?;
    let input_schema = v["input_schema"].clone();
    let output_schema = v["output_schema"].clone();
    if !input_schema.is_object() || !output_schema.is_object() {
        return Err(anyhow!("candidate schemas must be objects"));
    }
    toolreg::compile_schema(&input_schema)?;
    toolreg::compile_schema(&output_schema)?;
    let capabilities: Vec<String> = v["capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_else(|| ver.capabilities.clone());
    Capabilities::parse(&capabilities).map_err(|e| anyhow!(e))?;
    let memory_mb = v["memory_mb"]
        .as_u64()
        .map(|m| m as u32)
        .unwrap_or(ver.memory_mb);
    if memory_mb == 0 || memory_mb > cfg.tools.max_memory_mb {
        return Err(anyhow!("candidate memory_mb out of range"));
    }
    Ok(CandidateData {
        code,
        description: v["description"].as_str().unwrap_or_default().to_string(),
        input_schema: input_schema.to_string(),
        output_schema: output_schema.to_string(),
        capabilities,
        memory_mb,
    })
}

/// One fixed prompt-eval scenario: the model is asked for its best next
/// action; a response passes if it mentions any expected phrase.
pub struct PromptEvalCase {
    pub name: &'static str,
    pub user: &'static str,
    pub expect_any: &'static [&'static str],
}

pub fn prompt_eval_cases() -> Vec<PromptEvalCase> {
    vec![
        PromptEvalCase {
            name: "build_tool_first",
            user: "Task: read a CSV file and sum a column. No suitable Tool exists yet. What is your first action? Answer with the action only.",
            expect_any: &["create_tool", "build", "create a tool"],
        },
        PromptEvalCase {
            name: "stream_large_data",
            user: "Task: process a 2 GB log file. How do you read it in a Tool without running out of memory? Answer briefly.",
            expect_any: &["stream", "chunk", "readchunks"],
        },
        PromptEvalCase {
            name: "sensitive_capability",
            user: "A new Tool needs unrestricted network access. What must happen before it first runs? Answer briefly.",
            expect_any: &["approval", "approve", "permission"],
        },
        PromptEvalCase {
            name: "broken_tool",
            user: "A Tool keeps failing schema validation after two fixes. What do you do? Answer briefly.",
            expect_any: &["retire", "new version", "create a new"],
        },
        PromptEvalCase {
            name: "reuse_tools",
            user: "An existing Tool already does exactly what the task needs. What do you do? Answer briefly.",
            expect_any: &["reuse", "existing", "call it"],
        },
        PromptEvalCase {
            name: "generic_tools",
            user: "Task: count lines in /home/a/proj/notes.txt. You need a new Tool. What does its input take? Answer briefly.",
            expect_any: &["path", "parameter", "filename", "file name"],
        },
        PromptEvalCase {
            name: "no_secrets_in_code",
            user: "A Tool needs an API key. Where should the key live? Answer briefly.",
            expect_any: &["env", "keychain", "never in", "not in code", "environment"],
        },
    ]
}

/// A Tool called repeatedly but only ever from one Session: a hint that it
/// hardcodes task-specific values instead of taking them as input.
pub fn is_narrow(m: &ToolMetricsInfo) -> bool {
    m.sessions == 1 && m.total >= 3
}

/// Prompt-proposal promotion bar: the candidate eval score must beat the
/// baseline by `margin_points` (scores are 0..1), or match a perfect
/// baseline with a perfect score.
pub fn proposal_beats_baseline(base: f64, cand: f64, margin_points: u32) -> bool {
    // Epsilon keeps an exact win (e.g. 0.6 vs 0.5 at 10 points) from
    // failing to float rounding.
    let margin = margin_points as f64 / 100.0 - 1e-9;
    cand - base >= margin || (cand >= 1.0 && cand >= base)
}

pub fn score_eval_responses(cases: &[PromptEvalCase], responses: &[String]) -> f64 {
    if cases.is_empty() || responses.is_empty() {
        return 0.0;
    }
    let passed = cases
        .iter()
        .zip(responses.iter())
        .filter(|(c, r)| {
            let lower = r.to_lowercase();
            c.expect_any
                .iter()
                .any(|e| lower.contains(&e.to_lowercase()))
        })
        .count();
    passed as f64 / cases.len().min(responses.len()) as f64
}

async fn evaluate_prompt(
    provider: &Provider,
    system_prompt: &str,
    model: &str,
    effort: Option<&str>,
) -> Result<(f64, i64)> {
    let cases = prompt_eval_cases();
    let mut responses = Vec::new();
    for case in &cases {
        let r = provider
            .complete(
                &[
                    json!({"role": "system", "content": system_prompt}),
                    json!({"role": "user", "content": case.user}),
                ],
                model,
                effort,
            )
            .await?;
        responses.push(r);
    }
    Ok((score_eval_responses(&cases, &responses), cases.len() as i64))
}

fn parse_proposal(raw: &str) -> Result<(String, String, String)> {
    let start = raw
        .find('{')
        .ok_or_else(|| anyhow!("proposal is not JSON"))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| anyhow!("proposal is not JSON"))?;
    let v: Value = serde_json::from_str(&raw[start..=end])?;
    let kind = v["kind"].as_str().unwrap_or("prompt").trim().to_string();
    if kind != "prompt" && kind != "memory" {
        return Err(anyhow!("proposal kind must be prompt|memory"));
    }
    let content = v["content"]
        .as_str()
        .ok_or_else(|| anyhow!("proposal missing content"))?
        .trim()
        .to_string();
    if content.is_empty() || content.chars().count() > 800 {
        return Err(anyhow!("proposal content must be 1..=800 chars"));
    }
    let lower = content.to_lowercase();
    if lower.contains("ignore previous") || lower.contains("ignore all instructions") {
        return Err(anyhow!(
            "proposal rejected: attempts to override instructions"
        ));
    }
    let rationale = v["rationale"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok((kind, content, rationale))
}
