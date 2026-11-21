use super::*;
use anyhow::{anyhow, bail, Context};
use formless_protocol::{
    HarnessKind, HarnessRole, TopicAttemptStatus, TopicFeatureProposal, TopicProposalRequest,
    TopicReviewOutcome,
};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

impl Store {
    pub fn is_main_chat(&self, session_id: &str) -> Result<bool> {
        let main_id: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM profile_metadata WHERE key='main_chat_session_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(main_id.as_deref() == Some(session_id))
    }

    pub fn main_chat(&self) -> Result<SessionInfo> {
        self.transaction(|st| {
            let existing = st
                .conn
                .query_row(
                    "SELECT value FROM profile_metadata WHERE key='main_chat_session_id'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(id) = existing {
                if let Some(session) = st.get_session(&id)? {
                    if st.session_deleted(&id)? {
                        bail!("Main Chat is pending deletion; restore it within the undo window or wait for it to be purged before opening a new Main Chat");
                    }
                    return Ok(session);
                }
            }
            let session = st.create_session(Some("Main Chat".into()))?;
            st.conn.execute(
                "UPDATE sessions SET session_kind='main' WHERE id=?1",
                params![session.id],
            )?;
            st.conn.execute(
                "INSERT INTO profile_metadata(key,value) VALUES('main_chat_session_id',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![session.id],
            )?;
            st.get_session(&session.id)?
                .ok_or_else(|| anyhow!("new main chat session vanished"))
        })
    }

    pub fn propose_topic(
        &self,
        proposal: &TopicProposalRequest,
        base_revision: &str,
    ) -> Result<TopicInfo> {
        if proposal.name.trim().is_empty() {
            bail!("topic name must not be empty");
        }
        if proposal.message_ids.is_empty() {
            bail!("select at least one user message");
        }
        if !proposal.repo.is_absolute() {
            bail!("topic repository path must be absolute");
        }
        let mut seen = HashSet::new();
        if proposal.message_ids.iter().any(|id| !seen.insert(id)) {
            bail!("selected message IDs must be unique");
        }
        self.transaction(|st| {
            st
                .get_session(&proposal.source_session_id)?
                .ok_or_else(|| anyhow!("source session not found"))?;
            let deleted: bool = st.conn.query_row(
                "SELECT deleted_at IS NOT NULL FROM sessions WHERE id=?1",
                params![proposal.source_session_id],
                |row| row.get(0),
            )?;
            if deleted {
                bail!("source session is deleted");
            }
            let mut selected = Vec::with_capacity(proposal.message_ids.len());
            for message_id in &proposal.message_ids {
                let message = st
                    .get_message(&proposal.source_session_id, message_id)?
                    .ok_or_else(|| anyhow!("selected message {message_id} is not in the source session"))?;
                if message.role != "user" {
                    bail!("only user messages can be proposed as feature requests");
                }
                selected.push(message);
            }
            selected.sort_by_key(|message| message.seq);
            let selected_by_id: HashMap<_, _> = selected
                .iter()
                .map(|message| (message.id.clone(), (message.seq, message.content.clone())))
                .collect();
            let features = if proposal.features.is_empty() {
                selected
                    .iter()
                    .map(|message| TopicFeatureProposal {
                        source_message_id: message.id.clone(),
                        request: message.content.clone(),
                    })
                    .collect()
            } else {
                proposal.features.clone()
            };
            if features.is_empty() || features.len() > 100 {
                bail!("topic proposals must contain between 1 and 100 features");
            }
            let mut represented = HashSet::new();
            let mut unique_features = HashSet::new();
            for feature in &features {
                let request = feature.request.trim();
                if request.is_empty() {
                    bail!("feature requests must not be empty");
                }
                if request.len() > 64 * 1024 {
                    bail!("feature requests must be no larger than 64 KiB");
                }
                if !selected_by_id.contains_key(&feature.source_message_id) {
                    bail!("feature refers to unselected source message {}", feature.source_message_id);
                }
                if !unique_features.insert((feature.source_message_id.clone(), request.to_owned())) {
                    bail!("duplicate feature request for source message {}", feature.source_message_id);
                }
                represented.insert(feature.source_message_id.as_str());
            }
            if selected.iter().any(|message| !represented.contains(message.id.as_str())) {
                bail!("every selected message must have at least one feature request");
            }

            let id = uuid::Uuid::new_v4().to_string();
            let timestamp = now();
            st.conn.execute(
                "INSERT INTO topics(id,name,repo,base_revision,source_session_id,status,builder_kind,builder_model,reviewer_kind,reviewer_model,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,'pending_approval',?6,?7,?8,?9,?10,?10)",
                params![
                    id,
                    proposal.name.trim(),
                    proposal.repo.to_string_lossy(),
                    base_revision,
                    proposal.source_session_id,
                    serde_json::to_value(proposal.builder)?.as_str().unwrap(),
                    proposal.builder_model,
                    serde_json::to_value(proposal.reviewer)?.as_str().unwrap(),
                    proposal.reviewer_model,
                    timestamp,
                ],
            )?;
            for message in selected {
                st.conn.execute(
                    "INSERT INTO topic_source_messages(topic_id,source_session_id,message_id,source_seq,source_content,source_content_hash)
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    params![id, proposal.source_session_id, message.id, message.seq, message.content, hash_token(&message.content)],
                )?;
            }
            for feature in features {
                let feature_id = uuid::Uuid::new_v4().to_string();
                let (source_seq, _) = selected_by_id.get(&feature.source_message_id)
                    .expect("feature source validated above");
                st.conn.execute(
                    "INSERT INTO topic_features(id,topic_id,source_message_id,source_seq,request,status,created_at,updated_at)
                     VALUES(?1,?2,?3,?4,?5,'pending',?6,?6)",
                    params![feature_id, id, feature.source_message_id, source_seq, feature.request.trim(), timestamp],
                )?;
            }
            topic_info(st, &id)?.ok_or_else(|| anyhow!("new topic vanished"))
        })
    }

    pub fn approve_topic(&self, topic_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let Some(mut topic) = topic_row(st, topic_id)? else {
                bail!("topic not found");
            };
            if topic.status == "approved" || topic.status == "running" || topic.status == "complete" {
                return topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"));
            }
            if topic.status != "pending_approval" {
                bail!("only a pending topic can be approved");
            }
            let active: bool = st.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id=?1 AND status IN ('running','paused'))
                      OR EXISTS(SELECT 1 FROM queued_messages WHERE session_id=?1)",
                params![topic.source_session_id],
                |row| row.get(0),
            )?;
            if active {
                bail!("cannot move selected messages while the source session has a running, paused, or queued run");
            }
            let session = st.create_session(Some(topic.name.clone()))?;
            let timestamp = now();
            st.conn.execute(
                "UPDATE sessions SET parent_session_id=?2, session_kind='topic', topic_id=?3, cwd=?4
                 WHERE id=?1",
                params![
                    session.id,
                    topic.source_session_id,
                    topic.id,
                    topic.repo,
                ],
            )?;
            let refs = st.conn.prepare(
                "SELECT message_id,source_seq,source_content_hash FROM topic_source_messages
                 WHERE topic_id=?1 ORDER BY source_seq",
            )?.query_map(params![topic_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            if refs.is_empty() {
                bail!("topic has no selected messages");
            }
            let mut moved = 0i64;
            for (seq, (message_id, source_seq, snapshot)) in refs.iter().enumerate() {
                let current = st.get_message(&topic.source_session_id, message_id)?
                    .ok_or_else(|| anyhow!("selected source message {message_id} no longer exists"))?;
                if current.role != "user" || hash_token(&current.content) != *snapshot {
                    bail!("selected source message {message_id} changed since proposal; create a new proposal to approve the edited request");
                }
                if let Some(meta) = &current.meta {
                    let meta: Value = serde_json::from_str(meta)
                        .context("selected source message has invalid attachment metadata")?;
                    for attachment in meta["attachments"].as_array().into_iter().flatten() {
                        if attachment["imported"] == true || attachment["reference"] == true || attachment["expired"] == true {
                            continue;
                        }
                        for id in [attachment["id"].as_str(), attachment["_artifact_id"].as_str()].into_iter().flatten() {
                            st.conn.execute(
                                "UPDATE artifacts SET session_id=?2 WHERE id=?1 AND session_id=?3",
                                params![id, session.id, topic.source_session_id],
                            )?;
                        }
                    }
                }
                let changed = st.conn.execute(
                    "UPDATE messages SET session_id=?2,seq=?3 WHERE id=?1 AND session_id=?4",
                    params![message_id, session.id, seq as i64 + 1, topic.source_session_id],
                )?;
                if changed != 1 {
                    bail!("selected source message {message_id} changed after proposal");
                }
                st.conn.execute(
                    "UPDATE topic_source_messages SET moved_at=?3 WHERE topic_id=?1 AND message_id=?2",
                    params![topic_id, message_id, timestamp],
                )?;
                let _ = source_seq;
                moved += 1;
            }
            let source_preview: Option<String> = st.conn.query_row(
                "SELECT substr(content,1,2000) FROM messages WHERE session_id=?1 AND role='user' ORDER BY seq DESC LIMIT 1",
                params![topic.source_session_id],
                |row| row.get(0),
            ).optional()?.flatten();
            let topic_preview: Option<String> = st.conn.query_row(
                "SELECT substr(content,1,2000) FROM messages WHERE session_id=?1 AND role='user' ORDER BY seq DESC LIMIT 1",
                params![session.id],
                |row| row.get(0),
            ).optional()?.flatten();
            st.conn.execute(
                "UPDATE sessions SET message_count=MAX(0,message_count-?2),preview=?3,updated_at=?4 WHERE id=?1",
                params![topic.source_session_id, moved, source_preview, timestamp],
            )?;
            st.conn.execute(
                "UPDATE sessions SET message_count=message_count+?2,preview=?3,updated_at=?4 WHERE id=?1",
                params![session.id, moved, topic_preview, timestamp],
            )?;
            st.conn.execute("DELETE FROM checkpoints WHERE session_id=?1", params![topic.source_session_id])?;
            st.conn.execute(
                "UPDATE topics SET topic_session_id=?2,status='approved',updated_at=?3 WHERE id=?1",
                params![topic_id, session.id, timestamp],
            )?;
            let redirect = st.append_message(
                &topic.source_session_id,
                "system",
                &format!("{} selected request(s) moved to topic '{}' ({}). Topic conversation: {}.", moved, topic.name, topic_id, session.id),
            )?;
            st.conn.execute(
                "UPDATE topics SET redirect_message_id=?2,updated_at=?3 WHERE id=?1",
                params![topic_id, redirect.id, now()],
            )?;
            st.note_rewrite();
            topic.topic_session_id = Some(session.id);
            topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished after approval"))
        })
    }

    pub fn reject_topic(&self, topic_id: &str, reason: &str) -> Result<TopicInfo> {
        if reason.trim().is_empty() {
            bail!("rejection reason must not be empty");
        }
        self.transaction(|st| {
            let status: Option<String> = st
                .conn
                .query_row(
                    "SELECT status FROM topics WHERE id=?1",
                    params![topic_id],
                    |row| row.get(0),
                )
                .optional()?;
            match status.as_deref() {
                None => bail!("topic not found"),
                Some("rejected") => {
                    return topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"))
                }
                Some("pending_approval") => {}
                _ => bail!("only a pending topic can be rejected"),
            }
            st.conn.execute(
                "UPDATE topics SET status='rejected',rejection_reason=?2,updated_at=?3 WHERE id=?1",
                params![topic_id, reason.trim(), now()],
            )?;
            topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"))
        })
    }

    pub fn list_topics(&self) -> Result<Vec<TopicInfo>> {
        let ids = self
            .conn
            .prepare("SELECT id FROM topics ORDER BY created_at DESC,id")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| topic_info(self, id)?.ok_or_else(|| anyhow!("topic vanished")))
            .collect()
    }

    pub fn get_topic(&self, topic_id: &str) -> Result<Option<TopicInfo>> {
        topic_info(self, topic_id)
    }

    pub fn list_topic_attempts(&self, topic_id: &str) -> Result<Vec<TopicAttemptInfo>> {
        if topic_row(self, topic_id)?.is_none() {
            bail!("topic not found");
        }
        let ids = self
            .conn
            .prepare("SELECT id FROM topic_attempts WHERE topic_id=?1 ORDER BY created_at,id")?
            .query_map(params![topic_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| topic_attempt(self, id)?.ok_or_else(|| anyhow!("topic attempt vanished")))
            .collect()
    }

    pub fn get_topic_attempt(&self, attempt_id: &str) -> Result<Option<TopicAttemptInfo>> {
        topic_attempt(self, attempt_id)
    }

    pub(crate) fn claim_topic_workflow(&self, topic_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.ok_or_else(|| anyhow!("topic not found"))?;
            if topic.status == "running" {
                bail!("topic workflow is already active; recover it explicitly if its child process is unknown");
            }
            if topic.status != "approved" {
                bail!("topic must be approved before its workflow can start");
            }
            let lock_owner: Option<String> = st.conn.query_row(
                "SELECT topic_id FROM topic_repo_locks WHERE repo=?1",
                params![topic.repo],
                |row| row.get(0),
            ).optional()?;
            if let Some(owner) = lock_owner {
                bail!("repository already has an active topic workflow ({owner})");
            }
            let timestamp = now();
            st.conn.execute(
                "INSERT INTO topic_repo_locks(repo,topic_id,acquired_at) VALUES(?1,?2,?3)",
                params![topic.repo, topic_id, timestamp],
            )?;
            st.conn.execute(
                "UPDATE topics SET status='running',updated_at=?2 WHERE id=?1",
                params![topic_id, timestamp],
            )?;
            topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"))
        })
    }

    pub(crate) fn begin_topic_attempt(
        &self,
        topic_id: &str,
        feature_id: &str,
        role: HarnessRole,
        harness: HarnessKind,
        round: u32,
        capability_token: &str,
        worktree: &Path,
        base_revision: &str,
    ) -> Result<TopicAttemptInfo> {
        if capability_token.len() < 32 {
            bail!("topic attempt capability token is too short");
        }
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.ok_or_else(|| anyhow!("topic not found"))?;
            if topic.status != "running" || topic.topic_session_id.is_none() {
                bail!("topic must be approved and running to create an attempt");
            }
            let feature_status: String = st.conn.query_row(
                "SELECT status FROM topic_features WHERE id=?1 AND topic_id=?2",
                params![feature_id, topic_id],
                |row| row.get(0),
            ).optional()?.ok_or_else(|| anyhow!("topic feature not found"))?;
            let expected = match role {
                HarnessRole::Build => "pending",
                HarnessRole::Review => "reviewing",
            };
            if feature_status != expected {
                bail!("topic feature is not ready for this attempt role");
            }
            if role == HarnessRole::Review {
                let reviewed: bool = st.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM topic_reviews WHERE feature_id=?1 AND revision=?2)",
                    params![feature_id, base_revision],
                    |row| row.get(0),
                )?;
                if reviewed {
                    bail!("this feature revision already has a submitted review");
                }
                let reserved: bool = st.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM topic_attempts WHERE feature_id=?1 AND revision=?2 AND role='review' AND status='reviewing')",
                    params![feature_id, base_revision],
                    |row| row.get(0),
                )?;
                if reserved {
                    bail!("a reviewer attempt is already reserved for this feature revision");
                }
            }
            let id = uuid::Uuid::new_v4().to_string();
            let session = st.create_session(Some(format!("{} {}", harness_label(harness), topic.name)))?;
            let timestamp = now();
            let role_text = serde_json::to_value(role)?.as_str().unwrap().to_owned();
            let harness_text = serde_json::to_value(harness)?.as_str().unwrap().to_owned();
            let pinned_revision = (role == HarnessRole::Review).then_some(base_revision);
            st.conn.execute(
                "INSERT INTO topic_attempts(id,topic_id,feature_id,round,role,harness_kind,status,conversation_session_id,capability_hash,worktree_path,base_revision,revision,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
                params![
                    id, topic_id, feature_id, round as i64, role_text, harness_text,
                    match role { HarnessRole::Build => "building", HarnessRole::Review => "reviewing" },
                    session.id, hash_token(capability_token), worktree.to_string_lossy(), base_revision, pinned_revision, timestamp,
                ],
            )?;
            st.conn.execute(
                "UPDATE sessions SET parent_session_id=?2,session_kind='topic_attempt',topic_id=?3,topic_attempt_id=?4,harness_kind=?5,cwd=?6 WHERE id=?1",
                params![session.id, topic.topic_session_id, topic_id, id, harness_text, worktree.to_string_lossy()],
            )?;
            st.conn.execute(
                "UPDATE topic_features SET status=?2,active_attempt_id=?3,detail=NULL,updated_at=?4 WHERE id=?1",
                params![feature_id, if role == HarnessRole::Build { "building" } else { "reviewing" }, id, timestamp],
            )?;
            st.conn.execute(
                "UPDATE topics SET updated_at=?2 WHERE id=?1",
                params![topic_id, timestamp],
            )?;
            topic_attempt(st, &id)?.ok_or_else(|| anyhow!("topic attempt vanished"))
        })
    }

    pub(crate) fn authenticate_topic_attempt(
        &self,
        attempt_id: &str,
        capability_token: &str,
    ) -> Result<TopicAttemptInfo> {
        let attempt =
            topic_attempt(self, attempt_id)?.ok_or_else(|| anyhow!("topic attempt not found"))?;
        let hash: String = self.conn.query_row(
            "SELECT capability_hash FROM topic_attempts WHERE id=?1",
            params![attempt_id],
            |row| row.get(0),
        )?;
        if hash != hash_token(capability_token) {
            bail!("invalid topic attempt capability");
        }
        Ok(attempt)
    }

    pub(crate) fn topic_attempt_paths(&self, attempt_id: &str) -> Result<(PathBuf, String)> {
        Ok(self.conn.query_row(
            "SELECT worktree_path,base_revision FROM topic_attempts WHERE id=?1",
            params![attempt_id],
            |row| Ok((PathBuf::from(row.get::<_, String>(0)?), row.get(1)?)),
        )?)
    }

    pub(crate) fn ready_for_topic_review(
        &self,
        attempt_id: &str,
        capability_token: &str,
        revision: &str,
        test_report: &str,
    ) -> Result<TopicAttemptInfo> {
        let attempt = self.authenticate_topic_attempt(attempt_id, capability_token)?;
        if attempt.role != HarnessRole::Build || attempt.status != TopicAttemptStatus::Building {
            bail!("only the active builder attempt can request review");
        }
        self.transaction(|st| {
            let timestamp = now();
            let changed = st.conn.execute(
                "UPDATE topic_attempts SET status='awaiting_review',revision=?2,test_report=?3,updated_at=?4 WHERE id=?1 AND status='building'",
                params![attempt_id, revision, test_report, timestamp],
            )?;
            if changed != 1 { bail!("builder attempt is no longer active"); }
            st.conn.execute(
                "UPDATE topic_features SET status='reviewing',ready_revision=?2,updated_at=?3 WHERE id=?1",
                params![attempt.feature_id, revision, timestamp],
            )?;
            topic_attempt(st, attempt_id)?.ok_or_else(|| anyhow!("topic attempt vanished"))
        })
    }

    pub(crate) fn submit_topic_review(
        &self,
        attempt_id: &str,
        capability_token: &str,
        revision: &str,
        outcome: &TopicReviewOutcome,
    ) -> Result<TopicAttemptInfo> {
        let attempt = self.authenticate_topic_attempt(attempt_id, capability_token)?;
        if attempt.role != HarnessRole::Review || attempt.status != TopicAttemptStatus::Reviewing {
            bail!("only the active reviewer attempt can submit a review");
        }
        if attempt.revision.as_deref() != Some(revision) {
            bail!("review revision does not match the pinned attempt revision");
        }
        self.transaction(|st| {
            let timestamp = now();
            let (feature_id,): (String,) = st.conn.query_row(
                "SELECT feature_id FROM topic_attempts WHERE id=?1",
                params![attempt_id],
                |row| Ok((row.get(0)?,)),
            )?;
            let (outcome_name, detail, attempt_status, feature_status) = match outcome {
                TopicReviewOutcome::Passed => ("passed", "", "passed", "ready"),
                TopicReviewOutcome::Findings { detail } => ("findings", detail.as_str(), "findings", "pending"),
                TopicReviewOutcome::NeedsUser { reason } => ("needs_user", reason.as_str(), "needs_user", "needs_user"),
            };
            st.conn.execute(
                "INSERT INTO topic_reviews(id,feature_id,attempt_id,revision,outcome,detail,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![uuid::Uuid::new_v4().to_string(), feature_id, attempt_id, revision, outcome_name, detail, timestamp],
            ).context("review for this feature revision already exists")?;
            let round: i64 = st.conn.query_row(
                "SELECT review_round FROM topic_features WHERE id=?1",
                params![feature_id],
                |row| row.get(0),
            )?;
            let next_round = if matches!(outcome, TopicReviewOutcome::Findings { .. }) { round + 1 } else { round };
            let bounded = next_round >= 3 && matches!(outcome, TopicReviewOutcome::Findings { .. });
            let feature_status = if bounded { "needs_user" } else { feature_status };
            let attempt_status = if bounded { "needs_user" } else { attempt_status };
            st.conn.execute(
                "UPDATE topic_attempts SET status=?2,detail=?3,updated_at=?4 WHERE id=?1",
                params![attempt_id, attempt_status, detail, timestamp],
            )?;
            st.conn.execute(
                "UPDATE topic_features SET status=?2,review_round=?3,active_attempt_id=NULL,detail=?4,updated_at=?5 WHERE id=?1",
                params![feature_id, feature_status, next_round, if bounded { "review rounds exhausted" } else { detail }, timestamp],
            )?;
            topic_attempt(st, attempt_id)?.ok_or_else(|| anyhow!("topic attempt vanished"))
        })
    }

    pub(crate) fn fail_topic_attempt(
        &self,
        attempt_id: &str,
        detail: &str,
    ) -> Result<TopicAttemptInfo> {
        self.transaction(|st| {
            let attempt = topic_attempt(st, attempt_id)?.ok_or_else(|| anyhow!("topic attempt not found"))?;
            st.conn.execute(
                "UPDATE topic_attempts SET status='failed',detail=?2,updated_at=?3 WHERE id=?1",
                params![attempt_id, detail, now()],
            )?;
            st.conn.execute(
                "UPDATE topic_features SET status='failed',active_attempt_id=NULL,detail=?2,updated_at=?3 WHERE id=?1",
                params![attempt.feature_id, detail, now()],
            )?;
            topic_attempt(st, attempt_id)?.ok_or_else(|| anyhow!("topic attempt vanished"))
        })
    }

    pub(crate) fn finish_topic_workflow(&self, topic_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.ok_or_else(|| anyhow!("topic not found"))?;
            let active: bool = st.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM topic_features WHERE topic_id=?1 AND status IN ('pending','building','reviewing'))",
                params![topic_id],
                |row| row.get(0),
            )?;
            if active { bail!("topic still has work that is not terminal"); }
            st.conn.execute("DELETE FROM topic_repo_locks WHERE repo=?1 AND topic_id=?2", params![topic.repo, topic_id])?;
            st.conn.execute("UPDATE topics SET status='complete',updated_at=?2 WHERE id=?1", params![topic_id, now()])?;
            topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"))
        })
    }

    pub fn recover_topic(&self, topic_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.ok_or_else(|| anyhow!("topic not found"))?;
            if topic.status != "running" {
                return topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"));
            }
            let timestamp = now();
            st.conn.execute(
                "UPDATE topic_attempts SET status='abandoned',detail='unknown child after restart; recovered explicitly',updated_at=?2 WHERE topic_id=?1 AND status IN ('building','awaiting_review','reviewing')",
                params![topic_id, timestamp],
            )?;
            st.conn.execute(
                "UPDATE topic_features SET status='pending',active_attempt_id=NULL,detail='recovered after an unknown child process',updated_at=?2 WHERE topic_id=?1 AND status IN ('building','reviewing')",
                params![topic_id, timestamp],
            )?;
            st.conn.execute("DELETE FROM topic_repo_locks WHERE topic_id=?1", params![topic_id])?;
            st.conn.execute("UPDATE topics SET status='approved',updated_at=?2 WHERE id=?1", params![topic_id, timestamp])?;
            topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"))
        })
    }

    pub fn retry_topic_feature(&self, topic_id: &str, feature_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.ok_or_else(|| anyhow!("topic not found"))?;
            let status: Option<String> = st.conn.query_row(
                "SELECT status FROM topic_features WHERE id=?1 AND topic_id=?2",
                params![feature_id, topic_id],
                |row| row.get(0),
            ).optional()?;
            match status.as_deref() {
                None => bail!("topic feature not found"),
                Some("failed" | "needs_user") => {},
                _ => bail!("only a failed or needs-user feature can be retried"),
            }
            let timestamp = now();
            st.conn.execute(
                "UPDATE topic_features SET status='pending',active_attempt_id=NULL,review_round=0,detail=NULL,updated_at=?3 WHERE id=?1 AND topic_id=?2",
                params![feature_id, topic_id, timestamp],
            )?;
            if topic.status == "running" {
                bail!("cannot retry a feature while its topic workflow is running; recover it first");
            }
            if topic.status == "complete" || topic.status == "failed" {
                st.conn.execute(
                    "DELETE FROM topic_repo_locks WHERE repo=?1 AND topic_id=?2",
                    params![topic.repo, topic_id],
                )?;
                st.conn.execute(
                    "UPDATE topics SET status='approved',updated_at=?2 WHERE id=?1",
                    params![topic_id, timestamp],
                )?;
            } else if topic.status != "approved" {
                bail!("topic must be approved or terminal before a feature can be retried");
            }
            topic_info(st, topic_id)?.ok_or_else(|| anyhow!("topic vanished"))
        })
    }
}

#[derive(Debug)]
struct TopicRow {
    id: String,
    name: String,
    repo: String,
    base_revision: String,
    source_session_id: String,
    topic_session_id: Option<String>,
    status: String,
    builder_kind: String,
    builder_model: Option<String>,
    reviewer_kind: String,
    reviewer_model: Option<String>,
    created_at: String,
    updated_at: String,
}

fn topic_row(st: &Store, id: &str) -> Result<Option<TopicRow>> {
    Ok(st.conn.query_row(
        "SELECT id,name,repo,base_revision,source_session_id,topic_session_id,status,builder_kind,builder_model,reviewer_kind,reviewer_model,created_at,updated_at FROM topics WHERE id=?1",
        params![id],
        |row| Ok(TopicRow {
            id: row.get(0)?, name: row.get(1)?, repo: row.get(2)?, base_revision: row.get(3)?,
            source_session_id: row.get(4)?, topic_session_id: row.get(5)?, status: row.get(6)?,
            builder_kind: row.get(7)?, builder_model: row.get(8)?, reviewer_kind: row.get(9)?,
            reviewer_model: row.get(10)?, created_at: row.get(11)?, updated_at: row.get(12)?,
        }),
    ).optional()?)
}

fn topic_info(st: &Store, id: &str) -> Result<Option<TopicInfo>> {
    let Some(row) = topic_row(st, id)? else {
        return Ok(None);
    };
    let feature_rows = st.conn.prepare(
        "SELECT id,source_message_id,source_seq,request,status,active_attempt_id,ready_revision,review_round,detail FROM topic_features WHERE topic_id=?1 ORDER BY source_seq,rowid",
    )?.query_map(params![id], |row| {
        Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?, row.get::<_, i64>(7)?, row.get::<_, Option<String>>(8)?,
        ))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    let features = feature_rows
        .into_iter()
        .map(
            |(
                id,
                source_message_id,
                source_seq,
                request,
                status,
                active_attempt_id,
                ready_revision,
                review_round,
                detail,
            )| {
                Ok(TopicFeatureInfo {
                    id,
                    source_message_id,
                    source_seq,
                    request,
                    status: db_enum(status, "topic feature status")?,
                    active_attempt_id,
                    ready_revision,
                    review_round: review_round as u32,
                    detail,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let attempts = st
        .conn
        .prepare("SELECT id FROM topic_attempts WHERE topic_id=?1 ORDER BY created_at,id")?
        .query_map(params![id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .map(|attempt_id| {
            topic_attempt(st, attempt_id)?.ok_or_else(|| anyhow!("topic attempt vanished"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(TopicInfo {
        id: row.id,
        name: row.name,
        repo: PathBuf::from(row.repo),
        base_revision: row.base_revision,
        source_session_id: row.source_session_id,
        topic_session_id: row.topic_session_id,
        status: serde_json::from_value(json!(row.status))
            .context("stored topic has invalid status")?,
        builder: serde_json::from_value(json!(row.builder_kind))
            .context("stored topic has invalid builder")?,
        builder_model: row.builder_model,
        reviewer: serde_json::from_value(json!(row.reviewer_kind))
            .context("stored topic has invalid reviewer")?,
        reviewer_model: row.reviewer_model,
        features,
        attempts,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

fn topic_attempt(st: &Store, id: &str) -> Result<Option<TopicAttemptInfo>> {
    let row = st.conn.query_row(
        "SELECT id,topic_id,feature_id,round,role,harness_kind,status,conversation_session_id,revision,harness_session_id,detail,created_at,updated_at FROM topic_attempts WHERE id=?1",
        params![id],
        |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            row.get::<_, String>(6)?, row.get::<_, String>(7)?, row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<String>>(9)?, row.get::<_, Option<String>>(10)?,
            row.get::<_, String>(11)?, row.get::<_, String>(12)?,
        )),
    ).optional()?;
    let Some((
        id,
        topic_id,
        feature_id,
        round,
        role,
        harness,
        status,
        conversation_session_id,
        revision,
        harness_session_id,
        detail,
        created_at,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(TopicAttemptInfo {
        id,
        topic_id,
        feature_id,
        round: round as u32,
        role: db_enum(role, "topic attempt role")?,
        harness: db_enum(harness, "topic attempt harness")?,
        status: db_enum(status, "topic attempt status")?,
        conversation_session_id,
        revision,
        harness_session_id,
        detail,
        created_at,
        updated_at,
    }))
}

fn db_enum<T: serde::de::DeserializeOwned>(value: String, label: &str) -> Result<T> {
    serde_json::from_value(json!(value)).with_context(|| format!("stored {label} is invalid"))
}

fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn harness_label(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "Claude",
        HarnessKind::Codex => "Codex",
        HarnessKind::Antigravity => "Antigravity",
    }
}
