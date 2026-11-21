//! Contract tests written from the requirements and `docs/design/architecture.md`
//! §5 (not from the implementation): boundaries, normalisation, paste handling.
//! Synthetic data only (R-24).

use serde_json::{Value, json};
use vocab_pack::hash::content_sha256;
use vocab_pack::keys::{headword_key, item_key, nfc, sentence_key};
use vocab_pack::marked::{self, MarkError};
use vocab_pack::paste::{MAX_PACK_BYTES, MAX_PACKS, MAX_PASTE_BYTES, PasteError, split_paste};
use vocab_pack::types::{Issue, IssueCode, PackValidation, PackWord, WordResult};
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

fn pack_of(words: Vec<Value>) -> Value {
    json!({
        "format": "vocab-pack",
        "format_version": 1,
        "pack_id": "test-book--2026-11-17--1",
        "exported_at": "2026-11-17",
        "part": { "index": 1, "total": 1 },
        "source": { "id": "test-book", "type": "book", "title": "Test Book" },
        "reading_position": "Chapter 2",
        "words": words
    })
}

fn pack() -> Value {
    pack_of(vec![word("hum")])
}

fn valid_word(result: &PackValidation, index: usize) -> &PackWord {
    match &result.words[index] {
        WordResult::Valid(w) => w,
        WordResult::Invalid(w) => panic!("word {index} invalid: {:?}", w.errors),
    }
}

fn word_is_valid(result: &PackValidation, index: usize) -> bool {
    matches!(result.words.get(index), Some(WordResult::Valid(_)))
}

fn word_errors(result: &PackValidation, index: usize) -> &[Issue] {
    match &result.words[index] {
        WordResult::Invalid(w) => &w.errors,
        WordResult::Valid(_) => &[],
    }
}

/// A pack with one word whose `field` is set to `value`.
fn with_word_field(field: &str, value: Value) -> Value {
    let mut w = word("hum");
    w[field] = value;
    pack_of(vec![w])
}

// ── [[ ]] scanner edge cases (R-20, R-23, design §5.2) ─────────────────────

#[test]
fn r23_adjacent_spans_are_two_spans() {
    assert_eq!(
        marked::split("A [[b]][[c]] d").expect_err("two spans"),
        MarkError::MoreThanOneSpan
    );
    assert_eq!(
        marked::split("[[b]] and [[c]]").expect_err("two spans"),
        MarkError::MoreThanOneSpan
    );
}

#[test]
fn r23_nested_or_doubled_brackets_are_rejected() {
    assert!(marked::split("A [[b [[c]] d]] e").is_err());
    assert!(marked::split("A [[b]]]] c").is_err());
    assert!(marked::split("A [[[[b]] c").is_err());
    assert!(marked::split("A ]] b [[c]]").is_err());
    assert!(marked::split("A [[b]] c ]]").is_err());
    assert!(marked::split("A [[b]] c [[").is_err());
}

#[test]
fn r23_opening_bracket_inside_the_span_is_rejected() {
    // "[[" inside the span would be a second opener.
    assert!(marked::split("A [[b[[c]] d").is_err());
}

#[test]
fn r23_empty_and_unclosed_spans_are_rejected() {
    assert_eq!(
        marked::split("A [[]] b").expect_err("empty"),
        MarkError::EmptySpan
    );
    assert_eq!(
        marked::split("A [[b c").expect_err("unclosed"),
        MarkError::Unclosed
    );
    assert_eq!(marked::split("A b c").expect_err("none"), MarkError::NoSpan);
    assert_eq!(
        marked::split("A b]] c").expect_err("close only"),
        MarkError::CloseBeforeOpen
    );
}

#[test]
fn r23_span_whitespace_variants_are_rejected_inner_whitespace_is_fine() {
    for bad in [
        "A [[ b]] c",
        "A [[b ]] c",
        "A [[\tb]] c",
        "A [[b\n]] c",
        "A [[ ]] c",
    ] {
        assert!(marked::split(bad).is_err(), "{bad:?}");
    }
    let m = marked::split("She was [[poring  over]] maps").expect("inner whitespace");
    assert_eq!(m.target, "poring  over");
}

#[test]
fn r23_spans_at_the_start_and_end_and_alone() {
    let m = marked::split("[[Hum]] loudly.").expect("start");
    assert_eq!(
        (m.pre.as_str(), m.target.as_str(), m.post.as_str()),
        ("", "Hum", " loudly.")
    );
    let m = marked::split("It went [[hum]]").expect("end");
    assert_eq!(
        (m.pre.as_str(), m.target.as_str(), m.post.as_str()),
        ("It went ", "hum", "")
    );
    let m = marked::split("[[hum]]").expect("alone");
    assert_eq!(m.plain(), "hum");
}

#[test]
fn r23_single_brackets_in_the_text_are_plain_characters() {
    let m = marked::split("The [sic] [[hum]] [1] arr[0].").expect("single brackets");
    assert_eq!(m.target, "hum");
    assert_eq!(m.pre, "The [sic] ");
    assert_eq!(m.post, " [1] arr[0].");
    let m = marked::split("A [[a[1]b]] c").expect("single bracket in span");
    assert_eq!(m.target, "a[1]b");
}

#[test]
fn r20_split_is_correct_for_multibyte_text() {
    let m = marked::split("Ve městě „[[žluťoučký]]“ kůň — 🐴.").expect("czech");
    assert_eq!(m.pre, "Ve městě „");
    assert_eq!(m.target, "žluťoučký");
    assert_eq!(m.post, "“ kůň — 🐴.");
    let m = marked::split("🐴[[🐴🐴]]🐴").expect("emoji");
    assert_eq!(m.target, "🐴🐴");
}

#[test]
fn r23_span_rule_is_applied_to_both_languages_independently() {
    let mut w = word("hum");
    w["sentences"][0]["cs"] = json!("Věta bez značky.");
    w["sentences"][1]["en"] = json!("A [[b]] and [[c]].");
    let result = validate(&pack_of(vec![w]));
    let word = valid_word(&result, 0);
    assert_eq!(word.sentences.len(), 1);
    assert_eq!(word.sentence_errors.len(), 2);
    assert!(
        word.sentence_errors
            .iter()
            .any(|e| e.pointer == "/words/0/sentences/0/cs")
    );
    assert!(
        word.sentence_errors
            .iter()
            .any(|e| e.pointer == "/words/0/sentences/1/en")
    );
}

// ── identity keys (design §5.3, R-03, R-22) ────────────────────────────────

#[test]
fn r03_headword_key_ignores_case_whitespace_and_unicode_form() {
    let key = headword_key("pore over");
    for variant in [
        "Pore over",
        "PORE OVER",
        "  pore  over ",
        "pore\tover",
        "pore\u{a0}over",
        "pore\nover",
    ] {
        assert_eq!(headword_key(variant), key, "{variant:?}");
    }
    assert_eq!(headword_key("cafe\u{301}"), headword_key("caf\u{e9}"));
    assert_eq!(headword_key("CAFE\u{301}"), headword_key("café"));
    assert_eq!(
        headword_key("zlu\u{165}ou\u{10d}k\u{fd}"),
        headword_key("zlu\u{74}\u{30c}ou\u{63}\u{30c}k\u{79}\u{301}")
    );
}

#[test]
fn r03_headword_key_apostrophes_and_hyphens() {
    let key = headword_key("don't");
    assert_eq!(headword_key("don\u{2019}t"), key);
    assert_eq!(headword_key("don\u{2018}t"), key);
    assert_eq!(headword_key("don\u{2bc}t"), key);
    let key = headword_key("carpet-bomb");
    assert_eq!(headword_key("carpet\u{2010}bomb"), key);
    assert_eq!(headword_key("carpet\u{2011}bomb"), key);
    // Needs judgement, so it is not done (design §5.3).
    assert_ne!(headword_key("carpet bomb"), key);
    assert_ne!(headword_key("carpetbomb"), key);
}

#[test]
fn r03_distinct_words_keep_distinct_keys() {
    assert_ne!(headword_key("hum"), headword_key("hummed"));
    assert_ne!(headword_key("hum"), headword_key("hymn"));
    assert_ne!(headword_key("pore over"), headword_key("pore"));
    // Diacritics are letters, not decoration.
    assert_ne!(headword_key("cafe"), headword_key("café"));
}

#[test]
fn r22_sentence_key_ignores_typography_markers_and_edges() {
    let key = sentence_key("He said, \"[[hum]] it.\"");
    for variant in [
        "He said, \u{201c}[[hum]] it.\u{201d}",
        "He said, \u{201e}[[hum]] it.\u{201c}",
        "He said, \"hum[[ it]].\"",
        "  HE SAID,   \"[[hum]]\tit.\"  ",
    ] {
        assert_eq!(sentence_key(variant), key, "{variant:?}");
    }
    // Marker position does not matter: the key is of the plain text.
    assert_eq!(
        sentence_key("A [[hum]]med tune."),
        sentence_key("A hum[[med]] tune.")
    );
    assert_eq!(
        sentence_key("It\u{2019}s a [[hum]]."),
        sentence_key("It's a [[hum]].")
    );
}

#[test]
fn r22_sentence_key_trims_leading_and_trailing_ellipses_only() {
    let key = sentence_key("the [[hum]] stopped");
    for variant in [
        "\u{2026}the [[hum]] stopped",
        "...the [[hum]] stopped",
        "the [[hum]] stopped\u{2026}",
        "the [[hum]] stopped...",
        "\u{2026} the [[hum]] stopped ...",
        "... \u{2026}the [[hum]] stopped",
    ] {
        assert_eq!(sentence_key(variant), key, "{variant:?}");
    }
    // R-22/R-53 (review): closing sentence punctuation is not identity.
    assert_eq!(sentence_key("the [[hum]] stopped."), key);
    assert_ne!(
        sentence_key("the [[hum]]. Stopped"),
        key,
        "a full stop inside the sentence is text"
    );
    assert_ne!(
        sentence_key("the [[hum]] \u{2026} stopped"),
        key,
        "inner ellipses are text"
    );
}

#[test]
fn r22_sentence_key_nfc() {
    assert_eq!(
        sentence_key("Kůň [[zlu\u{165}ou\u{10d}k\u{fd}]]."),
        sentence_key("Kůň [[zlu\u{74}\u{30c}ou\u{63}\u{30c}k\u{79}\u{301}]].")
    );
}

#[test]
fn r03_item_key_trims_collapses_and_lowercases() {
    assert_eq!(item_key("  Bzučet  "), item_key("bzučet"));
    assert_eq!(item_key("slovo  od\tslova"), item_key("Slovo od slova"));
    assert_eq!(item_key("z\u{30c}lutý"), item_key("žlutý"));
    assert_ne!(item_key("bzučet"), item_key("bzucet"));
    assert_eq!(nfc("e\u{301}"), "\u{e9}");
}

// ── control characters (design §5.2 "All text") ────────────────────────────

#[test]
fn r84_control_characters_rejected_in_word_scope_fields() {
    for c in ['\u{0}', '\u{7}', '\u{1b}', '\u{b}', '\u{7f}'] {
        let text = format!("bad{c}text");
        for field in [
            "meaning",
            "note",
            "hint",
            "grammar",
            "pronunciation",
            "icon",
        ] {
            let result = validate(&with_word_field(field, json!(text)));
            assert!(
                !word_is_valid(&result, 0),
                "{field} with U+{:04X} should drop the word",
                c as u32
            );
            assert!(
                word_errors(&result, 0)
                    .iter()
                    .any(|e| e.code == IssueCode::ControlCharacter),
                "{field} U+{:04X}: {:?}",
                c as u32,
                word_errors(&result, 0)
            );
        }
        let result = validate(&with_word_field("headword", json!(text)));
        assert!(!word_is_valid(&result, 0), "headword U+{:04X}", c as u32);
        let result = validate(&with_word_field("czech", json!([text])));
        assert!(!word_is_valid(&result, 0), "czech U+{:04X}", c as u32);
        let result = validate(&with_word_field("accepted", json!([text])));
        assert!(!word_is_valid(&result, 0), "accepted U+{:04X}", c as u32);
        let result = validate(&with_word_field("confusables", json!([text])));
        assert!(!word_is_valid(&result, 0), "confusables U+{:04X}", c as u32);
        let result = validate(&with_word_field(
            "forms",
            json!([{ "form": text, "common": true }]),
        ));
        assert!(!word_is_valid(&result, 0), "forms U+{:04X}", c as u32);
        let result = validate(&with_word_field(
            "tag",
            json!({ "kind": "seen-before", "text": text }),
        ));
        assert!(!word_is_valid(&result, 0), "tag text U+{:04X}", c as u32);
    }
}

#[test]
fn r84_control_characters_in_sentences_drop_only_the_sentence() {
    for c in ['\u{0}', '\u{8}', '\u{c}', '\u{7f}'] {
        for field in ["en", "cs", "location"] {
            let mut w = word("hum");
            w["sentences"][1][field] = if field == "location" {
                json!(format!("Ch{c}1"))
            } else if field == "en" {
                json!(format!("A [[hum]]{c} here."))
            } else {
                json!(format!("Tady [[bzukot]]{c}."))
            };
            let result = validate(&pack_of(vec![w]));
            let word = valid_word(&result, 0);
            assert_eq!(word.sentences.len(), 2, "{field} U+{:04X}", c as u32);
            assert_eq!(word.sentence_errors.len(), 1, "{field} U+{:04X}", c as u32);
        }
    }
}

#[test]
fn r84_control_characters_in_pack_level_text_reject_the_pack() {
    let mut p = pack();
    p["source"]["title"] = json!("Test\u{0}Book");
    assert!(validate(&p).pack.is_err());
    let mut p = pack();
    p["reading_position"] = json!("Chapter\u{7} 2");
    assert!(validate(&p).pack.is_err());
    let mut p = pack();
    p["source"]["author"] = json!("Test\u{1b}Author");
    assert!(validate(&p).pack.is_err());
}

#[test]
fn r84_newline_and_tab_are_allowed_in_text() {
    let mut w = word("hum");
    w["note"] = json!("line one\n\tline two\n");
    w["meaning"] = json!("a\nb");
    let result = validate(&pack_of(vec![w]));
    let w = valid_word(&result, 0);
    assert_eq!(w.note.as_deref(), Some("line one\n\tline two\n"));
    assert_eq!(w.meaning, "a\nb");
}

// ── limits: exactly at the limit and one over, counted in characters ──────

fn long(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

#[test]
fn r84_word_text_limits_exactly_at_and_one_over() {
    // (field, limit)
    let cases = [
        ("meaning", 1500),
        ("note", 6000),
        ("hint", 300),
        ("grammar", 200),
        ("pronunciation", 200),
        ("speak", 200),
        ("spelling_note", 200),
        ("icon", 40),
    ];
    for (field, limit) in cases {
        let mut at = word("hum");
        at[field] = json!(long('x', limit));
        if field == "spelling_note" {
            at["spelling"] = json!(true);
        }
        let result = validate(&pack_of(vec![at]));
        assert!(word_is_valid(&result, 0), "{field} at {limit}");

        let mut over = word("hum");
        over[field] = json!(long('x', limit + 1));
        let result = validate(&pack_of(vec![over]));
        assert!(!word_is_valid(&result, 0), "{field} at {}", limit + 1);
    }
}

#[test]
fn r84_limits_count_characters_not_bytes() {
    // 100 two-byte letters and 40 four-byte emoji are exactly at the limit.
    let mut w = word("hum");
    w["headword"] = json!(long('ž', 100));
    w["icon"] = json!(long('🐴', 40));
    w["czech"] = json!([long('ř', 200)]);
    w["sentences"][0]["en"] = json!(format!("[[x]]{}", long('é', 595)));
    let result = validate(&pack_of(vec![w]));
    let w = valid_word(&result, 0);
    assert_eq!(w.sentences.len(), 3, "{:?}", w.sentence_errors);

    let mut over = word("hum");
    over["headword"] = json!(long('ž', 101));
    assert!(!word_is_valid(&validate(&pack_of(vec![over])), 0));
    let mut over = word("hum");
    over["icon"] = json!(long('🐴', 41));
    assert!(!word_is_valid(&validate(&pack_of(vec![over])), 0));
}

#[test]
fn r84_headword_and_list_item_limits() {
    let mut w = word("hum");
    w["headword"] = json!(long('h', 100));
    assert!(word_is_valid(&validate(&pack_of(vec![w])), 0));
    let mut w = word("hum");
    w["headword"] = json!(long('h', 101));
    assert!(!word_is_valid(&validate(&pack_of(vec![w])), 0));

    for field in ["czech", "accepted", "confusables"] {
        let mut w = word("hum");
        w[field] = json!([long('c', 200)]);
        assert!(word_is_valid(&validate(&pack_of(vec![w])), 0), "{field}");
        let mut w = word("hum");
        w[field] = json!([long('c', 201)]);
        assert!(!word_is_valid(&validate(&pack_of(vec![w])), 0), "{field}");
        let items: Vec<String> = (0..50).map(|i| format!("item{i}")).collect();
        let mut w = word("hum");
        w[field] = json!(items);
        assert!(word_is_valid(&validate(&pack_of(vec![w])), 0), "{field} 50");
        let items: Vec<String> = (0..51).map(|i| format!("item{i}")).collect();
        let mut w = word("hum");
        w[field] = json!(items);
        assert!(
            !word_is_valid(&validate(&pack_of(vec![w])), 0),
            "{field} 51"
        );
    }
}

#[test]
fn r20_sentence_limits_exactly_at_and_one_over() {
    let at = |n: usize| format!("[[x]]{}", long('a', n - 5));
    let mut w = word("hum");
    w["sentences"][0]["en"] = json!(at(600));
    w["sentences"][0]["cs"] = json!(at(600));
    w["sentences"][0]["location"] = json!(long('l', 100));
    let result = validate(&pack_of(vec![w]));
    assert_eq!(valid_word(&result, 0).sentences.len(), 3);

    for (field, text) in [
        ("en", at(601)),
        ("cs", at(601)),
        ("location", long('l', 101)),
    ] {
        let mut w = word("hum");
        w["sentences"][0][field] = json!(text);
        let result = validate(&pack_of(vec![w]));
        let word = valid_word(&result, 0);
        assert_eq!(word.sentences.len(), 2, "{field}");
        assert_eq!(word.sentence_errors.len(), 1, "{field}");
    }
}

#[test]
fn r20_sentence_count_limit_is_20() {
    let mut w = word("hum");
    w["sentences"] = json!(
        (0..20)
            .map(|i| json!({
                "en": format!("Line {i} with a [[hum]]."),
                "cs": format!("Řádek {i} s [[bzukotem]]."),
                "origin": "example"
            }))
            .collect::<Vec<_>>()
    );
    let result = validate(&pack_of(vec![w.clone()]));
    assert_eq!(valid_word(&result, 0).sentences.len(), 20);
    let mut list = w["sentences"].as_array().expect("list").clone();
    list.push(list[0].clone());
    w["sentences"] = Value::Array(list);
    assert!(!word_is_valid(&validate(&pack_of(vec![w])), 0));
}

#[test]
fn r84_pack_level_limits_exactly_at_and_one_over() {
    let mut p = pack();
    p["source"]["title"] = json!(long('t', 300));
    assert!(validate(&p).pack.is_ok());
    p["source"]["title"] = json!(long('t', 301));
    assert!(validate(&p).pack.is_err());

    let mut p = pack();
    p["reading_position"] = json!(long('r', 200));
    assert!(validate(&p).pack.is_ok());
    p["reading_position"] = json!(long('r', 201));
    assert!(validate(&p).pack.is_err());

    let words: Vec<Value> = (0..100).map(|i| word(&format!("hum{i}"))).collect();
    assert!(validate(&pack_of(words)).pack.is_ok());
    let words: Vec<Value> = (0..101).map(|i| word(&format!("hum{i}"))).collect();
    assert!(validate(&pack_of(words)).pack.is_err());

    let mut p = pack();
    p["source"]["id"] = json!(long('a', 80));
    p["pack_id"] = json!(format!("{}--2026-11-17--1", long('a', 80)));
    assert!(validate(&p).pack.is_ok());
    p["source"]["id"] = json!(long('a', 81));
    assert!(validate(&p).pack.is_err());
}

// ── format version (design §5.2, §5.6) ─────────────────────────────────────

#[test]
fn r10_format_version_2_is_rejected_with_the_documented_message() {
    let mut p = pack();
    p["format_version"] = json!(2);
    let result = validate(&p);
    let errors = result.pack.expect_err("rejected");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, IssueCode::UnknownVersion);
    assert_eq!(errors[0].pointer, "/format_version");
    let m = errors[0].message.to_lowercase();
    assert!(m.contains("format 2"), "{m}");
    assert!(m.contains("update the app"), "{m}");
    assert!(m.contains("re-paste"), "{m}");
    assert!(m.contains("instructions"), "{m}");
}

#[test]
fn r10_other_format_versions_are_rejected() {
    for v in [json!(0), json!(-1), json!(3), json!(1_000_000)] {
        let mut p = pack();
        p["format_version"] = v.clone();
        let result = validate(&p);
        assert!(result.pack.is_err(), "{v}");
    }
    for v in [json!("1"), json!(1.5), json!(true), json!(null), json!([1])] {
        let mut p = pack();
        p["format_version"] = v.clone();
        assert!(validate(&p).pack.is_err(), "{v}");
    }
    let mut p = pack();
    p["format_version"] = json!(1);
    assert!(validate(&p).pack.is_ok());
}

// ── warnings, not errors ───────────────────────────────────────────────────

#[test]
fn r10_unknown_fields_warn_and_do_not_reject_anything() {
    let mut p = pack();
    p["future_pack_field"] = json!(1);
    p["source"]["future_source_field"] = json!("x");
    p["words"][0]["future_word_field"] = json!([1, 2]);
    p["words"][0]["sentences"][0]["future_sentence_field"] = json!({});
    let result = validate(&p);
    assert!(result.is_clean(), "{result:#?}");
    let unknown: Vec<&Issue> = result
        .all_warnings()
        .filter(|w| w.code == IssueCode::UnknownProperty)
        .collect();
    assert!(unknown.len() >= 4, "{unknown:?}");
    let word = valid_word(&result, 0);
    assert_eq!(word.sentences.len(), 3, "the sentence is kept");
}

#[test]
fn r20_fewer_than_three_sentences_warn_but_import() {
    for n in 0..3 {
        let mut w = word("hum");
        w["sentences"] = Value::Array(
            w["sentences"]
                .as_array()
                .expect("list")
                .iter()
                .take(n)
                .cloned()
                .collect(),
        );
        let result = validate(&pack_of(vec![w]));
        assert!(result.is_clean(), "{n}");
        let word = valid_word(&result, 0);
        assert!(
            word.warnings
                .iter()
                .any(|w| w.code == IssueCode::FewSentences),
            "{n} sentences: {:?}",
            word.warnings
        );
    }
    let result = validate(&pack());
    assert!(
        valid_word(&result, 0)
            .warnings
            .iter()
            .all(|w| w.code != IssueCode::FewSentences)
    );
}

#[test]
fn r20_the_sentence_warning_counts_valid_sentences_only() {
    // Three given, one broken: two valid → warning, and the error is listed.
    let mut w = word("hum");
    w["sentences"][2]["en"] = json!("No span here.");
    let result = validate(&pack_of(vec![w]));
    let vw = valid_word(&result, 0);
    assert_eq!(vw.sentence_errors.len(), 1);
    assert!(
        vw.warnings
            .iter()
            .any(|w| w.code == IssueCode::FewSentences)
    );

    // Three given, two duplicates by key: two valid → warning.
    let mut w = word("hum");
    w["sentences"][2]["en"] = json!("TEST   sentence number 1 has a [[hum]] in it.\u{2026}");
    let result = validate(&pack_of(vec![w]));
    let vw = valid_word(&result, 0);
    assert_eq!(vw.sentences.len(), 2);
    assert!(
        vw.warnings
            .iter()
            .any(|w| w.code == IssueCode::FewSentences)
    );
    assert!(
        vw.warnings
            .iter()
            .any(|w| w.code == IssueCode::DuplicateSentence)
    );
}

#[test]
fn r22_duplicate_sentences_in_a_pack_are_found_by_key() {
    let mut w = word("hum");
    w["sentences"] = json!([
        { "en": "He said, \u{201c}[[hum]] it.\u{201d}", "cs": "Řekl: „[[Bzuč]].“", "origin": "book", "location": "Ch 1" },
        { "en": "He said, \"[[hum]] it.\"", "cs": "Jiný překlad [[bzukotu]].", "origin": "book" },
        { "en": "...he said, \"hum[[ ]] it.\"", "cs": "x [[y]]", "origin": "book" },
        { "en": "\u{2026}HE SAID,  \"[[hum]] it.\"", "cs": "Další [[překlad]].", "origin": "book" },
    ]);
    let result = validate(&pack_of(vec![w]));
    let word = valid_word(&result, 0);
    // #3 has a whitespace-only span → sentence error; #2 and #4 duplicate #1.
    assert_eq!(word.sentences.len(), 1);
    assert_eq!(word.sentences[0].location.as_deref(), Some("Ch 1"));
    assert_eq!(word.sentence_errors.len(), 1);
    assert_eq!(
        word.warnings
            .iter()
            .filter(|w| w.code == IssueCode::DuplicateSentence)
            .count(),
        2
    );
}

#[test]
fn r03_headwords_that_differ_only_in_case_whitespace_or_nfd_are_duplicates() {
    let words = vec![
        word("café au lait"),
        word("Cafe\u{301}  AU lait"),
        word(" CAFÉ au lait "),
        word("cafe au lait"),
    ];
    let result = validate(&pack_of(words));
    assert!(word_is_valid(&result, 0));
    assert!(!word_is_valid(&result, 1));
    assert!(!word_is_valid(&result, 2));
    assert!(word_is_valid(&result, 3), "no diacritics = another word");
    assert_eq!(word_errors(&result, 1)[0].code, IssueCode::DuplicateWord);
}

#[test]
fn r84_text_is_nfc_normalised_and_otherwise_verbatim() {
    let mut w = word("Cafe\u{301}");
    w["note"] = json!("<script>alert(1)</script> & **x** [link](javascript:1) \u{202e}");
    w["czech"] = json!(["z\u{30c}lutý"]);
    let result = validate(&pack_of(vec![w]));
    let w = valid_word(&result, 0);
    assert_eq!(w.headword, "Caf\u{e9}");
    assert_eq!(w.czech, ["žlutý"]);
    assert_eq!(
        w.note.as_deref(),
        Some("<script>alert(1)</script> & **x** [link](javascript:1) \u{202e}")
    );
}

#[test]
fn r15_optional_fields_absent_are_none_and_goal_is_not_defaulted_by_the_format() {
    let result = validate(&pack());
    let w = valid_word(&result, 0);
    assert_eq!(w.goal, None, "the default is derived later (R-15)");
    assert_eq!(w.spelling, None);
    assert_eq!(w.hint, None);
    assert!(w.accepted.is_empty());
    assert_eq!(w.seq, None);
}

#[test]
fn r01_seq_explicit_values_are_carried() {
    let mut a = word("alpha");
    a["seq"] = json!(7);
    let b = word("beta");
    let mut c = word("gamma");
    c["seq"] = json!(1_000_000);
    let result = validate(&pack_of(vec![a, b, c]));
    assert_eq!(valid_word(&result, 0).seq, Some(7));
    assert_eq!(valid_word(&result, 1).seq, None);
    assert_eq!(valid_word(&result, 2).seq, Some(1_000_000));
    let mut d = word("delta");
    d["seq"] = json!(1_000_001);
    assert!(!word_is_valid(&validate(&pack_of(vec![d])), 0));
}

// ── paste handling (R-11, design §5.1) ─────────────────────────────────────

fn pack_text(n: usize) -> String {
    let mut p = pack();
    p["pack_id"] = json!(format!("test-book--2026-11-17--{n}"));
    serde_json::to_string_pretty(&p).expect("json")
}

#[test]
fn r11_realistic_answer_with_prose_and_several_fence_styles() {
    let text = format!(
        "Sure! Here are the packs.\n\n**Part 1/3** {{not json}}\n```json\n{}\n```\nNext, part 2 (a plain fence):\n\n```\n{}\n```\n\nAnd part 3:\n~~~json\n{}\n~~~\n\nLet me know if you want more {{\"a\": 1}} words.\n",
        pack_text(1),
        pack_text(2),
        pack_text(3)
    );
    let items = split_paste(&text).expect("paste");
    assert_eq!(items.len(), 3);
    let ids: Vec<String> = items
        .iter()
        .map(|i| {
            i.as_ref().expect("pack").value["pack_id"]
                .as_str()
                .expect("id")
                .to_owned()
        })
        .collect();
    assert_eq!(
        ids,
        [
            "test-book--2026-11-17--1",
            "test-book--2026-11-17--2",
            "test-book--2026-11-17--3"
        ]
    );
}

#[test]
fn r11_windows_line_endings_and_a_byte_order_mark_free_paste() {
    let text =
        format!("Here:\r\n```json\r\n{}\r\n```\r\nDone.\r\n", pack_text(1)).replace('\n', "\r\n");
    let text = text.replace("\r\r\n", "\r\n");
    let items = split_paste(&text).expect("paste");
    assert_eq!(items.len(), 1);
    assert!(items[0].is_ok(), "{:?}", items[0]);
}

#[test]
fn r11_two_packs_in_one_fence_and_without_any_fence() {
    let both = format!("```json\n{}\n{}\n```", pack_text(1), pack_text(2));
    assert_eq!(split_paste(&both).expect("paste").len(), 2);
    let bare = format!("{}\n\n{}", pack_text(1), pack_text(2));
    let items = split_paste(&bare).expect("paste");
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(Result::is_ok));
}

#[test]
fn r11_a_pack_inside_a_longer_fence_with_backticks_in_text() {
    let mut p = pack();
    p["words"][0]["note"] = json!("use ``` fences carefully");
    let json_text = serde_json::to_string(&p).expect("json");
    // The note's backticks are not at the start of a line, so they cannot close the block.
    let text = format!("```json\n{json_text}\n```\n");
    let items = split_paste(&text).expect("paste");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].as_ref().expect("pack").value, p);
}

#[test]
fn r11_hash_ignores_indentation_key_order_and_whitespace_but_not_content() {
    let p = pack();
    let pretty = serde_json::to_string_pretty(&p).expect("json");
    let compact = serde_json::to_string(&p).expect("json");
    let reindented = pretty.replace("  ", "\t");
    let hash = |t: &str| {
        split_paste(t).expect("paste")[0]
            .as_ref()
            .expect("pack")
            .content_sha256
            .clone()
    };
    assert_eq!(hash(&pretty), hash(&compact));
    assert_eq!(hash(&pretty), hash(&reindented));
    assert_eq!(hash(&format!("```json\n{pretty}\n```")), hash(&compact));

    // Key order: reverse the top-level members by hand.
    let a: Value =
        serde_json::from_str(r#"{"x":1,"y":{"b":2,"a":[1,2,{"q":1,"p":2}]}}"#).expect("a");
    let b: Value =
        serde_json::from_str(r#"{"y":{"a":[1,2,{"p":2,"q":1}],"b":2},"x":1}"#).expect("b");
    assert_eq!(content_sha256(&a), content_sha256(&b));

    let mut changed = p.clone();
    changed["words"][0]["usefulness"] = json!(7);
    assert_ne!(content_sha256(&changed), content_sha256(&p));
    // Array order is content.
    let c: Value = json!({"a": [1, 2]});
    let d: Value = json!({"a": [2, 1]});
    assert_ne!(content_sha256(&c), content_sha256(&d));
    // Escaped and literal characters are the same value.
    let e: Value = serde_json::from_str(r#"{"a":"é"}"#).expect("e");
    let f: Value = serde_json::from_str("{\"a\":\"é\"}").expect("f");
    assert_eq!(content_sha256(&e), content_sha256(&f));
}

#[test]
fn r84_paste_limit_is_2_mib_inclusive() {
    let body = pack_text(1);
    let at = format!("{body}{}", " ".repeat(MAX_PASTE_BYTES - body.len()));
    assert_eq!(at.len(), MAX_PASTE_BYTES);
    assert!(split_paste(&at).is_ok());
    let over = format!("{at} ");
    assert_eq!(
        split_paste(&over),
        Err(PasteError::TooLarge {
            bytes: MAX_PASTE_BYTES + 1
        })
    );
}

#[test]
fn r84_single_pack_limit_is_1_mib_inclusive() {
    let build = |padding: usize| {
        let mut p = pack();
        p["x_padding"] = json!("a".repeat(padding));
        serde_json::to_string(&p).expect("json")
    };
    let base = build(0).len();
    let at = build(MAX_PACK_BYTES - base);
    assert_eq!(at.len(), MAX_PACK_BYTES);
    let items = split_paste(&at).expect("paste");
    assert!(items[0].is_ok(), "exactly at the limit");
    let over = build(MAX_PACK_BYTES - base + 1);
    assert_eq!(over.len(), MAX_PACK_BYTES + 1);
    let items = split_paste(&over).expect("paste");
    assert!(items[0].is_err(), "one byte over the limit");
}

#[test]
fn r84_exactly_20_packs_are_read_the_21st_rejects_the_paste() {
    // Guarantor decision F1 (M2 fix round): more than 20 values reject the
    // whole paste with one error, so no report grows past the limit.
    let pastes = |n: usize| (1..=n).map(pack_text).collect::<Vec<_>>().join("\n");
    let items = split_paste(&pastes(MAX_PACKS)).expect("paste");
    assert_eq!(items.len(), MAX_PACKS);
    assert!(items.iter().all(Result::is_ok));
    assert_eq!(
        split_paste(&pastes(MAX_PACKS + 1)),
        Err(PasteError::TooManyPacks)
    );
}

#[test]
fn r23_a_broken_block_does_not_hide_the_good_ones() {
    let text = format!(
        "```json\n{}\n```\n```json\n{{\"format\": \"vocab-pack\", \n```\n```json\n{}\n```",
        pack_text(1),
        pack_text(3)
    );
    let items = split_paste(&text).expect("paste");
    assert_eq!(items.len(), 3);
    assert!(items[0].is_ok());
    assert!(items[1].is_err());
    assert!(items[2].is_ok());
}

#[test]
fn r11_json_that_is_not_an_object_is_still_split_and_rejected_by_validation() {
    let items = split_paste("```json\n[1,2]\n```\n```json\n\"text\"\n```").expect("paste");
    assert_eq!(items.len(), 2);
    for item in items {
        let value = item.expect("json value").value;
        assert!(validate(&value).pack.is_err());
    }
}
