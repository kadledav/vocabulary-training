//! Schema ↔ importer conformance (design §5.2, R-10, R-23).
//!
//! Every fixture in `testdata/packs/{valid,invalid}/` gets the same verdict
//! from `docs/vocab-pack.schema.json` (checked with the `jsonschema` crate)
//! and from [`vocab_pack::validate::validate`], except the documented
//! differences, which the file names state:
//!
//! * `valid/lenient-*`: unknown properties. The schema rejects them, the
//!   importer accepts the pack and reports them as warnings.
//! * `invalid/importer-only--*`: rules a JSON Schema cannot express (a
//!   headword repeated within a pack, control characters, a Czech
//!   translation without letters, `part.index > part.total`, an
//!   `exported_at` that is no calendar date). The schema accepts them, the
//!   importer does not.
//!
//! [`r23_schema_and_importer_agree_on_single_value_changes`] checks the same
//! agreement for a table of one-value changes to a valid pack (the cases a
//! review found the two disagreeing on, and their neighbours).
//!
//! The importer "accepts" a pack when it has no pack, word or sentence
//! errors (warnings are allowed).

use std::path::{Path, PathBuf};

use serde_json::Value;
use vocab_pack::paste::split_paste;
use vocab_pack::types::IssueCode;
use vocab_pack::validate::validate;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema_validator() -> jsonschema::Validator {
    let text = std::fs::read_to_string(repo_root().join("docs/vocab-pack.schema.json"))
        .expect("schema file");
    let schema: Value = serde_json::from_str(&text).expect("schema is JSON");
    jsonschema::draft202012::new(&schema).expect("schema compiles")
}

fn fixtures(kind: &str) -> Vec<(String, Value)> {
    let dir = repo_root().join("testdata/packs").join(kind);
    let mut out: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let name = path
                .file_stem()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path).expect("fixture");
            (name, serde_json::from_str(&text).expect("fixture is JSON"))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn schema_errors(validator: &jsonschema::Validator, value: &Value) -> Vec<String> {
    validator
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect()
}

#[test]
fn r10_valid_fixtures_are_accepted_by_schema_and_importer() {
    let validator = schema_validator();
    let fixtures = fixtures("valid");
    assert!(fixtures.len() >= 5, "valid fixtures present");
    for (name, value) in &fixtures {
        let result = validate(value);
        assert!(
            result.is_clean(),
            "{name}: importer rejects it: {result:#?}"
        );
        let unknown = result
            .all_warnings()
            .any(|w| w.code == IssueCode::UnknownProperty);
        if name.starts_with("lenient-") {
            assert!(unknown, "{name}: expected unknown-property warnings");
            assert!(
                !validator.is_valid(value),
                "{name}: the schema should reject unknown properties"
            );
        } else {
            assert!(!unknown, "{name}: unexpected unknown-property warning");
            assert_eq!(
                schema_errors(&validator, value),
                Vec::<String>::new(),
                "{name}: schema rejects it"
            );
        }
    }
}

#[test]
fn r23_invalid_fixtures_are_rejected_by_schema_and_importer() {
    let validator = schema_validator();
    let fixtures = fixtures("invalid");
    assert!(fixtures.len() >= 20, "invalid fixtures present");
    for (name, value) in &fixtures {
        let result = validate(value);
        assert!(!result.is_clean(), "{name}: importer accepts it");
        if name.starts_with("importer-only--") {
            assert_eq!(
                schema_errors(&validator, value),
                Vec::<String>::new(),
                "{name}: documented as a rule the schema cannot express"
            );
        } else {
            assert!(!validator.is_valid(value), "{name}: schema accepts it");
        }
    }
}

/// The example pack in `docs/claude-export-instructions.md` (what the owner
/// sees as "a minimal valid pack") passes both.
#[test]
fn r10_documented_example_pack_is_valid() {
    let text = std::fs::read_to_string(repo_root().join("docs/claude-export-instructions.md"))
        .expect("doc");
    let packs = split_paste(&text).expect("the doc contains a JSON block");
    assert_eq!(packs.len(), 1);
    let value = packs
        .into_iter()
        .next()
        .expect("pack")
        .expect("valid JSON")
        .value;
    let result = validate(&value);
    assert!(result.is_clean(), "{result:#?}");
    assert_eq!(result.all_warnings().count(), 0, "{result:#?}");
    assert!(schema_validator().is_valid(&value));
}

/// The word example in the instruction text Claude follows
/// (`docs/vocabulary-export-instructions.txt`) is a valid word.
#[test]
fn r14_instruction_text_word_example_is_valid() {
    let text = std::fs::read_to_string(repo_root().join("docs/vocabulary-export-instructions.txt"))
        .expect("instruction text");
    let start = text.find("## Word format").expect("word section");
    let rest = &text[start..];
    let open = rest.find("\n{").expect("word example") + 1;
    let close = rest[open..].find("\n}\n").expect("end of example") + open + 2;
    let word: Value = serde_json::from_str(&rest[open..close]).expect("example is JSON");
    let pack = serde_json::json!({
        "format": "vocab-pack",
        "format_version": 1,
        "pack_id": "test-book--2026-11-17--1",
        "exported_at": "2026-11-17",
        "part": { "index": 1, "total": 1 },
        "source": { "id": "test-book", "type": "book", "title": "Test Book" },
        "reading_position": "Chapter 14",
        "words": [word],
    });
    let result = validate(&pack);
    assert!(result.is_clean(), "{result:#?}");
    assert_eq!(result.all_warnings().count(), 0, "{result:#?}");
    assert!(schema_validator().is_valid(&pack));
}

/// A valid pack to change one value of.
fn base_pack() -> Value {
    serde_json::json!({
        "format": "vocab-pack", "format_version": 1, "pack_id": "test-book--2026-11-17--1",
        "exported_at": "2026-11-17", "part": {"index": 1, "total": 1},
        "source": {"id": "test-book", "type": "book", "title": "Test Book"},
        "reading_position": "Chapter 14",
        "words": [{
            "headword": "thud", "meaning": "A dull sound.", "czech": ["žuchnutí"], "usefulness": 7,
            "sentences": [
                {"en": "A loud [[thud]] came.", "cs": "Ozvalo se [[žuchnutí]].", "origin": "example", "location": "Chapter 12"},
                {"en": "Boots [[thudded]] down.", "cs": "Boty [[dusaly]] dolů.", "origin": "example"},
                {"en": "It fell with a [[thud]].", "cs": "Spadlo to s [[žuchnutím]].", "origin": "example"}
            ]
        }]
    })
}

fn set(pack: &mut Value, pointer: &str, value: Value) {
    let (parent, key) = pointer.rsplit_once('/').expect("pointer");
    match pack.pointer_mut(parent).expect("parent exists") {
        Value::Object(map) => {
            map.insert(key.to_owned(), value);
        }
        Value::Array(items) => items[key.parse::<usize>().expect("index")] = value,
        other => panic!("{pointer}: parent is {other}"),
    }
}

/// A8 (review finding "format agreement"): the schema and the importer give
/// the same verdict, and it is the documented one.
#[test]
fn r23_schema_and_importer_agree_on_single_value_changes() {
    use serde_json::json;
    let validator = schema_validator();
    assert!(validator.is_valid(&base_pack()) && validate(&base_pack()).is_clean());
    let null = Value::Null;
    let cases: Vec<(&str, Value, bool)> = vec![
        // null = not given, for every optional property.
        ("/source/title", null.clone(), true),
        ("/source/url", null.clone(), true),
        ("/exported_at", null.clone(), true),
        ("/part", null.clone(), true),
        ("/reading_position", null.clone(), true),
        ("/words/0/goal", null.clone(), true),
        ("/words/0/spelling", null.clone(), true),
        ("/words/0/forms", null.clone(), true),
        ("/words/0/accepted", null.clone(), true),
        ("/words/0/confusables", null.clone(), true),
        ("/words/0/tag", null.clone(), true),
        ("/words/0/icon", null.clone(), true),
        ("/words/0/seq", null.clone(), true),
        ("/words/0/sentences", null.clone(), true),
        ("/words/0/sentences/0/location", null.clone(), true),
        // … but not for required ones.
        ("/words/0/czech", null.clone(), false),
        ("/words/0/usefulness", null.clone(), false),
        ("/words/0/sentences/0/origin", null.clone(), false),
        ("/source/type", null.clone(), false),
        // Line breaks.
        ("/words/0/note", json!("line one\r\nline two"), true),
        ("/words/0/note", json!("line one\rline two"), true),
        (
            "/words/0/sentences/0/en",
            json!("A loud [[thud]]\r\ncame."),
            true,
        ),
        // Location: text or number.
        ("/words/0/sentences/0/location", json!(214), true),
        ("/words/0/sentences/0/location", json!(""), true),
        ("/words/0/sentences/0/location", json!(false), false),
        ("/words/0/sentences/0/location", json!(["12"]), false),
        // Blank required text, headword without a visible character.
        ("/words/0/meaning", json!("   "), false),
        ("/words/0/meaning", json!("\u{a0}"), false),
        ("/words/0/headword", json!("\u{200b}"), false),
        ("/words/0/headword", json!("\u{a0}"), false),
        ("/words/0/headword", json!("\u{feff}\u{ad}\u{2060}"), false),
        ("/words/0/headword", json!(" a\u{200b}"), true),
        // Target spans.
        (
            "/words/0/sentences/0/en",
            json!("A loud [[]thud]] came."),
            false,
        ),
        (
            "/words/0/sentences/0/en",
            json!("A loud [[\u{feff}thud]] came."),
            true,
        ),
        ("/words/0/sentences/0/en", json!("[[thud]]"), true),
        ("/words/0/sentences/0/cs", json!("[[3D]]"), true),
        // seq, numbers and types.
        ("/words/0/seq", json!(0), false),
        ("/words/0/seq", json!(1), true),
        ("/words/0/usefulness", json!("7"), false),
        ("/words/0/usefulness", json!(7.0), true),
        ("/source/series_number", json!("2"), false),
        ("/reading_position", json!(14), false),
        ("/words/0/pronunciation", json!(1), false),
        ("/words/0/czech", json!("žuchnutí, rána"), false),
        ("/words/0/czech", json!([" ", "žuchnutí"]), true),
        ("/words/0/forms", json!(["thud", "thudded"]), false),
        ("/words/0/tag", json!("low-priority"), false),
        ("/words/0/tag", json!({"kind": "skip"}), false),
        ("/words/0/sentences/0/origin", json!("Book"), false),
        ("/source/type", json!("movie"), false),
        ("/exported_at", json!("2026-11-17T10:00:00Z"), false),
        ("/exported_at", json!("2026-13-01"), false),
        // Source URL.
        ("/source/url", json!("https://test.example/a"), true),
        ("/source/url", json!("http://test.example/a"), true),
        ("/source/url", json!(""), true),
        ("/source/url", json!("javascript:alert(1)"), false),
        ("/source/url", json!("test.example/a"), false),
        // Advice only.
        ("/words/0/hint", json!("x".repeat(121)), true),
        ("/words/0/spelling_note", json!("one s"), true),
    ];
    for (pointer, value, accepted) in cases {
        let mut pack = base_pack();
        set(&mut pack, pointer, value.clone());
        let schema = validator.is_valid(&pack);
        let importer = validate(&pack).is_clean();
        assert_eq!(
            (schema, importer),
            (accepted, accepted),
            "{pointer} = {value}: (schema, importer) accept"
        );
    }
}

/// A1: the examples Claude copies never carry `"tag": null` (a re-export
/// that copied it used to un-skip low-priority words), and the text says
/// what to do instead.
#[test]
fn r03_instruction_examples_leave_the_tag_out_when_there_is_none() {
    for doc in [
        "docs/vocabulary-export-instructions.txt",
        "docs/claude-export-instructions.md",
    ] {
        let text = std::fs::read_to_string(repo_root().join(doc)).expect("doc");
        let json_lines: Vec<&str> = text
            .lines()
            .filter(|line| line.trim_start().starts_with("\"tag\""))
            .collect();
        assert_eq!(json_lines, Vec::<&str>::new(), "{doc}");
    }
    let text = std::fs::read_to_string(repo_root().join("docs/vocabulary-export-instructions.txt"))
        .expect("instruction text");
    assert!(text.contains("omit `tag` when there is none"));
}

/// A8 (R-13): the instruction text maps every key of the owner's old pages
/// (requirements R-01) to a pack field, with the three old tag values, and
/// the tag kinds it names are the format's.
#[test]
fn r13_instruction_text_maps_every_key_of_the_old_pages() {
    let text = std::fs::read_to_string(repo_root().join("docs/vocabulary-export-instructions.txt"))
        .expect("instruction text");
    let start = text
        .find("## Converting my old pages")
        .expect("old-page section");
    let section = &text[start..];
    for (old_key, field) in [
        ("`w`", "`headword`"),
        ("`m`", "`meaning`"),
        ("`cz`", "`czech`"),
        ("`forms`", "`forms`"),
        ("`s`", "`pronunciation`"),
        ("`say`", "`speak`"),
        ("`u`", "\"example\""),
        ("`n`", "`note`"),
        ("`ux`", "`usefulness`"),
        ("`ic`", "`icon`"),
        ("`em`", "`icon`"),
        ("`seq`", "`seq`"),
        ("`tag`", "`tag`"),
        ("`tagtext`", "`\"text\"`"),
    ] {
        let line = section
            .lines()
            .find(|line| line.starts_with("- ") && line.contains(old_key) && line.contains(field))
            .unwrap_or_else(|| panic!("no line maps {old_key} to {field}"));
        assert!(line.contains('→'), "{line}");
    }
    for (old_tag, kind) in [
        ("`skip`", "low-priority"),
        ("`rep`", "seen-before"),
        ("`late`", "not-from-source"),
    ] {
        assert!(
            section.contains(&format!("{old_tag} → {{ \"kind\": \"{kind}\" }}")),
            "{old_tag} → {kind}"
        );
        assert!(vocab_pack::types::TagKind::parse(kind).is_some());
    }
    assert!(section.contains("keep the text in `note`"));
}
