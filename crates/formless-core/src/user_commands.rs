//! Local prompt templates, never executable commands. Loading inspects template
//! metadata/text only; mentions are left for the client's ordinary intake flow.
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

pub const MAX_FILE_BYTES: usize = 64 * 1024;
pub const MAX_TOTAL_BYTES: usize = 1024 * 1024;
pub const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
pub const MAX_EXPANDED_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_COMMANDS: usize = 128;
const MAX_DIAGNOSTICS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Global,
    Project,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub source: Source,
    pub max_position: u8,
    pub uses_arguments: bool,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
}
#[derive(Clone, Debug)]
pub struct UserCommand {
    pub name: String,
    pub description: String,
    pub source: Source,
    pub max_position: u8,
    pub uses_arguments: bool,
    pub path: PathBuf,
    body: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub key: String,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ShadowedCommand {
    pub name: String,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub source: Source,
    pub reason: String,
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub commands: Vec<UserCommand>,
    pub diagnostics: Vec<Diagnostic>,
    pub shadowed: Vec<ShadowedCommand>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Projection {
    pub commands: Vec<CommandInfo>,
    pub diagnostics: Vec<Diagnostic>,
    pub shadowed: Vec<ShadowedCommand>,
}
// Paths in this projection are display labels, never filesystem authority.
// Preserve malformed native filenames as diagnostics without failing JSON.
fn serialize_path<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

impl Snapshot {
    pub fn find(&self, name: &str) -> Option<&UserCommand> {
        self.commands.iter().find(|command| command.name == name)
    }
    pub fn projection(&self) -> Projection {
        Projection {
            commands: self
                .commands
                .iter()
                .map(|command| CommandInfo {
                    name: command.name.clone(),
                    description: command.description.clone(),
                    source: command.source,
                    max_position: command.max_position,
                    uses_arguments: command.uses_arguments,
                    path: command.path.clone(),
                })
                .collect(),
            diagnostics: self.diagnostics.clone(),
            shadowed: self.shadowed.clone(),
        }
    }
    fn diagnostic(&mut self, path: &Path, message: &str) {
        if self.diagnostics.len() >= MAX_DIAGNOSTICS {
            return;
        }
        // Stable across reloads; callers deduplicate notes by this key. No body,
        // arguments, environment or attachment bytes enter a diagnostic.
        use sha2::{Digest, Sha256};
        let key = format!(
            "{:x}",
            Sha256::digest(format!("{:?}\0{message}", path.as_os_str()).as_bytes())
        );
        self.diagnostics.push(Diagnostic {
            key,
            path: path.into(),
            message: message.into(),
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpansionError(String);
impl std::fmt::Display for ExpansionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ExpansionError {}

enum Placeholder<'a> {
    Literal(&'a str),
    Arguments,
    Position(u8),
}
// Called only at a dollar. Discovery and expansion consume exactly the same
// token, including dollar escapes and multi-digit literals.
fn placeholder(rest: &str) -> (Placeholder<'_>, &str) {
    if let Some(tail) = rest.strip_prefix("$$") {
        (Placeholder::Literal("$"), tail)
    } else if let Some(tail) = rest.strip_prefix("$ARGUMENTS") {
        if tail
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            (Placeholder::Literal("$ARGUMENTS"), tail)
        } else {
            (Placeholder::Arguments, tail)
        }
    } else if rest.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
        let digits = rest[1..].bytes().take_while(u8::is_ascii_digit).count();
        let token = if digits == 1 && rest.as_bytes()[1] != b'0' {
            Placeholder::Position(rest.as_bytes()[1] - b'0')
        } else {
            Placeholder::Literal(&rest[..1 + digits])
        };
        (token, &rest[1 + digits..])
    } else {
        (Placeholder::Literal("$"), &rest[1..])
    }
}
fn argument_metadata(mut body: &str) -> (u8, bool) {
    let (mut max_position, mut uses_arguments) = (0, false);
    while let Some(index) = body.find('$') {
        let (token, tail) = placeholder(&body[index..]);
        match token {
            Placeholder::Position(position) => max_position = max_position.max(position),
            Placeholder::Arguments => uses_arguments = true,
            Placeholder::Literal(_) => {}
        }
        body = tail;
    }
    (max_position, uses_arguments)
}
fn usage(name: &str, max_position: u8, uses_arguments: bool) -> String {
    let mut hint = format!("/{name}");
    for position in 1..=max_position {
        hint.push_str(&format!(" <arg{position}>"));
    }
    if uses_arguments {
        hint.push_str(" [arguments…]");
    }
    hint
}
impl CommandInfo {
    pub fn usage(&self) -> String {
        usage(&self.name, self.max_position, self.uses_arguments)
    }
}
impl UserCommand {
    pub fn usage(&self) -> String {
        usage(&self.name, self.max_position, self.uses_arguments)
    }
    /// Literal single pass. Positions 1..9 split on Unicode whitespace; all
    /// arguments preserve their original spelling/whitespace in $ARGUMENTS.
    /// `$$` emits a literal dollar; missing positions are errors before send.
    /// No quote, environment, shell, backtick or recursive template evaluation.
    pub fn expand(&self, arguments: &str) -> Result<String, ExpansionError> {
        if arguments.len() > MAX_ARGUMENT_BYTES {
            return Err(ExpansionError("Command arguments exceed 64 KiB".into()));
        }
        let positions: Vec<&str> = arguments.split_whitespace().take(9).collect();
        let mut output = String::new();
        let push = |output: &mut String, text: &str| -> Result<(), ExpansionError> {
            if text.len() > MAX_EXPANDED_BYTES.saturating_sub(output.len()) {
                return Err(ExpansionError("Expanded command exceeds 1 MiB".into()));
            }
            output.push_str(text);
            Ok(())
        };
        let mut rest = self.body.as_str();
        while let Some(index) = rest.find('$') {
            push(&mut output, &rest[..index])?;
            let (token, tail) = placeholder(&rest[index..]);
            match token {
                Placeholder::Literal(text) => push(&mut output, text)?,
                Placeholder::Arguments => push(&mut output, arguments)?,
                Placeholder::Position(number) => {
                    let position = (number - 1) as usize;
                    let value = positions.get(position).ok_or_else(|| ExpansionError(format!(
                        "{} references ${}, but only {} argument(s) were supplied; provide at least {} or write $${} for a literal ${}",
                        self.path.display(), position + 1, positions.len(), position + 1, position + 1, position + 1,
                    )))?;
                    push(&mut output, value)?;
                }
            }
            rest = tail;
        }
        push(&mut output, rest)?;
        Ok(output)
    }
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_lowercase()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn parse(text: &str) -> Result<(String, String), &'static str> {
    if text.contains('\0') {
        return Err("Template contains a NUL byte");
    }
    let text = text.trim_start_matches('\u{feff}');
    let mut body = text;
    let mut description = "User command".to_string();
    if text.lines().next() == Some("---") {
        let mut consumed = text.find('\n').map_or(text.len(), |index| index + 1);
        let mut header = Vec::new();
        let mut closed = false;
        for line in text[consumed..].split_inclusive('\n') {
            consumed += line.len();
            let line = line.trim_end_matches(['\n', '\r']);
            if line == "---" {
                closed = true;
                break;
            }
            header.push(line);
        }
        if !closed {
            return Err("Command frontmatter is not closed with ---");
        }
        body = &text[consumed..];
        let mut found = false;
        let mut index = 0;
        while index < header.len() {
            let line = header[index];
            index += 1;
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let Some(value) = line.strip_prefix("description:") else {
                return Err("Only description frontmatter is supported");
            };
            if found {
                return Err("Duplicate frontmatter description");
            }
            found = true;
            let value = crate::frontmatter::scalar_without_comment(value.trim());
            description = if matches!(value, ">" | "|" | ">-" | "|-") {
                let mut parts = Vec::new();
                while index < header.len()
                    && (header[index].starts_with(char::is_whitespace) || header[index].is_empty())
                {
                    parts.push(header[index].trim());
                    index += 1;
                }
                parts.join(" ")
            } else if value.starts_with('"') {
                serde_json::from_str::<String>(value).map_err(|_| "Invalid quoted description")?
            } else if let Some(value) = value.strip_prefix('\'') {
                let Some(value) = value.strip_suffix('\'') else {
                    return Err("Invalid quoted description");
                };
                let decoded = value.replace("''", "\0");
                if decoded.contains('\'') {
                    return Err("Invalid quoted description");
                }
                decoded.replace('\0', "'")
            } else {
                if value.starts_with(['[', '{', '&', '*', '!', '>', '|']) {
                    return Err("Description must be a plain or quoted string");
                }
                value.into()
            };
        }
        if !found || description.trim().is_empty() {
            return Err("Frontmatter needs a non-empty description");
        }
    }
    if body.trim().is_empty() {
        return Err("Command template body is empty");
    }
    if description.len() > 512 {
        return Err("Command description exceeds 512 bytes");
    }
    let description = crate::descriptions::screen(&description).text;
    if description.trim().is_empty() {
        return Err("Command description has no displayable text");
    }
    Ok((description, body.into()))
}

/// Read a fresh bounded snapshot for palette/help/list/dispatch. Config and the
/// selected Session CWD are trusted roots whose aliases resolve once; every
/// .formless/commands descendant and template is opened without following links.
/// Built-ins always win; project templates override global templates. No disk
/// writes, provider calls, attachment reads or argument expansion occur here.
pub fn load(config_dir: &Path, project_cwd: Option<&Path>, builtin_names: &[String]) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let builtins: HashSet<&str> = builtin_names.iter().map(String::as_str).collect();
    let mut selected = BTreeMap::<String, UserCommand>::new();
    let mut total = 0usize;
    let sources = [
        (Source::Global, Some(config_dir)),
        (Source::Project, project_cwd),
    ];
    for (source, root) in sources {
        let Some(root) = root else {
            continue;
        };
        let shown = if source == Source::Global {
            root.join("commands")
        } else {
            root.join(".formless/commands")
        };
        let loaded = (|| -> io::Result<native::Directory> {
            let mut directory = native::Directory::root(root)?;
            if source == Source::Project {
                directory = directory.child(".formless")?;
            }
            directory.child("commands")
        })();
        let directory = match loaded {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                snapshot.diagnostic(
                    &shown,
                    "Command directory is unreadable or uses an unsupported link/namespace",
                );
                continue;
            }
        };
        let mut names = match directory.names() {
            Ok(names) => names,
            Err(_) => {
                snapshot.diagnostic(
                    &shown,
                    "Command directory cannot be enumerated safely or exceeds 256 entries",
                );
                continue;
            }
        };
        names.sort();
        let mut count = 0;
        for name in names {
            let path = shown.join(&name);
            if Path::new(&name).extension() != Some(std::ffi::OsStr::new("md")) {
                continue;
            }
            count += 1;
            if count > MAX_COMMANDS {
                snapshot.diagnostic(&shown, "Command directory exceeds 128 Markdown templates");
                break;
            }
            let Some(name_text) = Path::new(&name)
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
            else {
                snapshot.diagnostic(&path, "Command filename is not UTF-8");
                continue;
            };
            if !valid_name(name_text) {
                snapshot.diagnostic(&path, "Command filename must be 1-64 lowercase ASCII letters/digits/_/- and start with a letter");
                continue;
            }
            let bytes = match directory.read(&name) {
                Ok(bytes) => bytes,
                Err(_) => {
                    snapshot.diagnostic(
                        &path,
                        "Command must be a readable ordinary single-link file of at most 64 KiB",
                    );
                    continue;
                }
            };
            if bytes.len() > MAX_TOTAL_BYTES.saturating_sub(total) {
                snapshot.diagnostic(&shown, "Combined command templates exceed 1 MiB");
                break;
            }
            total += bytes.len();
            let parsed = std::str::from_utf8(&bytes)
                .map_err(|_| "Command template is not UTF-8")
                .and_then(parse);
            let (description, body) = match parsed {
                Ok(parsed) => parsed,
                Err(message) => {
                    snapshot.diagnostic(&path, message);
                    continue;
                }
            };
            let (max_position, uses_arguments) = argument_metadata(&body);
            let command = UserCommand {
                name: name_text.into(),
                description,
                source,
                max_position,
                uses_arguments,
                path: path.clone(),
                body,
            };
            if builtins.contains(command.name.as_str()) {
                snapshot.shadowed.push(ShadowedCommand {
                    name: command.name,
                    path,
                    source,
                    reason: "Built-in command always wins".into(),
                });
                continue;
            }
            if let Some(previous) = selected.insert(command.name.clone(), command) {
                snapshot.shadowed.push(ShadowedCommand {
                    name: previous.name,
                    path: previous.path,
                    source: previous.source,
                    reason: "Project command overrides global command".into(),
                });
            }
        }
    }
    snapshot.commands = selected.into_values().collect();
    snapshot
}

fn read_file(file: File) -> io::Result<Vec<u8>> {
    let before = file.metadata()?;
    if !before.is_file() || before.len() > MAX_FILE_BYTES as u64 || !native::single_link(&file)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_FILE_BYTES as u64 + 1);
    reader.read_to_end(&mut bytes)?;
    let after = reader.get_ref().metadata()?;
    if bytes.len() > MAX_FILE_BYTES
        || bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
        || !native::single_link(reader.get_ref())?
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::{
        ffi::{CStr, CString, OsStr, OsString},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
    };
    pub(super) struct Directory(File);
    fn open(parent: Option<&File>, name: &OsStr, directory: bool) -> io::Result<File> {
        let name = CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        let fd = unsafe {
            libc::openat(
                parent.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd),
                name.as_ptr(),
                flags,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn single_link(file: &File) -> io::Result<bool> {
        Ok(file.metadata()?.nlink() == 1)
    }
    impl Directory {
        pub(super) fn root(path: &Path) -> io::Result<Self> {
            let path = path.canonicalize()?; // explicitly trusted configured root only
            let expected = std::fs::metadata(&path)?;
            let file = open(None, path.as_os_str(), true)?;
            let opened = file.metadata()?;
            if (opened.dev(), opened.ino()) != (expected.dev(), expected.ino()) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(Self(file))
        }
        pub(super) fn child(&self, name: &str) -> io::Result<Self> {
            Ok(Self(open(Some(&self.0), OsStr::new(name), true)?))
        }
        pub(super) fn read(&self, name: &OsStr) -> io::Result<Vec<u8>> {
            read_file(open(Some(&self.0), name, false)?)
        }
        pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                return Err(io::ErrorKind::Unsupported.into());
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                // dup shares the directory offset: restart it for this single,
                // privately owned snapshot before handing it to fdopendir.
                let fd = unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                let stream = unsafe { libc::fdopendir(fd) };
                if stream.is_null() {
                    let error = io::Error::last_os_error();
                    unsafe {
                        libc::close(fd);
                    }
                    return Err(error);
                }
                struct Stream(*mut libc::DIR);
                impl Drop for Stream {
                    fn drop(&mut self) {
                        unsafe {
                            libc::closedir(self.0);
                        }
                    }
                }
                let stream = Stream(stream);
                unsafe {
                    libc::rewinddir(stream.0);
                }
                let mut names = Vec::new();
                loop {
                    #[cfg(target_os = "linux")]
                    let errno = unsafe { libc::__errno_location() };
                    #[cfg(target_os = "macos")]
                    let errno = unsafe { libc::__error() };
                    unsafe {
                        *errno = 0;
                    }
                    let entry = unsafe { libc::readdir(stream.0) };
                    if entry.is_null() {
                        let code = unsafe { *errno };
                        return if code == 0 {
                            Ok(names)
                        } else {
                            Err(io::Error::from_raw_os_error(code))
                        };
                    }
                    let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                    if name == b"." || name == b".." {
                        continue;
                    }
                    if names.len() >= MAX_ENTRIES {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    names.push(OsStr::from_bytes(name).to_owned());
                }
            }
        }
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        ffi::{OsStr, OsString},
        mem::{offset_of, size_of},
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
            io::{AsRawHandle, FromRawHandle},
        },
        sync::{
            atomic::{AtomicBool, Ordering},
            Mutex,
        },
    };
    use windows_sys::{
        Wdk::{
            Foundation::OBJECT_ATTRIBUTES,
            Storage::FileSystem::{
                NtCreateFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
                FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
            },
        },
        Win32::{
            Foundation::{
                CloseHandle, RtlNtStatusToDosError, ERROR_NO_MORE_FILES, HANDLE,
                INVALID_HANDLE_VALUE, STATUS_PENDING, STATUS_SUCCESS, UNICODE_STRING,
                WAIT_OBJECT_0,
            },
            Storage::FileSystem::*,
            System::{
                Threading::{WaitForSingleObject, INFINITE},
                IO::{IO_STATUS_BLOCK, IO_STATUS_BLOCK_0},
            },
        },
    };
    static GATE: Mutex<()> = Mutex::new(());
    static POISONED: AtomicBool = AtomicBool::new(false);
    pub(super) struct Directory(File);
    struct Request {
        parent: File,
        name: Vec<u16>,
        unicode: UNICODE_STRING,
        attrs: OBJECT_ATTRIBUTES,
        status: IO_STATUS_BLOCK,
        output: HANDLE,
    }
    impl Drop for Request {
        fn drop(&mut self) {
            if !self.output.is_null() && self.output != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.output);
                }
            }
        }
    }
    fn open(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
        let _gate = GATE.lock().map_err(|_| io::ErrorKind::Other)?;
        if POISONED.load(Ordering::Acquire) {
            return Err(io::Error::other("command HANDLE open completion uncertain"));
        }
        let name: Vec<u16> = name.encode_wide().collect();
        if name.is_empty()
            || name.len() > 255
            || name.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92))
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut request = Box::new(Request {
            parent: parent.try_clone()?,
            name,
            unicode: UNICODE_STRING {
                Length: 0,
                MaximumLength: 0,
                Buffer: std::ptr::null_mut(),
            },
            attrs: OBJECT_ATTRIBUTES {
                Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
                RootDirectory: std::ptr::null_mut(),
                ObjectName: std::ptr::null(),
                Attributes: 0,
                SecurityDescriptor: std::ptr::null(),
                SecurityQualityOfService: std::ptr::null(),
            },
            status: IO_STATUS_BLOCK {
                Anonymous: IO_STATUS_BLOCK_0 {
                    Status: STATUS_PENDING,
                },
                Information: 0,
            },
            output: std::ptr::null_mut(),
        });
        request.unicode.Length = (request.name.len() * 2) as u16;
        request.unicode.MaximumLength = request.unicode.Length;
        request.unicode.Buffer = request.name.as_mut_ptr();
        request.attrs.RootDirectory = request.parent.as_raw_handle();
        request.attrs.ObjectName = &request.unicode;
        let access = if directory {
            FILE_LIST_DIRECTORY
        } else {
            FILE_READ_DATA
        };
        // Rooted, read-only, synchronous existing-object open. Every input and
        // output allocation remains owned through even unexpected pending I/O.
        let mut status = unsafe {
            NtCreateFile(
                &mut request.output,
                access | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &request.attrs,
                &mut request.status,
                std::ptr::null(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_OPEN_REPARSE_POINT
                    | FILE_SYNCHRONOUS_IO_NONALERT
                    | if directory {
                        FILE_DIRECTORY_FILE
                    } else {
                        FILE_NON_DIRECTORY_FILE
                    },
                std::ptr::null(),
                0,
            )
        };
        if status == STATUS_PENDING {
            let waited = if request.output.is_null() || request.output == INVALID_HANDLE_VALUE {
                None
            } else {
                Some(unsafe { WaitForSingleObject(request.output, INFINITE) })
            };
            if waited != Some(WAIT_OBJECT_0) {
                POISONED.store(true, Ordering::Release);
                std::mem::forget(request);
                return Err(io::Error::other("command HANDLE completion uncertain"));
            }
            status = unsafe { request.status.Anonymous.Status };
            if status == STATUS_PENDING {
                POISONED.store(true, Ordering::Release);
                std::mem::forget(request);
                return Err(io::Error::other("command HANDLE still pending"));
            }
        }
        if status != STATUS_SUCCESS {
            return Err(if status < 0 {
                io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
            } else {
                io::Error::other("ambiguous native command open")
            });
        }
        if request.output.is_null() || request.output == INVALID_HANDLE_VALUE {
            return Err(io::ErrorKind::Other.into());
        }
        let raw = request.output;
        request.output = std::ptr::null_mut();
        let file = unsafe { File::from_raw_handle(raw) };
        let metadata = file.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file()
            }
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(file)
    }
    pub(super) fn single_link(file: &File) -> io::Result<bool> {
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.assume_init() }.nNumberOfLinks == 1)
    }
    impl Directory {
        pub(super) fn root(path: &Path) -> io::Result<Self> {
            let path = path.canonicalize()?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(Self(file))
        }
        pub(super) fn child(&self, name: &str) -> io::Result<Self> {
            Ok(Self(open(&self.0, OsStr::new(name), true)?))
        }
        pub(super) fn read(&self, name: &OsStr) -> io::Result<Vec<u8>> {
            read_file(open(&self.0, name, false)?)
        }
        pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
            let mut storage = vec![0u64; 8192];
            let mut names = Vec::new();
            // The total entry bound also bounds the number of native batches,
            // even for a filesystem returning only one entry per response.
            for batch in 0..MAX_ENTRIES + 3 {
                storage.fill(0);
                if unsafe {
                    GetFileInformationByHandleEx(
                        self.0.as_raw_handle(),
                        if batch == 0 {
                            FileIdBothDirectoryRestartInfo
                        } else {
                            FileIdBothDirectoryInfo
                        },
                        storage.as_mut_ptr().cast(),
                        (storage.len() * 8) as u32,
                    )
                } == 0
                {
                    let error = io::Error::last_os_error();
                    return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                        Ok(names)
                    } else {
                        Err(error)
                    };
                }
                let buffer = unsafe {
                    std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), storage.len() * 8)
                };
                let mut offset = 0;
                loop {
                    let remaining = buffer.get(offset..).ok_or(io::ErrorKind::InvalidData)?;
                    let start = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
                    let length_offset = offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
                    if remaining.len() < start {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    let next = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
                    let length = u32::from_le_bytes(
                        remaining[length_offset..length_offset + 4]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    if length == 0
                        || length % 2 != 0
                        || start + length > remaining.len()
                        || (next != 0
                            && (next % 8 != 0 || next < start + length || next >= remaining.len()))
                    {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    let wide: Vec<u16> = remaining[start..start + length]
                        .chunks_exact(2)
                        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                        .collect();
                    if wide.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92)) {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    if wide != [46] && wide != [46, 46] {
                        if names.len() >= MAX_ENTRIES {
                            return Err(io::ErrorKind::InvalidData.into());
                        }
                        names.push(OsString::from_wide(&wide));
                    }
                    if next == 0 {
                        break;
                    }
                    offset += next;
                }
            }
            Err(io::ErrorKind::InvalidData.into())
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod native {
    use super::*;
    pub(super) struct Directory;
    pub(super) fn single_link(_: &File) -> io::Result<bool> {
        Err(io::ErrorKind::Unsupported.into())
    }
    impl Directory {
        pub(super) fn root(_: &Path) -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn child(&self, _: &str) -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn names(&self) -> io::Result<Vec<std::ffi::OsString>> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(super) fn read(&self, _: &std::ffi::OsStr) -> io::Result<Vec<u8>> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("formless-user-commands-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(path.join("config/commands")).unwrap();
            std::fs::create_dir_all(path.join("project/.formless/commands")).unwrap();
            Self(path)
        }
        fn config(&self) -> PathBuf {
            self.0.join("config")
        }
        fn cwd(&self) -> PathBuf {
            self.0.join("project")
        }
        fn global(&self, name: &str, text: &str) {
            std::fs::write(self.config().join("commands").join(name), text).unwrap();
        }
        fn project(&self, name: &str, text: &str) {
            std::fs::write(self.cwd().join(".formless/commands").join(name), text).unwrap();
        }
        fn load(&self, builtins: &[String]) -> Snapshot {
            load(&self.config(), Some(&self.cwd()), builtins)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn command(body: &str) -> UserCommand {
        let (max_position, uses_arguments) = argument_metadata(body);
        UserCommand {
            name: "test".into(),
            description: "test".into(),
            source: Source::Global,
            max_position,
            uses_arguments,
            path: "test.md".into(),
            body: body.into(),
        }
    }

    #[test]
    fn argument_metadata_matches_expansion_and_metadata_projection() {
        let fixture = Fixture::new();
        for (body, maximum, arguments) in [
            ("$1/$2", 2, false),
            ("$$5", 0, false),
            ("$10 $0 $100", 0, false),
            ("$9", 9, false),
            ("$ARGUMENTS", 0, true),
            ("$$ARGUMENTS", 0, false),
            ("$$$3 $ARGUMENTSsuffix", 3, false),
            ("雪 $1é $9.99", 9, false),
            ("no arguments $HOME $", 0, false),
        ] {
            fixture.global("test.md", body);
            let snapshot = fixture.load(&[]);
            let command = snapshot.find("test").unwrap();
            assert_eq!(
                (command.max_position, command.uses_arguments),
                (maximum, arguments),
                "{body}"
            );
            let projection = snapshot.projection();
            let info = &projection.commands[0];
            assert_eq!(
                (info.max_position, info.uses_arguments),
                (maximum, arguments)
            );
            assert_eq!(info.usage(), command.usage());
            assert_eq!(serde_json::to_value(info).unwrap()["max_position"], maximum);
            let supplied = (1..=maximum).map(|_| "arg").collect::<Vec<_>>().join(" ");
            assert!(command.expand(&supplied).is_ok(), "{body}");
            if maximum > 0 {
                let too_few = (1..maximum).map(|_| "arg").collect::<Vec<_>>().join(" ");
                assert!(command.expand(&too_few).is_err(), "{body}");
            }
        }
        assert_eq!(
            command("$1 $2 $ARGUMENTS").usage(),
            "/test <arg1> <arg2> [arguments…]"
        );
        assert_eq!(command("$$5").usage(), "/test");
        assert_eq!(command("$ARGUMENTS").usage(), "/test [arguments…]");
    }

    #[test]
    fn literal_expansion_is_one_pass_preserves_mentions_and_never_executes() {
        let template = command(
            "$ARGUMENTS|$1|$2|$3|$10|$0|$HOME|$(touch never)|`touch never`|@notes.md|/quit",
        );
        let expanded = template.expand("$2  @private.txt\n$ARGUMENTS").unwrap();
        assert_eq!(expanded,"$2  @private.txt\n$ARGUMENTS|$2|@private.txt|$ARGUMENTS|$10|$0|$HOME|$(touch never)|`touch never`|@notes.md|/quit");
        assert_eq!(command("é-$1-$ARGUMENTS").expand("雪").unwrap(), "é-雪-雪");
        assert!(command("$1/$2")
            .expand("")
            .unwrap_err()
            .to_string()
            .contains("references $1"));
        assert_eq!(
            command("$1 $2 $3").expand("'two words' x").unwrap(),
            "'two words' x"
        );
    }
    #[test]
    fn arguments_placeholder_requires_an_identifier_boundary() {
        for (body, expected, uses_arguments) in [
            ("Use $ARGUMENTS_PATH now", "Use $ARGUMENTS_PATH now", false),
            ("See $ARGUMENTS2.", "See $ARGUMENTS2.", false),
            ("$ARGUMENTSsuffix", "$ARGUMENTSsuffix", false),
            (
                "$ARGUMENTS_FILE $ARGUMENTS9",
                "$ARGUMENTS_FILE $ARGUMENTS9",
                false,
            ),
            ("See $ARGUMENTS.", "See report.md.", true),
            ("e-$ARGUMENTS", "e-report.md", true),
            ("$ARGUMENTSé", "report.mdé", true),
            ("$$ARGUMENTS_PATH", "$ARGUMENTS_PATH", false),
            (
                "$ARGUMENTS_PATH $1PATH $10",
                "$ARGUMENTS_PATH report.mdPATH $10",
                false,
            ),
        ] {
            let template = command(body);
            assert_eq!(template.expand("report.md").unwrap(), expected, "{body}");
            assert_eq!(template.uses_arguments, uses_arguments, "{body}");
        }
        let fixture = Fixture::new();
        fixture.global("literal.md", "$ARGUMENTS_PATH");
        let snapshot = fixture.load(&[]);
        assert_eq!(snapshot.find("literal").unwrap().usage(), "/literal");
        assert!(!snapshot.projection().commands[0].uses_arguments);
    }

    #[test]
    fn dollar_escapes_preserve_prices_code_and_single_pass_substitution() {
        for (body, expected) in [
            ("Keep it under $$5 per run", "Keep it under $5 per run"),
            ("Our plan costs $$9.99", "Our plan costs $9.99"),
            ("Explain `echo $$1`", "Explain `echo $1`"),
            ("$$ARGUMENTS", "$ARGUMENTS"),
            ("$$$$ / $0 / $10 / $100", "$$ / $0 / $10 / $100"),
            ("trailing $", "trailing $"),
        ] {
            assert_eq!(command(body).expand("").unwrap(), expected);
        }
        assert_eq!(command("$$$1|$ARGUMENTS").expand("$2").unwrap(), "$$2|$2");
        assert_eq!(
            command("$1/$2/$3/$4/$5/$6/$7/$8/$9")
                .expand("a b c d e f g h i")
                .unwrap(),
            "a/b/c/d/e/f/g/h/i"
        );
    }

    #[test]
    fn missing_positions_are_named_actionable_errors_without_partial_output() {
        for (body, args, position, count) in [
            ("Keep it under $5 per run", "", 5, 0),
            ("Our plan costs $9.99", "", 9, 0),
            ("Explain `echo $1`", "", 1, 0),
            ("first $1 then $2", "private-argument", 2, 1),
            ("$9", "a b c d e f g h", 9, 8),
        ] {
            let error = command(body).expand(args).unwrap_err().to_string();
            assert!(
                error.contains("test.md")
                    && error.contains(&format!("references ${position}"))
                    && error.contains(&format!("only {count} argument(s)")),
                "{error}"
            );
            assert!(error.contains(&format!("write $${position}")), "{error}");
            assert!(
                !error.contains("private-argument"),
                "argument contents do not enter diagnostics"
            );
        }
    }

    #[test]
    fn expansion_bounds_arguments_and_repeated_insertions_before_allocation_growth() {
        assert!(command("$ARGUMENTS")
            .expand(&"x".repeat(MAX_ARGUMENT_BYTES + 1))
            .is_err());
        let args = "x".repeat(MAX_ARGUMENT_BYTES);
        assert_eq!(
            command("$ARGUMENTS").expand(&args).unwrap().len(),
            MAX_ARGUMENT_BYTES
        );
        assert!(command(&"$ARGUMENTS".repeat(17)).expand(&args).is_err());
        assert_eq!(
            command(&"$ARGUMENTS".repeat(16))
                .expand(&args)
                .unwrap()
                .len(),
            MAX_EXPANDED_BYTES
        );
    }
    #[test]
    fn reload_precedence_builtin_shadowing_and_metadata_projection_are_explicit() {
        let fixture = Fixture::new();
        fixture.global(
            "review.md",
            "---\ndescription: Global review\n---\nGLOBAL BODY SECRET",
        );
        fixture.project(
            "review.md",
            "---\ndescription: Project review\n---\nPROJECT BODY SECRET @notes",
        );
        fixture.global(
            "help.md",
            "---\ndescription: Shadowed help\n---\nNever overrides builtins",
        );
        let first = fixture.load(&["help".into()]);
        assert_eq!(first.commands.len(), 1);
        assert_eq!(first.find("review").unwrap().source, Source::Project);
        assert_eq!(
            first.find("review").unwrap().expand("").unwrap(),
            "PROJECT BODY SECRET @notes"
        );
        assert!(first.find("help").is_none());
        assert_eq!(first.shadowed.len(), 2);
        assert!(first
            .shadowed
            .iter()
            .any(|shadow| shadow.name == "help" && shadow.reason.contains("Built-in")));
        assert!(first
            .shadowed
            .iter()
            .any(|shadow| shadow.name == "review" && shadow.source == Source::Global));
        let json = serde_json::to_string(&first.projection()).unwrap();
        assert!(!json.contains("BODY SECRET") && !json.contains("@notes"));
        fixture.project("review.md", "UPDATED");
        fixture.global("added.md", "New command");
        let second = fixture.load(&["help".into()]);
        assert_eq!(
            second.find("review").unwrap().expand("").unwrap(),
            "UPDATED"
        );
        assert!(second.find("added").is_some());
        std::fs::remove_file(fixture.cwd().join(".formless/commands/review.md")).unwrap();
        assert_eq!(
            fixture.load(&[]).find("review").unwrap().source,
            Source::Global
        );
    }
    #[test]
    fn malformed_files_have_one_stable_note_without_body_disclosure() {
        let fixture = Fixture::new();
        fixture.global(
            "bad.md",
            "---\ndescription: Secret invalid\nNEVER PRINT THIS BODY",
        );
        let first = fixture.load(&[]);
        let second = fixture.load(&[]);
        assert!(first.commands.is_empty());
        assert_eq!(first.diagnostics.len(), 1);
        assert_eq!(first.diagnostics, second.diagnostics);
        assert!(!serde_json::to_string(&first.projection())
            .unwrap()
            .contains("NEVER PRINT"));
        fixture.global("bad name.md", "ordinary");
        fixture.global("binary.md", "\0binary");
        fixture.global("valid.md", "Plain Markdown");
        assert_eq!(fixture.load(&[]).commands.len(), 1);
    }
    #[test]
    fn frontmatter_subset_and_crlf_are_bounded_and_unambiguous() {
        for (text, description, body) in [
            (
                "---\ndescription: Simple text\n---\nBody",
                "Simple text",
                "Body",
            ),
            (
                "---\r\ndescription: \"Quoted text\"\r\n---\r\nBody",
                "Quoted text",
                "Body",
            ),
            (
                "---\ndescription: 'It''s useful'\n---\nBody",
                "It's useful",
                "Body",
            ),
            (
                "---\ndescription: >\n  First line\n  second line\n---\nBody",
                "First line second line",
                "Body",
            ),
            ("Plain body", "User command", "Plain body"),
        ] {
            assert_eq!(parse(text).unwrap(), (description.into(), body.into()));
        }
        for text in [
            "---\nmissing closure",
            "---\nother: value\n---\nBody",
            "---\ndescription: [bad]\n---\nBody",
            "---\ndescription: \"unfinished\n---\nBody",
            "---\ndescription: a\ndescription: b\n---\nBody",
            "---\ndescription:\n---\nBody",
            "---\ndescription: 'a'b'\n---\nBody",
            "",
        ] {
            assert!(parse(text).is_err(), "{text:?}");
        }
    }
    #[test]
    fn frontmatter_comments_preserve_scalar_text_and_command_discovery() {
        for (header, expected) in [
            (
                " # review helper\n  # another note\ndescription: Review the diff",
                "Review the diff",
            ),
            ("description: Review the diff # why", "Review the diff"),
            ("description: \"Review the diff\" # why", "Review the diff"),
            ("description: 'Review the diff' # why", "Review the diff"),
            ("description: \"hash # stays\" # annotation", "hash # stays"),
            (
                "description: 'It''s # useful' # annotation",
                "It's # useful",
            ),
            (
                r#"description: "Say \"hi\" # text" # annotation"#,
                "Say \"hi\" # text",
            ),
            ("description: Bob's notes # annotation", "Bob's notes"),
            (
                "description: https://example.test/#anchor # annotation",
                "https://example.test/#anchor",
            ),
            ("description: C# helpers # annotation", "C# helpers"),
            (
                "description: |- # header annotation\n  literal # content",
                "literal # content",
            ),
            (
                "description: >- # header annotation\n  first\n  second",
                "first second",
            ),
            ("\t# note\r\ndescription: Simple\t# comment", "Simple"),
        ] {
            let source = format!("---\n{header}\n---\n# Body stays literal\n$ARGUMENTS_PATH\n");
            let (description, body) = parse(&source).unwrap_or_else(|e| panic!("{header}: {e}"));
            assert_eq!(description, expected, "{header}");
            assert_eq!(body, "# Body stays literal\n$ARGUMENTS_PATH\n");
            let fixture = Fixture::new();
            fixture.global("review.md", &source);
            let snapshot = fixture.load(&[]);
            assert!(
                snapshot.diagnostics.is_empty(),
                "{header}: {:?}",
                snapshot.diagnostics
            );
            assert_eq!(snapshot.find("review").unwrap().description, expected);
            assert_eq!(snapshot.projection().commands[0].description, expected);
        }
        for header in [
            "description: \"unfinished # still quoted",
            "description: 'unfinished # still quoted",
            "description: \"fine\" trailing # invalid",
            "description: 'fine' trailing # invalid",
            "description: # only comment",
            "description: \"\" # empty",
            "description: first # comment\ndescription: second",
            " # note\nother: unsupported",
        ] {
            assert!(
                parse(&format!("---\n{header}\n---\nBody")).is_err(),
                "{header}"
            );
        }
    }

    #[test]
    fn discovery_limits_oversized_and_excess_entries_without_attachment_reads() {
        let fixture = Fixture::new();
        fixture.global("large.md", &"x".repeat(MAX_FILE_BYTES + 1));
        fixture.global("reference.md", "@does-not-exist $(never) `never` /quit");
        let snapshot = fixture.load(&[]);
        assert!(snapshot.find("large").is_none());
        assert!(snapshot.find("reference").is_some());
        assert_eq!(snapshot.diagnostics.len(), 1);
        for index in 0..MAX_ENTRIES {
            fixture.project(&format!("entry{index}.md"), "Body");
        }
        fixture.project("overflow.md", "Body");
        let snapshot = fixture.load(&[]);
        assert!(snapshot
            .commands
            .iter()
            .all(|command| command.source == Source::Global));
        assert!(snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("256 entries")));
    }
    #[test]
    fn hardlinks_are_refused_before_template_bytes_are_read() {
        let fixture = Fixture::new();
        let secret = fixture.0.join("credential");
        std::fs::write(&secret, b"PRIVATE CREDENTIAL CONTENT").unwrap();
        std::fs::hard_link(&secret, fixture.config().join("commands/stolen.md")).unwrap();
        let snapshot = fixture.load(&[]);
        assert!(snapshot.find("stolen").is_none());
        assert_eq!(snapshot.diagnostics.len(), 1);
        assert!(!serde_json::to_string(&snapshot.projection())
            .unwrap()
            .contains("PRIVATE CREDENTIAL"));
    }
    #[cfg(unix)]
    #[test]
    fn project_directory_and_template_symlinks_never_read_credentials() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let secret = fixture.0.join("credentials");
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("secret.md"), b"PRIVATE CREDENTIAL CONTENT").unwrap();
        symlink(
            secret.join("secret.md"),
            fixture.config().join("commands/leak.md"),
        )
        .unwrap();
        std::fs::remove_dir_all(fixture.cwd().join(".formless")).unwrap();
        symlink(&secret, fixture.cwd().join(".formless")).unwrap();
        let snapshot = fixture.load(&[]);
        assert!(snapshot.commands.is_empty());
        assert_eq!(snapshot.diagnostics.len(), 2);
        assert!(!serde_json::to_string(&snapshot.projection())
            .unwrap()
            .contains("PRIVATE CREDENTIAL"));
    }
    #[cfg(unix)]
    #[test]
    fn trusted_config_alias_and_non_utf8_diagnostics_do_not_break_json() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let fixture = Fixture::new();
        fixture.global("review.md", "Body");
        let alias = fixture.0.join("trusted-config-alias");
        symlink(fixture.config(), &alias).unwrap();
        let bad = std::ffi::OsStr::from_bytes(b"bad\xff.md");
        let bad_path = fixture.config().join("commands").join(bad);
        let created = match std::fs::write(&bad_path, b"Body") {
            Ok(()) => true,
            // APFS rejects invalid UTF-8 names. Still exercise diagnostic
            // serialization below without requiring an impossible directory entry.
            Err(error)
                if cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EILSEQ) =>
            {
                false
            }
            Err(error) => panic!("create malformed-name fixture: {error}"),
        };
        let snapshot = load(&alias, None, &[]);
        assert!(snapshot.find("review").is_some());
        assert_eq!(snapshot.diagnostics.len(), usize::from(created));
        if created {
            assert_eq!(
                snapshot.diagnostics[0].message,
                "Command filename is not UTF-8"
            );
        }
        let mut snapshot = snapshot;
        if !created {
            snapshot.diagnostic(&bad_path, "Command filename is not UTF-8");
        }
        assert_eq!(snapshot.diagnostics.len(), 1);
        assert!(serde_json::to_string(&snapshot.projection()).is_ok());
    }
}
