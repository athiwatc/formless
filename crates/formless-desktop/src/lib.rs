//! Optional native desktop client for Formless: Phase 0 spike of epic #1753.
//!
//! [`model`] and [`fixture`] carry no GUI dependency. [`shell`] is the GPUI
//! Kit view over them and is only built with the `gui` feature.

pub mod fixture;
pub mod model;
#[cfg(feature = "gui")]
pub mod shell;
