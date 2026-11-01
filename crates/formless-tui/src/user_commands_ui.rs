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
        for diagnostic in &snapshot.diagnostics {
            if state.seen.contains(&diagnostic.key) {
                continue;
            }
            self.notes.push(format!(
                "Command template {}: {}",
                display_label(&diagnostic.path.to_string_lossy()),
                display_label(&diagnostic.message)
            ));
            state.seen.push_back(diagnostic.key.clone());
            if state.seen.len() > 1024 {
                state.seen.pop_front();
            }
        }
    }
    pub(super) fn user_command_help(&mut self) -> Vec<String> {
        let snapshot = self.user_command_snapshot(true);
        self.user_commands_tick();
        let mut out = commands::help_lines();
        out.extend([String::new(), "Yours".into()]);
        if snapshot.commands.is_empty() {
            out.push("  No local templates. Add <config>/commands/NAME.md or .formless/commands/NAME.md in this Session's working directory.".into());
        }
        for command in &snapshot.commands {
            out.push(format!(
                "  /{} [ARGUMENTS]  {}",
                command.name, command.description
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
