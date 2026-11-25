//! Skills (SPEC §7.10): `SKILL.md` packages of procedural know-how
//! (Agent Skills format: YAML frontmatter `name` + `description`, a
//! Markdown body, optional extra files). Installed versioned under
//! `<data>/skills/<name>/v<N>/`; a new install is never trusted until the
//! user enables it.

use anyhow::{anyhow, bail, Context, Result};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::{Paths, Store};

/// Bundled files beyond this total are refused.
const MAX_BYTES: u64 = 10_000_000;
/// At most this many entries in a skill archive (#856).
const MAX_ZIP_ENTRIES: usize = 2_000;

#[derive(Debug, Clone, PartialEq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub body: String,
}

#[derive(Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

#[derive(Clone, Copy)]
enum SkillField {
    Name,
    Description,
}

impl SkillField {
    fn validate(self, value: &str) -> Result<()> {
        match self {
            Self::Name => validate_name(value),
            Self::Description => {
                let chars = value.chars().count();
                if chars == 0 || crate::descriptions::screen(value).text.is_empty() {
                    bail!("description must be non-empty");
                }
                if chars > 1024 {
                    bail!("description must be at most 1024 characters");
                }
                Ok(())
            }
        }
    }
}

struct SkillFieldSeed(SkillField);

impl<'de> DeserializeSeed<'de> for SkillFieldSeed {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StringVisitor(SkillField);

        impl<'de> Visitor<'de> for StringVisitor {
            type Value = String;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a YAML string")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string::<E>(value.to_string())
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string::<E>(value.to_string())
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.0.validate(&value).map_err(E::custom)?;
                Ok(value)
            }
        }

        deserializer.deserialize_any(StringVisitor(self.0))
    }
}

struct UniqueFrontmatterKeySeed<'a>(&'a mut HashSet<String>);

impl<'de, 'a> DeserializeSeed<'de> for UniqueFrontmatterKeySeed<'a> {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KeyVisitor<'a>(&'a mut HashSet<String>);

        impl<'de, 'a> Visitor<'de> for KeyVisitor<'a> {
            type Value = String;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string frontmatter key")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string::<E>(value.to_string())
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string::<E>(value.to_string())
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if !self.0.insert(value.clone()) {
                    return Err(E::custom(format!("duplicate frontmatter key '{value}'")));
                }
                Ok(value)
            }
        }

        deserializer.deserialize_any(KeyVisitor(self.0))
    }
}

impl<'de> Deserialize<'de> for SkillFrontmatter {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldsVisitor;

        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = SkillFrontmatter;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a YAML mapping of skill frontmatter fields")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut fields = SkillFrontmatter::default();
                let mut seen = HashSet::new();
                while let Some(key) = map.next_key_seed(UniqueFrontmatterKeySeed(&mut seen))? {
                    match key.as_str() {
                        "name" => {
                            fields.name =
                                Some(map.next_value_seed(SkillFieldSeed(SkillField::Name))?)
                        }
                        "description" => {
                            fields.description =
                                Some(map.next_value_seed(SkillFieldSeed(SkillField::Description))?)
                        }
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(fields)
            }
        }

        deserializer.deserialize_map(FieldsVisitor)
    }
}

fn split_skill_md(text: &str) -> Result<(&str, &str)> {
    let text = text.trim_start_matches('\u{feff}');
    let (opening, rest) = text
        .split_once('\n')
        .context("SKILL.md must start with --- alone on line 1")?;
    if opening.strip_suffix('\r').unwrap_or(opening) != "---" {
        bail!("SKILL.md must start with --- alone on line 1");
    }
    let mut offset = 0;
    let mut closing = None;
    for line in rest.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if content == "---" {
            closing = Some((offset, line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((end, closing_len)) = closing else {
        let line = 2 + rest.split_inclusive('\n').count();
        bail!("SKILL.md frontmatter has no closing --- line (expected on line {line})");
    };
    let front = &rest[..end];
    let body = &rest[end + closing_len..];
    Ok((front, body))
}

pub(crate) fn skill_body(text: &str) -> Result<String> {
    let (_, body) = split_skill_md(text)?;
    Ok(body.trim_start_matches(['\r', '\n']).to_string())
}

/// Parse `SKILL.md`: `---` frontmatter with `name` and `description`
/// (YAML strings), then the body.
pub fn parse_skill_md(text: &str) -> Result<SkillMeta> {
    let (front, body) = split_skill_md(text)?;
    let closing_line = 2 + front.split_inclusive('\n').count();
    let fields = serde_yaml_ng::from_str::<SkillFrontmatter>(front).map_err(|error| {
        let line = error
            .location()
            .map_or(closing_line, |location| location.line() + 1);
        anyhow!("invalid SKILL.md frontmatter on line {line}: {error}")
    })?;
    let Some(name) = fields.name else {
        bail!("SKILL.md frontmatter on line {closing_line} is missing required 'name' field");
    };
    let Some(description) = fields.description else {
        bail!(
            "SKILL.md frontmatter on line {closing_line} is missing required 'description' field"
        );
    };
    Ok(SkillMeta {
        name,
        description,
        body: body.trim_start_matches(['\r', '\n']).to_string(),
    })
}

/// Agent Skills names: 1-64 lowercase letters, digits and hyphens.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if ok {
        Ok(())
    } else {
        Err(anyhow!(
            "skill names use 1-64 lowercase letters, digits and '-' (got '{name}')"
        ))
    }
}

/// Files of a skill directory, relative, sorted (no `.git`).
pub fn files(dir: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                walk(root, &path, out)?;
            } else {
                out.push(
                    path.strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// Whether a skill bundles runnable scripts (they never run on the host;
/// see SPEC §7.10).
pub fn has_scripts(files: &[String]) -> bool {
    !scripts(files).is_empty()
}

/// The bundled files that are runnable scripts.
pub fn scripts(files: &[String]) -> Vec<String> {
    const EXT: &[&str] = &[
        ".py", ".sh", ".bash", ".js", ".mjs", ".ts", ".rb", ".pl", ".ps1", ".bat", ".cmd",
    ];
    files
        .iter()
        .filter(|f| EXT.iter().any(|e| f.to_lowercase().ends_with(e)))
        .cloned()
        .collect()
}

/// What the agent is told about a skill's scripts (SPEC §7.10): they are
/// never executed on the host directly.
pub const SCRIPTS_NOTE: &str = "This skill bundles scripts. Formless never runs them on the host. \
To use one, read it with read_skill_file and port it to a Tool with create_tool \
(sandboxed, approval applies), or create a Tool that runs it through process.run \
with a process: capability the user approves.";

fn copy_dir(from: &Path, to: &Path) -> Result<u64> {
    let mut total = 0;
    std::fs::create_dir_all(to)?;
    let root = from.canonicalize()?;
    for rel in files(from)? {
        let (src, dst) = (from.join(&rel), to.join(&rel));
        // A link may point anywhere on this machine: only ones that stay
        // inside the skill are followed (#856).
        if std::fs::symlink_metadata(&src)?.file_type().is_symlink()
            && !src.canonicalize().is_ok_and(|t| t.starts_with(&root))
        {
            bail!("skill file '{rel}' is a link to outside the skill folder");
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        total += std::fs::copy(&src, &dst)?;
        if total > MAX_BYTES {
            bail!("skill is larger than {} MB", MAX_BYTES / 1_000_000);
        }
    }
    Ok(total)
}

/// How long fetching a skill (`git clone`) may take.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Run `cmd`, killing it after [`FETCH_TIMEOUT`] (#856).
fn run(cmd: &mut std::process::Command, what: &str) -> Result<()> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("run {what}"))?;
    let mut stderr = child.stderr.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > FETCH_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{what} took longer than {}s", FETCH_TIMEOUT.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let err = reader.join().unwrap_or_default();
    if !status.success() {
        bail!("{what} failed: {}", err.trim());
    }
    Ok(())
}

/// Extract a skill archive in-process (no `unzip`, so Windows works too),
/// enforcing the caps while writing: entry count, each file's size and
/// the total, counted on the bytes actually decompressed. Entries that
/// would land outside `dest` (absolute, `..`) and links are refused by
/// name (#856).
pub fn extract_zip(archive: &Path, dest: &Path) -> Result<u64> {
    use std::io::Read;
    let file =
        std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("not a readable .zip archive")?;
    if zip.len() > MAX_ZIP_ENTRIES {
        bail!(
            "the archive has {} entries; skills may have at most {MAX_ZIP_ENTRIES}",
            zip.len()
        );
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry
            .name()
            .context("zip entry name could not be decoded")?
            .into_owned();
        if entry.is_symlink() {
            bail!("zip entry '{name}' is a symbolic link; skills may not contain links");
        }
        // `enclosed_name` would quietly make "/etc/x" relative: refuse it.
        let absolute = name.starts_with(['/', '\\']) || name.as_bytes().get(1) == Some(&b':');
        let Some(rel) = entry.enclosed_name().filter(|_| !absolute) else {
            bail!("zip entry '{name}' would be written outside the skill folder");
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let left = MAX_BYTES.saturating_sub(total);
        let mut writer = std::fs::File::create(&out)?;
        // Read one byte past what is left: more means over the cap, however
        // small the archive claimed the entry was.
        let n = std::io::copy(&mut (&mut entry).take(left + 1), &mut writer)?;
        total += n;
        if total > MAX_BYTES {
            bail!(
                "the archive expands to more than {} MB (at '{name}')",
                MAX_BYTES / 1_000_000
            );
        }
    }
    Ok(total)
}

/// Fetch a source into a temporary directory: a local folder, a git URL
/// (optionally at `git_ref`), or a `.zip`. Returns the staging dir; on
/// failure nothing is left behind.
fn stage(paths: &Paths, source: &str, git_ref: Option<&str>) -> Result<PathBuf> {
    let staging = paths
        .data_dir
        .join("skills")
        .join(".staging")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&staging)?;
    match fetch_into(&staging, source, git_ref) {
        Ok(()) => Ok(staging),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

// Normalize only recognized URI schemes for Git's transport lookup. Legacy
// git@ and .git spellings still delegate validation/policy to Git itself.
fn git_source(source: &str) -> Option<std::borrow::Cow<'_, str>> {
    if let Some((scheme, remainder)) = source.split_once("://") {
        if ["http", "https", "ssh", "git", "file"]
            .iter()
            .any(|known| scheme.eq_ignore_ascii_case(known))
        {
            return Some(std::borrow::Cow::Owned(format!(
                "{}://{remainder}",
                scheme.to_ascii_lowercase()
            )));
        }
    }
    (source.starts_with("git@") || source.trim_end_matches('/').ends_with(".git"))
        .then_some(std::borrow::Cow::Borrowed(source))
}

fn fetch_into(staging: &Path, source: &str, git_ref: Option<&str>) -> Result<()> {
    let local = Path::new(source);
    if local.is_dir() {
        copy_dir(local, staging)?;
    } else if source.to_lowercase().ends_with(".zip") && local.is_file() {
        extract_zip(local, staging)?;
    } else if let Some(git_source) = git_source(source) {
        let mut git = std::process::Command::new("git");
        git.args(["clone", "--depth", "1"]);
        if let Some(r) = git_ref {
            git.args(["--branch", r]);
        }
        git.arg(git_source.as_ref()).arg(staging);
        run(&mut git, "git clone")?;
    } else {
        bail!("skill source must be a folder, a .zip or a git URL: {source}");
    }
    Ok(())
}

fn source_directory_name(
    source: &str,
    subdir: Option<&str>,
    staging: &Path,
    root: &Path,
) -> Option<String> {
    if root != staging {
        return root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    if let Some(subdir) = subdir.filter(|subdir| !subdir.is_empty() && *subdir != ".") {
        return Path::new(subdir)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    let local = Path::new(source);
    if local.is_dir() {
        return local
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    if git_source(source).is_some() {
        let path = source.split(['?', '#']).next()?.trim_end_matches('/');
        let name = path.rsplit(['/', ':']).next()?;
        let name = name.strip_suffix(".git").unwrap_or(name);
        return (!name.is_empty()).then(|| name.to_string());
    }
    None
}

/// A fetched, validated skill not yet installed (no database access, so
/// slow fetches never hold the store).
pub struct Staged {
    staging: PathBuf,
    root: PathBuf,
    pub meta: SkillMeta,
    pub files: Vec<String>,
    source: String,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.staging);
    }
}

/// Fetch and validate a skill source.
pub fn prepare(
    paths: &Paths,
    source: &str,
    subdir: Option<&str>,
    git_ref: Option<&str>,
) -> Result<Staged> {
    prepare_inner(paths, source, subdir, git_ref, true)
}

fn prepare_inner(
    paths: &Paths,
    source: &str,
    subdir: Option<&str>,
    git_ref: Option<&str>,
    validate_directory_name: bool,
) -> Result<Staged> {
    let staging = stage(paths, source, git_ref)?;
    let mut staged = Staged {
        staging: staging.clone(),
        root: staging.clone(),
        meta: SkillMeta {
            name: String::new(),
            description: String::new(),
            body: String::new(),
        },
        files: Vec::new(),
        source: source.to_string(),
    };
    if let Some(sub) = subdir.filter(|s| !s.is_empty()) {
        if sub.split(['/', '\\']).any(|p| p == "..") {
            bail!("subdir may not contain '..'");
        }
        staged.root = staged.root.join(sub);
    }
    // A zip often wraps the skill in one folder.
    if !staged.root.join("SKILL.md").is_file() {
        let dirs: Vec<PathBuf> = std::fs::read_dir(&staged.root)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("SKILL.md").is_file())
            .collect();
        if let [only] = dirs.as_slice() {
            staged.root = only.clone();
        }
    }
    let text = std::fs::read_to_string(staged.root.join("SKILL.md"))
        .context("no SKILL.md in the skill source")?;
    staged.meta = parse_skill_md(&text)?;
    if validate_directory_name {
        if let Some(directory) = source_directory_name(source, subdir, &staging, &staged.root) {
            if staged.meta.name != directory {
                bail!(
                    "skill name '{}' does not match source directory '{directory}'",
                    staged.meta.name
                );
            }
        }
    }
    staged.files = files(&staged.root)?;
    Ok(staged)
}

/// Copy a staged skill into place as its next version, recorded
/// **disabled**: the user reviews it (`GetSkill`) and enables it.
pub fn commit(
    paths: &Paths,
    store: &Store,
    staged: &Staged,
    author: &str,
) -> Result<formless_protocol::SkillInfo> {
    let meta = &staged.meta;
    let version = store.next_skill_version(&meta.name)?;
    let dest = paths
        .data_dir
        .join("skills")
        .join(&meta.name)
        .join(format!("v{version}"));
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    copy_dir(&staged.root, &dest)?;
    store.add_skill_version(
        &meta.name,
        &meta.description,
        version,
        &dest.to_string_lossy(),
        &staged.source,
        author,
        has_scripts(&staged.files),
    )
}

/// Fetch, validate and install in one go (CLI and tests).
pub fn install(
    paths: &Paths,
    store: &Store,
    source: &str,
    subdir: Option<&str>,
    git_ref: Option<&str>,
    author: &str,
) -> Result<formless_protocol::SkillInfo> {
    let staged = prepare(paths, source, subdir, git_ref)?;
    commit(paths, store, &staged, author)
}

/// Export a skill's current version as a `formless.skill/1` bundle: its
/// files only (base64), never metrics, loads or history (SPEC §7.8).
pub fn export(store: &Store, name: &str) -> Result<serde_json::Value> {
    use base64::Engine;
    let info = store
        .get_skill(name)?
        .with_context(|| format!("no skill named '{name}'"))?;
    let dir = current_dir(store, name)?;
    let mut out = Vec::new();
    for rel in files(&dir)? {
        let bytes = std::fs::read(dir.join(&rel))?;
        out.push(serde_json::json!({
            "path": rel,
            "content_b64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }));
    }
    Ok(serde_json::json!({
        "format": "formless.skill/1",
        "name": info.name,
        "description": info.description,
        "files": out,
    }))
}

/// Unpack a `formless.skill/1` bundle and validate it like any other
/// source. Paths may not be absolute or leave the skill folder.
pub fn prepare_bundle(paths: &Paths, bundle: &serde_json::Value) -> Result<Staged> {
    use base64::Engine;
    if bundle["format"].as_str() != Some("formless.skill/1") {
        bail!("not a formless.skill/1 bundle");
    }
    let unpack = paths
        .data_dir
        .join("skills")
        .join(".staging")
        .join(uuid::Uuid::new_v4().to_string());
    let result = (|| {
        let mut total = 0u64;
        for f in bundle["files"].as_array().into_iter().flatten() {
            let rel = f["path"].as_str().context("bundle file without a path")?;
            let rel_path = Path::new(rel);
            if rel_path.is_absolute()
                || rel_path
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                bail!("bundle path '{rel}' leaves the skill folder");
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(f["content_b64"].as_str().unwrap_or_default())
                .with_context(|| format!("bad content for '{rel}'"))?;
            total += bytes.len() as u64;
            if total > MAX_BYTES {
                bail!("skill is larger than {} MB", MAX_BYTES / 1_000_000);
            }
            let dst = unpack.join(rel_path);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(dst, bytes)?;
        }
        let mut staged = prepare_inner(paths, &unpack.to_string_lossy(), None, None, false)?;
        staged.source = "import".into();
        Ok(staged)
    })();
    let _ = std::fs::remove_dir_all(&unpack);
    result
}

/// Install a skill from SKILL.md text alone (auto-created skills).
pub fn install_text(
    paths: &Paths,
    store: &Store,
    skill_md: &str,
    source: &str,
    author: &str,
) -> Result<formless_protocol::SkillInfo> {
    let dir = paths
        .data_dir
        .join("skills")
        .join(".staging")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)?;
    let result = (|| {
        std::fs::write(dir.join("SKILL.md"), skill_md)?;
        let mut staged = prepare_inner(paths, &dir.to_string_lossy(), None, None, false)?;
        staged.source = source.into();
        commit(paths, store, &staged, author)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// System prompt for distilling a skill from a successful Run.
pub const DISTIL_PROMPT: &str = "You turn a successfully completed agent task into a reusable \
skill: a SKILL.md that helps an agent do the same kind of task well next time.

Reply with the SKILL.md only, in this exact form:
---
name: lowercase-words-with-hyphens
description: \"One or two sentences: what the skill does and when to use it.\"
---
# Title
## When to use
## Steps
## Pitfalls
## Tools

Rules:
- Generic: describe the kind of task, not this instance. No names, paths, \
URLs, numbers, file contents or other user data from the transcript.
- Never include secrets, tokens, passwords or keys.
- Mention the Tools by name where they help.
- If an existing skill listed below already covers this, reuse its exact \
name to improve it; if the task is trivial or not reusable, reply NONE.";

/// The user message for [`DISTIL_PROMPT`]: the task, the Tool calls and
/// the outcome, each clipped, plus the existing skills for dedup.
pub fn distil_input(
    task: &str,
    calls: &[(String, String, String)],
    outcome: &str,
    existing: &[formless_protocol::SkillInfo],
) -> String {
    fn clip(s: &str, n: usize) -> String {
        let mut out: String = s.chars().take(n).collect();
        if out.len() < s.len() {
            out.push('…');
        }
        out
    }
    let mut text = format!("Task:\n{}\n\nTool calls:\n", clip(task, 2000));
    for (i, (tool, input, status)) in calls.iter().enumerate().take(60) {
        text.push_str(&format!(
            "{}. {tool} [{status}] {}\n",
            i + 1,
            clip(input, 300)
        ));
    }
    text.push_str(&format!(
        "\nOutcome:\n{}\n\nExisting skills:\n",
        clip(outcome, 2000)
    ));
    if existing.is_empty() {
        text.push_str("(none)\n");
    }
    for s in existing {
        text.push_str(&format!(
            "- {}: {}\n",
            s.name,
            crate::descriptions::model_text(&s.description)
        ));
    }
    text
}

/// The SKILL.md in a distillation reply, or `None` for NONE or anything
/// that does not parse as a valid skill.
pub fn parse_distilled(reply: &str) -> Option<(SkillMeta, String)> {
    let mut text = reply.trim();
    if let Some(rest) = text.strip_prefix("```") {
        // Drop a code fence (and its language tag) around the reply.
        text = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
        text = text.trim_end().trim_end_matches("```").trim();
    }
    if text.eq_ignore_ascii_case("none") || !text.starts_with("---") {
        return None;
    }
    let meta = parse_skill_md(text).ok()?;
    (!meta.body.trim().is_empty()).then(|| (meta, format!("{text}\n")))
}

/// The installed directory of a skill's current version.
pub fn current_dir(store: &Store, name: &str) -> Result<PathBuf> {
    store
        .skill_path(name)?
        .map(PathBuf::from)
        .with_context(|| format!("no skill named '{name}'"))
}

/// Read a file of a skill, refusing paths that leave its folder.
pub fn read_file(store: &Store, name: &str, rel: &str) -> Result<String> {
    let dir = current_dir(store, name)?.canonicalize()?;
    let path = dir
        .join(rel)
        .canonicalize()
        .with_context(|| format!("no file '{rel}' in skill '{name}'"))?;
    if !path.starts_with(&dir) {
        bail!("'{rel}' is outside the skill");
    }
    Ok(String::from_utf8_lossy(&std::fs::read(path)?).into_owned())
}

#[cfg(test)]
mod source_tests {
    use super::git_source;

    #[test]
    fn recognized_git_uri_schemes_normalize_only_the_scheme() {
        for (source, expected) in [
            (
                "http://Case.Example/Team/Skill",
                "http://Case.Example/Team/Skill",
            ),
            (
                "HTTPS://Case.Example/Team/Skill",
                "https://Case.Example/Team/Skill",
            ),
            (
                "HtTpS://Case.Example/Team/Skill%2FPart?Token=AbC#Frag",
                "https://Case.Example/Team/Skill%2FPart?Token=AbC#Frag",
            ),
            (
                "ssh://GitUser@Case.Example/Team/Skill",
                "ssh://GitUser@Case.Example/Team/Skill",
            ),
            (
                "SSH://GitUser@Case.Example/Team/Skill",
                "ssh://GitUser@Case.Example/Team/Skill",
            ),
            (
                "GiT://Case.Example/Team/Skill",
                "git://Case.Example/Team/Skill",
            ),
            (
                "FILE:///Owned/Case/Repo.git/",
                "file:///Owned/Case/Repo.git/",
            ),
            ("git@Case.Example:Team/Skill", "git@Case.Example:Team/Skill"),
            ("host:Team/Repo.git", "host:Team/Repo.git"),
        ] {
            assert_eq!(git_source(source).as_deref(), Some(expected), "{source}");
        }
        for source in [
            "notes.txt",
            "httpsx://example.test/Skill",
            "ws://example.test/Skill",
            "",
        ] {
            assert!(git_source(source).is_none(), "{source}");
        }
    }
}
