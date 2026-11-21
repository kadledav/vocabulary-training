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
    "audio_engine",
    "auth_owner",
    "auth_session",
    "auth_throttle",
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
    assert_eq!(version, 6);

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
    // Tables of later migrations (the fixture also serves the v1 schema).
    rt.block_on(db.write(|tx| {
        tx.execute_batch(
            "INSERT INTO audio_engine (id, frontend, engine, checked_at) \
             VALUES (1, 'g2p-2', 'kokoro-test', '2026-11-17T10:00:00Z');",
        )
        .map_err(DbError::from)
    }))
    .expect("later tables");

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
    assert_eq!(version, 6);
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
            known: 6,
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

/// Migrations 0002 and 0003 on a database at version 1 holding the synthetic
/// fixture: data kept, `word.spelling` nullable, constraints intact.
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
    assert_eq!(version, 6);
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

/// Migration 0003 on a database at version 2 holding the synthetic fixture:
/// practice items kept with `format` NULL, the new column's CHECK enforced.
#[test]
fn r85_v2_database_with_data_migrates_to_v3() {
    let (_root, data) = data_dir();
    {
        let conn = Connection::open(data.join(DB_FILE)).expect("create");
        conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
        conn.execute_batch(include_str!("../migrations/0001_initial.sql"))
            .expect("v1 schema");
        conn.execute_batch(FIXTURE).expect("fixture");
        conn.execute_batch(include_str!(
            "../migrations/0002_word_spelling_nullable.sql"
        ))
        .expect("v2 schema");
        conn.pragma_update(None, "user_version", 2)
            .expect("version");
    }
    drop(Db::open(&data).expect("migrates"));
    let conn = inspect(&data);
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("user_version");
    assert_eq!(version, 6);
    let (items, with_format): (i64, i64) = conn
        .query_row(
            "SELECT count(*), count(format) FROM session_item",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("items");
    assert!(items >= 1);
    assert_eq!(with_format, 0, "existing items have no stored format");
    conn.execute("UPDATE session_item SET format = 'word_only'", [])
        .expect("valid format");
    assert!(
        conn.execute("UPDATE session_item SET format = 'bogus'", [])
            .is_err(),
        "CHECK enforced"
    );
    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
    assert_eq!(pragma_rows(&conn, "integrity_check"), vec!["Text(\"ok\")"]);
}

/// A database at version 4 (the schema before fix round R1) holding the
/// synthetic fixture.
fn v4_database(data: &Path) -> Connection {
    let conn = Connection::open(data.join(DB_FILE)).expect("create");
    conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
    conn.execute_batch(include_str!("../migrations/0001_initial.sql"))
        .expect("v1 schema");
    conn.execute_batch(FIXTURE).expect("fixture");
    for sql in [
        include_str!("../migrations/0002_word_spelling_nullable.sql"),
        include_str!("../migrations/0003_session_item_format.sql"),
        include_str!("../migrations/0004_auth_owner.sql"),
    ] {
        conn.execute_batch(sql).expect("migration");
    }
    conn.pragma_update(None, "user_version", 4)
        .expect("version");
    conn
}

/// Migration 0005 on a database at version 4: the new columns and the
/// throttle row exist, data is kept, constraints hold.
#[test]
fn r85_v4_database_with_data_migrates_to_v5() {
    let (_root, data) = data_dir();
    {
        let conn = v4_database(&data);
        conn.execute(
            "INSERT INTO source (id, slug, type, title, created_at, updated_at) \
             VALUES (2, 'test-untitled', 'book', 'test-untitled', 'x', 'x')",
            [],
        )
        .expect("source named after its slug");
    }
    drop(Db::open(&data).expect("migrates"));
    let conn = inspect(&data);
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("user_version");
    assert_eq!(version, 6);
    let defaulted: Vec<(String, bool)> = conn
        .prepare("SELECT slug, title_defaulted FROM source ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(
        defaulted,
        vec![
            ("test-fixture-book".to_owned(), false),
            ("test-untitled".to_owned(), true)
        ],
        "R-02: a title equal to the slug was a stand-in"
    );
    let (memory_updated, failures, throttle): (i64, i64, String) = conn
        .query_row(
            "SELECT (SELECT memory_updated FROM review_log WHERE id = 1), \
             (SELECT password_failures FROM auth_session), \
             (SELECT state_json FROM auth_throttle WHERE id = 1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("new columns");
    assert_eq!((memory_updated, failures, throttle.as_str()), (1, 0, "{}"));
    assert!(
        conn.execute("UPDATE review_log SET memory_updated = 2", [])
            .is_err(),
        "CHECK enforced"
    );
    assert!(
        conn.execute(
            "INSERT INTO auth_throttle (id, state_json) VALUES (2, '{}')",
            []
        )
        .is_err(),
        "one row only"
    );
    // The fixture sentence was re-keyed (its closing full stop is no longer
    // part of the key).
    let key: String = conn
        .query_row("SELECT en_key FROM sentence WHERE id = 1", [], |r| r.get(0))
        .expect("key");
    assert_eq!(key, "the glimmerwick on the shelf flickered twice");
    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
    assert_eq!(pragma_rows(&conn, "integrity_check"), vec!["Text(\"ok\")"]);
}

/// R-22/R-53 (migration 0005): sentences that differ only in what the new
/// key folds are merged into the oldest row with the newest text (hidden if
/// a copy was); reviews and practice items that showed a copy point to the
/// kept row; other words are untouched.
#[test]
fn r22_migration_rekeys_sentences_and_merges_duplicates_safely() {
    let (_root, data) = data_dir();
    {
        let conn = v4_database(&data);
        // Word 1 has sentence 1 (fixture). Added: copies of it as another
        // export might have punctuated them, a pair that differs in its
        // dash, and a second word with the same text (must stay separate).
        conn.execute_batch(
            "INSERT INTO sentence (id, word_id, en_marked, cs_marked, en_key, origin, position, hidden, created_at, updated_at) VALUES
               (2, 1, 'The [[glimmerwick]] on the shelf flickered twice', 'Druhá [[lampička]].', 'the glimmerwick on the shelf flickered twice', 'example', 1, 1, 'x', 'x'),
               (3, 1, 'The [[glimmerwick]] on the shelf flickered twice\u{2026}', 'Třetí [[lampička]].', 'the glimmerwick on the shelf flickered twice\u{2026}x', 'example', 2, 0, 'x', 'x'),
               (4, 1, 'A different [[glimmerwick]] \u{2014} quite dark.', 'Jiná [[lampička]].', 'a different glimmerwick \u{2014} quite dark.', 'example', 3, 0, 'x', 'x'),
               (5, 1, 'A different [[glimmerwick]] - quite dark', 'Jiná [[lampička]] znovu.', 'a different glimmerwick - quite dark', 'example', 4, 0, 'x', 'x');
             INSERT INTO word (id, headword, headword_key, meaning, usefulness, created_at, updated_at)
               VALUES (2, 'shelfling', 'shelfling', 'an invented word', 5, 'x', 'x');
             INSERT INTO sentence (id, word_id, en_marked, cs_marked, en_key, origin, position, created_at, updated_at) VALUES
               (6, 2, 'The glimmerwick on the [[shelf]] flickered twice!', 'Na [[polici]].', 'the glimmerwick on the shelf flickered twice!', 'example', 0, 'x', 'x'),
               (7, 2, 'Another [[shelf]] entirely.', 'Jiná [[police]].', 'the glimmerwick on the shelf flickered twice', 'example', 1, 'x', 'x');
             INSERT INTO review_log (id, client_review_id, card_id, session_id, reviewed_at, review_day, raw_grade,
                                     fsrs_rating, format, sentence_id, elapsed_days, before_json)
               VALUES (2, '6f1e2d3c-0000-4000-8000-000000000002', 1, '00112233445566778899aabbccddeeff', 'x',
                       '2026-11-17', 2, 3, 'sentence', 3, 0, '{}'),
                      (3, '6f1e2d3c-0000-4000-8000-000000000003', 1, '00112233445566778899aabbccddeeff', 'x',
                       '2026-11-17', 2, 3, 'sentence', 5, 0, '{}');
             INSERT INTO session_item (id, session_id, ord, card_id, origin, sentence_id)
               VALUES (20, '00112233445566778899aabbccddeeff', 2.0, 1, 'due', 2),
                      (21, '00112233445566778899aabbccddeeff', 3.0, 1, 'due', 6);",
        )
        .expect("colliding sentences");
    }
    drop(Db::open(&data).expect("migrates"));
    let conn = inspect(&data);
    let sentences: Vec<(i64, i64, String, i64)> = conn
        .prepare("SELECT id, word_id, en_key, hidden FROM sentence ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let row = |id, word, key: &str, hidden| (id, word, key.to_owned(), hidden);
    assert_eq!(
        sentences,
        vec![
            // A hidden copy (2) makes the merged sentence hidden.
            row(1, 1, "the glimmerwick on the shelf flickered twice", 1),
            row(4, 1, "a different glimmerwick-quite dark", 0),
            // Sentence 7's old key was the new key of sentence 6.
            row(6, 2, "the glimmerwick on the shelf flickered twice", 0),
            row(7, 2, "another shelf entirely", 0),
        ],
        "the oldest id of each group stays"
    );
    let texts: Vec<(String, String)> = conn
        .prepare("SELECT en_marked, cs_marked FROM sentence WHERE id IN (1, 4) ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(
        texts,
        vec![
            (
                // Same span: the English stays; the Czech is the newest.
                "The [[glimmerwick]] on the shelf flickered twice.".to_owned(),
                "Třetí [[lampička]].".to_owned()
            ),
            (
                "A different [[glimmerwick]] \u{2014} quite dark.".to_owned(),
                "Jiná [[lampička]] znovu.".to_owned()
            ),
        ],
        "the newest copy's text, as an import merge would have left it"
    );
    let pairs = |sql: &str| -> Vec<(i64, Option<i64>)> {
        conn.prepare(sql)
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    };
    assert_eq!(
        pairs("SELECT id, sentence_id FROM review_log ORDER BY id"),
        vec![(1, Some(1)), (2, Some(1)), (3, Some(4))],
        "reviews follow the kept sentence"
    );
    assert_eq!(
        pairs("SELECT id, sentence_id FROM session_item WHERE id >= 20 ORDER BY id"),
        vec![(20, Some(1)), (21, Some(6))]
    );
    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
    assert_eq!(pragma_rows(&conn, "integrity_check"), vec!["Text(\"ok\")"]);
    // Opening again changes nothing.
    drop(conn);
    drop(Db::open(&data).expect("reopen"));
    let count: i64 = inspect(&data)
        .query_row("SELECT count(*) FROM sentence", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 4);
}

/// R-22 (migration 0005): the owner's work wins a merge (an edited copy
/// gives the text, even an older one), another marked span is taken from
/// the newest copy, and punctuation-only sentences (empty key) are never
/// merged and keep their key.
#[test]
fn r22_migration_merge_keeps_the_owners_edits_and_leaves_empty_keys_alone() {
    let (_root, data) = data_dir();
    {
        let conn = v4_database(&data);
        conn.execute_batch(
            "INSERT INTO word (id, headword, headword_key, meaning, usefulness, created_at, updated_at) VALUES
               (2, 'test-edit', 'test-edit', 'invented', 5, 'x', 'x'),
               (3, 'test-span', 'test-span', 'invented', 5, 'x', 'x'),
               (4, 'test-dots', 'test-dots', 'invented', 5, 'x', 'x');
             INSERT INTO sentence (id, word_id, en_marked, cs_marked, en_key, origin, location, position, hidden, edited, created_at, updated_at) VALUES
               (10, 2, 'An [[edit]] here.', 'Owner [[edit]].', 'an edit here.', 'example', 'p. 1', 0, 0, 1, 'x', '2026-01-01'),
               (11, 2, 'An [[edit]] here', 'Pack [[text]].', 'an edit here', 'example', 'p. 2', 1, 0, 0, 'x', '2026-02-01'),
               (12, 3, 'Two [[spans]] here.', 'Dva [[úseky]].', 'two spans here.', 'example', NULL, 0, 0, 0, 'x', 'x'),
               (13, 3, '[[Two]] spans here', 'Dva úseky [[tady]].', 'two spans here', 'example', 'ch. 2', 1, 0, 0, 'x', 'x'),
               (14, 4, '[[...]]', 'a', '...', 'example', NULL, 0, 0, 0, 'x', 'x'),
               (15, 4, '[[!]]', 'b', '!', 'example', NULL, 1, 0, 0, 'x', 'x');",
        )
        .expect("sentences");
    }
    drop(Db::open(&data).expect("migrates"));
    let conn = inspect(&data);
    let rows: Vec<(i64, String, String, Option<String>, i64, String)> = conn
        .prepare(
            "SELECT id, en_marked, cs_marked, location, edited, en_key FROM sentence \
             WHERE id >= 10 ORDER BY id",
        )
        .expect("prepare")
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let row = |id, en: &str, cs: &str, location: Option<&str>, edited, key: &str| {
        (
            id,
            en.to_owned(),
            cs.to_owned(),
            location.map(str::to_owned),
            edited,
            key.to_owned(),
        )
    };
    assert_eq!(
        rows,
        vec![
            row(
                10,
                "An [[edit]] here.",
                "Owner [[edit]].",
                Some("p. 1"),
                1,
                "an edit here"
            ),
            row(
                12,
                "[[Two]] spans here",
                "Dva úseky [[tady]].",
                Some("ch. 2"),
                0,
                "two spans here"
            ),
            row(14, "[[...]]", "a", None, 0, "..."),
            row(15, "[[!]]", "b", None, 0, "!"),
        ]
    );
    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
}

/// Migration 0005 merges set-based: thousands of merges over many reviews
/// take well under the bound (the per-row version was quadratic: 10 000
/// merges with 100 000 reviews took more than a minute).
#[test]
fn r22_migration_merges_thousands_of_sentences_in_linear_time() {
    let (_root, data) = data_dir();
    {
        let mut conn = v4_database(&data);
        let tx = conn.transaction().expect("tx");
        let mut review = 100;
        for word in 10..3010 {
            tx.execute(
                "INSERT INTO word (id, headword, headword_key, meaning, usefulness, created_at, updated_at) \
                 VALUES (?1, 'test-w' || ?1, 'test-w' || ?1, 'invented', 5, 'x', 'x')",
                [word],
            )
            .expect("word");
            tx.execute(
                "INSERT INTO card (id, word_id, kind, state, created_at) VALUES (?1, ?1, 'understand', 'review', 'x')",
                [word],
            )
            .expect("card");
            for copy in 0..4i64 {
                let id = word * 10 + copy;
                let end = if copy % 2 == 0 { "." } else { "\u{2026}" };
                tx.execute(
                    "INSERT INTO sentence (id, word_id, en_marked, cs_marked, en_key, origin, position, created_at, updated_at) \
                     VALUES (?1, ?2, 'Line [[w]] ' || (?3 / 2) || ?4, 'cz', 'old' || ?1, 'example', ?3, 'x', 'x')",
                    rusqlite::params![id, word, copy, end],
                )
                .expect("sentence");
                for _ in 0..8 {
                    tx.execute(
                        "INSERT INTO review_log (id, client_review_id, card_id, reviewed_at, review_day, raw_grade, \
                         fsrs_rating, format, sentence_id, elapsed_days, before_json) \
                         VALUES (?1, 'r' || ?1, ?2, 'x', '2026-11-01', 2, 3, 'sentence', ?3, 1, '{}')",
                        rusqlite::params![review, word, id],
                    )
                    .expect("review");
                    review += 1;
                }
            }
        }
        tx.commit().expect("commit");
    }
    let started = std::time::Instant::now();
    drop(Db::open(&data).expect("migrates"));
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(20),
        "6000 merges over 96 000 reviews took {took:?}"
    );
    let conn = inspect(&data);
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).expect(sql) };
    assert_eq!(
        count("SELECT COUNT(*) FROM sentence WHERE word_id >= 10"),
        6000
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM review_log WHERE sentence_id >= 100 AND sentence_id % 2 = 1"),
        0,
        "every review points to the kept (even) id"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM review_log WHERE sentence_id IS NULL"),
        0
    );
    assert!(pragma_rows(&conn, "foreign_key_check").is_empty());
}

/// R-02 (migration 0005): v0.1.0 named an untitled general source
/// "General Vocabulary"; that title is a stand-in too.
#[test]
fn r02_migration_flags_general_vocabulary_as_a_defaulted_title() {
    let (_root, data) = data_dir();
    {
        let conn = v4_database(&data);
        conn.execute_batch(
            "INSERT INTO source (id, slug, type, title, created_at, updated_at) VALUES
               (2, 'test-general', 'general', 'General Vocabulary', 'x', 'x'),
               (3, 'test-other', 'book', 'General Vocabulary', 'x', 'x');",
        )
        .expect("sources");
    }
    drop(Db::open(&data).expect("migrates"));
    let flags: Vec<(String, bool)> = inspect(&data)
        .prepare("SELECT slug, title_defaulted FROM source ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(
        flags,
        vec![
            ("test-fixture-book".to_owned(), false),
            ("test-general".to_owned(), true),
            ("test-other".to_owned(), false),
        ]
    );
}
