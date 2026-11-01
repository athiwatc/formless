use super::*;
use crate::{artifact_delete::Root, Paths};
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fl-artifact-delete-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn paths(&self, name: &str) -> Paths {
        Paths::from_dirs(
            self.0.join(name).join("data"),
            self.0.join(name).join("config"),
        )
        .unwrap()
    }
    fn file(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"private sentinel").unwrap();
        p
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn assert_sentinel(path: &Path) {
    assert_eq!(std::fs::read(path).unwrap(), b"private sentinel");
}

#[test]
fn restored_foreign_rows_neither_delete_victim_nor_force_owned_eviction() {
    let f = Fixture::new();
    let source = f.paths("source");
    let victim = f.file("outside/victim");
    let source_store = Store::open(&source.db_file()).unwrap();
    source_store
        .save_artifact(None, victim.to_str().unwrap(), "", 2_000_000_000)
        .unwrap();
    let backup = f.0.join("crafted-valid.db");
    source_store.backup_into(&backup).unwrap();
    drop(source_store);
    let target = f.paths("target");
    let report = crate::housekeeping::restore_report(&target, &backup).unwrap();
    assert_eq!(report.references.unwrap().artifacts.foreign, 1);
    let owned = target.data_dir.join("artifacts/kept");
    std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
    std::fs::write(&owned, b"keep").unwrap();
    let store = Store::open(&target.db_file()).unwrap();
    let good = store
        .save_artifact(None, owned.to_str().unwrap(), "", 4)
        .unwrap();
    let cfg = ArtifactsCfg {
        max_age_days: 30,
        max_total_mb: 1,
    };
    assert_eq!(gc_preview(&store, &cfg).unwrap(), (0, 0));
    assert_eq!(gc(&store, &cfg).unwrap(), Collected::default());
    assert_sentinel(&victim);
    assert_eq!(std::fs::read(&owned).unwrap(), b"keep");
    let rows = store.list_artifacts().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, good);
}

#[test]
fn session_deletion_drops_foreign_and_dotdot_rows_but_only_unlinks_owned_files() {
    let f = Fixture::new();
    let paths = f.paths("profile");
    let victim = f.file("profile/data/private-victim");
    let store = Store::open(&paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    let owned = paths.data_dir.join("artifacts/owned");
    std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
    std::fs::write(&owned, b"owned").unwrap();
    for path in [
        &victim,
        &owned,
        &paths.data_dir.join("artifacts/../private-victim"),
    ] {
        store
            .save_artifact(Some(&session), path.to_str().unwrap(), "", 5)
            .unwrap();
    }
    remove_for_session(&store, &session).unwrap();
    store.delete_session(&session).unwrap();
    assert_sentinel(&victim);
    assert!(!owned.exists());
    assert!(store.list_artifacts().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn gc_and_session_deletion_never_follow_parent_final_or_root_symlinks() {
    use std::os::unix::fs::symlink;
    for session_delete in [false, true] {
        let f = Fixture::new();
        let paths = f.paths("profile");
        let victim = f.file("outside/victim");
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap().id;
        let root = paths.data_dir.join("artifacts");
        std::fs::create_dir_all(&root).unwrap();
        symlink(victim.parent().unwrap(), root.join("parent")).unwrap();
        symlink(&victim, root.join("final")).unwrap();
        for path in [root.join("parent/victim"), root.join("final")] {
            store
                .save_artifact(Some(&session), path.to_str().unwrap(), "", 2_000_000_000)
                .unwrap();
        }
        if session_delete {
            remove_for_session(&store, &session).unwrap();
        } else {
            gc(
                &store,
                &ArtifactsCfg {
                    max_age_days: 0,
                    max_total_mb: 1,
                },
            )
            .unwrap();
        }
        assert_sentinel(&victim);
        assert!(std::fs::symlink_metadata(root.join("final"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(store.list_artifacts().unwrap().is_empty());
        std::fs::remove_file(root.join("parent")).unwrap();
        std::fs::remove_file(root.join("final")).unwrap();
        std::fs::remove_dir(&root).unwrap();
        symlink(victim.parent().unwrap(), &root).unwrap();
        store
            .save_artifact(
                Some(&session),
                root.join("victim").to_str().unwrap(),
                "",
                2_000_000_000,
            )
            .unwrap();
        gc(
            &store,
            &ArtifactsCfg {
                max_age_days: 0,
                max_total_mb: 1,
            },
        )
        .unwrap();
        assert_sentinel(&victim);
    }
}

#[cfg(unix)]
#[test]
fn pinned_parent_and_final_component_swaps_do_not_redirect_unlink() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let root_path = f.0.join("artifacts");
    let victim = f.file("outside/victim");
    let owned = f.file("artifacts/child/victim");
    let root = Root::open(&root_path).unwrap();
    root.remove_with_hook(&owned, || {
        std::fs::rename(root_path.join("child"), root_path.join("aside")).unwrap();
        symlink(victim.parent().unwrap(), root_path.join("child")).unwrap();
    })
    .unwrap();
    assert_sentinel(&victim);
    assert!(!root_path.join("aside/victim").exists());
    let owned = f.file("artifacts/final");
    root.remove_with_hook(&owned, || {
        std::fs::remove_file(&owned).unwrap();
        symlink(&victim, &owned).unwrap();
    })
    .unwrap();
    assert_sentinel(&victim);
    assert!(!owned.exists());
}

#[cfg(unix)]
#[test]
fn reconciliation_does_not_delete_through_emitting_directory_link() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let paths = f.paths("profile");
    let store = Store::open(&paths.db_file()).unwrap();
    let victim = f.file("outside/victim");
    std::fs::File::open(&victim)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    let root = paths.data_dir.join("artifacts");
    std::fs::create_dir_all(&root).unwrap();
    symlink(victim.parent().unwrap(), root.join("emitting")).unwrap();
    assert_eq!(
        reconcile(&store, &root, std::time::Duration::ZERO).unwrap(),
        Reconciled::default()
    );
    assert_sentinel(&victim);
}

#[test]
fn uncertain_root_errors_preserve_all_rows_attachment_state_and_preview_honesty() {
    let f = Fixture::new();
    let paths = f.paths("profile");
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let owned = f.file("profile/data/artifacts/owned");
    let id = store
        .save_artifact(Some(&sid), owned.to_str().unwrap(), "", 2_000_000_000)
        .unwrap();
    let meta = serde_json::json!({"attachments":[{"id":id,"path":owned,"bytes":2_000_000_000}]})
        .to_string();
    store
        .append_message_meta(&sid, "user", "private", Some(meta))
        .unwrap();
    let rows = store.list_artifacts().unwrap();
    let messages = store.list_messages(&sid).unwrap();
    let cfg = ArtifactsCfg {
        max_age_days: 0,
        max_total_mb: 1,
    };
    for kind in [
        std::io::ErrorKind::Unsupported,
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::Other,
    ] {
        let error = gc_with_open(&store, &cfg, |_| {
            Err(std::io::Error::new(kind, "injected unverifiable root"))
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("uncertain"));
        assert!(
            gc_preview_with_open(&store, &cfg, |_| Err(std::io::Error::new(
                kind,
                "injected unverifiable root"
            )))
            .is_err(),
            "unknown inventory must not become a zero-byte preview"
        );
        assert_eq!(store.list_artifacts().unwrap(), rows);
        assert_eq!(store.list_messages(&sid).unwrap()[0].meta, messages[0].meta);
        assert_sentinel(&owned);
    }
}

#[cfg(unix)]
#[test]
#[ignore = "private permission subprocess, invoked by the parent regression"]
fn artifact_permission_helper() {
    let data = PathBuf::from(
        std::env::var_os("FORMLESS_PRIVATE_ARTIFACT_PERMISSION_DATA").expect("private helper data"),
    );
    let store = Store::open(&data.join("formless.db")).unwrap();
    let rows = store.list_artifacts().unwrap();
    let sid = rows[0].0.clone();
    let id = &rows[0].0;
    let owner = store.get_artifact(id).unwrap().unwrap().0.unwrap();
    let before = store.list_messages(&owner).unwrap()[0].meta.clone();
    let cfg = ArtifactsCfg {
        max_age_days: 0,
        max_total_mb: 1,
    };
    let error = gc(&store, &cfg).unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(gc_preview(&store, &cfg).is_err());
    assert!(reconcile(&store, &data.join("artifacts"), std::time::Duration::ZERO).is_err());
    assert!(remove_for_session(&store, &owner).is_err());
    assert!(remove_record(&store, &sid).is_err());
    assert_eq!(store.list_artifacts().unwrap(), rows);
    assert_eq!(store.list_messages(&owner).unwrap()[0].meta, before);
}

#[cfg(unix)]
#[test]
fn actual_unprivileged_permission_failure_preserves_artifacts_and_message_metadata() {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    // /tmp avoids inheriting a root-only validation TMPDIR ancestor; every
    // permission changed below belongs to this disposable fixture alone.
    let f = Fixture(
        std::path::PathBuf::from("/tmp")
            .join(format!("fl-art-perm-{}", uuid::Uuid::new_v4().simple())),
    );
    std::fs::create_dir_all(&f.0).unwrap();
    let data = f.0.join("data");
    let owned = f.file("data/artifacts/owned");
    let store = Store::open(&data.join("formless.db")).unwrap();
    let owner = store.create_session(None).unwrap().id;
    let id = store
        .save_artifact(Some(&owner), owned.to_str().unwrap(), "", 2_000_000_000)
        .unwrap();
    store
        .append_message_meta(
            &owner,
            "user",
            "private",
            Some(serde_json::json!({"attachments":[{"id":id,"path":owned}]}).to_string()),
        )
        .unwrap();
    let rows = store.list_artifacts().unwrap();
    let meta = store.list_messages(&owner).unwrap()[0].meta.clone();
    drop(store);
    for path in [&f.0, &data, &data.join("artifacts")] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o777)).unwrap();
    std::fs::set_permissions(
        data.join("formless.db"),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0)).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "artifacts::deletion_tests::artifact_permission_helper",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FORMLESS_PRIVATE_ARTIFACT_PERMISSION_DATA", &data);
    if unsafe { libc::geteuid() } == 0 {
        child.gid(65534).uid(65534);
    }
    let result = child.output().unwrap();
    std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let store = Store::open(&data.join("formless.db")).unwrap();
    assert_eq!(store.list_artifacts().unwrap(), rows);
    assert_eq!(store.list_messages(&owner).unwrap()[0].meta, meta);
    assert_sentinel(&owned);
}

#[cfg(unix)]
#[test]
fn reconciliation_protects_registered_file_identity_across_hardlink_names() {
    let f = Fixture::new();
    let paths = f.paths("profile");
    let store = Store::open(&paths.db_file()).unwrap();
    let registered = f.file("profile/data/artifacts/registered");
    let alias = paths.data_dir.join("artifacts/other-name");
    std::fs::hard_link(&registered, &alias).unwrap();
    store
        .save_artifact(None, registered.to_str().unwrap(), "", 16)
        .unwrap();
    assert_eq!(
        reconcile(
            &store,
            &paths.data_dir.join("artifacts"),
            std::time::Duration::ZERO
        )
        .unwrap(),
        Reconciled::default()
    );
    assert_sentinel(&registered);
    assert_sentinel(&alias);
    assert_eq!(store.list_artifacts().unwrap().len(), 1);
}

#[test]
fn registered_abandoned_emit_is_protected_while_unregistered_peer_is_collected() {
    let f = Fixture::new();
    let paths = f.paths("profile");
    let store = Store::open(&paths.db_file()).unwrap();
    let registered = f.file("profile/data/artifacts/emitting/registered");
    let orphan = f.file("profile/data/artifacts/emitting/orphan");
    for path in [&registered, &orphan] {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
    }
    store
        .save_artifact(None, registered.to_str().unwrap(), "", 16)
        .unwrap();
    assert_eq!(
        reconcile(
            &store,
            &paths.data_dir.join("artifacts"),
            std::time::Duration::ZERO
        )
        .unwrap(),
        Reconciled {
            orphan_files: 1,
            orphan_bytes: 16,
            missing_rows: 0
        }
    );
    assert_sentinel(&registered);
    assert!(!orphan.exists());
    assert_eq!(store.list_artifacts().unwrap().len(), 1);
}
