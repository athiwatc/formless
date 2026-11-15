//! Invalid search limits stop at parsing, before profile or daemon work.
use std::process::Command;
#[test]
fn invalid_search_limits_exit_two_without_creating_a_profile_or_contacting_a_daemon() {
    let root = std::env::temp_dir().join(format!(
        "formless-search-limit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for limit in ["0", "201", "-1", "184467440737095516160000"] {
        let output = Command::new(env!("CARGO_BIN_EXE_formless"))
            .args(["sessions", "search", "fixture", &format!("--limit={limit}")])
            .env("FORMLESS_DATA_DIR", root.join("data"))
            .env("FORMLESS_CONFIG_DIR", root.join("config"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("1 to 200"));
        assert!(
            !root.exists(),
            "usage rejection must precede profile/startup work"
        );
    }
}
