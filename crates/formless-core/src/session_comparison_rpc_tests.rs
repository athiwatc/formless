use super::*;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn branch_rpc_is_local_write_free_and_preserves_typed_failures() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-comparison-fixture");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let root =
        Fixture(std::env::temp_dir().join(format!("fl-branch-rpc-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.tools.approval_summaries = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let parent = daemon
        .with_store(|st| {
            let parent = st.create_session(Some("Parent".into()))?;
            st.append_message(&parent.id, "user", "copied prompt")?;
            st.append_message(&parent.id, "assistant", "original answer")?;
            Ok(parent)
        })
        .unwrap();
    let fork = daemon
        .handle(Request::ForkSession {
            id: parent.id.clone(),
            name: Some("Child".into()),
            up_to_seq: Some(1),
        })
        .await
        .unwrap();
    let child: SessionInfo = serde_json::from_value(fork).unwrap();
    assert_eq!(child.forked_from.as_deref(), Some(parent.id.as_str()));
    assert_eq!(child.fork_point_seq, Some(1));
    let before = daemon
        .with_reader(|st| {
            Ok((
                st.list_sessions()?,
                st.list_messages(&parent.id)?,
                st.list_messages(&child.id)?,
                st.usage_rows(None, None)?,
            ))
        })
        .unwrap();
    let value = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        daemon.handle(Request::CompareSessions {
            left: parent.id.clone(),
            right: child.id.clone(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let comparison: formless_protocol::SessionComparison = serde_json::from_value(value).unwrap();
    assert_eq!(
        comparison.common_ancestor.as_deref(),
        Some(parent.id.as_str())
    );
    let after = daemon
        .with_reader(|st| {
            Ok((
                st.list_sessions()?,
                st.list_messages(&parent.id)?,
                st.list_messages(&child.id)?,
                st.usage_rows(None, None)?,
            ))
        })
        .unwrap();
    assert_eq!(
        before, after,
        "pure comparison cannot create Runs, charges, notes or Sessions"
    );
    assert!(
        matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),
        "no provider connection is permitted"
    );
    let error = daemon
        .handle(Request::CompareSessions {
            left: "missing-session".into(),
            right: child.id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::NOT_FOUND
    );
    daemon
        .with_store(|st| st.append_message(&parent.id, "assistant", &"x".repeat(2 * 1024 * 1024)))
        .unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        daemon.handle(Request::CompareSessions {
            left: parent.id,
            right: child.id,
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::INVALID
    );
    assert!(error.to_string().contains("No partial diff was sent"));
}

#[test]
fn comparison_attribution_tracks_the_displayed_answer_after_the_fork_prompt() {
    let message = |seq, role: &str, content: &str, model: &str| formless_protocol::MessageInfo {
        id: format!("m{seq}"),
        session_id: "fork".into(),
        seq,
        role: role.into(),
        generated_by: None,
        content: content.into(),
        created_at: String::new(),
        meta: Some(
            serde_json::json!({"model":{"name":model,"via":"fallback","requested":"auto"}})
                .to_string(),
        ),
    };
    let messages = vec![
        message(1, "assistant", "ancestor answer", "ancestor-model"),
        message(3, "assistant", "displayed answer", "answer-model"),
        message(4, "assistant", "", "later-tool-call-model"),
    ];
    let (answer, served_model) = Daemon::comparison_answer(&messages, 2);
    assert_eq!(answer, "displayed answer");
    assert_eq!(served_model.unwrap().name, "answer-model");

    let (answer, served_model) = Daemon::comparison_answer(&messages[..1], 2);
    assert!(answer.is_empty());
    assert!(served_model.is_none());
}
