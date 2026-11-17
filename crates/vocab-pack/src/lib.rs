//! Pack format v1 (`docs/vocab-pack.schema.json`): pure format logic shared by
//! the server and its tests. No I/O and no database access.
//!
//! Milestone M1 only creates the crate; paste splitting, validation, the
//! `[[…]]` scanner, identity keys and content hashing arrive in M2
//! (design §3.1).
