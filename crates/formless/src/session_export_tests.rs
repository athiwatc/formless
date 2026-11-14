//! Argument/format decisions and temporary-file IO only; never export a real Session.
use super::{Cli, Cmd, SessionExportFormat as Format, SessionsAction};
use clap::{CommandFactory, Parser};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

fn parsed(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("formless").chain(args.iter().copied())).unwrap()
}
fn selected(cli: &Cli) -> Format {
    match &cli.cmd {
        Some(Cmd::Sessions {
            action: SessionsAction::Export { format, output, .. },
        }) => super::session_export_format(*format, output.as_deref()).unwrap(),
        _ => panic!("expected export parser fixture"),
    }
}

#[test]
fn export_parser_accepts_only_known_formats_and_exposes_help_completion_values() {
    for (name, expected) in [
        ("json", Format::Json),
        ("markdown", Format::Markdown),
        ("md", Format::Markdown),
        ("html", Format::Html),
    ] {
        let cli = parsed(&["sessions", "export", "fixture-id", "--format", name]);
        assert_eq!(selected(&cli), expected);
    }
    for bad in ["pdf", "htm", "xml"] {
        let error = Cli::try_parse_from([
            "formless",
            "sessions",
            "export",
            "fixture-id",
            "--format",
            bad,
        ])
        .err()
        .unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        for good in ["json", "markdown", "html"] {
            assert!(error.to_string().contains(good));
        }
    }
    let mut root = Cli::command();
    root.build();
    let export = root
        .find_subcommand("sessions")
        .unwrap()
        .find_subcommand("export")
        .unwrap();
    let format = export
        .get_arguments()
        .find(|arg| arg.get_id() == "format")
        .unwrap();
    let values = format.get_possible_values();
    assert_eq!(
        values
            .iter()
            .map(|value| value.get_name())
            .collect::<Vec<_>>(),
        ["json", "markdown", "html"]
    );
    assert!(values.iter().all(|value| value.get_help().is_some()));
    let help = export.clone().render_long_help().to_string();
    for text in ["json", "markdown", "html", "infer", "output extension"] {
        assert!(help.contains(text), "{help}");
    }
}

#[test]
fn export_infers_case_normalized_extensions_and_keeps_json_fallbacks() {
    for (name, expected) in [
        ("chat.md", Format::Markdown),
        ("chat.MARKDOWN", Format::Markdown),
        ("chat.html", Format::Html),
        ("chat.HtM", Format::Html),
        ("chat.JSON", Format::Json),
        ("chat.archive", Format::Json),
        ("chat", Format::Json),
    ] {
        assert_eq!(
            selected(&parsed(&["sessions", "export", "fixture-id", "-o", name])),
            expected,
            "{name}"
        );
    }
    assert_eq!(
        selected(&parsed(&["sessions", "export", "fixture-id"])),
        Format::Json
    );
    assert_eq!(
        selected(&parsed(&[
            "sessions",
            "export",
            "fixture-id",
            "--format",
            "md"
        ])),
        Format::Markdown
    );
    assert_eq!(
        selected(&parsed(&[
            "sessions",
            "export",
            "fixture-id",
            "--format",
            "html",
            "-o",
            "unknown.suffix"
        ])),
        Format::Html
    );
    assert_eq!(
        selected(&parsed(&[
            "sessions",
            "export",
            "fixture-id",
            "--format",
            "markdown",
            "-o",
            "chat.MD"
        ])),
        Format::Markdown
    );
    assert_eq!(Format::Markdown.wire_name(), "markdown");
}

#[test]
fn export_conflicts_are_early_usage_errors_without_dispatch_or_profile_resolution() {
    for (format, output) in [
        ("json", "chat.md"),
        ("md", "chat.html"),
        ("html", "chat.json"),
    ] {
        let cli = parsed(&[
            "sessions",
            "export",
            "does-not-exist",
            "--format",
            format,
            "-o",
            output,
        ]);
        // This is the same pure preflight run_cli calls before profile setup;
        // no run_cli/session lookup/daemon dispatch is invoked by this test.
        let error = super::validate_cli_export_format(&cli).unwrap_err();
        assert_eq!(super::exit_code(&error), 2);
        let text = error.to_string();
        assert!(
            text.contains("conflicts") && text.contains(output),
            "{text}"
        );
    }
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "formless-export-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
    fn entries(&self) -> Vec<std::ffi::OsString> {
        let mut names = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn export_atomic_replacement_and_io_failures_preserve_destinations() {
    let dir = Temp::new();
    let file = dir.join("chat.md");
    std::fs::write(&file, "old complete export").unwrap();
    let new = "complete replacement\n".repeat(8192);
    super::write_session_export(&file, new.as_bytes()).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), new);
    assert_eq!(dir.entries(), vec![std::ffi::OsString::from("chat.md")]);
    // Actual rename failure after staging: a file cannot replace this nonempty
    // directory. Existing content and the surrounding directory remain intact.
    let blocked = dir.join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("existing-export.md"), "kept complete export").unwrap();
    assert!(super::write_session_export(&blocked, b"never committed").is_err());
    assert_eq!(
        std::fs::read_to_string(blocked.join("existing-export.md")).unwrap(),
        "kept complete export"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), new);
    assert_eq!(
        dir.entries(),
        vec![
            std::ffi::OsString::from("blocked"),
            std::ffi::OsString::from("chat.md")
        ]
    );
    assert!(super::write_session_export(&dir.join("missing/chat.md"), b"not written").is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), new);
    assert!(!dir.join("missing").exists());
}

#[cfg(unix)]
#[test]
fn export_atomic_replacement_keeps_existing_reader_snapshot_and_file_mode() {
    use std::{io::Read, os::unix::fs::PermissionsExt};
    let dir = Temp::new();
    let file = dir.join("chat.md");
    std::fs::write(&file, "old snapshot").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let mut reader = std::fs::File::open(&file).unwrap();
    super::write_session_export(&file, b"new complete export").unwrap();
    let mut old = String::new();
    reader.read_to_string(&mut old).unwrap();
    assert_eq!(
        old, "old snapshot",
        "atomic replacement cannot truncate a reader's old inode"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "new complete export"
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o640
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    let previously_writable = std::fs::OpenOptions::new().write(true).open(&file).is_ok();
    let result = super::write_session_export(&file, b"same write-access semantics");
    assert_eq!(
        result.is_ok(),
        previously_writable,
        "atomic output preserves effective write-open permissions"
    );
    if !previously_writable {
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "new complete export"
        );
    }
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o444
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
}

#[cfg(unix)]
#[test]
fn export_private_staging_and_partial_failure_do_not_expose_or_replace_old_data() {
    use std::{io::Write, os::unix::fs::PermissionsExt};
    let dir = Temp::new();
    let file = dir.join("private.md");
    std::fs::write(&file, "previous complete private export").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let expected = dir.entries();
    let error = super::atomic_session_export_with(&file, |staged| {
        assert_eq!(
            staged.metadata()?.permissions().mode() & 0o777,
            0o600,
            "private before the first content byte"
        );
        staged.write_all(b"incomplete private replacement")?;
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "previous complete private export"
        );
        Err(std::io::Error::other(
            "injected write failure after partial output",
        ))
    })
    .unwrap_err();
    assert!(error.to_string().contains("injected write failure"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "previous complete private export"
    );
    assert_eq!(dir.entries(), expected, "failed partial stage is removed");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Compare a newly created export with the old writer under the actual
    // process umask; tests do not mutate global umask/environment state.
    let ordinary = dir.join("ordinary.md");
    let exported = dir.join("new-export.md");
    std::fs::write(&ordinary, "ordinary new output").unwrap();
    super::write_session_export(&exported, b"new complete output").unwrap();
    assert_eq!(
        std::fs::metadata(ordinary).unwrap().permissions().mode() & 0o777,
        std::fs::metadata(exported).unwrap().permissions().mode() & 0o777
    );
}

#[cfg(unix)]
#[test]
fn export_preserves_relative_and_dangling_leaf_links_and_bounds_cycles() {
    use std::os::unix::fs::symlink;
    let dir = Temp::new();
    let target = dir.join("target.md");
    let link = dir.join("link.md");
    std::fs::write(&target, "old").unwrap();
    symlink("target.md", &link).unwrap();
    super::write_session_export(&link, b"new").unwrap();
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    let dangling = dir.join("dangling.md");
    symlink("future.md", &dangling).unwrap();
    super::write_session_export(&dangling, b"created through existing link").unwrap();
    assert!(std::fs::symlink_metadata(&dangling)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_to_string(dir.join("future.md")).unwrap(),
        "created through existing link"
    );
    let cycle = dir.join("cycle.md");
    symlink("cycle.md", &cycle).unwrap();
    assert!(super::write_session_export(&cycle, b"not committed").is_err());
    assert_eq!(std::fs::read_link(&cycle).unwrap(), Path::new("cycle.md"));
}

#[cfg(unix)]
#[test]
fn export_descriptor_aliases_are_not_regular_file_replacements() {
    for name in [
        "/dev/stdout",
        "/dev/stderr",
        "/dev/fd/1",
        "/proc/self/fd/1",
        "/proc/thread-self/fd/2",
        "/proc/123/task/456/fd/7",
    ] {
        assert!(super::export_descriptor_path(Path::new(name)), "{name}");
    }
    assert!(!super::export_descriptor_path(Path::new("/tmp/stdout")));
}

#[cfg(target_os = "linux")]
#[test]
fn export_redirected_descriptor_keeps_its_underlying_file() {
    let dir = Temp::new();
    let output = dir.join("redirected.md");
    let alias = dir.join("alias.md");
    std::os::unix::fs::symlink("/proc/self/fd/1", &alias).unwrap();
    let opened = std::fs::File::create(&output).unwrap();
    use std::os::unix::fs::MetadataExt;
    let inode = opened.metadata().unwrap().ino();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "session_export_tests::descriptor_child_fixture",
            "--nocapture",
        ])
        .env("FORMLESS_EXPORT_DESCRIPTOR_FIXTURE", &alias)
        .stdout(opened)
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("descriptor fixture exceeded bounded wait");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success());
    assert_eq!(
        std::fs::metadata(&output).unwrap().ino(),
        inode,
        "must write the descriptor, not atomically detach it"
    );
    assert!(std::fs::read_to_string(&output)
        .unwrap()
        .contains("descriptor fixture body"));
    assert!(std::fs::symlink_metadata(&alias)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_child_fixture() {
    let Some(path) = std::env::var_os("FORMLESS_EXPORT_DESCRIPTOR_FIXTURE") else {
        return;
    };
    super::write_session_export(Path::new(&path), b"descriptor fixture body").unwrap();
    std::process::exit(0);
}

#[cfg(windows)]
#[test]
fn export_windows_descriptor_aliases_remain_streams() {
    for name in [
        "NUL",
        "CON",
        "CONOUT$",
        "COM1",
        "LPT9.txt",
        r"C:\output\NUL.txt",
    ] {
        assert!(super::export_descriptor_path(Path::new(name)), "{name}");
    }
    assert!(!super::export_descriptor_path(Path::new(
        r"C:\output\chat.md"
    )));
}
