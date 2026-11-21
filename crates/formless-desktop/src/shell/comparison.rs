use super::*;
use crate::comparison::{Pair, Report};

impl Shell {
    pub(super) fn comparison_connected(&self) -> bool {
        self.client_connected && self.client.as_ref().is_some_and(|client| client.is_ready())
    }
    pub(super) fn comparison_selected_id(&self) -> Option<&str> {
        if self.destination != Destination::Sessions {
            return None;
        }
        let index = self.directory.choice(&self.directory_rows)?;
        Some(
            &self
                .directory_rows
                .get(*self.directory.matches().get(index)?)?
                .id,
        )
    }
    pub(super) fn comparison_selected_pair(&self) -> Option<Pair> {
        let pair = Pair {
            left: self.comparison.left.clone()?,
            right: self.comparison_selected_id()?.into(),
        };
        pair.valid().then_some(pair)
    }
    pub(super) fn comparison_disabled_reason(&self) -> Option<&'static str> {
        if !self.comparison_connected() {
            Some("Connect to a Profile first")
        } else if self.destination != Destination::Sessions {
            Some("Choose Sessions in the Sessions directory")
        } else if self.comparison.left.is_none() {
            Some("Mark a left Session first")
        } else if self.comparison.pending() {
            Some("A comparison is awaiting its reply")
        } else if self.comparison_selected_pair().is_none() {
            Some("Select a different right Session")
        } else {
            None
        }
    }
    pub(super) fn reset_comparison(&mut self, notice: &str) {
        self.comparison.reset(notice);
        self.resource_pages.remove(&Destination::Comparison);
    }
    pub(super) fn mark_comparison(
        &mut self,
        id: String,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.comparison.revision != revision
            || !self.comparison_connected()
            || self.comparison_selected_id() != Some(id.as_str())
            || self.background_inert(window)
            || self.finish_directory_composition(window, cx)
        {
            return;
        }
        self.comparison.mark(id);
        self.resource_pages.remove(&Destination::Comparison);
        cx.notify();
    }
    pub(super) fn read_comparison(
        &mut self,
        pair: Pair,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.comparison_connected()
            || self.comparison.revision != revision
            || self.comparison.pending()
            || self.background_inert(window)
            || !pair.valid()
        {
            return;
        }
        if self.destination == Destination::Sessions {
            if self.comparison_selected_pair().as_ref() != Some(&pair)
                || self.finish_directory_composition(window, cx)
            {
                return;
            }
        } else if self.destination != Destination::Comparison
            || self.comparison.pair.as_ref() != Some(&pair)
        {
            return;
        }
        if [&pair.left, &pair.right]
            .iter()
            .any(|id| self.client_sessions.iter().filter(|s| &s.id == *id).count() != 1)
        {
            self.reset_comparison(
                "Pair identities changed. Refresh Sessions and mark a pair again.",
            );
            cx.notify();
            return;
        }
        let operation = uuid::Uuid::new_v4();
        let connection = self.client.as_ref().unwrap().connection();
        self.set_destination(Destination::Comparison, window, cx);
        self.resource_pages.remove(&Destination::Comparison);
        self.resource_body.set_offset(point(px(0.), px(0.)));
        self.resource_scroll
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        self.comparison.start(connection, operation, pair.clone());
        if let Err(error) = self
            .client
            .as_ref()
            .unwrap()
            .send(ClientCommand::CompareSessions {
                connection,
                operation,
                pair: pair.clone(),
            })
        {
            self.comparison_outcome(connection, operation, &pair, Err(error.into()), cx);
        }
        cx.notify();
    }
    pub(super) fn comparison_outcome(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        pair: &Pair,
        result: Result<Report, String>,
        cx: &mut Context<Self>,
    ) {
        if !self.comparison_connected()
            || self.client.as_ref().unwrap().connection() != connection
            || self.destination != Destination::Comparison
            || !self.comparison.receive(connection, operation, pair)
        {
            return;
        }
        let mut page = match result {
            Ok(report) => report.page,
            Err(error) => crate::pages::Page {
                notice: format!(
                    "Comparison unavailable: {}. Refresh explicitly or return to Sessions.",
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
        self.resource_pages.insert(Destination::Comparison, page);
        cx.notify();
    }
    #[cfg(feature = "test-support")]
    pub fn comparison_revision(&self) -> uuid::Uuid {
        self.comparison.revision
    }
    #[cfg(feature = "test-support")]
    pub fn compare_presented_pair(
        &mut self,
        pair: Pair,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.read_comparison(pair, revision, window, cx);
    }
}
