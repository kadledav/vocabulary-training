//! Validation of one pack (design §5.2, R-23, R-84).
//!
//! Errors are scoped: **pack** errors reject the pack, **word** errors drop
//! the word, **sentence** errors drop the sentence (the word stays). Nothing
//! is dropped silently: every finding is returned with its JSON pointer.
//! The rules are those of `docs/vocab-pack.schema.json` plus the few a schema
//! cannot express (a headword repeated within the pack, control characters,
//! a Czech translation without a letter, `part.index > part.total`, an
//! `exported_at` that is no calendar date). Unknown properties are warnings
//! here, not errors (the one documented leniency).
//!
//! Two things are the same for every optional property: `null` means "not
//! given", and line breaks written as `\r\n` or `\r` are read as `\n`.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::keys::{headword_key, nfc, sentence_key};
use crate::marked::{self, strip_markers};
use crate::types::{
    FORMAT, Goal, InvalidWord, Issue, IssueCode, KNOWN_FORMAT_VERSIONS, Origin, PackForm,
    PackHeader, PackSentence, PackSource, PackTag, PackValidation, PackWord, Part, SourceType,
    TagKind, WordResult,
};

/// Most words in one pack.
pub const MAX_WORDS: usize = 100;
/// Unknown properties listed one by one per object.
const MAX_UNKNOWN_LISTED: usize = 10;
/// Most sentences per word.
pub const MAX_SENTENCES: usize = 20;
/// Most items of a list field (`czech`, `accepted`, `confusables`, `forms`).
pub const MAX_LIST_ITEMS: usize = 50;
/// Largest explicit `seq`.
pub const MAX_SEQ: i64 = 1_000_000;
/// The instruction text asks for hints of about this length.
pub const HINT_ADVICE_CHARS: usize = 120;
/// Fewer valid sentences than this → word-only Understand card.
pub const MIN_SENTENCES_FOR_SENTENCE_CARD: usize = 3;

const PACK_KEYS: &[&str] = &[
    "format",
    "format_version",
    "pack_id",
    "exported_at",
    "part",
    "source",
    "reading_position",
    "words",
];
const SOURCE_KEYS: &[&str] = &[
    "id",
    "type",
    "title",
    "author",
    "series",
    "series_number",
    "url",
];
const PART_KEYS: &[&str] = &["index", "total"];
const WORD_KEYS: &[&str] = &[
    "headword",
    "grammar",
    "forms",
    "pronunciation",
    "speak",
    "meaning",
    "czech",
    "usefulness",
    "goal",
    "accepted",
    "confusables",
    "hint",
    "spelling",
    "spelling_note",
    "note",
    "tag",
    "icon",
    "seq",
    "sentences",
];
const FORM_KEYS: &[&str] = &["form", "common"];
const TAG_KEYS: &[&str] = &["kind", "text"];
const SENTENCE_KEYS: &[&str] = &["en", "cs", "origin", "location"];

/// Validate one pack value.
pub fn validate(value: &Value) -> PackValidation {
    let mut pack = Findings::default();
    let Some(obj) = value.as_object() else {
        pack.error("", IssueCode::Invalid, "A pack must be a JSON object.");
        return PackValidation {
            pack: Err(pack.errors),
            words: Vec::new(),
            warnings: pack.warnings,
        };
    };

    // The version decides which rules apply, so it is checked first.
    let format_version = match obj.get("format_version") {
        None => {
            pack.error(
                "/format_version",
                IssueCode::Missing,
                "format_version is missing.",
            );
            None
        }
        Some(v) => match integer(v) {
            Some(n) if KNOWN_FORMAT_VERSIONS.iter().any(|&k| i64::from(k) == n) => {
                u32::try_from(n).ok()
            }
            Some(n) => {
                pack.error(
                    "/format_version",
                    IssueCode::UnknownVersion,
                    format!(
                        "This pack uses format {n}, which this app does not know; update the app, or re-paste the export instructions into the Claude project."
                    ),
                );
                return PackValidation {
                    pack: Err(pack.errors),
                    words: Vec::new(),
                    warnings: pack.warnings,
                };
            }
            None => {
                pack.error(
                    "/format_version",
                    IssueCode::Invalid,
                    "format_version must be an integer.",
                );
                None
            }
        },
    };

    match obj.get("format") {
        Some(Value::String(f)) if f == FORMAT => {}
        Some(_) => pack.error(
            "/format",
            IssueCode::Invalid,
            format!("format must be \"{FORMAT}\"."),
        ),
        None => pack.error("/format", IssueCode::Missing, "format is missing."),
    }
    pack.unknown(obj, "", PACK_KEYS);

    let pack_id = pack.required_text(obj, "", "pack_id", 1, 120);
    let pack_id_parts = pack_id.as_deref().and_then(|id| {
        let parts = parse_pack_id(id);
        if parts.is_none() {
            pack.error(
                "/pack_id",
                IssueCode::Invalid,
                "pack_id must look like <source-id>--YYYY-MM-DD--n (lowercase letters, digits and hyphens; n from 1 to 9999).",
            );
        }
        parts
    });

    let exported_at = match obj.get("exported_at") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_date(s) => Some(s.clone()),
        Some(Value::String(s)) if is_date_shaped(s) => {
            pack.error(
                "/exported_at",
                IssueCode::Invalid,
                "exported_at is not a real calendar date (check the day of the month).",
            );
            None
        }
        Some(_) => {
            pack.error(
                "/exported_at",
                IssueCode::Invalid,
                "exported_at must be a date written as YYYY-MM-DD.",
            );
            None
        }
    };

    let part = match obj.get("part") {
        None | Some(Value::Null) => {
            pack.warn(
                "/part",
                IssueCode::Advice,
                "part is missing; treated as part 1 of 1.",
            );
            Some(Part { index: 1, total: 1 })
        }
        Some(Value::Object(p)) => {
            pack.unknown(p, "/part", PART_KEYS);
            let index = pack.required_int(p, "/part", "index", 1, 999);
            let total = pack.required_int(p, "/part", "total", 1, 999);
            match (index, total) {
                (Some(index), Some(total)) if index > total => {
                    pack.error(
                        "/part/index",
                        IssueCode::Invalid,
                        format!("part.index ({index}) is greater than part.total ({total})."),
                    );
                    None
                }
                (Some(index), Some(total)) => Some(Part {
                    index: as_u32(index),
                    total: as_u32(total),
                }),
                _ => None,
            }
        }
        Some(_) => {
            pack.error(
                "/part",
                IssueCode::Invalid,
                "part must be an object {\"index\": …, \"total\": …}.",
            );
            None
        }
    };

    let source = match obj.get("source") {
        None => {
            pack.error("/source", IssueCode::Missing, "source is missing.");
            None
        }
        Some(Value::Object(s)) => source(&mut pack, s),
        Some(_) => {
            pack.error("/source", IssueCode::Invalid, "source must be an object.");
            None
        }
    };

    let reading_position = pack.optional_text(obj, "", "reading_position", 200);
    if let (Some(src), Some(_)) = (&source, &reading_position)
        && src.kind != SourceType::Book
    {
        pack.warn(
            "/reading_position",
            IssueCode::Advice,
            "reading_position is meant for books only.",
        );
    }
    if let (Some(src), Some((prefix, _, _))) = (&source, &pack_id_parts)
        && prefix != &src.id
    {
        pack.warn(
            "/pack_id",
            IssueCode::Advice,
            "pack_id does not start with source.id.",
        );
    }

    let words = match obj.get("words") {
        None => {
            pack.error("/words", IssueCode::Missing, "words is missing.");
            Vec::new()
        }
        Some(Value::Array(items)) => {
            if items.is_empty() {
                pack.error("/words", IssueCode::Invalid, "words is empty.");
            } else if items.len() > MAX_WORDS {
                // The words are not validated one by one: a huge array must
                // not turn into a huge report.
                pack.error(
                    "/words",
                    IssueCode::Invalid,
                    format!(
                        "A pack may hold at most {MAX_WORDS} words; this one has {}. Split it into several packs.",
                        items.len()
                    ),
                );
            }
            if items.len() > MAX_WORDS {
                Vec::new()
            } else {
                words(items)
            }
        }
        Some(_) => {
            pack.error("/words", IssueCode::Invalid, "words must be an array.");
            Vec::new()
        }
    };

    let header = match (format_version, pack_id, pack_id_parts, part, source) {
        (
            Some(format_version),
            Some(pack_id),
            Some((_, pack_date, pack_number)),
            Some(part),
            Some(source),
        ) if pack.errors.is_empty() => Ok(PackHeader {
            format_version,
            pack_id,
            pack_date,
            pack_number,
            exported_at,
            part,
            source,
            reading_position,
        }),
        _ => Err(pack.errors),
    };
    PackValidation {
        pack: header,
        words,
        warnings: pack.warnings,
    }
}

fn source(pack: &mut Findings, s: &Map<String, Value>) -> Option<PackSource> {
    const BASE: &str = "/source";
    pack.unknown(s, BASE, SOURCE_KEYS);
    let id = pack.required_text(s, BASE, "id", 1, 80);
    let id = id.filter(|id| {
        let ok = is_slug(id);
        if !ok {
            pack.error(
                "/source/id",
                IssueCode::Invalid,
                "source.id must be lowercase letters and digits separated by single hyphens.",
            );
        }
        ok
    });
    let kind = match s.get("type") {
        Some(Value::String(t)) => SourceType::parse(t).or_else(|| {
            pack.error(
                "/source/type",
                IssueCode::Invalid,
                format!("source.type must be one of {}.", SourceType::ALL.join(", ")),
            );
            None
        }),
        Some(_) => {
            pack.error(
                "/source/type",
                IssueCode::Invalid,
                "source.type must be a string.",
            );
            None
        }
        None => {
            pack.error(
                "/source/type",
                IssueCode::Missing,
                "source.type is missing.",
            );
            None
        }
    };
    // Optional: real exports sometimes leave it out (e.g. for `general`).
    // When present it must not be empty (schema `minLength: 1`).
    let title = match s.get("title") {
        Some(Value::String(t)) if t.is_empty() => {
            pack.error("/source/title", IssueCode::Invalid, "title is empty.");
            None
        }
        // `null` and a blank title mean "no title given".
        _ => pack.optional_text(s, BASE, "title", 300),
    };
    let author = pack.optional_text(s, BASE, "author", 300);
    let series = pack.optional_text(s, BASE, "series", 300);
    let series_number = match s.get("series_number") {
        None | Some(Value::Null) => None,
        Some(v) => match integer(v) {
            Some(n) if (1..=9999).contains(&n) => Some(as_u32(n)),
            _ => {
                pack.error(
                    "/source/series_number",
                    IssueCode::Invalid,
                    "source.series_number must be an integer from 1 to 9999, or null.",
                );
                None
            }
        },
    };
    // Only web addresses: the app shows the URL as a link.
    let url = pack.optional_text(s, BASE, "url", 2000).filter(|url| {
        let ok = is_web_url(url);
        if !ok {
            pack.error(
                "/source/url",
                IssueCode::Invalid,
                "source.url must start with http:// or https://.",
            );
        }
        ok
    });
    Some(PackSource {
        id: id?,
        kind: kind?,
        title,
        author,
        series,
        series_number,
        url,
    })
}

fn words(items: &[Value]) -> Vec<WordResult> {
    let mut seen: HashSet<String> = HashSet::new();
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let result = word(index, item);
            match result {
                WordResult::Valid(w) if !seen.insert(w.key.clone()) => {
                    let mut errors = Vec::new();
                    errors.push(Issue {
                        pointer: format!("/words/{index}/headword"),
                        code: IssueCode::DuplicateWord,
                        message: "The same headword appears earlier in this pack; this occurrence is skipped.".to_owned(),
                    });
                    WordResult::Invalid(InvalidWord {
                        index,
                        headword: Some(w.headword),
                        errors,
                        warnings: w.warnings,
                    })
                }
                other => other,
            }
        })
        .collect()
}

fn word(index: usize, value: &Value) -> WordResult {
    let base = format!("/words/{index}");
    let mut f = Findings::default();
    let Some(w) = value.as_object() else {
        f.error(&base, IssueCode::Invalid, "A word must be a JSON object.");
        return WordResult::Invalid(InvalidWord {
            index,
            headword: None,
            errors: f.errors,
            warnings: f.warnings,
        });
    };
    f.unknown(w, &base, WORD_KEYS);

    // Blank also when only invisible characters are left: such a headword
    // has an empty identity key and would merge with every other one.
    let headword = f
        .required_text(w, &base, "headword", 1, 100)
        .map(|h| h.trim().to_owned())
        .filter(|h| {
            let ok = !headword_key(h).is_empty();
            if !ok {
                f.error(
                    format!("{base}/headword"),
                    IssueCode::Invalid,
                    "headword is blank (it has no visible character).",
                );
            }
            ok
        });
    let grammar = f.optional_text(w, &base, "grammar", 200);
    let forms = forms(&mut f, w, &base);
    let pronunciation = f.optional_text(w, &base, "pronunciation", 200);
    let speak = f.optional_text(w, &base, "speak", 200);
    let meaning = f.required_text(w, &base, "meaning", 1, 1500).filter(|m| {
        let ok = !is_blank(m);
        if !ok {
            f.error(
                format!("{base}/meaning"),
                IssueCode::Invalid,
                "meaning is blank (it has no visible character).",
            );
        }
        ok
    });
    let czech = f.text_list(w, &base, "czech", true);
    if czech.as_ref().is_some_and(Vec::is_empty) {
        f.error(
            format!("{base}/czech"),
            IssueCode::Invalid,
            "czech needs at least one Czech equivalent.",
        );
    }
    let usefulness = f.required_int(w, &base, "usefulness", 1, 10);
    let goal = match w.get("goal") {
        None | Some(Value::Null) => None,
        Some(Value::String(g)) if Goal::parse(g).is_some() => Goal::parse(g),
        Some(_) => {
            f.error(
                format!("{base}/goal"),
                IssueCode::Invalid,
                format!("goal must be one of {}.", Goal::ALL.join(", ")),
            );
            None
        }
    };
    let accepted = f.text_list(w, &base, "accepted", false).unwrap_or_default();
    let confusables = f
        .text_list(w, &base, "confusables", false)
        .unwrap_or_default();
    let hint = f.optional_text(w, &base, "hint", 300);
    if hint
        .as_ref()
        .is_some_and(|h| h.chars().count() > HINT_ADVICE_CHARS)
    {
        f.warn(
            format!("{base}/hint"),
            IssueCode::Advice,
            format!("hint is longer than about {HINT_ADVICE_CHARS} characters."),
        );
    }
    let spelling = match w.get("spelling") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => {
            f.error(
                format!("{base}/spelling"),
                IssueCode::Invalid,
                "spelling must be true, false or null.",
            );
            None
        }
    };
    let spelling_note = f.optional_text(w, &base, "spelling_note", 200);
    if spelling_note.is_some() && spelling != Some(true) {
        f.warn(
            format!("{base}/spelling_note"),
            IssueCode::Advice,
            "spelling_note is given but spelling is not true.",
        );
    }
    let note = f.optional_text(w, &base, "note", 6000);
    let tag = tag(&mut f, w, &base);
    let icon = f.optional_text(w, &base, "icon", 40);
    let seq = match w.get("seq") {
        None | Some(Value::Null) => None,
        Some(v) => match integer(v) {
            Some(n) if (1..=MAX_SEQ).contains(&n) => Some(as_u32(n)),
            _ => {
                f.error(
                    format!("{base}/seq"),
                    IssueCode::Invalid,
                    format!("seq must be an integer from 1 to {MAX_SEQ}."),
                );
                None
            }
        },
    };

    let mut sentence_errors = Vec::new();
    let mut sentences: Vec<PackSentence> = Vec::new();
    match w.get("sentences") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) if items.len() > MAX_SENTENCES => f.error(
            format!("{base}/sentences"),
            IssueCode::Invalid,
            format!(
                "A word may have at most {MAX_SENTENCES} sentences; this one has {}.",
                items.len()
            ),
        ),
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                let pointer = format!("{base}/sentences/{i}");
                let mut sf = Findings::default();
                let parsed = sentence(&mut sf, item, &pointer);
                sentence_errors.append(&mut sf.errors);
                f.warnings.append(&mut sf.warnings);
                if let Some(s) = parsed {
                    if sentences.iter().any(|known| known.key == s.key) {
                        f.warn(
                            pointer,
                            IssueCode::DuplicateSentence,
                            "Repeats an earlier sentence of this word; skipped.",
                        );
                    } else {
                        sentences.push(s);
                    }
                }
            }
        }
        Some(_) => f.error(
            format!("{base}/sentences"),
            IssueCode::Invalid,
            "sentences must be an array.",
        ),
    }

    let fields = (headword, meaning, czech, usefulness, forms);
    match fields {
        (Some(headword), Some(meaning), Some(czech), Some(usefulness), Some(forms))
            if f.errors.is_empty() =>
        {
            if sentences.len() < MIN_SENTENCES_FOR_SENTENCE_CARD {
                f.warn(
                    format!("{base}/sentences"),
                    IssueCode::FewSentences,
                    format!(
                        "Only {} valid sentence(s); the word will use the word-only card until it has {MIN_SENTENCES_FOR_SENTENCE_CARD}.",
                        sentences.len()
                    ),
                );
            }
            WordResult::Valid(Box::new(PackWord {
                index,
                key: headword_key(&headword),
                headword,
                grammar,
                forms,
                pronunciation,
                speak,
                meaning,
                czech,
                usefulness: u8::try_from(usefulness).unwrap_or(1),
                goal,
                accepted,
                confusables,
                hint,
                spelling,
                spelling_note,
                note,
                tag,
                icon,
                seq,
                sentences,
                sentence_errors,
                warnings: f.warnings,
            }))
        }
        (headword, ..) => {
            let mut errors = f.errors;
            // Sentence errors of a dropped word are still worth showing.
            errors.extend(sentence_errors);
            WordResult::Invalid(InvalidWord {
                index,
                headword,
                errors,
                warnings: f.warnings,
            })
        }
    }
}

fn forms(f: &mut Findings, w: &Map<String, Value>, base: &str) -> Option<Vec<PackForm>> {
    let pointer = format!("{base}/forms");
    let items = match w.get("forms") {
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => {
            f.error(pointer, IssueCode::Invalid, "forms must be an array.");
            return None;
        }
    };
    if items.len() > MAX_LIST_ITEMS {
        f.error(
            &pointer,
            IssueCode::Invalid,
            format!("forms may have at most {MAX_LIST_ITEMS} items."),
        );
        return None;
    }
    let before = f.errors.len();
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let item_base = format!("{pointer}/{i}");
        let Some(obj) = item.as_object() else {
            f.error(
                item_base,
                IssueCode::Invalid,
                "A form must be an object {\"form\": …, \"common\": …}.",
            );
            continue;
        };
        f.unknown(obj, &item_base, FORM_KEYS);
        let form = f
            .required_text(obj, &item_base, "form", 1, 100)
            .filter(|form| {
                let blank = form.trim().is_empty();
                if blank {
                    f.warn(
                        format!("{item_base}/form"),
                        IssueCode::Advice,
                        "A blank form is ignored.",
                    );
                }
                !blank
            });
        let common = match obj.get("common") {
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => {
                f.error(
                    format!("{item_base}/common"),
                    IssueCode::Invalid,
                    "common must be true or false.",
                );
                None
            }
            None => {
                f.error(
                    format!("{item_base}/common"),
                    IssueCode::Missing,
                    "common is missing.",
                );
                None
            }
        };
        if let (Some(form), Some(common)) = (form, common) {
            out.push(PackForm { form, common });
        }
    }
    (f.errors.len() == before).then_some(out)
}

/// The word's `tag`. Leaving it out and `null` both say nothing about the
/// tag (an existing one stays, R-03); only a tag object sets one.
fn tag(f: &mut Findings, w: &Map<String, Value>, base: &str) -> Option<PackTag> {
    let pointer = format!("{base}/tag");
    match w.get("tag") {
        None | Some(Value::Null) => None,
        Some(Value::Object(t)) => {
            f.unknown(t, &pointer, TAG_KEYS);
            let kind = match t.get("kind") {
                Some(Value::String(k)) if TagKind::parse(k).is_some() => TagKind::parse(k),
                None => {
                    f.error(
                        format!("{pointer}/kind"),
                        IssueCode::Missing,
                        "tag.kind is missing.",
                    );
                    None
                }
                Some(_) => {
                    f.error(
                        format!("{pointer}/kind"),
                        IssueCode::Invalid,
                        format!("tag.kind must be one of {}.", TagKind::ALL.join(", ")),
                    );
                    None
                }
            };
            let text = f.optional_text(t, &pointer, "text", 300);
            // On an error the word is dropped, so `None` is never used.
            kind.map(|kind| PackTag { kind, text })
        }
        Some(_) => {
            f.error(
                pointer,
                IssueCode::Invalid,
                "tag must be null or an object {\"kind\": …, \"text\": …}.",
            );
            None
        }
    }
}

fn sentence(f: &mut Findings, value: &Value, base: &str) -> Option<PackSentence> {
    let Some(s) = value.as_object() else {
        f.error(
            base,
            IssueCode::Invalid,
            "A sentence must be a JSON object.",
        );
        return None;
    };
    f.unknown(s, base, SENTENCE_KEYS);
    let en = marked_text(f, s, base, "en");
    let cs = marked_text(f, s, base, "cs");
    if let Some(cs) = &cs
        && !strip_markers(cs).chars().any(char::is_alphabetic)
    {
        f.error(
            format!("{base}/cs"),
            IssueCode::Invalid,
            "cs must be a Czech translation (it contains no letters).",
        );
    }
    let origin = match s.get("origin") {
        Some(Value::String(o)) if Origin::parse(o).is_some() => Origin::parse(o),
        None => {
            f.error(
                format!("{base}/origin"),
                IssueCode::Missing,
                "origin is missing.",
            );
            None
        }
        Some(_) => {
            f.error(
                format!("{base}/origin"),
                IssueCode::Invalid,
                format!("origin must be one of {}.", Origin::ALL.join(", ")),
            );
            None
        }
    };
    // A page number may arrive as a JSON number; it is kept as its text.
    let location = match s.get("location") {
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => f.optional_text(s, base, "location", 100),
    };
    if !f.errors.is_empty() {
        return None;
    }
    let en = en?;
    Some(PackSentence {
        key: sentence_key(&en),
        en,
        cs: cs?,
        origin: origin?,
        location,
    })
}

fn marked_text(f: &mut Findings, s: &Map<String, Value>, base: &str, key: &str) -> Option<String> {
    let text = f.required_text(s, base, key, 1, 600)?;
    match marked::split(&text) {
        Ok(_) => Some(text),
        Err(err) => {
            f.error(
                format!("{base}/{key}"),
                IssueCode::Marker,
                format!("{key}: {err}."),
            );
            None
        }
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Errors and warnings of one scope.
#[derive(Default)]
struct Findings {
    errors: Vec<Issue>,
    warnings: Vec<Issue>,
}

impl Findings {
    fn error(&mut self, pointer: impl Into<String>, code: IssueCode, message: impl Into<String>) {
        self.errors.push(Issue {
            pointer: pointer.into(),
            code,
            message: message.into(),
        });
    }

    fn warn(&mut self, pointer: impl Into<String>, code: IssueCode, message: impl Into<String>) {
        self.warnings.push(Issue {
            pointer: pointer.into(),
            code,
            message: message.into(),
        });
    }

    /// One warning per unknown property, at most [`MAX_UNKNOWN_LISTED`] per
    /// object plus one summary line (bounded report size).
    fn unknown(&mut self, obj: &Map<String, Value>, base: &str, known: &[&str]) {
        let unknown: Vec<&String> = obj
            .keys()
            .filter(|k| !known.contains(&k.as_str()))
            .collect();
        for key in unknown.iter().take(MAX_UNKNOWN_LISTED) {
            self.warn(
                format!("{base}/{}", escape_pointer(key)),
                IssueCode::UnknownProperty,
                format!("Unknown property \"{}\" is ignored.", printable(key)),
            );
        }
        if unknown.len() > MAX_UNKNOWN_LISTED {
            self.warn(
                base,
                IssueCode::UnknownProperty,
                format!(
                    "… and {} more unknown properties, all ignored.",
                    unknown.len() - MAX_UNKNOWN_LISTED
                ),
            );
        }
    }

    /// A required string of `min..=max` characters; returned NFC-normalised.
    fn required_text(
        &mut self,
        obj: &Map<String, Value>,
        base: &str,
        key: &str,
        min: usize,
        max: usize,
    ) -> Option<String> {
        let pointer = format!("{base}/{key}");
        match obj.get(key) {
            None => {
                self.error(pointer, IssueCode::Missing, format!("{key} is missing."));
                None
            }
            Some(Value::String(s)) => self.text(s, &pointer, key, min, max),
            Some(_) => {
                self.error(
                    pointer,
                    IssueCode::Invalid,
                    format!("{key} must be a string."),
                );
                None
            }
        }
    }

    /// An optional string or null of at most `max` characters; blank is
    /// treated like absent. Returned NFC-normalised.
    fn optional_text(
        &mut self,
        obj: &Map<String, Value>,
        base: &str,
        key: &str,
        max: usize,
    ) -> Option<String> {
        let pointer = format!("{base}/{key}");
        match obj.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => self
                .text(s, &pointer, key, 0, max)
                .filter(|t| !t.trim().is_empty()),
            Some(_) => {
                self.error(
                    pointer,
                    IssueCode::Invalid,
                    format!("{key} must be a string or null."),
                );
                None
            }
        }
    }

    /// One text value: its length is counted as written (like the schema
    /// does), then line breaks are unified and the text NFC-normalised.
    fn text(
        &mut self,
        s: &str,
        pointer: &str,
        key: &str,
        min: usize,
        max: usize,
    ) -> Option<String> {
        let chars = s.chars().count();
        let unified;
        let s = if s.contains('\r') {
            unified = s.replace("\r\n", "\n").replace('\r', "\n");
            unified.as_str()
        } else {
            s
        };
        if s.chars().any(is_forbidden_control) {
            self.error(
                pointer,
                IssueCode::ControlCharacter,
                format!(
                    "{key} contains control characters (only line breaks and tabs are allowed)."
                ),
            );
            return None;
        }
        if chars < min {
            self.error(pointer, IssueCode::Invalid, format!("{key} is empty."));
            return None;
        }
        if chars > max {
            self.error(
                pointer,
                IssueCode::Invalid,
                format!("{key} is too long ({chars} characters; at most {max})."),
            );
            return None;
        }
        Some(nfc(s))
    }

    /// A list of 1–200 character strings, at most [`MAX_LIST_ITEMS`] items.
    /// Blank (whitespace-only) items are dropped with a warning.
    /// `None` = missing (only an error when required) or invalid.
    fn text_list(
        &mut self,
        obj: &Map<String, Value>,
        base: &str,
        key: &str,
        required: bool,
    ) -> Option<Vec<String>> {
        let pointer = format!("{base}/{key}");
        let items = match obj.get(key) {
            None | Some(Value::Null) => {
                if required {
                    self.error(pointer, IssueCode::Missing, format!("{key} is missing."));
                }
                return None;
            }
            Some(Value::Array(items)) => items,
            Some(_) => {
                self.error(
                    pointer,
                    IssueCode::Invalid,
                    format!("{key} must be an array of strings."),
                );
                return None;
            }
        };
        if items.len() > MAX_LIST_ITEMS {
            self.error(
                pointer,
                IssueCode::Invalid,
                format!("{key} may have at most {MAX_LIST_ITEMS} items."),
            );
            return None;
        }
        let before = self.errors.len();
        let mut out = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let item_pointer = format!("{pointer}/{i}");
            match item {
                Value::String(s) if !s.is_empty() && s.trim().is_empty() => self.warn(
                    item_pointer,
                    IssueCode::Advice,
                    format!("A blank item of {key} is ignored."),
                ),
                Value::String(s) => {
                    if let Some(t) = self.text(s, &item_pointer, key, 1, 200) {
                        out.push(t);
                    }
                }
                _ => self.error(
                    item_pointer,
                    IssueCode::Invalid,
                    format!("Every item of {key} must be a string."),
                ),
            }
        }
        (self.errors.len() == before).then_some(out)
    }

    fn required_int(
        &mut self,
        obj: &Map<String, Value>,
        base: &str,
        key: &str,
        min: i64,
        max: i64,
    ) -> Option<i64> {
        let pointer = format!("{base}/{key}");
        match obj.get(key) {
            None => {
                self.error(pointer, IssueCode::Missing, format!("{key} is missing."));
                None
            }
            Some(v) => match integer(v) {
                Some(n) if (min..=max).contains(&n) => Some(n),
                _ => {
                    self.error(
                        pointer,
                        IssueCode::Invalid,
                        format!("{key} must be an integer from {min} to {max}."),
                    );
                    None
                }
            },
        }
    }
}

/// A JSON integer (JSON Schema counts `7.0` as an integer too).
fn integer(value: &Value) -> Option<i64> {
    let n = value.as_number()?;
    if let Some(i) = n.as_i64() {
        return Some(i);
    }
    let f = n.as_f64()?;
    // Exact integral values in a range where the conversion is lossless.
    let in_range = f.fract() == 0.0 && f.abs() < 9.0e15;
    // `f` is integral and well inside the i64 range: the cast is exact.
    in_range.then_some(f as i64)
}

fn as_u32(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn is_forbidden_control(c: char) -> bool {
    c.is_control() && c != '\n' && c != '\t'
}

/// `^[a-z0-9]+(-[a-z0-9]+)*$`
fn is_slug(text: &str) -> bool {
    !text.is_empty()
        && text.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Whitespace-only, or nothing but whitespace and the invisible characters
/// the identity keys drop.
fn is_blank(text: &str) -> bool {
    headword_key(text).is_empty()
}

/// `http://…` or `https://…` (scheme in any case).
fn is_web_url(url: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        url.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

/// Year, month and day of a text shaped like the schema's date pattern
/// `^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$`.
fn date_parts(text: &str) -> Option<(u32, u32, u32)> {
    let b = text.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let number = |r: std::ops::Range<usize>| {
        b[r].iter().try_fold(0u32, |n, d| {
            d.is_ascii_digit().then(|| n * 10 + u32::from(d - b'0'))
        })
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some((year, month, day))
}

fn is_date_shaped(text: &str) -> bool {
    date_parts(text).is_some()
}

/// A real calendar date written as `YYYY-MM-DD` (30 February is none).
fn is_date(text: &str) -> bool {
    date_parts(text).is_some_and(|(year, month, day)| {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let days = match month {
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 31,
        };
        day <= days
    })
}

/// `<slug>--<digits YYYY-MM-DD>--<n>` with n = `[1-9][0-9]{0,3}` (the
/// schema's pattern; the date part is only checked for its digit layout).
fn parse_pack_id(id: &str) -> Option<(String, String, u32)> {
    let (rest, number) = id.rsplit_once("--")?;
    let (slug, date) = rest.rsplit_once("--")?;
    let date_ok = date.len() == 10
        && date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    let number_ok = (1..=4).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit())
        && !number.starts_with('0');
    (is_slug(slug) && date_ok && number_ok).then(|| {
        (
            slug.to_owned(),
            date.to_owned(),
            number.parse().unwrap_or(1),
        )
    })
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// A property name safe to show (no control characters, bounded length).
fn printable(key: &str) -> String {
    let cleaned: String = key.chars().filter(|c| !c.is_control()).take(60).collect();
    if cleaned.chars().count() < key.chars().count() {
        format!("{cleaned}…")
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r10_pack_id_pattern() {
        assert_eq!(
            parse_pack_id("test-novel-2--2026-11-17--3"),
            Some(("test-novel-2".to_owned(), "2026-11-17".to_owned(), 3))
        );
        for bad in [
            "test--2026-11-17--0",
            "test--2026-11-17--01",
            "test--2026-11-17--10000",
            "Test--2026-11-17--1",
            "test-novel--2026-1-17--1",
            "test--novel--2026-11-17",
            "test-novel-2026-11-17-1",
            "-test--2026-11-17--1",
            "test_novel--2026-11-17--1",
        ] {
            assert_eq!(parse_pack_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn r10_dates_and_slugs() {
        assert!(is_date("2026-11-17"));
        assert!(!is_date("2026-13-01"));
        assert!(!is_date("2026-00-10"));
        assert!(!is_date("2026-11-32"));
        assert!(!is_date("2026-11-1"));
        assert!(!is_date("2026/11/17"));
        // A7: shaped like a date is not enough.
        for (text, real) in [
            ("2026-02-28", true),
            ("2026-02-29", false),
            ("2028-02-29", true),
            ("2100-02-29", false),
            ("2000-02-29", true),
            ("2026-04-31", false),
            ("2026-12-31", true),
        ] {
            assert!(is_date_shaped(text), "{text}");
            assert_eq!(is_date(text), real, "{text}");
        }
        assert!(is_web_url("https://test.example/a"));
        assert!(is_web_url("HTTP://test.example"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("ftp://test.example"));
        assert!(!is_web_url("test.example"));
        assert!(is_blank(" \u{a0}\u{200b}\u{feff}"));
        assert!(!is_blank(" a "));
        assert!(is_slug("test-a1"));
        assert!(!is_slug("test--a"));
        assert!(!is_slug("test-"));
        assert!(!is_slug("Test"));
    }

    #[test]
    fn r10_integers_accept_integral_numbers_only() {
        assert_eq!(integer(&serde_json::json!(7)), Some(7));
        assert_eq!(integer(&serde_json::json!(7.0)), Some(7));
        assert_eq!(integer(&serde_json::json!(7.5)), None);
        assert_eq!(integer(&serde_json::json!("7")), None);
    }
}
