//! `vocab-server`: the vocabulary training app (design §3.2).
//!
//! Every module owns its state and exposes a small interface (R-92); the
//! binary in `main.rs` only wires them together. Modules of later milestones
//! (audio, exports) are added next to these.

pub mod audio;
pub mod auth;
pub mod catalog;
pub mod clock;
pub mod config;
pub mod db;
pub mod http;
pub mod importer;
mod keep_alive;
mod rate_limit;
pub mod scheduler;
pub mod session;
pub mod settings;
pub mod stats;
pub mod web_assets;
pub mod workflows;
