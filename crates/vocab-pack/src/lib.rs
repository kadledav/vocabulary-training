//! Pack format v1 (`docs/vocab-pack.schema.json`): pure format logic shared by
//! the server and its tests. No I/O and no database access (design §3.1).
//!
//! * [`paste::split_paste`]: a paste (prose, code fences, one or many JSON
//!   values) → raw packs with their content hash.
//! * [`validate::validate`]: one pack → typed header/words/sentences plus
//!   scoped errors and warnings (design §5.2).
//! * [`marked::split`]: the `[[…]]` target-span scanner.
//! * [`keys`]: mechanical identity keys (design §5.3).
//! * [`hash::content_sha256`]: canonical content hash (idempotency, R-11).

pub mod hash;
pub mod keys;
pub mod marked;
pub mod paste;
pub mod types;
pub mod validate;
