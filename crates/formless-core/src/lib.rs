pub mod anthropic;
pub mod artifacts;
pub mod auth;
pub mod backup_verification;
mod budget;
pub mod config;
pub mod daemon;
pub mod diff;
pub mod docs;
pub mod dupes;
pub mod errors;
pub mod files;
pub mod fuzzy;
pub mod hooks;
pub mod housekeeping;
pub mod import_url;
pub mod jobs;
pub mod logs;
pub mod mcp;
pub mod mcp_auth;
pub mod mcp_catalog;
pub mod memory;
pub mod namer;
pub mod optimiser;
pub mod path_scope;
pub mod paths;
pub mod presets;
pub mod profile_lock;
pub mod provider;
pub mod relay;
pub mod restore_references;
pub mod routing;
pub mod runs;
pub mod scripts;
pub mod secrets;
pub mod service;
pub mod setup_draft;
pub mod skills;
pub mod storage;
pub mod tools;
pub mod transcript;
pub mod webui;
#[cfg(windows)]
pub mod win_pipe;

pub use config::Config;
pub use paths::Paths;
pub use storage::Store;

#[cfg(test)]
#[path = "../tests/support/env.rs"]
mod credential_env;
