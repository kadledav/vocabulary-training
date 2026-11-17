//! `vocab-server`: the vocabulary training app (design §3.2).
//!
//! Every module owns its state and exposes a small interface (R-92); the
//! binary in `main.rs` only wires them together. Modules of later milestones
//! (scheduler, session, audio, auth, settings, stats, exports) are added next
//! to these.

pub mod catalog;
pub mod clock;
pub mod config;
pub mod db;
pub mod http;
pub mod importer;
pub mod scheduler;
pub mod session;
pub mod settings;
pub mod stats;
pub mod web_assets;
pub mod workflows;
