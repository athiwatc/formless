//! Daemon log rotation (SPEC §9.4): size-triggered, bounded history.

use formless_core::logs::{log_usage, rotate_if_needed};
use formless_core::Paths;

#[test]
fn logs_rotate_past_the_size_cap_and_keep_a_bounded_history() {
    let base = std::env::temp_dir().join(format!("fl-logs-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    let log = paths.log_file();
    let rotated = |n: usize| format!("{}.{n}", log.display());

    std::fs::write(&log, "small").unwrap();
    assert!(!rotate_if_needed(&paths, 100, 2).unwrap(), "under the cap");

    for round in 1..=3 {
        std::fs::write(&log, format!("round {round} {}", "x".repeat(200))).unwrap();
        assert!(rotate_if_needed(&paths, 100, 2).unwrap());
        assert!(!log.exists(), "the daemon reopens a fresh file");
    }
    // Newest first; only `keep` files survive.
    assert!(std::fs::read_to_string(rotated(1))
        .unwrap()
        .starts_with("round 3"));
    assert!(std::fs::read_to_string(rotated(2))
        .unwrap()
        .starts_with("round 2"));
    assert!(!std::path::Path::new(&rotated(3)).exists());
    assert_eq!(log_usage(&paths), 2 * ("round 3 ".len() as u64 + 200));

    // keep = 0 discards the old log; max = 0 disables rotation.
    std::fs::write(&log, "y".repeat(300)).unwrap();
    assert!(!rotate_if_needed(&paths, 0, 2).unwrap());
    assert!(rotate_if_needed(&paths, 100, 0).unwrap());
    assert!(!log.exists());
}

struct Fixture {
    paths: Paths,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-log-history-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        Self { root, paths }
    }
    fn rotation(&self, suffix: &str) -> std::path::PathBuf {
        let mut name = self.paths.log_file().into_os_string();
        name.push(format!(".{suffix}"));
        name.into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn numeric_discovery_counts_all_history_and_ignores_lookalikes_and_nonfiles() {
    use formless_core::logs::rotated_logs;
    let f = Fixture::new();
    std::fs::write(f.paths.log_file(), "live").unwrap();
    for suffix in ["1", "21", "65", "100", "999999999999999999999999999999"] {
        std::fs::write(f.rotation(suffix), "history").unwrap();
    }
    for suffix in ["foo", "1.bak", "-1", "0"] {
        std::fs::write(f.rotation(suffix), "ignored").unwrap();
    }
    std::fs::create_dir(f.rotation("200")).unwrap();
    #[cfg(unix)]
    {
        let target = f.root.join("private-sentinel");
        std::fs::write(&target, "not a log").unwrap();
        std::os::unix::fs::symlink(&target, f.rotation("300")).unwrap();
    }
    let found = rotated_logs(&f.paths).unwrap();
    assert_eq!(
        found.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        [1, 21, 65, 100, u64::MAX]
    );
    assert_eq!(log_usage(&f.paths), 4 + 5 * 7);
    assert!(!rotate_if_needed(&f.paths, 0, 1).unwrap());
    assert_eq!(log_usage(&f.paths), 4 + 7);
    assert!(f.rotation("foo").exists() && f.rotation("200").is_dir());
    #[cfg(unix)]
    assert!(std::fs::symlink_metadata(f.rotation("300"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn retention_reductions_apply_under_cap_and_when_live_rotation_is_disabled() {
    let f = Fixture::new();
    for n in 1..=4 {
        std::fs::write(f.rotation(&n.to_string()), "old").unwrap();
    }
    std::fs::write(f.paths.log_file(), "x".repeat(200)).unwrap();
    assert!(rotate_if_needed(&f.paths, 100, 4).unwrap());
    assert_eq!(
        formless_core::logs::rotated_logs(&f.paths).unwrap().len(),
        4
    );
    std::fs::write(f.paths.log_file(), "small").unwrap();
    assert!(!rotate_if_needed(&f.paths, 100, 1).unwrap());
    assert_eq!(
        formless_core::logs::rotated_logs(&f.paths).unwrap().len(),
        1
    );
    assert_eq!(log_usage(&f.paths), 5 + 200);
    assert!(!rotate_if_needed(&f.paths, 0, 0).unwrap());
    assert_eq!(
        log_usage(&f.paths),
        5,
        "disabled size rotation still enforces history retention"
    );
    assert!(formless_core::logs::rotated_logs(&f.paths)
        .unwrap()
        .is_empty());
    std::fs::write(f.paths.log_file(), "x".repeat(200)).unwrap();
    assert!(rotate_if_needed(&f.paths, 100, 0).unwrap());
    assert_eq!(log_usage(&f.paths), 0);
}

#[test]
fn invalid_keep_is_rejected_before_any_cleanup_or_rename() {
    let f = Fixture::new();
    std::fs::write(f.paths.log_file(), "live").unwrap();
    std::fs::write(f.rotation("65"), "old").unwrap();
    let error = rotate_if_needed(&f.paths, 1, usize::MAX).unwrap_err();
    assert!(error.to_string().contains("0 and 64"));
    assert_eq!(std::fs::read_to_string(f.paths.log_file()).unwrap(), "live");
    assert_eq!(std::fs::read_to_string(f.rotation("65")).unwrap(), "old");
}

#[test]
fn log_keep_bounds_cover_loads_saves_and_preserve_file_only_setting_policy() {
    use formless_core::Config;
    let f = Fixture::new();
    for value in [0, 64] {
        std::fs::write(
            f.paths.config_file(),
            format!("[daemon]\nlog_keep={value}\n"),
        )
        .unwrap();
        assert_eq!(Config::load(&f.paths).unwrap().daemon.log_keep, value);
        assert_eq!(
            Config::load_file_only(&f.paths).unwrap().daemon.log_keep,
            value
        );
    }
    std::fs::write(f.paths.config_file(), "[daemon]\nlog_keep=65\n").unwrap();
    for result in [Config::load(&f.paths), Config::load_file_only(&f.paths)] {
        assert!(format!("{:#}", result.unwrap_err()).contains("0 and 64"));
    }
    let mut cfg = Config::default();
    let before = cfg.daemon.log_keep;
    for value in [64, 65] {
        let error = cfg
            .set_key("daemon.log_keep", serde_json::json!(value))
            .unwrap_err();
        assert!(error.to_string().contains("cannot be changed this way"));
    }
    assert_eq!(cfg.daemon.log_keep, before);
    cfg.daemon.log_keep = 500;
    assert!(cfg
        .save(&f.paths)
        .unwrap_err()
        .to_string()
        .contains("0 and 64"));
    assert_eq!(
        std::fs::read_to_string(f.paths.config_file()).unwrap(),
        "[daemon]\nlog_keep=65\n"
    );
    let empty: Config = toml::from_str("").unwrap();
    assert_eq!(empty.daemon.log_keep, 5);
}

#[test]
fn forward_and_reverse_lines_preserve_unicode_crlf_and_unterminated_lines() {
    use formless_core::logs::visit_lines;
    let f = Fixture::new();
    let large = format!("{}👩‍💻e\u{301}\nlast", "x".repeat(8190));
    for text in ["", "\n", "a\n", "a\n\n", "a\r\nb", "a\r", &large] {
        std::fs::write(f.paths.log_file(), text).unwrap();
        let expected: Vec<String> = text.lines().map(str::to_string).collect();
        for reverse in [false, true] {
            let mut actual = Vec::new();
            let read = visit_lines(&f.paths, reverse, |line| {
                actual.push(line);
                Ok(true)
            })
            .unwrap();
            if reverse {
                actual.reverse();
            }
            assert_eq!(actual, expected, "{text:?} reverse={reverse}");
            assert_eq!(read.truncated_lines, 0);
        }
    }
    std::fs::write(
        f.paths.log_file(),
        b"ok\n2026-10-20T00:00:00Z INFO damaged \xff\n",
    )
    .unwrap();
    let mut lines = Vec::new();
    visit_lines(&f.paths, true, |line| {
        lines.push(line);
        Ok(true)
    })
    .unwrap();
    assert!(lines[0].contains('�'));
}

#[test]
fn reverse_early_stop_reads_only_tail_blocks_and_giant_lines_are_bounded() {
    use formless_core::logs::{visit_lines, MAX_LOG_LINE_BYTES};
    let f = Fixture::new();
    std::fs::write(
        f.paths.log_file(),
        format!("{}latest\n", "old\n".repeat(100_000)),
    )
    .unwrap();
    let read = visit_lines(&f.paths, true, |line| {
        assert_eq!(line, "latest");
        Ok(false)
    })
    .unwrap();
    assert!(read.bytes_read < 20_000, "{read:?}");
    std::fs::write(
        f.paths.log_file(),
        format!("{}\n", "z".repeat(MAX_LOG_LINE_BYTES * 4)),
    )
    .unwrap();
    for reverse in [false, true] {
        let mut lines = Vec::new();
        let read = visit_lines(&f.paths, reverse, |line| {
            lines.push(line);
            Ok(true)
        })
        .unwrap();
        assert_eq!(read.truncated_lines, 1);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].len() <= MAX_LOG_LINE_BYTES);
        assert!(lines[0].ends_with("[log line truncated]"));
    }
}

#[test]
fn raw_tail_uses_rotations_and_caps_read_bytes_without_exposing_partial_prefixes() {
    use formless_core::logs::tail_lines;
    let f = Fixture::new();
    std::fs::write(f.rotation("21"), "oldest\n").unwrap();
    std::fs::write(f.rotation("1"), "recent\n").unwrap();
    std::fs::write(f.paths.log_file(), "live\n").unwrap();
    let tail = tail_lines(&f.paths, 200, 1024).unwrap();
    assert_eq!(tail.lines, ["oldest", "recent", "live"]);
    assert!(!tail.byte_limit_hit);
    std::fs::write(
        f.paths.log_file(),
        format!("{}\nlatest\n", "private-prefix".repeat(100_000)),
    )
    .unwrap();
    let tail = tail_lines(&f.paths, 200, 4096).unwrap();
    assert!(tail.byte_limit_hit);
    assert!(tail.bytes_read <= 4096);
    assert_eq!(
        tail.lines,
        ["latest"],
        "partial leading line is omitted, not passed to redaction as complete"
    );
    let enough = tail_lines(&f.paths, 1, 4096).unwrap();
    assert_eq!(enough.lines, ["latest"]);
    assert!(
        !enough.byte_limit_hit,
        "one complete requested line was found within budget"
    );
}

#[test]
fn oversized_decimal_rotation_indices_still_sort_by_numeric_value() {
    let f = Fixture::new();
    let lower = "999999999999999999999999999999";
    let higher = "1000000000000000000000000000000";
    std::fs::write(f.rotation(higher), "higher").unwrap();
    std::fs::write(f.rotation(lower), "lower").unwrap();
    let files = formless_core::logs::rotated_logs(&f.paths).unwrap();
    assert_eq!(files[0].1, f.rotation(lower));
    assert_eq!(files[1].1, f.rotation(higher));
}

#[cfg(unix)]
#[test]
fn usage_preserves_known_live_bytes_when_directory_listing_is_unavailable() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    std::fs::write(f.paths.log_file(), "live").unwrap();
    std::fs::write(f.rotation("1"), "history").unwrap();
    std::fs::set_permissions(&f.paths.data_dir, std::fs::Permissions::from_mode(0o111)).unwrap();
    let listing = formless_core::logs::rotated_logs(&f.paths);
    let usage = log_usage(&f.paths);
    std::fs::set_permissions(&f.paths.data_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Root may bypass mode bits; in an ordinary account this proves the
    // discovery-error path. Both environments retain their known subtotal.
    assert_eq!(usage, if listing.is_err() { 4 } else { 11 });
}
