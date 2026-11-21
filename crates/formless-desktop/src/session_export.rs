//! Explicit, bounded copies of the daemon's persisted Session export.
//! Export replies have no Session id: ownership comes from the request envelope,
//! captured Session, connection and operation, never the displayed active Session.
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

pub const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Markdown,
    Html,
}

impl Format {
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Json => "formless-session.json",
            Self::Markdown => "formless-session.md",
            Self::Html => "formless-session.html",
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON bundle",
            Self::Markdown => "Markdown",
            Self::Html => "HTML source",
        }
    }
}

#[derive(Debug)]
pub struct Document {
    pub name: String,
    pub text: String,
}

struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("export exceeds 1 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn decode(format: Format, value: Value) -> Result<Document, String> {
    let invalid = "Malformed or unsupported export. Use Formless TUI to export this Session.";
    let name = value.get("name").and_then(Value::as_str).ok_or(invalid)?;
    if name.len() > 16 * 1024 {
        return Err(invalid.into());
    }
    let text = if format == Format::Json {
        if value["format"] != "formless.session/1"
            || !value["messages"].is_array()
            || !value["checkpoints"].is_array()
        {
            return Err(invalid.into());
        }
        // Do not allocate an unbounded pretty-printed second copy of a reply.
        let mut output = Bounded(Vec::new());
        serde_json::to_writer_pretty(&mut output, &value).map_err(|_| {
            "Export exceeds the native 1 MiB copy limit. Use Formless TUI.".to_string()
        })?;
        String::from_utf8(output.0).expect("JSON serialization is UTF-8")
    } else {
        if value["format"] != format.wire() {
            return Err(invalid.into());
        }
        let content = value["content"].as_str().ok_or(invalid)?;
        if content.len() > MAX_BYTES {
            return Err("Export exceeds the native 1 MiB copy limit. Use Formless TUI.".into());
        }
        content.to_string()
    };
    Ok(Document {
        name: name.into(),
        text,
    })
}

pub fn excerpt(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut excerpt: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        excerpt.push_str("\n… Preview truncated; Copy export copies the complete returned text.");
    }
    excerpt
}

#[derive(Debug)]
pub struct State {
    pub revision: uuid::Uuid,
    pub document: Option<Document>,
    pub notice: String,
    pending: Option<(uuid::Uuid, Format)>,
    format: Option<Format>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: uuid::Uuid::new_v4(),
            document: None,
            notice: String::new(),
            pending: None,
            format: None,
        }
    }
}

impl State {
    pub fn format(&self) -> Option<Format> {
        self.format
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn retire(&mut self, notice: &str) {
        self.revision = uuid::Uuid::new_v4();
        self.pending = None;
        self.document = None;
        self.format = None;
        self.notice = notice.into();
    }
    pub fn start(&mut self, operation: uuid::Uuid, format: Format) {
        self.retire("Reading persisted export. No file is written and nothing is copied yet.");
        self.pending = Some((operation, format));
    }
    pub fn receive(
        &mut self,
        operation: uuid::Uuid,
        format: Format,
        result: Result<Document, String>,
    ) -> bool {
        if self.pending != Some((operation, format)) {
            return false;
        }
        self.retire("");
        match result {
            Ok(document) => {
                self.notice = format!(
                    "{} · {} bytes. Preview is plain text, up to 4,000 characters. Copy export copies the complete returned text to the system clipboard. This is a past read, not a live view.",
                    format.label(), document.text.len()
                );
                self.document = Some(document);
                self.format = Some(format);
            }
            Err(error) => self.notice = crate::questions::preview(&error, 500),
        }
        true
    }
}

struct Staged(PathBuf);
impl Staged {
    fn create(path: PathBuf) -> io::Result<(Self, std::fs::File)> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok((Self(path), file))
    }
}
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Publish a complete new file without replacing any existing entry. Stage
/// and sync in the same directory, then link exclusively into the chosen name.
/// No direct-write fallback: unsupported volumes fail rather than leave a
/// partial final file. The caller must still validate its export's ownership.
pub fn save_new(path: &Path, text: &str) -> Result<(), &'static str> {
    if text.len() > MAX_BYTES {
        return Err("Export exceeds the native 1 MiB file limit. Use Formless TUI.");
    }
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("Choose an absolute path for a new export file.");
    }
    let parent = path.parent().ok_or("Choose a new export file.")?;
    let stage_path = parent.join(format!(".formless-export-{}.tmp", uuid::Uuid::new_v4()));
    let (staged, mut file) = Staged::create(stage_path).map_err(|_| {
        "Could not stage export in the selected directory. No export file was published."
    })?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);
    written.map_err(|_| "Could not finish writing export. No export file was published.")?;
    std::fs::hard_link(&staged.0, path).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            "The chosen file already exists. Nothing was overwritten; choose a new name."
        } else {
            "Could not publish export on this volume. Inspect the destination before retrying; choose a volume supporting new-file links or use Formless TUI."
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn export_copy_is_exact_unicode_and_html_is_only_source() {
        for format in [Format::Markdown, Format::Html] {
            let text = "\n<script>ไทย 😀</script>\r\n";
            let document = decode(
                format,
                json!({"name":"Session", "format":format.wire(),"content":text}),
            )
            .unwrap();
            assert_eq!(document.text, text);
            assert!(decode(
                format,
                json!({"name":"Session", "format":"json","content":text})
            )
            .is_err());
        }
        let bundle = json!({"name":"ไทย", "format":"formless.session/1","messages":[],"checkpoints":[],"future_field":{"a":1}});
        assert_eq!(
            serde_json::from_str::<Value>(&decode(Format::Json, bundle.clone()).unwrap().text)
                .unwrap(),
            bundle
        );
    }

    #[test]
    fn invalid_and_oversized_exports_are_rejected_whole() {
        for value in [
            Value::Null,
            json!({"name":"s","format":"formless.session/1"}),
            json!({"name":"s","format":"json","messages":[],"checkpoints":[]}),
        ] {
            assert!(decode(Format::Json, value).is_err());
        }
        let text = "x".repeat(MAX_BYTES);
        assert_eq!(
            decode(
                Format::Markdown,
                json!({"name":"s","format":"markdown","content":text})
            )
            .unwrap()
            .text
            .len(),
            MAX_BYTES
        );
        assert!(decode(
            Format::Html,
            json!({"name":"s","format":"html","content":"x".repeat(MAX_BYTES+1)})
        )
        .is_err());
        assert!(decode(Format::Json,json!({"name":"s","format":"formless.session/1","messages":[{"content":"x".repeat(MAX_BYTES)}],"checkpoints":[]})).is_err());
        let mut writer = Bounded(vec![0; MAX_BYTES - 1]);
        assert!(writer.write_all(&[1, 2]).is_err());
        assert_eq!(writer.0.len(), MAX_BYTES - 1);
    }

    #[test]
    fn pending_format_operation_and_retirement_own_export_results() {
        let mut state = State::default();
        assert_ne!(
            state.revision,
            State::default().revision,
            "new panels never reuse callback identities"
        );
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        state.start(a, Format::Json);
        state.start(b, Format::Html);
        assert!(!state.receive(a, Format::Json, Err("old".into())));
        assert!(!state.receive(b, Format::Json, Err("wrong format".into())));
        assert!(state.pending());
        assert!(state.receive(
            b,
            Format::Html,
            Ok(Document {
                name: "s".into(),
                text: "exact".into()
            })
        ));
        let old = state.revision;
        state.retire("Disconnected");
        assert!(state.document.is_none());
        assert_ne!(state.revision, old);
        assert!(!state.receive(b, Format::Html, Err("late".into())));
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("formless-save-export-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn new_file_is_exact_private_and_never_overwrites_an_existing_file() {
        let directory = Directory::new();
        let path = directory.0.join("export.html");
        let text = "<script>not opened ไทย 👩‍💻</script>\r\n\t  \n";
        save_new(&path, text).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        assert!(save_new(&path, "replacement")
            .unwrap_err()
            .contains("exists"));
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn size_bound_is_whole_and_bad_destinations_publish_nothing() {
        let directory = Directory::new();
        let exact = directory.0.join("exact.md");
        let text = "x".repeat(MAX_BYTES);
        save_new(&exact, &text).unwrap();
        assert_eq!(std::fs::metadata(&exact).unwrap().len(), MAX_BYTES as u64);
        let oversized = directory.0.join("oversized.md");
        assert!(save_new(&oversized, &(text + "x")).is_err());
        assert!(!oversized.exists());
        let missing_parent = directory.0.join("missing/export.json");
        assert!(save_new(&missing_parent, "content").is_err());
        assert!(!missing_parent.parent().unwrap().exists());
        assert!(save_new(Path::new("relative-export.json"), "content").is_err());
        let existing_directory = directory.0.join("directory");
        std::fs::create_dir(&existing_directory).unwrap();
        assert!(save_new(&existing_directory, "content").is_err());
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 2);
    }

    #[test]
    fn staging_collision_does_not_delete_an_unowned_file() {
        let directory = Directory::new();
        let path = directory.0.join("occupied-stage");
        std::fs::write(&path, b"another writer").unwrap();
        assert!(Staged::create(path.clone()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"another writer");
    }

    #[test]
    fn concurrent_savers_publish_one_complete_file_without_replacement() {
        let directory = Directory::new();
        let path = directory.0.join("one.md");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let texts = ["a".repeat(64 * 1024), "ไทย".repeat(8000)];
        let jobs: Vec<_> = texts
            .iter()
            .map(|text| {
                let text = text.clone();
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    save_new(&path, &text)
                })
            })
            .collect();
        let results: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        let saved = std::fs::read_to_string(path).unwrap();
        assert!(texts.contains(&saved));
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlinks_and_hardlinks_are_not_followed_or_modified() {
        use std::os::unix::fs::symlink;
        let directory = Directory::new();
        let target = directory.0.join("original");
        std::fs::write(&target, b"untouched").unwrap();
        let symbolic = directory.0.join("symbolic");
        let hard = directory.0.join("hard");
        symlink(&target, &symbolic).unwrap();
        std::fs::hard_link(&target, &hard).unwrap();
        assert!(save_new(&symbolic, "new").is_err());
        assert!(save_new(&hard, "new").is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"untouched");
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 3);
    }

    #[test]
    fn suggested_names_are_fixed_by_format_and_ready_format_retires_with_the_document() {
        for (format, name) in [
            (Format::Json, "formless-session.json"),
            (Format::Markdown, "formless-session.md"),
            (Format::Html, "formless-session.html"),
        ] {
            assert_eq!(format.file_name(), name);
            let mut state = State::default();
            let operation = uuid::Uuid::new_v4();
            state.start(operation, format);
            assert!(state.receive(
                operation,
                format,
                Ok(Document {
                    name: "../../private title".into(),
                    text: "exact".into()
                })
            ));
            assert_eq!(state.format(), Some(format));
            state.retire("changed");
            assert_eq!(state.format(), None);
        }
    }
}
