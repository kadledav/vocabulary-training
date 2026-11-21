//! The audio tables (owned by `audio`): `audio_clip` (one row per clip:
//! text, voice, status, retries) and `audio_engine` (the TTS service's
//! versions, part of every clip's identity).

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::key::Versions;

/// Kind of a clip; words go first in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Word,
    Sentence,
}

impl ClipKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Sentence => "sentence",
        }
    }
}

/// A clip that should exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub hash: String,
    pub text: String,
    pub voice: &'static str,
    pub kind: ClipKind,
}

/// A clip to generate now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub hash: String,
    pub text: String,
    pub voice: String,
    pub attempts: i64,
    pub crashes: i64,
}

/// A ready clip that can be played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playable {
    pub hash: String,
    pub approximate: bool,
}

/// The stored state of one clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipState {
    pub ready: bool,
    pub approximate: bool,
}

/// Clip counts for Settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub ready: i64,
    pub pending: i64,
    pub failed: i64,
    /// Ready clips with at least one approximated word.
    pub approximate: i64,
}

pub fn versions(conn: &Connection) -> rusqlite::Result<Option<Versions>> {
    conn.query_row(
        "SELECT frontend, engine FROM audio_engine WHERE id = 1",
        [],
        |row| {
            Ok(Versions {
                frontend: row.get(0)?,
                engine: row.get(1)?,
            })
        },
    )
    .optional()
}

pub fn set_versions(tx: &Transaction<'_>, versions: &Versions, now: &str) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO audio_engine (id, frontend, engine, checked_at) VALUES (1, ?1, ?2, ?3) \
         ON CONFLICT (id) DO UPDATE SET frontend = excluded.frontend, engine = excluded.engine, \
         checked_at = excluded.checked_at",
        params![versions.frontend, versions.engine, now],
    )?;
    Ok(())
}

/// Queue the wanted clips that have no row yet; returns how many.
pub fn ensure(tx: &Transaction<'_>, wanted: &[Wanted], now: &str) -> rusqlite::Result<usize> {
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO audio_clip (hash, text, voice, kind, status, attempts, next_attempt_at, \
         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?5, ?5)",
    )?;
    let mut added = 0;
    for w in wanted {
        added += stmt.execute(params![w.hash, w.text, w.voice, w.kind.as_str(), now])?;
    }
    Ok(added)
}

/// Delete every clip row whose hash is not in `keep`; returns the deleted
/// hashes (their files go too).
pub fn delete_except(
    tx: &Transaction<'_>,
    keep: &HashSet<String>,
) -> rusqlite::Result<Vec<String>> {
    let all = all_hashes(tx)?;
    let mut stmt = tx.prepare("DELETE FROM audio_clip WHERE hash = ?1")?;
    let mut deleted = Vec::new();
    for hash in all.into_iter().filter(|h| !keep.contains(h)) {
        stmt.execute([&hash])?;
        deleted.push(hash);
    }
    Ok(deleted)
}

pub fn all_hashes(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT hash FROM audio_clip ORDER BY hash")?;
    stmt.query_map([], |row| row.get(0))?.collect()
}

pub fn ready_hashes(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT hash FROM audio_clip WHERE status = 'ready' ORDER BY hash")?;
    stmt.query_map([], |row| row.get(0))?.collect()
}

/// The next due clip: words before sentences, then by when it became due
/// (a clip that failed waits behind the others with its backoff, so one
/// clip never starves the rest), oldest first.
pub fn next_due(conn: &Connection, now: &str) -> rusqlite::Result<Option<Job>> {
    conn.query_row(
        "SELECT hash, text, voice, attempts, crashes FROM audio_clip \
         WHERE status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?1) \
         ORDER BY kind = 'word' DESC, attempts > 0, COALESCE(next_attempt_at, created_at), \
         created_at, hash LIMIT 1",
        [now],
        |row| {
            Ok(Job {
                hash: row.get(0)?,
                text: row.get(1)?,
                voice: row.get(2)?,
                attempts: row.get(3)?,
                crashes: row.get(4)?,
            })
        },
    )
    .optional()
}

/// Bytes of all clip files (every voice and version).
pub fn total_bytes(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(bytes), 0) FROM audio_clip WHERE status = 'ready'",
        [],
        |row| row.get(0),
    )
}

/// The newest ready clip of `text` in the TTS voice `voice`, whatever
/// versions made it (an older one plays until its replacement is ready).
pub fn ready_by_text(
    conn: &Connection,
    voice: &str,
    text: &str,
) -> rusqlite::Result<Option<Playable>> {
    conn.query_row(
        "SELECT hash, approximate FROM audio_clip WHERE voice = ?1 AND text = ?2 \
         AND status = 'ready' ORDER BY updated_at DESC, hash LIMIT 1",
        params![voice, text],
        |row| {
            Ok(Playable {
                hash: row.get(0)?,
                approximate: row.get::<_, i64>(1)? > 0,
            })
        },
    )
    .optional()
}

/// Ready clips that still stand in for a wanted clip that is not ready
/// yet (same text and voice, older versions): kept by garbage collection.
pub fn stand_ins(conn: &Connection, wanted: &[Wanted]) -> rusqlite::Result<HashSet<String>> {
    let mut keep = HashSet::new();
    for w in wanted {
        if clip_state(conn, &w.hash)?.is_some_and(|s| s.ready) {
            continue;
        }
        if let Some(old) = ready_by_text(conn, w.voice, &w.text)? {
            keep.insert(old.hash);
        }
    }
    Ok(keep)
}

/// Delete the other clips of the same text and voice as `hash` (made by
/// older versions, replaced now); returns their hashes (their files go too).
pub fn delete_replaced(tx: &Transaction<'_>, hash: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "SELECT old.hash FROM audio_clip AS old JOIN audio_clip AS new \
         ON new.hash = ?1 AND old.voice = new.voice AND old.text = new.text AND old.hash != new.hash \
         ORDER BY old.hash",
    )?;
    let replaced: Vec<String> = stmt
        .query_map([hash], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut delete = tx.prepare("DELETE FROM audio_clip WHERE hash = ?1")?;
    for old in &replaced {
        delete.execute([old])?;
    }
    Ok(replaced)
}

/// When the earliest pending clip is due (none: `None`).
pub fn next_due_at(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT MIN(next_attempt_at) FROM audio_clip WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )
}

pub fn mark_ready(
    tx: &Transaction<'_>,
    hash: &str,
    bytes: i64,
    duration_ms: i64,
    approximate: i64,
    now: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE audio_clip SET status = 'ready', bytes = ?2, duration_ms = ?3, approximate = ?4, \
         last_error = NULL, next_attempt_at = NULL, updated_at = ?5 WHERE hash = ?1",
        params![hash, bytes, duration_ms, approximate, now],
    )?;
    Ok(())
}

/// How far a clip got: its attempts and crashes so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tries {
    pub attempts: i64,
    pub crashes: i64,
}

pub fn mark_retry(
    tx: &Transaction<'_>,
    hash: &str,
    tries: Tries,
    next_at: &str,
    error: &str,
    now: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE audio_clip SET attempts = ?2, crashes = ?3, next_attempt_at = ?4, last_error = ?5, \
         updated_at = ?6 WHERE hash = ?1 AND status = 'pending'",
        params![hash, tries.attempts, tries.crashes, next_at, error, now],
    )?;
    Ok(())
}

pub fn mark_failed(
    tx: &Transaction<'_>,
    hash: &str,
    tries: Tries,
    error: &str,
    now: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE audio_clip SET status = 'failed', attempts = ?2, crashes = ?3, last_error = ?4, \
         next_attempt_at = NULL, updated_at = ?5 WHERE hash = ?1",
        params![hash, tries.attempts, tries.crashes, error, now],
    )?;
    Ok(())
}

/// Back to the queue (a ready clip whose file is gone, R-85).
pub fn requeue(tx: &Transaction<'_>, hash: &str, now: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE audio_clip SET status = 'pending', attempts = 0, crashes = 0, next_attempt_at = ?2, \
         bytes = NULL, duration_ms = NULL, updated_at = ?2 WHERE hash = ?1",
        params![hash, now],
    )?;
    Ok(())
}

/// Failed clips get a fresh start; returns how many.
pub fn retry_failed(tx: &Transaction<'_>, now: &str) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE audio_clip SET status = 'pending', attempts = 0, crashes = 0, next_attempt_at = ?1, \
         last_error = NULL, updated_at = ?1 WHERE status = 'failed'",
        [now],
    )
}

pub fn clip_state(conn: &Connection, hash: &str) -> rusqlite::Result<Option<ClipState>> {
    conn.query_row(
        "SELECT status, approximate FROM audio_clip WHERE hash = ?1",
        [hash],
        |row| {
            Ok(ClipState {
                ready: row.get::<_, String>(0)? == "ready",
                approximate: row.get::<_, i64>(1)? > 0,
            })
        },
    )
    .optional()
}

/// Counts of the clips with these hashes (the current voice's set).
pub fn counts(conn: &Connection, hashes: &HashSet<String>) -> rusqlite::Result<Counts> {
    let mut stmt = conn.prepare("SELECT hash, status, approximate FROM audio_clip")?;
    let mut counts = Counts::default();
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (hash, status, approximate) = row?;
        if !hashes.contains(&hash) {
            continue;
        }
        match status.as_str() {
            "ready" => {
                counts.ready += 1;
                if approximate > 0 {
                    counts.approximate += 1;
                }
            }
            "pending" => counts.pending += 1,
            _ => counts.failed += 1,
        }
    }
    Ok(counts)
}
