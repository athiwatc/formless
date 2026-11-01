use super::*;

struct Fixture {
    daemon: Arc<Daemon>,
    path: std::path::PathBuf,
    session: String,
    tool: ToolInfo,
    candidate: CandidateData,
}
impl Fixture {
    fn new(_env: &crate::credential_env::EnvGuard) -> Self {
        let path =
            std::env::temp_dir().join(format!("formless-schema-attempt-{}", uuid::Uuid::new_v4()));
        let paths = crate::Paths::from_dirs(path.join("data"), path.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
        cfg.provider.model = "private-no-provider".into();
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let session = daemon.with_store(|st| st.create_session(None)).unwrap().id;
        let (tool, _) = daemon.with_store(|st| st.create_tool_version(
            "schema_target", "test", r#"{"type":"object","properties":{"legacy":{"type":"string"}},"additionalProperties":false}"#,
            r#"{"type":"object"}"#, "function run(){throw new Error('old implementation');}", &Capabilities::default(), 64, "agent",
        )).unwrap();
        let candidate = CandidateData { code: "function run(){return {}; }".into(), description: "fix validation".into(),
            input_schema: r#"{"type":"object","properties":{"legacy":{"type":"string"},"new_required":{"type":"string"}},"required":["new_required"],"additionalProperties":false}"#.into(),
            output_schema: r#"{"type":"object"}"#.into(), capabilities: vec![], memory_mb: 64 };
        Self {
            daemon,
            path,
            session,
            tool,
            candidate,
        }
    }
    fn dependent(&self, name: &str) -> ToolInfo {
        self.daemon
            .with_store(|st| {
                st.create_tool_version(
                    name,
                    "caller",
                    "{}",
                    "{}",
                    "async function run(input){return await tools.call('schema_target',input);}",
                    &Capabilities::default(),
                    64,
                    "agent",
                )
            })
            .unwrap()
            .0
    }
    fn attempt(&self) -> String {
        self.daemon
            .with_store(|st| st.create_attempt(&self.tool.id, 1, &self.candidate, 0))
            .unwrap()
    }
    fn info(&self) -> (ToolInfo, ToolVersionInfo) {
        self.daemon
            .with_store(|st| st.get_tool_current(&self.tool.id))
            .unwrap()
            .unwrap()
    }
    fn detail(&self, id: &str) -> Value {
        let (text, _, _, _, _) = self.daemon.with_store(|st| st.attempt_scores(id)).unwrap();
        serde_json::from_str(&text).unwrap()
    }
    fn take_ticket(&self) -> PromotionTicket {
        let mut tickets = self.daemon.promotions.locked();
        assert_eq!(tickets.len(), 1);
        let id = tickets.keys().next().unwrap().clone();
        tickets.remove(&id).unwrap()
    }
    async fn validation_failure(&self) {
        let (_, ok) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.daemon.call_tool_direct_outcome(
                &self.session,
                "schema_target",
                json!({"legacy":7}),
            ),
        )
        .await
        .unwrap();
        assert!(!ok);
        assert!(self
            .daemon
            .with_store(|st| st.replay_set(&self.tool.id))
            .unwrap()
            .iter()
            .any(|item| item.tag == "validation" && !item.recorded_success));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.daemon.shutdown.cancel();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn real_validation_replay_cannot_hide_breaking_schema_or_auto_promote_live_callers() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let dependent = f.dependent("real_caller");
    f.validation_failure().await;
    let id = f.attempt();
    let before = f.detail(&id);
    assert_eq!(
        before["schema_compatibility"]["compatibility"]["verdict"],
        "breaking"
    );
    assert_eq!(before["validation_fix_context"], true);
    assert!(before["validation_schema_warning"]
        .as_str()
        .unwrap()
        .contains("validation-tag"));
    assert!(before["schema_compatibility"]["affected_dependents"]
        .to_string()
        .contains(&dependent.id));
    f.daemon
        .replay_and_maybe_promote(
            id.clone(),
            &f.daemon.config().unwrap(),
            PromotionApprovals::default(),
        )
        .await;
    assert_eq!(f.info().1.version, 1);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_attempt(&id))
            .unwrap()
            .unwrap()
            .status,
        "awaiting_approval"
    );
    let scored = f.detail(&id);
    assert!(scored["summary"]
        .as_str()
        .unwrap()
        .contains("candidate 1/1 baseline 0/1"));
    assert!(scored["validation_schema_warning"].is_string());
    let ticket = f.take_ticket();
    assert!(matches!(ticket.gate, PromotionGate::Schema(_)));
    f.daemon.clone().resume_promotion(ticket, true).await;
    assert_eq!(f.info().1.version, 2);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_attempt(&id))
            .unwrap()
            .unwrap()
            .status,
        "promoted"
    );
    let promoted = f.detail(&id);
    assert_eq!(promoted["schema_compatibility"]["base_version"], 1);
    assert_eq!(
        promoted["schema_compatibility"]["compatibility"]["verdict"],
        "breaking"
    );
    assert!(promoted["validation_schema_warning"].is_string());
    assert_eq!(
        f.daemon
            .with_store(|st| st.internal_tokens_today("optimiser"))
            .unwrap(),
        0
    );
}

#[test]
fn changed_dependent_set_or_candidate_invalidates_an_earlier_typed_schema_approval() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    f.dependent("caller_one");
    let id = f.attempt();
    f.daemon
        .promote_attempt_with(&f.tool, &id, &f.candidate, &Config::default());
    assert_eq!(f.info().1.version, 1);
    let ticket = f.take_ticket();
    let PromotionGate::Schema(old_context) = ticket.gate else {
        panic!("expected schema approval");
    };
    f.dependent("caller_two");
    f.daemon.promote_attempt_with_approvals(
        &f.tool,
        &id,
        &f.candidate,
        &Config::default(),
        PromotionApprovals {
            schema_context: Some(old_context),
            replay_limited: true,
        },
    );
    assert_eq!(f.info().1.version, 1);
    let replacement = f.take_ticket();
    assert!(
        replacement.approvals.replay_limited,
        "existing replay-limited approval survives schema re-gating"
    );
    let PromotionGate::Schema(fresh_context) = replacement.gate else {
        panic!("schema context must be refreshed");
    };
    let report = f
        .daemon
        .with_store(|st| st.attempt_schema_report(&id))
        .unwrap();
    let mut changed = f.candidate.clone();
    changed.code = "function run(){return {changed:true};}".into();
    assert_ne!(
        schema_context(&id, &changed, &report).unwrap(),
        fresh_context
    );
    assert_eq!(
        schema_context(&id, &f.candidate, &report).unwrap(),
        fresh_context
    );
}

#[tokio::test]
async fn denying_schema_approval_preserves_the_live_version_and_report() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    f.dependent("caller");
    f.validation_failure().await;
    let id = f.attempt();
    f.daemon
        .replay_and_maybe_promote(
            id.clone(),
            &f.daemon.config().unwrap(),
            PromotionApprovals::default(),
        )
        .await;
    let ticket = f.take_ticket();
    f.daemon.clone().resume_promotion(ticket, false).await;
    assert_eq!(f.info().1.version, 1);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_attempt(&id))
            .unwrap()
            .unwrap()
            .status,
        "discarded"
    );
    assert!(f.detail(&id)["validation_schema_warning"].is_string());
    assert!(f.daemon.promotions.locked().is_empty());
}

#[tokio::test]
async fn compatible_fix_keeps_live_callers_and_auto_promotion_after_the_existing_replay_gate() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    f.dependent("compatible_caller");
    f.validation_failure().await;
    f.candidate.input_schema = f.info().1.input_schema;
    let id = f.attempt();
    assert_eq!(
        f.detail(&id)["schema_compatibility"]["compatibility"]["verdict"],
        "compatible"
    );
    f.daemon
        .replay_and_maybe_promote(
            id.clone(),
            &f.daemon.config().unwrap(),
            PromotionApprovals::default(),
        )
        .await;
    assert_eq!(f.info().1.version, 2);
    assert!(f.daemon.promotions.locked().is_empty());
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_attempt(&id))
            .unwrap()
            .unwrap()
            .status,
        "promoted"
    );
    assert!(f.detail(&id)["validation_schema_warning"].is_null());
}

#[test]
fn audit_or_attempt_detail_text_never_mints_a_schema_promotion_acknowledgement() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    f.dependent("caller");
    let id = f.attempt();
    let report = f
        .daemon
        .with_store(|st| st.attempt_schema_report(&id))
        .unwrap();
    let context = schema_context(&id, &f.candidate, &report).unwrap();
    f.daemon
        .with_store(|st| {
            st.audit(
                "user",
                "optimiser.schema_approved",
                &json!({"attempt_id":id,"context":context}).to_string(),
            )?;
            st.update_attempt(
                &id,
                "generated",
                &json!({"schema_approved":true,"context":context}).to_string(),
                None,
                None,
                0,
                0,
            )
        })
        .unwrap();
    f.daemon
        .promote_attempt_with(&f.tool, &id, &f.candidate, &Config::default());
    assert_eq!(f.info().1.version, 1);
    assert_eq!(f.daemon.promotions.locked().len(), 1);
}

#[tokio::test]
async fn schema_acknowledgement_composes_with_widening_and_replay_limited_approvals() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    f.dependent("network_caller");
    f.validation_failure().await;
    f.candidate.capabilities = vec!["net:127.0.0.1".into()];
    let id = f.attempt();
    f.daemon
        .with_store(|st| st.set_consecutive_failures(&f.tool.id, 2))
        .unwrap();
    f.daemon
        .request_promotion_ticket(&f.tool, PromotionTicket::widening(id.clone()), &f.candidate);
    let widening = f.take_ticket();
    assert!(matches!(widening.gate, PromotionGate::Widening));
    f.daemon.clone().resume_promotion(widening, true).await;
    assert_eq!(f.info().1.version, 1);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_opt_state(&f.tool.id))
            .unwrap()
            .1,
        2
    );
    let limited = f.take_ticket();
    assert!(matches!(limited.gate, PromotionGate::ReplayLimited));
    f.daemon.clone().resume_promotion(limited, true).await;
    assert_eq!(f.info().1.version, 1);
    let schema = f.take_ticket();
    assert!(matches!(schema.gate, PromotionGate::Schema(_)));
    assert!(schema.approvals.replay_limited);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_opt_state(&f.tool.id))
            .unwrap()
            .1,
        2
    );
    f.daemon.clone().resume_promotion(schema, true).await;
    assert_eq!(f.info().1.version, 2);
    assert_eq!(
        f.daemon
            .with_store(|st| st.get_opt_state(&f.tool.id))
            .unwrap()
            .1,
        0
    );
    assert!(f.daemon.promotions.locked().is_empty());
    assert_eq!(
        f.daemon
            .with_store(|st| st.internal_tokens_today("optimiser"))
            .unwrap(),
        0
    );
}
