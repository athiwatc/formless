use super::{Daemon, RequestFailure, Store, ToolDraft};
use crate::{config::Config, onboarding::TOUR_TOOL_NAME};
use anyhow::{Context, Result};
use formless_protocol::{Event, ToolChange, ToolInfo};
use formless_runtime::LockExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, Arc};

const NAMESPACE: &str = "harness:onboarding";
const SAMPLE_KEY: &str = "sample";
const TAG: &str = "guided-tour";
const SESSION_NAME: &str = "Guided tour";
const TOOL_TITLE: &str = "Guided tour word counter";

#[derive(Serialize, Deserialize)]
struct Sample {
    id: String,
    version: i64,
    created_in: String,
}

fn sample(st: &Store) -> Result<Option<(Sample, ToolInfo)>> {
    let Some(stored) = st.kv_get(NAMESPACE, SAMPLE_KEY)? else {
        return Ok(None);
    };
    let sample: Sample = serde_json::from_value(stored)?;
    let Some(tool) = st.tool_info(&sample.id)? else {
        return Ok(None);
    };
    Ok(Some((sample, tool)))
}

fn is_tour_session(st: &Store, id: &str) -> Result<bool> {
    Ok(st.kv_get(NAMESPACE, id)? == Some(Value::Bool(true)))
}

fn unchanged(sample: &Sample, tool: &ToolInfo) -> bool {
    tool.name == TOUR_TOOL_NAME
        && tool.current_version == sample.version
        && tool.title == TOOL_TITLE
        && tool.tags.iter().any(|tag| tag == TAG)
}

pub(super) fn validate_sample(draft: &ToolDraft) -> Result<()> {
    anyhow::ensure!(
        draft.name == TOUR_TOOL_NAME,
        "the guided tour creates only {TOUR_TOOL_NAME}"
    );
    anyhow::ensure!(
        draft.caps.to_specs().is_empty(),
        "the guided tour sample must have no capabilities"
    );
    Ok(())
}

pub(super) fn register_sample(st: &Store, tool: &ToolInfo, run_id: &str) -> Result<ToolInfo> {
    let mut tags = tool.tags.clone();
    if !tags.iter().any(|tag| tag == TAG) {
        tags.push(TAG.into());
        tags.sort();
    }
    st.set_tool_title(&tool.id, TOOL_TITLE)?;
    let tool = st
        .set_tool_tags(&tool.id, &tags)?
        .context("sample Tool disappeared")?;
    st.kv_set(
        NAMESPACE,
        SAMPLE_KEY,
        &json!(Sample {
            id: tool.id.clone(),
            version: tool.current_version,
            created_in: run_id.into(),
        }),
    )?;
    Ok(tool)
}

impl Daemon {
    pub(super) fn create_tour_session(&self) -> Result<Value> {
        let _admission = self.forecast_admission.locked();
        let (session, existing, restored) = self.with_store(|st| {
            st.transaction(|st| {
                let existing = sample(st)?;
                if let Some((record, tool)) = &existing {
                    anyhow::ensure!(
                        unchanged(record, tool) && !tool.disabled,
                        "the guided tour sample has been modified or disabled"
                    );
                } else {
                    anyhow::ensure!(
                        st.get_tool_by_name(TOUR_TOOL_NAME)?.is_none(),
                        "the guided tour sample name is already in use"
                    );
                }
                let s = st.create_session(Some(SESSION_NAME.into()))?;
                st.kv_set(NAMESPACE, &s.id, &Value::Bool(true))?;
                let session = st
                    .set_session_tags(&s.id, &[TAG.into()])?
                    .context("tour Session disappeared")?;
                let restored = if let Some((_, tool)) = &existing {
                    let (tool, before) = Self::set_registry_retired(st, &tool.id, false)?;
                    st.load_session_tools(&s.id, std::slice::from_ref(&tool.id), "created")?;
                    before
                        .map(|version| {
                            st.audit("user", "tool.restore", &tool.name)?;
                            let change = Self::record_registry_change(
                                st, &tool, "restored", version, "user",
                            )?;
                            Ok::<_, anyhow::Error>((tool, change))
                        })
                        .transpose()?
                } else {
                    None
                };
                Ok((session, existing.is_some(), restored))
            })
        })?;
        if let Some((tool, change)) = restored {
            self.broadcast(Event::RegistryChanged { change });
            self.broadcast(Event::ToolUpdated { tool });
        }
        Ok(json!({"session":session,"sample_exists":existing}))
    }

    pub(super) async fn create_agent_tool(
        self: &Arc<Self>,
        session_id: &str,
        run_id: &str,
        args: &Value,
        cfg: &Config,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(String, ToolChange)> {
        if self.with_reader(|st| is_tour_session(st, session_id))? {
            self.create_tool_for_run(args, cfg, cancel, Some(run_id))
                .await
        } else {
            self.create_tool_impl(args, cfg, cancel).await
        }
    }

    pub(super) fn get_tour_run(&self, session_id: &str, run_id: &str) -> Result<Value> {
        self.with_reader(|st| {
            st.read_transaction(|st| {
                if !is_tour_session(st, session_id)? {
                    return Err(RequestFailure::not_found("tour Session not found").into());
                }
                let run = st
                    .inspect_exact_run(run_id)?
                    .filter(|run| run["session_id"] == session_id)
                    .context(RequestFailure::not_found("tour Run not found"))?;
                let (created, used) = match sample(st)? {
                    Some((sample, _)) => (
                        sample.created_in == run_id,
                        st.run_used_tool(run_id, &sample.id)?,
                    ),
                    None => (false, false),
                };
                Ok(json!({"status":run["status"], "created":created, "used":used}))
            })
        })
    }

    pub(super) fn cleanup_tour(&self) -> Result<Value> {
        let _admission = self.forecast_admission.locked();
        let sessions = self.with_reader(|st| {
            let mut tours = Vec::new();
            for s in st.list_sessions()? {
                if is_tour_session(st, &s.id)? {
                    tours.push(s);
                }
            }
            Ok(tours)
        })?;
        if sessions
            .iter()
            .any(|s| self.runs.locked().contains_key(&s.id) || self.session_is_publishing(&s.id))
        {
            return Err(RequestFailure::busy(
                "wait for the guided tour Run to end before cleaning up",
            )
            .into());
        }
        let (archived, retired, kept) = self.with_store(|st| {
            st.transaction(|st| {
                let mut archived = Vec::new();
                for s in &sessions {
                    if !s.archived {
                        if let Some(s) = st.set_session_archived(&s.id, true)? {
                            archived.push(s);
                        }
                    }
                    st.kv_delete(NAMESPACE, &s.id)?;
                }
                let Some((record, tool)) = sample(st)? else {
                    return Ok((archived, None, false));
                };
                if !unchanged(&record, &tool) || tool.pinned {
                    return Ok((archived, None, true));
                }
                let (tool, before) = Self::set_registry_retired(st, &tool.id, true)?;
                let change = before
                    .map(|version| {
                        st.audit("user", "tool.retire", &tool.name)?;
                        Self::record_registry_change(st, &tool, "retired", version, "user")
                    })
                    .transpose()?;
                let retired = change.map(|change| (tool, change));
                Ok((archived, retired, false))
            })
        })?;
        let count = archived.len();
        for session in archived {
            self.broadcast(Event::SessionUpdated { session });
        }
        let did_retire = retired.is_some();
        if let Some((tool, change)) = retired {
            self.broadcast(Event::RegistryChanged { change });
            self.broadcast(Event::ToolUpdated { tool });
        }
        Ok(json!({"archived":count, "retired_sample":did_retire, "kept_sample":kept}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_protocol::{Request, SessionInfo};

    fn fixture() -> (Arc<Daemon>, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("fl-tour-{}", uuid::Uuid::new_v4()));
        let paths = crate::Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        (Arc::new(Daemon::new(paths).unwrap()), root)
    }

    fn draft() -> Value {
        json!({
            "name": TOUR_TOOL_NAME,
            "description": "Count the number of words in text.",
            "input_schema": {"type":"object"},
            "output_schema": {"type":"object"},
            "implementation_js": "function run(input) { return {words: (input.text || '').trim().split(/\\s+/).filter(Boolean).length}; }",
            "capabilities": []
        })
    }

    fn create_run(daemon: &Daemon, session: &str) -> String {
        daemon.with_store(|st| st.create_run(session)).unwrap()
    }

    #[tokio::test]
    async fn guided_tour_reuses_and_retires_only_its_owned_sample() {
        let (daemon, root) = fixture();
        let first = daemon.handle(Request::CreateTourSession).await.unwrap();
        let session: SessionInfo = serde_json::from_value(first["session"].clone()).unwrap();
        assert_eq!(session.name, SESSION_NAME);
        assert_eq!(session.tags, [TAG]);
        assert_eq!(first["sample_exists"], false);

        let run_id = create_run(&daemon, &session.id);
        let cfg = daemon.config().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut wrong = draft();
        wrong["name"] = json!("some_other_tool");
        assert!(daemon
            .create_agent_tool(&session.id, &run_id, &wrong, &cfg, &cancel)
            .await
            .is_err());
        let (_, created) = daemon
            .create_agent_tool(&session.id, &run_id, &draft(), &cfg, &cancel)
            .await
            .unwrap();
        let tool = daemon
            .with_reader(|st| st.tool_info(&created.tool_id))
            .unwrap()
            .unwrap();
        assert_eq!(tool.title, TOOL_TITLE);
        assert_eq!(tool.tags, [TAG]);
        assert!(daemon
            .create_agent_tool(&session.id, &run_id, &draft(), &cfg, &cancel)
            .await
            .is_err());

        let repeat = daemon.handle(Request::CreateTourSession).await.unwrap();
        let repeat_id = repeat["session"]["id"].as_str().unwrap();
        assert_ne!(repeat_id, session.id);
        assert_eq!(repeat["sample_exists"], true);
        let other_run = create_run(&daemon, repeat_id);
        daemon
            .with_store(|st| {
                st.record_invocation(
                    &tool.id,
                    1,
                    Some(repeat_id),
                    Some(&other_run),
                    None,
                    "success",
                    "",
                    "{}",
                    "{}",
                    "",
                    0,
                    20,
                )?;
                st.set_run_status(&other_run, "completed")
            })
            .unwrap();
        assert!(daemon.get_tour_run(&session.id, &other_run).is_err());
        assert_eq!(
            daemon.get_tour_run(&session.id, &run_id).unwrap(),
            json!({"status":"running", "created":true, "used":false})
        );
        assert_eq!(
            daemon.get_tour_run(repeat_id, &other_run).unwrap(),
            json!({"status":"completed", "created":false, "used":true})
        );

        let normal = daemon
            .handle(Request::CreateSession {
                name: Some(SESSION_NAME.into()),
            })
            .await
            .unwrap();
        let normal_id = normal["id"].as_str().unwrap();
        daemon
            .with_store(|st| st.set_session_tags(normal_id, &[TAG.into()]))
            .unwrap();
        let cleaned = daemon.handle(Request::CleanupTour).await.unwrap();
        assert_eq!(
            cleaned,
            json!({"archived":2, "retired_sample":true, "kept_sample":false})
        );
        assert!(
            !daemon
                .with_reader(|st| st.get_session(normal_id))
                .unwrap()
                .unwrap()
                .archived
        );
        assert_eq!(
            daemon.handle(Request::CleanupTour).await.unwrap()["archived"],
            0
        );
        let restored = daemon.handle(Request::CreateTourSession).await.unwrap();
        assert_eq!(restored["sample_exists"], true);
        let current = daemon
            .with_reader(|st| st.tool_info(&tool.id))
            .unwrap()
            .unwrap();
        assert_eq!(current.current_version, 1);
        assert!(!current.retired);
        drop(daemon);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn tour_does_not_adopt_or_retire_an_unrelated_tool_with_the_sample_name() {
        let (daemon, root) = fixture();
        let mut args = draft();
        args["title"] = json!(TOOL_TITLE);
        args["tags"] = json!([TAG]);
        let (tool, _, _, _) = daemon
            .create_tool_as(&args, &daemon.config().unwrap(), "user", None)
            .await
            .unwrap();
        assert!(daemon.handle(Request::CreateTourSession).await.is_err());
        assert_eq!(
            daemon.handle(Request::CleanupTour).await.unwrap()["retired_sample"],
            false
        );
        assert!(
            !daemon
                .with_reader(|st| st.tool_info(&tool.id))
                .unwrap()
                .unwrap()
                .retired
        );
        drop(daemon);
        std::fs::remove_dir_all(root).unwrap();
    }
}
