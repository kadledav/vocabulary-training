//! Database foundations: PRAGMAs, migration 0001 (the full §4.2 schema),
//! integrity checks and where SQLite puts its files (R-85).

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::Connection;
use vocab_server::db::{Db, DbError};

const DB_FILE: &str = "vocabulary.sqlite";
const FIXTURE: &str = include_str!("fixtures/schema_v1_synthetic.sql");

/// Every table of design §4.2.
const TABLES: &[&str] = &[
    "audio_clip",
    "auth_session",
    "card",
    "field_conflict",
    "fsrs_params",
    "pack_import",
    "practice_session",
    "review_log",
    "sentence",
    "session_item",
    "setting",
    "source",
    "word",
    "word_form",
    "word_item",
    "word_override",
    "word_source",
];

fn data_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let data = root.path().join("data");
    std::fs::create_dir(&data).expect("data dir");
    (root, data)
}

/// A second, independent connection for inspecting the file.
fn inspect(data: &Path) -> Connection {
    Connection::open(data.join(DB_FILE)).expect("open for inspection")
}

fn pragma_rows(conn: &Connection, pragma: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("PRAGMA {pragma}")).expect("prepare");
    stmt.query_map([], |row| row.get::<_, rusqlite::types::Value>(0))
        .expect("query")
        .map(|v| format!("{:?}", v.expect("row")))
        .collect()
}

fn file_names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn r85_empty_database_migrates_to_v1_with_foreign_keys_and_integrity_ok() {
    let (_root, data) = data_dir();
    let _db = Db::open(&data).expect("open");
    let conn = inspect(&data);

    let version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("user_version");
    assert_eq!(version, 2);

    let mut stmt = conn
        .prepare("SELECT name, strict FROM pragma_table_list WHERE schema = 'main' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("table list");
    let tables: Vec<(String, bool)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let names: Vec<&str> = tables.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, TABLES);
    assert!(
        tables.iter().all(|(_, strict)| *strict),
        "every table is STRICT: {tables:?}"
    );

    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
    assert_eq!(pragma_rows(&conn, "integrity_check"), vec!["Text(\"ok\")"]);
}

#[test]
fn r85_synthetic_fixture_satisfies_every_constraint() {
    let (_root, data) = data_dir();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let db = Db::open(&data).expect("open");
    rt.block_on(db.write(|tx| tx.execute_batch(FIXTURE).map_err(DbError::from)))
        .expect("fixture inserts");

    let conn = inspect(&data);
    for table in TABLES {
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .expect("count");
        assert!(count >= 1, "fixture covers {table}");
    }
    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
    assert_eq!(pragma_rows(&conn, "integrity_check"), vec!["Text(\"ok\")"]);

    // Cascades work: deleting the word removes its dependent rows.
    rt.block_on(db.write(|tx| {
        tx.execute("DELETE FROM session_item", [])?;
        tx.execute("DELETE FROM review_log", [])?;
        tx.execute("DELETE FROM word_source", [])?;
        tx.execute("DELETE FROM word WHERE id = 1", [])
            .map_err(DbError::from)
    }))
    .expect("delete word");
    let leftovers: i64 = conn
        .query_row(
            "SELECT (SELECT count(*) FROM sentence) + (SELECT count(*) FROM card) + (SELECT count(*) FROM word_item)",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(leftovers, 0);
}

#[test]
fn r85_foreign_keys_and_checks_are_enforced_on_the_app_connection() {
    let (_root, data) = data_dir();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let db = Db::open(&data).expect("open");

    let orphan = rt.block_on(db.write(|tx| {
        tx.execute(
            "INSERT INTO sentence (word_id, en_marked, cs_marked, en_key, origin, position, created_at, updated_at)
             VALUES (999, 'a [[b]]', 'c [[d]]', 'a b', 'example', 0, 'x', 'x')",
            [],
        )
        .map_err(DbError::from)
    }));
    assert!(orphan.is_err(), "foreign key violation must be rejected");

    let wrong_type = rt.block_on(db.write(|tx| {
        tx.execute(
            "INSERT INTO fsrs_params (fitted_at, params_json, review_count, active) VALUES ('x', '[]', 'many', 0)",
            [],
        )
            .map_err(DbError::from)
    }));
    assert!(
        wrong_type.is_err(),
        "STRICT tables reject wrongly typed values"
    );

    // A failed write transaction leaves nothing behind.
    let count: i64 = rt
        .block_on(db.read(|conn| {
            conn.query_row("SELECT count(*) FROM sentence", [], |r| r.get(0))
                .map_err(DbError::from)
        }))
        .expect("count");
    assert_eq!(count, 0);
}

#[test]
fn r85_connection_uses_wal_full_sync_and_memory_temp_store() {
    let (_root, data) = data_dir();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let db = Db::open(&data).expect("open");
    let (mode, sync, temp, fk, busy) = rt
        .block_on(db.read(|conn| {
            let get = |p: &str| {
                conn.pragma_query_value(None, p, |r| r.get::<_, rusqlite::types::Value>(0))
            };
            Ok::<_, DbError>((
                get("journal_mode")?,
                get("synchronous")?,
                get("temp_store")?,
                get("foreign_keys")?,
                get("busy_timeout")?,
            ))
        }))
        .expect("pragmas");
    use rusqlite::types::Value::{Integer, Text};
    assert_eq!(mode, Text("wal".into()));
    assert_eq!(sync, Integer(2), "synchronous = FULL");
    assert_eq!(temp, Integer(2), "temp_store = MEMORY");
    assert_eq!(fk, Integer(1));
    assert_eq!(busy, Integer(5000));
}

#[test]
fn r85_database_files_live_only_in_the_data_dir() {
    let (root, data) = data_dir();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let db = Db::open(&data).expect("open");
    rt.block_on(db.write(|tx| {
        tx.execute(
            "INSERT INTO setting (key, value_json) VALUES ('block_size', '20')",
            [],
        )
        .map_err(DbError::from)
    }))
    .expect("write");

    let expected: BTreeSet<String> = [DB_FILE, "vocabulary.sqlite-wal", "vocabulary.sqlite-shm"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        file_names(&data),
        expected,
        "only the SQLite file set is in the data dir"
    );
    assert_eq!(
        file_names(root.path()),
        BTreeSet::from(["data".to_owned()]),
        "nothing is written next to the data dir"
    );

    // The committed write is durable in the WAL: a fresh connection sees it.
    let value: String = inspect(&data)
        .query_row(
            "SELECT value_json FROM setting WHERE key = 'block_size'",
            [],
            |r| r.get(0),
        )
        .expect("read back");
    assert_eq!(value, "20");
}

#[test]
fn r85_reopening_is_idempotent() {
    let (_root, data) = data_dir();
    drop(Db::open(&data).expect("first open"));
    drop(Db::open(&data).expect("second open"));
    let version: u32 = inspect(&data)
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("user_version");
    assert_eq!(version, 2);
}

#[test]
fn r85_newer_schema_is_refused() {
    let (_root, data) = data_dir();
    drop(Db::open(&data).expect("open"));
    inspect(&data)
        .pragma_update(None, "user_version", 99)
        .expect("bump");
    match Db::open(&data) {
        Err(DbError::SchemaTooNew {
            found: 99,
            known: 2,
        }) => {}
        other => panic!("expected SchemaTooNew, got {other:?}"),
    }
}

#[test]
fn r85_damaged_database_file_is_refused() {
    let (_root, data) = data_dir();
    std::fs::write(data.join(DB_FILE), vec![0x5a; 8192]).expect("write garbage");
    assert!(Db::open(&data).is_err());
}

#[test]
fn r85_missing_data_dir_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    match Db::open(&root.path().join("absent")) {
        Err(DbError::DataDir(_)) => {}
        other => panic!("expected DataDir error, got {other:?}"),
    }
    assert!(file_names(root.path()).is_empty(), "nothing created");
}

#[test]
fn r85_writes_inside_a_read_fail_loudly() {
    let (_root, data) = data_dir();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let db = Db::open(&data).expect("open");
    let attempt = rt.block_on(db.read(|conn| {
        conn.execute(
            "INSERT INTO setting (key, value_json) VALUES ('x', '1')",
            [],
        )
        .map_err(DbError::from)
    }));
    let err = attempt.expect_err("write in read must fail");
    assert!(err.to_string().contains("readonly"), "{err}");

    // The connection is writable again afterwards.
    rt.block_on(db.write(|tx| {
        tx.execute(
            "INSERT INTO setting (key, value_json) VALUES ('x', '1')",
            [],
        )
        .map_err(DbError::from)
    }))
    .expect("write after read");
}

/// Migration 0002 on a database at version 1 holding the synthetic fixture:
/// data kept, `word.spelling` nullable, constraints intact.
#[test]
fn r85_v1_database_with_data_migrates_to_v2() {
    let (_root, data) = data_dir();
    {
        let conn = Connection::open(data.join(DB_FILE)).expect("create");
        conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
        conn.execute_batch(include_str!("../migrations/0001_initial.sql"))
            .expect("v1 schema");
        conn.pragma_update(None, "user_version", 1)
            .expect("version");
        conn.execute_batch(FIXTURE).expect("fixture");
        conn.execute("UPDATE word SET spelling = 1 WHERE id = 1", [])
            .expect("spelling");
    }
    drop(Db::open(&data).expect("migrates"));
    let conn = inspect(&data);
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("user_version");
    assert_eq!(version, 2);
    let (headword, spelling): (String, Option<i64>) = conn
        .query_row(
            "SELECT headword, spelling FROM word WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("word kept");
    assert_eq!((headword.as_str(), spelling), ("glimmerwick", Some(1)));
    let counts: (i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM sentence), (SELECT count(*) FROM word_source), (SELECT count(*) FROM card)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("counts");
    assert!(
        counts.0 >= 1 && counts.1 >= 1 && counts.2 >= 1,
        "{counts:?}"
    );
    conn.execute("UPDATE word SET spelling = NULL WHERE id = 1", [])
        .expect("nullable now");
    assert!(
        conn.execute("UPDATE word SET spelling = 2 WHERE id = 1", [])
            .is_err(),
        "CHECK kept"
    );
    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
    assert_eq!(pragma_rows(&conn, "integrity_check"), vec!["Text(\"ok\")"]);
}
