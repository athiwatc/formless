use super::*;
use std::path::PathBuf;

struct Fixture {
    daemon: Arc<Daemon>,
    paths: crate::Paths,
    root: PathBuf,
    tool: ToolInfo,
    candidate: CandidateData,
}
impl Fixture {
    fn new(env: &crate::credential_env::EnvGuard, minimum: usize) -> Self {
        Self::with_codes(
            env,
            minimum,
            "function run(){throw new Error('baseline fails');}",
            "function run(){return {ok:true};}",
        )
    }
    fn with_codes(
        env: &crate::credential_env::EnvGuard,
        minimum: usize,
        baseline_code: &str,
        candidate_code: &str,
    ) -> Self {
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        let root =
            std::env::temp_dir().join(format!("formless-paired-gate-{}", uuid::Uuid::new_v4()));
        let paths = crate::Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
        cfg.provider.model = "no-live-provider".into();
        cfg.optimiser.replay_min_holdout = minimum;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let input = r#"{"type":"object","properties":{"optional":{"type":"integer"}}}"#;
        let tool = daemon
            .with_store(|st| {
                st.create_tool_version(
                    "held_target",
                    "paired fixture",
                    input,
                    r#"{"type":"object"}"#,
                    baseline_code,
                    &Capabilities::default(),
                    64,
                    "agent",
                )
            })
            .unwrap()
            .0;
        let candidate = CandidateData {
            code: candidate_code.into(),
            description: "candidate".into(),
            input_schema: input.into(),
            output_schema: r#"{"type":"object"}"#.into(),
            capabilities: vec![],
            memory_mb: 64,
        };
        Self {
            daemon,
            paths,
            root,
            tool,
            candidate,
        }
    }
    fn seed(&self, n: usize) {
        self.daemon
            .with_store(|st| {
                for index in 0..n {
                    st.record_invocation(
                        &self.tool.id,
                        1,
                        None,
                        None,
                        None,
                        "failure",
                        "js",
                        &json!({"case":format!("unique-source-{index}"),"optional":1}).to_string(),
                        "",
                        "baseline fails",
                        1,
                        1000,
                    )?;
                }
                Ok(())
            })
            .unwrap();
    }
    fn mixed(&self, n: usize, tag: &str) {
        self.daemon
            .with_store(|st| {
                for index in 0..n {
                    let input = json!({"case":format!("mixed-{index}"),"optional":1}).to_string();
                    st.record_invocation(
                        &self.tool.id,
                        1,
                        None,
                        None,
                        None,
                        "success",
                        "",
                        &input,
                        "{}",
                        "",
                        1,
                        1000,
                    )?;
                    st.record_invocation(
                        &self.tool.id,
                        1,
                        None,
                        None,
                        None,
                        "failure",
                        tag,
                        &input,
                        "",
                        &format!("latest-{tag}-{index}"),
                        1,
                        1000,
                    )?;
                }
                Ok(())
            })
            .unwrap();
    }
    fn attempt(&self) -> (String, ReplaySnapshot) {
        self.daemon
            .with_store(|st| {
                let snapshot = st.create_replay_snapshot(&self.tool.id, 1, 200, 50)?;
                let id = st.create_attempt_with_replay_snapshot(
                    &self.tool.id,
                    1,
                    &self.candidate,
                    0,
                    &snapshot.id,
                )?;
                Ok((id, snapshot))
            })
            .unwrap()
    }
    async fn replay(&self, id: &str) {
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.daemon.replay_and_maybe_promote(
                id.into(),
                &self.daemon.config().unwrap(),
                PromotionApprovals::default(),
            ),
        )
        .await
        .unwrap();
    }
    fn ticket(&self) -> PromotionTicket {
        let mut tickets = self.daemon.promotions.locked();
        assert_eq!(tickets.len(), 1);
        let id = tickets.keys().next().unwrap().clone();
        tickets.remove(&id).unwrap()
    }
    fn version(&self) -> i64 {
        self.daemon
            .with_store(|st| st.get_tool_current(&self.tool.id))
            .unwrap()
            .unwrap()
            .1
            .version
    }
    fn detail(&self, id: &str) -> Value {
        serde_json::from_str(
            &self
                .daemon
                .with_store(|st| st.attempt_scores(id))
                .unwrap()
                .0,
        )
        .unwrap()
    }
    fn status(&self, id: &str) -> String {
        self.daemon
            .with_store(|st| st.get_attempt(id))
            .unwrap()
            .unwrap()
            .status
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.daemon.shutdown.cancel();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn exact_five_vs_six_discordant_wins_select_manual_vs_automatic_promotion() {
    let env = crate::credential_env::EnvGuard::isolated();
    let manual = Fixture::new(&env, 2);
    manual.seed(10);
    let (id, snapshot) = manual.attempt();
    manual.replay(&id).await;
    assert_eq!(snapshot.holdout.len(), 5);
    assert_eq!(manual.version(), 1);
    assert_eq!(manual.status(&id), "awaiting_approval");
    let evidence = manual.detail(&id);
    let paired = &evidence["holdout"]["paired"];
    assert_eq!(paired["n"], 5);
    assert_eq!(paired["candidate_only"], 5);
    assert_eq!(paired["baseline_only"], 0);
    assert_eq!(paired["p_value"], 0.0625);
    assert_eq!(paired["reason"], "not_significant");
    assert_eq!(evidence["holdout"]["paired_counts"]["both_fail"], 0);
    assert!(paired["candidate_ci"].is_array() && paired["baseline_ci"].is_array());
    let ticket = manual.ticket();
    assert!(matches!(ticket.gate, PromotionGate::Holdout(_)));
    manual.daemon.clone().resume_promotion(ticket, true).await;
    assert_eq!(manual.version(), 2);
    assert_eq!(manual.detail(&id)["holdout"]["manual_acknowledged"], true);
    let automatic = Fixture::new(&env, 2);
    automatic.seed(12);
    let (id, snapshot) = automatic.attempt();
    automatic.replay(&id).await;
    assert_eq!(snapshot.holdout.len(), 6);
    assert_eq!(automatic.version(), 2);
    assert_eq!(automatic.status(&id), "promoted");
    assert_eq!(
        automatic.detail(&id)["holdout"]["paired"]["p_value"],
        0.03125
    );
    assert_eq!(
        automatic.detail(&id)["holdout"]["paired"]["eligible_auto"],
        true
    );
    assert!(automatic.daemon.promotions.locked().is_empty());
}

#[tokio::test]
async fn significant_but_small_holdout_waits_and_a_denial_never_changes_the_tool() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, 20);
    f.seed(12);
    let (id, _) = f.attempt();
    f.replay(&id).await;
    assert_eq!(f.version(), 1);
    assert_eq!(
        f.detail(&id)["holdout"]["paired"]["reason"],
        "insufficient_holdout"
    );
    assert_eq!(f.detail(&id)["holdout"]["paired"]["p_value"], 0.03125);
    let ticket = f.ticket();
    f.daemon.clone().resume_promotion(ticket, false).await;
    assert_eq!(f.version(), 1);
    assert_eq!(f.status(&id), "discarded");
    assert_eq!(f.detail(&id)["holdout"]["holdout_count"], 6);
}

#[tokio::test]
async fn frozen_pairs_survive_new_calls_and_changed_criteria_require_a_new_manual_ack() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, 2);
    f.seed(10);
    let (id, snapshot) = f.attempt();
    f.replay(&id).await;
    let ticket = f.ticket();
    let PromotionGate::Holdout(old_context) = &ticket.gate else {
        panic!("holdout ticket");
    };
    let old_context = old_context.clone();
    f.seed(80);
    f.daemon
        .handle(Request::SetSetting {
            key: "optimiser.replay_min_holdout".into(),
            value: json!(6),
        })
        .await
        .unwrap();
    f.daemon.clone().resume_promotion(ticket, true).await;
    assert_eq!(f.version(), 1);
    let refreshed = f.ticket();
    let PromotionGate::Holdout(new_context) = &refreshed.gate else {
        panic!("fresh holdout ticket");
    };
    assert_ne!(&old_context, new_context);
    assert_eq!(
        refreshed
            .approvals
            .evaluation
            .as_ref()
            .unwrap()
            .counts
            .candidate_only,
        5
    );
    let detail = f.detail(&id);
    assert_eq!(detail["holdout"]["snapshot_digest"], snapshot.digest);
    assert_eq!(detail["holdout"]["holdout_count"], 5);
    assert_eq!(detail["holdout"]["paired"]["criteria"]["minimum_n"], 6);
    f.daemon.clone().resume_promotion(refreshed, true).await;
    assert_eq!(f.version(), 2);
    assert_eq!(f.detail(&id)["holdout"]["paired"]["n"], 5);
}

#[tokio::test]
async fn an_unproven_legacy_attempt_keeps_history_but_cannot_resample_or_promote() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, 2);
    f.seed(12);
    let id = f
        .daemon
        .with_store(|st| st.create_attempt(&f.tool.id, 1, &f.candidate, 0))
        .unwrap();
    f.daemon
        .with_store(|st| {
            st.update_attempt(
                &id,
                "awaiting_approval",
                "historical replay result",
                Some(1.0),
                Some(0.0),
                6,
                0,
            )
        })
        .unwrap();
    f.replay(&id).await;
    assert_eq!(f.version(), 1);
    assert_eq!(f.status(&id), "awaiting_approval");
    assert!(f.daemon.promotions.locked().is_empty());
    let (_, candidate, baseline, n, _) = f.daemon.with_store(|st| st.attempt_scores(&id)).unwrap();
    assert_eq!((candidate, baseline, n), (Some(1.0), Some(0.0), 6));
    assert_eq!(f.detail(&id)["holdout"]["action"], "regenerate");
    assert!(f
        .daemon
        .with_store(|st| st.attempt_replay_snapshot(&id))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn audit_or_detail_statistics_cannot_replace_actual_paired_evidence_or_regression_gates() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, 2);
    f.seed(10);
    let (id, _) = f.attempt();
    f.daemon
        .with_store(|st| {
            st.set_attempt_holdout_detail(
                &id,
                &json!({"paired":{"eligible_auto":true,"p_value":0.0},"manual_acknowledged":true}),
            )?;
            st.audit("user", "optimiser.holdout_approved", &id)
        })
        .unwrap();
    f.replay(&id).await;
    assert_eq!(f.version(), 1);
    assert_eq!(f.detail(&id)["holdout"]["paired"]["p_value"], 0.0625);
    let ticket = f.ticket();
    f.daemon
        .with_store(|st| {
            st.create_tool_version(
                "held_target",
                "newer",
                "{}",
                "{}",
                "function run(){return {newer:true};}",
                &Capabilities::default(),
                64,
                "user",
            )
        })
        .unwrap();
    f.daemon.clone().resume_promotion(ticket, true).await;
    assert_eq!(f.version(), 2);
    assert_eq!(f.status(&id), "discarded");
}

#[tokio::test]
async fn weak_holdout_ack_composes_with_capability_network_and_schema_gates_without_early_credit() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env, 2);
    f.seed(10);
    f.daemon
        .with_store(|st| {
            st.create_tool_version(
                "held_caller",
                "caller",
                "{}",
                "{}",
                "function run(i){return tools.call('held_target',i);}",
                &Capabilities::default(),
                64,
                "agent",
            )
        })
        .unwrap();
    let mut schema: Value = serde_json::from_str(&f.candidate.input_schema).unwrap();
    schema["required"] = json!(["optional"]);
    f.candidate.input_schema = schema.to_string();
    f.candidate.capabilities = vec!["net:127.0.0.1".into()];
    let (id, _) = f.attempt();
    f.daemon
        .with_store(|st| st.set_consecutive_failures(&f.tool.id, 2))
        .unwrap();
    f.daemon
        .request_promotion_ticket(&f.tool, PromotionTicket::widening(id.clone()), &f.candidate);
    let widening = f.ticket();
    f.daemon.clone().resume_promotion(widening, true).await;
    let weak = f.ticket();
    assert!(matches!(weak.gate, PromotionGate::Holdout(_)));
    assert_eq!(f.version(), 1);
    f.daemon.clone().resume_promotion(weak, true).await;
    let network = f.ticket();
    assert!(matches!(network.gate, PromotionGate::ReplayLimited));
    assert_eq!(f.version(), 1);
    f.daemon.clone().resume_promotion(network, true).await;
    let schema = f.ticket();
    assert!(matches!(schema.gate, PromotionGate::Schema(_)));
    assert_eq!(f.version(), 1);
    assert!(schema.approvals.holdout_context.is_some() && schema.approvals.replay_limited);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_opt_state(&f.tool.id))
            .unwrap()
            .1,
        2
    );
    f.daemon.clone().resume_promotion(schema, true).await;
    assert_eq!(f.version(), 2);
    assert_eq!(f.status(&id), "promoted");
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_opt_state(&f.tool.id))
            .unwrap()
            .1,
        0
    );
    assert_eq!(
        f.daemon
            .with_store(|st| st.internal_tokens_today("optimiser"))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn malformed_candidate_output_schema_never_becomes_an_any_schema_replay_success() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env, 2);
    f.seed(10);
    f.candidate.output_schema = "{not-json".into();
    let (id, _) = f.attempt();
    f.replay(&id).await;
    assert_eq!(f.version(), 1);
    assert_eq!(f.status(&id), "discarded");
    assert!(f.daemon.promotions.locked().is_empty());
    let report = f.detail(&id);
    assert_eq!(report["holdout"]["paired"]["candidate_passes"], 0);
    assert_eq!(report["holdout"]["paired_counts"]["both_fail"], 5);
    assert_eq!(report["holdout"]["paired"]["eligible_auto"], false);
}

#[tokio::test]
async fn recorded_success_regressions_preserve_failure_credit_and_never_offer_manual_override() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::with_codes(
        &env,
        2,
        "function run(){return {ok:true};}",
        "function run(){throw new Error('candidate regresses');}",
    );
    f.daemon
        .with_store(|st| {
            for index in 0..10 {
                st.record_invocation(
                    &f.tool.id,
                    1,
                    None,
                    None,
                    None,
                    "success",
                    "",
                    &json!({"case":index,"optional":1}).to_string(),
                    "{\"ok\":true}",
                    "",
                    1,
                    1000,
                )?;
            }
            st.set_consecutive_failures(&f.tool.id, 2)
        })
        .unwrap();
    let (id, _) = f.attempt();
    f.replay(&id).await;
    assert_eq!(f.version(), 1);
    assert_eq!(f.status(&id), "discarded");
    assert!(f.daemon.promotions.locked().is_empty());
    assert_eq!(
        f.detail(&id)["holdout"]["paired_counts"]["baseline_only"],
        5
    );
    assert_eq!(f.detail(&id)["holdout"]["recorded_success_regressions"], 5);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_opt_state(&f.tool.id))
            .unwrap()
            .1,
        3
    );
}

#[tokio::test]
async fn mixed_validation_context_keeps_prior_success_guards_without_hiding_the_latest_failure_warning(
) {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env, 2);
    f.mixed(12, "validation");
    let mut schema: Value = serde_json::from_str(&f.candidate.input_schema).unwrap();
    schema["required"] = json!(["optional"]);
    f.candidate.input_schema = schema.to_string();
    let (id, snapshot) = f.attempt();
    assert!(snapshot.generation.iter().all(|item| item.recorded_success));
    assert!(snapshot.holdout.iter().all(|item| item.recorded_success));
    let latest = generation_context(&snapshot).unwrap();
    assert!(latest
        .iter()
        .all(|item| !item.recorded_success && item.tag == "validation"));
    assert!(
        matches!(fix_plan(&latest),FixPlan::Rewrite(strategy) if strategy.contains("validation"))
    );
    let detail = f.detail(&id);
    assert_eq!(detail["validation_fix_context"], true);
    assert!(detail["validation_schema_warning"].is_string());
    assert_eq!(
        detail["schema_compatibility"]["compatibility"]["verdict"],
        "breaking"
    );
}

#[tokio::test]
async fn mixed_prior_success_and_latest_capability_denials_make_zero_provider_requests_and_no_attempt(
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-capability-preflight-key");
    let f = Fixture::new(&env, 2);
    f.mixed(12, "denied-capability");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut cfg = f.daemon.config().unwrap();
    cfg.provider.base_url = endpoint.clone();
    cfg.save(&f.paths).unwrap();
    assert_eq!(f.daemon.config().unwrap().provider.base_url, endpoint);
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {_=&mut stopped=>break,accepted=listener.accept()=>{let(stream,_)=accepted.unwrap();counted.fetch_add(1,Ordering::SeqCst);drop(stream);}}
        }
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.daemon.optimise_tool(&f.tool.id, &cfg),
    )
    .await;
    let _ = stop.send(());
    task.await.unwrap();
    result.unwrap();
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "known capability-only failures must not pay for model rewrite"
    );
    assert!(f
        .daemon
        .with_store(|st| st.list_attempts(&f.tool.id, 10))
        .unwrap()
        .is_empty());
    assert!(f
        .daemon
        .with_store(|st| st.replay_set(&f.tool.id))
        .unwrap()
        .iter()
        .all(|item| !item.recorded_success && item.tag == "denied-capability"));
    assert_eq!(f.version(), 1);
    assert_eq!(
        f.daemon
            .with_store(|st| st.internal_tokens_today("optimiser"))
            .unwrap(),
        0
    );
}
