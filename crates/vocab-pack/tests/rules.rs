//! One test per validation rule of design §5.2, checking the scope of each
//! finding (pack / word / sentence) and its JSON pointer. Synthetic data only.

use serde_json::{Value, json};
use vocab_pack::types::{
    Goal, Issue, IssueCode, Origin, PackValidation, PackWord, SourceType, TagKind, WordResult,
};
use vocab_pack::validate::validate;

fn sentence(n: usize) -> Value {
    json!({
        "en": format!("Test sentence number {n} has a [[hum]] in it."),
        "cs": format!("Testovací věta číslo {n} v sobě má [[bzukot]]."),
        "origin": "example"
    })
}

fn word(headword: &str) -> Value {
    json!({
        "headword": headword,
        "meaning": "A low steady sound.",
        "czech": ["bzukot"],
        "usefulness": 6,
        "sentences": [sentence(1), sentence(2), sentence(3)]
    })
}

fn pack() -> Value {
    json!({
        "format": "vocab-pack",
        "format_version": 1,
        "pack_id": "test-book--2026-11-17--1",
        "exported_at": "2026-11-17",
        "part": { "index": 1, "total": 1 },
        "source": { "id": "test-book", "type": "book", "title": "Test Book" },
        "reading_position": "Chapter 2",
        "words": [word("hum"), word("glint")]
    })
}

/// Set (or add) the value at `pointer`.
fn with(mut value: Value, pointer: &str, new: Value) -> Value {
    if let Some(slot) = value.pointer_mut(pointer) {
        *slot = new;
        return value;
    }
    let (parent, key) = pointer.rsplit_once('/').expect("pointer");
    value
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("{pointer}: parent is not an object"))
        .insert(key.to_owned(), new);
    value
}

fn without(mut value: Value, pointer: &str) -> Value {
    let (parent, key) = pointer.rsplit_once('/').expect("pointer");
    value
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .expect("parent object")
        .remove(key);
    value
}

fn pack_errors(result: &PackValidation) -> &[Issue] {
    result.pack.as_ref().err().map_or(&[], Vec::as_slice)
}

fn assert_pack_error(value: &Value, pointer: &str, code: IssueCode) {
    let result = validate(value);
    let errors = pack_errors(&result);
    assert!(
        errors
            .iter()
            .any(|e| e.pointer == pointer && e.code == code),
        "expected pack error {code:?} at {pointer}, got {errors:#?}"
    );
}

fn valid_word(result: &PackValidation, index: usize) -> &PackWord {
    match &result.words[index] {
        WordResult::Valid(word) => word,
        WordResult::Invalid(word) => panic!("word {index} invalid: {word:#?}"),
    }
}

/// The word at `index` is dropped with `code` at `pointer`; the pack and the
/// other word survive.
fn assert_word_error(value: &Value, index: usize, pointer: &str, code: IssueCode) {
    let result = validate(value);
    assert!(result.pack.is_ok(), "{:#?}", result.pack);
    match &result.words[index] {
        WordResult::Invalid(word) => assert!(
            word.errors
                .iter()
                .any(|e| e.pointer == pointer && e.code == code),
            "expected word error {code:?} at {pointer}, got {:#?}",
            word.errors
        ),
        WordResult::Valid(_) => panic!("word {index} should be invalid ({pointer})"),
    }
    let other = 1 - index;
    assert!(matches!(result.words[other], WordResult::Valid(_)));
}

/// Sentence `s` of word 0 is dropped with `code`; the word keeps the others.
fn assert_sentence_error(value: &Value, s: usize, field: &str, code: IssueCode) {
    let result = validate(value);
    let word = valid_word(&result, 0);
    let pointer = format!("/words/0/sentences/{s}{field}");
    assert!(
        word.sentence_errors
            .iter()
            .any(|e| e.pointer == pointer && e.code == code),
        "expected sentence error {code:?} at {pointer}, got {:#?}",
        word.sentence_errors
    );
    assert_eq!(word.sentences.len(), 2, "the other sentences stay");
    assert!(!result.is_clean());
}

fn has_warning(issues: &[Issue], pointer: &str, code: IssueCode) -> bool {
    issues
        .iter()
        .any(|w| w.pointer == pointer && w.code == code)
}

// ── a complete, valid pack ───────────────────────────────────────────────

#[test]
fn r10_valid_pack_is_typed_and_normalised() {
    let mut value = pack();
    value["words"][0] = json!({
        "headword": "  Cafe\u{301} Au Lait ",
        "grammar": "noun",
        "forms": [{ "form": "café au lait", "common": true }],
        "pronunciation": "ka-FAY oh LAY",
        "speak": "café au lait",
        "meaning": "Coffee with hot milk.",
        "czech": ["bílá káva"],
        "usefulness": 4,
        "goal": "recognise",
        "accepted": ["café au lait"],
        "confusables": ["latte"],
        "hint": "Half coffee, half milk.",
        "spelling": true,
        "spelling_note": "accent on e",
        "note": "",
        "tag": { "kind": "seen-before", "text": "also elsewhere" },
        "icon": "☕",
        "seq": 7,
        "sentences": [sentence(1), sentence(2), sentence(3)]
    });
    let result = validate(&value);
    assert!(result.is_clean(), "{result:#?}");
    let header = result.pack.as_ref().expect("header");
    assert_eq!(header.pack_id, "test-book--2026-11-17--1");
    assert_eq!(header.pack_date, "2026-11-17");
    assert_eq!(header.pack_number, 1);
    assert_eq!(header.source.kind, SourceType::Book);
    assert_eq!(header.reading_position.as_deref(), Some("Chapter 2"));
    let w = valid_word(&result, 0);
    assert_eq!(w.headword, "Caf\u{e9} Au Lait", "trimmed and NFC");
    assert_eq!(w.key, "caf\u{e9} au lait");
    assert_eq!(w.goal, Some(Goal::Recognise));
    assert_eq!(w.tag.tag().map(|t| t.kind), Some(TagKind::SeenBefore));
    assert_eq!(w.seq, Some(7));
    assert_eq!(w.spelling, Some(true));
    assert_eq!(w.note, None, "blank text counts as absent");
    assert_eq!(w.sentences.len(), 3);
    assert_eq!(w.sentences[0].origin, Origin::Example);
    assert!(w.sentences[0].en.contains("[[hum]]"), "stored with markers");
}

// ── pack scope ───────────────────────────────────────────────────────────

#[test]
fn r84_pack_must_be_an_object() {
    assert_pack_error(&json!([pack()]), "", IssueCode::Invalid);
    assert_pack_error(&json!("text"), "", IssueCode::Invalid);
}

#[test]
fn r10_format_must_be_vocab_pack() {
    assert_pack_error(
        &with(pack(), "/format", json!("vocab")),
        "/format",
        IssueCode::Invalid,
    );
    assert_pack_error(&without(pack(), "/format"), "/format", IssueCode::Missing);
}

#[test]
fn r10_unknown_format_version_is_rejected_with_advice() {
    let result = validate(&with(pack(), "/format_version", json!(2)));
    let errors = pack_errors(&result);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, IssueCode::UnknownVersion);
    assert!(
        errors[0].message.contains("format 2"),
        "{}",
        errors[0].message
    );
    assert!(errors[0].message.contains("instructions"));
    assert!(
        result.words.is_empty(),
        "words of an unknown format are not read"
    );
    assert_pack_error(
        &with(pack(), "/format_version", json!("1")),
        "/format_version",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &without(pack(), "/format_version"),
        "/format_version",
        IssueCode::Missing,
    );
}

#[test]
fn r10_pack_id_must_match_the_pattern() {
    for bad in [
        "test-book--2026-11-17",
        "test-book--2026-11-17--0",
        "Test--2026-11-17--1",
        "",
    ] {
        assert_pack_error(
            &with(pack(), "/pack_id", json!(bad)),
            "/pack_id",
            IssueCode::Invalid,
        );
    }
    assert_pack_error(&without(pack(), "/pack_id"), "/pack_id", IssueCode::Missing);
}

#[test]
fn r02_source_must_be_valid() {
    assert_pack_error(&without(pack(), "/source"), "/source", IssueCode::Missing);
    assert_pack_error(
        &with(pack(), "/source/id", json!("test_book")),
        "/source/id",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &with(pack(), "/source/type", json!("podcast")),
        "/source/type",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &with(pack(), "/source/title", json!("")),
        "/source/title",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &with(pack(), "/source/title", json!("t".repeat(301))),
        "/source/title",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &with(pack(), "/source/series_number", json!(0)),
        "/source/series_number",
        IssueCode::Invalid,
    );
    let full = with(
        pack(),
        "/source",
        json!({ "id": "test-book", "type": "series", "title": "T", "author": "A", "series": "S", "series_number": 3, "url": "https://example.org/a" }),
    );
    let result = validate(&full);
    let source = &result.pack.as_ref().expect("valid").source;
    assert_eq!(source.series_number, Some(3));
    assert_eq!(source.url.as_deref(), Some("https://example.org/a"));
}

#[test]
fn r84_words_must_hold_1_to_100_items() {
    assert_pack_error(
        &with(pack(), "/words", json!([])),
        "/words",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &with(pack(), "/words", json!({})),
        "/words",
        IssueCode::Invalid,
    );
    let many: Vec<Value> = (0..101).map(|i| word(&format!("w{i}"))).collect();
    assert_pack_error(
        &with(pack(), "/words", json!(many)),
        "/words",
        IssueCode::Invalid,
    );
    let max: Vec<Value> = (0..100).map(|i| word(&format!("w{i}"))).collect();
    assert!(validate(&with(pack(), "/words", json!(max))).is_clean());
}

#[test]
fn r59_exported_at_must_be_a_date_if_present() {
    for bad in [
        json!("17.11.2026"),
        json!("2026-11-32"),
        json!(null),
        json!(20261117),
    ] {
        assert_pack_error(
            &with(pack(), "/exported_at", bad),
            "/exported_at",
            IssueCode::Invalid,
        );
    }
    let result = validate(&without(pack(), "/exported_at"));
    assert_eq!(result.pack.expect("valid").exported_at, None);
}

#[test]
fn r13_part_index_must_not_exceed_total() {
    assert_pack_error(
        &with(pack(), "/part", json!({ "index": 2, "total": 1 })),
        "/part/index",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &with(pack(), "/part", json!({ "index": 0, "total": 1 })),
        "/part/index",
        IssueCode::Invalid,
    );
    assert_pack_error(
        &with(pack(), "/part", json!({ "index": 1 })),
        "/part/total",
        IssueCode::Missing,
    );
    assert_pack_error(
        &with(pack(), "/part", json!(1)),
        "/part",
        IssueCode::Invalid,
    );
}

#[test]
fn r13_missing_part_is_a_warning_and_defaults_to_one_of_one() {
    let result = validate(&without(pack(), "/part"));
    assert!(has_warning(&result.warnings, "/part", IssueCode::Advice));
    let part = result.pack.expect("valid").part;
    assert_eq!((part.index, part.total), (1, 1));
}

#[test]
fn r10_pack_warnings_prefix_and_reading_position() {
    let result = validate(&with(
        pack(),
        "/pack_id",
        json!("test-other--2026-11-17--1"),
    ));
    assert!(result.pack.is_ok());
    assert!(has_warning(&result.warnings, "/pack_id", IssueCode::Advice));

    let film = with(pack(), "/source/type", json!("film"));
    let result = validate(&film);
    assert!(result.pack.is_ok());
    assert!(has_warning(
        &result.warnings,
        "/reading_position",
        IssueCode::Advice
    ));

    let result = validate(&with(film, "/reading_position", json!(null)));
    assert!(result.warnings.is_empty(), "{:#?}", result.warnings);
}

#[test]
fn r10_unknown_properties_are_warnings_everywhere() {
    let mut value = pack();
    value["extra"] = json!(1);
    value["source"]["x/y"] = json!(1);
    value["words"][0]["colour"] = json!("blue");
    value["words"][0]["sentences"][0]["speaker"] = json!("x");
    let result = validate(&value);
    assert!(result.is_clean(), "{result:#?}");
    assert!(has_warning(
        &result.warnings,
        "/extra",
        IssueCode::UnknownProperty
    ));
    assert!(has_warning(
        &result.warnings,
        "/source/x~1y",
        IssueCode::UnknownProperty
    ));
    let w = valid_word(&result, 0);
    assert!(has_warning(
        &w.warnings,
        "/words/0/colour",
        IssueCode::UnknownProperty
    ));
    assert!(has_warning(
        &w.warnings,
        "/words/0/sentences/0/speaker",
        IssueCode::UnknownProperty
    ));
}

#[test]
fn r84_control_characters_reject_the_scope_they_occur_in() {
    // Pack scope.
    assert_pack_error(
        &with(pack(), "/source/title", json!("Bad\u{1b}[31m title")),
        "/source/title",
        IssueCode::ControlCharacter,
    );
    // Word scope.
    assert_word_error(
        &with(pack(), "/words/1/meaning", json!("Line\rbreak")),
        1,
        "/words/1/meaning",
        IssueCode::ControlCharacter,
    );
    // Sentence scope.
    assert_sentence_error(
        &with(pack(), "/words/0/sentences/1/en", json!("A [[hum]]\u{0}.")),
        1,
        "/en",
        IssueCode::ControlCharacter,
    );
    // Newlines and tabs are fine.
    let ok = with(pack(), "/words/0/note", json!("line 1\n\tline 2"));
    assert!(validate(&ok).is_clean());
}

// ── word scope ───────────────────────────────────────────────────────────

#[test]
fn r01_headword_must_be_1_to_100_chars_and_not_blank() {
    assert_word_error(
        &with(pack(), "/words/0/headword", json!("  ")),
        0,
        "/words/0/headword",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/headword", json!("")),
        0,
        "/words/0/headword",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/headword", json!("h".repeat(101))),
        0,
        "/words/0/headword",
        IssueCode::Invalid,
    );
    assert_word_error(
        &without(pack(), "/words/0/headword"),
        0,
        "/words/0/headword",
        IssueCode::Missing,
    );
}

#[test]
fn r01_meaning_is_required_and_at_most_1500_chars() {
    assert_word_error(
        &without(pack(), "/words/1/meaning"),
        1,
        "/words/1/meaning",
        IssueCode::Missing,
    );
    assert_word_error(
        &with(pack(), "/words/1/meaning", json!("m".repeat(1501))),
        1,
        "/words/1/meaning",
        IssueCode::Invalid,
    );
}

#[test]
fn r01_czech_needs_at_least_one_item_of_1_to_200_chars() {
    assert_word_error(
        &with(pack(), "/words/0/czech", json!([])),
        0,
        "/words/0/czech",
        IssueCode::Invalid,
    );
    assert_word_error(
        &without(pack(), "/words/0/czech"),
        0,
        "/words/0/czech",
        IssueCode::Missing,
    );
    assert_word_error(
        &with(pack(), "/words/0/czech", json!([""])),
        0,
        "/words/0/czech/0",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/czech", json!(["c".repeat(201)])),
        0,
        "/words/0/czech/0",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/czech", json!("bzukot")),
        0,
        "/words/0/czech",
        IssueCode::Invalid,
    );
}

#[test]
fn r01_usefulness_must_be_an_integer_from_1_to_10() {
    for bad in [json!(0), json!(11), json!(6.5), json!("6"), json!(null)] {
        assert_word_error(
            &with(pack(), "/words/0/usefulness", bad),
            0,
            "/words/0/usefulness",
            IssueCode::Invalid,
        );
    }
    assert_word_error(
        &without(pack(), "/words/0/usefulness"),
        0,
        "/words/0/usefulness",
        IssueCode::Missing,
    );
}

#[test]
fn r15_goal_must_be_recognise_or_active() {
    assert_word_error(
        &with(pack(), "/words/0/goal", json!("produce")),
        0,
        "/words/0/goal",
        IssueCode::Invalid,
    );
    let result = validate(&with(pack(), "/words/0/goal", json!("active")));
    assert_eq!(valid_word(&result, 0).goal, Some(Goal::Active));
    // Absent: derived later from usefulness (R-15).
    assert_eq!(valid_word(&validate(&pack()), 0).goal, None);
}

#[test]
fn r01_forms_items_need_form_and_common() {
    assert_word_error(
        &with(pack(), "/words/0/forms", json!([{ "form": "hums" }])),
        0,
        "/words/0/forms/0/common",
        IssueCode::Missing,
    );
    assert_word_error(
        &with(
            pack(),
            "/words/0/forms",
            json!([{ "form": "", "common": true }]),
        ),
        0,
        "/words/0/forms/0/form",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/forms", json!(["hums"])),
        0,
        "/words/0/forms/0",
        IssueCode::Invalid,
    );
}

#[test]
fn r01_tag_is_null_or_a_known_kind() {
    assert_word_error(
        &with(pack(), "/words/0/tag", json!({ "kind": "skip" })),
        0,
        "/words/0/tag/kind",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/tag", json!({ "text": "x" })),
        0,
        "/words/0/tag/kind",
        IssueCode::Missing,
    );
    assert_word_error(
        &with(pack(), "/words/0/tag", json!("low-priority")),
        0,
        "/words/0/tag",
        IssueCode::Invalid,
    );
    assert!(validate(&with(pack(), "/words/0/tag", json!(null))).is_clean());
    for kind in TagKind::ALL {
        assert!(validate(&with(pack(), "/words/0/tag", json!({ "kind": kind }))).is_clean());
    }
}

#[test]
fn r84_lists_are_limited_to_50_items() {
    let items: Vec<String> = (0..51).map(|i| format!("a{i}")).collect();
    for field in ["czech", "accepted", "confusables"] {
        let pointer = format!("/words/0/{field}");
        assert_word_error(
            &with(pack(), &pointer, json!(items)),
            0,
            &pointer,
            IssueCode::Invalid,
        );
    }
    let forms: Vec<Value> = (0..51)
        .map(|i| json!({ "form": format!("f{i}"), "common": true }))
        .collect();
    assert_word_error(
        &with(pack(), "/words/0/forms", json!(forms)),
        0,
        "/words/0/forms",
        IssueCode::Invalid,
    );
}

#[test]
fn r84_text_fields_have_length_limits_and_must_be_strings_or_null() {
    for (field, max) in [
        ("note", 6000),
        ("hint", 300),
        ("grammar", 200),
        ("pronunciation", 200),
        ("speak", 200),
        ("spelling_note", 200),
        ("icon", 40),
    ] {
        let pointer = format!("/words/0/{field}");
        assert_word_error(
            &with(pack(), &pointer, json!("x".repeat(max + 1))),
            0,
            &pointer,
            IssueCode::Invalid,
        );
        assert_word_error(
            &with(pack(), &pointer, json!(5)),
            0,
            &pointer,
            IssueCode::Invalid,
        );
        let ok = with(pack(), &pointer, json!(null));
        assert!(validate(&ok).is_clean(), "{field}: null allowed");
    }
}

#[test]
fn r01_seq_is_an_optional_positive_integer() {
    assert_word_error(
        &with(pack(), "/words/0/seq", json!(0)),
        0,
        "/words/0/seq",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/seq", json!("3")),
        0,
        "/words/0/seq",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/seq", json!(1_000_001)),
        0,
        "/words/0/seq",
        IssueCode::Invalid,
    );
    let result = validate(&with(pack(), "/words/0/seq", json!(12)));
    assert_eq!(valid_word(&result, 0).seq, Some(12));
}

#[test]
fn r03_repeated_headword_in_one_pack_drops_the_second_occurrence() {
    let value = with(pack(), "/words/1/headword", json!(" HUM "));
    let result = validate(&value);
    assert!(result.pack.is_ok());
    assert!(matches!(result.words[0], WordResult::Valid(_)));
    match &result.words[1] {
        WordResult::Invalid(w) => {
            assert_eq!(w.errors[0].code, IssueCode::DuplicateWord);
            assert_eq!(w.errors[0].pointer, "/words/1/headword");
            assert_eq!(w.headword.as_deref(), Some("HUM"));
        }
        WordResult::Valid(_) => panic!("duplicate accepted"),
    }
}

#[test]
fn r15_word_warnings_spelling_note_few_sentences_long_hint() {
    let mut value = pack();
    value["words"][0]["spelling_note"] = json!("double m");
    value["words"][0]["hint"] = json!("h".repeat(121));
    value["words"][0]["sentences"] = json!([sentence(1)]);
    let result = validate(&value);
    assert!(result.is_clean());
    let w = valid_word(&result, 0);
    assert!(has_warning(
        &w.warnings,
        "/words/0/spelling_note",
        IssueCode::Advice
    ));
    assert!(has_warning(&w.warnings, "/words/0/hint", IssueCode::Advice));
    assert!(has_warning(
        &w.warnings,
        "/words/0/sentences",
        IssueCode::FewSentences
    ));
    // spelling: true silences the spelling_note warning.
    value["words"][0]["spelling"] = json!(true);
    let result = validate(&value);
    assert!(!has_warning(
        &valid_word(&result, 0).warnings,
        "/words/0/spelling_note",
        IssueCode::Advice
    ));
}

#[test]
fn r20_too_many_sentences_is_a_word_error() {
    let many: Vec<Value> = (0..21).map(sentence).collect();
    assert_word_error(
        &with(pack(), "/words/0/sentences", json!(many)),
        0,
        "/words/0/sentences",
        IssueCode::Invalid,
    );
    assert_word_error(
        &with(pack(), "/words/0/sentences", json!("x")),
        0,
        "/words/0/sentences",
        IssueCode::Invalid,
    );
}

// ── sentence scope ───────────────────────────────────────────────────────

#[test]
fn r23_sentence_with_two_spans_is_rejected() {
    assert_sentence_error(
        &with(
            pack(),
            "/words/0/sentences/0/en",
            json!("A [[hum]] and a [[buzz]]."),
        ),
        0,
        "/en",
        IssueCode::Marker,
    );
    assert_sentence_error(
        &with(
            pack(),
            "/words/0/sentences/2/cs",
            json!("[[Bzukot]] a [[bzučení]]."),
        ),
        2,
        "/cs",
        IssueCode::Marker,
    );
}

#[test]
fn r23_target_span_must_be_present_non_empty_and_trimmed() {
    for bad in [
        "A hum.",
        "A [[]] hum.",
        "A [[ hum]].",
        "A [[hum ]].",
        "A [[hum.",
        "A hum]] [[x]].",
    ] {
        assert_sentence_error(
            &with(pack(), "/words/0/sentences/1/en", json!(bad)),
            1,
            "/en",
            IssueCode::Marker,
        );
    }
}

#[test]
fn r23_translation_must_contain_a_letter() {
    assert_sentence_error(
        &with(pack(), "/words/0/sentences/0/cs", json!("[[…]] 42.")),
        0,
        "/cs",
        IssueCode::Invalid,
    );
    assert_sentence_error(
        &without(pack(), "/words/0/sentences/0/cs"),
        0,
        "/cs",
        IssueCode::Missing,
    );
}

#[test]
fn r20_sentence_lengths_origin_and_location() {
    assert_sentence_error(
        &with(
            pack(),
            "/words/0/sentences/0/en",
            json!(format!("[[x]]{}", "y".repeat(596))),
        ),
        0,
        "/en",
        IssueCode::Invalid,
    );
    assert_sentence_error(
        &with(pack(), "/words/0/sentences/0/origin", json!("quote")),
        0,
        "/origin",
        IssueCode::Invalid,
    );
    assert_sentence_error(
        &without(pack(), "/words/0/sentences/0/origin"),
        0,
        "/origin",
        IssueCode::Missing,
    );
    assert_sentence_error(
        &with(
            pack(),
            "/words/0/sentences/0/location",
            json!("l".repeat(101)),
        ),
        0,
        "/location",
        IssueCode::Invalid,
    );
    let mut value = pack();
    value["words"][0]["sentences"][0]["origin"] = json!("book");
    value["words"][0]["sentences"][0]["location"] = json!("Chapter 12");
    value["words"][0]["sentences"][1]["origin"] = json!("source");
    let result = validate(&value);
    let w = valid_word(&result, 0);
    assert_eq!(w.sentences[0].origin, Origin::Book);
    assert_eq!(w.sentences[0].location.as_deref(), Some("Chapter 12"));
    assert_eq!(w.sentences[1].origin, Origin::Source);
}

#[test]
fn r22_duplicate_sentence_in_one_word_is_a_warning_and_dropped() {
    let mut value = pack();
    value["words"][0]["sentences"][2]["en"] = json!("…TEST SENTENCE NUMBER 1 has a hum [[in]] it.");
    let result = validate(&value);
    assert!(result.is_clean());
    let w = valid_word(&result, 0);
    assert_eq!(w.sentences.len(), 2);
    assert!(has_warning(
        &w.warnings,
        "/words/0/sentences/2",
        IssueCode::DuplicateSentence
    ));
}

#[test]
fn r23_errors_in_every_scope_are_all_listed_at_once() {
    let mut value = pack();
    value["source"]["type"] = json!("podcast");
    value["words"][0]["usefulness"] = json!(0);
    value["words"][1]["sentences"][0]["en"] = json!("no marker");
    let result = validate(&value);
    assert_eq!(pack_errors(&result).len(), 1);
    assert!(matches!(result.words[0], WordResult::Invalid(_)));
    assert_eq!(valid_word(&result, 1).sentence_errors.len(), 1);
}

// ── M2 fix round ─────────────────────────────────────────────────────────

#[test]
fn r02_source_title_is_optional() {
    // The exact shape of a real export from claude.ai.
    let value = with(
        pack(),
        "/source",
        json!({ "id": "general", "type": "general" }),
    );
    let result = validate(&with(value, "/pack_id", json!("general--2026-11-17--1")));
    let value_without_reading = result.pack.as_ref().expect("accepted");
    assert_eq!(value_without_reading.source.title, None);
    // Present but empty is still an error (schema minLength 1); blank = absent.
    assert_pack_error(
        &with(pack(), "/source/title", json!("")),
        "/source/title",
        IssueCode::Invalid,
    );
    let blank = validate(&with(pack(), "/source/title", json!("  ")));
    assert_eq!(blank.pack.expect("accepted").source.title, None);
}

#[test]
fn r01_blank_list_items_are_dropped_with_a_warning() {
    let mut value = pack();
    value["words"][0]["czech"] = json!(["bzukot", " "]);
    value["words"][0]["accepted"] = json!(["\t", "hum"]);
    value["words"][0]["confusables"] = json!([" "]);
    value["words"][0]["forms"] =
        json!([{ "form": " ", "common": true }, { "form": "hums", "common": true }]);
    let result = validate(&value);
    assert!(result.is_clean(), "{result:#?}");
    let w = valid_word(&result, 0);
    assert_eq!(w.czech, ["bzukot"]);
    assert_eq!(w.accepted, ["hum"]);
    assert!(w.confusables.is_empty());
    assert_eq!(w.forms.len(), 1);
    for pointer in [
        "/words/0/czech/1",
        "/words/0/accepted/0",
        "/words/0/confusables/0",
        "/words/0/forms/0/form",
    ] {
        assert!(
            has_warning(&w.warnings, pointer, IssueCode::Advice),
            "{pointer}: {:#?}",
            w.warnings
        );
    }
    // Only blank Czech equivalents: nothing left, the word is dropped.
    assert_word_error(
        &with(pack(), "/words/0/czech", json!([" ", "\t"])),
        0,
        "/words/0/czech",
        IssueCode::Invalid,
    );
}

#[test]
fn r01_absent_tag_and_null_tag_differ() {
    use vocab_pack::types::TagField;
    let absent = validate(&pack());
    assert_eq!(valid_word(&absent, 0).tag, TagField::Absent);
    let null = validate(&with(pack(), "/words/0/tag", json!(null)));
    assert_eq!(valid_word(&null, 0).tag, TagField::Null);
    let set = validate(&with(
        pack(),
        "/words/0/tag",
        json!({ "kind": "seen-before" }),
    ));
    assert_eq!(
        valid_word(&set, 0).tag.tag().map(|t| t.kind),
        Some(TagKind::SeenBefore)
    );
}

#[test]
fn r84_reports_stay_small_for_hostile_input() {
    // 10 000 unknown properties: ten warnings plus one summary.
    let mut value = pack();
    for i in 0..10_000 {
        value["words"][0][format!("x{i}")] = json!(1);
    }
    let result = validate(&value);
    let w = valid_word(&result, 0);
    let unknown = w
        .warnings
        .iter()
        .filter(|i| i.code == IssueCode::UnknownProperty)
        .count();
    assert_eq!(unknown, 11);
    // A huge words array is one pack error, not one entry per item.
    let many: Vec<Value> = (0..50_000).map(|_| json!(1)).collect();
    let result = validate(&with(pack(), "/words", json!(many)));
    assert_eq!(pack_errors(&result).len(), 1);
    assert!(result.words.is_empty());
}
