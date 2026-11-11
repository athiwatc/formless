//! Runtime overrides and file-only edits, with private paths and environment.
#[path = "support/env.rs"]
mod credential_env;

use formless_core::{Config, Paths};
use formless_protocol::Request;
use serde_json::json;
use std::sync::Arc;

struct Fixture {
    root: std::path::PathBuf,
    paths: Paths,
}
impl Fixture {
    fn new(env: &credential_env::EnvGuard) -> Self {
        for name in [
            "FORMLESS_BASE_URL",
            "FORMLESS_MODEL",
            "FORMLESS_IDLE_GRACE_SECS",
            "FORMLESS_SECURE",
        ] {
            env.remove(name);
        }
        env.set("FORMLESS_API_KEY", "unused-private-environment-fixture");
        let root = std::env::temp_dir().join(format!("fl-env-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.model = "file-model".into();
        cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
        cfg.provider.auto_test_models = false;
        cfg.daemon.idle_grace_secs = 17;
        cfg.naming.enabled = false;
        cfg.save(&paths).unwrap();
        Self { root, paths }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn text_overrides_ignore_blank_values_and_trim_nonblank_values() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    for blank in ["", " ", "\n\t"] {
        env.set("FORMLESS_BASE_URL", blank);
        env.set("FORMLESS_MODEL", blank);
        let cfg = Config::load(&f.paths).unwrap();
        assert_eq!(cfg.provider.base_url, "http://127.0.0.1:9/v1");
        assert_eq!(cfg.provider.model, "file-model");
        assert!(cfg.provider.is_configured());
    }
    env.set("FORMLESS_BASE_URL", "  http://127.0.0.1:9/new  ");
    env.set("FORMLESS_MODEL", "\t env-model \n");
    let cfg = Config::load_from(&f.paths.config_file()).unwrap();
    assert_eq!(cfg.provider.base_url, "http://127.0.0.1:9/new");
    assert_eq!(cfg.provider.model, "env-model");
    assert_eq!(
        Config::load_file_only(&f.paths).unwrap().provider.model,
        "file-model"
    );
}

#[test]
fn idle_override_errors_name_the_variable_and_expected_form() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    for invalid in ["60s", "-1", "1.5", "", " ", "18446744073709551616"] {
        env.set("FORMLESS_IDLE_GRACE_SECS", invalid);
        let error = Config::load(&f.paths).unwrap_err().to_string();
        assert!(error.contains("FORMLESS_IDLE_GRACE_SECS"), "{error}");
        assert!(
            error.contains("nonnegative whole number of seconds"),
            "{error}"
        );
    }
    for (input, expected) in [("0", 0), (" 60 ", 60), ("18446744073709551615", u64::MAX)] {
        env.set("FORMLESS_IDLE_GRACE_SECS", input);
        assert_eq!(
            Config::load(&f.paths).unwrap().daemon.idle_grace_secs,
            expected
        );
    }
    env.remove("FORMLESS_IDLE_GRACE_SECS");
    assert_eq!(Config::load(&f.paths).unwrap().daemon.idle_grace_secs, 17);
}

#[cfg(unix)]
#[test]
fn nontext_overrides_are_loud_errors() {
    use std::os::unix::ffi::OsStringExt;
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    for name in [
        "FORMLESS_BASE_URL",
        "FORMLESS_MODEL",
        "FORMLESS_IDLE_GRACE_SECS",
    ] {
        env.set(name, std::ffi::OsString::from_vec(vec![0xff]));
        let error = Config::load(&f.paths).unwrap_err().to_string();
        assert!(error.contains(name), "{error}");
        env.remove(name);
    }
}

#[tokio::test]
async fn daemon_reports_its_own_shadowing_and_keeps_file_values_separate() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    env.set("FORMLESS_MODEL", " env-model ");
    env.set("FORMLESS_IDLE_GRACE_SECS", "60");
    let d = Arc::new(formless_core::daemon::Daemon::new(f.paths.clone()).unwrap());
    for (key, value, source, effective) in [
        (
            "provider.model",
            json!("saved-model"),
            "FORMLESS_MODEL",
            json!("env-model"),
        ),
        (
            "daemon.idle_grace_secs",
            json!(19),
            "FORMLESS_IDLE_GRACE_SECS",
            json!(60),
        ),
    ] {
        let r = d
            .handle(Request::SetSetting {
                key: key.into(),
                value: value.clone(),
            })
            .await
            .unwrap();
        assert_eq!(r["applies"], "env_override");
        assert_eq!(r["override_source"], source);
        assert_eq!(r["effective_value"], effective);
        assert_eq!(r["value"], value);
        assert_eq!(
            Config::load_file_only(&f.paths)
                .unwrap()
                .setting_value(key)
                .unwrap(),
            Some(value)
        );
    }
    let r = d
        .handle(Request::SetSetting {
            key: "tui.on_start".into(),
            value: json!("new"),
        })
        .await
        .unwrap();
    assert_eq!(r["applies"], "on_tui_start");
    assert!(r["override_source"].is_null());
    let r = d
        .handle(Request::MergeSetting {
            key: "provider.model_context_tokens".into(),
            value: json!({"model.4.5":2000}),
        })
        .await
        .unwrap();
    assert_eq!(r["applies"], "now");
    assert!(r["override_source"].is_null());
    assert_eq!(r["effective_value"]["model.4.5"], 2000);
    env.set("FORMLESS_MODEL", " \t ");
    env.remove("FORMLESS_IDLE_GRACE_SECS");
    let r = d
        .handle(Request::SetSetting {
            key: "provider.model".into(),
            value: json!("unshadowed"),
        })
        .await
        .unwrap();
    assert_eq!(r["applies"], "now");
    assert!(r["override_source"].is_null());
    assert_eq!(r["effective_value"], "unshadowed");
}
