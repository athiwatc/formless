use super::*;
use crate::questions::preview;
use crate::session_edit::{Change, Field, Metadata, Outcome, MAX_INPUT};
use crate::session_export::{self, Format};

#[derive(Clone, Copy)]
enum Waiting {
    Read(uuid::Uuid),
    Write(uuid::Uuid),
}

#[derive(Clone)]
struct PresentedSave {
    revision: uuid::Uuid,
    id: String,
    connection: u64,
    field: Field,
    toggle: Option<Change>,
}

#[derive(Clone)]
pub(super) struct ExportSavePrompt {
    token: uuid::Uuid,
    id: String,
    connection: u64,
    revision: uuid::Uuid,
    format: Format,
}

pub(super) struct Panel {
    pub export: session_export::State,
    revision: uuid::Uuid,
    model: String,
    effort: String,
    id: String,
    metadata: Option<Metadata>,
    field: Field,
    name: String,
    tags: String,
    instructions: Option<String>,
    input: Option<Entity<TextareaState>>,
    _subscription: Option<Subscription>,
    accepted: String,
    connection: u64,
    waiting: Option<Waiting>,
    retired: bool,
    notice: String,
    focus: FocusHandle,
    pub cancel: FocusHandle,
    scroll: ScrollHandle,
    controls: Vec<(FocusHandle, gpui_kit::ScrollAnchor)>,
    input_anchor: gpui_kit::ScrollAnchor,
    revealed: Option<FocusHandle>,
    viewport: Option<Size<Pixels>>,
    scale: TextScale,
    frames: u8,
}

impl Shell {
    pub(super) fn retire_session_export_attachment(&mut self) {
        if let Some(panel) = &mut self.session_editor {
            panel.export.retire(
                "Profile attachment changed. Read another export; nothing was replayed or saved.",
            );
            panel.frames = 2;
        }
    }

    pub(super) fn session_edit_target(&self) -> Option<&str> {
        self.client.as_ref()?;
        if self.destination == Destination::Sessions {
            self.directory.choice(&self.directory_rows).map(|index| {
                self.directory_rows[self.directory.matches()[index]]
                    .id
                    .as_str()
            })
        } else {
            self.sidebar.active_id()
        }
    }
    #[cfg(feature = "test-support")]
    pub fn session_edit_input(&self) -> Option<Entity<TextareaState>> {
        self.session_editor.as_ref()?.input.clone()
    }

    #[cfg(feature = "test-support")]
    pub fn session_edit_revision(&self) -> Option<uuid::Uuid> {
        Some(self.session_editor.as_ref()?.revision)
    }
    #[cfg(feature = "test-support")]
    pub fn save_presented_session_selection(
        &mut self,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.session_editor else {
            return;
        };
        assert!(matches!(p.field, Field::Model | Field::Effort));
        let presented = PresentedSave {
            revision,
            id: p.id.clone(),
            connection: p.connection,
            field: p.field,
            toggle: None,
        };
        self.save_session_edit(presented, window, cx);
    }
    /// Exercise the same callback with an action captured by an earlier render.
    /// TestWindow::click redraws first, so it cannot deliver a stale callback.
    #[cfg(feature = "test-support")]
    pub fn save_presented_session_toggle(
        &mut self,
        id: &str,
        connection: u64,
        presented: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = match presented {
            Change::Pin(_) => Field::Pin,
            Change::Archive(_) => Field::Archive,
            _ => panic!("expected a toggle"),
        };
        self.save_session_edit(
            PresentedSave {
                revision: self.session_editor.as_ref().unwrap().revision,
                id: id.into(),
                connection,
                field,
                toggle: Some(presented),
            },
            window,
            cx,
        );
    }

    pub(super) fn open_session_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_editor.is_some() || !self.client_connected {
            return;
        }
        if self.destination == Destination::Sessions
            && self.finish_directory_composition(window, cx)
        {
            return;
        }
        let Some(id) = self.session_edit_target().map(str::to_string) else {
            return;
        };
        self.invalidate_forecast(cx);
        self.palette = None;
        self.viewer = None;
        self.prefs.close_overlays();
        let scroll = ScrollHandle::new();
        let field = if self
            .client_sessions
            .iter()
            .any(|session| session.id == id && session.archived)
        {
            Field::Archive
        } else {
            Field::Name
        };
        self.session_editor = Some(Panel {
            export: session_export::State::default(),
            revision: uuid::Uuid::new_v4(),
            model: String::new(),
            effort: String::new(),
            id,
            metadata: None,
            field,
            name: String::new(),
            tags: String::new(),
            instructions: None,
            input: None,
            _subscription: None,
            accepted: String::new(),
            connection: self.client.as_ref().unwrap().connection(),
            waiting: None,
            retired: true,
            notice: String::new(),
            focus: cx.focus_handle(),
            cancel: cx.focus_handle(),
            controls: (0..14)
                .map(|_| {
                    (
                        cx.focus_handle(),
                        gpui_kit::ScrollAnchor::for_handle(scroll.clone()),
                    )
                })
                .collect(),
            input_anchor: gpui_kit::ScrollAnchor::for_handle(scroll.clone()),
            scroll,
            revealed: None,
            viewport: None,
            scale: self.text_scale,
            frames: 2,
        });
        self.reload_session_details(window, cx);
        self.settle(window, cx);
    }

    fn rebuild_session_edit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.session_editor.as_mut().unwrap();
        if panel.field == Field::Instructions && panel.instructions.is_none() {
            panel.accepted.clear();
            panel.input = None;
            panel._subscription = None;
            panel.frames = 2;
            return;
        }
        let text = match panel.field {
            Field::Model => &panel.model,
            Field::Effort => &panel.effort,
            Field::Name => &panel.name,
            Field::Tags => &panel.tags,
            Field::Instructions => panel.instructions.as_deref().unwrap_or_default(),
            Field::Pin | Field::Archive => "",
        };
        panel.accepted = text.into();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 2)
                .default_value(text.to_string())
        });
        let subscription =
            cx.observe_in(&input, window, |this, _, window, cx| {
                    let Some(panel) = this.session_editor.as_mut() else {
                        return;
                    };
                    let input = panel.input.as_ref().unwrap().clone();
                    if !input.update(cx, |input, cx| {
                        input.marked_text_range(window, cx).is_some()
                    }) {
                        let text = input.read(cx).value().to_string();
                        if text.len() > MAX_INPUT {
                            let accepted=panel.accepted.clone();
                            cx.defer_in(window,move|this,window,cx| {
                                let Some(panel)=this.session_editor.as_mut() else {return;};
                                if panel.input.as_ref()!=Some(&input) || input.update(cx,|input,cx|input.marked_text_range(window,cx).is_some()) || input.read(cx).value().len()<=MAX_INPUT {return;}
                                let focused=input.focus_handle(cx).contains_focused(window,cx);
                                match panel.field {Field::Model=>panel.model=accepted,Field::Effort=>panel.effort=accepted,Field::Name=>panel.name=accepted,Field::Tags=>panel.tags=accepted,Field::Instructions=>panel.instructions=Some(accepted),Field::Pin|Field::Archive=>{}}
                                panel.notice="Edit exceeds 16 KiB and was rejected whole. Earlier text kept; undo history cleared.".into();
                                this.rebuild_session_edit_input(window,cx);
                                if focused {this.session_editor.as_ref().unwrap().input.as_ref().unwrap().update(cx,|input,cx|input.focus(window,cx));}
                                cx.notify();
                            });
                        } else {
                            panel.accepted = text;
                        }
                    }
                    panel.frames = 2;
                    cx.notify();
            });
        panel.input = Some(input);
        panel._subscription = Some(subscription);
        panel.frames = 2;
    }

    fn keep_session_edit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let panel = self.session_editor.as_mut().unwrap();
        let Some(input) = &panel.input else {
            return true;
        };
        if input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            panel.notice = "Finish composing before changing fields or saving.".into();
            cx.notify();
            return false;
        }
        let text = input.read(cx).value().to_string();
        if text.len() > MAX_INPUT {
            panel.notice = "Edit exceeds 16 KiB; shorten or copy it before continuing.".into();
            cx.notify();
            return false;
        }
        match panel.field {
            Field::Model => panel.model = text,
            Field::Effort => panel.effort = text,
            Field::Name => panel.name = text,
            Field::Tags => panel.tags = text,
            Field::Instructions => panel.instructions = Some(text),
            Field::Pin | Field::Archive => {}
        }
        true
    }

    fn choose_session_edit_field(
        &mut self,
        field: Field,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session_editor.as_ref().unwrap().waiting.is_some()
            || !self.keep_session_edit_input(window, cx)
        {
            return;
        }
        let panel = self.session_editor.as_mut().unwrap();
        panel.field = field;
        panel.revision = uuid::Uuid::new_v4();
        // A fresh entity keeps undo history scoped to the displayed field.
        self.rebuild_session_edit_input(window, cx);
        self.session_editor
            .as_ref()
            .unwrap()
            .cancel
            .focus(window, cx);
        cx.notify();
    }

    fn reload_session_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.keep_session_edit_input(window, cx) {
            return;
        }
        let panel = self.session_editor.as_mut().unwrap();
        if panel.waiting.is_some() {
            return;
        }
        let Some(client) = &self.client else {
            return;
        };
        if !client.is_ready() {
            panel.notice = "Not connected. Your edits were kept; reload when connected.".into();
            cx.notify();
            return;
        }
        let operation = uuid::Uuid::new_v4();
        panel.connection = client.connection();
        panel.revision = uuid::Uuid::new_v4();
        panel.retired = true;
        match client.send(ClientCommand::ReadSessionDetails {
            connection: panel.connection,
            operation,
            session_id: panel.id.clone(),
        }) {
            Ok(()) => {
                panel.waiting = Some(Waiting::Read(operation));
                panel.cancel.focus(window, cx);
                panel.notice =
                    "Loading current Session metadata. Reload replaces unsaved metadata edits."
                        .into();
            }
            Err(error) => panel.notice = error.into(),
        }
        cx.notify();
    }

    fn save_session_edit(
        &mut self,
        presented: PresentedSave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.session_editor.as_mut() else {
            return;
        };
        if panel.revision != presented.revision
            || panel.id != presented.id
            || panel.connection != presented.connection
            || panel.field != presented.field
        {
            panel.notice="The displayed target or field changed before this click. Review the current details; no change was sent.".into();
            cx.notify();
            return;
        }
        if !self.keep_session_edit_input(window, cx) {
            return;
        }
        let panel = self.session_editor.as_mut().unwrap();
        let Some(client) = &self.client else {
            return;
        };
        if !self.client_connected
            || panel.retired
            || panel.waiting.is_some()
            || !client.is_ready()
            || client.connection() != panel.connection
        {
            panel.notice =
                "Not saved. Reload current details after connecting; your edits were kept.".into();
            cx.notify();
            return;
        }
        let Some(metadata) = &panel.metadata else {
            return;
        };
        let text = match panel.field {
            Field::Model => &panel.model,
            Field::Effort => &panel.effort,
            Field::Name => &panel.name,
            Field::Tags => &panel.tags,
            Field::Instructions => panel.instructions.as_deref().unwrap_or_default(),
            Field::Pin | Field::Archive => "",
        };
        let change = match crate::session_edit::Change::from_input(panel.field, text, metadata) {
            Ok(change) => change,
            Err(error) => {
                panel.notice = error.into();
                cx.notify();
                return;
            }
        };
        if (matches!(panel.field, Field::Pin | Field::Archive)
            && presented.toggle.as_ref() != Some(&change))
            || (!matches!(panel.field, Field::Pin | Field::Archive) && presented.toggle.is_some())
        {
            panel.notice="The displayed action changed before this click. Review the confirmed state; no change was sent.".into();
            cx.notify();
            return;
        }
        let operation = uuid::Uuid::new_v4();
        match client.send(ClientCommand::EditSession {
            connection: panel.connection,
            operation,
            session_id: panel.id.clone(),
            change,
        }) {
            Ok(()) => {
                panel.waiting = Some(Waiting::Write(operation));
                panel.notice =
                    "Waiting for acknowledgement. This change will not be retried automatically."
                        .into();
                panel.cancel.focus(window, cx);
            }
            Err(error) => panel.notice = format!("{error} Your edits were kept."),
        }
        cx.notify();
    }

    pub(super) fn session_details_received(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        id: &str,
        result: Result<Metadata, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .client
            .as_ref()
            .is_none_or(|client| client.connection() != connection)
        {
            return;
        }
        let Some(panel) = &mut self.session_editor else {
            return;
        };
        if panel.connection != connection
            || panel.id != id
            || !matches!(panel.waiting,Some(Waiting::Read(expected)) if expected==operation)
        {
            return;
        }
        panel.waiting = None;
        match result {
            Ok(metadata) if metadata.id == id => {
                if let Some(selection) = &metadata.selection {
                    panel.model = selection.model.clone().unwrap_or_default();
                    panel.effort = selection.reasoning_effort.clone().unwrap_or_default();
                }
                panel.name = metadata.name.clone();
                panel.tags = metadata.tags.join("\n");
                panel.instructions = metadata.instructions.text().map(str::to_string);
                panel.metadata = Some(metadata);
                panel.retired = false;
                panel.notice =
                    "Current values loaded. Each Save changes only the displayed field.".into();
                self.rebuild_session_edit_input(window, cx);
            }
            Ok(_) => {
                panel.retired = true;
                panel.notice =
                    "The details belonged to another Session; nothing was applied.".into();
            }
            Err(error) => {
                panel.retired = true;
                panel.notice = format!(
                    "Could not load details: {}. Edits kept.",
                    preview(&error, 500)
                );
            }
        }
        cx.notify();
    }

    pub(super) fn session_edit_outcome(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        id: &str,
        outcome: Outcome,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .client
            .as_ref()
            .is_none_or(|client| client.connection() != connection)
        {
            return;
        }
        let Some(panel) = &mut self.session_editor else {
            return;
        };
        if panel.connection != connection
            || panel.id != id
            || !matches!(panel.waiting,Some(Waiting::Write(expected)) if expected==operation)
        {
            return;
        }
        panel.waiting = None;
        panel.revision = uuid::Uuid::new_v4();
        let observed = matches!(&outcome, Outcome::ObservedSelection(_));
        match outcome {
            Outcome::Applied(metadata) | Outcome::ObservedSelection(metadata)
                if metadata.id == id =>
            {
                panel.metadata = Some(metadata);
                panel.notice = if observed {"The current model/effort pair already matches. No setting was written or model test started. Other unsaved fields are unchanged."} else {"Saved and acknowledged. Other unsaved fields are unchanged."}.into();
            }
            Outcome::Rejected(error) => {
                panel.notice = format!(
                    "Save error: {}. Your edits were kept.",
                    preview(&error, 500)
                )
            }
            _ => {
                panel.retired = true;
                panel.notice="Outcome unknown. It was not retried. Copy your edits, then reload current values before another save.".into();
            }
        }
        panel.frames = 2;
        cx.notify();
    }

    fn close_session_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self
            .session_editor
            .as_ref()
            .and_then(|panel| panel.input.clone())
        {
            if input.update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            }) {
                input.update(cx, |input, cx| {
                    input.unmark_text(window, cx);
                    cx.notify();
                });
                return;
            }
        }
        let relocate = self.session_editor.as_ref().is_some_and(|panel| {
            if self.destination == Destination::Sessions {
                !self.directory.contains(&self.directory_rows, &panel.id)
            } else {
                self.sidebar.active_id().is_none()
            }
        });
        if relocate {
            self.restore_focus = Some(FocusOrigin {
                handle: self
                    .browse_query(self.destination)
                    .map(|query| query.focus_handle(cx))
                    .unwrap_or_else(|| self.conversation_focus.clone()),
                region: Some(Region::Conversation),
            });
        }
        self.session_editor = None;
        self.settle(window, cx);
    }

    pub(super) fn reveal_session_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &mut self.session_editor else {
            return;
        };
        if let Some(client) = &self.client {
            if client.connection() != panel.connection {
                let unknown = matches!(panel.waiting, Some(Waiting::Write(_)));
                panel.connection = client.connection();
                panel.revision = uuid::Uuid::new_v4();
                panel
                    .export
                    .retire("Connection changed. Request another export; nothing was replayed.");
                panel.waiting = None;
                panel.retired = true;
                panel.notice=if unknown {"Connection changed; save outcome unknown. No retry. Copy edits before reloading current values."} else {"Connection changed. Edits kept; reload current values before saving."}.into();
            }
        }
        let focused = window.focused(cx);
        if panel.revealed != focused
            || panel.viewport != Some(window.viewport_size())
            || panel.scale != self.text_scale
        {
            panel.revealed = focused;
            panel.viewport = Some(window.viewport_size());
            panel.scale = self.text_scale;
            panel.frames = 2;
        }
        if panel.frames > 0 {
            panel.frames -= 1;
            let anchor = if !matches!(panel.field, Field::Pin | Field::Archive)
                && panel
                    .input
                    .as_ref()
                    .is_some_and(|input| input.focus_handle(cx).contains_focused(window, cx))
            {
                Some(&panel.input_anchor)
            } else {
                panel
                    .controls
                    .iter()
                    .find(|(focus, _)| focus.contains_focused(window, cx))
                    .map(|(_, anchor)| anchor)
            };
            if let Some(anchor) = anchor {
                anchor.scroll_to(window, cx);
                window.request_animation_frame();
            }
        }
    }

    fn scroll_session_editor(&self, lines: f32, page: bool, cx: &mut Context<Self>) {
        let Some(panel) = &self.session_editor else {
            return;
        };
        let amount = if page {
            panel.scroll.bounds().size.height.max(self.scaled(40.))
        } else {
            self.scaled(40.)
        };
        let offset = panel.scroll.offset();
        let y = (offset.y + amount * lines).clamp(-panel.scroll.max_offset().y, px(0.));
        panel.scroll.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn request_session_export(
        &mut self,
        id: &str,
        connection: u64,
        revision: uuid::Uuid,
        format: Format,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.export_save_prompt.is_some() {
            return;
        }
        let Some(panel) = &self.session_editor else {
            return;
        };
        if panel.id != id
            || panel.connection != connection
            || panel.export.revision != revision
            || panel.export.pending()
            || panel.waiting.is_some()
            || panel.retired
            || panel.metadata.is_none()
            || !self.client_connected
            || self
                .client
                .as_ref()
                .is_none_or(|client| !client.is_ready() || client.connection() != connection)
        {
            return;
        }
        if !self.keep_session_edit_input(window, cx) {
            return;
        }
        let operation = uuid::Uuid::new_v4();
        let panel = self.session_editor.as_mut().unwrap();
        panel.export.start(operation, format);
        if let Err(error) = self
            .client
            .as_ref()
            .unwrap()
            .send(ClientCommand::ExportSession {
                connection,
                operation,
                session_id: id.into(),
                format,
            })
        {
            panel.export.receive(operation, format, Err(error.into()));
        }
        panel.frames = 2;
        cx.notify();
    }

    pub(super) fn session_export_outcome(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        id: &str,
        format: Format,
        result: Result<session_export::Document, String>,
        cx: &mut Context<Self>,
    ) {
        if !self.client_connected
            || self
                .client
                .as_ref()
                .is_none_or(|client| client.connection() != connection)
        {
            return;
        }
        let Some(panel) = &mut self.session_editor else {
            return;
        };
        if panel.id == id && panel.connection == connection {
            panel.export.receive(operation, format, result);
            panel.frames = 2;
            cx.notify();
        }
    }

    /// Stale rendered buttons cannot copy a replacement export or another Session.
    fn copy_session_export(
        &self,
        id: &str,
        connection: u64,
        revision: uuid::Uuid,
        cx: &mut Context<Self>,
    ) {
        if self.export_save_prompt.is_some() {
            return;
        }
        let Some(panel) = &self.session_editor else {
            return;
        };
        if panel.id == id
            && panel.connection == connection
            && panel.export.revision == revision
            && self.client_connected
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.is_ready() && client.connection() == connection)
        {
            if let Some(document) = &panel.export.document {
                cx.write_to_clipboard(ClipboardItem::new_string(document.text.clone()));
            }
        }
    }

    fn export_save_current(&self, id: &str, connection: u64, revision: uuid::Uuid) -> bool {
        self.client_connected
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.is_ready() && client.connection() == connection)
            && self.session_editor.as_ref().is_some_and(|panel| {
                panel.id == id
                    && panel.connection == connection
                    && panel.export.revision == revision
                    && !panel.retired
                    && panel.metadata.is_some()
                    && panel.waiting.is_none()
                    && !panel.export.pending()
                    && panel.export.format().is_some()
                    && panel
                        .export
                        .document
                        .as_ref()
                        .is_some_and(|document| document.text.len() <= session_export::MAX_BYTES)
            })
    }

    fn save_session_export(
        &mut self,
        id: &str,
        connection: u64,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.export_save_prompt.is_some()
            || !self.export_save_current(id, connection, revision)
            || self.current_modal(self.layout(window)) != Some(Modal::SessionEditor)
            || !self.keep_session_edit_input(window, cx)
        {
            return;
        }
        let home = std::env::var_os(if cfg!(target_os = "windows") {
            "USERPROFILE"
        } else {
            "HOME"
        });
        let directory = home
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::current_dir().ok());
        let Some(directory) = directory else {
            self.session_editor.as_mut().unwrap().export.notice =
                "Cannot open Save dialog without an initial directory. No file was written.".into();
            cx.notify();
            return;
        };
        let panel = self.session_editor.as_mut().unwrap();
        let format = panel.export.format().unwrap();
        panel.export.revision = uuid::Uuid::new_v4();
        panel.export.notice = "Choose a NEW file for this persisted export. Private conversation/Tool data may be included. Existing files are never overwritten; cancel writes nothing.".into();
        panel.frames = 2;
        let ticket = ExportSavePrompt {
            token: uuid::Uuid::new_v4(),
            id: id.into(),
            connection,
            revision: panel.export.revision,
            format,
        };
        self.export_save_prompt = Some(ticket.clone());
        self.export_save_notice = None;
        let dialog = cx.prompt_for_new_path(&directory, Some(format.file_name()));
        cx.spawn_in(window, async move |this, cx| {
            let choice = match dialog.await {
                Ok(Ok(path)) => Ok(path),
                _ => Err("Save dialog unavailable or closed unexpectedly. No file was written."),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                this.finish_export_save(ticket, choice, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_export_save(
        &mut self,
        ticket: ExportSavePrompt,
        choice: Result<Option<std::path::PathBuf>, &'static str>,
        cx: &mut Context<Self>,
    ) {
        if self
            .export_save_prompt
            .as_ref()
            .is_none_or(|prompt| prompt.token != ticket.token)
        {
            return;
        }
        self.export_save_prompt = None;
        if !self.export_save_current(&ticket.id, ticket.connection, ticket.revision)
            || self
                .session_editor
                .as_ref()
                .and_then(|panel| panel.export.format())
                != Some(ticket.format)
        {
            self.export_save_notice = Some(
                "Export save canceled: Session, export or connection changed. No file was written."
                    .into(),
            );
            cx.notify();
            return;
        }
        let panel = self.session_editor.as_mut().unwrap();
        let document = panel.export.document.as_ref().unwrap();
        let notice = match choice {
            Ok(None) => "Export save canceled. No file was written.".to_string(),
            Err(error) => error.to_string(),
            Ok(Some(path)) => match session_export::save_new(&path, &document.text) {
                Ok(()) => format!("Saved complete {} export ({} bytes) to the chosen new file. No file was opened.", ticket.format.label(), document.text.len()),
                Err(error) => error.to_string(),
            },
        };
        panel.export.revision = uuid::Uuid::new_v4();
        panel.export.notice = notice.clone();
        panel.frames = 2;
        self.export_save_notice = Some(notice.into());
        cx.notify();
    }

    #[cfg(feature = "test-support")]
    pub fn save_presented_session_export(
        &mut self,
        id: &str,
        connection: u64,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_session_export(id, connection, revision, window, cx);
    }

    #[cfg(feature = "test-support")]
    pub fn request_presented_session_export(
        &mut self,
        id: &str,
        connection: u64,
        revision: uuid::Uuid,
        format: Format,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_session_export(id, connection, revision, format, window, cx);
    }

    #[cfg(feature = "test-support")]
    pub fn copy_presented_session_export(
        &self,
        id: &str,
        connection: u64,
        revision: uuid::Uuid,
        cx: &mut Context<Self>,
    ) {
        self.copy_session_export(id, connection, revision, cx);
    }
    #[cfg(feature = "test-support")]
    pub fn session_export_revision(&self) -> Option<uuid::Uuid> {
        self.session_editor
            .as_ref()
            .map(|panel| panel.export.revision)
    }

    pub(super) fn render_session_editor(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = self.session_editor.as_ref().unwrap();
        let viewport = window.viewport_size();
        let margin = px(16.);
        let busy = panel.waiting.is_some();
        let ready = self.client_connected
            && !busy
            && !panel.retired
            && panel.metadata.is_some()
            && (panel.field != Field::Instructions
                || panel
                    .metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.instructions.text().is_some()))
            && (!matches!(panel.field, Field::Model | Field::Effort)
                || panel
                    .metadata
                    .as_ref()
                    .is_some_and(|m| m.selection.is_some()))
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.is_ready() && client.connection() == panel.connection);
        let mut body=v_flex().id("session-edit-body").test_support().flex_1().min_h_0().overflow_y_scroll().track_scroll(&panel.scroll).gap_2()
            .child(readable("session-edit-id",format!("Session: {}",panel.id)))
            .child(readable("session-edit-help","Saves replace the current field, including changes made by another client. Cancel discards unsaved metadata edits; it does not undo a requested save. Reload also replaces these edits."));
        let mut tabs = h_flex().flex_wrap().gap_2();
        for (index, field) in [
            (0, Field::Name),
            (1, Field::Tags),
            (2, Field::Pin),
            (3, Field::Archive),
            (4, Field::Instructions),
            (12, Field::Model),
            (13, Field::Effort),
        ] {
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!(
                        "session-edit-field-anchor-{index}"
                    )))
                    .pt_1()
                    .anchor_scroll(Some(panel.controls[index].1.clone()))
                    .child(
                        Button::new(SharedString::from(format!(
                            "session-edit-{}",
                            field.label().to_lowercase()
                        )))
                        .label(field.label())
                        .selected(panel.field == field)
                        .disabled(busy || panel.metadata.is_none())
                        .track_focus(&panel.controls[index].0)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.choose_session_edit_field(field, window, cx)
                            },
                        )),
                    ),
            );
        }
        body = body.child(tabs);
        if matches!(panel.field, Field::Model | Field::Effort) {
            body=body.child(readable("session-model-help","Per-Session override for future Runs. Enter a model ID, alias or role:name; blank inherits the default. A running Run keeps its current model. This form does not validate provider/model/Role availability. Changing one field preserves the last confirmed other value; unsaved edits in that other field are not applied."))
                .child(readable("session-model-cost","Saving may start the daemon's configured paid background model test. No conversation message or Run is started by this form. No model-catalog request is made automatically."));
            if let Some(choice) = panel.metadata.as_ref().and_then(|m| m.selection.as_ref()) {
                body = body.child(readable(
                    "session-model-confirmed",
                    format!(
                        "Last confirmed model/Role: {} · effort: {}",
                        preview(
                            &crate::directory::visual_text(
                                choice.model.as_deref().unwrap_or("Inherited default")
                            ),
                            160
                        ),
                        preview(
                            &crate::directory::visual_text(
                                choice
                                    .reasoning_effort
                                    .as_deref()
                                    .unwrap_or("Inherited default")
                            ),
                            80
                        )
                    ),
                ));
            } else {
                body=body.child(readable("session-model-unavailable","This reply did not expose a complete bounded model/effort pair. Saving either field is disabled. Any draft remains available to copy; reload or use Formless TUI."));
            }
        }
        if panel.field == Field::Instructions {
            body=body.child(readable("session-edit-instructions-help","Standing instructions for this Session. Saved changes take effect at the next agent step, including during a Run. Leading/trailing whitespace is trimmed; blank clears. Save replaces the whole field."));
            if let Some(crate::session_edit::Instructions::TooLarge(bytes)) = panel
                .metadata
                .as_ref()
                .map(|metadata| &metadata.instructions)
            {
                body=body.child(readable("session-edit-instructions-unavailable",format!("The current instructions contain {bytes} bytes, above the native 16 KiB limit. They were not truncated into an edit. Use Formless TUI to inspect or replace them. Other fields remain editable; any earlier draft shown here is still copyable.")));
            }
            if panel.metadata.as_ref().is_some_and(|metadata| {
                metadata.instructions == crate::session_edit::Instructions::Unavailable
            }) {
                body=body.child(readable("session-edit-instructions-unavailable","This daemon did not expose standing instructions. Use Formless TUI to inspect or change them. Other fields remain editable; any earlier draft shown here is still copyable."));
            }
            if let Some(input) = &panel.input {
                body = body.child(
                    div()
                        .id("session-edit-input-anchor")
                        .test_support()
                        .pt_1()
                        .anchor_scroll(Some(panel.input_anchor.clone()))
                        .child(Textarea::new(input).aria_label("Session standing instructions")),
                );
            }
        } else if panel.field == Field::Pin {
            if let Some(metadata) = &panel.metadata {
                body = body.child(readable(
                    "session-edit-pin-value",
                    format!(
                        "Currently {}. Save will {} this Session.",
                        if metadata.pinned {
                            "pinned"
                        } else {
                            "unpinned"
                        },
                        if metadata.pinned { "unpin" } else { "pin" }
                    ),
                ));
            }
        } else if panel.field == Field::Archive {
            if let Some(metadata) = &panel.metadata {
                body=body.child(readable("session-edit-archive-value",if metadata.archived {"Last confirmed: archived. Restore returns this Session to the default list."} else {"Last confirmed: unarchived. Archive hides this Session from the default list and retains its history. Running work continues."}));
            }
        } else if let Some(input) = &panel.input {
            let (label,aria)=match panel.field {
                Field::Name=>("Session name (one line)","Session name"),
                Field::Model=>("Session model ID, alias or role:name (one line; blank inherits)","Session model or Role"),
                Field::Effort=>("Provider-specific reasoning effort, such as low, medium or high (one line; blank inherits). Unsupported values can fail when used.","Session reasoning effort"),
                _=>("Tags (one per line; blank clears). The daemon normalizes case, # prefixes and duplicates.","Session tags, one per line"),
            };
            body = body
                .child(readable("session-edit-field-label", label))
                .child(
                    div()
                        .id("session-edit-input-anchor")
                        .test_support()
                        .pt_1()
                        .anchor_scroll(Some(panel.input_anchor.clone()))
                        .child(Textarea::new(input).aria_label(aria)),
                );
        }
        body = body
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .id("session-edit-copy-anchor")
                            .pt_1()
                            .anchor_scroll(Some(panel.controls[5].1.clone()))
                            .child(
                                Button::new("session-edit-copy")
                                    .label("Copy edit")
                                    .disabled(
                                        panel.field == Field::Instructions && panel.input.is_none(),
                                    )
                                    .track_focus(&panel.controls[5].0)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(panel) = &this.session_editor {
                                            if panel.field == Field::Instructions
                                                && panel.input.is_none()
                                            {
                                                return;
                                            }
                                            let text = if matches!(
                                                panel.field,
                                                Field::Pin | Field::Archive
                                            ) {
                                                panel
                                                    .metadata
                                                    .as_ref()
                                                    .map(|metadata| panel.field.value(metadata))
                                                    .unwrap_or_default()
                                            } else {
                                                panel
                                                    .input
                                                    .as_ref()
                                                    .map(|input| input.read(cx).value().to_string())
                                                    .unwrap_or_default()
                                            };
                                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("session-edit-reload-anchor")
                            .pt_1()
                            .anchor_scroll(Some(panel.controls[6].1.clone()))
                            .child(
                                Button::new("session-edit-reload")
                                    .label("Reload")
                                    .disabled(
                                        busy || self
                                            .client
                                            .as_ref()
                                            .is_none_or(|client| !client.is_ready()),
                                    )
                                    .track_focus(&panel.controls[6].0)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.reload_session_details(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                announced(div().id("session-edit-notice"))
                    .test_support()
                    .role(Role::Label)
                    .aria_value(panel.notice.clone())
                    .child(panel.notice.clone()),
            );
        let can_export = self.client_connected
            && !busy
            && !panel.retired
            && panel.metadata.is_some()
            && !panel.export.pending()
            && self.export_save_prompt.is_none()
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.is_ready() && client.connection() == panel.connection);
        let mut exports = h_flex().flex_wrap().gap_2();
        for (index, format) in [Format::Json, Format::Markdown, Format::Html]
            .into_iter()
            .enumerate()
        {
            let id = panel.id.clone();
            let connection = panel.connection;
            let revision = panel.export.revision;
            exports = exports.child(
                div()
                    .id(SharedString::from(format!("session-export-anchor-{index}")))
                    .pt_1()
                    .anchor_scroll(Some(panel.controls[7 + index].1.clone()))
                    .child(
                        Button::new(SharedString::from(format!(
                            "session-export-{}",
                            format.wire()
                        )))
                        .label(format.label())
                        .accessibility_label(format!("Read Session export as {}", format.label()))
                        .disabled(!can_export)
                        .track_focus(&panel.controls[7 + index].0)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.request_session_export(
                                    &id, connection, revision, format, window, cx,
                                )
                            },
                        )),
                    ),
            );
        }
        body=body.child(readable("session-export-help","Export persisted history. JSON is the daemon's re-importable bundle; Markdown/HTML contain its transcript. Exports may include private conversation, Tool data, plans and checkpoints. Reading writes no file. Native copies are limited to 1 MiB; use Formless TUI for larger exports. HTML is displayed as source and never executed. Unsaved edits are excluded. Save export writes the complete returned text to a new file chosen in the system dialog, up to 1 MiB. Existing files are never overwritten; native import remains unavailable."))
            .child(exports)
            .child(announced(div().id("session-export-notice").test_support().role(Role::Status)
                .aria_value(panel.export.notice.clone()).child(Label::new(panel.export.notice.clone()))));
        if let Some(document) = &panel.export.document {
            body = body
                .child(readable(
                    "session-export-name",
                    format!(
                        "Exported name: {}",
                        session_export::excerpt(&document.name, 256)
                    ),
                ))
                .child(readable(
                    "session-export-preview",
                    session_export::excerpt(&document.text, 4000),
                ));
        }
        let id = panel.id.clone();
        let connection = panel.connection;
        let revision = panel.export.revision;
        body = body.child(
            div()
                .id("session-export-copy-anchor")
                .pt_1()
                .anchor_scroll(Some(panel.controls[10].1.clone()))
                .child(
                    Button::new("session-export-copy")
                        .label("Copy export")
                        .accessibility_label("Copy complete Session export to system clipboard")
                        .disabled(
                            panel.export.document.is_none()
                                || self.export_save_prompt.is_some()
                                || !self.client_connected
                                || self.client.as_ref().is_none_or(|client| {
                                    !client.is_ready() || client.connection() != panel.connection
                                }),
                        )
                        .track_focus(&panel.controls[10].0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_session_export(&id, connection, revision, cx)
                        })),
                ),
        );
        let id = panel.id.clone();
        let connection = panel.connection;
        let revision = panel.export.revision;
        body = body.child(div().id("session-export-save-anchor").pt_1()
            .anchor_scroll(Some(panel.controls[11].1.clone()))
            .child(Button::new("session-export-save-file")
                .label("Save export…")
                .accessibility_label("Save complete persisted Session export as a new file")
                .tooltip("Choose a new file in the system Save dialog. May contain private data. Existing files are never overwritten.")
                .disabled(self.export_save_prompt.is_some() || !self.export_save_current(&id, connection, revision))
                .track_focus(&panel.controls[11].0)
                .on_click(cx.listener(move |this, _, window, cx| this.save_session_export(&id, connection, revision, window, cx)))));
        let expected_toggle = panel
            .metadata
            .as_ref()
            .and_then(|metadata| match panel.field {
                Field::Pin => Some(Change::Pin(!metadata.pinned)),
                Field::Archive => Some(Change::Archive(!metadata.archived)),
                _ => None,
            });
        let save_label = match expected_toggle {
            Some(Change::Archive(true)) => "Archive",
            Some(Change::Archive(false)) => "Restore",
            _ => "Save",
        };
        let presented = PresentedSave {
            revision: panel.revision,
            id: panel.id.clone(),
            connection: panel.connection,
            field: panel.field,
            toggle: expected_toggle,
        };
        let dialog = v_flex()
            .id("session-edit-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label("Session details")
            .map(modal_dialog)
            .key_context(SESSION_EDITOR_CONTEXT)
            .on_action(cx.listener(|this, _: &CloseSessionEditor, window, cx| {
                this.close_session_editor(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerUp, _, cx| {
                this.scroll_session_editor(1., false, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerDown, _, cx| {
                this.scroll_session_editor(-1., false, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageUp, _, cx| {
                this.scroll_session_editor(1., true, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageDown, _, cx| {
                this.scroll_session_editor(-1., true, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerStart, _, cx| {
                if let Some(panel) = &this.session_editor {
                    panel.scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerEnd, _, cx| {
                if let Some(panel) = &this.session_editor {
                    panel.scroll.scroll_to_bottom();
                    cx.notify();
                }
            }))
            .w(self.scaled(640.).min(viewport.width - margin * 2.))
            .max_h(viewport.height - margin * 2.)
            .p_3()
            .gap_2()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .child(
                readable("session-edit-heading", "Session")
                    .text_lg()
                    .flex_shrink_0(),
            )
            .child(body)
            .when(matches!(panel.field,Field::Model|Field::Effort),|dialog|dialog.child(readable("session-model-paid-footer","Saving may run a paid model test.").text_sm().flex_shrink_0()))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("session-edit-cancel")
                            .label("Cancel")
                            .track_focus(&panel.cancel)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_session_editor(window, cx)
                            })),
                    )
                    .child(
                        Button::new("session-edit-save")
                            .label(save_label)
                            .accessibility_label(match save_label {
                                "Archive" => "Archive this Session",
                                "Restore" => "Restore this Session",
                                _ if panel.field == Field::Instructions => {
                                    "Save Session standing instructions"
                                }
                                _ if matches!(panel.field,Field::Model|Field::Effort) => "Save Session model and reasoning effort; configured paid model test may run",
                                _ => "Save displayed Session field",
                            })
                            .primary()
                            .disabled(!ready)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_session_edit(presented.clone(), window, cx)
                            })),
                    ),
            )
            .focus_trap("session-edit-dialog", &panel.focus);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(cx.theme().overlay)
                    .occlude(),
            )
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .justify_center()
                    .items_start()
                    .pt(margin)
                    .child(dialog),
            )
    }
}
