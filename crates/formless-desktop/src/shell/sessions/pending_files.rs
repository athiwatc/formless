use super::*;
use crate::pending_files::{Report, PAGE_SIZE};
mod removal;
const BASE: usize = 17 + crate::model_catalog::PAGE_SIZE;
#[derive(Default)]
pub(super) struct State {
    pending: Option<uuid::Uuid>,
    revision: Option<uuid::Uuid>,
    report: Option<Report>,
    notice: String,
    page: usize,
    armed: Option<Review>,
    observed: crate::reference_detach::Status,
}
#[derive(Clone)]
struct Review {
    action: crate::reference_detach::Action,
    text: SharedString,
}
impl std::ops::Deref for Review {
    type Target = crate::reference_detach::Action;
    fn deref(&self) -> &Self::Target {
        &self.action
    }
}
#[derive(Clone, Copy)]
struct Owner {
    instance: uuid::Uuid,
    connection: u64,
    form: uuid::Uuid,
}
impl Owner {
    fn of(p: &Panel) -> Self {
        Self {
            instance: p.instance,
            connection: p.connection,
            form: p.revision,
        }
    }
}
impl Shell {
    fn pending_files_current(&self, owner: Owner) -> bool {
        self.client_connected
            && self.session_editor.as_ref().is_some_and(|p| {
                p.instance == owner.instance
                    && p.connection == owner.connection
                    && p.revision == owner.form
                    && !p.retired
                    && p.waiting.is_none()
                    && p.metadata.is_some()
                    && self.client.as_ref().is_some_and(|c| {
                        c.is_ready()
                            && c.instance() == owner.instance
                            && c.connection() == owner.connection
                    })
            })
    }
    fn read_pending_files(&mut self, owner: Owner, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_files_current(owner)
            || self.current_modal(self.layout(window)) != Some(Modal::SessionEditor)
            || !self.keep_session_edit_input(window, cx)
        {
            return;
        }
        if self
            .client
            .as_ref()
            .is_some_and(|c| c.reference_status().busy())
        {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        if p.pending_files.pending.is_some() {
            return;
        }
        let operation = uuid::Uuid::new_v4();
        match self
            .client
            .as_ref()
            .unwrap()
            .send(ClientCommand::ReadPendingAttachments {
                connection: p.connection,
                operation,
                session_id: p.id.clone(),
            }) {
            Ok(()) => {
                p.pending_files = State {
                    pending: Some(operation),
                    notice:
                        "Reading stored pending-file metadata; attachment paths are not opened."
                            .into(),
                    ..Default::default()
                }
            }
            Err(_) => {
                p.pending_files.notice =
                    "Pending files were not requested. Connect and reload Session details first."
                        .into()
            }
        }
        p.frames = 2;
        cx.notify();
    }
    pub(in crate::shell) fn pending_files_received(
        &mut self,
        instance: uuid::Uuid,
        connection: u64,
        operation: uuid::Uuid,
        id: &str,
        result: Result<Report, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.session_editor.as_mut() else {
            return;
        };
        if p.instance != instance
            || p.connection != connection
            || p.id != id
            || p.pending_files.pending != Some(operation)
            || self.client.as_ref().is_none_or(|c| {
                !c.is_ready() || c.instance() != instance || c.connection() != connection
            })
        {
            return;
        }
        p.pending_files.pending = None;
        p.pending_files.revision = Some(operation);
        match result {
            Ok(report) if report.session_id() == id => {
                p.pending_files.report = Some(report);
                p.pending_files.notice="Complete metadata snapshot read. Other clients or a message may change/consume these files; read again explicitly. This view does not authorize attachment changes or sending.".into();
            }
            Ok(_) | Err(_) => {
                p.pending_files.report = None;
                p.pending_files.notice="Complete pending-file inventory unavailable, unsupported or above bounds. Use Formless TUI or update the daemon for pending-file inventory support. No partial list is shown.".into();
            }
        }
        p.frames = 2;
        cx.notify();
    }
    fn pending_files_page(
        &mut self,
        owner: Owner,
        revision: uuid::Uuid,
        page: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.pending_files_current(owner) {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        if p.pending_files.revision != Some(revision) {
            return;
        }
        if let Some(r) = &p.pending_files.report {
            p.pending_files.armed = None;
            p.pending_files.page = page.min(r.rows.len().saturating_sub(1) / PAGE_SIZE);
            p.frames = 2;
            cx.notify();
        }
    }
    fn copy_pending_files(&self, owner: Owner, revision: uuid::Uuid, cx: &mut Context<Self>) {
        if !self.pending_files_current(owner) {
            return;
        }
        let p = self.session_editor.as_ref().unwrap();
        if p.pending_files.revision != Some(revision) || p.pending_files.pending.is_some() {
            return;
        }
        if let Some(r) = &p.pending_files.report {
            cx.write_to_clipboard(ClipboardItem::new_string(r.copy()));
        }
    }
    pub(super) fn render_pending_files(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.session_editor.as_ref().unwrap();
        let owner = Owner::of(p);
        let ready = self.pending_files_current(owner);
        let mut body=v_flex().gap_2().child(readable("pending-files-help","Pending files belong to this Session's next user message and are shared across clients. Read a metadata snapshot; attachment paths are not opened, and this does not attach/remove files or start a Run. Metadata includes private daemon paths. References can be reviewed for removal below. Stored files and attachment additions require Formless TUI. Reading/copying does not remove anything and is not a recovery guarantee."))
            .child(div().id("pending-files-read-anchor").pt_1().anchor_scroll(Some(p.controls[BASE].1.clone())).child(Button::new("pending-files-read").label("Read pending files").disabled(!ready||p.pending_files.pending.is_some()||self.client.as_ref().is_some_and(|c|c.reference_status().busy())).track_focus(&p.controls[BASE].0).on_click(cx.listener(move|this,_,window,cx|this.read_pending_files(owner,window,cx)))))
            .child(readable("pending-files-notice",p.pending_files.notice.clone()));
        if let (Some(r), Some(revision)) = (&p.pending_files.report, p.pending_files.revision) {
            let page = p
                .pending_files
                .page
                .min(r.rows.len().saturating_sub(1) / PAGE_SIZE);
            body = body.child(readable(
                "pending-files-count",
                format!(
                    "Snapshot: {} pending files · page {} of {}",
                    r.rows.len(),
                    page + 1,
                    r.rows.len().saturating_sub(1) / PAGE_SIZE + 1
                ),
            ));
            for (slot, row) in r
                .rows
                .iter()
                .skip(page * PAGE_SIZE)
                .take(PAGE_SIZE)
                .enumerate()
            {
                body = body.child(readable(
                    SharedString::from(format!("pending-file-{slot}")),
                    format!(
                        "{} · {} · {} recorded bytes · {}",
                        preview(&crate::directory::visual_text(&row.name), 160),
                        row.kind,
                        row.bytes,
                        preview(&crate::directory::visual_text(&row.path), 240)
                    ),
                ));
            }
            let mut pages = h_flex().flex_wrap().gap_2();
            for (offset, id, label, target, edge) in [
                (
                    2,
                    "pending-files-prev",
                    "Previous files",
                    page.saturating_sub(1),
                    page == 0,
                ),
                (
                    3,
                    "pending-files-next",
                    "Next files",
                    page + 1,
                    (page + 1) * PAGE_SIZE >= r.rows.len(),
                ),
            ] {
                pages = pages.child(
                    div()
                        .id(SharedString::from(format!("{id}-anchor")))
                        .pt_1()
                        .anchor_scroll(Some(p.controls[BASE + offset].1.clone()))
                        .child(
                            Button::new(id)
                                .label(label)
                                .disabled(!ready || edge)
                                .track_focus(&p.controls[BASE + offset].0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pending_files_page(owner, revision, target, cx)
                                })),
                        ),
                );
            }
            body = body.child(pages);
        }
        let revision = p.pending_files.revision;
        body.child(
            div()
                .id("pending-files-copy-anchor")
                .pt_1()
                .anchor_scroll(Some(p.controls[BASE + 1].1.clone()))
                .child(
                    Button::new("pending-files-copy")
                        .label("Copy complete pending metadata")
                        .disabled(
                            !ready
                                || p.pending_files.report.is_none()
                                || p.pending_files.pending.is_some(),
                        )
                        .track_focus(&p.controls[BASE + 1].0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(revision) = revision {
                                this.copy_pending_files(owner, revision, cx);
                            }
                        })),
                ),
        )
        .child(self.render_reference_removal(cx))
    }
}
#[cfg(feature = "test-support")]
impl Shell {
    pub fn pending_files_count(&self) -> Option<usize> {
        Some(
            self.session_editor
                .as_ref()?
                .pending_files
                .report
                .as_ref()?
                .rows
                .len(),
        )
    }
    pub fn read_presented_pending_files(
        &mut self,
        form: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.session_editor else {
            return;
        };
        self.read_pending_files(
            Owner {
                instance: p.instance,
                connection: p.connection,
                form,
            },
            window,
            cx,
        );
    }
    pub fn pending_files_instance(&self) -> uuid::Uuid {
        self.session_editor.as_ref().unwrap().instance
    }
    pub fn pending_files_revision(&self) -> Option<uuid::Uuid> {
        self.session_editor.as_ref()?.pending_files.revision
    }
    pub fn copy_presented_pending_files(
        &self,
        form: uuid::Uuid,
        revision: uuid::Uuid,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.session_editor else {
            return;
        };
        self.copy_pending_files(
            Owner {
                instance: p.instance,
                connection: p.connection,
                form,
            },
            revision,
            cx,
        );
    }
}
