//! Private, short-lived onboarding progress. Credentials are never drafted.

use crate::{config, Paths};
use anyhow::Result;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub fn file(paths: &Paths) -> std::path::PathBuf {
    paths.config_dir.join("setup-draft.json")
}

pub fn discard(paths: &Paths) -> std::io::Result<()> {
    match std::fs::remove_file(file(paths)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Skip URLs whose authority/query/fragment can contain credentials. Refusing
/// the whole draft avoids changing the endpoint's semantics on resume.
fn draftable_url(raw: &str) -> bool {
    if raw.contains(['@', '?', '#']) {
        return false;
    }
    reqwest::Url::parse(raw).map_or(true, |url| {
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

pub fn save(paths: &Paths, progress: &Value) -> Result<()> {
    save_at(paths, progress, SystemTime::now())
}

fn save_at(paths: &Paths, progress: &Value, now: SystemTime) -> Result<()> {
    let url = progress["base_url"].as_str().unwrap_or_default().trim();
    if !draftable_url(url) {
        discard(paths)?;
        return Ok(());
    }
    // Whitelist progress fields: adding credentials to a caller's state cannot
    // accidentally add them to this file.
    let draft = json!({
        "version": 1,
        "saved_at": now.duration_since(UNIX_EPOCH)?.as_secs(),
        "step": progress["step"],
        "preset": progress["preset"],
        "custom": progress["custom"],
        "base_url": url,
        "model": progress["model"],
        "effort_idx": progress["effort_idx"],
    });
    config::write_secret_file(&file(paths), &draft.to_string())
}

pub fn load(paths: &Paths) -> Option<Value> {
    load_at(paths, SystemTime::now())
}

fn load_at(paths: &Paths, now: SystemTime) -> Option<Value> {
    let path = file(paths);
    let read = || {
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > 64 * 1024 {
            return None;
        }
        let draft: Value = serde_json::from_str(&config::read_secret_file(&path)?).ok()?;
        let saved = UNIX_EPOCH.checked_add(Duration::from_secs(draft["saved_at"].as_u64()?))?;
        if draft["version"] != 1
            || now.duration_since(saved).ok()? > MAX_AGE
            || !draftable_url(draft["base_url"].as_str()?)
        {
            return None;
        }
        if std::fs::metadata(paths.config_file())
            .and_then(|m| m.modified())
            .ok()
            .zip(meta.modified().ok())
            .is_some_and(|(config, draft)| config > draft)
        {
            return None;
        }
        Some(draft)
    };
    let result = read();
    if result.is_none() {
        let _ = discard(paths);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        let base = std::env::temp_dir().join(format!("fl-setup-draft-{}", uuid::Uuid::new_v4()));
        Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
    }

    #[test]
    fn drafts_are_private_atomic_and_never_include_credentials() {
        let paths = paths();
        let progress = json!({"step":"model", "base_url":"https://example.com/v1", "model":"m", "api_key":"never-save-me"});
        save(&paths, &progress).unwrap();
        let first = std::fs::read(file(&paths)).unwrap();
        assert!(!String::from_utf8_lossy(&first).contains("never-save-me"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(file(&paths))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            // Replacement must also tighten an existing permissive draft.
            std::fs::set_permissions(file(&paths), std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        save(&paths, &progress).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(file(&paths))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(load(&paths).is_some());
        assert_eq!(std::fs::read_dir(&paths.config_dir).unwrap().count(), 1);
        for url in [
            "https://u:p@example.com/v1",
            "https://example.com/v1?api-key=secret",
            "https://example.com/v1#secret",
        ] {
            save(&paths, &json!({"base_url":url})).unwrap();
            assert!(!file(&paths).exists(), "{url}");
        }
        std::fs::remove_dir_all(paths.config_dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn old_invalid_and_superseded_drafts_are_removed() {
        let paths = paths();
        let progress = json!({"step":"model", "base_url":"https://example.com/v1"});
        let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        save_at(&paths, &progress, now - MAX_AGE - Duration::from_secs(1)).unwrap();
        assert!(load_at(&paths, now).is_none());
        assert!(!file(&paths).exists());
        save_at(&paths, &progress, now).unwrap();
        // A config written by any path (including an external editor) wins.
        let config = std::fs::File::create(paths.config_file()).unwrap();
        config.set_modified(now + Duration::from_secs(1)).unwrap();
        assert!(load_at(&paths, now).is_none());
        save_at(&paths, &progress, now).unwrap();
        crate::Config::default().save(&paths).unwrap();
        assert!(
            !file(&paths).exists(),
            "CLI/config saves discard old setup progress"
        );
        std::fs::write(file(&paths), "{truncated").unwrap();
        assert!(load_at(&paths, now).is_none());
        assert!(!file(&paths).exists());
        std::fs::remove_dir_all(paths.config_dir.parent().unwrap()).unwrap();
    }
}
