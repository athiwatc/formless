//! Durable summary bases and one-note receipts. No schema change or inference
//! from timestamps, message counts, display counters or provider usage.
use super::*;
use formless_protocol::{
    BranchSummaryApplied, BranchSummaryBasis, BranchSummaryProjection, BranchSummaryUsage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const SUMMARY_SOURCE_BYTES: usize = 64 * 1024;
const SUMMARY_SOURCE_MESSAGES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub(crate) enum BranchSummaryFailure {
    #[error("Source or target Session not found or deleted")]
    NotFound,
    #[error("Source or target changed; inspect again and generate a new summary")]
    Stale,
    #[error("{0}")]
    Invalid(String),
}

pub(crate) struct BranchSummarySnapshot {
    pub basis: BranchSummaryBasis,
    pub frame: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BranchSummaryConfirmation {
    pub preview_id: String,
    pub source_id: String,
    pub target_id: String,
    pub source_revision: String,
    pub target_revision: String,
    pub candidate_hash: String,
}
#[derive(Serialize, Deserialize)]
struct SummaryReceipt {
    confirmation: BranchSummaryConfirmation,
    message_id: String,
}

impl Store {
    pub(crate) fn branch_summary_snapshot(
        &self,
        source: &str,
        target: &str,
        cfg: &crate::Config,
    ) -> Result<BranchSummarySnapshot> {
        self.read_transaction(|st| {
            if source==target {return Err(BranchSummaryFailure::Invalid("Choose a different source Session to summarize into this Session".into()).into());}
            if source.len()>256 || target.len()>256 {return Err(BranchSummaryFailure::Invalid("Session identifiers exceed the summary review limit".into()).into());}
            let a=st.summary_session(source)?;let b=st.summary_session(target)?;
            let source_name=summary_label(&a.name);let target_name=summary_label(&b.name);
            let (provider,requested_model,_)=summary_selection(&b,cfg)?;
            let source_revision=st.branch_summary_revision(source,cfg)?;
            let target_revision=st.branch_summary_revision(target,cfg)?;
            let (total_messages,total_bytes):(u64,u64)=st.conn.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(content AS BLOB))),0) FROM messages WHERE session_id=?1",params![source],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if total_messages==0 {return Err(BranchSummaryFailure::Invalid("The source Session has no transcript to summarize".into()).into());}
            let mut projection=BranchSummaryProjection{total_messages,included_messages:0,omitted_messages:total_messages,total_bytes,included_bytes:0,omitted_bytes:total_bytes,truncated_messages:0,metadata_omitted:true,attachment_bodies_omitted:true};
            let mut messages=Vec::<Value>::new();
            // Whole messages only: no partial credential at an excerpt boundary.
            // Oversized messages are omitted explicitly, without reading their
            // body or any attachment. Revisions still hash their actual bytes.
            let mut query=st.ps("SELECT seq,substr(role,1,64),CASE WHEN length(CAST(content AS BLOB))<=?2 THEN content ELSE NULL END,length(CAST(content AS BLOB)) FROM messages WHERE session_id=?1 ORDER BY seq DESC,id DESC LIMIT ?3")?;
            let rows=query.query_map(params![source,SUMMARY_SOURCE_BYTES,SUMMARY_SOURCE_MESSAGES],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,u64>(3)?)))?;
            for row in rows {
                let (seq,role,content,bytes)=row?;
                let Some(content)=content else{continue;};
                let row=crate::tools::redact(&json!({"seq":seq,"role":role,"content":content}));
                messages.push(row);
                projection.included_messages+=1;projection.included_bytes+=bytes;
                projection.omitted_messages=total_messages-projection.included_messages;projection.omitted_bytes=total_bytes-projection.included_bytes;
                if summary_frame(source,&source_name,&projection,&messages).len()>SUMMARY_SOURCE_BYTES {
                    messages.pop();projection.included_messages-=1;projection.included_bytes-=bytes;
                    projection.omitted_messages=total_messages-projection.included_messages;projection.omitted_bytes=total_bytes-projection.included_bytes;
                }
            }
            if messages.is_empty() {return Err(BranchSummaryFailure::Invalid("No whole source message fits the bounded summary projection; no provider request was made".into()).into());}
            messages.reverse();
            let frame=summary_frame(source,&source_name,&projection,&messages);
            Ok(BranchSummarySnapshot {basis:BranchSummaryBasis{source_id:source.into(),target_id:target.into(),source_name,target_name,source_revision,target_revision,provider,requested_model,projection},frame})
        })
    }

    fn summary_session(&self, id: &str) -> Result<SessionInfo> {
        let session = self
            .get_session(id)?
            .context(BranchSummaryFailure::NotFound)?;
        if self.session_deleted(id)? {
            return Err(BranchSummaryFailure::NotFound.into());
        }
        Ok(session)
    }

    pub(crate) fn branch_summary_revision(&self, id: &str, cfg: &crate::Config) -> Result<String> {
        let session = self.summary_session(id)?;
        let (_, _, selection) = summary_selection(&session, cfg)?;
        let mut revision = Sha256::new();
        hash_value(
            &mut revision,
            &json!({"id":session.id,"name":session.name,"model":session.model,"effort":session.reasoning_effort,"instructions":session.instructions,"cwd":session.cwd,"forked_from":session.forked_from,"fork_point_seq":session.fork_point_seq,"selection":selection}),
        )?;
        let mut query = self.ps(
            "SELECT id,seq,role,content,meta FROM messages WHERE session_id=?1 ORDER BY seq,id",
        )?;
        let mut rows = query.query(params![id])?;
        while let Some(row) = rows.next()? {
            for column in 0..5 {
                use rusqlite::types::ValueRef;
                match row.get_ref(column)? {
                    ValueRef::Null => revision.update([0]),
                    ValueRef::Integer(value) => {
                        revision.update([1]);
                        revision.update(value.to_le_bytes());
                    }
                    ValueRef::Text(bytes) => {
                        revision.update([2]);
                        revision.update((bytes.len() as u64).to_le_bytes());
                        revision.update(bytes);
                    }
                    _ => anyhow::bail!("invalid transcript field in summary revision"),
                }
            }
        }
        Ok(format!("{:x}", revision.finalize()))
    }

    pub(crate) fn branch_summary_usage(
        &self,
        target: &str,
        operation: &str,
        cfg: &crate::Config,
        requests: u64,
    ) -> Result<BranchSummaryUsage> {
        let mut usage = BranchSummaryUsage {
            requests,
            reported_requests: 0,
            reported_prompt_tokens: 0,
            reported_completion_tokens: 0,
            reported_cached_tokens: 0,
            reported_cache_write_tokens: 0,
            usage_complete: true,
            cost: None,
            priced_cost_subtotal: 0.0,
            estimated_requests: 0,
            unpriced: Vec::new(),
        };
        let mut prices_complete = true;
        let mut query=self.ps("SELECT provider,model,prompt_tokens,completion_tokens,cached_tokens,cache_write_tokens,cost,usage_complete,cost_unknown FROM run_usage WHERE session_id=?1 AND run_id=?2")?;
        let rows = query.query_map(params![target, operation], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
                r.get::<_, u64>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, u64>(5)?,
                r.get::<_, Option<f64>>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, bool>(8)?,
            ))
        })?;
        for row in rows {
            let (provider, model, p, c, cache, write, reported, complete, cost_unknown) = row?;
            usage.reported_requests += 1;
            usage.reported_prompt_tokens = usage
                .reported_prompt_tokens
                .checked_add(p)
                .context("summary token total overflow")?;
            usage.reported_completion_tokens = usage
                .reported_completion_tokens
                .checked_add(c)
                .context("summary token total overflow")?;
            usage.reported_cached_tokens = usage
                .reported_cached_tokens
                .checked_add(cache)
                .context("summary token total overflow")?;
            usage.reported_cache_write_tokens = usage
                .reported_cache_write_tokens
                .checked_add(write)
                .context("summary token total overflow")?;
            usage.usage_complete &= complete;
            let price = reported
                .or_else(|| {
                    cfg.provider_cfg(Some(&provider)).ok()?;
                    cfg.cost_of_known(
                        &provider,
                        &model,
                        p as i64,
                        c as i64,
                        cache as i64,
                        write as i64,
                        None,
                        cost_unknown,
                    )
                })
                .filter(|p| p.is_finite());
            if reported.is_none() && price.is_some() {
                usage.estimated_requests += 1;
            }
            if let Some(price) = price {
                usage.priced_cost_subtotal += price;
                if !usage.priced_cost_subtotal.is_finite() {
                    anyhow::bail!("summary cost total overflow");
                }
            } else {
                prices_complete = false;
                let name = format!("{provider}/{model}");
                if !usage.unpriced.contains(&name) {
                    usage.unpriced.push(name);
                }
            }
        }
        usage.usage_complete &= requests > 0 && requests == usage.reported_requests;
        if usage.usage_complete && prices_complete {
            usage.cost = Some(usage.priced_cost_subtotal);
        }
        Ok(usage)
    }

    pub(crate) fn branch_summary_receipt(
        &self,
        key: &BranchSummaryConfirmation,
    ) -> Result<Option<BranchSummaryApplied>> {
        let detail: Option<String> = self
            .conn
            .query_row(
                "SELECT detail FROM audit WHERE id=?1 AND action='branch_summary.applied'",
                params![format!("branch-summary-commit:{}", key.preview_id)],
                |r| r.get(0),
            )
            .optional()?;
        let Some(detail) = detail else {
            return Ok(None);
        };
        let receipt: SummaryReceipt = serde_json::from_str(&detail)?;
        if receipt.confirmation != *key {
            return Err(BranchSummaryFailure::Invalid("Summary confirmation does not match its recorded source, target, revisions or candidate hash".into()).into());
        }
        self.summary_session(&key.target_id)?;
        let message = self
            .conn
            .query_row(
                "SELECT * FROM messages WHERE id=?1 AND session_id=?2",
                params![receipt.message_id, key.target_id],
                Self::row_msg,
            )
            .optional()?
            .context(BranchSummaryFailure::Stale)?;
        let meta: Value = serde_json::from_str(message.meta.as_deref().unwrap_or_default())
            .context("invalid summary receipt metadata")?;
        if summary_hash(&message.content) != key.candidate_hash
            || meta["tool_data"] != true
            || meta["branch_summary"]["preview_id"] != key.preview_id
            || meta["branch_summary"]["source_id"] != key.source_id
            || meta["branch_summary"]["target_id"] != key.target_id
            || meta["branch_summary"]["source_revision"] != key.source_revision
            || meta["branch_summary"]["target_revision"] != key.target_revision
            || meta["branch_summary"]["candidate_hash"] != key.candidate_hash
        {
            return Err(BranchSummaryFailure::Stale.into());
        }
        Ok(Some(BranchSummaryApplied {
            preview_id: key.preview_id.clone(),
            source_id: key.source_id.clone(),
            target_id: key.target_id.clone(),
            message,
            already_applied: true,
        }))
    }

    pub(crate) fn append_branch_summary(
        &self,
        key: &BranchSummaryConfirmation,
        candidate: &str,
        cfg: &crate::Config,
        provider: &str,
        served: &str,
        projection: &BranchSummaryProjection,
    ) -> Result<BranchSummaryApplied> {
        self.transaction(|st| {
            if let Some(receipt)=st.branch_summary_receipt(key)?{return Ok(receipt);}
            if st.branch_summary_revision(&key.source_id,cfg)?!=key.source_revision || st.branch_summary_revision(&key.target_id,cfg)?!=key.target_revision {return Err(BranchSummaryFailure::Stale.into());}
            if summary_hash(candidate)!=key.candidate_hash {return Err(BranchSummaryFailure::Invalid("Summary candidate hash mismatch".into()).into());}
            let origin=crate::provenance::Origin::new("branch_summary","session",&format!("session:{}",key.source_id));
            let meta=json!({"tool_data":true,"data_origins":[origin],"branch_summary":{"preview_id":key.preview_id,"source_id":key.source_id,"target_id":key.target_id,"source_revision":key.source_revision,"target_revision":key.target_revision,"candidate_hash":key.candidate_hash,"provider":provider,"served_model":served,"projection":projection}});
            let message=st.append_message_meta(&key.target_id,"system-note",candidate,Some(meta.to_string()))?;
            let receipt=SummaryReceipt{confirmation:key.clone(),message_id:message.id.clone()};
            st.ex("INSERT INTO audit(id,actor,action,detail,created_at) VALUES(?1,'user','branch_summary.applied',?2,?3)",params![format!("branch-summary-commit:{}",key.preview_id),serde_json::to_string(&receipt)?,now()])?;
            Ok(BranchSummaryApplied {preview_id:key.preview_id.clone(),source_id:key.source_id.clone(),target_id:key.target_id.clone(),message,already_applied:false})
        })
    }
}

pub(crate) fn summary_hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
pub(crate) fn summary_label(text: &str) -> String {
    let clean = crate::tools::redact(&json!(text));
    let text = clean.as_str().unwrap_or_default();
    let mut label: String = text.chars().take(128).collect();
    if text.chars().count() > 128 {
        label.push_str("… (name shortened)");
    }
    label
}
fn summary_frame(
    source: &str,
    name: &str,
    manifest: &BranchSummaryProjection,
    messages: &[Value],
) -> String {
    let origin =
        crate::provenance::Origin::new("branch_summary", "session", &format!("session:{source}"));
    crate::provenance::frame("source_branch",&[origin],&json!({"source_session_id":source,"source_name":name,"omission_manifest":manifest,"messages":messages}).to_string())
}
fn hash_value(hash: &mut Sha256, value: &Value) -> Result<()> {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let ordered: std::collections::BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
                serde_json::to_value(ordered).expect("JSON map")
            }
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    let bytes = serde_json::to_vec(&canonical(value))?;
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
    Ok(())
}
fn summary_selection(
    session: &SessionInfo,
    cfg: &crate::Config,
) -> Result<(String, String, Value)> {
    let requested = cfg
        .provider
        .resolve_alias(session.model.as_deref().unwrap_or(&cfg.provider.model));
    let role = requested
        .strip_prefix("role:")
        .map(|name| cfg.role(name))
        .transpose()?;
    let provider_name = role
        .and_then(|r| r.provider.as_deref())
        .filter(|name| !name.is_empty())
        .unwrap_or(crate::config::DEFAULT_PROVIDER);
    let provider = cfg.provider_cfg(Some(provider_name))?;
    let mut settings = serde_json::to_value(&provider)?;
    // Listing freshness, cost estimates and calibrated context counters are
    // not the semantic model/transcript revision of a review candidate.
    if let Some(map) = settings.as_object_mut() {
        map.remove("prices");
        map.remove("model_context_tokens");
    }
    let selection = json!({"default_aliases":cfg.provider.aliases,"provider":provider_name,"settings":settings,"role":role,"task_min_samples":cfg.tools.task_min_samples,"task_quality_bar":cfg.tools.task_quality_bar,"task_explore_rate":cfg.tools.task_explore_rate});
    if requested.is_empty() || requested.len() > 2048 || provider_name.len() > 2048 {
        return Err(BranchSummaryFailure::Invalid(
            "Choose a bounded, non-empty target model before summarizing".into(),
        )
        .into());
    }
    Ok((provider_name.into(), requested, selection))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_usage_preserves_native_unknown_and_reported_cost_after_restart() {
        let root = std::env::temp_dir().join(format!("fl-summary-cost-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("formless.db");
        let st = Store::open(&file).unwrap();
        let session = st.create_session(None).unwrap().id;
        let unknown = crate::provider::Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            reported: 3,
            cost_unknown: true,
            ..Default::default()
        };
        let reported = crate::provider::Usage {
            cost: Some(0.4),
            ..unknown.clone()
        };
        let text = crate::provider::Usage {
            cost_unknown: false,
            ..unknown.clone()
        };
        for (operation, rows) in [
            ("unknown", vec![unknown.clone()]),
            ("reported", vec![reported.clone()]),
            ("mixed", vec![unknown, reported, text]),
        ] {
            for usage in rows {
                st.record_usage_full(
                    &session,
                    operation,
                    "default",
                    "model",
                    &usage,
                    "branch-summary",
                )
                .unwrap();
            }
        }
        drop(st);
        for price in [1_000_000.0, 0.0] {
            let st = Store::open(&file).unwrap();
            let mut cfg = crate::Config::default();
            cfg.provider.prices.insert(
                "model".into(),
                crate::config::ModelPrice {
                    input: price,
                    output: price,
                    ..Default::default()
                },
            );
            let unknown = st
                .branch_summary_usage(&session, "unknown", &cfg, 1)
                .unwrap();
            assert!(unknown.usage_complete);
            assert_eq!(
                unknown.cost, None,
                "native unknown never becomes text-priced"
            );
            assert_eq!(unknown.estimated_requests, 0);
            assert_eq!(unknown.priced_cost_subtotal, 0.0);
            assert_eq!(unknown.unpriced, vec!["default/model"]);
            let reported = st
                .branch_summary_usage(&session, "reported", &cfg, 1)
                .unwrap();
            assert_eq!(
                reported.cost,
                Some(0.4),
                "reported USD wins regardless of current price"
            );
            assert_eq!(reported.estimated_requests, 0);
            assert!(reported.unpriced.is_empty());
            let mixed = st.branch_summary_usage(&session, "mixed", &cfg, 3).unwrap();
            assert_eq!(mixed.cost, None);
            assert_eq!(mixed.estimated_requests, 1);
            assert_eq!(mixed.priced_cost_subtotal, 0.4 + 12.0 * price / 1_000_000.0);
            assert_eq!(mixed.reported_requests, 3);
            assert!(mixed.usage_complete);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn fixture_config() -> crate::Config {
        let mut cfg = crate::Config::default();
        cfg.provider.model = "private-summary-fixture-model".into();
        cfg
    }

    fn fixture() -> (Store, String, String, crate::Config) {
        let st = Store::open_in_memory().unwrap();
        let source = st.create_session(Some("Evidence branch".into())).unwrap();
        let target = st.create_session(Some("Target branch".into())).unwrap();
        st.append_message_meta(
            &source.id,
            "user",
            "Evidence and unresolved questions",
            Some(r#"{"private_metadata":"not projected"}"#.into()),
        )
        .unwrap();
        (st, source.id, target.id, fixture_config())
    }
    fn confirmation(basis: &BranchSummaryBasis, candidate: &str) -> BranchSummaryConfirmation {
        BranchSummaryConfirmation {
            preview_id: uuid::Uuid::new_v4().to_string(),
            source_id: basis.source_id.clone(),
            target_id: basis.target_id.clone(),
            source_revision: basis.source_revision.clone(),
            target_revision: basis.target_revision.clone(),
            candidate_hash: summary_hash(candidate),
        }
    }

    #[test]
    fn empty_target_model_refuses_basis_without_changing_transcripts() {
        let (st, source, target, mut cfg) = fixture();
        cfg.provider.model.clear();
        let before_source = st.list_messages(&source).unwrap();
        let before_target = st.list_messages(&target).unwrap();
        let error = match st.branch_summary_snapshot(&source, &target, &cfg) {
            Ok(_) => panic!("empty model must not produce a summary basis"),
            Err(error) => error,
        };
        assert!(matches!(
            error.downcast_ref::<BranchSummaryFailure>(),
            Some(BranchSummaryFailure::Invalid(message))
                if message.contains("non-empty target model")
        ));
        assert_eq!(st.list_messages(&source).unwrap(), before_source);
        assert_eq!(st.list_messages(&target).unwrap(), before_target);
    }

    #[test]
    fn durable_revision_detects_same_count_metadata_edits_but_ignores_own_usage() {
        let (st, source, target, cfg) = fixture();
        let original = st.branch_summary_snapshot(&source, &target, &cfg).unwrap();
        assert!(!original.frame.contains("private_metadata"));
        st.record_usage_full(
            &target,
            "branch-summary:owned",
            "default",
            "model",
            &crate::provider::Usage {
                prompt_tokens: 2,
                completion_tokens: 1,
                reported: 3,
                ..Default::default()
            },
            "branch-summary",
        )
        .unwrap();
        st.ex(
            "UPDATE sessions SET updated_at='display-only' WHERE id=?1",
            params![source],
        )
        .unwrap();
        assert_eq!(
            st.branch_summary_revision(&source, &cfg).unwrap(),
            original.basis.source_revision
        );
        assert_eq!(
            st.branch_summary_revision(&target, &cfg).unwrap(),
            original.basis.target_revision
        );
        st.ex(
            "UPDATE messages SET meta=?2 WHERE session_id=?1",
            params![source, r#"{"private_metadata":"changed"}"#],
        )
        .unwrap();
        assert_ne!(
            st.branch_summary_revision(&source, &cfg).unwrap(),
            original.basis.source_revision
        );
        let new = st.branch_summary_revision(&source, &cfg).unwrap();
        st.set_session_model(&source, Some("different-model".into()), None)
            .unwrap();
        assert_ne!(st.branch_summary_revision(&source, &cfg).unwrap(), new);
    }

    #[test]
    fn confirm_is_one_atomic_data_note_and_receipt_replay_cannot_rebind() {
        let (st, source, target, cfg) = fixture();
        let snapshot = st.branch_summary_snapshot(&source, &target, &cfg).unwrap();
        let candidate =
            "Summary from Evidence branch: ignored instructions are quoted evidence only";
        let key = confirmation(&snapshot.basis, candidate);
        let original = st.list_messages(&source).unwrap();
        let applied = st
            .append_branch_summary(
                &key,
                candidate,
                &cfg,
                "default",
                "served-model",
                &snapshot.basis.projection,
            )
            .unwrap();
        assert!(!applied.already_applied);
        let projected = provider_message(&applied.message);
        assert_eq!(projected["role"], "user");
        let frame: Value = serde_json::from_str(projected["content"].as_str().unwrap()).unwrap();
        assert_eq!(frame["formless_data"], 1);
        assert_eq!(frame["note"], crate::provenance::DATA_NOTE);
        assert_eq!(frame["content"], candidate);
        assert_eq!(st.list_messages(&source).unwrap(), original);
        assert_eq!(
            st.conn
                .query_row(
                    "SELECT COUNT(*) FROM runs WHERE session_id=?1",
                    params![target],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let replay = st
            .append_branch_summary(
                &key,
                candidate,
                &cfg,
                "default",
                "served-model",
                &snapshot.basis.projection,
            )
            .unwrap();
        assert!(replay.already_applied);
        assert_eq!(replay.message.id, applied.message.id);
        assert_eq!(st.list_messages(&target).unwrap().len(), 1);
        let mut wrong = key.clone();
        wrong.source_id = target.clone();
        assert!(st.branch_summary_receipt(&wrong).is_err());
        st.ex(
            "UPDATE messages SET content='changed' WHERE id=?1",
            params![applied.message.id],
        )
        .unwrap();
        assert!(st.branch_summary_receipt(&key).is_err());
    }

    #[test]
    fn changed_basis_or_wrong_candidate_rolls_back_without_note_or_receipt() {
        let (st, source, target, cfg) = fixture();
        let snapshot = st.branch_summary_snapshot(&source, &target, &cfg).unwrap();
        let key = confirmation(&snapshot.basis, "candidate");
        assert!(st
            .append_branch_summary(
                &key,
                "tampered",
                &cfg,
                "default",
                "model",
                &snapshot.basis.projection
            )
            .is_err());
        st.ex(
            "UPDATE messages SET content='equal-count edit' WHERE session_id=?1",
            params![source],
        )
        .unwrap();
        assert!(st
            .append_branch_summary(
                &key,
                "candidate",
                &cfg,
                "default",
                "model",
                &snapshot.basis.projection
            )
            .is_err());
        assert!(st.list_messages(&target).unwrap().is_empty());
        assert!(st.branch_summary_receipt(&key).unwrap().is_none());
    }

    #[test]
    fn projection_reports_whole_message_omissions_without_attachment_or_metadata_access() {
        let (st, source, target, cfg) = fixture();
        st.append_message_meta(
            &source,
            "assistant",
            &"x".repeat(SUMMARY_SOURCE_BYTES + 1),
            Some(r#"{"attachments":[{"path":"never-open-this"}]}"#.into()),
        )
        .unwrap();
        for _ in 0..130 {
            st.append_message(&source, "user", "bounded evidence")
                .unwrap();
        }
        let snapshot = st.branch_summary_snapshot(&source, &target, &cfg).unwrap();
        let p = &snapshot.basis.projection;
        assert!(snapshot.frame.len() <= SUMMARY_SOURCE_BYTES);
        assert_eq!(p.total_messages, 132);
        assert_eq!(p.included_messages, 128);
        assert_eq!(p.omitted_messages, 4);
        assert_eq!(p.total_bytes, p.included_bytes + p.omitted_bytes);
        assert_eq!(p.truncated_messages, 0);
        assert!(p.metadata_omitted && p.attachment_bodies_omitted);
        assert!(!snapshot.frame.contains("never-open-this"));
    }
    #[test]
    fn durable_receipt_survives_reopen_but_rejects_attribution_metadata_edit() {
        struct Private(std::path::PathBuf);
        impl Drop for Private {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Private(
            std::env::temp_dir().join(format!("fl-summary-receipt-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&dir.0).unwrap();
        let file = dir.0.join("private.db");
        let cfg = fixture_config();
        let (key, message) = {
            let st = Store::open(&file).unwrap();
            let a = st.create_session(None).unwrap();
            let b = st.create_session(None).unwrap();
            st.append_message(&a.id, "user", "Durable evidence")
                .unwrap();
            let basis = st
                .branch_summary_snapshot(&a.id, &b.id, &cfg)
                .unwrap()
                .basis;
            let key = confirmation(&basis, "Reviewed data note");
            let applied = st
                .append_branch_summary(
                    &key,
                    "Reviewed data note",
                    &cfg,
                    "default",
                    "served",
                    &basis.projection,
                )
                .unwrap();
            (key, applied.message)
        };
        let st = Store::open(&file).unwrap();
        let replay = st.branch_summary_receipt(&key).unwrap().unwrap();
        assert!(replay.already_applied);
        assert_eq!(replay.message, message);
        let mut meta: Value = serde_json::from_str(message.meta.as_deref().unwrap()).unwrap();
        meta["branch_summary"]["source_id"] = json!("a-different-branch");
        st.ex(
            "UPDATE messages SET meta=?2 WHERE id=?1",
            params![message.id, meta.to_string()],
        )
        .unwrap();
        assert!(st.branch_summary_receipt(&key).is_err());
        assert_eq!(st.list_messages(&key.target_id).unwrap().len(), 1);
    }
}
