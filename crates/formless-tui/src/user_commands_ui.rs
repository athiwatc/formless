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

#[derive(Default)]
pub(super) struct State {
    binding: Option<(PathBuf, PathBuf, Option<String>)>,
    checked: Option<Instant>,
    snapshot: Option<Arc<Snapshot>>,
    seen: VecDeque<String>,
}
impl App {
    pub(super) fn user_command_snapshot(&self, force: bool) -> Arc<Snapshot> {
        let binding = (
            self.paths.config_dir.clone(),
            self.work_dir(),
            self.current.clone(),
        );
        let mut state = self.user_commands.borrow_mut();
        if force
            || state.binding.as_ref() != Some(&binding)
            || state
                .checked
                .is_none_or(|checked| checked.elapsed() >= Duration::from_millis(250))
        {
            state.snapshot = Some(Arc::new(user_commands::load(
                &binding.0,
                Some(&binding.1),
                &builtin_command_names(),
            )));
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
