//! Resolve exactly the program that will be guarded and passed to Command.
//! Never check a parent's PATH and subsequently execute a child's other PATH.
use super::{caps, process_grant, PROCESS_BASE_ENV};
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) struct ChildEnv {
    values: Vec<(OsString, Option<OsString>)>,
    #[cfg(windows)]
    parent_path: Option<OsString>,
}
pub(super) struct Program {
    pub raw: PathBuf,
    pub canonical: PathBuf,
}
impl ChildEnv {
    pub(super) fn capture(grants: &[String], overrides: &Value) -> Self {
        let parent_path = std::env::var_os("PATH");
        let mut values = Vec::new();
        for key in PROCESS_BASE_ENV
            .iter()
            .copied()
            .chain(grants.iter().map(String::as_str))
        {
            if key.to_ascii_uppercase().starts_with("FORMLESS_") {
                continue;
            }
            let value = if key_eq(OsStr::new(key), "PATH") {
                parent_path.clone()
            } else {
                std::env::var_os(key)
            };
            if let Some(value) = value {
                values.push((key.into(), Some(value)));
            }
        }
        if let Some(overrides) = overrides.as_object() {
            for (key, value) in overrides {
                let value = match value {
                    Value::Null => None,
                    Value::String(value) => Some(value.into()),
                    value => Some(value.to_string().into()),
                };
                values.push((key.into(), value));
            }
        }
        Self {
            values,
            #[cfg(windows)]
            parent_path,
        }
    }
    fn get(&self, key: &str) -> Option<&OsStr> {
        self.values
            .iter()
            .rev()
            .find(|(name, _)| key_eq(name, key))
            .and_then(|(_, value)| value.as_deref())
    }
    pub(super) fn apply(&self, command: &mut Command) {
        command.env_clear();
        for (key, value) in &self.values {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
    }
}
fn key_eq(name: &OsStr, expected: &str) -> bool {
    #[cfg(windows)]
    {
        name.to_string_lossy().eq_ignore_ascii_case(expected)
    }
    #[cfg(not(windows))]
    {
        name == OsStr::new(expected)
    }
}
fn explicit(program: &str) -> bool {
    program.contains('/') || (cfg!(windows) && program.contains(['\\', ':']))
}
fn actual_names(program: &str) -> Vec<OsString> {
    #[cfg(windows)]
    if Path::new(program).extension().is_none() {
        // Rust Command does not use PATHEXT to implicitly select batch files.
        return vec![format!("{program}.exe").into(), program.into()];
    }
    vec![program.into()]
}
fn candidate(raw: PathBuf) -> Option<Program> {
    if !raw.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = std::ffi::CString::new(raw.as_os_str().as_bytes()).ok()?;
        // Check effective-user execute access, including ACLs, so a non-executable
        // earlier PATH entry does not hide the actual later executable.
        if unsafe { libc::faccessat(libc::AT_FDCWD, bytes.as_ptr(), libc::X_OK, libc::AT_EACCESS) }
            != 0
        {
            return None;
        }
    }
    let canonical = caps::canonical_path(raw.to_str()?).ok()?;
    Some(Program { raw, canonical })
}
fn in_dirs(
    dirs: impl IntoIterator<Item = PathBuf>,
    names: &[OsString],
    cwd: &Path,
) -> Result<Option<Program>, String> {
    for dir in dirs {
        let dir = caps::lexical_absolute(cwd, &dir)?;
        for name in names {
            let raw = caps::lexical_absolute(&dir, Path::new(name))?;
            if let Some(program) = candidate(raw) {
                return Ok(Some(program));
            }
        }
    }
    Ok(None)
}
fn in_path(path: &OsStr, names: &[OsString], cwd: &Path) -> Result<Option<Program>, String> {
    in_dirs(std::env::split_paths(path), names, cwd)
}
#[cfg(unix)]
fn default_path() -> Result<OsString, String> {
    use std::os::unix::ffi::OsStringExt;
    // POSIX execvp's platform default, rather than accidentally inheriting the
    // parent's PATH after an explicit child env_remove("PATH").
    let len = unsafe { libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0) };
    if len == 0 {
        return Err("cannot establish the native default executable PATH".into());
    }
    let mut bytes = vec![0u8; len];
    let written = unsafe { libc::confstr(libc::_CS_PATH, bytes.as_mut_ptr().cast(), bytes.len()) };
    if written == 0 || written > bytes.len() {
        return Err("cannot read the native default executable PATH".into());
    }
    bytes.truncate(written.saturating_sub(1));
    Ok(OsString::from_vec(bytes))
}
#[cfg(windows)]
fn native_windows_dirs() -> Result<Vec<PathBuf>, String> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::{
        GetSystemDirectoryW, GetWindowsDirectoryW,
    };
    let mut dirs = Vec::new();
    if let Some(parent) = std::env::current_exe().map_err(|e| e.to_string())?.parent() {
        dirs.push(parent.to_path_buf());
    }
    for get in [GetSystemDirectoryW, GetWindowsDirectoryW] {
        let mut buffer = vec![0u16; 32768];
        // SAFETY: native directory APIs receive a valid sized UTF-16 buffer.
        let len = unsafe { get(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if len == 0 || len >= buffer.len() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        dirs.push(PathBuf::from(OsString::from_wide(&buffer[..len])));
    }
    Ok(dirs)
}
fn search(names: &[OsString], cwd: &Path, env: &ChildEnv) -> Result<Option<Program>, String> {
    if let Some(path) = env.get("PATH") {
        if let Some(program) = in_path(path, names, cwd)? {
            return Ok(Some(program));
        }
    }
    #[cfg(unix)]
    if env.get("PATH").is_none() {
        return in_path(&default_path()?, names, cwd);
    }
    #[cfg(windows)]
    {
        // Preserve Rust's documented Windows lookup order. The parent's PATH
        // is a fallback even when the child's PATH was explicitly removed.
        // https://doc.rust-lang.org/std/process/struct.Command.html#platform-specific-behavior
        if let Some(program) = in_dirs(native_windows_dirs()?, names, cwd)? {
            return Ok(Some(program));
        }
        if let Some(parent) = &env.parent_path {
            return in_path(parent, names, cwd);
        }
    }
    Ok(None)
}
pub(super) fn resolve(program: &str, cwd: &Path, env: &ChildEnv) -> Result<Program, String> {
    if program.is_empty() || program.contains('\0') {
        return Err("process executable must be a nonempty path without NUL".into());
    }
    let found = if explicit(program) {
        let raw = caps::lexical_absolute(cwd, Path::new(program))?;
        #[cfg(windows)]
        {
            let parent = raw
                .parent()
                .ok_or("executable has no parent directory")?
                .to_path_buf();
            let name = raw
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or("executable is not UTF-8")?;
            in_dirs([parent], &actual_names(name), cwd)?
        }
        #[cfg(not(windows))]
        {
            candidate(raw)
        }
    } else {
        search(&actual_names(program), cwd, env)?
    };
    found.ok_or_else(|| {
        format!("{program}: executable not found in the effective child PATH or not executable")
    })
}
fn grant_program(grant: &str, cwd: &Path, env: &ChildEnv) -> Result<Program, String> {
    #[cfg(windows)]
    if !explicit(grant) {
        // Preserve existing basename-grant matching for explicit .cmd/.bat
        // paths. PATHEXT is a grant spelling convention, not an instruction to
        // invent shell parsing or broaden actual implicit executable lookup.
        let extensions = env
            .get("PATHEXT")
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".EXE;.CMD;.BAT".into());
        let names: Vec<OsString> = extensions
            .split(';')
            .map(|ext| format!("{grant}{ext}").into())
            .chain([OsString::from(grant)])
            .collect();
        return search(&names, cwd, env)?
            .ok_or_else(|| "process grant executable was not found".into());
    }
    resolve(grant, cwd, env)
}
pub(super) fn allowed(
    grants: &[String],
    requested: &str,
    first_arg: Option<&str>,
    actual: &Program,
    cwd: &Path,
    env: &ChildEnv,
) -> bool {
    grants.is_empty()
        || grants.iter().any(|grant| {
            let (name, subcommands) = process_grant(grant);
            if subcommands.is_some_and(|subs| !first_arg.is_some_and(|arg| subs.contains(&arg))) {
                return false;
            }
            if !explicit(name) && !explicit(requested) {
                return name == requested;
            }
            grant_program(name, cwd, env).is_ok_and(|grant| grant.canonical == actual.canonical)
        })
}

/// Reject impossible names/subcommands before probing executable paths. A path
/// grant may need canonical identity comparison, but two different bare names
/// never authorize one another.
pub(super) fn possibly_allowed(
    grants: &[String],
    requested: &str,
    first_arg: Option<&str>,
) -> bool {
    grants.is_empty()
        || grants.iter().any(|grant| {
            let (name, subs) = process_grant(grant);
            if subs.is_some_and(|subs| !first_arg.is_some_and(|arg| subs.contains(&arg))) {
                return false;
            }
            explicit(name) || explicit(requested) || name == requested
        })
}
pub(super) fn names_request(grants: &[String], requested: &str) -> bool {
    grants
        .iter()
        .any(|grant| process_grant(grant).0 == requested)
}
