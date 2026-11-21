use super::*;
use anyhow::{anyhow, bail, Context};
use formless_protocol::{
    HarnessKind, StepAccess, TopicArtifact, TopicAttemptStatus, TopicFeatureProposal,
    TopicProposalRequest, WorkspaceKind,
};
use rusqlite::params;
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

impl Store {
    pub(crate) fn pause_topic_attempt(&self, id: &str, token: &str) -> Result<TopicAttemptInfo> {
        self.transaction(|st| {
            let attempt=st.authenticate_topic_attempt(id,token)?;
            if attempt.status!=TopicAttemptStatus::Running {bail!("attempt already has a pending interaction");}
            st.conn.execute("UPDATE topic_attempts SET status='needs_user',updated_at=?2 WHERE id=?1",params![id,now()])?;
            st.conn.execute("UPDATE topic_features SET status='needs_user',updated_at=?2 WHERE active_attempt_id=?1",params![id,now()])?;
            topic_attempt(st,id)?.context("attempt vanished")
        })
    }

    pub(crate) fn resume_topic_attempt(&self, id: &str) -> Result<bool> {
        self.transaction(|st| {
            let changed=st.conn.execute("UPDATE topic_attempts SET status='running',updated_at=?2 WHERE id=?1 AND status='needs_user' AND EXISTS(SELECT 1 FROM topic_features f JOIN topics t ON t.id=f.topic_id WHERE f.active_attempt_id=?1 AND f.status='needs_user' AND t.status='running')",params![id,now()])?;
            if changed>0 {st.conn.execute("UPDATE topic_features SET status='running',updated_at=?2 WHERE active_attempt_id=?1",params![id,now()])?;}
            Ok(changed>0)
        })
    }
    pub(crate) fn ensure_topic_chat_writable(&self, id: &str) -> Result<()> {
        let kind: Option<String> = self
            .conn
            .query_row("SELECT session_kind FROM sessions WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if kind.as_deref() == Some("topic_attempt") {
            bail!("attempt conversations are read-only; send messages in the Topic Chat instead");
        }
        Ok(())
    }

    pub(crate) fn read_topic_input_file(
        &self,
        id: &str,
        path: &Path,
        sha256: &str,
    ) -> Result<Vec<u8>> {
        let cwd: Option<String> = self.conn.query_row(
            "SELECT worktree_path FROM topic_attempts WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        let cwd = PathBuf::from(cwd.context("attempt has no workspace")?).canonicalize()?;
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
        .canonicalize()?;
        let private = self
            .artifact_root
            .as_ref()
            .and_then(|root| root.parent())
            .map(|p| p.join("topic-attempts"));
        let in_private = private
            .and_then(|p| p.canonicalize().ok())
            .is_some_and(|p| path.starts_with(p));
        if !path.starts_with(&cwd) && !in_private {
            bail!("input artifact is outside the attempt workspace and private outputs");
        }
        if std::fs::metadata(&path)?.len() > 16 * 1024 * 1024 {
            bail!("input artifact exceeds 16 MiB");
        }
        let bytes = std::fs::read(path)?;
        if hash_bytes(&bytes) != sha256 {
            bail!("input artifact SHA-256 does not match");
        }
        Ok(bytes)
    }

    pub(crate) fn topic_artifact_access(
        &self,
        id: &str,
        scope: &str,
        seq: Option<i64>,
        owner: Option<&str>,
    ) -> Result<bool> {
        crate::attachment_access::topic_provenance(&self.conn, id, scope, seq, owner)
    }

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
        workspace_kind: WorkspaceKind,
        base_revision: Option<&str>,
    ) -> Result<TopicInfo> {
        self.ensure_topic_chat_writable(&proposal.source_session_id)?;
        if proposal.name.trim().is_empty() {
            bail!("topic name must not be empty");
        }
        if proposal.message_ids.is_empty() {
            bail!("select at least one user message");
        }
        if proposal
            .workspace
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            bail!("topic workspace path must be absolute");
        }
        if proposal.outline.len() > 20
            || proposal
                .outline
                .iter()
                .any(|step| step.title.trim().is_empty())
        {
            bail!("topic outline must contain at most 20 steps with nonblank titles");
        }
        if (workspace_kind == WorkspaceKind::Scratch) != proposal.workspace.is_none()
            || (workspace_kind == WorkspaceKind::Git) != base_revision.is_some()
        {
            bail!("workspace kind, path and base revision are inconsistent");
        }
        for step in &proposal.outline {
            crate::local_harness::validate_acp_agent(step.harness, step.acp_agent.as_deref())?;
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

            let parent_topic_id = match &proposal.parent_topic_id {
                Some(id) => {
                    let parent=topic_info(st,id)?.context("parent topic not found")?;
                    if matches!(parent.status,formless_protocol::TopicStatus::PendingApproval|formless_protocol::TopicStatus::Rejected) {
                        bail!("parent topic must be approved before it can have children");
                    }
                    Some(id.clone())
                }
                None => st.conn.query_row("SELECT id FROM topics WHERE topic_session_id=?1",[&proposal.source_session_id],|r|r.get::<_,String>(0)).optional()?,
            };
            let mut cursor=parent_topic_id.clone();
            let mut ancestors=HashSet::new();
            while let Some(parent)=cursor {
                if !ancestors.insert(parent.clone()) {bail!("topic hierarchy contains a cycle");}
                cursor=st.conn.query_row("SELECT parent_topic_id FROM topics WHERE id=?1",[parent],|r|r.get(0))?;
            }
            let id = uuid::Uuid::new_v4().to_string();
            let timestamp = now();
            st.conn.execute(
                "INSERT INTO topics(id,name,parent_topic_id,workspace,workspace_kind,base_revision,outline,source_session_id,status,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending_approval',?9,?9)",
                params![id, proposal.name.trim(), parent_topic_id, proposal.workspace.as_ref().map(|p|p.to_string_lossy().into_owned()),
                    enum_text(workspace_kind)?, base_revision, serde_json::to_string(&proposal.outline)?,
                    proposal.source_session_id, timestamp],
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
            if topic.topic_session_id.is_some() {
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
            let workspace = match &topic.workspace {
                Some(path) => PathBuf::from(path),
                None => {
                    let data = st.artifact_root.as_ref().and_then(|root| root.parent())
                        .context("scratch approval requires a profile-backed Store")?;
                    let path = data.join("topic-workspaces").join(topic_id);
                    let mut builder = std::fs::DirBuilder::new();
                    builder.recursive(true);
                    #[cfg(unix)]
                    { builder.mode(0o700); }
                    builder.create(&path)?;
                    st.conn.execute("UPDATE topics SET workspace=?2 WHERE id=?1", params![topic_id, path.to_string_lossy()])?;
                    path
                }
            };
            let parent_session=match &topic.parent_topic_id {
                Some(parent)=>topic_row(st,parent)?.context("parent topic not found")?.topic_session_id.context("parent topic has no conversation")?,
                None=>topic.source_session_id.clone(),
            };
            let session = st.create_session(Some(topic.name.clone()))?;
            let timestamp = now();
            st.conn.execute(
                "UPDATE sessions SET parent_session_id=?2, session_kind='topic', topic_id=?3, cwd=?4
                 WHERE id=?1",
                params![
                    session.id,
                    parent_session,
                    topic.id,
                    workspace.to_string_lossy(),
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

    pub fn cancel_topic(&self, topic_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.context("topic not found")?;
            match topic.status.as_str() {
                "cancelled" => return topic_info(st, topic_id)?.context("topic vanished"),
                "approved" | "running" | "needs_user" => {},
                "pending_approval" => bail!("pending topics must be rejected, not cancelled"),
                _ => bail!("only an approved, running or needs-user topic can be cancelled"),
            }
            let timestamp = now();
            st.conn.execute("UPDATE topic_attempts SET status='cancelled',detail='topic cancelled',updated_at=?2 WHERE topic_id=?1 AND status IN ('running','needs_user')", params![topic_id,timestamp])?;
            st.conn.execute("UPDATE topic_features SET status='cancelled',active_attempt_id=NULL,detail='topic cancelled',updated_at=?2 WHERE topic_id=?1 AND status IN ('pending','running','needs_user')", params![topic_id,timestamp])?;
            st.conn.execute("DELETE FROM topic_repo_locks WHERE topic_id=?1", [topic_id])?;
            st.conn.execute("UPDATE topics SET status='cancelled',updated_at=?2 WHERE id=?1", params![topic_id,timestamp])?;
            topic_info(st, topic_id)?.context("topic vanished")
        })
    }

    pub(crate) fn claim_topic_workflow(&self, topic_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.context("topic not found")?;
            if topic.status != "approved" {
                bail!(
                    "topic must be approved and not already running before its workflow can start"
                );
            }
            let workspace = topic.workspace.context("approved topic has no workspace")?;
            st.conn
                .execute(
                    "INSERT INTO topic_repo_locks(workspace,topic_id,acquired_at) VALUES(?1,?2,?3)",
                    params![workspace, topic_id, now()],
                )
                .context("workspace already has an active topic workflow")?;
            st.conn.execute(
                "UPDATE topics SET status='running',updated_at=?2 WHERE id=?1",
                params![topic_id, now()],
            )?;
            topic_info(st, topic_id)?.context("topic vanished")
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_topic_attempt(
        &self,
        topic_id: &str,
        feature_id: &str,
        step_title: &str,
        access: StepAccess,
        harness: HarnessKind,
        model: Option<&str>,
        acp_agent: Option<&str>,
        inputs: &[TopicArtifact],
        capability_token: &str,
        cwd: &Path,
    ) -> Result<TopicAttemptInfo> {
        crate::local_harness::validate_acp_agent(Some(harness), acp_agent)?;
        if capability_token.len() < 32 {
            bail!("topic attempt capability token is too short");
        }
        if step_title.trim().is_empty() {
            bail!("step title must not be blank");
        }
        self.transaction(|st| {
            let topic = topic_row(st, topic_id)?.context("topic not found")?;
            if topic.status != "running" { bail!("topic must be running to launch an attempt"); }
            let (status, count, active): (String, u32, Option<String>) = st.conn.query_row(
                "SELECT status,attempt_count,active_attempt_id FROM topic_features WHERE topic_id=?1 AND id=?2",
                params![topic_id,feature_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if status != "pending" || active.is_some() { bail!("feature is not pending or already has an active attempt"); }
            if count >= 12 { bail!("work item reached the 12-attempt limit"); }
            let id = uuid::Uuid::new_v4().to_string();
            let session = st.create_session(Some(format!("{}: {}", harness_label(harness), step_title.trim())))?;
            let outputs_dir = st.artifact_root.as_ref().and_then(|root|root.parent())
                .map(|data|data.join("topic-attempts").join(&id).join("outputs"));
            if let Some(dir) = &outputs_dir {
                let mut builder=std::fs::DirBuilder::new(); builder.recursive(true);
                #[cfg(unix)] {builder.mode(0o700);}
                builder.create(dir)?;
            }
            let timestamp=now();
            st.conn.execute("INSERT INTO topic_attempts(id,topic_id,feature_id,step_title,access,harness_kind,model,acp_agent,inputs,status,conversation_session_id,capability_hash,worktree_path,outputs_dir,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'running',?10,?11,?12,?13,?14,?14)",
                params![id,topic_id,feature_id,step_title.trim(),enum_text(access)?,enum_text(harness)?,model,acp_agent,serde_json::to_string(inputs)?,session.id,hash_token(capability_token),cwd.to_string_lossy(),outputs_dir.as_ref().map(|p|p.to_string_lossy().into_owned()),timestamp])?;
            st.conn.execute("UPDATE sessions SET parent_session_id=?2,session_kind='topic_attempt',topic_id=?3,topic_attempt_id=?4,harness_kind=?5,cwd=?6 WHERE id=?1",
                params![session.id,topic.topic_session_id,topic_id,id,enum_text(harness)?,cwd.to_string_lossy()])?;
            st.conn.execute("UPDATE topic_features SET status='running',attempt_count=attempt_count+1,active_attempt_id=?2,detail=NULL,updated_at=?3 WHERE id=?1", params![feature_id,id,timestamp])?;
            topic_attempt(st,&id)?.context("attempt vanished")
        })
    }

    pub(crate) fn authenticate_topic_attempt(
        &self,
        id: &str,
        token: &str,
    ) -> Result<TopicAttemptInfo> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT capability_hash FROM topic_attempts WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if stored.as_deref() != Some(hash_token(token).as_str()) {
            bail!("invalid topic attempt capability");
        }
        let attempt = topic_attempt(self, id)?.context("attempt not found")?;
        let active: bool=self.conn.query_row("SELECT EXISTS(SELECT 1 FROM topic_features f JOIN topics t ON t.id=f.topic_id WHERE f.active_attempt_id=?1 AND f.status IN ('running','needs_user') AND t.status='running')", [id], |r|r.get(0))?;
        if !matches!(
            attempt.status,
            TopicAttemptStatus::Running | TopicAttemptStatus::NeedsUser
        ) || !active
        {
            bail!("topic attempt is no longer active");
        }
        Ok(attempt)
    }

    pub(crate) fn report_topic_result(
        &self,
        id: &str,
        token: &str,
        status: TopicAttemptStatus,
        summary: &str,
        outputs: &[TopicArtifact],
    ) -> Result<TopicAttemptInfo> {
        if !matches!(
            status,
            TopicAttemptStatus::Succeeded
                | TopicAttemptStatus::Failed
                | TopicAttemptStatus::NeedsUser
        ) {
            bail!("result status must be succeeded, failed or needs_user");
        }
        self.transaction(|st| {
            let attempt=st.authenticate_topic_attempt(id,token)?;
            let mut outputs=outputs.to_vec();
            for artifact in &mut outputs {
                if let TopicArtifact::File {path,sha256} = artifact {
                    let root: Option<String> = st.conn.query_row("SELECT outputs_dir FROM topic_attempts WHERE id=?1", [id], |r|r.get(0))?;
                    let root=PathBuf::from(root.context("attempt has no private outputs directory")?).canonicalize()?;
                    let file=if path.is_absolute(){path.clone()}else{root.join(&*path)}.canonicalize()?;
                    if !file.starts_with(&root) || !file.is_file() {bail!("file output must be inside this attempt's private outputs directory");}
                    if hash_bytes(&std::fs::read(&file)?) != *sha256 {bail!("file output SHA-256 does not match");}
                    *path=file;
                }
                if let TopicArtifact::GitRevision {revision} = artifact {
                    if revision.len()!=40 || !revision.bytes().all(|b|b.is_ascii_hexdigit()) {bail!("Git revision must be a full 40-character SHA");}
                }
            }
            let timestamp=now();
            st.conn.execute("UPDATE topic_attempts SET status=?2,summary=?3,outputs=?4,updated_at=?5 WHERE id=?1",
                params![id,enum_text(status)?,summary,serde_json::to_string(&outputs)?,timestamp])?;
            st.append_message(&attempt.conversation_session_id,"assistant",summary)?;
            st.conn.execute("UPDATE topic_features SET status=?2,active_attempt_id=NULL,updated_at=?3 WHERE id=?1",
                params![attempt.feature_id,if status==TopicAttemptStatus::NeedsUser {"needs_user"}else{"pending"},timestamp])?;
            if status==TopicAttemptStatus::NeedsUser {st.conn.execute("UPDATE topics SET status='needs_user',updated_at=?2 WHERE id=?1", params![attempt.topic_id,timestamp])?;}
            topic_attempt(st,id)?.context("attempt vanished")
        })
    }

    pub(crate) fn fail_topic_workflow(&self, topic_id: &str, detail: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic=topic_row(st,topic_id)?.context("topic not found")?;
            if topic.status=="running" || topic.status=="needs_user" {
                st.conn.execute("UPDATE topic_attempts SET status='failed',detail=?2,updated_at=?3 WHERE topic_id=?1 AND status IN ('running','needs_user')", params![topic_id,detail,now()])?;
                st.conn.execute("UPDATE topic_features SET status='failed',active_attempt_id=NULL,detail=?2,updated_at=?3 WHERE topic_id=?1 AND status IN ('pending','running','needs_user')", params![topic_id,detail,now()])?;
                st.conn.execute("DELETE FROM topic_repo_locks WHERE topic_id=?1",[topic_id])?;
                st.conn.execute("UPDATE topics SET status='failed',updated_at=?2 WHERE id=?1",params![topic_id,now()])?;
            }
            topic_info(st,topic_id)?.context("topic vanished")
        })
    }

    pub fn recover_topic(&self, topic_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            st.fail_topic_workflow(topic_id,"unknown child after recovery; retry explicitly")?;
            st.conn.execute("UPDATE topic_attempts SET status='abandoned' WHERE topic_id=?1 AND status='failed' AND detail='unknown child after recovery; retry explicitly'",[topic_id])?;
            topic_info(st,topic_id)?.context("topic vanished")
        })
    }

    pub fn retry_topic_feature(&self, topic_id: &str, feature_id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let topic=topic_row(st,topic_id)?.context("topic not found")?;
            if !matches!(topic.status.as_str(),"approved"|"failed"|"complete"|"needs_user"|"cancelled") {bail!("topic must be approved or terminal before retry; recover a running topic first");}
            let changed=st.conn.execute("UPDATE topic_features SET status='pending',active_attempt_id=NULL,outputs='[]',detail=NULL,updated_at=?3 WHERE topic_id=?1 AND id=?2",params![topic_id,feature_id,now()])?;
            if changed!=1 {bail!("topic feature not found");}
            st.conn.execute("DELETE FROM topic_repo_locks WHERE topic_id=?1",[topic_id])?;
            st.conn.execute("UPDATE topics SET status='approved',updated_at=?2 WHERE id=?1",params![topic_id,now()])?;
            topic_info(st,topic_id)?.context("topic vanished")
        })
    }
}

#[derive(Debug)]
struct TopicRow {
    id: String,
    parent_topic_id: Option<String>,
    name: String,
    workspace: Option<String>,
    workspace_kind: String,
    base_revision: Option<String>,
    outline: String,
    source_session_id: String,
    topic_session_id: Option<String>,
    status: String,
    created_at: String,
    updated_at: String,
}

fn topic_row(st: &Store, id: &str) -> Result<Option<TopicRow>> {
    Ok(st.conn.query_row("SELECT id,name,workspace,workspace_kind,base_revision,outline,source_session_id,topic_session_id,status,created_at,updated_at,parent_topic_id FROM topics WHERE id=?1",[id],|r|Ok(TopicRow{
        id:r.get(0)?,name:r.get(1)?,workspace:r.get(2)?,workspace_kind:r.get(3)?,base_revision:r.get(4)?,outline:r.get(5)?,source_session_id:r.get(6)?,topic_session_id:r.get(7)?,status:r.get(8)?,created_at:r.get(9)?,updated_at:r.get(10)?,parent_topic_id:r.get(11)?,
    })).optional()?)
}

fn topic_info(st: &Store, id: &str) -> Result<Option<TopicInfo>> {
    let Some(row) = topic_row(st, id)? else {
        return Ok(None);
    };
    let mut statement=st.conn.prepare("SELECT id,source_message_id,source_seq,request,status,active_attempt_id,outputs,attempt_count,detail FROM topic_features WHERE topic_id=?1 ORDER BY source_seq,rowid")?;
    let raw = statement
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, u32>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let features = raw
        .into_iter()
        .map(
            |(
                id,
                source_message_id,
                source_seq,
                request,
                status,
                active_attempt_id,
                outputs,
                attempt_count,
                detail,
            )| {
                Ok(TopicFeatureInfo {
                    id,
                    source_message_id,
                    source_seq,
                    request,
                    status: db_enum(status, "feature status")?,
                    active_attempt_id,
                    outputs: serde_json::from_str(&outputs)?,
                    attempt_count,
                    detail,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(TopicInfo {
        id: row.id,
        parent_topic_id: row.parent_topic_id,
        name: row.name,
        workspace: row.workspace.map(PathBuf::from),
        workspace_kind: db_enum(row.workspace_kind, "workspace kind")?,
        base_revision: row.base_revision,
        outline: serde_json::from_str(&row.outline)?,
        source_session_id: row.source_session_id,
        topic_session_id: row.topic_session_id,
        status: db_enum(row.status, "topic status")?,
        features,
        attempts: st.list_topic_attempts(id)?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

fn topic_attempt(st: &Store, id: &str) -> Result<Option<TopicAttemptInfo>> {
    let row=st.conn.query_row("SELECT topic_id,feature_id,step_title,access,harness_kind,model,inputs,outputs,summary,status,conversation_session_id,harness_session_id,detail,created_at,updated_at,acp_agent FROM topic_attempts WHERE id=?1",[id],|r|Ok((
        r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,String>(9)?,r.get::<_,String>(10)?,r.get::<_,Option<String>>(11)?,r.get::<_,Option<String>>(12)?,r.get::<_,String>(13)?,r.get::<_,String>(14)?,r.get::<_,Option<String>>(15)?
    ))).optional()?;
    let Some((
        topic_id,
        feature_id,
        step_title,
        access,
        harness,
        model,
        inputs,
        outputs,
        summary,
        status,
        conversation_session_id,
        harness_session_id,
        detail,
        created_at,
        updated_at,
        acp_agent,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(TopicAttemptInfo {
        id: id.into(),
        topic_id,
        feature_id,
        step_title,
        access: db_enum(access, "step access")?,
        harness: db_enum(harness, "harness")?,
        acp_agent,
        model,
        inputs: serde_json::from_str(&inputs)?,
        outputs: serde_json::from_str(&outputs)?,
        summary,
        status: db_enum(status, "attempt status")?,
        conversation_session_id,
        harness_session_id,
        detail,
        created_at,
        updated_at,
    }))
}

fn db_enum<T: serde::de::DeserializeOwned>(value: String, label: &str) -> Result<T> {
    serde_json::from_value(json!(value)).with_context(|| format!("stored {label} is invalid"))
}
fn enum_text<T: serde::Serialize>(value: T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .context("enum is not a string")
}
fn hash_token(token: &str) -> String {
    hash_bytes(token.as_bytes())
}
fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn harness_label(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "Claude",
        HarnessKind::Codex => "Codex",
        HarnessKind::Pi => "Pi",
        HarnessKind::OpenCode => "OpenCode",
        HarnessKind::Antigravity => "Antigravity",
        HarnessKind::Acp => "ACP",
        HarnessKind::Muse => "Muse",
    }
}
