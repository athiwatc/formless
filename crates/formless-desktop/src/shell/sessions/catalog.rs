use super::*;
use crate::model_catalog::{Catalog, PAGE_SIZE};
#[derive(Default)]
pub(super) struct State {
    pending: Option<uuid::Uuid>,
    revision: Option<uuid::Uuid>,
    value: Option<Catalog>,
    notice: String,
    pub filter: String,
    pub page: usize,
}
#[derive(Clone, Copy)]
struct Read {
    instance: uuid::Uuid,
    connection: u64,
    form: uuid::Uuid,
}
#[derive(Clone)]
struct Fill {
    instance: uuid::Uuid,
    connection: u64,
    form: uuid::Uuid,
    catalog: uuid::Uuid,
    basis: String,
    selector: String,
    page: usize,
    filter: String,
}
impl Fill {
    fn present(p: &Panel, selector: &str, cx: &App) -> Self {
        Self {
            instance: p.instance,
            connection: p.connection,
            form: p.revision,
            catalog: p.catalog.revision.unwrap(),
            basis: p.input.as_ref().unwrap().read(cx).value().to_string(),
            selector: selector.into(),
            page: p.catalog.page,
            filter: p.catalog.filter.clone(),
        }
    }
}
impl Shell {
    fn catalog_ready(&self) -> bool {
        self.client_connected
            && self.session_editor.as_ref().is_some_and(|p| {
                !p.retired
                    && p.waiting.is_none()
                    && p.field == Field::Model
                    && p.metadata.as_ref().is_some_and(|m| m.selection.is_some())
                    && self.client.as_ref().is_some_and(|c| {
                        c.is_ready() && c.instance() == p.instance && c.connection() == p.connection
                    })
            })
    }
    fn read_model_catalog(&mut self, read: Read, window: &mut Window, cx: &mut Context<Self>) {
        if !self.catalog_ready()
            || self.current_modal(self.layout(window)) != Some(Modal::SessionEditor)
            || !self.keep_session_edit_input(window, cx)
        {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        if p.catalog.pending.is_some()
            || p.instance != read.instance
            || p.connection != read.connection
            || p.revision != read.form
        {
            return;
        }
        let operation = uuid::Uuid::new_v4();
        match self
            .client
            .as_ref()
            .unwrap()
            .send(ClientCommand::ReadModelCatalog {
                connection: p.connection,
                operation,
            }) {
            Ok(()) => {
                p.catalog=State{pending:Some(operation),notice:"Reading configured provider catalog; this may contact its authentication service. No Run or model test was requested.".into(),..Default::default()};
            }
            Err(_) => {
                p.catalog.notice =
                    "Catalog was not requested. Reconnect and reload current details first.".into()
            }
        }
        p.frames = 2;
        cx.notify();
    }
    pub(in crate::shell) fn model_catalog_received(
        &mut self,
        instance: uuid::Uuid,
        connection: u64,
        operation: uuid::Uuid,
        result: Result<Catalog, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.session_editor.as_mut() else {
            return;
        };
        if p.instance != instance
            || p.connection != connection
            || p.catalog.pending != Some(operation)
            || self.client.as_ref().is_none_or(|c| {
                !c.is_ready() || c.instance() != instance || c.connection() != connection
            })
        {
            return;
        }
        p.catalog.pending = None;
        p.catalog.revision = Some(operation);
        match result {
            Ok(value) => {
                p.catalog.notice=if value.provider_unavailable{"Provider listing unavailable. Showing stored aliases, Roles and recent choices from this read."}else{"Catalog read complete. Suggestions are a snapshot, not a guarantee that a model or Role will work."}.into();
                p.catalog.value = Some(value);
            }
            Err(_) => {
                p.catalog.value = None;
                p.catalog.notice="Catalog unavailable, malformed or above native bounds. Use manual entry or Formless TUI; provider error details are not displayed.".into();
            }
        }
        p.frames = 2;
        cx.notify();
    }
    fn fill_model_catalog(&mut self, fill: Fill, window: &mut Window, cx: &mut Context<Self>) {
        if !self.catalog_ready()
            || self.current_modal(self.layout(window)) != Some(Modal::SessionEditor)
        {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        if p.instance != fill.instance
            || p.connection != fill.connection
            || p.revision != fill.form
            || p.catalog.revision != Some(fill.catalog)
            || p.catalog.page != fill.page
            || p.catalog.filter != fill.filter
            || p.catalog.pending.is_some()
            || p.catalog
                .value
                .as_ref()
                .is_none_or(|c| !c.choices.iter().any(|v| v.selector == fill.selector))
        {
            return;
        }
        let Some(input) = p.input.clone() else {
            return;
        };
        if input.read(cx).value().as_ref() != fill.basis
            || input.update(cx, |i, cx| i.marked_text_range(window, cx).is_some())
        {
            return;
        }
        let end = fill.basis.encode_utf16().count();
        input.update(cx, |i, cx| {
            i.replace_text_in_range(Some(0..end), &fill.selector, window, cx)
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        p.catalog.notice="Choice filled the Model draft only. Review it, then use Save to request a setting change. Saving does not run a Provider test.".into();
        p.frames = 2;
        cx.notify();
    }
    fn catalog_page(
        &mut self,
        revision: uuid::Uuid,
        filter: String,
        page: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.catalog_ready() {
            return;
        }
        let p = self.session_editor.as_mut().unwrap();
        if p.catalog.revision != Some(revision) || p.catalog.filter != filter {
            return;
        }
        if let Some(c) = &p.catalog.value {
            p.catalog.page = page.min(c.matches(&filter).len().saturating_sub(1) / PAGE_SIZE);
            p.frames = 2;
            cx.notify();
        }
    }
    pub(super) fn render_model_catalog(
        &self,
        ready: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.session_editor.as_ref().unwrap();
        let read = Read {
            instance: p.instance,
            connection: p.connection,
            form: p.revision,
        };
        let mut body=v_flex().gap_2().child(readable("session-catalog-help","Explicit read contacts the default provider; no Run or model test. Includes configured aliases, Roles and recent selectors. Model draft edits then filter locally; clear it to show all. Choosing a row only fills the draft."))
            .child(div().id("session-catalog-read-anchor").pt_1().anchor_scroll(Some(p.controls[14].1.clone())).child(Button::new("session-catalog-read").label("Read catalog (contacts provider)").disabled(!ready||p.catalog.pending.is_some()).track_focus(&p.controls[14].0).on_click(cx.listener(move|this,_,window,cx|this.read_model_catalog(read,window,cx)))))
            .child(readable("session-catalog-notice",p.catalog.notice.clone()));
        if let (Some(c), Some(revision), Some(_input)) =
            (&p.catalog.value, p.catalog.revision, &p.input)
        {
            let matches = c.matches(&p.catalog.filter);
            let page = p
                .catalog
                .page
                .min(matches.len().saturating_sub(1) / PAGE_SIZE);
            body = body.child(readable(
                "session-catalog-count",
                format!(
                    "{} matching choices · page {} of {} · no availability test performed",
                    matches.len(),
                    page + 1,
                    (matches.len().saturating_sub(1) / PAGE_SIZE) + 1
                ),
            ));
            for (slot, index) in matches
                .iter()
                .skip(page * PAGE_SIZE)
                .take(PAGE_SIZE)
                .enumerate()
            {
                let choice = &c.choices[*index];
                let fill = Fill::present(p, &choice.selector, cx);
                body = body.child(
                    div()
                        .id(SharedString::from(format!(
                            "session-catalog-choice-anchor-{slot}"
                        )))
                        .pt_1()
                        .anchor_scroll(Some(p.controls[17 + slot].1.clone()))
                        .child(
                            Button::new(SharedString::from(format!(
                                "session-catalog-choice-{slot}"
                            )))
                            .label(choice.label())
                            .disabled(!ready)
                            .track_focus(&p.controls[17 + slot].0)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.fill_model_catalog(fill.clone(), window, cx)
                                },
                            )),
                        ),
                );
            }
            let mut pages = h_flex().flex_wrap().gap_2();
            for (slot, label, target, disabled) in [
                (15, "Previous choices", page.saturating_sub(1), page == 0),
                (
                    16,
                    "Next choices",
                    page + 1,
                    (page + 1) * PAGE_SIZE >= matches.len(),
                ),
            ] {
                let filter = p.catalog.filter.clone();
                pages = pages.child(
                    div()
                        .id(SharedString::from(format!(
                            "session-catalog-page-anchor-{slot}"
                        )))
                        .pt_1()
                        .anchor_scroll(Some(p.controls[slot].1.clone()))
                        .child(
                            Button::new(if slot == 15 {
                                "session-catalog-prev"
                            } else {
                                "session-catalog-next"
                            })
                            .label(label)
                            .disabled(!ready || disabled)
                            .track_focus(&p.controls[slot].0)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.catalog_page(revision, filter.clone(), target, cx)
                                },
                            )),
                        ),
                );
            }
            body = body.child(pages);
        }
        body
    }
}

#[cfg(feature = "test-support")]
#[derive(Clone)]
pub struct CatalogChoiceForTest(Fill);
#[cfg(feature = "test-support")]
impl Shell {
    pub fn capture_catalog_choice(&self, selector: &str, cx: &App) -> CatalogChoiceForTest {
        CatalogChoiceForTest(Fill::present(
            self.session_editor.as_ref().unwrap(),
            selector,
            cx,
        ))
    }
    pub fn fill_captured_catalog_choice(
        &mut self,
        ticket: CatalogChoiceForTest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill_model_catalog(ticket.0, window, cx);
    }

    pub fn read_presented_session_catalog(
        &mut self,
        form: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.session_editor else {
            return;
        };
        self.read_model_catalog(
            Read {
                instance: p.instance,
                connection: p.connection,
                form,
            },
            window,
            cx,
        );
    }
    pub fn session_catalog_instance(&self) -> uuid::Uuid {
        self.session_editor.as_ref().unwrap().instance
    }
    pub fn session_catalog_revision(&self) -> Option<uuid::Uuid> {
        self.session_editor.as_ref()?.catalog.revision
    }
    pub fn fill_presented_session_catalog(
        &mut self,
        form: uuid::Uuid,
        catalog: uuid::Uuid,
        basis: &str,
        selector: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.session_editor else {
            return;
        };
        self.fill_model_catalog(
            Fill {
                instance: p.instance,
                connection: p.connection,
                form,
                catalog,
                basis: basis.into(),
                selector: selector.into(),
                page: p.catalog.page,
                filter: p.catalog.filter.clone(),
            },
            window,
            cx,
        );
    }
}
