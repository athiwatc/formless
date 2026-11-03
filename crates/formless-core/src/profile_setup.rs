//! Read-only setup context. Never load credentials, apply another profile's
//! environment overrides, start a daemon, or probe an alternate provider.
use crate::{Config, Paths};

pub fn setup_command(profile: &str) -> String {
    if profile == "default" {
        "formless".into()
    } else {
        format!("formless --profile {profile}")
    }
}

pub fn missing_setup_message(paths: &Paths) -> String {
    let mut message = format!(
        "Profile '{}' is not configured; run `{}` for the wizard.",
        paths.profile,
        setup_command(&paths.profile)
    );
    if let Some(hint) = alternate_setup_hint(paths) {
        message.push('\n');
        message.push_str(&hint);
    }
    message
}

pub fn alternate_setup_hint(paths: &Paths) -> Option<String> {
    let other = paths.list_profiles().into_iter().find(|other| {
        other.profile != paths.profile
            && Config::load_file_only(other).is_ok_and(|cfg| cfg.provider.is_configured())
    })?;
    Some(format!("Saved setup exists in profile '{}'.\nOpen it with `{}`.\nSee profiles with `formless profiles list`.", other.profile, setup_command(&other.profile)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_hints_use_saved_profile_configuration_without_creating_or_changing_files() {
        let root = std::env::temp_dir().join(format!("fl-setup-{}", uuid::Uuid::new_v4()));
        struct Owned(std::path::PathBuf);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _owned = Owned(root.clone());
        let base = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let other = base.named("work").unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
        cfg.provider.model = "saved-model".into();
        cfg.save(&other).unwrap();
        let before = std::fs::read(other.config_file()).unwrap();
        let scratch = base.named("scratch").unwrap();
        for active in [&base, &scratch] {
            let message = missing_setup_message(active);
            assert!(message.contains(&format!("Profile '{}'", active.profile)));
            assert!(message.contains("formless --profile work"));
            assert!(message.contains("formless profiles list"));
            assert!(!active.config_file().exists());
            assert!(!active.db_file().exists());
        }
        assert_eq!(std::fs::read(other.config_file()).unwrap(), before);
    }
}
