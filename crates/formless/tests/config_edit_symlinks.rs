#![cfg(unix)]

use formless_core::Paths;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn edit(paths: &Paths, editor: &Path, edited: &str) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_formless"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FORMLESS_DATA_DIR", &paths.data_dir)
        .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
        .env(
            "EDITOR",
            format!("'{}'", editor.display().to_string().replace('\'', "'\\''")),
        )
        .env("FIXTURE_CONFIG_TEXT", edited)
        .args(["config", "edit"])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn config_edit_keeps_the_symlink_and_publishes_the_validated_text() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-edit-{}-{nonce}", std::process::id())));
    std::fs::create_dir_all(&fixture.0).unwrap();
    let editor = fixture.0.join("editor");
    std::fs::write(
        &editor,
        "#!/bin/sh\nprintf '%s' \"$FIXTURE_CONFIG_TEXT\" > \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cancel_editor = fixture.0.join("cancel-editor");
    std::fs::write(&cancel_editor, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&cancel_editor, std::fs::Permissions::from_mode(0o700)).unwrap();
    let edited = "# keep my comment\n[provider]\nmodel = \"fixture\"\n";
    for name in ["linked", "regular"] {
        let root = fixture.0.join(name);
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let file = paths.config_file();
        let target = if name == "linked" {
            let target = root.join("dotfiles.toml");
            symlink(&target, &file).unwrap();
            target
        } else {
            file.clone()
        };
        std::fs::write(&target, "# old config\n").unwrap();
        let invalid = edit(&paths, &editor, "[provider").await;
        assert!(!invalid.status.success());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "# old config\n");

        let cancelled = edit(&paths, &cancel_editor, "").await;
        assert!(!cancelled.status.success());
        assert!(String::from_utf8_lossy(&cancelled.stderr).contains("config.toml unchanged"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "# old config\n");

        let result = edit(&paths, &editor, edited).await;
        assert!(result.status.success(), "{result:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), edited);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), edited);

        let unmarked = "[naming]\nauto_download = true\n";
        let result = edit(&paths, &editor, unmarked).await;
        assert!(result.status.success(), "{result:?}");
        let warning = String::from_utf8_lossy(&result.stderr);
        assert!(
            warning.contains("unmarked naming.auto_download = true is treated as false"),
            "{warning}"
        );
        assert!(
            warning.contains("formless config set naming.auto_download true"),
            "{warning}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), unmarked);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), unmarked);
        assert_eq!(
            std::fs::symlink_metadata(&file)
                .unwrap()
                .file_type()
                .is_symlink(),
            name == "linked"
        );
        assert!(!file.with_extension("toml.edit").exists());
    }
}
