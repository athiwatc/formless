use super::*;
use crate::reference_detach::{Action, Outcome, Status};
use serde_json::Value;
const FIRST: usize = BASE + 4;
const CANCEL: usize = FIRST + PAGE_SIZE;
const CONFIRM: usize = CANCEL + 1;
impl Shell {
    fn arm_reference_removal(
        &mut self,
        owner: Owner,
        read: uuid::Uuid,
        index: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.pending_files_current(owner)
            || self.current_modal(self.layout(window)) != Some(Modal::SessionEditor)
            || !self.keep_session_edit_input(window, cx)
        {
            return;
        }
        let client = self.client.as_ref().unwrap();
        if client.reference_status().blocked() {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        if p.pending_files.revision != Some(read) || p.pending_files.pending.is_some() {
            return;
        }
        let Some(report) = p
            .pending_files
            .report
            .as_ref()
            .filter(|r| r.is_reference(index))
        else {
            return;
        };
        let action = Action {
            client: owner.instance,
            connection: owner.connection,
            epoch: client.epoch(),
            form: owner.form,
            read,
            operation: uuid::Uuid::new_v4(),
            session_id: p.id.clone(),
            index,
            report: std::sync::Arc::new(report.clone()),
        };
        let text = format!(
            "Remove only reference {} from Session {}. Complete selected metadata: {}",
            action.index + 1,
            action.session_id,
            action
                .report
                .reference_metadata(action.index)
                .unwrap_or_default()
        );
        p.pending_files.armed = Some(Review {
            action,
            text: text.into(),
        });
        p.pending_files.notice="Review this exact reference occurrence. Remove detaches metadata only; it never deletes target or artifact files. Cancel is focused.".into();
        p.controls[CANCEL].0.focus(window, cx);
        p.frames = 2;
        cx.notify();
    }
    fn cancel_reference_review(
        &mut self,
        owner: Owner,
        operation: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.pending_files_current(owner) {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        if p.pending_files
            .armed
            .as_ref()
            .is_some_and(|a| a.operation == operation && a.form == owner.form)
        {
            p.pending_files.armed = None;
            p.pending_files.notice = "Reference review cancelled; nothing was removed.".into();
            p.controls[BASE].0.focus(window, cx);
            p.frames = 2;
            cx.notify();
        }
    }
    fn confirm_reference_removal(
        &mut self,
        owner: Owner,
        operation: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.pending_files_current(owner)
            || self.current_modal(self.layout(window)) != Some(Modal::SessionEditor)
            || !self.keep_session_edit_input(window, cx)
        {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        let Some(a) = p
            .pending_files
            .armed
            .as_ref()
            .filter(|a| {
                a.operation == operation
                    && a.form == owner.form
                    && a.read == p.pending_files.revision.unwrap_or_default()
                    && a.client == owner.instance
                    && a.connection == owner.connection
            })
            .cloned()
            .map(|r| r.action)
        else {
            return;
        };
        let client = self.client.as_ref().unwrap();
        if client.epoch() != a.epoch || client.reference_status().blocked() {
            return;
        }
        p.pending_files.armed = None;
        match client.detach_reference(a){Ok(())=>p.pending_files.notice="Awaiting the reference-removal receipt; no automatic retry. Closing the form is not cancellation after writing.".into(),Err(e)=>p.pending_files.notice=format!("{e} No removal was queued.")}
        p.controls[BASE].0.focus(window, cx);
        p.frames = 2;
        cx.notify();
    }
    pub(in crate::shell) fn sync_reference_status(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        let status = client.reference_status();
        let Some(p) = self.session_editor.as_mut() else {
            return;
        };
        // Status is terminal once per operation. Compare its small identity and
        // phase, not the complete metadata on every UI poll.
        if p.pending_files.observed.action().map(|a| a.operation)
            == status.action().map(|a| a.operation)
            && std::mem::discriminant(&p.pending_files.observed) == std::mem::discriminant(&status)
        {
            return;
        }
        p.pending_files.observed = status.clone();
        if let Some(a) = status.action().filter(|a| {
            a.client == p.instance
                && a.session_id == p.id
                && a.form == p.revision
                && Some(a.read) == p.pending_files.revision
        }) {
            let _ = a;
            p.pending_files.notice = status.notice().unwrap_or_default().into();
            if let Status::Finished { outcome, .. } = &status {
                p.pending_files.armed = None;
                if matches!(outcome, Outcome::Removed | Outcome::Unknown) {
                    p.pending_files.report = None;
                    p.pending_files.revision = None;
                    p.pending_files.page = 0;
                }
            }
        }
        p.frames = 2;
        cx.notify();
    }
    pub(super) fn render_reference_removal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.session_editor.as_ref().unwrap();
        let owner = Owner::of(p);
        let status = self
            .client
            .as_ref()
            .map(|c| c.reference_status())
            .unwrap_or_default();
        let ready = self.pending_files_current(owner)
            && p.pending_files.pending.is_none()
            && !status.blocked();
        let mut body = v_flex().gap_2();
        if status.blocked() {
            body = body.child(readable(
                "reference-removal-status",
                status.notice().unwrap_or_default().to_string(),
            ));
        }
        if let (Some(report), Some(read)) = (&p.pending_files.report, p.pending_files.revision) {
            for (slot, index) in (p.pending_files.page * PAGE_SIZE..report.rows.len())
                .take(PAGE_SIZE)
                .enumerate()
            {
                if !report.is_reference(index as u32) {
                    continue;
                }
                let focus = p.controls[FIRST + slot].0.clone();
                let anchor = p.controls[FIRST + slot].1.clone();
                body = body.child(
                    div()
                        .id(SharedString::from(format!(
                            "reference-review-{slot}-anchor"
                        )))
                        .pt_1()
                        .anchor_scroll(Some(anchor))
                        .child(
                            Button::new(SharedString::from(format!("reference-review-{slot}")))
                                .label(format!("Review removing reference {}", index + 1))
                                .disabled(!ready)
                                .track_focus(&focus)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.arm_reference_removal(
                                        owner,
                                        read,
                                        index as u32,
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                );
            }
        }
        if let Some(a) = p
            .pending_files
            .armed
            .as_ref()
            .filter(|a| a.form == owner.form && Some(a.read) == p.pending_files.revision)
        {
            let operation = a.operation;
            body=body.child(readable("reference-removal-target",a.text.clone()))
                .child(readable("reference-removal-help","This removes one pending reference occurrence, not files. Shared metadata may change; the daemon compares the complete inventory atomically. Errors or a missing receipt remain Unknown; nothing is automatically replayed."))
                .child(div().id("reference-cancel-anchor").pt_1().anchor_scroll(Some(p.controls[CANCEL].1.clone())).child(Button::new("reference-cancel").label("Cancel reference removal").track_focus(&p.controls[CANCEL].0).on_click(cx.listener(move|this,_,window,cx|this.cancel_reference_review(owner,operation,window,cx)))))
                .child(div().id("reference-confirm-anchor").pt_1().anchor_scroll(Some(p.controls[CONFIRM].1.clone())).child(Button::new("reference-confirm").label("Remove this reference only").danger().disabled(!ready).track_focus(&p.controls[CONFIRM].0).on_click(cx.listener(move|this,_,window,cx|this.confirm_reference_removal(owner,operation,window,cx)))));
        }
        body
    }
}

#[cfg(feature = "test-support")]
impl Shell {
    pub fn reference_review_operation(&self) -> Option<uuid::Uuid> {
        self.session_editor
            .as_ref()?
            .pending_files
            .armed
            .as_ref()
            .map(|a| a.operation)
    }
    pub fn confirm_presented_reference(
        &mut self,
        form: uuid::Uuid,
        operation: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.session_editor else {
            return;
        };
        let owner = Owner {
            instance: p.instance,
            connection: p.connection,
            form,
        };
        self.confirm_reference_removal(owner, operation, window, cx);
    }
    pub fn cancel_presented_reference(
        &mut self,
        form: uuid::Uuid,
        operation: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.session_editor else {
            return;
        };
        let owner = Owner {
            instance: p.instance,
            connection: p.connection,
            form,
        };
        self.cancel_reference_review(owner, operation, window, cx);
    }
    pub fn finish_reference_for_test(
        &self,
        d: &crate::reference_detach::Dispatch,
        result: Result<Value, String>,
        ok: bool,
    ) {
        self.client
            .as_ref()
            .unwrap()
            .finish_recorded_reference(d, result, ok);
    }
    pub fn reference_status(&self) -> crate::reference_detach::Status {
        self.client
            .as_ref()
            .map(|c| c.reference_status())
            .unwrap_or_default()
    }
}
