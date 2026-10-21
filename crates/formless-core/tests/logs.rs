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
