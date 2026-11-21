//! Profile onboarding preferences shared between the daemon and its clients.

use crate::Paths;
use anyhow::{Context, Result};
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub approval_help: bool,
    pub tour_offered: bool,
    pub tour_done: bool,
}

fn flag_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => anyhow::bail!("onboarding flag {} is not a regular file", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("read onboarding flag {}", path.display()))
        }
    }
}

pub fn load(paths: &Paths) -> Result<Seen> {
    let legacy = (|| {
        let file = fs::File::open(paths.data_dir.join("tui-seen.json")).ok()?;
        let mut text = String::new();
        file.take(4097).read_to_string(&mut text).ok()?;
        if text.len() > 4096 {
            return None;
        }
        serde_json::from_str::<serde_json::Value>(&text).ok()
    })()
    .unwrap_or_default();
    let directory = paths.data_dir.join("onboarding-seen");
    let seen = |name: &str| -> Result<bool> {
        let current = flag_exists(&directory.join(name))?;
        Ok(current || legacy[name] == true)
    };
    Ok(Seen {
        approval_help: seen("approval_help")?,
        tour_offered: seen("tour_offered")?,
        tour_done: seen("tour_done")?,
    })
}

/// Record flags independently. A concurrent writer can only add flags.
pub fn save(paths: &Paths, seen: &Seen) -> Result<Seen> {
    fn mark(path: &Path) -> Result<()> {
        match fs::File::create_new(path) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && flag_exists(path)? => {
                Ok(())
            }
            Err(error) => {
                Err(error).with_context(|| format!("record onboarding flag {}", path.display()))
            }
        }
    }

    let directory = paths.data_dir.join("onboarding-seen");
    fs::create_dir_all(&directory)
        .with_context(|| format!("create onboarding directory {}", directory.display()))?;
    for (name, value) in [
        ("approval_help", seen.approval_help),
        ("tour_offered", seen.tour_offered),
        ("tour_done", seen.tour_done),
    ] {
        if value {
            mark(&directory.join(name))?;
        }
    }
    load(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_onboarding_writes_merge_flags_and_preserve_legacy_values() {
        let root = std::env::temp_dir().join(format!("fl-onboarding-{}", uuid::Uuid::new_v4()));
        let paths = Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            profile: "default".into(),
        };
        assert_eq!(load(&paths).unwrap(), Seen::default());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                save(
                    &paths,
                    &Seen {
                        approval_help: true,
                        ..Default::default()
                    },
                )
                .unwrap()
            });
            save(
                &paths,
                &Seen {
                    tour_offered: true,
                    ..Default::default()
                },
            )
            .unwrap();
        });
        fs::write(
            paths.data_dir.join("tui-seen.json"),
            r#"{"tour_done":true}"#,
        )
        .unwrap();
        assert_eq!(
            load(&paths).unwrap(),
            Seen {
                approval_help: true,
                tour_offered: true,
                tour_done: true
            }
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn acknowledgement_is_visible_to_later_status_requests() {
        use formless_protocol::Request;
        let root =
            std::env::temp_dir().join(format!("fl-onboarding-daemon-{}", uuid::Uuid::new_v4()));
        let paths = Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            profile: "default".into(),
        };
        let daemon = std::sync::Arc::new(crate::daemon::Daemon::new(paths.clone()).unwrap());
        assert_eq!(
            daemon.handle(Request::Status).await.unwrap()["approval_help_seen"],
            false
        );
        assert_eq!(
            daemon
                .handle(Request::AcknowledgeApprovalHelp)
                .await
                .unwrap()["approval_help_seen"],
            true
        );
        assert!(load(&paths).unwrap().approval_help);
        assert_eq!(
            daemon.handle(Request::Status).await.unwrap()["approval_help_seen"],
            true
        );
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }
}
