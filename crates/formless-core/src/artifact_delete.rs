//! SQLite paths never confer filesystem deletion authority. Unix traverses
//! ordinary components through pinned no-follow descriptors. Windows authorizes
//! deletion using exact physical final-handle containment; directory names and
//! attributes can be observed transiently during an in-place reparse race.
use std::{
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    Foreign,
    Missing,
    NonRegular,
}
impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "artifact path rejected: {self:?}")
    }
}
impl std::error::Error for Rejection {}
fn rejected(reason: Rejection) -> io::Error {
    io::Error::new(
        if reason == Rejection::Missing {
            io::ErrorKind::NotFound
        } else {
            io::ErrorKind::PermissionDenied
        },
        reason,
    )
}
/// Classification is explicit; e.g. a Windows final-path query can return
/// NotFound for an EXISTING network file with no GUID volume identity.
pub(crate) fn rejection(error: &io::Error) -> Option<Rejection> {
    error.get_ref()?.downcast_ref::<Rejection>().copied()
}
fn lookup(error: io::Error) -> io::Error {
    if rejection(&error).is_some() {
        return error;
    }
    #[cfg(unix)]
    if error.kind() == io::ErrorKind::NotFound {
        return rejected(Rejection::Missing);
    }
    // A Windows absolute lookup can race directory retagging before any
    // final handle exists. Even its NotFound cannot prove owned-file absence.
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENOTDIR | libc::EISDIR)
    ) {
        return rejected(Rejection::NonRegular);
    }
    error
}
#[derive(Debug)]
struct Uncertain(&'static str, io::Error);
impl std::fmt::Display for Uncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0, self.1)
    }
}
impl std::error::Error for Uncertain {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.1)
    }
}
fn uncertain(context: &'static str, error: io::Error) -> io::Error {
    io::Error::other(Uncertain(context, error))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileIdentity {
    volume: u64,
    file: u64,
}
#[derive(Debug)]
pub(crate) struct VerifiedMetadata {
    metadata: std::fs::Metadata,
    pub(crate) identity: FileIdentity,
}
impl std::ops::Deref for VerifiedMetadata {
    type Target = std::fs::Metadata;
    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}

#[cfg(windows)]
#[path = "artifact_delete_windows_directory.rs"]
mod directory_entries;

pub(crate) struct Root {
    logical: PathBuf,
    resolved: PathBuf,
    directory: File,
    #[cfg(windows)]
    _ancestors: Vec<File>,
    #[cfg(windows)]
    physical_root: PathBuf,
    #[cfg(windows)]
    enumeration: std::sync::Mutex<()>,
}

impl Root {
    pub(crate) fn open(root: &Path) -> io::Result<Self> {
        let logical = std::path::absolute(root)?;
        // Configured profile-directory aliases are trusted configuration;
        // resolve them once. The artifacts entry itself is NEVER resolved.
        let parent = logical.parent().ok_or(io::ErrorKind::PermissionDenied)?;
        let resolved = parent
            .canonicalize()
            .map_err(|e| uncertain("profile-directory identity could not be verified", e))?
            .join(logical.file_name().ok_or(io::ErrorKind::PermissionDenied)?);
        platform::root(logical, resolved)
    }

    pub(crate) fn relative(&self, path: &Path) -> io::Result<PathBuf> {
        let path = std::path::absolute(path)?;
        let relative = path
            .strip_prefix(&self.logical)
            .or_else(|_| path.strip_prefix(&self.resolved))
            .map_err(|_| rejected(Rejection::Foreign))?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(rejected(Rejection::Foreign));
        }
        Ok(relative.into())
    }

    /// Enumerate through the pinned root, never through an untrusted named
    /// directory path. Empty relative selects the artifact root itself.
    pub(crate) fn entries(&self, relative: &Path) -> io::Result<Vec<PathBuf>> {
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(rejected(Rejection::Foreign));
        }
        platform::entries(self, relative)
    }

    pub(crate) fn metadata(&self, path: &Path) -> io::Result<VerifiedMetadata> {
        platform::metadata(self, &self.relative(path)?)
    }

    #[cfg(all(unix, test))]
    pub(crate) fn remove_with_hook(&self, path: &Path, hook: impl FnOnce()) -> io::Result<()> {
        platform::remove_with(self, &self.relative(path)?, hook)
    }

    pub(crate) fn remove(&self, path: &Path) -> io::Result<()> {
        platform::remove(self, &self.relative(path)?)
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{
        ffi::{CString, OsStr},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };
    fn directory(parent: Option<&File>, name: &OsStr) -> io::Result<File> {
        let name = CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
        // SAFETY: valid borrowed descriptor/name; every successful fd becomes
        // owned File. O_NOFOLLOW applies to EACH traversed directory entry.
        let fd = unsafe {
            libc::openat(
                parent.map_or(libc::AT_FDCWD, |p| p.as_raw_fd()),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(lookup(io::Error::last_os_error()));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn root(logical: PathBuf, resolved: PathBuf) -> io::Result<Root> {
        let mut dir = directory(None, OsStr::new("/"))?;
        for part in resolved.components().skip(1) {
            let Component::Normal(name) = part else {
                return Err(rejected(Rejection::Foreign));
            };
            dir = directory(Some(&dir), name)?;
        }
        Ok(Root {
            logical,
            resolved,
            directory: dir,
        })
    }
    fn parent(root: &Root, relative: &Path) -> io::Result<(File, CString)> {
        let mut dir = root.directory.try_clone()?;
        let mut parts = relative.components().peekable();
        while let Some(Component::Normal(name)) = parts.next() {
            if parts.peek().is_none() {
                return Ok((
                    dir,
                    CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?,
                ));
            }
            dir = directory(Some(&dir), name)?;
        }
        Err(rejected(Rejection::Foreign))
    }
    pub(super) fn entries(root: &Root, relative: &Path) -> io::Result<Vec<PathBuf>> {
        use std::os::fd::IntoRawFd;
        let mut directory_file = directory(Some(&root.directory), OsStr::new("."))?;
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err(rejected(Rejection::Foreign));
            };
            directory_file = directory(Some(&directory_file), name)?;
        }
        let fd = directory_file.into_raw_fd();
        // SAFETY: owned directory fd transfers to DIR on success. On failure
        // ownership remains ours and is closed immediately.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe {
                libc::close(fd);
            };
            return Err(uncertain(
                "directory enumeration could not start",
                io::Error::last_os_error(),
            ));
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
        let mut entries = Vec::new();
        loop {
            // SAFETY: valid DIR exclusively used on this stack. The returned
            // name is copied before the next readdir or closedir invalidates it.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            entries.push(root.logical.join(relative).join(OsStr::from_bytes(name)));
        }
        Ok(entries)
    }

    fn regular(parent: &File, name: &CString) -> io::Result<()> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: live parent fd, terminated name, valid output storage.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(lookup(io::Error::last_os_error()));
        }
        if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(rejected(Rejection::NonRegular));
        }
        Ok(())
    }
    pub(super) fn metadata(root: &Root, relative: &Path) -> io::Result<VerifiedMetadata> {
        let (parent, name) = parent(root, relative)?;
        regular(&parent, &name)?;
        // O_NONBLOCK prevents a concurrently substituted FIFO blocking cleanup.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(lookup(io::Error::last_os_error()));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let info = file.metadata()?;
        if !info.is_file() {
            return Err(rejected(Rejection::NonRegular));
        }
        use std::os::unix::fs::MetadataExt;
        Ok(VerifiedMetadata {
            identity: FileIdentity {
                volume: info.dev(),
                file: info.ino(),
            },
            metadata: info,
        })
    }
    pub(super) fn remove(root: &Root, relative: &Path) -> io::Result<()> {
        remove_with(root, relative, || {})
    }
    pub(super) fn remove_with(
        root: &Root,
        relative: &Path,
        before_unlink: impl FnOnce(),
    ) -> io::Result<()> {
        let (parent, name) = parent(root, relative)?;
        regular(&parent, &name)?;
        before_unlink();
        // unlinkat does not follow its final component, even if that entry is
        // swapped for a symlink after inspection. The pinned parent cannot be
        // redirected by replacing an ancestor with a symlink.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(lookup(io::Error::last_os_error()));
        }
        Ok(())
    }
}

/// Only canonical GUID-volume paths returned by GetFinalPathNameByHandle
/// participate in authority comparisons. Exact case is deliberate: NTFS may
/// enable case-sensitive directories, so case folding could broaden ownership.
#[cfg(any(windows, test))]
fn windows_contained(root: &str, path: &str, allow_root: bool) -> bool {
    fn parts(path: &str) -> Option<Vec<&str>> {
        let path = path.strip_prefix("\\\\?\\")?;
        if path.len() > 131072 || path.contains('/') {
            return None;
        }
        let mut parts: Vec<_> = path.trim_end_matches('\\').split('\\').collect();
        let volume = *parts.first()?;
        let guid = volume.strip_prefix("Volume{")?.strip_suffix('}')?;
        if uuid::Uuid::parse_str(guid).is_err() || guid.len() != 36 {
            return None;
        }
        if parts.len() < 2
            || parts
                .iter()
                .skip(1)
                .any(|p| p.is_empty() || *p == "." || *p == ".." || !windows_component(p))
        {
            return None;
        }
        Some(std::mem::take(&mut parts))
    }
    let (Some(root), Some(path)) = (parts(root), parts(path)) else {
        return false;
    };
    path.starts_with(&root) && (allow_root || path.len() > root.len())
}

/// Avoid native path normalization and DOS device aliases even if a caller
/// supplies a non-verbatim spelling. This is not a string-prefix authority check.
#[cfg(any(windows, test))]
fn windows_component(name: &str) -> bool {
    if name.is_empty()
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| {
            c < ' '
                || matches!(
                    c,
                    '\0' | '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|'
                )
        })
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(['.', ' '])
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::{
        ffi::OsStringExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    };
    use windows_sys::Win32::Storage::FileSystem::*;
    fn final_path(file: &File) -> io::Result<PathBuf> {
        let mut buffer = vec![0u16; 32768];
        // SAFETY: live borrowed handle and correctly sized writable UTF-16
        // buffer. GUID-volume normalized names avoid drive-letter aliases.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_GUID,
            )
        } as usize;
        if length == 0 {
            return Err(uncertain(
                "native artifact verification failed",
                io::Error::last_os_error(),
            ));
        }
        if length >= buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native artifact path exceeds verification limit",
            ));
        }
        Ok(std::ffi::OsString::from_wide(&buffer[..length]).into())
    }
    fn contained(root: &Root, file: &File, allow_root: bool) -> io::Result<()> {
        let path = final_path(file)?;
        let (Some(root), Some(path)) = (root.physical_root.to_str(), path.to_str()) else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ambiguous native artifact path encoding",
            ));
        };
        if !super::windows_contained(root, root, true)
            || !super::windows_contained(path, path, true)
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported native artifact volume or path namespace",
            ));
        }
        if !super::windows_contained(root, path, allow_root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native physical artifact scope could not be verified",
            ));
        }
        Ok(())
    }
    fn directory(path: &Path) -> io::Result<File> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let info = file
            .metadata()
            .map_err(|e| uncertain("directory handle metadata could not be verified", e))?;
        if !info.is_dir() || info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(rejected(Rejection::NonRegular));
        }
        Ok(file)
    }
    fn absent(parent: &File, name: &std::ffi::OsStr, error: io::Error) -> io::Error {
        if error.kind() != io::ErrorKind::NotFound {
            return error;
        }
        match super::directory_entries::child_missing(parent, name) {
            Ok(true) => rejected(Rejection::Missing),
            Ok(false) => uncertain(
                "named lookup failed for a child still present in its pinned parent",
                error,
            ),
            Err(proof) => uncertain(
                "child absence could not be verified on its pinned parent",
                proof,
            ),
        }
    }
    fn physical_child(expected: &Path, file: &File) -> io::Result<()> {
        let actual = final_path(file)?;
        if !expected
            .to_str()
            .zip(actual.to_str())
            .is_some_and(|(expected, actual)| {
                super::windows_contained(expected, expected, true)
                    && super::windows_contained(actual, actual, true)
            })
            || actual.parent().map(Path::as_os_str) != expected.parent().map(Path::as_os_str)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "opened child physical identity does not match its original pinned parent",
            ));
        }
        Ok(())
    }
    fn child_directory(parent: &File, named: &Path, name: &std::ffi::OsStr) -> io::Result<File> {
        let expected = final_path(parent)?.join(name);
        let file = directory(named).map_err(|error| absent(parent, name, error))?;
        physical_child(&expected, &file)?;
        Ok(file)
    }
    pub(super) fn root(logical: PathBuf, resolved: PathBuf) -> io::Result<Root> {
        let mut named = PathBuf::new();
        let mut guards = Vec::new();
        for part in resolved.components() {
            named.push(part.as_os_str());
            match part {
                Component::Prefix(_) => continue,
                Component::RootDir => guards.push(directory(&named)?),
                Component::Normal(name) => {
                    if !name.to_str().is_some_and(super::windows_component) {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "artifact root component is ambiguous",
                        ));
                    }
                    let parent = guards.last().ok_or(io::ErrorKind::PermissionDenied)?;
                    guards.push(child_directory(parent, &named, name)?);
                }
                _ => return Err(io::ErrorKind::PermissionDenied.into()),
            }
        }
        let dir = guards.pop().ok_or(io::ErrorKind::PermissionDenied)?;
        let physical_root = final_path(&dir)?;
        if !physical_root
            .to_str()
            .is_some_and(|p| super::windows_contained(p, p, true))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native artifact root volume is unverifiable",
            ));
        }
        Ok(Root {
            logical,
            resolved,
            directory: dir,
            _ancestors: guards,
            physical_root,
            enumeration: std::sync::Mutex::new(()),
        })
    }
    pub(super) fn entries(root: &Root, relative: &Path) -> io::Result<Vec<PathBuf>> {
        let _serial = root
            .enumeration
            .lock()
            .map_err(|_| io::Error::other("directory enumeration state is poisoned"))?;
        let mut named = root.resolved.clone();
        let mut guards = Vec::new();
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err(rejected(Rejection::Foreign));
            };
            if !name.to_str().is_some_and(super::windows_component) {
                return Err(rejected(Rejection::Foreign));
            }
            named.push(name);
            let parent = guards.last().unwrap_or(&root.directory);
            let guard = child_directory(parent, &named, name)?;
            contained(root, &guard, false)?;
            guards.push(guard);
        }
        let directory = guards.last().unwrap_or(&root.directory);
        contained(root, directory, true)?;
        Ok(super::directory_entries::entries(directory)
            .map_err(|error| uncertain("pinned directory enumeration could not complete", error))?
            .into_iter()
            .map(|name| root.logical.join(relative).join(name))
            .collect())
    }
    fn file(root: &Root, relative: &Path, deleting: bool) -> io::Result<(File, Vec<File>)> {
        let mut named = root.resolved.clone();
        let mut guards = Vec::new();
        let mut parts = relative.components().peekable();
        while let Some(Component::Normal(name)) = parts.next() {
            if !name.to_str().is_some_and(super::windows_component) {
                return Err(rejected(Rejection::Foreign));
            }
            named.push(name);
            let parent = guards.last().unwrap_or(&root.directory);
            if parts.peek().is_some() {
                let guard = child_directory(parent, &named, name)?;
                contained(root, &guard, false)?;
                guards.push(guard);
            } else {
                let expected = final_path(parent)?.join(name);
                let file = std::fs::OpenOptions::new()
                    .access_mode(FILE_READ_ATTRIBUTES | if deleting { DELETE } else { 0 })
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(&named)
                    .map_err(|error| absent(parent, name, error))?;
                physical_child(&expected, &file)?;
                contained(root, &file, false)?;
                let info = file
                    .metadata()
                    .map_err(|e| uncertain("artifact handle metadata could not be verified", e))?;
                if !info.is_file() || info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(rejected(Rejection::NonRegular));
                }
                return Ok((file, guards));
            }
        }
        Err(rejected(Rejection::Foreign))
    }
    pub(super) fn metadata(root: &Root, relative: &Path) -> io::Result<VerifiedMetadata> {
        let _serial = root
            .enumeration
            .lock()
            .map_err(|_| io::Error::other("directory enumeration state is poisoned"))?;
        let (file, _guards) = file(root, relative, false)?;
        let metadata = file
            .metadata()
            .map_err(|e| uncertain("artifact metadata read failed", e))?;
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: live verified leaf HANDLE and valid native output storage.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(uncertain(
                "artifact file identity read failed",
                io::Error::last_os_error(),
            ));
        }
        let info = unsafe { info.assume_init() };
        Ok(VerifiedMetadata {
            metadata,
            identity: FileIdentity {
                volume: u64::from(info.dwVolumeSerialNumber),
                file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            },
        })
    }
    pub(super) fn remove(root: &Root, relative: &Path) -> io::Result<()> {
        let _serial = root
            .enumeration
            .lock()
            .map_err(|_| io::Error::other("directory enumeration state is poisoned"))?;
        let (file, _guards) = file(root, relative, true)?;
        let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        // SAFETY: owned regular non-reparse handle opened with DELETE access;
        // all directory ancestors remain pinned without write/delete sharing.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(uncertain(
                "native artifact verification failed",
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub(super) fn root(_: PathBuf, _: PathBuf) -> io::Result<Root> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn entries(_: &Root, _: &Path) -> io::Result<Vec<PathBuf>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn metadata(_: &Root, _: &Path) -> io::Result<VerifiedMetadata> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(super) fn remove(_: &Root, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod component_tests {
    use super::*;

    #[test]
    fn windows_physical_handle_containment_is_exact_case_volume_and_component_scoped() {
        let volume = r"\\?\Volume{00000000-0000-0000-0000-000000000001}\";
        let root = format!("{volume}profile\\Artifacts");
        assert!(windows_contained(
            &root,
            &format!("{root}\\child\\file"),
            false
        ));
        assert!(!windows_contained(&root, &root, false));
        assert!(windows_contained(&root, &root, true));
        for path in [
            format!("{volume}profile\\Artifacts-other\\file"),
            format!("{volume}profile\\ARTIFACTS\\file"),
            format!("{volume}outside\\file"),
            format!("{root}\\..\\victim"),
            format!("{root}\\child.\\file"),
            root.replace("000000000001", "000000000002") + "\\file",
            r"C:\profile\Artifacts\file".to_string(),
            r"\\?\UNC\server\share\profile\Artifacts\file".to_string(),
        ] {
            assert!(!windows_contained(&root, &path, false), "{path:?}");
        }
    }

    #[test]
    fn windows_component_policy_rejects_normalization_stream_and_device_aliases() {
        for name in [
            ".. ",
            "...",
            "child.",
            "child ",
            "file:stream",
            "CON",
            "nul.txt",
            "COM1.log",
            "LPT9",
            "COM¹",
            "NUL .txt",
            "CONIN$",
            "bad\nname",
        ] {
            assert!(!windows_component(name), "{name:?}");
        }
        for name in [
            "ordinary.txt",
            "ไทย-文.txt",
            ".uploads",
            "emit-x.txt",
            "COM10.log",
        ] {
            assert!(windows_component(name), "{name:?}");
        }
    }
}

#[cfg(all(windows, test))]
#[path = "artifact_delete_windows_tests.rs"]
mod windows_tests;
