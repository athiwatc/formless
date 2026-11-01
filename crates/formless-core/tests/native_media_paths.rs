//! @path native media use the image intake bound and existing private-path
//! denials. A project is a sibling of the profile, never inside private data.
#[path = "support/env.rs"]
mod credential_env;
use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;
#[tokio::test]
async fn native_mentions_copy_only_bounded_authorized_files_and_keep_large_references() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-path-fixture");
    let root = std::env::temp_dir().join(format!("fl-native-mentions-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    let sid = st.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let attach = |path: String, authority: std::path::PathBuf| {
        let daemon = daemon.clone();
        let sid = sid.clone();
        async move {
            daemon
                .handle(Request::AttachPath {
                    session_id: sid,
                    path,
                    root: Some(authority.to_string_lossy().into_owned()),
                })
                .await
        }
    };
    for (name, header, mime) in [
        ("document.pdf", b"%PDF-1.4\n".as_slice(), "application/pdf"),
        (
            "clip.wav",
            b"RIFF\x00\x00\x00\x00WAVE".as_slice(),
            "audio/wav",
        ),
        (
            "clip.mp3",
            b"ID3\x04\x00\x00\x00\x00\x00\x00".as_slice(),
            "audio/mpeg",
        ),
    ] {
        let mut bytes = header.to_vec();
        bytes.resize(300 * 1024, 0);
        std::fs::write(project.join(name), &bytes).unwrap();
        let result = attach(name.into(), project.clone()).await.unwrap();
        assert_eq!(result["reference"], false);
        assert_eq!(result["mime"], mime);
        let (_, stored, size) = st
            .get_artifact(result["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(size, bytes.len() as i64);
        assert_eq!(std::fs::read(stored).unwrap(), bytes);
    }
    let large = project.join("large.pdf");
    let mut file = std::fs::File::create(&large).unwrap();
    use std::io::Write;
    file.write_all(b"%PDF-1.4\n").unwrap();
    file.set_len(8 * 1024 * 1024 + 1).unwrap();
    drop(file);
    let result = attach("large.pdf".into(), project.clone()).await.unwrap();
    assert_eq!(result["reference"], true);
    assert!(result.get("id").is_none());
    std::fs::write(project.join("unsupported.flac"), vec![0u8; 300 * 1024]).unwrap();
    assert_eq!(
        attach("unsupported.flac".into(), project.clone())
            .await
            .unwrap()["reference"],
        true
    );
    let before = st.list_artifacts().unwrap().len();
    std::fs::create_dir_all(project.join(".ssh")).unwrap();
    std::fs::write(project.join(".ssh/private.pdf"), b"%PDF-private").unwrap();
    let error = attach(".ssh/private.pdf".into(), project.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("credential"));
    let protected = paths.data_dir.join("private.pdf");
    std::fs::write(&protected, b"%PDF-private-profile").unwrap();
    let error = attach(protected.to_string_lossy().into_owned(), root.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("protected profile"));
    #[cfg(unix)]
    {
        let outside = root.join("outside.pdf");
        std::fs::write(&outside, b"%PDF-private-outside").unwrap();
        std::os::unix::fs::symlink(&outside, project.join("escape.pdf")).unwrap();
        assert!(attach("escape.pdf".into(), project.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("outside"));
        std::os::unix::fs::symlink(&paths.data_dir, project.join("profile-alias")).unwrap();
        assert!(
            attach("project/profile-alias/private.pdf".into(), root.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("protected profile")
        );
        let alias_root = root.join(".ssh");
        std::os::unix::fs::symlink(&project, &alias_root).unwrap();
        assert!(
            attach("document.pdf".into(), alias_root)
                .await
                .unwrap_err()
                .to_string()
                .contains("credential"),
            "configured root's lexical credential namespace remains protected"
        );
        std::os::unix::fs::symlink(project.join("document.pdf"), project.join(".ssh/safe.pdf"))
            .unwrap();
        assert!(attach(".ssh/safe.pdf".into(), project.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("credential"));
    }
    assert_eq!(
        st.list_artifacts().unwrap().len(),
        before,
        "denied paths create no artifact copies"
    );
    drop(st);
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
