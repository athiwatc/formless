//! Skills (SPEC §7.10): `SKILL.md` packages of procedural know-how
//! (Agent Skills format: YAML frontmatter `name` + `description`, a
//! Markdown body, optional extra files). Installed versioned under
//! `<data>/skills/<name>/v<N>/`; a new install is never trusted until the
//! user enables it.

use anyhow::{anyhow, bail, Context, Result};
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

// Decode one complete quoted scalar. Literal quote fragments in the
// permissive Skill metadata format remain text instead of losing characters.
fn quoted_scalar(value: &str) -> Option<String> {
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value).ok();
    }
    let inner = value.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut chars = inner.chars();
    let mut decoded = String::new();
    while let Some(c) = chars.next() {
        if c == '\'' && chars.next() != Some('\'') {
            return None;
        }
        decoded.push(c);
    }
    Some(decoded)
}

/// Parse `SKILL.md`: `---` frontmatter with `name` and `description`
/// (plain, quoted, or folded `>`/`|` values), then the body.
pub fn parse_skill_md(text: &str) -> Result<SkillMeta> {
    let text = text.trim_start_matches('\u{feff}');
    let rest = text
        .strip_prefix("---")
        .context("SKILL.md must start with --- frontmatter")?;
    let end = rest
        .find("\n---")
        .context("SKILL.md frontmatter is not closed with ---")?;
    let (front, body) = (&rest[..end], &rest[end + 4..]);
    let mut fields = std::collections::HashMap::new();
    let lines: Vec<&str> = front.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.starts_with(char::is_whitespace)
            || line.trim().is_empty()
            || line.trim_start().starts_with('#')
        {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let mut value = crate::frontmatter::scalar_without_comment(value.trim()).to_string();
        if value.is_empty() || value == ">" || value == "|" || value == ">-" || value == "|-" {
            // Folded/literal block: the indented lines that follow.
            let mut parts = Vec::new();
            while i < lines.len()
                && (lines[i].starts_with(char::is_whitespace) || lines[i].trim().is_empty())
            {
                parts.push(lines[i].trim());
                i += 1;
            }
            value = parts.join(" ").trim().to_string();
        } else if let Some(decoded) = quoted_scalar(&value) {
            value = decoded;
        }
        fields.insert(key.trim().to_string(), value);
    }
    let name = fields.remove("name").unwrap_or_default();
    let description = fields.remove("description").unwrap_or_default();
    validate_name(&name)?;
    if crate::descriptions::screen(&description).text.is_empty() {
        bail!("SKILL.md needs a non-empty description");
    }
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
        && !name.starts_with('-');
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
        let name = entry.name().to_string();
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

fn fetch_into(staging: &Path, source: &str, git_ref: Option<&str>) -> Result<()> {
    let local = Path::new(source);
    let is_git = source.starts_with("https://")
        || source.starts_with("git@")
        || source.trim_end_matches('/').ends_with(".git");
    if is_git {
        let mut git = std::process::Command::new("git");
        git.args(["clone", "--depth", "1"]);
        if let Some(r) = git_ref {
            git.args(["--branch", r]);
        }
        git.arg(source).arg(staging);
        run(&mut git, "git clone")?;
    } else if local.is_dir() {
        copy_dir(local, staging)?;
    } else if source.to_lowercase().ends_with(".zip") && local.is_file() {
        extract_zip(local, staging)?;
    } else {
        bail!("skill source must be a folder, a .zip or a git URL: {source}");
    }
    Ok(())
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
        let mut staged = prepare(paths, &unpack.to_string_lossy(), None, None)?;
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
        let mut staged = prepare(paths, &dir.to_string_lossy(), None, None)?;
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
description: One or two sentences: what the skill does and when to use it.
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
