use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{FixedOffset, SecondsFormat, Utc};
use serde_json::{json, Value};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "formless-logs-jsonl-{}-{}-{nonce}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn write(&self, name: &str, text: &str) {
        let data = self.0.join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join(name), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_formless"))
            .args(["--profile", "default", "logs"])
            .args(args)
            .env("FORMLESS_DATA_DIR", self.0.join("data"))
            .env("FORMLESS_CONFIG_DIR", self.0.join("config"))
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn successful_stdout(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn snapshot_jsonl_matches_raw_entries_and_selection() {
    let fixture = Fixture::new();
    let now = Utc::now();
    let utc = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let offset = now
        .with_timezone(&FixedOffset::east_opt(7 * 3600).unwrap())
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let undated = "invalid-timestamp WARN ภาษาไทย\n\n  \"context\"\\line";
    let warn = format!("{offset} WARN warning\n preceded rotation\n continued after rotation");
    let old = "2001-01-02T03:04:05Z ERROR old";
    let unknown = format!("{utc} NOTICE 未知");
    let missing = &utc;
    let error = format!("{utc} error latest error");
    let debug = format!("{utc} DEBUG debug details");
    let trace = format!("{utc} trace trace details");
    fixture.write(
        "daemon.log.80",
        &format!("{undated}\n{offset} WARN warning\n preceded rotation").replace('\n', "\r\n"),
    );
    fixture.write(
        "daemon.log.2",
        &format!(" continued after rotation\r\n{old}\r\n{unknown}\r\n{missing}\r\n"),
    );
    fixture.write("daemon.log", &format!("{error}\r\n{debug}\r\n{trace}"));

    let entries = [
        json!({"timestamp": null, "filter_level": "info", "text": undated}),
        json!({"timestamp": offset, "filter_level": "warn", "text": warn}),
        json!({"timestamp": "2001-01-02T03:04:05Z", "filter_level": "error", "text": old}),
        json!({"timestamp": utc, "filter_level": "info", "text": unknown}),
        json!({"timestamp": utc, "filter_level": "info", "text": missing}),
        json!({"timestamp": utc, "filter_level": "error", "text": error}),
        json!({"timestamp": utc, "filter_level": "debug", "text": debug}),
        json!({"timestamp": utc, "filter_level": "trace", "text": trace}),
    ];
    for (args, indices) in [
        (&[][..], &[0, 1, 2, 3, 4, 5, 6, 7][..]),
        (&["--lines=0"][..], &[0, 1, 2, 3, 4, 5, 6, 7][..]),
        (&["--lines=3"][..], &[5, 6, 7][..]),
        (
            &["--level=info", "--since=1d", "--lines=0"][..],
            &[1, 3, 4, 5][..],
        ),
        (&["--level=warn", "--since=1d", "--lines=1"][..], &[5][..]),
    ] {
        let raw = successful_stdout(fixture.run(args));
        let jsonl = successful_stdout(fixture.run(&[args, &["--jsonl"]].concat()));
        assert!(jsonl.ends_with('\n'), "{args:?}");
        assert!(!jsonl.contains('\r'), "{args:?}");
        let records: Vec<Value> = jsonl
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let expected: Vec<_> = indices
            .iter()
            .map(|&index| entries[index].clone())
            .collect();
        assert_eq!(records, expected, "{args:?}");
        let text: String = records
            .iter()
            .map(|record| format!("{}\n", record["text"].as_str().unwrap()))
            .collect();
        assert_eq!(text, raw, "{args:?}");
    }
}

#[test]
fn empty_jsonl_has_only_the_existing_stderr_diagnostic() {
    let fixture = Fixture::new();
    let empty = fixture.run(&["--jsonl", "--lines=0"]);
    assert!(empty.status.success(), "{empty:?}");
    assert!(empty.stdout.is_empty(), "{empty:?}");
    assert!(String::from_utf8_lossy(&empty.stderr).contains("no log entries in "));

    fixture.write("daemon.log", "2026-01-02T03:04:05Z INFO filtered out\n");
    let args = ["--level=error"];
    let raw = fixture.run(&args);
    let jsonl = fixture.run(&[args.as_slice(), &["--jsonl"]].concat());
    assert!(jsonl.status.success(), "{jsonl:?}");
    assert!(jsonl.stdout.is_empty(), "{jsonl:?}");
    assert!(String::from_utf8_lossy(&jsonl.stderr).contains("no log entries match in "));
    assert_eq!(jsonl.stderr, raw.stderr);
}

#[test]
fn jsonl_conflicts_with_follow_before_profile_setup() {
    let fixture = Fixture::new();
    for args in [["--jsonl", "--follow"], ["-f", "--jsonl"]] {
        let output = fixture.run(&args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--jsonl") && stderr.contains("--follow"),
            "{stderr}"
        );
        assert!(stderr.contains("cannot be used with"), "{stderr}");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    }
}
