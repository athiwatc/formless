use super::*;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> (Fixture, Arc<Daemon>, Config) {
    let fixture =
        Fixture(std::env::temp_dir().join(format!("formless-model-text-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(fixture.0.join("data"), fixture.0.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.tools.artifact_preview_chars = 4;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    (fixture, daemon, cfg)
}
const TEXT: &str = "αβไทย日本語 large output";

fn assert_failure(text: &str, cap: usize, output: &str) {
    let preview: String = text.chars().take(cap).collect();
    assert!(output.starts_with(&format!("{preview}\n")), "{output}");
    assert!(
        output.contains(&format!(
            "truncated to {cap} characters (Unicode scalar values)"
        )),
        "{output}"
    );
    assert!(
        output.contains(&format!("{} UTF-8 bytes", text.len())),
        "{output}"
    );
    assert!(
        output.contains("could not be stored as an artifact"),
        "{output}"
    );
    assert!(!output.contains("stored as artifact "), "{output}");
    assert!(!output.contains("read_artifact"), "{output}");
}

#[test]
fn model_text_returns_under_cap_values_verbatim_without_artifact_io() {
    let (_fixture, daemon, cfg) = fixture();
    let dir = daemon.paths.data_dir.join("artifacts");
    std::fs::remove_dir(&dir).unwrap();
    std::fs::write(&dir, "block directory creation").unwrap();
    for value in [
        json!("ไทย"),
        json!("αβไทย".chars().take(4).collect::<String>()),
        json!(null),
        json!(12),
    ] {
        let expected = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        assert_eq!(daemon.model_text("fixture", &value, &cfg), expected);
    }
    assert_eq!(
        std::fs::read_to_string(dir).unwrap(),
        "block directory creation"
    );
}

#[test]
fn model_text_marks_file_write_failure_and_zero_character_preview() {
    let (_fixture, daemon, mut cfg) = fixture();
    std::fs::remove_dir(daemon.paths.data_dir.join("artifacts")).unwrap();
    std::fs::write(
        daemon.paths.data_dir.join("artifacts"),
        "block directory creation",
    )
    .unwrap();
    assert_failure(TEXT, 4, &daemon.model_text("fixture", &json!(TEXT), &cfg));
    cfg.tools.artifact_preview_chars = 0;
    assert_failure(TEXT, 0, &daemon.model_text("fixture", &json!(TEXT), &cfg));
    assert!(daemon
        .with_store(|st| st.list_artifacts())
        .unwrap()
        .is_empty());
}

#[test]
fn model_text_does_not_advertise_artifact_when_row_insert_fails() {
    let (_fixture, daemon, cfg) = fixture();
    let conn = rusqlite::Connection::open(daemon.paths.db_file()).unwrap();
    conn.execute_batch("CREATE TRIGGER fixture_artifact_failure BEFORE INSERT ON artifacts BEGIN SELECT RAISE(ABORT,'synthetic artifact row failure'); END;").unwrap();
    let session = daemon.with_store(|st| st.create_session(None)).unwrap();
    assert_failure(TEXT, 4, &daemon.model_text(&session.id, &json!(TEXT), &cfg));
    assert!(daemon
        .with_store(|st| st.list_artifacts())
        .unwrap()
        .is_empty());
    let files: Vec<_> = std::fs::read_dir(daemon.paths.data_dir.join("artifacts"))
        .unwrap()
        .collect();
    assert_eq!(
        files.len(),
        1,
        "existing orphan-file behavior remains outside this fix"
    );
    assert_eq!(
        std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap(),
        crate::secrets::redact(TEXT).0
    );
}

#[test]
fn model_text_success_keeps_preview_wording_and_usable_full_artifact() {
    let (_fixture, daemon, cfg) = fixture();
    let session = daemon.with_store(|st| st.create_session(None)).unwrap();
    let output = daemon.model_text(&session.id, &json!(TEXT), &cfg);
    let entries = daemon.with_store(|st| st.list_artifacts()).unwrap();
    assert_eq!(entries.len(), 1);
    let id = &entries[0].0;
    assert!(!id.is_empty());
    let (owner, path, _) = daemon
        .with_store(|st| st.get_artifact(id))
        .unwrap()
        .unwrap();
    assert_eq!(owner.as_deref(), Some(session.id.as_str()));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        crate::secrets::redact(TEXT).0
    );
    let preview: String = TEXT.chars().take(4).collect();
    assert_eq!(output,format!("{preview}\n[full output ({} bytes) stored as artifact {id}; read it with read_artifact]",TEXT.len()));
}
