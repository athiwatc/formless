//! Optional native desktop client for Formless, with a preserved Phase 0 fixture.
//!
//! [`model`], [`commands`], [`directory`], [`replies`], [`transcript`], [`memory`], [`timeline`], [`full_view`] and
//! [`fixture`] carry no GUI dependency. [`shell`] is the GPUI Kit view over them and is
//! only built with the `gui` feature.

pub mod bootstrap;
pub mod client;
pub mod commands;
pub mod comparison;
pub mod directory;
pub mod fixture;
pub mod forecast;
pub mod full_view;
pub mod live_overlay;
pub mod manage;
pub mod memory;
pub mod message_queue;
pub mod model;
pub mod pages;
pub mod preferences;
pub mod profile_search;
pub mod questions;
pub mod replies;
pub mod search;
pub mod session_edit;
pub mod session_export;
pub mod session_usage;
#[cfg(feature = "gui")]
pub mod shell;
pub mod slash;
pub mod snapshot_guard;
#[cfg(feature = "thai-breaks")]
pub mod thai;
#[cfg(feature = "gui")]
pub mod theme;
pub mod timeline;
pub mod topics;
pub mod transcript;
pub mod transcript_attachments;

pub mod reading_positions;

pub mod run_actions;
pub mod run_inspection;

pub mod session_fork;

pub mod invocations;

pub mod attempt_admission;

pub mod steps;

pub mod step_decision;

pub mod step_mode;

pub mod approvals;

pub mod model_catalog;

pub mod pending_files;

pub mod reference_detach;
