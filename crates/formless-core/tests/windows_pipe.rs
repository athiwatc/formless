//! Windows named pipe: owner-only security, and a client can talk to the
//! daemon over it (SPEC §2.3/§14).
#![cfg(windows)]

use formless_core::Paths;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[test]
fn owner_only_security_attributes_build() {
    let sa = formless_core::win_pipe::OwnerOnly::new().unwrap();
    assert!(!sa.as_raw().is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_answers_over_the_named_pipe() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::env::temp_dir().join(format!("fl-win-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-win-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    let client = loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(paths.pipe_name()) {
            Ok(c) => break c,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    };
    let (r, mut w) = tokio::io::split(client);
    let env = formless_protocol::RequestEnvelope {
        id: 1,
        protocol: formless_protocol::PROTOCOL_VERSION,
        request: formless_protocol::Request::Ping,
    };
    w.write_all(format!("{}\n", serde_json::to_string(&env).unwrap()).as_bytes())
        .await
        .unwrap();
    let mut lines = tokio::io::BufReader::new(r).lines();
    let line = lines.next_line().await.unwrap().unwrap();
    assert!(line.contains("\"id\":1"), "{line}");
}
