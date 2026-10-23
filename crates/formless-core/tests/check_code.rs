//! The web editor's live syntax check (SPEC §10.4).

use formless_core::{Config, Paths};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn tool_code_is_checked_without_saving() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-check-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-check-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let check = |code: &str| Request::CheckToolCode { code: code.into() };
    let ok = daemon
        .handle(check("function run(input) {\n  return input;\n}\n"))
        .await
        .unwrap();
    assert_eq!(ok["ok"], true);
    let bad = daemon
        .handle(check(
            "function run(input) {\n  const x = ;\n  return x;\n}\n",
        ))
        .await
        .unwrap();
    assert_eq!(bad["ok"], false);
    println!("error: {}", bad["error"]);
    assert_eq!(bad["line"], 2, "{bad}");
    let _ = std::fs::remove_dir_all(&dir);
}
