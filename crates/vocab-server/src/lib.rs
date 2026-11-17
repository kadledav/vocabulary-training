//! `vocab-server`: the vocabulary training app (design §3.2).
//!
//! Every module owns its state and exposes a small interface (R-92); the
//! binary in `main.rs` only wires them together. Modules of later milestones
//! (catalog, importer, scheduler, session, audio, auth, settings, stats,
//! exports, workflows) are added next to these.

pub mod clock;
pub mod config;
pub mod db;
pub mod http;
pub mod web_assets;
