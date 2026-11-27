//! SQLite connection, PRAGMAs, migrations and integrity check (design §4).
//!
//! The module owns the SQLite file set in the data directory
//! (`vocabulary.sqlite` plus its `-wal`/`-shm` files and the process lock
//! `vocabulary.lock`; besides them the data directory holds only the cache
//! directory, R-85) and the schema version (`PRAGMA user_version`). Other modules
//! get a connection only through [`Db::read`] and [`Db::write`]; the SQL for a
//! table lives in the module that owns the table (R-92).
//!
//! One connection behind a mutex is plenty for a single user. Calls run on
//! tokio's blocking pool so they never stall the async runtime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

/// File name of the database inside the data directory.
const DB_FILE_NAME: &str = "vocabulary.sqlite";
/// Lock file: held by the one process that may use the database (see
/// [`DataDirLock`]).
const LOCK_FILE_NAME: &str = "vocabulary.lock";

/// One forward-only migration: its SQL, then an optional data step for
/// what SQL alone cannot compute. Both run in one transaction.
struct Migration {
    name: &'static str,
    sql: &'static str,
    data_step: Option<fn(&Connection) -> rusqlite::Result<()>>,
}

const fn sql_only(name: &'static str, sql: &'static str) -> Migration {
    Migration {
        name,
        sql,
        data_step: None,
    }
}

/// Embedded migrations; entry `i` brings the schema to version `i + 1`.
const MIGRATIONS: &[Migration] = &[
    sql_only(
        "0001_initial",
        include_str!("../migrations/0001_initial.sql"),
    ),
    sql_only(
        "0002_word_spelling_nullable",
        include_str!("../migrations/0002_word_spelling_nullable.sql"),
    ),
    sql_only(
        "0003_session_item_format",
        include_str!("../migrations/0003_session_item_format.sql"),
    ),
    sql_only(
        "0004_auth_owner",
        include_str!("../migrations/0004_auth_owner.sql"),
    ),
    Migration {
        name: "0005_review_round_1",
        sql: include_str!("../migrations/0005_review_round_1.sql"),
        data_step: Some(rekey_sentences),
    },
    sql_only("0006_audio", include_str!("../migrations/0006_audio.sql")),
    sql_only(
        "0007_word_edits",
        include_str!("../migrations/0007_word_edits.sql"),
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("data directory {0} does not exist or is not a directory")]
    DataDir(PathBuf),
    #[error(
        "there is no database in {0}; is the data directory mounted? (only `serve` creates one)"
    )]
    NoDatabase(PathBuf),
    #[error("the database in {0} is in use by a running vocab-server: stop the stack first")]
    InUse(PathBuf),
    #[error("cannot lock the database in {0}: {1}")]
    Lock(PathBuf, std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not enable {0}")]
    Pragma(&'static str),
    #[error("integrity check failed: {0}")]
    Integrity(String),
    #[error(
        "database schema version {found} is newer than this app knows ({known}); refusing to start"
    )]
    SchemaTooNew { found: u32, known: u32 },
    #[error("migration {name} failed: {source}")]
    Migration {
        name: &'static str,
        source: rusqlite::Error,
    },
    #[error("migration {name} left foreign key violations")]
    MigrationForeignKeys { name: &'static str },
    #[error("database worker failed: {0}")]
    Worker(String),
}

/// Exclusive use of the data directory by this process, for as long as the
/// value lives: an advisory `flock` on `vocabulary.lock`. `serve` holds it
/// for its lifetime, so a `reset-password` or `check` run next to a running
/// server (e.g. `docker compose run`) is refused instead of changing the
/// database behind the server's in-memory state.
#[derive(Debug)]
pub struct DataDirLock {
    _file: std::fs::File,
}

impl DataDirLock {
    /// Take the lock without waiting.
    pub fn acquire(data_dir: &Path) -> Result<Self, DbError> {
        let fail = |err| DbError::Lock(data_dir.to_owned(), err);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join(LOCK_FILE_NAME))
            .map_err(fail)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(DbError::InUse(data_dir.to_owned())),
            Err(std::fs::TryLockError::Error(err)) => Err(fail(err)),
        }
    }
}

/// Handle to the database; cheap to clone.
#[derive(Debug, Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    /// The database file, for [`Db::read_detached`].
    path: Arc<PathBuf>,
}

impl Db {
    /// Open (or create) the database in `data_dir`, apply the connection
    /// PRAGMAs, run `PRAGMA quick_check` and then the pending migrations.
    /// Any failure means the app must not start.
    pub fn open(data_dir: &Path) -> Result<Self, DbError> {
        if !data_dir.is_dir() {
            return Err(DbError::DataDir(data_dir.to_owned()));
        }
        Self::open_file(data_dir)
    }

    /// Is there a database file in `data_dir`?
    pub fn exists(data_dir: &Path) -> bool {
        data_dir.join(DB_FILE_NAME).is_file()
    }

    /// Like [`Db::open`], but only an existing database: a wrong or empty
    /// mount is an error rather than a fresh, empty database.
    pub fn open_existing(data_dir: &Path) -> Result<Self, DbError> {
        if !Self::exists(data_dir) {
            return Err(DbError::NoDatabase(data_dir.to_owned()));
        }
        Self::open_file(data_dir)
    }

    fn open_file(data_dir: &Path) -> Result<Self, DbError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let path = data_dir.join(DB_FILE_NAME);
        let mut conn = Connection::open_with_flags(&path, flags)?;
        configure(&conn)?;
        quick_check(&conn)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Arc::new(path),
        })
    }

    /// The schema version this app brings its databases to (the number of
    /// the last migration).
    pub fn schema_version() -> u32 {
        known_version()
    }

    /// Run long read-only work (a streamed export) on a connection of its
    /// own, so the shared connection stays free for everything else. The
    /// closure sees one consistent snapshot (WAL: writers go on meanwhile)
    /// and the connection is opened read-only.
    pub async fn read_detached<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        let path = Arc::clone(&self.path);
        tokio::task::spawn_blocking(move || {
            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let mut conn = Connection::open_with_flags(path.as_path(), flags)
                .map_err(|err| E::from(DbError::from(err)))?;
            conn.busy_timeout(Duration::from_millis(5000))
                .and_then(|()| conn.pragma_update(None, "query_only", true))
                .and_then(|()| conn.pragma_update(None, "temp_store", "MEMORY"))
                .map_err(|err| E::from(DbError::from(err)))?;
            let tx = conn
                .transaction()
                .map_err(|err| E::from(DbError::from(err)))?;
            f(&tx)
        })
        .await
        .map_err(|err| E::from(DbError::Worker(err.to_string())))?
    }

    /// Run read-only work. The closure sees one consistent snapshot (it runs
    /// inside a transaction that is rolled back afterwards) and the
    /// connection is `query_only` meanwhile, so an accidental write fails
    /// loudly instead of being rolled back silently.
    pub async fn read<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        self.with_conn(move |conn| {
            conn.pragma_update(None, "query_only", true)
                .map_err(DbError::from)?;
            let result = conn
                .transaction()
                .map_err(|err| E::from(DbError::from(err)))
                .and_then(|tx| f(&tx));
            // Always switch back, also when the closure failed.
            conn.pragma_update(None, "query_only", false)
                .map_err(DbError::from)?;
            result
        })
        .await
    }

    /// Run work in one write transaction (`BEGIN IMMEDIATE`); committed when
    /// the closure returns `Ok`, rolled back otherwise.
    pub async fn write<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        self.with_conn(move |conn| {
            // Normally already off; re-assert it in case a read closure panicked.
            conn.pragma_update(None, "query_only", false)
                .map_err(DbError::from)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(DbError::from)?;
            let value = f(&tx)?;
            tx.commit().map_err(DbError::from)?;
            Ok(value)
        })
        .await
    }

    /// Run work in a write transaction that is **always rolled back**, also
    /// when the closure succeeds. Used for previews that must behave exactly
    /// like the real operation but never persist anything.
    pub async fn dry_run<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        self.with_conn(move |conn| {
            conn.pragma_update(None, "query_only", false)
                .map_err(DbError::from)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(DbError::from)?;
            let result = f(&tx);
            tx.rollback().map_err(DbError::from)?;
            result
        })
        .await
    }

    async fn with_conn<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<DbError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            // A panic while holding the lock cannot leave a transaction open:
            // rusqlite rolls back on drop. So the connection stays usable.
            let mut guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(|err| E::from(DbError::Worker(err.to_string())))?
    }
}

/// Where an export writes table rows (design §8.1): one table after the
/// other, first its column names, then each row as the values of those
/// columns in that order.
pub trait TableSink {
    /// A table with these columns starts.
    fn begin_table(&mut self, name: &str, columns: &[String]) -> std::io::Result<()>;
    /// One row of the current table (one value per column).
    fn row(&mut self, values: &[serde_json::Value]) -> std::io::Result<()>;
    /// The current table is complete.
    fn end_table(&mut self) -> std::io::Result<()>;
}

/// Why writing a table failed.
#[derive(Debug, thiserror::Error)]
pub enum TableError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The sink refused (the reader went away, a limit was reached).
    #[error("output: {0}")]
    Io(#[from] std::io::Error),
}

/// Write every row of `sql` (one statement, read row by row, never
/// collected) to `sink` as table `name`. Integers, reals, text and NULL
/// keep their JSON type; blobs (no exported table has one) become NULL.
/// The owning module passes its own SQL (R-92).
pub fn write_table(
    conn: &Connection,
    sink: &mut dyn TableSink,
    name: &str,
    sql: &str,
) -> Result<(), TableError> {
    use rusqlite::types::ValueRef;
    let mut stmt = conn.prepare(sql)?;
    let columns: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    sink.begin_table(name, &columns)?;
    let mut rows = stmt.query([])?;
    let mut values = Vec::with_capacity(columns.len());
    while let Some(row) = rows.next()? {
        values.clear();
        for i in 0..columns.len() {
            values.push(match row.get_ref(i)? {
                ValueRef::Integer(n) => serde_json::Value::from(n),
                ValueRef::Real(x) => serde_json::Number::from_f64(x)
                    .map_or(serde_json::Value::Null, serde_json::Value::Number),
                ValueRef::Text(t) => {
                    serde_json::Value::String(String::from_utf8_lossy(t).into_owned())
                }
                ValueRef::Null | ValueRef::Blob(_) => serde_json::Value::Null,
            });
        }
        sink.row(&values)?;
    }
    sink.end_table()?;
    Ok(())
}

/// Connection PRAGMAs (§4.1).
fn configure(conn: &Connection) -> Result<(), DbError> {
    // WAL: crash-safe, -wal/-shm live next to the database file in the data dir.
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(DbError::Pragma("journal_mode=WAL"));
    }
    // R-85 asks for synchronous >= NORMAL; FULL costs nothing at this scale.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Read-only root filesystem: never create temp files outside the data dir.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    let foreign_keys: i64 = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(DbError::Pragma("foreign_keys"));
    }
    Ok(())
}

fn quick_check(conn: &Connection) -> Result<(), DbError> {
    let mut stmt = conn.prepare("PRAGMA quick_check")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() == 1 && rows[0] == "ok" {
        Ok(())
    } else {
        Err(DbError::Integrity(
            rows.into_iter().take(10).collect::<Vec<_>>().join("; "),
        ))
    }
}

fn known_version() -> u32 {
    u32::try_from(MIGRATIONS.len()).unwrap_or(u32::MAX)
}

fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let known = known_version();
    if current > known {
        return Err(DbError::SchemaTooNew {
            found: current,
            known,
        });
    }
    for (version, migration) in (1u32..).zip(MIGRATIONS.iter()).skip(current as usize) {
        let name = migration.name;
        let fail = |source| DbError::Migration { name, source };
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Exclusive)
            .map_err(fail)?;
        tx.execute_batch(migration.sql).map_err(fail)?;
        if let Some(data_step) = migration.data_step {
            data_step(&tx).map_err(fail)?;
        }
        let violations = tx
            .prepare("PRAGMA foreign_key_check")
            .and_then(|mut stmt| stmt.query([])?.next().map(|row| row.is_some()))
            .map_err(fail)?;
        if violations {
            return Err(DbError::MigrationForeignKeys { name });
        }
        tx.pragma_update(None, "user_version", version)
            .map_err(fail)?;
        tx.commit().map_err(fail)?;
        tracing::info!(migration = name, version, "applied database migration");
    }
    Ok(())
}

/// One stored sentence, as the 0005 data step reads it.
struct StoredSentence {
    id: i64,
    word_id: i64,
    en_marked: String,
    cs_marked: String,
    location: Option<String>,
    hidden: bool,
    edited: bool,
    updated_at: String,
}

/// The row a group of sentence copies becomes: the oldest id with the
/// newest text, as an import merge would have left it (R-22). The owner's
/// work wins: if a copy was edited, the newest edited copy gives the text;
/// a hidden or edited copy makes the result hidden or edited. Otherwise the
/// newest copy gives the Czech text and location, and its English only
/// when it marks another part of the sentence. `copies` are oldest first.
fn merged(copies: &[StoredSentence]) -> Option<StoredSentence> {
    let (oldest, newest) = (copies.first()?, copies.last()?);
    let source = copies.iter().rev().find(|c| c.edited).unwrap_or(newest);
    let en_marked = if source.edited
        || vocab_pack::keys::span_key(&source.en_marked)
            != vocab_pack::keys::span_key(&oldest.en_marked)
    {
        source.en_marked.clone()
    } else {
        oldest.en_marked.clone()
    };
    Some(StoredSentence {
        id: oldest.id,
        word_id: oldest.word_id,
        en_marked,
        cs_marked: source.cs_marked.clone(),
        location: source.location.clone(),
        hidden: copies.iter().any(|c| c.hidden),
        edited: copies.iter().any(|c| c.edited),
        updated_at: copies.iter().map(|c| c.updated_at.clone()).max()?,
    })
}

/// Data step of migration 0005: store every sentence's key as today's
/// [`vocab_pack::keys::sentence_key`] computes it. Sentences of one word
/// that now share a key are one sentence (see [`merged`]): reviews and
/// practice items that showed a newer copy point to the oldest row, and
/// the copies are deleted, set-based (one statement per table). A sentence
/// whose new key is empty (punctuation only) is never merged and keeps its
/// key.
fn rekey_sentences(conn: &Connection) -> rusqlite::Result<()> {
    // (word, key) → the copies, oldest first (rows come by id). A row whose
    // new key is empty keeps its old key and is never grouped with others.
    let mut groups: Vec<(String, Vec<StoredSentence>)> = Vec::new();
    let mut group_of: HashMap<(i64, String), usize> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, word_id, en_marked, cs_marked, location, hidden, edited, updated_at, \
             en_key FROM sentence ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let sentence = StoredSentence {
                id: row.get(0)?,
                word_id: row.get(1)?,
                en_marked: row.get(2)?,
                cs_marked: row.get(3)?,
                location: row.get(4)?,
                hidden: row.get(5)?,
                edited: row.get(6)?,
                updated_at: row.get(7)?,
            };
            let key = vocab_pack::keys::sentence_key(&sentence.en_marked);
            if key.is_empty() {
                let old_key: String = row.get(8)?;
                groups.push((old_key, vec![sentence]));
                continue;
            }
            match group_of.get(&(sentence.word_id, key.clone())) {
                Some(&index) => groups[index].1.push(sentence),
                None => {
                    group_of.insert((sentence.word_id, key.clone()), groups.len());
                    groups.push((key, vec![sentence]));
                }
            }
        }
    }
    conn.execute_batch(
        "CREATE TEMP TABLE sentence_merge (old_id INTEGER PRIMARY KEY, keep_id INTEGER NOT NULL)",
    )?;
    let mut merged_rows = 0usize;
    {
        let mut map =
            conn.prepare("INSERT INTO temp.sentence_merge (old_id, keep_id) VALUES (?1, ?2)")?;
        let mut update = conn.prepare(
            "UPDATE sentence SET en_marked = ?2, cs_marked = ?3, location = ?4, hidden = ?5, \
             edited = ?6, updated_at = ?7 WHERE id = ?1",
        )?;
        for (_, copies) in groups.iter().filter(|(_, copies)| copies.len() > 1) {
            let Some(result) = merged(copies) else {
                continue;
            };
            for copy in &copies[1..] {
                map.execute([copy.id, result.id])?;
                merged_rows += 1;
            }
            update.execute(rusqlite::params![
                result.id,
                result.en_marked,
                result.cs_marked,
                result.location,
                result.hidden,
                result.edited,
                result.updated_at
            ])?;
        }
    }
    conn.execute_batch(
        "UPDATE review_log SET sentence_id = m.keep_id FROM temp.sentence_merge m \
           WHERE review_log.sentence_id = m.old_id;
         UPDATE session_item SET sentence_id = m.keep_id FROM temp.sentence_merge m \
           WHERE session_item.sentence_id = m.old_id;
         DELETE FROM sentence WHERE id IN (SELECT old_id FROM temp.sentence_merge);
         DROP TABLE temp.sentence_merge;",
    )?;
    // Two passes: a new key may equal the old key of a row not yet updated,
    // and `(word_id, en_key)` is unique. A row kept with its old key whose
    // old key is now another row's key gets a key no text can have.
    conn.execute("UPDATE sentence SET en_key = char(1) || id", [])?;
    let mut update = conn.prepare("UPDATE sentence SET en_key = ?2 WHERE id = ?1")?;
    for (key, copies) in &groups {
        let Some(kept) = copies.first() else {
            continue;
        };
        let taken = vocab_pack::keys::sentence_key(&kept.en_marked).is_empty()
            && group_of.contains_key(&(kept.word_id, key.clone()));
        if !taken {
            update.execute(rusqlite::params![kept.id, key])?;
        }
    }
    if merged_rows > 0 {
        tracing::info!(
            merged = merged_rows,
            "merged sentences that differ only in punctuation"
        );
    }
    Ok(())
}
