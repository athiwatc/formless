//! Role deletion guards through both management entry points.
use super::*;
use formless_protocol::TemplateInfo;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn delete(d: &Arc<Daemon>, harness: bool) -> Result<Value> {
    if harness {
        d.harness_apply("roles.delete", &json!({"name":"primary"}))
            .await
    } else {
        d.handle(Request::DeleteRole {
            name: "primary".into(),
        })
        .await
    }
}

async fn refusal(d: &Arc<Daemon>, harness: bool, expected: &[&str]) {
    let before = std::fs::read(d.paths.config_file()).unwrap();
    let error = delete(d, harness)
        .await
        .expect_err("in-use role deletion must be refused")
        .to_string();
    for name in expected {
        assert!(error.contains(name), "missing {name}: {error}");
    }
    assert!(
        error.contains("change") && error.contains("first"),
        "{error}"
    );
    assert_eq!(std::fs::read(d.paths.config_file()).unwrap(), before);
    assert!(Config::load_file_only(&d.paths)
        .unwrap()
        .roles
        .contains_key("primary"));
}

async fn exercise_references(harness: bool) {
    let env = crate::credential_env::EnvGuard::isolated();
    for name in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
        env.remove(name);
    }
    env.set("FORMLESS_API_KEY", "unused-private-role-reference");
    let root = Fixture(std::env::temp_dir().join(format!("fl-role-refs-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("d"), root.0.join("c")).unwrap();
    let tripwire = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    tripwire.set_nonblocking(true).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", tripwire.local_addr().unwrap());
    cfg.provider.model = "role:primary".into();
    cfg.provider.auto_test_models = false;
    cfg.naming.enabled = false;
    cfg.roles.insert(
        "primary".into(),
        crate::config::RoleCfg {
            model: "model".into(),
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let d = Arc::new(Daemon::new(paths).unwrap());
    let (live, archived, deleted) = d
        .with_store(|st| {
            let live = st.create_session(Some("pinned-session".into()))?.id;
            let archived = st.create_session(Some("archived-session".into()))?.id;
            let deleted = st.create_session(Some("deleted-session".into()))?.id;
            for id in [&live, &archived, &deleted] {
                st.set_session_model(id, Some("role:primary".into()), None)?;
            }
            st.set_session_archived(&archived, true)?;
            st.soft_delete_session(&deleted)?;
            Ok((live, archived, deleted))
        })
        .unwrap();
    let mut template = TemplateInfo {
        name: "pinned-template".into(),
        model: Some("role:primary".into()),
        reasoning_effort: None,
        instructions: None,
        cwd: None,
        first_message: None,
    };
    d.with_store(|st| st.save_template(&template)).unwrap();
    refusal(
        &d,
        harness,
        &[
            "provider.model",
            &live,
            &archived,
            &deleted,
            "pinned-template",
        ],
    )
    .await;
    d.handle(Request::SetSetting {
        key: "provider.model".into(),
        value: json!("plain-model"),
    })
    .await
    .unwrap();
    refusal(
        &d,
        harness,
        &[&live, &archived, &deleted, "pinned-template"],
    )
    .await;
    for id in [&live, &archived] {
        d.handle(Request::SetSessionModel {
            id: id.clone(),
            model: Some("plain-model".into()),
            reasoning_effort: None,
        })
        .await
        .unwrap();
    }
    refusal(&d, harness, &[&deleted, "pinned-template"]).await;
    d.with_store(|st| st.set_session_model(&deleted, Some("plain-model".into()), None))
        .unwrap();
    refusal(&d, harness, &["pinned-template"]).await;
    // A longer model id containing the role text is not the same reference.
    template.model = Some("vendor/role:primary.extra".into());
    d.with_store(|st| st.save_template(&template)).unwrap();
    delete(&d, harness).await.unwrap();
    assert!(!Config::load_file_only(&d.paths)
        .unwrap()
        .roles
        .contains_key("primary"));
    assert!(delete(&d, harness)
        .await
        .unwrap_err()
        .to_string()
        .contains("no role"));
    assert_eq!(
        tripwire.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn daemon_role_delete_names_all_references_and_succeeds_after_repointing() {
    exercise_references(false).await;
}

#[tokio::test]
async fn harness_role_delete_names_all_references_and_succeeds_after_repointing() {
    exercise_references(true).await;
}

#[tokio::test]
async fn daemon_environment_role_pin_also_blocks_deletion() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_SECURE");
    env.set("FORMLESS_MODEL", "role:primary");
    env.set("FORMLESS_API_KEY", "unused-private-role-reference");
    let root = Fixture(std::env::temp_dir().join(format!("fl-role-env-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("d"), root.0.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.model = "file-model".into();
    cfg.naming.enabled = false;
    cfg.roles.insert("primary".into(), Default::default());
    cfg.save(&paths).unwrap();
    let d = Arc::new(Daemon::new(paths).unwrap());
    refusal(&d, false, &["FORMLESS_MODEL"]).await;
    refusal(&d, true, &["FORMLESS_MODEL"]).await;
    env.remove("FORMLESS_MODEL");
    d.handle(Request::SetSetting {
        key: "provider.model".into(),
        value: json!("file-model"),
    })
    .await
    .unwrap();
    delete(&d, false).await.unwrap();
}
