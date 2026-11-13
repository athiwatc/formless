//! Optional native desktop client for Formless: Phase 0 spike of epic #1753.
//!
//! [`model`], [`commands`], [`directory`], [`replies`], [`transcript`], [`memory`], [`timeline`], [`full_view`] and
//! [`fixture`] carry no GUI dependency. [`shell`] is the GPUI Kit view over them and is
//! only built with the `gui` feature.

pub mod commands;
pub mod directory;
pub mod fixture;
pub mod full_view;
pub mod memory;
pub mod model;
pub mod replies;
pub mod search;
#[cfg(feature = "gui")]
pub mod shell;
pub mod snapshot_guard;
#[cfg(feature = "thai-breaks")]
pub mod thai;
pub mod timeline;
pub mod transcript;
