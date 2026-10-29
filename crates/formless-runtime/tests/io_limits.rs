//! Native caps outside the JS heap (#604). Their own test binary: the caps
//! are process-wide, so lowering them here can't affect other tests.

use formless_runtime::{execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, NoHostOps};
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn run(code: &str, caps: &[String]) -> Result<serde_json::Value, formless_runtime::ExecError> {
    let req = ExecRequest {
        code: code.into(),
        input: json!({}),
        caps: Capabilities::parse(caps).unwrap(),
        memory_mb: 64,
        mode: HostMode::Live,
        cancel: Arc::new(AtomicBool::new(false)),
        overlay: None,
        activity: None,
        modules: Arc::new(Vec::new()),
    };
    execute(&req, Arc::new(NoHostOps)).map(|o| o.output)
}

#[test]
fn reads_output_and_logs_stop_at_their_caps() {
    formless_runtime::set_io_limits(1_000, 2_000, 300);
    let dir = std::env::temp_dir().join(format!("fl-io-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let big = dir.join("big.txt");
    std::fs::write(&big, "x".repeat(5_000)).unwrap();
    let read = format!("fs:read:{}", dir.display());

    let e = run(
        &format!(
            "function run(){{ return fs.read({:?}); }}",
            big.display().to_string()
        ),
        &[read.clone()],
    )
    .unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::Resource, "{}", e.message);
    assert!(
        e.message.contains("fs.readChunks") && e.message.contains("tools.max_read_bytes"),
        "{}",
        e.message
    );

    // Chunked reading still works past the cap.
    let n = run(
        &format!("function run(){{ let n = 0; fs.readChunks({:?}, 500, (c) => {{ n += c.length; }}); return n; }}", big.display().to_string()),
        &[read],
    )
    .unwrap();
    assert_eq!(n, 5_000);

    #[cfg(unix)]
    {
        let out = run(
            "function run(){ const r = process.run('head', ['-c', '50000', '/dev/zero']); return {len: r.stdout.length, truncated: r.truncated, code: r.code}; }",
            &["process:head".into()],
        )
        .unwrap();
        assert_eq!(out["truncated"], true);
        assert_eq!(out["code"], 0, "the child finished: no blocked pipe");
        assert!(out["len"].as_u64().unwrap() <= 2_000);
    }

    let logged = execute(
        &ExecRequest {
            code: "function run(){ for (let i = 0; i < 100; i++) console.log('line ' + i); return 1; }".into(),
            input: json!({}),
            caps: Capabilities::default(),
            memory_mb: 64,
            mode: HostMode::Live,
            cancel: Arc::new(AtomicBool::new(false)),
            overlay: None,
            activity: None,
            modules: Arc::new(Vec::new()),
        },
        Arc::new(NoHostOps),
    )
    .unwrap();
    let total: usize = logged.logs.iter().map(String::len).sum();
    assert!(total < 450, "{total}");
    assert!(logged.logs.last().unwrap().starts_with("[log limit"));
    let _ = std::fs::remove_dir_all(&dir);
}
