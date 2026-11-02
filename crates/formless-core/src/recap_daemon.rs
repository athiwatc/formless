//! A recap describes one exact terminal Run. Capture it while its controls still
//! exclude a successor, and persist its actual live facts before announcing it.
use super::*;

impl Daemon {
    pub(super) fn run_recap(
        &self,
        session: &str,
        run: &str,
        changes: Option<&[ToolChange]>,
        files: &[RunFileInfo],
    ) -> Result<Option<RunRecap>> {
        self.with_reader(|store| {
            store.read_transaction(|store| {
                let facts: Option<crate::storage::RecapRunFacts> =
                    store.recap_run_facts(session, run)?;
                let Some(facts) = facts else {
                    return Ok(None);
                };
                if facts.steps < 4 {
                    return Ok(None);
                }
                let calls = store.run_calls(run)?;
                let terminal_calls = calls.iter().try_fold(0i64, |sum, (_, count, _)| {
                    sum.checked_add(*count).context("Recap call count overflow")
                })?;
                let unfinished_calls = store
                    .run_unfinished_calls(session, run)?
                    .context("Owned terminal Run facts changed during recap")?;
                let total_calls = terminal_calls
                    .checked_add(unfinished_calls)
                    .context("Recap total call count overflow")?;
                let steered = store
                    .run_steering_count(session, run)?
                    .context("Owned terminal Run steering bounds disappeared")?;
                let used = calls
                    .into_iter()
                    .map(|(name, count, failed)| {
                        let times = if count > 1 {
                            format!(" ×{count}")
                        } else {
                            String::new()
                        };
                        let failures = if failed > 0 {
                            format!(" ({failed} failed)")
                        } else {
                            String::new()
                        };
                        format!("{name}{times}{failures}")
                    })
                    .collect();
                Ok(Some(RunRecap {
                    status: facts.status.clone(),
                    total_calls: Some(total_calls),
                    unfinished_calls,
                    steps: facts.steps,
                    duration_ms: facts.duration_ms(),
                    used,
                    // These are the actual changes captured by this Run's execution.
                    // A new resumed Run never inherits another Run's build history.
                    built: changes
                        .unwrap_or_default()
                        .iter()
                        .map(|change| {
                            format!("{} {} v{}", change.change, change.name, change.version)
                        })
                        .collect(),
                    files: files
                        .iter()
                        .filter(|file| file.status != "unchanged")
                        .map(|file| format!("{} {}", file.status, file.path))
                        .collect(),
                    steered,
                    untracked_writes_possible: store.run_untracked_writes_possible(session, run)?,
                }))
            })
        })
    }

    pub(super) fn capture_terminal_recap(
        &self,
        session: &str,
        run: &str,
        changes: Option<&[ToolChange]>,
        files: &[RunFileInfo],
    ) -> Result<Option<RunRecap>> {
        let Some(recap) = self.run_recap(session, run, changes, files)? else {
            return Ok(None);
        };
        self.with_store(|store| store.record_run_recap(session, run, &recap))?;
        Ok(Some(recap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: std::path::PathBuf,
        daemon: Arc<Daemon>,
        session: String,
        run: String,
    }
    impl Fixture {
        fn new(env: &crate::credential_env::EnvGuard) -> Self {
            env.set("FORMLESS_API_KEY", "private-recap-fixture");
            env.remove("FORMLESS_BASE_URL");
            env.remove("FORMLESS_MODEL");
            let root = std::env::temp_dir().join(format!("fl-recap-{}", uuid::Uuid::new_v4()));
            let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
            let mut config = Config::default();
            config.provider.base_url = "http://127.0.0.1:9".into();
            config.provider.model = "private-unused-model".into();
            config.naming.enabled = false;
            config.skills.auto_create = false;
            config.save(&paths).unwrap();
            let daemon = Arc::new(Daemon::new(paths).unwrap());
            let (session, run) = daemon
                .with_store(|store| {
                    let session = store.create_session(None)?.id;
                    let run = store.create_run(&session)?;
                    for _ in 0..4 {
                        store.bump_run_steps(&run)?;
                    }
                    store.set_run_status(&run, "completed")?;
                    Ok((session, run))
                })
                .unwrap();
            Self {
                root,
                daemon,
                session,
                run,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.daemon.shutdown.cancel();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn recap_preserves_exact_status_and_actual_changes_as_an_immutable_snapshot() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let changes = vec![ToolChange {
            tool_id: "fixture-tool".into(),
            name: "created_here".into(),
            version: 2,
            change: "created".into(),
        }];
        let files = vec![RunFileInfo {
            path: "owned.txt".into(),
            status: "modified".into(),
            revertible: false,
            diff: None,
            kind: "file".into(),
            revert_reason: Some("fixture".into()),
        }];
        let first = f
            .daemon
            .capture_terminal_recap(&f.session, &f.run, Some(&changes), &files)
            .unwrap()
            .unwrap();
        assert_eq!(first.status, "completed");
        assert_eq!(first.total_calls, Some(0));
        assert_eq!(first.built, ["created created_here v2"]);
        assert_eq!(first.files, ["modified owned.txt"]);
        assert!(
            f.daemon
                .capture_terminal_recap(&f.session, &f.run, None, &[])
                .is_err(),
            "different historical data must not overwrite the stored terminal snapshot"
        );
        f.daemon
            .with_store(|store| store.set_run_status(&f.run, "resumed"))
            .unwrap();
        let recorded = f
            .daemon
            .with_reader(|store| store.recorded_run_recap(&f.session, &f.run))
            .unwrap()
            .unwrap();
        assert_eq!(recorded.recap, first);
        let new_run = f
            .daemon
            .with_store(|store| store.create_run(&f.session))
            .unwrap();
        assert_ne!(new_run, f.run);
        assert!(f
            .daemon
            .with_reader(|store| store.latest_recorded_run_recap(&f.session))
            .unwrap()
            .is_none());
    }

    #[test]
    fn terminal_status_is_explicit_and_ineligible_or_unowned_runs_have_no_recap() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        for status in ["completed", "paused", "interrupted", "failed", "abandoned"] {
            f.daemon
                .with_store(|store| store.set_run_status(&f.run, status))
                .unwrap();
            let recap = f
                .daemon
                .run_recap(&f.session, &f.run, None, &[])
                .unwrap()
                .unwrap();
            assert_eq!(recap.status, status);
        }
        assert!(f
            .daemon
            .run_recap("foreign", &f.run, None, &[])
            .unwrap()
            .is_none());
        assert!(f
            .daemon
            .run_recap(&f.session, "missing", None, &[])
            .unwrap()
            .is_none());
        let short = f
            .daemon
            .with_store(|store| {
                let run = store.create_run(&f.session)?;
                store.set_run_status(&run, "completed")?;
                Ok(run)
            })
            .unwrap();
        assert!(f
            .daemon
            .capture_terminal_recap(&f.session, &short, None, &[])
            .unwrap()
            .is_none());
        assert!(f
            .daemon
            .with_reader(|store| store.latest_recorded_run_recap(&f.session))
            .unwrap()
            .is_none());
    }

    #[test]
    fn unfinished_only_calls_persist_without_inventing_successes_or_later_rewrites() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let (tool, invocations) = f
            .daemon
            .with_store(|store| {
                let (tool, _) = store.create_tool_version(
                    "background",
                    "fixture",
                    "{}",
                    "{}",
                    "function run(){return {};}",
                    &Capabilities::default(),
                    256,
                    "agent",
                )?;
                let mut invocations = Vec::new();
                for status in ["running", "pending", "running", "pending", "running"] {
                    invocations.push(store.record_invocation(
                        &tool.id,
                        1,
                        Some(&f.session),
                        Some(&f.run),
                        Some("parent"),
                        status,
                        "",
                        "{}",
                        "",
                        "",
                        1,
                        200,
                    )?);
                }
                Ok((tool, invocations))
            })
            .unwrap();
        let first = f
            .daemon
            .capture_terminal_recap(&f.session, &f.run, None, &[])
            .unwrap()
            .unwrap();
        assert_eq!(first.total_calls, Some(5));
        assert_eq!(first.unfinished_calls, 5);
        assert!(
            first.used.is_empty(),
            "unfinished calls must not be listed as successful work"
        );
        f.daemon
            .with_store(|store| {
                for invocation in &invocations {
                    store.finish_invocation(
                        invocation, &tool.id, "success", "", "{}", "", 10, 200,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let later = f
            .daemon
            .run_recap(&f.session, &f.run, None, &[])
            .unwrap()
            .unwrap();
        assert_eq!(later.unfinished_calls, 0);
        assert!(!later.used.is_empty());
        assert!(f
            .daemon
            .capture_terminal_recap(&f.session, &f.run, None, &[])
            .is_err());
        let stored = f
            .daemon
            .with_reader(|store| store.recorded_run_recap(&f.session, &f.run))
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.recap, first,
            "background completion must not rewrite the terminal snapshot"
        );
    }

    #[tokio::test]
    async fn built_attribution_keeps_the_exact_create_result_after_another_runs_update() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let args = json!({"name":"interleaved","description":"fixture","input_schema":{},"output_schema":{},
            "implementation_js":"function run(){ return {}; }","capabilities":[],"memory_mb":256});
        let cfg = Config::load(&f.daemon.paths).unwrap();
        let (text, created) = f
            .daemon
            .create_tool_impl(&args, &cfg, &Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["version"], 1);
        assert_eq!(created.version, 1);
        let (other_run, updated) = f
            .daemon
            .with_store(|store| {
                let other = store.create_session(None)?.id;
                let run = store.create_run(&other)?;
                let (tool, version) = store.create_tool_version(
                    "interleaved",
                    "fixture",
                    "{}",
                    "{}",
                    "function run(){ return {}; }",
                    &Capabilities::default(),
                    256,
                    "agent",
                )?;
                Ok((
                    run,
                    ToolChange {
                        tool_id: tool.id,
                        name: tool.name,
                        version,
                        change: "updated".into(),
                    },
                ))
            })
            .unwrap();
        assert_eq!(updated.version, 2);
        f.daemon.note_tool_change(&other_run, updated);
        assert_eq!(
            f.daemon
                .with_reader(|store| Ok(store
                    .get_tool_current(&created.tool_id)?
                    .unwrap()
                    .0
                    .current_version))
                .unwrap(),
            2
        );
        // Deliberately notify the first Run after the competing Run committed.
        f.daemon.note_tool_change(&f.run, created);
        let changes = f
            .daemon
            .run_tool_changes
            .locked()
            .get(&f.run)
            .unwrap()
            .clone();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].version, changes[0].change.as_str()),
            (1, "created")
        );
        let recap = f
            .daemon
            .capture_terminal_recap(&f.session, &f.run, Some(&changes), &[])
            .unwrap()
            .unwrap();
        assert_eq!(recap.built, ["created interleaved v1"]);
        let stored = f
            .daemon
            .with_reader(|store| store.recorded_run_recap(&f.session, &f.run))
            .unwrap()
            .unwrap();
        assert_eq!(stored.recap.built, ["created interleaved v1"]);
    }

    #[test]
    fn own_out_of_order_notifications_keep_the_highest_attributed_version_and_creation() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let changes = f
            .daemon
            .with_store(|store| {
                let mut changes = Vec::new();
                for _ in 0..2 {
                    let (tool, version) = store.create_tool_version(
                        "ordered",
                        "fixture",
                        "{}",
                        "{}",
                        "function run(){ return {}; }",
                        &Capabilities::default(),
                        256,
                        "agent",
                    )?;
                    changes.push(ToolChange {
                        tool_id: tool.id,
                        name: tool.name,
                        version,
                        change: if version == 1 { "created" } else { "updated" }.into(),
                    });
                }
                Ok(changes)
            })
            .unwrap();
        for change in changes.into_iter().rev() {
            f.daemon.note_tool_change(&f.run, change);
        }
        let changes = f
            .daemon
            .run_tool_changes
            .locked()
            .get(&f.run)
            .unwrap()
            .clone();
        assert_eq!(
            (changes[0].version, changes[0].change.as_str()),
            (2, "created")
        );
    }
}
