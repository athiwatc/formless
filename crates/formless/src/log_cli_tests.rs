//! Private history fixtures exercise the production CLI output/report builders.
use super::*;

struct Fixture {
    paths: Paths,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        // Unique per process (pid) and per fixture (counter); the timestamp only
        // guards against stale roots left by an earlier process with a reused pid.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "fl-cli-logs-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Exclusive create: an existing root fails loudly instead of being shared.
        std::fs::create_dir(&root).unwrap();
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        Self { paths, root }
    }
    fn rotation(&self, n: usize) -> std::path::PathBuf {
        let mut file = self.paths.log_file().into_os_string();
        file.push(format!(".{n}"));
        file.into()
    }
    fn history(
        &self,
        n: usize,
        keep: impl Fn(&LogEntry) -> bool,
    ) -> (String, log_output::OutputSummary) {
        let mut out = Vec::new();
        let summary = log_output::history(&self.paths, n, &keep, &mut out).unwrap();
        (String::from_utf8(out).unwrap(), summary)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn log_history_all_reads_numeric_gaps_beyond_twenty_and_sixty_four_lossily() {
    let f = Fixture::new();
    for (n, text) in [
        (100, "oldest"),
        (65, "older"),
        (21, "past20"),
        (1, "recent"),
    ] {
        std::fs::write(f.rotation(n), format!("2026-10-23T10:00:00Z INFO {text}\n")).unwrap();
    }
    std::fs::write(
        f.paths.log_file(),
        b"2026-10-23T10:00:01Z WARN damaged \xff\n",
    )
    .unwrap();
    let (out, summary) = f.history(0, |_| true);
    assert_eq!(summary.entries, 5);
    let labels = ["oldest", "older", "past20", "recent", "damaged �"];
    let mut at = 0;
    for label in labels {
        at += out[at..].find(label).unwrap() + label.len();
    }
    assert_eq!(summary.read.files_read, 5);
}

#[test]
fn log_history_tail_counts_entries_and_keeps_cross_rotation_continuations() {
    let f = Fixture::new();
    std::fs::write(
        f.rotation(1),
        "2026-10-23T10:00:00Z INFO old\n2026-10-23T10:00:01Z WARN warning\n continued in old",
    )
    .unwrap();
    std::fs::write(
        f.paths.log_file(),
        " continued in live\n2026-10-23T10:00:02Z INFO newest\n",
    )
    .unwrap();
    let (out, summary) = f.history(2, |_| true);
    assert_eq!(summary.entries, 2);
    assert!(!out.contains("INFO old"));
    assert!(out.contains("WARN warning\n continued in old\n continued in live"));
    assert!(out.ends_with("INFO newest\n"));
    let (out, summary) = f.history(1, |entry| entry.level <= 1);
    assert_eq!(summary.entries, 1);
    assert!(out.contains("continued in live"));
    assert!(!out.contains("newest"));
}

#[test]
fn log_history_filters_do_not_stop_on_old_timestamps_or_ignore_older_matches() {
    let f = Fixture::new();
    std::fs::write(
        f.rotation(65),
        "2026-10-23T11:00:00Z WARN wanted\n context\n",
    )
    .unwrap();
    std::fs::write(f.rotation(1), "2026-10-23T12:00:00Z INFO excluded level\n").unwrap();
    std::fs::write(
        f.paths.log_file(),
        "2020-10-23T12:00:00Z ERROR excluded date\n",
    )
    .unwrap();
    let cutoff = chrono::DateTime::parse_from_rfc3339("2026-10-23T10:00:00Z").unwrap();
    let (out, summary) = f.history(1, |e| e.level <= 1 && e.at.is_some_and(|t| t >= cutoff));
    assert_eq!(summary.entries, 1);
    assert_eq!(out, "2026-10-23T11:00:00Z WARN wanted\n context\n");
}

#[test]
fn log_history_tail_stops_before_materializing_older_files() {
    let f = Fixture::new();
    std::fs::write(f.rotation(1), "older\n".repeat(200_000)).unwrap();
    std::fs::write(
        f.paths.log_file(),
        format!(
            "{}2026-10-23T12:00:00Z INFO latest\n",
            "2026-10-23T11:00:00Z INFO old\n".repeat(20_000)
        ),
    )
    .unwrap();
    let (out, summary) = f.history(1, |_| true);
    assert_eq!(out, "2026-10-23T12:00:00Z INFO latest\n");
    assert_eq!(summary.read.files_read, 1);
    assert!(summary.read.bytes_read < 20_000, "{:?}", summary.read);
}

#[test]
fn log_history_long_lines_and_entries_are_bounded_and_reported() {
    let f = Fixture::new();
    let text = format!(
        "2026-10-23T10:00:00Z WARN header\n{}\n",
        " continuation\n".repeat(20_000)
    );
    std::fs::write(f.paths.log_file(), text).unwrap();
    for n in [0, 1] {
        let (out, summary) = f.history(n, |_| true);
        assert_eq!(summary.entries, 1);
        assert!(out.len() <= formless_core::logs::MAX_LOG_LINE_BYTES + 1);
        assert!(summary.truncated_entries > 0);
        assert!(out.contains("[log entry truncated]"));
        assert!(out.starts_with("2026-10-23T10:00:00Z WARN header"));
    }
    std::fs::write(
        f.paths.log_file(),
        format!("2026-10-23T10:00:00Z WARN {}\n", "x".repeat(200_000)),
    )
    .unwrap();
    let (_, summary) = f.history(1, |_| true);
    assert_eq!(summary.read.truncated_lines, 1);
}

#[test]
fn log_history_writer_failures_are_propagated_instead_of_hidden() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture output failed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    std::fs::write(f.paths.log_file(), "2026-10-23T10:00:00Z INFO value\n").unwrap();
    assert!(log_output::history(&f.paths, 0, &|_| true, &mut Broken)
        .unwrap_err()
        .to_string()
        .contains("fixture output failed"));
}

#[test]
fn log_report_uses_recent_rotations_and_retains_existing_redaction() {
    let f = Fixture::new();
    let older = (0..250).map(|n| format!("rot-{n}\n")).collect::<String>();
    std::fs::write(f.rotation(1), older).unwrap();
    std::fs::write(
        f.paths.log_file(),
        "api_key = \"sk-test-1234567890abcdef\"\n",
    )
    .unwrap();
    let report = bug_report_log(&f.paths);
    assert!(report.contains("last 200 lines"));
    assert!(report.contains("\nrot-51\n"));
    assert!(!report.contains("\nrot-50\n"));
    assert!(report.contains("rot-249"));
    assert!(!report.contains("sk-test-1234567890abcdef"));
    assert!(!report.contains("read limit"));
    std::fs::remove_file(f.paths.log_file()).unwrap();
    assert!(
        bug_report_log(&f.paths).contains("rot-249"),
        "missing live file must not hide rotations"
    );
}

#[test]
fn log_report_byte_limit_is_visible_and_partial_leading_content_is_omitted() {
    let f = Fixture::new();
    std::fs::write(
        f.paths.log_file(),
        format!("{}\nlatest\n", "private-prefix".repeat(100_000)),
    )
    .unwrap();
    let report = bug_report_log(&f.paths);
    assert!(report.contains("1 MiB read limit"));
    assert!(report.contains("leading boundary line was omitted"));
    assert!(report.contains("\nlatest\n"));
    assert!(!report.contains("private-prefix"));
    assert!(report.len() < 2048);
}

#[test]
fn log_follow_append_stream_is_bounded_and_uses_lossy_decoding() {
    let f = Fixture::new();
    let prefix = "old ignored\n";
    let mut text = prefix.as_bytes().to_vec();
    text.extend_from_slice(b"2026-10-23T10:00:00Z WARN damaged \xff\n continuation\n");
    std::fs::write(f.paths.log_file(), &text).unwrap();
    let mut out = Vec::new();
    let summary = log_output::appended(
        &f.paths.log_file(),
        prefix.len() as u64,
        text.len() as u64,
        &|e| e.level <= 1,
        &mut out,
    )
    .unwrap();
    assert_eq!(summary.entries, 1);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("damaged �\n continuation"));
    assert!(!out.contains("old ignored"));
}

#[test]
fn log_history_headerless_giant_line_is_retained_in_all_and_tail_modes() {
    let f = Fixture::new();
    std::fs::write(f.paths.log_file(), "x".repeat(200_000)).unwrap();
    let (all, all_summary) = f.history(0, |_| true);
    let (tail, tail_summary) = f.history(1, |_| true);
    assert_eq!(all_summary.entries, 1);
    assert_eq!(tail_summary.entries, 1);
    assert_eq!(tail, all);
    assert!(tail.contains("[log line truncated]"));
    assert!(tail.len() <= formless_core::logs::MAX_LOG_LINE_BYTES + 1);
}
