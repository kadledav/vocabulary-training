//! SQLite connection, PRAGMAs, migrations and integrity check (design §4).
//!
//! The module owns the SQLite file set in the data directory
//! (`vocabulary.sqlite` plus its `-wal`/`-shm` files; nothing else is written
//! there, R-85) and the schema version (`PRAGMA user_version`). Other modules
//! get a connection only through [`Db::read`] and [`Db::write`]; the SQL for a
//! table lives in the module that owns the table (R-92).
//!
//! One connection behind a mutex is plenty for a single user. Calls run on
//! tokio's blocking pool so they never stall the async runtime.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

/// File name of the database inside the data directory.
const DB_FILE_NAME: &str = "vocabulary.sqlite";

/// Embedded, forward-only migrations; entry `i` brings the schema to version `i + 1`.
const MIGRATIONS: &[(&str, &str)] = &[(
    "0001_initial",
    include_str!("../migrations/0001_initial.sql"),
)];

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("data directory {0} does not exist or is not a directory")]
    DataDir(PathBuf),
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

/// Handle to the database; cheap to clone.
#[derive(Debug, Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Open (or create) the database in `data_dir`, apply the connection
    /// PRAGMAs, run `PRAGMA quick_check` and then the pending migrations.
    /// Any failure means the app must not start.
    pub fn open(data_dir: &Path) -> Result<Self, DbError> {
        if !data_dir.is_dir() {
            return Err(DbError::DataDir(data_dir.to_owned()));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut conn = Connection::open_with_flags(data_dir.join(DB_FILE_NAME), flags)?;
        configure(&conn)?;
        quick_check(&conn)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
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

    /// Liveness probe: the connection answers a trivial query.
    pub async fn ping(&self) -> Result<(), DbError> {
        self.read(|conn| {
            Ok(conn
                .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .map(drop)?)
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
    for (version, (name, sql)) in (1u32..).zip(MIGRATIONS.iter()).skip(current as usize) {
        let name: &'static str = name;
        let fail = |source| DbError::Migration { name, source };
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Exclusive)
            .map_err(fail)?;
        tx.execute_batch(sql).map_err(fail)?;
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
