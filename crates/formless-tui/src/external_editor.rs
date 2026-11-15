//! Private, bounded file round trips. This module never starts a shell or
//! changes terminal state; the TUI owns the shared terminal handoff.
use anyhow::{bail, Context, Result};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
#[derive(Debug, Clone)]
pub(super) struct EditorCommand {
    program: OsString,
    args: Vec<OsString>,
}
impl EditorCommand {
    pub(super) fn from_env() -> Result<Self> {
        let value = |name| -> Result<Option<String>> {
            std::env::var_os(name)
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("{name} must be a UTF-8 editor command"))
                })
                .transpose()
        };
        Self::from_values(value("VISUAL")?.as_deref(), value("EDITOR")?.as_deref())
    }
    fn from_values(visual: Option<&str>, editor: Option<&str>) -> Result<Self> {
        Self::parse(
            visual
                .filter(|s| !s.trim().is_empty())
                .or(editor.filter(|s| !s.trim().is_empty()))
                .unwrap_or(if cfg!(windows) { "notepad" } else { "vi" }),
        )
    }
    pub(super) fn parse(value: &str) -> Result<Self> {
        anyhow::ensure!(!value.contains('\0'), "editor command contains a NUL byte");
        #[cfg(not(windows))]
        let words = shlex::split(value).context("unclosed quote or escape in editor command")?;
        #[cfg(windows)]
        let words = split_windows(value)?;
        let mut words = words.into_iter();
        let program = words
            .next()
            .filter(|s| !s.is_empty())
            .context("editor command is empty")?;
        Ok(Self {
            program: program.into(),
            args: words.map(Into::into).collect(),
        })
    }
    fn name(&self) -> String {
        let name = Path::new(&self.program)
            .file_name()
            .unwrap_or(&self.program)
            .to_string_lossy()
            .to_ascii_lowercase();
        name.strip_suffix(".exe")
            .or_else(|| name.strip_suffix(".cmd"))
            .or_else(|| name.strip_suffix(".bat"))
            .unwrap_or(&name)
            .into()
    }
    pub(super) fn gui(&self) -> bool {
        matches!(
            self.name().as_str(),
            "code"
                | "code-insiders"
                | "codium"
                | "cursor"
                | "windsurf"
                | "subl"
                | "sublime_text"
                | "mate"
                | "zed"
                | "gedit"
                | "kate"
                | "gvim"
                | "mvim"
        )
    }
    pub(super) fn command(
        &self,
        file: &Path,
        line: Option<u32>,
        roundtrip: bool,
    ) -> Result<Command> {
        let name = self.name();
        if roundtrip {
            anyhow::ensure!(
                !(name == "emacsclient" && self.args.iter().any(|arg| arg == "-n")),
                "emacsclient -n cannot return an edited draft; remove -n"
            );
            anyhow::ensure!(!matches!(name.as_str(),"open"|"xdg-open"|"explorer"|"start"),"a file-open launcher cannot return an edited draft; configure an editor that waits for the file to close");
            anyhow::ensure!(
                !self.args.iter().any(|arg| matches!(
                    arg.to_str(),
                    Some("--no-wait" | "--new-window-no-wait")
                ) && (self.gui() || name == "emacsclient")),
                "a non-waiting GUI editor cannot return an edited draft; remove its no-wait option"
            );
        }
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        if roundtrip {
            match name.as_str() {
                "code" | "code-insiders" | "codium" | "cursor" | "windsurf" | "subl"
                | "sublime_text" | "mate" | "zed" | "gedit" => {
                    cmd.arg("--wait");
                }
                "kate" => {
                    cmd.arg("--block");
                }
                "gvim" | "mvim" => {
                    cmd.arg("--nofork");
                }
                _ => {}
            }
        }
        match (line, name.as_str()) {
            (Some(line), "code" | "code-insiders" | "codium" | "cursor" | "windsurf") => {
                let mut path = file.as_os_str().to_os_string();
                path.push(format!(":{line}"));
                cmd.arg("-g").arg(path);
            }
            (Some(line), "hx" | "helix" | "subl" | "sublime_text" | "zed") => {
                let mut path = file.as_os_str().to_os_string();
                path.push(format!(":{line}"));
                cmd.arg(path);
            }
            (
                Some(line),
                "vi" | "vim" | "nvim" | "nano" | "emacs" | "emacsclient" | "gvim" | "mvim"
                | "gedit",
            ) => {
                cmd.arg(format!("+{line}")).arg(file);
            }
            (Some(line), "mate") => {
                cmd.arg("-l").arg(line.to_string()).arg(file);
            }
            (Some(line), "kate") => {
                cmd.arg("--line").arg(line.to_string()).arg(file);
            }
            _ => {
                cmd.arg(file);
            }
        }
        Ok(cmd)
    }
}

// Windows CRT-style quoting: preserve ordinary path backslashes, including
// those before spaces inside double quotes. Kept testable on every platform.
#[cfg(any(windows, test))]
fn split_windows(value: &str) -> Result<Vec<String>> {
    let chars: Vec<char> = value.chars().collect();
    let mut i = 0;
    let mut words = Vec::new();
    while i < chars.len() {
        while i < chars.len() && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == chars.len() {
            break;
        }
        let mut word = String::new();
        let mut quoted = false;
        while i < chars.len() {
            if !quoted && chars[i].is_ascii_whitespace() {
                break;
            }
            if chars[i] == '\\' {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let count = i - start;
                if i < chars.len() && chars[i] == '"' {
                    word.extend(std::iter::repeat_n('\\', count / 2));
                    if count % 2 == 1 {
                        word.push('"');
                        i += 1;
                        continue;
                    }
                } else {
                    word.extend(std::iter::repeat_n('\\', count));
                    continue;
                }
            }
            if i < chars.len() && chars[i] == '"' {
                if quoted && chars.get(i + 1) == Some(&'"') {
                    word.push('"');
                    i += 2;
                } else {
                    quoted = !quoted;
                    i += 1;
                }
            } else if i < chars.len() {
                word.push(chars[i]);
                i += 1;
            }
        }
        anyhow::ensure!(!quoted, "unclosed double quote in editor command");
        words.push(word);
    }
    Ok(words)
}

pub(super) struct PrivateDraft {
    dir: PathBuf,
    file: PathBuf,
    #[cfg(windows)]
    owner: String,
}
impl PrivateDraft {
    pub(super) fn create(text: &str, extension: &str) -> Result<Self> {
        anyhow::ensure!(
            text.len() <= MAX_BYTES,
            "draft exceeds the 4 MiB external editor limit"
        );
        anyhow::ensure!(
            matches!(extension, "md" | "js" | "json"),
            "unsupported editor file extension"
        );
        let dir =
            std::env::temp_dir().join(format!("formless-edit-{}", uuid::Uuid::new_v4().simple()));
        #[cfg(not(windows))]
        {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(&dir)
                .context("create private editor directory")?;
        }
        #[cfg(windows)]
        windows_private::create_dir(&dir).context("create private editor directory")?;
        let this = Self {
            file: dir.join(format!("draft.{extension}")),
            dir,
            #[cfg(windows)]
            owner: String::new(),
        };
        #[cfg(not(windows))]
        let mut file = {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(&this.file)
                .context("create private editor draft")?
        };
        #[cfg(windows)]
        let mut file =
            windows_private::create_file(&this.file).context("create private editor draft")?;
        #[cfg(windows)]
        let this = {
            let mut this = this;
            this.owner = windows_private::file_owner(&file)?;
            this
        };
        file.write_all(text.as_bytes())?;
        file.flush()?;
        Ok(this)
    }
    pub(super) fn path(&self) -> &Path {
        &self.file
    }
    fn read(&self) -> Result<String> {
        let before =
            fs::symlink_metadata(&self.file).context("editor did not return its draft file")?;
        anyhow::ensure!(
            before.is_file() && !before.file_type().is_symlink(),
            "editor replaced its draft with a nonregular file or link"
        );
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
            anyhow::ensure!(
                before.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                "editor draft is a reparse point"
            );
            options
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .access_mode(
                    windows_sys::Win32::Foundation::GENERIC_READ
                        | windows_sys::Win32::Storage::FileSystem::WRITE_DAC,
                );
        }
        let file = options
            .open(&self.file)
            .context("read returned editor draft")?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file(),
            "editor draft is no longer a regular file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            anyhow::ensure!(
                metadata.uid() == unsafe { libc::geteuid() } && metadata.nlink() == 1,
                "returned editor draft must be an owned, unshared file"
            );
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            anyhow::ensure!(
                metadata.file_attributes() & 0x400 == 0,
                "editor draft is a reparse point"
            );
            windows_private::validate_file(&file, &self.owner)?;
            windows_private::protect_file(&file)?;
        }
        anyhow::ensure!(
            metadata.len() <= MAX_BYTES as u64,
            "edited draft exceeds the 4 MiB limit"
        );
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() <= MAX_BYTES,
            "edited draft exceeds the 4 MiB limit"
        );
        String::from_utf8(bytes).context("edited draft is not UTF-8")
    }
}
impl Drop for PrivateDraft {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub(super) fn edit(
    text: &str,
    extension: &str,
    run: impl FnOnce(&Path) -> Result<ExitStatus>,
) -> Result<String> {
    let draft = PrivateDraft::create(text, extension)?;
    let text = (|| {
        let status = run(draft.path())?;
        if !status.success() {
            bail!("external editor exited with {status}; original buffer kept")
        }
        draft.read()
    })();
    let cleanup = fs::remove_dir_all(&draft.dir)
        .context("could not remove private editor files; original buffer kept");
    match (text, cleanup) {
        (Ok(text), Ok(())) => Ok(text),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(error),
    }
}

// Catch (rather than ignore) interrupts only in the waiting parent. POSIX
// resets caught handlers to default at exec, so the editor still receives ^C.
#[cfg(unix)]
pub(super) struct InterruptGuard {
    saved: Vec<(libc::c_int, libc::sigaction)>,
}
#[cfg(unix)]
impl InterruptGuard {
    pub(super) fn install() -> Result<Self> {
        extern "C" fn parent_interrupt(_: libc::c_int) {}
        let mut guard = Self {
            saved: Vec::with_capacity(2),
        };
        for signal in [libc::SIGINT, libc::SIGQUIT] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = parent_interrupt as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            anyhow::ensure!(
                unsafe { libc::sigemptyset(&mut action.sa_mask) } == 0,
                std::io::Error::last_os_error()
            );
            let mut old = unsafe { std::mem::zeroed() };
            anyhow::ensure!(
                unsafe { libc::sigaction(signal, &action, &mut old) } == 0,
                std::io::Error::last_os_error()
            );
            guard.saved.push((signal, old));
        }
        Ok(guard)
    }
}
#[cfg(unix)]
impl Drop for InterruptGuard {
    fn drop(&mut self) {
        for (signal, action) in self.saved.iter().rev() {
            unsafe {
                libc::sigaction(*signal, action, std::ptr::null_mut());
            }
        }
    }
}

// Signature/constants checked against windows-sys 0.61.2's SDK bindings.
// A callback is parent-local; setting the inherited NULL/ignore flag is avoided.
#[cfg(windows)]
mod console_interrupt {
    use super::*;
    type Handler = unsafe extern "system" fn(u32) -> i32;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<Handler>, add: i32) -> i32;
    }
    unsafe extern "system" fn parent_interrupt(kind: u32) -> i32 {
        i32::from(matches!(kind, 0 | 1))
    }
    pub(crate) struct Guard;
    impl Guard {
        pub(crate) fn install() -> Result<Self> {
            anyhow::ensure!(
                unsafe { SetConsoleCtrlHandler(Some(parent_interrupt), 1) } != 0,
                std::io::Error::last_os_error()
            );
            Ok(Self)
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                SetConsoleCtrlHandler(Some(parent_interrupt), 0);
            }
        }
    }
}
#[cfg(windows)]
pub(super) use console_interrupt::Guard as InterruptGuard;

/// Upper bound, in UTF-16 units, on the SDDL buffer the Windows ACL test
/// fixture will inspect. Matches the private review DACL bound from #1779.
#[cfg(test)]
const ACL_FIXTURE_TEXT_LIMIT: u32 = 32768;

/// Decode the SDDL text returned to the Windows ACL test fixture by
/// `ConvertSecurityDescriptorToStringSecurityDescriptorW`.
///
/// That API reports the size of the returned buffer in WCHARs. It is the
/// buffer capacity, not the string length, and the capacity may extend past
/// the terminator. This reads one unit at a time and stops at the first NUL,
/// so units after it (padding) are never read and cannot affect the
/// fingerprint. A null or misaligned pointer, or a capacity outside
/// `1..=ACL_FIXTURE_TEXT_LIMIT`, is refused before any read. A missing
/// terminator within the capacity, or invalid UTF-16 before it, fails closed.
/// Test-only, and compiled on every platform so the synthetic tests run
/// everywhere.
///
/// # Safety
/// If `text` is non-null, aligned for `u16` and `capacity` is within
/// `1..=ACL_FIXTURE_TEXT_LIMIT`, the caller must guarantee that `text` points
/// to a single allocation valid for reads of `capacity` `u16` units, and that
/// the units are initialized through the first NUL (or through the whole
/// capacity if there is no NUL). The allocation must not be freed or written
/// during the call. Units after the first NUL may be uninitialized.
#[cfg(test)]
unsafe fn acl_fixture_text(text: *const u16, capacity: u32) -> Result<String> {
    if text.is_null() {
        bail!("private ACL fixture string is missing");
    }
    if (text as usize) % std::mem::align_of::<u16>() != 0 {
        bail!("private ACL fixture string is misaligned");
    }
    if capacity == 0 || capacity > ACL_FIXTURE_TEXT_LIMIT {
        bail!("private ACL fixture string exceeds its inspection limit");
    }
    let mut units = Vec::new();
    for index in 0..capacity as usize {
        // SAFETY: `text` is non-null and aligned, `index < capacity`, and the
        // caller guarantees `capacity` readable units initialized through the
        // first NUL. The loop returns at that NUL, so no later unit is read.
        let unit = unsafe { text.add(index).read() };
        if unit == 0 {
            return String::from_utf16(&units).context("invalid private ACL fixture string");
        }
        units.push(unit);
    }
    bail!("private ACL fixture string is missing its terminator")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    pub(crate) struct Fixture(pub(crate) PathBuf);
    impl Fixture {
        pub(crate) fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "formless-private-editor-test-{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    pub(crate) fn run_private(
        file: &Path,
        mode: &str,
        result: &str,
        record: Option<&Path>,
    ) -> Result<ExitStatus> {
        let spec = EditorCommand {
            program: std::env::current_exe()?.into_os_string(),
            args: [
                "--exact",
                "external_editor::tests::private_editor_fixture",
                "--ignored",
                "--nocapture",
                "--skip",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
        };
        let mut cmd = spec.command(file, None, true)?;
        configure_fixture(&mut cmd, file, mode, result, record);
        let ack = file.with_extension(format!("fixture-ack-{}", uuid::Uuid::new_v4().simple()));
        cmd.env("FORMLESS_PRIVATE_EDITOR_ACK", &ack);
        let status =
            bounded_status(&mut cmd).expect("private editor fixture must start and finish");
        match mode {
            "nonzero" => assert_eq!(status.code(), Some(7)),
            "cancel" => assert_eq!(status.code(), Some(130)),
            #[cfg(unix)]
            "raise-int" => {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(libc::SIGINT));
            }
            #[cfg(unix)]
            "raise-quit" => {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(libc::SIGQUIT));
            }
            _ => assert!(
                status.success(),
                "private fixture exited before completing {mode}: {status}"
            ),
        }
        if !matches!(mode, "raise-int" | "raise-quit") {
            assert_eq!(
                fs::read_to_string(&ack).expect("private fixture completion marker required"),
                mode
            );
        }
        let _ = fs::remove_file(ack);
        Ok(status)
    }
    fn configure_fixture(
        cmd: &mut Command,
        file: &Path,
        mode: &str,
        result: &str,
        record: Option<&Path>,
    ) {
        cmd.env("FORMLESS_PRIVATE_EDITOR_FILE", file)
            .env("FORMLESS_PRIVATE_EDITOR_MODE", mode)
            .env("FORMLESS_PRIVATE_EDITOR_RESULT", result)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(record) = record {
            cmd.env("FORMLESS_PRIVATE_EDITOR_RECORD", record);
        } else {
            cmd.env_remove("FORMLESS_PRIVATE_EDITOR_RECORD");
        }
    }
    fn bounded_status(cmd: &mut Command) -> Result<ExitStatus> {
        let mut child = cmd.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("owned private editor fixture exceeded ten seconds")
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    #[ignore = "private editor subprocess, invoked only by owned fixture commands"]
    fn private_editor_fixture() {
        let file = PathBuf::from(
            std::env::var_os("FORMLESS_PRIVATE_EDITOR_FILE")
                .expect("private file must be explicit"),
        );
        let mode = std::env::var("FORMLESS_PRIVATE_EDITOR_MODE")
            .expect("private editor mode must be explicit");
        #[cfg(unix)]
        {
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(file.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        if let Some(record) = std::env::var_os("FORMLESS_PRIVATE_EDITOR_RECORD") {
            fs::write(record, fs::read(&file).unwrap()).unwrap();
        }
        let result = std::env::var("FORMLESS_PRIVATE_EDITOR_RESULT").unwrap();
        match mode.as_str() {
            "replace" => {
                fs::write(file.with_extension("editor-backup"), "owned backup").unwrap();
                let next = file.with_extension("replacement");
                fs::write(&next, result).unwrap();
                fs::rename(next, &file).unwrap();
            }
            "unchanged" => {}
            "empty" => fs::write(&file, []).unwrap(),
            "nonzero" => {
                fs::write(&file, "MUST NOT LOAD").unwrap();
                complete_fixture(&mode);
                std::process::exit(7)
            }
            "cancel" => {
                fs::write(&file, "MUST NOT LOAD").unwrap();
                complete_fixture(&mode);
                std::process::exit(130)
            }
            "missing" => fs::remove_file(&file).unwrap(),
            "directory" => {
                fs::remove_file(&file).unwrap();
                fs::create_dir(&file).unwrap();
            }
            "huge" => OpenOptions::new()
                .write(true)
                .open(&file)
                .unwrap()
                .set_len((MAX_BYTES + 1) as u64)
                .unwrap(),
            "invalid" => fs::write(&file, [0xff]).unwrap(),
            #[cfg(unix)]
            "symlink" => {
                fs::remove_file(&file).unwrap();
                std::os::unix::fs::symlink(
                    std::env::var_os("FORMLESS_PRIVATE_EDITOR_RESULT").unwrap(),
                    &file,
                )
                .unwrap();
            }
            "hardlink" => {
                fs::remove_file(&file).unwrap();
                fs::hard_link(
                    std::env::var_os("FORMLESS_PRIVATE_EDITOR_RESULT").unwrap(),
                    &file,
                )
                .unwrap();
            }
            #[cfg(unix)]
            "raise-int" => {
                unsafe {
                    libc::raise(libc::SIGINT);
                }
                std::process::exit(91)
            }
            #[cfg(unix)]
            "raise-quit" => {
                unsafe {
                    libc::raise(libc::SIGQUIT);
                }
                std::process::exit(92)
            }
            #[cfg(unix)]
            "signal-guard" => private_signal_guard(&file),
            _ => panic!("unknown private editor mode"),
        }
        complete_fixture(&mode);
    }
    fn complete_fixture(mode: &str) {
        fs::write(
            std::env::var_os("FORMLESS_PRIVATE_EDITOR_ACK")
                .expect("private acknowledgement path required"),
            mode,
        )
        .unwrap();
    }
    #[cfg(unix)]
    fn private_signal_guard(file: &Path) {
        use std::os::unix::process::ExitStatusExt;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn prior(_: libc::c_int) {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        for (signal, mode) in [(libc::SIGINT, "raise-int"), (libc::SIGQUIT, "raise-quit")] {
            CALLS.store(0, Ordering::SeqCst);
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = prior as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaddset(&mut action.sa_mask, libc::SIGUSR1);
            }
            let mut original: libc::sigaction = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::sigaction(signal, &action, &mut original) },
                0
            );
            let guard = InterruptGuard::install().unwrap();
            assert_eq!(unsafe { libc::raise(signal) }, 0);
            assert_eq!(CALLS.load(Ordering::SeqCst), 0);
            let status = run_private(file, mode, "", None).unwrap();
            assert_eq!(
                status.signal(),
                Some(signal),
                "caught parent handlers must reset in editor exec, never inherit SIG_IGN"
            );
            drop(guard);
            let mut restored: libc::sigaction = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::sigaction(signal, std::ptr::null(), &mut restored) },
                0
            );
            assert_eq!(restored.sa_sigaction, action.sa_sigaction);
            assert_eq!(
                restored.sa_flags & libc::SA_RESTART,
                action.sa_flags & libc::SA_RESTART
            );
            assert_eq!(
                unsafe { libc::sigismember(&restored.sa_mask, libc::SIGUSR1) },
                1
            );
            assert_eq!(unsafe { libc::raise(signal) }, 0);
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(
                unsafe { libc::sigaction(signal, &original, std::ptr::null_mut()) },
                0
            );
        }
    }
    #[test]
    fn private_process_roundtrip_accepts_atomic_replacement_and_removes_all_files() {
        let fixture = Fixture::new();
        let record = fixture.0.join("seed");
        let mut path = None;
        let result = edit("draft 界\n", "md", |file| {
            path = Some(file.to_path_buf());
            run_private(file, "replace", "edited 界\n", Some(&record))
        })
        .unwrap();
        assert_eq!(result, "edited 界\n");
        assert_eq!(fs::read_to_string(record).unwrap(), "draft 界\n");
        let path = path.unwrap();
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }
    #[test]
    fn failed_private_editors_and_invalid_replacements_clean_up_without_loading() {
        for mode in [
            "nonzero",
            "cancel",
            "missing",
            "directory",
            "huge",
            "invalid",
        ] {
            let mut path = None;
            let result = edit("keep original", "js", |file| {
                path = Some(file.to_path_buf());
                run_private(file, mode, "discard", None)
            });
            assert!(result.is_err(), "{mode}");
            let path = path.unwrap();
            assert!(!path.exists(), "{mode}");
            assert!(!path.parent().unwrap().exists(), "{mode}");
        }
        let mut path = None;
        assert!(edit("keep", "md", |file| {
            path = Some(file.to_path_buf());
            bail!("private failed spawn")
        })
        .is_err());
        assert!(!path.unwrap().parent().unwrap().exists());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_replacement_is_refused_without_reading_or_altering_the_target() {
        let fixture = Fixture::new();
        let target = fixture.0.join("private-sentinel");
        fs::write(&target, "DO NOT LOAD").unwrap();
        let result = edit("original", "md", |file| {
            run_private(file, "symlink", target.to_str().unwrap(), None)
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"DO NOT LOAD");
    }
    #[cfg(unix)]
    #[test]
    fn signal_policy_proof_runs_only_inside_a_private_subprocess() {
        let result = edit("private signal fixture", "md", |file| {
            run_private(file, "signal-guard", "", None)
        })
        .unwrap();
        assert_eq!(result, "private signal fixture");
    }
    #[test]
    fn editor_argv_is_literal_and_known_gui_roundtrips_wait() {
        #[cfg(not(windows))]
        let spec = EditorCommand::parse(
            r#"'/private editor' --flag 'two words' ';literal' '$NO_EXPANSION'"#,
        )
        .unwrap();
        #[cfg(windows)]
        let spec = EditorCommand::parse(
            r#""C:\private editor.exe" --flag "two words" ";literal" "$NO_EXPANSION""#,
        )
        .unwrap();
        let file = Path::new("draft ; do-not-evaluate $(literal).md");
        let command = spec.command(file, Some(4), true).unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args.last().unwrap(), &file.as_os_str());
        assert!(args.iter().any(|arg| *arg == "two words"));
        assert!(args.iter().any(|arg| *arg == "$NO_EXPANSION"));
        assert!(args.iter().any(|arg| *arg == ";literal"));
        for name in [
            "code",
            "code-insiders",
            "codium",
            "cursor",
            "windsurf",
            "subl",
            "mate",
            "zed",
            "gedit",
        ] {
            let spec = EditorCommand::parse(name).unwrap();
            let command = spec.command(file, None, true).unwrap();
            assert!(command.get_args().any(|arg| arg == "--wait"), "{name}");
            let open = spec.command(file, None, false).unwrap();
            assert!(!open.get_args().any(|arg| arg == "--wait"));
        }
        assert!(EditorCommand::parse("xdg-open")
            .unwrap()
            .command(file, None, true)
            .is_err());
        assert!(EditorCommand::parse("code --no-wait")
            .unwrap()
            .command(file, None, true)
            .is_err());
        assert!(EditorCommand::parse("code -n")
            .unwrap()
            .command(file, None, true)
            .is_ok());
        assert!(EditorCommand::parse("'unclosed\"").is_err());
        assert!(EditorCommand::parse("x\0y").is_err());
    }
    #[test]
    fn known_editors_keep_cursor_lines_wait_modes_and_literal_arguments() {
        let file = Path::new("ไทย draft ; $(literal):part.md");
        let suffix = || {
            let mut path = file.as_os_str().to_os_string();
            path.push(":42");
            path
        };
        for (name, wait, location) in [
            ("subl", "--wait", vec![suffix()]),
            ("sublime_text", "--wait", vec![suffix()]),
            ("SUBL.EXE", "--wait", vec![suffix()]),
            (
                "mate",
                "--wait",
                vec!["-l".into(), "42".into(), file.into()],
            ),
            ("zed", "--wait", vec![suffix()]),
            ("gedit", "--wait", vec!["+42".into(), file.into()]),
            (
                "kate",
                "--block",
                vec!["--line".into(), "42".into(), file.into()],
            ),
            ("code", "--wait", vec!["-g".into(), suffix()]),
            ("hx", "", vec![suffix()]),
            ("vim", "", vec!["+42".into(), file.into()]),
            ("unknown-editor", "", vec![file.into()]),
        ] {
            let spec = EditorCommand::parse(&format!("{name} --configured-option")).unwrap();
            for roundtrip in [false, true] {
                for line in [None, Some(42)] {
                    let command = spec.command(file, line, roundtrip).unwrap();
                    let mut expected: Vec<OsString> = vec!["--configured-option".into()];
                    if roundtrip && !wait.is_empty() {
                        expected.push(wait.into());
                    }
                    expected.extend(if line.is_some() {
                        location.clone()
                    } else {
                        vec![file.into()]
                    });
                    assert_eq!(
                        command.get_args().collect::<Vec<_>>(),
                        expected,
                        "{name}, line={line:?}, roundtrip={roundtrip}"
                    );
                }
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn cursor_location_preserves_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let file = PathBuf::from(OsString::from_vec(b"/tmp/draft \xff.md".to_vec()));
        for name in ["subl", "sublime_text", "zed", "mate", "gedit", "kate"] {
            let spec = EditorCommand::parse(name).unwrap();
            let command = spec.command(&file, Some(42), true).unwrap();
            let mut expected = file.as_os_str().to_os_string();
            if matches!(name, "subl" | "sublime_text" | "zed") {
                expected.push(":42");
            }
            assert_eq!(command.get_args().last().unwrap(), expected);
        }
    }
    #[test]
    fn windows_quoting_preserves_path_backslashes_and_empty_arguments() {
        assert_eq!(
            split_windows(
                r#""C:\Program Files\Editor.exe" --flag "two words" "" C:\plain\editor.exe"#
            )
            .unwrap(),
            vec![
                r"C:\Program Files\Editor.exe",
                "--flag",
                "two words",
                "",
                r"C:\plain\editor.exe"
            ]
        );
        assert!(split_windows("\"unclosed").is_err());
    }
    #[test]
    fn visual_precedes_editor_but_empty_values_fall_back() {
        assert_eq!(
            EditorCommand::from_values(Some("chosen-visual"), Some("unused-editor"))
                .unwrap()
                .program,
            "chosen-visual"
        );
        assert_eq!(
            EditorCommand::from_values(Some("  "), Some("chosen-editor"))
                .unwrap()
                .program,
            "chosen-editor"
        );
    }
    #[cfg(unix)]
    #[test]
    fn hardlink_replacement_is_rejected_before_target_permissions_or_content_change() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let target = fixture.0.join("owned-target");
        fs::write(&target, "PRIVATE TARGET MUST NOT LOAD").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        let result = edit("original", "md", |file| {
            run_private(file, "hardlink", target.to_str().unwrap(), None)
        });
        assert!(format!("{:#}", result.unwrap_err()).contains("unshared file"));
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            mode
        );
        assert_eq!(fs::read(&target).unwrap(), b"PRIVATE TARGET MUST NOT LOAD");
    }
    #[cfg(windows)]
    #[test]
    fn hardlink_replacement_is_rejected_before_acl_or_content_changes() {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::{
            Foundation::GENERIC_READ,
            Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT},
            Security::UNPROTECTED_DACL_SECURITY_INFORMATION,
            Storage::FileSystem::WRITE_DAC,
        };
        let fixture = Fixture::new();
        let target = fixture.0.join("owned-sentinel.txt");
        fs::write(&target, "MUST KEEP CONTENT AND ACL").unwrap();
        let handle = OpenOptions::new()
            .read(true)
            .access_mode(GENERIC_READ | WRITE_DAC)
            .open(&target)
            .unwrap();
        // Deliberately differ from the protected returned-file policy. A guard
        // placed after SetSecurityInfo would observably alter this target.
        assert_eq!(
            unsafe {
                SetSecurityInfo(
                    handle.as_raw_handle().cast(),
                    SE_FILE_OBJECT,
                    UNPROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        let before = windows_private::acl_fingerprint(&handle).unwrap();
        let result = edit("original", "md", |file| {
            run_private(file, "hardlink", target.to_str().unwrap(), None)
        });
        assert!(result.is_err());
        assert!(format!("{:#}", result.unwrap_err()).contains("hard links"));
        assert_eq!(fs::read(&target).unwrap(), b"MUST KEEP CONTENT AND ACL");
        assert_eq!(windows_private::acl_fingerprint(&handle).unwrap(), before);
    }

    fn wide_units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }
    fn fixture_error(units: &[u16], capacity: u32) -> String {
        assert!(capacity as usize <= units.len());
        // SAFETY: `units` is a live, aligned Vec allocation of at least
        // `capacity` initialized u16 values.
        format!(
            "{:#}",
            unsafe { acl_fixture_text(units.as_ptr(), capacity) }.unwrap_err()
        )
    }
    fn fixture_text(units: &[u16], capacity: u32) -> String {
        assert!(capacity as usize <= units.len());
        // SAFETY: as in `fixture_error`.
        unsafe { acl_fixture_text(units.as_ptr(), capacity) }.unwrap()
    }

    #[test]
    fn acl_fixture_text_stops_at_first_nul_and_ignores_hostile_padding() {
        let sddl = "O:S-1-5-21-1D:P(A;;FA;;;OW)(A;;FA;;;SY)";
        let mut units = wide_units(sddl);
        let exact = units.len() as u32 + 1;
        units.push(0);
        // Hostile padding after the first NUL: extra NULs, lone high and low
        // surrogates, an extra Everyone grant, and arbitrary garbage.
        units.extend([0, 0, 0xD800, 0xDC00, 0xFFFF]);
        units.extend(wide_units("(A;;FA;;;WD)garbage"));
        units.extend([0xDBFF, 0x0041, 0]);
        units.extend(wide_units("(A;;GA;;;BU)"));
        let padded = units.len() as u32;
        assert_eq!(fixture_text(&units, padded), sddl);
        // The fingerprint depends only on the string, not the capacity.
        assert_eq!(fixture_text(&units, exact), sddl);
        assert!(!fixture_text(&units, padded).contains("WD"));
    }

    #[test]
    fn acl_fixture_text_refuses_lone_high_surrogate_before_nul() {
        let mut units = wide_units("D:P(A;;FA;;;OW)");
        units.extend([0xD800, 0]);
        assert!(fixture_error(&units, units.len() as u32)
            .contains("invalid private ACL fixture string"));
        // High surrogate followed directly by the terminator.
        let units = [0x0044, 0xD800, 0];
        assert!(fixture_error(&units, 3).contains("invalid private ACL fixture string"));
    }

    #[test]
    fn acl_fixture_text_refuses_lone_low_surrogate_before_nul() {
        let mut units = wide_units("D:P(A;;FA;;;OW)");
        units.extend([0xDC00, 0x0041, 0]);
        assert!(fixture_error(&units, units.len() as u32)
            .contains("invalid private ACL fixture string"));
    }

    #[test]
    fn acl_fixture_text_decodes_valid_surrogate_pair() {
        // U+1F600 is the pair D83D DE00.
        let units = [0x0044, 0xD83D, 0xDE00, 0x003A, 0, 0xD800];
        assert_eq!(fixture_text(&units, 6), "D\u{1F600}:");
    }

    #[test]
    fn acl_fixture_text_accepts_capacity_limit_and_refuses_one_more() {
        assert_eq!(ACL_FIXTURE_TEXT_LIMIT, 32768);
        // Exactly the limit, with the NUL in the final slot.
        let mut units = vec![u16::from(b'A'); 32768];
        units[32767] = 0;
        assert_eq!(fixture_text(&units, 32768), "A".repeat(32767));
        // One past the limit is refused before any read, even though the
        // backing buffer is real and a NUL is present early.
        let mut units = vec![u16::from(b'A'); 32769];
        units[0] = 0;
        assert!(fixture_error(&units, 32769).contains("exceeds its inspection limit"));
    }

    #[test]
    fn acl_fixture_text_accepts_nul_in_final_slot() {
        let units = [0x0044, 0x003A, 0x0050, 0];
        assert_eq!(fixture_text(&units, 4), "D:P");
        // Only a NUL: the empty string, at capacity one.
        assert_eq!(fixture_text(&[0], 1), "");
    }

    #[test]
    fn acl_fixture_text_refuses_missing_terminator() {
        let units = wide_units("D:P(A;;FA;;;OW)");
        assert!(fixture_error(&units, units.len() as u32).contains("missing its terminator"));
        // A NUL just past the reported capacity is not read.
        let mut units = wide_units("D:P");
        units.push(0);
        assert!(fixture_error(&units, 3).contains("missing its terminator"));
    }

    #[test]
    fn acl_fixture_text_refuses_null_pointer() {
        for capacity in [0, 1, 4, ACL_FIXTURE_TEXT_LIMIT, u32::MAX] {
            // SAFETY: a null pointer is refused before any read.
            let error = unsafe { acl_fixture_text(std::ptr::null(), capacity) }.unwrap_err();
            assert!(format!("{error:#}").contains("string is missing"));
        }
    }

    #[test]
    fn acl_fixture_text_refuses_zero_capacity() {
        let units = [0x0044, 0];
        assert!(fixture_error(&units, 0).contains("exceeds its inspection limit"));
    }

    #[test]
    fn acl_fixture_text_refuses_misaligned_pointer() {
        let units = [0u16; 4];
        // An odd address inside the buffer; refused before any read.
        let misaligned = units.as_ptr().cast::<u8>().wrapping_add(1).cast::<u16>();
        // SAFETY: the misaligned pointer is refused before any read.
        let error = unsafe { acl_fixture_text(misaligned, 2) }.unwrap_err();
        assert!(format!("{error:#}").contains("misaligned"));
    }
}

#[cfg(windows)]
mod windows_private {
    use super::*;
    use std::{
        fs::File,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        },
    };
    use windows_sys::Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT},
        Security::{
            GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        },
    };
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    pub(super) fn create_dir(path: &Path) -> Result<()> {
        let security = formless_core::win_pipe::OwnerOnly::new()?;
        anyhow::ensure!(
            unsafe { CreateDirectoryW(wide(path).as_ptr(), security.as_raw().cast()) } != 0,
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    pub(super) fn create_file(path: &Path) -> Result<File> {
        let security = formless_core::win_pipe::OwnerOnly::new()?;
        let handle = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                security.as_raw().cast(),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(
            handle != INVALID_HANDLE_VALUE,
            std::io::Error::last_os_error()
        );
        Ok(unsafe { File::from_raw_handle(handle) })
    }
    pub(super) fn protect_file(file: &File) -> Result<()> {
        let security = formless_core::win_pipe::OwnerOnly::new()?;
        let attrs = unsafe { &*security.as_raw().cast::<SECURITY_ATTRIBUTES>() };
        let (mut present, mut defaulted, mut dacl) = (0, 0, std::ptr::null_mut());
        anyhow::ensure!(
            unsafe {
                GetSecurityDescriptorDacl(
                    attrs.lpSecurityDescriptor,
                    &mut present,
                    &mut dacl,
                    &mut defaulted,
                )
            } != 0,
            std::io::Error::last_os_error()
        );
        anyhow::ensure!(
            present != 0 && !dacl.is_null(),
            "owner-only descriptor must contain a DACL"
        );
        let error = unsafe {
            SetSecurityInfo(
                file.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(error == 0, std::io::Error::from_raw_os_error(error as i32));
        Ok(())
    }
    struct Local(*mut std::ffi::c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.0);
            }
        }
    }
    pub(super) fn file_owner(file: &File) -> Result<String> {
        use windows_sys::Win32::{
            Security::Authorization::{ConvertSidToStringSidW, GetSecurityInfo},
            Security::OWNER_SECURITY_INFORMATION,
        };
        let (mut owner, mut descriptor) = (std::ptr::null_mut(), std::ptr::null_mut());
        let error = unsafe {
            GetSecurityInfo(
                file.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        let _descriptor = Local(descriptor);
        anyhow::ensure!(error == 0, std::io::Error::from_raw_os_error(error as i32));
        anyhow::ensure!(
            !owner.is_null(),
            "returned editor file has no owner identity"
        );
        let mut sid = std::ptr::null_mut();
        anyhow::ensure!(
            unsafe { ConvertSidToStringSidW(owner, &mut sid) } != 0,
            std::io::Error::last_os_error()
        );
        let _sid = Local(sid.cast());
        let mut len = 0;
        while len < 256 && unsafe { *sid.add(len) } != 0 {
            len += 1;
        }
        anyhow::ensure!(len < 256, "returned editor file owner identity is invalid");
        String::from_utf16(unsafe { std::slice::from_raw_parts(sid, len) })
            .context("invalid editor file owner identity")
    }
    pub(super) fn validate_file(file: &File, expected_owner: &str) -> Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        anyhow::ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } != 0,
            std::io::Error::last_os_error()
        );
        anyhow::ensure!(
            info.nNumberOfLinks == 1,
            "returned editor draft must not share hard links"
        );
        anyhow::ensure!(
            !expected_owner.is_empty() && file_owner(file)? == expected_owner,
            "returned editor draft owner changed"
        );
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn acl_fingerprint(file: &File) -> Result<String> {
        use windows_sys::Win32::{
            Security::Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                SDDL_REVISION_1,
            },
            Security::{DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION},
        };
        let mut descriptor = std::ptr::null_mut();
        let info = DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION;
        let error = unsafe {
            GetSecurityInfo(
                file.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                info,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        let _descriptor = Local(descriptor);
        anyhow::ensure!(error == 0, std::io::Error::from_raw_os_error(error as i32));
        let mut text = std::ptr::null_mut();
        let mut len = 0;
        anyhow::ensure!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    SDDL_REVISION_1,
                    info,
                    &mut text,
                    &mut len,
                )
            } != 0,
            std::io::Error::last_os_error()
        );
        // `_text` frees the buffer with LocalFree on every return below,
        // including a decode error. The decoded string is owned.
        let _text = Local(text.cast());
        // SAFETY: a successful conversion returns a LocalAlloc buffer, which
        // is suitably aligned for u16, holding `len` WCHARs with a
        // NUL-terminated string. `_text` keeps it alive for this call.
        unsafe { super::acl_fixture_text(text, len) }
    }
}
