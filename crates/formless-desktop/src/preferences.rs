//! Explicitly saved view preferences. No Profile, Session, draft or credential data.
//! A bounded, versioned file; cooperating windows lock and compare before replacing it.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::TextScale;

const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    version: u32,
    pub text_scale: u16,
    pub dark: bool,
    pub sidebar_open: bool,
    pub inspector_open: bool,
    pub enter_sends: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            text_scale: 100,
            dark: false,
            sidebar_open: true,
            inspector_open: true,
            enter_sends: true,
        }
    }
}

impl Settings {
    pub fn for_view(
        scale: TextScale,
        dark: bool,
        sidebar_open: bool,
        inspector_open: bool,
        enter_sends: bool,
    ) -> Self {
        Self {
            version: 1,
            text_scale: scale.percent(),
            dark,
            sidebar_open,
            inspector_open,
            enter_sends,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 || !TextScale::STEPS.contains(&self.text_scale) {
            return Err("Unsupported desktop preference version or text size. File unchanged; repair or move it, then Reload desktop preferences.".into());
        }
        Ok(())
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let settings: Self = serde_json::from_slice(bytes).map_err(|_| {
            "Invalid desktop preferences. File unchanged; repair or move it, then Reload desktop preferences.".to_string()
        })?;
        settings.validate()?;
        Ok(settings)
    }
}

/// Does not create a directory, lock or preference file while reading.
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read desktop preferences. File unchanged.".into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES {
        return Err(
            "Desktop preferences must be a regular file of at most 64 KiB. File unchanged.".into(),
        );
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| "Cannot read desktop preferences. File unchanged.".to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Desktop preferences exceed 64 KiB. File unchanged.".into());
    }
    Ok(Some(bytes))
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

fn lock(path: &Path) -> Result<File, String> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    let path = PathBuf::from(name);
    let file = match private_options().create_new(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&path)
                .map_err(|_| "Cannot inspect desktop preference lock.".to_string())?
                .file_type()
                .is_file()
            {
                return Err("Desktop preference lock must be a regular file.".into());
            }
            private_options()
                .open(&path)
                .map_err(|_| "Cannot open desktop preference lock.".to_string())?
        }
        Err(_) => return Err("Cannot create desktop preference lock. Nothing saved.".into()),
    };
    file.try_lock()
        .map_err(|_| "Desktop preferences are busy or locking is unavailable. Nothing saved; try Save again later.".to_string())?;
    // Keep this inode on disk: removing a lock file would allow a second lock
    // inode while another cooperating window still holds the original.
    Ok(file)
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    basis: Option<Vec<u8>>,
    settings: Settings,
    error: Option<String>,
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            basis: None,
            settings: Settings::default(),
            error: None,
        };
        if let Err(error) = store.reload() {
            store.error = Some(error);
        }
        store
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn notice(&self) -> &str {
        self.error.as_deref().unwrap_or(if self.basis.is_none() {
            "No saved desktop preferences. Use Save to create them; Reload replaces unsaved view choices."
        } else {
            "Desktop view preferences loaded. Changes are saved only by Save desktop preferences; Reload discards unsaved view choices."
        })
    }

    pub fn reload(&mut self) -> Result<Settings, String> {
        let result = (|| {
            let bytes = read(&self.path)?;
            let settings = bytes
                .as_deref()
                .map_or(Ok(Settings::default()), Settings::decode)?;
            self.basis = bytes;
            self.settings = settings.clone();
            Ok(settings)
        })();
        self.error = result.as_ref().err().cloned();
        result
    }

    pub fn save(&mut self, settings: Settings) -> Result<(), String> {
        settings.validate()?;
        if let Some(error) = &self.error {
            return Err(format!("Not saved: {error}"));
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|_| {
            "Cannot create the desktop preference directory. Nothing saved.".to_string()
        })?;
        let _lock = lock(&self.path)?;
        if read(&self.path)? != self.basis {
            return Err("Desktop preferences changed in another window or on disk. Nothing saved. Reload explicitly before saving; Reload replaces unsaved view choices.".into());
        }
        let mut bytes =
            serde_json::to_vec_pretty(&settings).expect("fixed preference fields serialize");
        bytes.push(b'\n');
        let mut name = self.path.as_os_str().to_os_string();
        name.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let temporary = Temporary(PathBuf::from(name));
        let mut file = private_options()
            .create_new(true)
            .open(&temporary.0)
            .map_err(|_| "Cannot stage desktop preferences. Nothing saved.".to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| {
                "Cannot write desktop preferences. Original file unchanged.".to_string()
            })?;
        drop(file);
        fs::rename(&temporary.0, &self.path).map_err(|_| {
            "Cannot replace desktop preferences. Original file unchanged.".to_string()
        })?;
        self.basis = Some(bytes);
        self.settings = settings;
        Ok(())
    }
}

pub fn default_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| Path::new(p).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.filter(|path| path.is_absolute())
        .map(|base| base.join("Formless Desktop/preferences-v1.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("formless-view-prefs-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self) -> PathBuf {
            self.0.join("nested/preferences.json")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn chosen() -> Settings {
        Settings::for_view(TextScale::LARGEST, true, false, true, false)
    }

    #[test]
    fn missing_preferences_use_defaults_without_creating_any_files() {
        let directory = Directory::new();
        let store = Store::open(directory.file());
        assert_eq!(store.settings(), &Settings::default());
        assert!(store.notice().contains("No saved"));
        assert!(!directory.0.join("nested").exists());
    }

    #[test]
    fn explicit_save_round_trips_only_versioned_view_fields_and_leaves_no_temporary_files() {
        let directory = Directory::new();
        let path = directory.file();
        let mut store = Store::open(path.clone());
        store.save(chosen()).unwrap();
        assert_eq!(Store::open(path.clone()).settings(), &chosen());
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 6);
        assert_eq!(value["version"], 1);
        assert!(value.get("draft").is_none() && value.get("session_id").is_none());
        let files: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(files.len(), 2, "preference file and stable lock inode only");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn another_window_cannot_overwrite_changes_without_explicit_reload() {
        let directory = Directory::new();
        let path = directory.file();
        let mut a = Store::open(path.clone());
        let mut b = Store::open(path.clone());
        a.save(chosen()).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(b.save(Settings::default()).unwrap_err().contains("changed"));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(b.reload().unwrap(), chosen());
        b.save(Settings::default()).unwrap();
        assert!(a.save(chosen()).is_err());
        assert_eq!(Store::open(path).settings(), &Settings::default());
    }

    #[test]
    fn a_busy_lock_fails_without_waiting_or_changing_the_file() {
        let directory = Directory::new();
        let path = directory.file();
        let mut store = Store::open(path.clone());
        store.save(chosen()).unwrap();
        let bytes = fs::read(&path).unwrap();
        let held = lock(&path).unwrap();
        assert!(store
            .save(Settings::default())
            .unwrap_err()
            .contains("busy"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        drop(held);
        store.save(Settings::default()).unwrap();
    }

    #[test]
    fn malformed_future_unknown_and_oversized_files_are_preserved_and_never_echoed() {
        let directory = Directory::new();
        let path = directory.file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut future = serde_json::to_value(chosen()).unwrap();
        future["version"] = serde_json::json!(2);
        let mut unknown = serde_json::to_value(chosen()).unwrap();
        unknown["draft"] = serde_json::json!("private-canary");
        for bytes in [
            b"private-canary".to_vec(),
            serde_json::to_vec(&future).unwrap(),
            serde_json::to_vec(&unknown).unwrap(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            fs::write(&path, &bytes).unwrap();
            let mut store = Store::open(path.clone());
            assert_eq!(store.settings(), &Settings::default());
            assert!(!store.notice().contains("private-canary"));
            assert!(store.save(chosen()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn invalid_size_and_failed_reload_do_not_replace_saved_or_current_preferences() {
        let directory = Directory::new();
        let path = directory.file();
        let mut store = Store::open(path.clone());
        store.save(chosen()).unwrap();
        let bytes = fs::read(&path).unwrap();
        let mut bad = chosen();
        bad.text_scale = 201;
        assert!(store.save(bad).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::write(&path, b"broken").unwrap();
        assert!(store.reload().is_err());
        assert_eq!(store.settings(), &chosen());
        assert!(store.save(Settings::default()).is_err());
        fs::write(&path, bytes).unwrap();
        assert_eq!(store.reload().unwrap(), chosen());
        store.save(Settings::default()).unwrap();
    }

    #[test]
    fn a_non_directory_parent_and_a_directory_target_are_refused() {
        let directory = Directory::new();
        let path = directory.0.join("not-a-directory");
        fs::write(&path, b"untouched").unwrap();
        let mut store = Store::open(path.join("prefs.json"));
        assert!(store.save(chosen()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"untouched");
        let mut store = Store::open(directory.0.clone());
        assert!(store.save(chosen()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn preference_and_lock_symlinks_are_not_followed() {
        use std::os::unix::fs::symlink;
        let directory = Directory::new();
        let target = directory.0.join("target");
        fs::write(&target, b"private-canary").unwrap();
        let path = directory.0.join("prefs.json");
        symlink(&target, &path).unwrap();
        let mut store = Store::open(path.clone());
        assert!(store.save(chosen()).is_err());
        fs::remove_file(&path).unwrap();
        symlink(&target, directory.0.join("prefs.json.lock")).unwrap();
        let mut store = Store::open(path.clone());
        assert!(store.save(chosen()).is_err());
        assert!(!path.exists());
        assert_eq!(fs::read(&target).unwrap(), b"private-canary");
    }
}
