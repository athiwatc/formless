//! Prompt templates are local text intake, never a second command dispatcher.
use super::*;
use formless_core::user_commands::{self, Snapshot};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

fn display_label(value: &str) -> String {
    let controls: String = value
        .chars()
        .map(|c| {
            if c.is_control() {
                format!("⟨U+{:04X}⟩", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect();
    text::escape_bidi(&controls).into_owned()
}

pub(super) fn diagnostic_notes(snapshot: &Snapshot, seen: &mut VecDeque<String>) -> Vec<String> {
    // A repaired (or removed) file can produce a fresh note if it breaks again.
    seen.retain(|key| {
        snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| &diagnostic.key == key)
    });
    let mut notes = Vec::new();
    for diagnostic in &snapshot.diagnostics {
        if !seen.contains(&diagnostic.key) {
            notes.push(format!(
                "Command template {}: {}",
                display_label(&diagnostic.path.to_string_lossy()),
                display_label(&diagnostic.message)
            ));
            seen.push_back(diagnostic.key.clone());
        }
    }
    notes
}

pub(super) fn help_lines(snapshot: &Snapshot) -> Vec<String> {
    let mut out = vec![String::new(), "Yours".into()];
    if snapshot.commands.is_empty() {
        out.push("  No local templates. Add <config>/commands/NAME.md or .formless/commands/NAME.md in this Session's working directory.".into());
    }
    for command in &snapshot.commands {
        out.push(format!(
            "  /{} [ARGUMENTS]  {}",
            command.name,
            display_label(&command.description)
        ));
        out.push(format!(
            "    {}",
            display_label(&command.path.to_string_lossy())
        ));
    }
    for shadowed in &snapshot.shadowed {
        out.push(format!(
            "  Shadowed /{} — {}: {}",
            shadowed.name,
            display_label(&shadowed.path.to_string_lossy()),
            display_label(&shadowed.reason)
        ));
    }
    out
}

/// On-demand diagnostics remain useful after a background note was dismissed.
pub(super) fn unavailable(snapshot: &Snapshot, name: &str) -> Option<String> {
    let diagnostics: Vec<_> = snapshot
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.path.extension() == Some(std::ffi::OsStr::new("md"))
                && diagnostic
                    .path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.eq_ignore_ascii_case(name))
        })
        .map(|diagnostic| {
            format!(
                "{}: {}",
                display_label(&diagnostic.path.to_string_lossy()),
                display_label(&diagnostic.message)
            )
        })
        .collect();
    (!diagnostics.is_empty()).then(|| {
        format!(
            "/{} isn't loaded: {}",
            display_label(name),
            diagnostics.join("; ")
        )
    })
}

pub(super) fn suggestion(snapshot: &Snapshot, typed: &str) -> Option<String> {
    let names = commands::COMMANDS
        .iter()
        .map(|command| command.name.split(' ').next().unwrap_or(command.name))
        .chain(
            snapshot
                .commands
                .iter()
                .map(|command| command.name.as_str()),
        )
        .chain(
            snapshot
                .shadowed
                .iter()
                .map(|command| command.name.as_str()),
        );
    let name = commands::closest_in(typed, names)?;
    let mut text = format!("did you mean /{name}?");
    for shadowed in snapshot
        .shadowed
        .iter()
        .filter(|command| command.name == name)
    {
        text.push_str(&format!(
            " Shadowed template {}: {}.",
            display_label(&shadowed.path.to_string_lossy()),
            display_label(&shadowed.reason)
        ));
    }
    Some(text)
}

#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: std::time::SystemTime,
    kind: [bool; 3],
    #[cfg(unix)]
    identity: (u64, u64, u64, i64, i64),
}
impl TryFrom<std::fs::Metadata> for Stamp {
    type Error = std::io::Error;
    fn try_from(metadata: std::fs::Metadata) -> Result<Self, Self::Error> {
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            kind: [
                metadata.is_dir(),
                metadata.is_file(),
                metadata.file_type().is_symlink(),
            ],
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.nlink(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        })
    }
}
type Fingerprint = Vec<(PathBuf, Option<Stamp>)>;

/// A bounded preview-cache hint: inspect names/metadata, never template bytes.
/// Explicit help/expansion always load through Core, regardless of this hint.
fn fingerprint(config: &std::path::Path, project: &std::path::Path) -> Option<Fingerprint> {
    let mut out = Vec::new();
    for (root, is_project) in [(config, false), (project, true)] {
        // Configured roots may be trusted aliases, as in Core's loader.
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                out.push((root.to_path_buf(), None));
                continue;
            }
            Err(_) => return None,
        };
        let mut dir = root;
        let parts: &[&str] = if is_project {
            &[".formless", "commands"]
        } else {
            &["commands"]
        };
        for (index, part) in parts.iter().enumerate() {
            dir.push(part);
            let metadata = match std::fs::symlink_metadata(&dir) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    out.push((dir.clone(), None));
                    break;
                }
                Err(_) => return None,
            };
            // Do not enumerate a known symlink or non-directory descendant.
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                out.push((dir.clone(), Some(metadata.try_into().ok()?)));
                break;
            }
            if index + 1 != parts.len() {
                continue;
            }
            out.push((dir.clone(), Some(metadata.try_into().ok()?)));
            for (count, entry) in std::fs::read_dir(&dir).ok()?.enumerate() {
                // Match Core's directory-entry bound; never scan unboundedly.
                if count >= 256 {
                    return None;
                }
                let path = entry.ok()?.path();
                if path.extension() == Some(std::ffi::OsStr::new("md")) {
                    out.push((
                        path.clone(),
                        Some(std::fs::symlink_metadata(path).ok()?.try_into().ok()?),
                    ));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

#[derive(Default)]
pub(super) struct State {
    binding: Option<(PathBuf, PathBuf, Option<String>)>,
    checked: Option<Instant>,
    snapshot: Option<Arc<Snapshot>>,
    seen: VecDeque<String>,
    fingerprint: Option<Fingerprint>,
    #[cfg(test)]
    loads: usize,
    #[cfg(test)]
    probes: usize,
}
impl App {
    pub(super) fn user_command_snapshot(&self, force: bool) -> Arc<Snapshot> {
        let binding = (
            self.paths.config_dir.clone(),
            self.work_dir(),
            self.current.clone(),
        );
        let mut state = self.user_commands.borrow_mut();
        let binding_changed = state.binding.as_ref() != Some(&binding);
        let palette = self.view == View::Chat
            && self.input.strip_prefix('/').is_some_and(|query| {
                !query.starts_with('/') && !query.chars().any(char::is_whitespace)
            })
            && !self.input.is_literal()
            && self.palette_hidden.as_deref() != Some(&*self.input)
            && !self.search_mode
            && self.renaming.is_none()
            && self.instructing.is_none()
            && self.import_mode.is_none()
            && self.bulk.is_none()
            && !self.editing_step;
        let due = state
            .checked
            .is_none_or(|checked| checked.elapsed() >= Duration::from_millis(250));
        if force || binding_changed || palette && due {
            #[cfg(test)]
            {
                state.probes += 1;
            }
            // Capture before loading: a concurrent edit must not stamp an old
            // snapshot with newer metadata and hide it from the next poll.
            let next = fingerprint(&binding.0, &binding.1);
            if force || binding_changed || next.is_none() || state.fingerprint != next {
                #[cfg(test)]
                {
                    state.loads += 1;
                }
                state.snapshot = Some(Arc::new(user_commands::load(
                    &binding.0,
                    Some(&binding.1),
                    &builtin_command_names(),
                )));
            }
            state.fingerprint = next;
            state.binding = Some(binding);
            state.checked = Some(Instant::now());
        }
        state.snapshot.as_ref().unwrap().clone()
    }
    pub(super) fn user_commands_tick(&mut self) {
        let snapshot = self.user_command_snapshot(false);
        let mut state = self.user_commands.borrow_mut();
        self.notes
            .extend(diagnostic_notes(&snapshot, &mut state.seen));
    }
    pub(super) fn user_command_help(&mut self) -> Vec<String> {
        let snapshot = self.user_command_snapshot(true);
        self.user_commands_tick();
        let mut out = commands::help_lines();
        out.extend(help_lines(&snapshot));
        out
    }
    /// Runs before forecast/attachment intake and only in a message composer.
    /// The literal marker survives confirmations, reconnects and Session drafts.
    pub(super) fn expand_user_command(&mut self) -> bool {
        if self.input.is_literal()
            || self.search_mode
            || self.renaming.is_some()
            || self.instructing.is_some()
            || self.import_mode.is_some()
            || self.bulk.is_some()
            || self.editing_step
        {
            return true;
        }
        let text = self.input.expanded();
        let Some(invocation) = text.strip_prefix('/') else {
            return true;
        };
        let end = invocation
            .find(char::is_whitespace)
            .unwrap_or(invocation.len());
        let name = &invocation[..end];
        if builtin_command_names()
            .iter()
            .any(|builtin| builtin == name)
        {
            return true;
        }
        let snapshot = self.user_command_snapshot(true);
        self.user_commands_tick();
        let Some(command) = snapshot.find(name) else {
            if let Some(message) = unavailable(&snapshot, name) {
                self.notes.push(message);
                return false;
            }
            return true;
        };
        // Remove the invocation's one separator; preserve the remainder's exact
        // whitespace for $ARGUMENTS. Substitution is owned by the shared Core.
        let args = invocation[end..]
            .chars()
            .next()
            .map_or("", |separator| &invocation[end + separator.len_utf8()..]);
        match command.expand(args) {
            Ok(expanded) => {
                self.input.set(&expanded);
                self.input.set_literal(true);
                self.vim_input.reset();
                self.palette_hidden = None;
                self.forecast.revision = self.forecast.revision.wrapping_add(1);
                true
            }
            Err(error) => {
                self.notes.push(format!("/{}: {error}", command.name));
                false
            }
        }
    }
    pub(super) fn user_command_palette(&self, query: &str) -> Vec<PaletteItem> {
        if query.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        let snapshot = self.user_command_snapshot(false);
        let mut rows: Vec<_> = snapshot
            .commands
            .iter()
            .filter_map(|command| {
                formless_core::fuzzy::fuzzy_match(query, &command.name)
                    .map(|(score, _)| (score, command))
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        rows.into_iter()
            .map(|(_, command)| PaletteItem {
                insert: format!("/{} ", command.name),
                label: format!("/{} [ARGUMENTS]", command.name),
                desc: command.description.clone(),
                runnable: true,
            })
            .collect()
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    struct Fixture {
        app: App,
        root: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn fixture() -> Fixture {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fl-refresh-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut app = crate::tests::test_app();
        app.paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(project.join(".formless/commands")).unwrap();
        std::fs::create_dir_all(app.paths.config_dir.join("commands")).unwrap();
        app.current = Some("refresh".into());
        app.sessions = vec![serde_json::from_value(serde_json::json!({"id":"refresh", "name":"Refresh", "created_at":"", "updated_at":"", "cwd":project})).unwrap()];
        std::fs::write(
            project.join(".formless/commands/workflow.md"),
            "---\ndescription: Old workflow\n---\nOld workflow",
        )
        .unwrap();
        Fixture { app, root }
    }
    fn expire(app: &App) {
        app.user_commands.borrow_mut().checked = Some(Instant::now() - Duration::from_millis(251));
    }
    #[test]
    fn unchanged_idle_templates_are_loaded_once_not_every_tick() {
        let mut fixture = fixture();
        for i in 0..64 {
            std::fs::write(
                fixture
                    .app
                    .paths
                    .config_dir
                    .join(format!("commands/template{i}.md")),
                format!("Template fixture\n{}", "x".repeat(8192)),
            )
            .unwrap();
        }
        fixture.app.user_commands_tick();
        assert_eq!(fixture.app.user_commands.borrow().loads, 1);
        for _ in 0..100 {
            expire(&fixture.app);
            fixture.app.user_commands_tick();
        }
        assert_eq!(
            fixture.app.user_commands.borrow().loads,
            1,
            "ordinary idle frames must not read template bodies again"
        );
        assert_eq!(
            fixture.app.user_commands.borrow().probes,
            1,
            "ordinary idle frames must not probe template metadata either"
        );
        fixture.app.input.set("/template");
        for _ in 0..100 {
            expire(&fixture.app);
            fixture.app.user_commands_tick();
        }
        assert_eq!(
            fixture.app.user_commands.borrow().loads,
            1,
            "unchanged palette polls must not read template bodies"
        );
        assert_eq!(fixture.app.user_commands.borrow().probes, 101);
        fixture.app.palette_hidden = Some(fixture.app.input.to_string());
        expire(&fixture.app);
        fixture.app.user_commands_tick();
        assert_eq!(
            fixture.app.user_commands.borrow().probes,
            101,
            "dismissed palettes must not keep polling"
        );
    }
    #[test]
    fn palette_polls_notice_edits_additions_and_removals_on_the_next_refresh() {
        let mut fixture = fixture();
        fixture.app.input.set("/work");
        assert!(fixture.app.user_command_palette("work")[0]
            .desc
            .contains("Old workflow"));
        let path = fixture.root.join("project/.formless/commands/workflow.md");
        std::fs::write(
            &path,
            "---\ndescription: Updated workflow\n---\nNew body with $ARGUMENTS",
        )
        .unwrap();
        expire(&fixture.app);
        assert_eq!(
            fixture.app.user_command_palette("work")[0].desc,
            "Updated workflow"
        );
        assert_eq!(fixture.app.user_commands.borrow().loads, 2);
        let added = path.with_file_name("worker.md");
        std::fs::write(&added, "Added worker").unwrap();
        expire(&fixture.app);
        assert_eq!(fixture.app.user_command_palette("work").len(), 2);
        std::fs::remove_file(&path).unwrap();
        expire(&fixture.app);
        let rows = fixture.app.user_command_palette("work");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].insert, "/worker ");
    }
    #[test]
    fn help_expansion_and_binding_changes_do_not_depend_on_palette_cache_hints() {
        let mut fixture = fixture();
        fixture.app.user_commands_tick();
        let path = fixture.root.join("project/.formless/commands/workflow.md");
        std::fs::write(
            &path,
            "---\ndescription: Help sees this\n---\nFresh $ARGUMENTS",
        )
        .unwrap();
        assert!(fixture
            .app
            .user_command_help()
            .iter()
            .any(|line| line.contains("Help sees this")));
        std::fs::write(&path, "Newest $ARGUMENTS").unwrap();
        fixture.app.input.set("/workflow exact");
        assert!(fixture.app.expand_user_command());
        assert_eq!(fixture.app.input.expanded(), "Newest exact");
        // A changed Session/project binding loads once even without a slash.
        let other = fixture.root.join("other");
        std::fs::create_dir_all(other.join(".formless/commands")).unwrap();
        std::fs::write(
            other.join(".formless/commands/workflow.md"),
            "Other project",
        )
        .unwrap();
        fixture.app.sessions[0].cwd = Some(other.to_string_lossy().into_owned());
        fixture.app.input.clear();
        let loads = fixture.app.user_commands.borrow().loads;
        fixture.app.user_commands_tick();
        assert_eq!(fixture.app.user_commands.borrow().loads, loads + 1);
        assert_eq!(
            fixture
                .app
                .user_command_snapshot(false)
                .find("workflow")
                .unwrap()
                .expand("")
                .unwrap(),
            "Other project"
        );
    }
    #[test]
    fn missing_template_argument_keeps_the_composer_and_stops_before_send() {
        let mut fixture = fixture();
        let path = fixture.root.join("project/.formless/commands/workflow.md");
        std::fs::write(&path, "$1 then $2").unwrap();
        let (client, sent) = crate::client::DaemonClient::recording();
        fixture.app.client = Some(client);
        fixture.app.input.set("/workflow private-argument");
        fixture.app.submit_chat(false);
        assert_eq!(fixture.app.input.expanded(), "/workflow private-argument");
        assert!(!fixture.app.input.is_literal());
        assert!(
            sent.try_recv().is_err(),
            "expansion failure sends no daemon request"
        );
        let note = fixture.app.notes.last().unwrap();
        assert!(
            note.contains("workflow.md")
                && note.contains("references $2")
                && note.contains("write $$2"),
            "{note}"
        );
        assert!(!note.contains("private-argument"), "{note}");
        std::fs::write(&path, "Cost $$5 and $$9.99; explain `echo $$1`").unwrap();
        fixture.app.input.set("/workflow");
        assert!(fixture.app.expand_user_command());
        assert_eq!(
            fixture.app.input.expanded(),
            "Cost $5 and $9.99; explain `echo $1`"
        );
        assert!(fixture.app.input.is_literal());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_expansion_rechecks_core_file_guards_instead_of_using_cached_text() {
        let mut fixture = fixture();
        fixture.app.user_commands_tick();
        let outside = fixture.root.join("outside.md");
        std::fs::write(&outside, "Never loaded through a descendant link").unwrap();
        let path = fixture.root.join("project/.formless/commands/workflow.md");
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        fixture.app.input.set("/workflow");
        assert!(!fixture.app.expand_user_command());
        assert_eq!(fixture.app.input.expanded(), "/workflow");
        assert!(fixture
            .app
            .notes
            .iter()
            .any(|note| note.contains("isn't loaded")));
        assert_eq!(
            std::fs::read_to_string(outside).unwrap(),
            "Never loaded through a descendant link"
        );
    }
}
