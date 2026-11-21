//! Launch the separately shipped CLI without linking daemon code into the GUI.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn is_bundle(executable: &Path) -> bool {
    let Some(macos) = executable.parent() else {
        return false;
    };
    macos.file_name() == Some(OsStr::new("MacOS"))
        && macos.parent().and_then(Path::file_name) == Some(OsStr::new("Contents"))
        && macos
            .parent()
            .and_then(Path::parent)
            .and_then(Path::extension)
            == Some(OsStr::new("app"))
}

pub fn should_start(executable: &Path, args: &[String], explicit: bool, connected: bool) -> bool {
    !connected
        && (explicit
            || (is_bundle(executable)
                && !args.iter().any(|a| {
                    matches!(
                        a.as_str(),
                        "--sessions"
                            | "--blocks"
                            | "--huge-block-mb"
                            | "--smoke"
                            | "--stream-seconds"
                            | "--stream-rate"
                            | "--stream-markdown"
                            | "--dump-a11y"
                            | "--dump-platform-a11y"
                            | "--activate-a11y"
                            | "--lifetime"
                            | "--open-panel"
                    )
                })))
}

pub fn find_cli(
    executable: &Path,
    override_bin: Option<OsString>,
    path: Option<OsString>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "formless.exe"
    } else {
        "formless"
    };
    executable
        .parent()
        .map(|p| p.join(name))
        .into_iter()
        .chain(override_bin.filter(|p| !p.is_empty()).map(PathBuf::from))
        .chain(path.into_iter().flat_map(|p| {
            std::env::split_paths(&p)
                .map(|p| p.join(name))
                .collect::<Vec<_>>()
        }))
        .find(|p| exists(p))
}

fn run(cli: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new(cli)
        .args(args)
        .output()
        .map_err(|e| format!("{}: {e}", cli.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} {} failed ({}): {}",
            cli.display(),
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}

pub fn start() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let cli = find_cli(
        &executable,
        std::env::var_os("FORMLESS_BIN"),
        std::env::var_os("PATH"),
        |p| p.is_file(),
    )
    .ok_or("cannot find formless CLI beside desktop, in FORMLESS_BIN, or on PATH")?;
    run(&cli, &["daemon", "start"])?;
    let status: serde_json::Value =
        serde_json::from_slice(&run(&cli, &["daemon", "status", "--json"])?)
            .map_err(|e| format!("invalid daemon status: {e}"))?;
    status["endpoint"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .ok_or("daemon status has no endpoint; use a matching formless CLI".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundle_and_launch_modes() {
        let app = Path::new("/Applications/Formless.app/Contents/MacOS/formless-desktop");
        assert!(is_bundle(app));
        assert!(!is_bundle(Path::new("/tmp/Contents/MacOS/desktop")));
        assert!(!is_bundle(Path::new("desktop")));
        assert!(should_start(app, &[], false, false));
        assert!(!should_start(app, &["--smoke".into()], false, false));
        assert!(!should_start(app, &[], true, true));
        assert!(should_start(Path::new("desktop"), &[], true, false));
    }
    #[test]
    fn lookup_priority_and_missing_cli() {
        let exe = Path::new("/bundle/desktop");
        let name = if cfg!(windows) {
            "formless.exe"
        } else {
            "formless"
        };
        let sibling = Path::new("/bundle").join(name);
        let override_bin = PathBuf::from("/override/cli");
        let on_path = Path::new("/path").join(name);
        for (available, expected) in [
            (
                vec![sibling.clone(), override_bin.clone(), on_path.clone()],
                Some(sibling),
            ),
            (
                vec![override_bin.clone(), on_path.clone()],
                Some(override_bin.clone()),
            ),
            (vec![on_path.clone()], Some(on_path)),
            (vec![], None),
        ] {
            assert_eq!(
                find_cli(
                    exe,
                    Some(override_bin.clone().into_os_string()),
                    Some("/path".into()),
                    |p| available.contains(&p.to_path_buf())
                ),
                expected
            );
        }
    }
}
