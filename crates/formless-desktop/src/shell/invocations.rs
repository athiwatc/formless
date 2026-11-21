use super::*;
use crate::invocations::{Key, Report};
type Presented = (u64, Destination, u64, usize, Key);
impl Shell {
    fn invocations_connected(&self) -> bool {
        self.client_connected && self.client.as_ref().is_some_and(|client| client.is_ready())
    }
    pub(super) fn invocations_disabled_reason(&self) -> Option<&'static str> {
        if !self.invocations_connected() {
            Some("Connect to a Profile first")
        } else if self.invocations.pending() {
            Some("An Invocation snapshot is awaiting its reply")
        } else {
            None
        }
    }
    pub(super) fn retire_invocations(&mut self, notice: &str) {
        self.invocations.retire(notice);
        self.resource_pages.remove(&Destination::Invocations);
    }
    pub(super) fn view_invocations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.invocations_disabled_reason().is_some()
            || self.background_inert(window)
            || self.composer.update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            })
        {
            return;
        }
        self.set_destination(Destination::Invocations, window, cx);
        self.retire_invocations("Reading unfinished Tool Invocations.");
        let connection = self.client.as_ref().unwrap().connection();
        self.read_invocations(self.invocations.revision, connection, window, cx);
    }
    pub(super) fn read_invocations(
        &mut self,
        revision: uuid::Uuid,
        connection: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.invocations_connected()
            || self.destination != Destination::Invocations
            || self.invocations.revision != revision
            || self.invocations.pending()
            || self.background_inert(window)
            || self.composer.update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            })
            || self.client.as_ref().unwrap().connection() != connection
            || self
                .invocations
                .connection
                .is_some_and(|owner| owner != connection)
        {
            return;
        }
        let operation = uuid::Uuid::new_v4();
        self.invocations.start(connection, operation);
        self.resource_pages.remove(&Destination::Invocations);
        self.resource_body.set_offset(point(px(0.), px(0.)));
        self.resource_scroll
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        self.focus_region(Region::Conversation, window, cx);
        if let Err(error) = self
            .client
            .as_ref()
            .unwrap()
            .send(ClientCommand::ReadInvocations {
                connection,
                operation,
            })
        {
            self.invocations_outcome(connection, operation, Err(error.into()), cx);
        }
        cx.notify();
    }
    pub(super) fn invocations_outcome(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        result: Result<Report, String>,
        cx: &mut Context<Self>,
    ) {
        if !self.invocations_connected()
            || self.client.as_ref().unwrap().connection() != connection
            || self.destination != Destination::Invocations
            || !self.invocations.receive(connection, operation)
        {
            return;
        }
        let mut page = match result {
            Ok(report) => {
                self.invocations.keys = report.keys;
                report.page
            }
            Err(error) => crate::pages::Page {
                notice: format!(
                    "Invocation snapshot unavailable: {}. Refresh explicitly or return to Tools.",
                    crate::questions::preview(&error, 500)
                ),
                ..Default::default()
            },
        };
        self.resource_revision = self
            .resource_revision
            .checked_add(1)
            .expect("resource revision exhausted");
        page.revision = self.resource_revision;
        self.resource_pages.insert(Destination::Invocations, page);
        cx.notify();
    }
    pub(super) fn selected_invocation(&self) -> Option<Presented> {
        if self.destination != Destination::Invocations {
            return None;
        }
        let source = self.resource_copy_target()?;
        let key = self.invocations.keys.get(source.2.checked_sub(1)?)?.clone();
        Some((
            self.invocations.connection?,
            source.0,
            source.1,
            source.2,
            key,
        ))
    }
    pub(super) fn invocation_open_reason(&self) -> Option<&'static str> {
        if !self.invocations_connected() {
            return Some("Connect to a Profile first");
        }
        let Some(source) = self.selected_invocation() else {
            return Some("Select an Invocation record first");
        };
        if self.client.as_ref().unwrap().connection() != source.0 {
            return Some("Connection changed. Read Invocations again");
        }
        if self
            .client_sessions
            .iter()
            .filter(|session| session.id == source.4.session_id && !session.archived)
            .count()
            != 1
        {
            return Some(
                "Session is unavailable or archived. Refresh/restore it in Sessions first",
            );
        }
        None
    }
    pub(super) fn open_invocation_session(
        &mut self,
        source: Presented,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.invocations.revision != revision
            || self.selected_invocation().as_ref() != Some(&source)
            || self.invocation_open_reason().is_some()
            || self.background_inert(window)
            || self.composer.update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            })
        {
            return;
        }
        if !self
            .sidebar
            .rows()
            .iter()
            .any(|row| row.id == source.4.session_id)
        {
            return;
        }
        self.sidebar.select_id(&source.4.session_id);
        self.activate_selected(window, cx);
        self.focus_region(Region::Composer, window, cx);
        cx.notify();
    }
    #[cfg(feature = "test-support")]
    pub fn refresh_presented_invocations(
        &mut self,
        revision: uuid::Uuid,
        connection: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.read_invocations(revision, connection, window, cx);
    }
    #[cfg(feature = "test-support")]
    pub fn invocation_presented(&self) -> Option<Presented> {
        self.selected_invocation()
    }
    #[cfg(feature = "test-support")]
    pub fn invocations_revision(&self) -> uuid::Uuid {
        self.invocations.revision
    }
    #[cfg(feature = "test-support")]
    pub fn open_presented_invocation_session(
        &mut self,
        source: Presented,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_invocation_session(source, revision, window, cx);
    }
}
