//! Exact env-template mentions still respect every other path boundary.
#[path = "support/env.rs"]
mod credential_env;
use formless_core::daemon::{credential_path, Daemon};
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn env_template_mentions_pass_both_guards_but_not_protected_paths() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_MODEL");
    env.remove("FORMLESS_BASE_URL");
    struct Owned(std::path::PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let owned =
        Owned(std::env::temp_dir().join(format!("fl-env-mention-{}", uuid::Uuid::new_v4())));
    let mut paths = Paths::from_dirs(owned.0.join("data"), owned.0.join("config")).unwrap();
    paths.profile = format!("fixture-env-{}", uuid::Uuid::new_v4().simple());
    let project = owned.0.join("project");
    std::fs::create_dir_all(project.join("key-source")).unwrap();
    std::fs::create_dir_all(project.join(".ssh")).unwrap();
    std::fs::create_dir_all(project.join("nested/.env.dist")).unwrap();
    let key_source = project.join("key-source/.env.example");
    std::fs::write(&key_source, "synthetic-fixture-only").unwrap();
    env.set("FORMLESS_API_KEY_FILE", &key_source);
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let sid = Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let attach = |daemon: Arc<Daemon>, path: std::path::PathBuf| {
        let sid = sid.clone();
        let root = owned.0.to_string_lossy().into_owned();
        async move {
            daemon
                .handle(Request::AttachPath {
                    session_id: sid,
                    path: path.to_string_lossy().into_owned(),
                    root: Some(root),
                })
                .await
        }
    };
    for name in [".env.example", ".env.sample", ".env.template", ".env.dist"] {
        let file = project.join(name);
        std::fs::write(&file, "OPTION=placeholder\n").unwrap();
        let result = attach(daemon.clone(), file.clone()).await;
        assert!(result.is_ok(), "{name}: {result:?}");
        let value = result.unwrap();
        assert_eq!(value["reference"], false);
        assert!(
            credential_path(&file).is_none(),
            "TUI/catalog mention predicate: {name}"
        );
    }
    for name in [
        ".env",
        ".env.local",
        ".env.production",
        ".env.local.example",
        ".env.example.bak",
        ".environment",
        ".ssh/.env.example",
        "nested/.env.dist/notes.txt",
        "nested/.env.dist/.env.example",
    ] {
        let file = project.join(name);
        std::fs::write(&file, "fixture only").unwrap();
        let error = attach(daemon.clone(), file).await.unwrap_err().to_string();
        if name.ends_with(".env.example") {
            assert!(
                credential_path(&project.join(name)).is_some(),
                "private ancestor: {name}"
            );
        }
        assert!(
            error.contains("credential") || error.contains("protected"),
            "{name}: {error}"
        );
    }
    assert!(attach(daemon.clone(), project.join("nested/.env.dist"))
        .await
        .is_err());
    let profile = paths.config_dir.join(".env.example");
    std::fs::write(&profile, "fixture only").unwrap();
    assert!(attach(daemon.clone(), profile)
        .await
        .unwrap_err()
        .to_string()
        .contains("protected"));
    assert!(attach(daemon.clone(), key_source)
        .await
        .unwrap_err()
        .to_string()
        .contains("protected"));
    #[cfg(unix)]
    {
        std::fs::create_dir_all(project.join("alias")).unwrap();
        let alias = project.join("alias/.env.example");
        std::os::unix::fs::symlink(project.join(".env.local"), &alias).unwrap();
        assert!(credential_path(&alias).is_some());
        assert!(attach(daemon.clone(), alias).await.is_err());
        let ordinary_alias = project.join("alias/.env.sample");
        std::os::unix::fs::symlink(project.join(".env.template"), &ordinary_alias).unwrap();
        assert!(attach(daemon.clone(), ordinary_alias).await.is_err());
    }
    drop(daemon);
    cfg.security
        .sensitive_paths
        .push(formless_runtime::sensitive_paths::SensitivePathRule::BasenamePrefix(".env".into()));
    cfg.save(&paths).unwrap();
    let restricted = Arc::new(Daemon::new(paths).unwrap());
    assert!(attach(restricted, project.join(".env.example"))
        .await
        .unwrap_err()
        .to_string()
        .contains("protected"));
}
