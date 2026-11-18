//! Native caps outside the JS heap (#604). Their own test binary: the caps
//! are process-wide, so lowering them here can't affect other tests.

use formless_runtime::{execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, NoHostOps};
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn run(code: &str, caps: &[String]) -> Result<serde_json::Value, formless_runtime::ExecError> {
    let req = ExecRequest {
        output_schema: None,
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

    assert!(
        e.message.contains("reported size 5000 bytes")
            && e.message.contains("1000-byte resource limit"),
        "{}",
        e.message
    );

    // A file whose size says 0 is still read only up to the cap (#1103).
    #[cfg(unix)]
    {
        let e = run(
            "function run(){ return fs.read('/dev/zero'); }",
            &["fs:read:/dev".into()],
        )
        .unwrap_err();
        assert_eq!(e.kind, ExecErrorKind::Resource, "{}", e.message);
        assert!(
            e.message.contains("observed at least 1001 bytes"),
            "{}",
            e.message
        );
    }
    // An overlay (dry-run) read over the cap fails the same way.
    let staged = dir.join("staged.txt");
    let e = execute(
        &ExecRequest {
            output_schema: None,
            code: format!(
                "function run(){{ fs.write({p:?}, 'y'.repeat(1500)); return fs.read({p:?}); }}",
                p = staged.display().to_string()
            ),
            input: json!({}),
            caps: Capabilities::parse(&[
                format!("fs:write:{}", dir.display()),
                format!("fs:read:{}", dir.display()),
            ])
            .unwrap(),
            memory_mb: 64,
            mode: HostMode::DryRun,
            cancel: Arc::new(AtomicBool::new(false)),
            overlay: None,
            activity: None,
            modules: Arc::new(Vec::new()),
        },
        Arc::new(NoHostOps),
    )
    .unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::Resource, "{}", e.message);
    assert!(!staged.exists(), "dry run never wrote");

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
            output_schema: None,
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

#[test]
fn dry_run_effects_share_log_budget_on_success_and_error() {
    formless_runtime::set_io_limits(1_000, 2_000, 300);
    for after_logging in [
        "return store.get('fixture');",
        "throw new Error('fixture failure');",
    ] {
        let req = ExecRequest {
            output_schema: None,
            code: format!(
                r#"function run() {{
                    console.log('é'.repeat(100));
                    store.set('fixture', 10);
                    console.warn('x'.repeat(70));
                    console.error('small later line');
                    for (let i = 0; i < 5; i++) store.set('fixture', i);
                    {after_logging}
                }}"#,
            ),
            input: json!({}),
            caps: Capabilities::parse(&["store".into()]).unwrap(),
            memory_mb: 64,
            mode: HostMode::DryRun,
            cancel: Arc::new(AtomicBool::new(false)),
            overlay: None,
            activity: None,
            modules: Arc::new(Vec::new()),
        };
        let logs = match execute(&req, Arc::new(NoHostOps)) {
            Ok(outcome) => {
                assert!(after_logging.starts_with("return"));
                assert_eq!(outcome.output, 4);
                outcome.logs
            }
            Err(error) => {
                assert!(error.message.contains("fixture failure"), "{error}");
                error.logs
            }
        };
        assert_eq!(
            logs,
            [
                "é".repeat(100),
                "[dry run] would store fixture = 10".into(),
                "[log limit of 300 bytes reached (tools.max_log_bytes); later lines dropped] [dry run] 5 further would-be effects omitted".into(),
            ],
        );
    }
}
