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
//!   translation without letters, `part.index > part.total`). The schema
//!   accepts them, the importer does not.
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
/// (`docs/claude-project-instructions.txt`) is a valid word.
#[test]
fn r14_instruction_text_word_example_is_valid() {
    let text = std::fs::read_to_string(repo_root().join("docs/claude-project-instructions.txt"))
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
