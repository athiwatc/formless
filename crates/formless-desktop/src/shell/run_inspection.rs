use super::*;
use crate::run_inspection::{Report, Target};
type Source = (u64, Destination, u64, usize);
impl Shell {
    fn run_inspection_connected(&self) -> bool {
        self.client_connected && self.client.as_ref().is_some_and(|client| client.is_ready())
    }
    pub(super) fn selected_run_target(&self) -> Option<(Target, Source)> {
        if self.destination != Destination::Metrics {
            return None;
        }
        let source = self.resource_copy_target()?;
        if source.2 == 0 {
            return None;
        }
        let row = self
            .resource_pages
            .get(&Destination::Metrics)?
            .rows
            .get(source.2)?;
        if row.detail.len() > crate::run_inspection::MAX_BYTES {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(&row.detail).ok()?;
        Some((
            Target::from_metric(&value)?,
            (
                self.client.as_ref()?.connection(),
                source.0,
                source.1,
                source.2,
            ),
        ))
    }
    pub(super) fn run_inspection_disabled_reason(&self) -> Option<&'static str> {
        if !self.run_inspection_connected() {
            Some("Connect to a Profile first")
        } else if self.run_inspection.pending() {
            Some("A Run inspection is awaiting its reply")
        } else if self.selected_run_target().is_none() {
            Some("Select a Run entry in Metrics first")
        } else {
            None
        }
    }
    pub(super) fn reset_run_inspection(&mut self, notice: &str) {
        self.run_inspection.retire(notice);
        self.resource_pages.remove(&Destination::RunDetails);
    }
    pub(super) fn read_run_inspection(
        &mut self,
        target: Target,
        revision: uuid::Uuid,
        source: Option<Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.run_inspection_connected()
            || self.run_inspection.revision != revision
            || self.run_inspection.pending()
            || !target.valid()
            || self.background_inert(window)
        {
            return;
        }
        match source {
            Some(source)
                if self.selected_run_target().as_ref() == Some(&(target.clone(), source)) => {}
            None if self.destination == Destination::RunDetails
                && self.run_inspection.target.as_ref() == Some(&target)
                && self.run_inspection.connection
                    == Some(self.client.as_ref().unwrap().connection()) => {}
            _ => return,
        }
        let operation = uuid::Uuid::new_v4();
        let connection = self.client.as_ref().unwrap().connection();
        self.set_destination(Destination::RunDetails, window, cx);
        self.focus_region(Region::Conversation, window, cx);
        self.resource_pages.remove(&Destination::RunDetails);
        self.resource_body.set_offset(point(px(0.), px(0.)));
        self.resource_scroll
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        self.run_inspection
            .start(connection, operation, target.clone());
        if let Err(error) = self
            .client
            .as_ref()
            .unwrap()
            .send(ClientCommand::InspectRun {
                connection,
                operation,
                target: target.clone(),
            })
        {
            self.run_inspection_outcome(connection, operation, &target, Err(error.into()), cx);
        }
        cx.notify();
    }
    pub(super) fn run_inspection_outcome(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        target: &Target,
        result: Result<Report, String>,
        cx: &mut Context<Self>,
    ) {
        if !self.run_inspection_connected()
            || self.client.as_ref().unwrap().connection() != connection
            || self.destination != Destination::RunDetails
            || !self.run_inspection.receive(connection, operation, target)
        {
            return;
        }
        let mut page = match result {
            Ok(report) => report.page,
            Err(error) => crate::pages::Page {
                notice: format!(
                    "Run inspection unavailable: {}. Refresh explicitly or return to Metrics.",
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
        self.resource_pages.insert(Destination::RunDetails, page);
        cx.notify();
    }
    #[cfg(feature = "test-support")]
    pub fn run_inspection_source(&self) -> Option<Source> {
        self.selected_run_target().map(|(_, source)| source)
    }
    #[cfg(feature = "test-support")]
    pub fn run_inspection_revision(&self) -> uuid::Uuid {
        self.run_inspection.revision
    }
    #[cfg(feature = "test-support")]
    pub fn inspect_presented_run(
        &mut self,
        target: Target,
        revision: uuid::Uuid,
        source: Option<Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.read_run_inspection(target, revision, source, window, cx);
    }
}
