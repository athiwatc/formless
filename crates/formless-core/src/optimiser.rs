//! Background optimiser (SPEC §8.3–§8.5): trigger scan, Candidate
//! generation, sandboxed replay gate, promotion.

use anyhow::{anyhow, Result};
use formless_protocol::*;
use formless_runtime::LockExt as _;
use formless_runtime::{new_overlay, Capabilities, ExecRequest, HostMode, HostOps, Overlay};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::config::Config;
use crate::daemon::Daemon;
use crate::provider::{Provider, ToolCall};
use crate::storage::{CandidateData, ReplayItem};
use crate::tools as toolreg;

const PROPOSAL_PROMPT: &str = "You improve the Formless agent's learned guidance. \
Reply with ONLY a JSON object: {\"kind\": \"prompt\", \"content\": one short imperative guidance line (max 400 chars), \"rationale\": why the signals justify it}. \
Propose exactly one concrete, generalisable lesson from the failure signals. User preferences are not guidance: the agent keeps those in its user memory. \
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
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(
                    self.config().map(|c| c.optimiser.scan_interval_secs).unwrap_or(10).max(1),
                )) => {}
            }
            let Ok(cfg) = self.config() else {
                continue;
            };
            if !cfg.provider.is_configured() {
                continue;
            }
            if let Ok(triggered) = self.scan_triggers(&cfg) {
                for id in triggered {
                    let _ = self.with_store(|st| st.enqueue_optimise(&id));
                }
            }
            // Politeness (§8.3): skip only Tools an active Run is using;
            // the queue is persisted, so nothing is lost meanwhile.
            if let Some(tool_id) = self.next_idle_queued() {
                let _ = self.with_store(|st| st.dequeue_optimise(&tool_id));
                self.optimise_tool(&tool_id, &cfg).await;
            }
        }
    }

    /// Run ids of the Runs in progress.
    fn active_run_ids(&self) -> Vec<String> {
        self.runs
            .lock()
            .unwrap()
            .values()
            .map(|r| r.run_id.clone())
            .collect()
    }

    /// Whether an active Run is using this Tool.
    pub(crate) fn tool_in_use(&self, tool_id: &str) -> bool {
        let active = self.active_run_ids();
        self.with_store(|st| st.tool_busy(tool_id, &active))
            .unwrap_or(true)
    }

    /// The oldest queued Tool no active Run is using.
    pub fn next_idle_queued(&self) -> Option<String> {
        let queue = self.with_store(|st| st.optimise_queue()).ok()?;
        queue.into_iter().find(|id| !self.tool_in_use(id))
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
        let Ok(cfg) = self.config() else {
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
                        if self.tool_in_use(&id) {
                            let _ = self.with_store(|st| st.enqueue_optimise(&id));
                            continue;
                        }
                        self.optimise_tool(&id, &cfg).await;
                    }
                }
            }
        }
    }

    pub(crate) async fn generate_and_measure_proposal(self: Arc<Self>) {
        let Ok(cfg) = self.config() else {
            return;
        };
        if !cfg.provider.is_configured() {
            return;
        }
        self.activity(
            "prompt",
            "generating a proposal (runs the eval suite)...".into(),
        );
        if let Err(e) = crate::daemon::counted(
            "optimiser-generate",
            "",
            "",
            self.generate_proposal_inner(&cfg),
        )
        .await
        {
            let text = e.to_string();
            self.activity(
                "prompt",
                if text.starts_with("no new proposal") {
                    text
                } else {
                    format!("proposal generation failed: {text}")
                },
            );
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
        // Scripts run again and again but never saved (§7.12).
        let recurring = self
            .with_store(|st| st.recurring_scripts(cfg.scripts.recurrence_threshold, 5))
            .unwrap_or_default();
        if !recurring.is_empty() {
            signals.push_str(
                "\nRecurring run_script scripts never saved as a Tool (save_as would make them reusable and measured):\n",
            );
            for (sessions, uses, source) in &recurring {
                let head: String = source.lines().take(3).collect::<Vec<_>>().join(" / ");
                signals.push_str(&format!(
                    "- {sessions} sessions, uses [{}]: {}\n",
                    uses.join(", "),
                    head.chars().take(160).collect::<String>()
                ));
            }
        }
        if signals.lines().count() <= 1 {
            signals.push_str("(no Tool activity yet; propose a general first-run lesson)\n");
        }
        // Run outcomes (§8.7): whether Runs reach the user's goal.
        if let Ok(runs) = self.with_store(|st| st.run_metrics(200)) {
            let summary = crate::runs::summarize(&runs);
            if summary.runs > 0 {
                signals.push_str(&format!("\n{}\n", crate::runs::summary_line(&summary)));
            }
        }
        // Every earlier proposal, not only applied ones: with unchanged
        // signals the model would otherwise suggest the same thing again.
        let earlier = self.with_store(|st| st.list_proposals())?;
        if !earlier.is_empty() {
            signals.push_str(
                "\nEarlier proposals (do not repeat or rephrase them; rejected and rolled-back ones were judged unhelpful):\n",
            );
            for p in earlier.iter().take(40) {
                signals.push_str(&format!("- [{}] {}\n", p.status, p.content));
            }
        }
        let provider = self.provider(cfg).await?;
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
        let mut messages = vec![
            json!({"role": "system", "content": PROPOSAL_PROMPT}),
            json!({"role": "user", "content": signals}),
        ];
        let mut est_tokens = 0i64;
        let mut attempt = 0;
        let (kind, content, rationale) = loop {
            let raw = provider.complete(&messages, &model, effort).await?;
            est_tokens += ((signals.len() + raw.len()) / 4) as i64;
            let parsed = parse_proposal(&raw)?;
            match earlier
                .iter()
                .find(|p| near_duplicate(&p.content, &parsed.1))
            {
                None => break parsed,
                Some(dup) if attempt == 0 => {
                    // One more try, naming the repeat.
                    attempt += 1;
                    messages.push(json!({"role": "assistant", "content": raw}));
                    messages.push(json!({"role": "user", "content": format!(
                        "That repeats an earlier proposal ({}): \"{}\". Propose a different lesson, or reply with {{\"kind\":\"none\"}} if the signals hold nothing new.",
                        dup.status, dup.content
                    )}));
                }
                Some(dup) => {
                    self.activity(
                        "prompt",
                        format!(
                            "no new proposal: nothing new to learn from the current signals (it repeated an earlier {} one)",
                            dup.status
                        ),
                    );
                    return Ok(());
                }
            }
        };
        let proposal =
            self.with_store(|st| st.create_proposal(&kind, &content, &rationale, est_tokens))?;
        self.broadcast(Event::ProposalUpdated {
            proposal: proposal.clone(),
        });
        // Measure: fixed eval suite, baseline vs candidate prompt.
        let baseline_prompt = self.effective_system_prompt();
        let candidate_prompt = format!(
            "{baseline_prompt}\n\nLearned guidance (candidate): {}",
            content.trim()
        );
        let repeats = cfg.optimiser.eval_repeats;
        let measured = async {
            let (base_score, total) = crate::daemon::counted(
                "optimiser-eval",
                "",
                "",
                evaluate_prompt(&provider, &baseline_prompt, &model, effort, repeats),
            )
            .await?;
            let (cand_score, _) = crate::daemon::counted(
                "optimiser-eval",
                "",
                "",
                evaluate_prompt(&provider, &candidate_prompt, &model, effort, repeats),
            )
            .await?;
            Ok::<_, anyhow::Error>((base_score, cand_score, total))
        }
        .await;
        let (base_score, cand_score, total) = match measured {
            Ok(m) => m,
            Err(e) => {
                // An unmeasured proposal can never be applied: it is kept as
                // rejected (what clients are told, and what a fresh list
                // shows), with the reason in the audit log (SPEC §8.6).
                let rejected = self
                    .with_store(|st| st.set_proposal_status(&proposal.id, "rejected"))
                    .ok()
                    .flatten();
                let _ = self.with_store(|st| {
                    st.audit(
                        "optimiser",
                        "proposal.discard_unmeasured",
                        &format!("{}: {e:#}", proposal.id),
                    )
                });
                if let Some(p) = rejected {
                    self.broadcast(Event::ProposalUpdated { proposal: p });
                }
                return Err(anyhow!(
                    "measuring the proposal failed ({e}); it was discarded"
                ));
            }
        };
        if let Some(measured) = self
            .with_store(|st| st.update_proposal_eval(&proposal.id, base_score, cand_score, total))?
        {
            let ci = |s: f64| {
                let (lo, hi) = wilson_interval((s * total as f64).round() as i64, total);
                format!("{:.0}% [{:.0}–{:.0}]", s * 100.0, lo * 100.0, hi * 100.0)
            };
            self.activity("prompt", format!(
                "proposal measured: candidate {} vs baseline {} ({} scenario runs each{}) - awaiting your decision",
                ci(cand_score), ci(base_score), total,
                if significant_improvement(base_score, cand_score, total) { ", significant" } else { ", not significant" }
            ));
            self.broadcast(Event::ProposalUpdated { proposal: measured });
        }
        Ok(())
    }

    /// Apply gate: only measured proposals that beat the baseline by the
    /// configured promotion margin (or match a perfect baseline) apply.
    pub(crate) fn apply_proposal_checked(&self, id: &str) -> Result<Option<PromptProposalInfo>> {
        let cfg = self.config()?;
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
        // A win must also be significant, unless both are perfect.
        let n = p.eval_total;
        if !(cand >= 1.0 && base >= 1.0) && !significant_improvement(base, cand, n) {
            return Err(anyhow!(
                "candidate {cand:.2} vs baseline {base:.2} over {n} runs is not a significant improvement; generate a new proposal or raise optimiser.eval_repeats"
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
            // Real reported tokens (generation and eval suites) when the
            // endpoint reports them, else the chars/4 estimates (#806).
            let real = self
                .with_store(|st| st.internal_tokens_today("optimiser"))
                .unwrap_or(0);
            let estimated = self.with_store(|st| st.attempt_tokens_today()).unwrap_or(0);
            if real.max(estimated) >= budget as i64 {
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
        // MCP tools run on their server: there is no code to rewrite.
        if info.name.starts_with(crate::mcp::PREFIX) || !self.guardrails_ok(&info, cfg) {
            return;
        }
        // Failures that are only denied Capabilities are a grant decision
        // for the user, not a code problem (§8.3): surface, don't rewrite.
        let replay = self
            .with_store(|st| st.replay_set(tool_id))
            .unwrap_or_default();
        if let FixPlan::NeedsCapability(n) = fix_plan(&replay) {
            let text = format!(
                "needs a capability: {n} recent calls of {} were denied a Capability; widen its grant or create a new version with the Capability it needs (your decision)",
                info.name
            );
            self.activity(&info.name, text.clone());
            self.broadcast(Event::Note {
                session_id: None,
                text,
            });
            let _ =
                self.with_store(|st| st.audit("optimiser", "tool.needs_capability", &info.name));
            self.set_cooldown_now(tool_id, cfg);
            return;
        }
        self.activity(&info.name, "generating candidate…".into());
        match crate::daemon::counted(
            "optimiser-generate",
            "",
            "",
            self.generate_candidate(&info, &ver, cfg),
        )
        .await
        {
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
        if let FixPlan::Rewrite(strategy) = fix_plan(&replay) {
            prompt.push_str(&strategy);
        } else if resource {
            prompt.push_str(
                "\nNOTE: resource (OOM) failures present - use streaming/chunking first.\n",
            );
        }
        let provider = self.provider(cfg).await?;
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
        self.promotions.locked().insert(
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
        let Ok(cfg) = self.config() else {
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
            if cand_caps.net.is_some() || !cand_caps.mcp.is_empty() {
                // Replay-limited (§8.4): net and MCP tools can only promote
                // by hand (what a Candidate sends a server isn't checked).
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
        attempt_id: &str,
        cand: &CandidateData,
        cfg: &Config,
    ) {
        let caps = Capabilities::parse(&cand.capabilities).unwrap_or_default();
        match self.with_store(|st| {
            // Checked under the same store lock as the write: a Tool
            // retired (or deleted) while the Candidate was measured stays
            // retired, and the Candidate is discarded and logged (SPEC §16).
            let live = st
                .get_tool_current(&info.id)?
                .is_some_and(|(t, _)| !t.retired);
            if !live {
                st.audit(
                    "optimiser",
                    "tool.promote_discarded",
                    &format!(
                        "{}: retired before its Candidate could be promoted",
                        info.name
                    ),
                )?;
                let _ = st.update_attempt(
                    attempt_id,
                    "discarded",
                    "the Tool was retired",
                    None,
                    None,
                    0,
                    0,
                );
                return Err(anyhow::anyhow!("the Tool was retired; Candidate discarded"));
            }
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
        output_schema: &str,
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
        let (code, cand_name, output_schema) = (
            code.to_string(),
            cand_name.to_string(),
            serde_json::from_str(output_schema).ok(),
        );
        let caps = caps.clone();
        let override_cand = override_cand.cloned();
        Box::pin(async move {
            let max_depth = this.config().map(|c| c.tools.max_tool_depth).unwrap_or(8);
            if depth > max_depth {
                return Err(format!("tool nesting deeper than {max_depth}: a cycle?"));
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
                output_schema,
                code,
                input,
                caps,
                memory_mb,
                mode: HostMode::Replay,
                overlay: Some(overlay),
                activity: None,
                modules: ops_daemon.module_code(),
                cancel: Arc::new(AtomicBool::new(false)),
            };
            let ops_dyn: Arc<dyn HostOps> = ops;
            let limit = std::time::Duration::from_secs(
                ops_daemon
                    .config()
                    .map(|c| c.tools.replay_timeout_secs)
                    .unwrap_or(60)
                    .max(1),
            );
            match crate::daemon::execute_bounded(req, ops_dyn, None, limit).await {
                crate::daemon::Bounded::Done(Ok(out)) => Ok(out.output),
                crate::daemon::Bounded::Done(Err(e)) => Err(e.message),
                crate::daemon::Bounded::TimedOut => {
                    Err(format!("replay did not finish within {}s", limit.as_secs()))
                }
                crate::daemon::Bounded::Cancelled => Err("interrupted".into()),
                crate::daemon::Bounded::Panicked(e) => Err(format!("replay panicked: {e}")),
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
                    &output_schema,
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
        // Replay never reaches a real MCP server (ADR 0004): a nested MCP
        // call is answered from a recorded call with the same (redacted)
        // input, as the live call's output was stored.
        if name.starts_with(crate::mcp::PREFIX) {
            let key = toolreg::redact(&input).to_string();
            return match daemon.with_reader(|st| st.recorded_output(name, &key)) {
                Ok(Some(out)) => Ok(serde_json::from_str(&out).unwrap_or(Value::String(out))),
                _ => Err(format!("unrecorded MCP call in replay: {name}")),
            };
        }
        let (code, output_schema, caps, memory_mb, tool_name) = if name == self.cand_name {
            match &self.override_cand {
                Some(c) => (
                    c.code.clone(),
                    c.output_schema.clone(),
                    Capabilities::parse(&c.capabilities).unwrap_or_default(),
                    c.memory_mb,
                    name.to_string(),
                ),
                None => match daemon.with_store(|st| st.get_tool_by_name(name)) {
                    Ok(Some((info, ver))) if !info.retired => (
                        ver.code.clone(),
                        ver.output_schema.clone(),
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
                    ver.output_schema.clone(),
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
                    &output_schema,
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

/// One prompt-eval scenario (SPEC §8.6): a scripted conversation with the
/// real built-in Tool definitions; the case passes on what the agent
/// *does* next (its tool calls), not on what it says.
pub struct ScenarioCase {
    pub name: &'static str,
    pub messages: Vec<Value>,
    /// Registry Tools the scenario has loaded, beside the built-ins.
    pub extra_tools: Vec<Value>,
    pub check: fn(&[ToolCall]) -> bool,
}

/// A user task after which `find_tools` already came back empty.
fn after_empty_search(task: &str, query: &str) -> Vec<Value> {
    vec![
        json!({"role": "user", "content": task}),
        json!({"role": "assistant", "content": null, "tool_calls": [{
            "id": "call_find", "type": "function",
            "function": {"name": "find_tools", "arguments": json!({"query": query}).to_string()}
        }]}),
        json!({"role": "tool", "tool_call_id": "call_find",
               "content": "{\"matches\":[],\"note\":\"No matching Tools. Create one with create_tool.\"}"}),
    ]
}

/// Arguments of the first call to `name`, parsed.
fn call_args(calls: &[ToolCall], name: &str) -> Option<Value> {
    calls
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| serde_json::from_str(&c.arguments).ok())
}

fn created(calls: &[ToolCall]) -> Option<(String, Vec<String>, Value)> {
    let a = call_args(calls, toolreg::CREATE_TOOL)?;
    let caps = a["capabilities"]
        .as_array()
        .map(|c| {
            c.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some((
        a["implementation_js"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        caps,
        a,
    ))
}

pub fn scenario_cases() -> Vec<ScenarioCase> {
    vec![
        ScenarioCase {
            name: "find_before_create",
            messages: vec![json!({"role": "user", "content": "Sum the 'amount' column of /data/sales.csv."})],
            extra_tools: vec![],
            check: |calls| calls.first().is_some_and(|c| c.name == toolreg::FIND_TOOLS),
        },
        ScenarioCase {
            name: "least_privilege",
            messages: after_empty_search(
                "Read the text file at /srv/app/README.md and tell me its first line.",
                "read text file",
            ),
            extra_tools: vec![],
            check: |calls| {
                created(calls).is_some_and(|(_, caps, _)| {
                    !caps.is_empty()
                        && caps
                            .iter()
                            .all(|c| c.starts_with("fs:read:") && c != "fs:read:/")
                })
            },
        },
        ScenarioCase {
            name: "stream_large_data",
            messages: after_empty_search(
                "Count the lines of the 5 GB log at /var/log/big.log.",
                "count lines",
            ),
            extra_tools: vec![],
            check: |calls| created(calls).is_some_and(|(js, _, _)| js.contains("Chunks")),
        },
        ScenarioCase {
            name: "throw_on_failure",
            messages: after_empty_search(
                "Fetch https://example.com/status.json and tell me its 'state' field.",
                "http get json",
            ),
            extra_tools: vec![],
            check: |calls| {
                created(calls).is_some_and(|(js, _, _)| {
                    let compact: String = js.chars().filter(|c| !c.is_whitespace()).collect();
                    js.contains("throw") && !compact.contains("return{error")
                })
            },
        },
        ScenarioCase {
            name: "generic_tools",
            messages: after_empty_search(
                "Count the lines in /home/a/proj/notes.txt.",
                "count lines in file",
            ),
            extra_tools: vec![],
            check: |calls| {
                created(calls).is_some_and(|(js, _, a)| {
                    let schema_props = a["input_schema"]["properties"]
                        .as_object()
                        .is_some_and(|p| !p.is_empty());
                    schema_props
                        && !js.contains("/home/a/proj")
                        && !a["name"].as_str().unwrap_or_default().contains("notes")
                })
            },
        },
        ScenarioCase {
            name: "reuse_tools",
            messages: vec![
                json!({"role": "user", "content": "Sum the 'total' column of /data/q3.csv."}),
                json!({"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_find", "type": "function",
                    "function": {"name": "find_tools", "arguments": "{\"query\":\"csv column sum\"}"}
                }]}),
                json!({"role": "tool", "tool_call_id": "call_find",
                       "content": "{\"matches\":[{\"name\":\"csv_column_sum\",\"description\":\"Sum a numeric column of a CSV file\",\"success_rate\":0.98}],\"note\":\"Matches are now callable.\"}"}),
            ],
            extra_tools: vec![json!({"type": "function", "function": {
                "name": "csv_column_sum",
                "description": "Sum a numeric column of a CSV file",
                "parameters": {"type": "object", "properties": {
                    "path": {"type": "string"}, "column": {"type": "string"}
                }, "required": ["path", "column"]}
            }})],
            check: |calls| calls.first().is_some_and(|c| c.name == "csv_column_sum"),
        },
        ScenarioCase {
            name: "no_secrets_in_code",
            messages: after_empty_search(
                "Post 'deploy done' to our chat webhook API; it needs the API key from WEBHOOK_KEY.",
                "post to webhook",
            ),
            extra_tools: vec![],
            check: |calls| {
                created(calls).is_some_and(|(_, caps, _)| caps.iter().any(|c| c.starts_with("env:")))
            },
        },
    ]
}

/// 95% Wilson score interval for `passes` out of `n`.
pub fn wilson_interval(passes: i64, n: i64) -> (f64, f64) {
    if n <= 0 {
        return (0.0, 1.0);
    }
    let (n, p, z) = (n as f64, passes as f64 / n as f64, 1.96f64);
    let denom = 1.0 + z * z / n;
    let centre = (p + z * z / (2.0 * n)) / denom;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// One-sided two-proportion z-test at 5%: is the candidate's pass rate
/// really above the baseline's (both over `n` trials)?
pub fn significant_improvement(base: f64, cand: f64, n: i64) -> bool {
    if n <= 0 || cand <= base {
        return false;
    }
    let n = n as f64;
    let pooled = (base + cand) / 2.0;
    let se = (pooled * (1.0 - pooled) * 2.0 / n).sqrt();
    se == 0.0 || (cand - base) / se >= 1.645
}

/// What the optimiser does about a Tool's failures (SPEC §8.3).
#[derive(Debug, PartialEq)]
pub enum FixPlan {
    /// Every failure is a denied Capability: the user's call (count).
    NeedsCapability(usize),
    /// Rewrite, guided by this per-tag strategy (for the prompt).
    Rewrite(String),
}

/// Failure counts per tag ("js" for untagged exceptions), most first.
pub fn tag_breakdown(replay: &[ReplayItem]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for r in replay.iter().filter(|r| !r.recorded_success) {
        let tag = if r.tag.is_empty() {
            "js"
        } else {
            r.tag.as_str()
        };
        *counts.entry(tag.to_string()).or_default() += 1;
    }
    let mut out: Vec<_> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Rank fixes by failure tag: validation failures mean the description
/// or schema misleads the model; resource failures want streaming;
/// exceptions want code fixes; denied Capabilities are never fixed by
/// widening (only the user grants).
pub fn fix_plan(replay: &[ReplayItem]) -> FixPlan {
    let tags = tag_breakdown(replay);
    let total: usize = tags.iter().map(|(_, n)| n).sum();
    let denied = tags
        .iter()
        .find(|(t, _)| t == "denied-capability")
        .map_or(0, |(_, n)| *n);
    if total > 0 && denied == total {
        return FixPlan::NeedsCapability(denied);
    }
    let mut out = String::from("\nFailure tags (most first): ");
    out.push_str(
        &tags
            .iter()
            .map(|(t, n)| format!("{t}={n}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str("\nStrategy, in order:\n");
    for (tag, _) in &tags {
        out.push_str(match tag.as_str() {
            "validation" => "- validation: callers send input the schema rejects (or the output breaks the output schema). Fix the description and input_schema first: clearer field names and descriptions, optional fields with defaults, accept common variants. Change the code only as needed to match.\n",
            "resource" => "- resource (out of memory/time): stream with fs.readChunks / fetchChunks / emit instead of loading whole.\n",
            "denied-capability" => "- denied-capability: do not widen Capabilities; handle the denial with a clear thrown error. Grants are the user's decision.\n",
            "js" => "- js (exceptions): fix the failing code paths shown in the errors, keeping recorded successes working.\n",
            "user-cancelled" => "- user-cancelled: often too slow; make it faster or report progress with emit.\n",
            _ => "",
        });
    }
    FixPlan::Rewrite(out)
}

/// Whether two proposals say the same thing: equal, or ≥ 80% of their
/// words shared (case, punctuation and plural 's' ignored).
pub fn near_duplicate(a: &str, b: &str) -> bool {
    let words = |s: &str| -> std::collections::BTreeSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(|w| w.to_lowercase().trim_end_matches('s').to_string())
            .collect()
    };
    let (x, y) = (words(a), words(b));
    if x.is_empty() || y.is_empty() {
        return a.trim().eq_ignore_ascii_case(b.trim());
    }
    x.intersection(&y).count() as f64 / x.union(&y).count() as f64 >= 0.8
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

/// Run every scenario `repeats` times against `system_prompt`: the pass
/// rate and the number of trials.
async fn evaluate_prompt(
    provider: &Provider,
    system_prompt: &str,
    model: &str,
    effort: Option<&str>,
    repeats: u32,
) -> Result<(f64, i64)> {
    let builtins = toolreg::tool_definitions(&crate::storage::Store::open_in_memory()?, None, 0)?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let (mut passes, mut total) = (0i64, 0i64);
    for case in scenario_cases() {
        let mut messages = vec![json!({"role": "system", "content": system_prompt})];
        messages.extend(case.messages.iter().cloned());
        let tools: Vec<Value> = builtins.iter().chain(&case.extra_tools).cloned().collect();
        for _ in 0..repeats.max(1) {
            // One retry, for a transient provider error only (429, 5xx, a
            // dropped connection): it must not void a whole suite, and a
            // determined failure (auth, bad request) must not cost twice.
            let r = match provider
                .chat_stream(&messages, &tools, model, effort, &cancel, |_, _| {})
                .await
            {
                Ok(r) => r,
                Err(e) if !crate::errors::is_transient(&format!("{e:#}")) => return Err(e),
                Err(_) => {
                    provider
                        .chat_stream(&messages, &tools, model, effort, &cancel, |_, _| {})
                        .await?
                }
            };
            total += 1;
            if (case.check)(&r.tool_calls) {
                passes += 1;
            }
        }
    }
    Ok((passes as f64 / total.max(1) as f64, total))
}

fn parse_proposal(raw: &str) -> Result<(String, String, String)> {
    let start = raw
        .find('{')
        .ok_or_else(|| anyhow!("proposal is not JSON"))?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| anyhow!("proposal is not JSON"))?;
    let v: Value = serde_json::from_str(&raw[start..=end])?;
    if v["kind"].as_str() == Some("none") {
        return Err(anyhow!(
            "no new proposal: the model found nothing new to learn"
        ));
    }
    // Only guidance is generated now; memory lives in the memory store.
    let kind = "prompt".to_string();
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

#[cfg(test)]
mod race_tests {
    use super::*;

    /// Promotion racing a retirement: retirement wins, the Candidate is
    /// discarded and logged (SPEC §16, #759).
    #[test]
    fn a_retired_tool_is_never_promoted() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let short = &uuid::Uuid::new_v4().to_string()[..8];
        let base = std::path::PathBuf::from(format!("/tmp/fl-race-{short}"));
        let paths = crate::Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        Config::default().save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let caps = Capabilities::parse(&[]).unwrap();
        let (info, _) = daemon
            .with_store(|st| {
                st.create_tool_version(
                    "t",
                    "d",
                    r#"{"type":"object"}"#,
                    "{}",
                    "function run(){}",
                    &caps,
                    64,
                    "agent",
                )
            })
            .unwrap();
        daemon
            .with_store(|st| st.set_tool_retired(&info.id, true))
            .unwrap();
        let cand = CandidateData {
            code: "function run(){ return 2; }".into(),
            description: "d".into(),
            input_schema: r#"{"type":"object"}"#.into(),
            output_schema: "{}".into(),
            capabilities: Vec::new(),
            memory_mb: 64,
        };
        daemon.promote_attempt_with(&info, "a1", &cand, &Config::default());
        let (now, ver) = daemon
            .with_store(|st| st.get_tool_current(&info.id))
            .unwrap()
            .unwrap();
        assert!(now.retired, "still retired");
        assert_eq!(ver.version, 1, "no new version");
        let audit = daemon
            .with_store(|st| st.recent_audit(Some("tool.promote"), 5))
            .unwrap();
        assert!(
            audit
                .iter()
                .any(|(_, action, _, _)| action == "tool.promote_discarded"),
            "{audit:?}"
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
