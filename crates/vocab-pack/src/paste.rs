//! Splitting a paste into packs (design §3.1, §5.1, §5.2 "Paste" row).
//!
//! The owner copies Claude's answer, which usually holds one or several
//! ```` ```json ```` code blocks with prose between them. When the paste
//! contains code fences, only the fenced blocks are read and the prose around
//! them is ignored. Without fences the whole paste must be JSON. Each block
//! (or the whole paste) is read as a stream of JSON values; every value
//! becomes one [`RawPack`] with its content hash.
//!
//! The paste is untrusted and may be hostile (R-84): a JSON value is first
//! only *measured* (its length in bytes, how many values it holds, how deep
//! it nests) without building anything, and is turned into a
//! [`serde_json::Value`] only when it is within the limits below. Two
//! megabytes of `[0,0,0,…]` therefore cost a scan, not tens of megabytes of
//! memory.

use std::fmt;

use serde::de::{Deserialize, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Deserializer, Value};

use crate::hash::content_sha256;

/// Largest accepted paste (bytes of UTF-8 text).
pub const MAX_PASTE_BYTES: usize = 2 * 1024 * 1024;
/// Largest accepted single pack (bytes of its JSON text).
pub const MAX_PACK_BYTES: usize = 1024 * 1024;
/// Most packs read from one paste.
pub const MAX_PACKS: usize = 20;
/// Most JSON values (objects, arrays, strings, numbers, …; keys not counted)
/// in one pack. The largest pack the format allows (100 words with every
/// list full) has about 45 000.
pub const MAX_PACK_VALUES: usize = 60_000;
/// Most JSON values in all packs of one paste together.
pub const MAX_PASTE_VALUES: usize = 200_000;
/// Deepest nesting of a pack (pack → words → word → sentences → sentence →
/// text is 6).
pub const MAX_PACK_DEPTH: usize = 8;

/// One JSON value found in the paste.
#[derive(Debug, Clone, PartialEq)]
pub struct RawPack {
    pub value: Value,
    /// [`content_sha256`] of `value`.
    pub content_sha256: String,
}

/// A part of the paste that could not be read as a pack. It is reported in
/// place of the pack it should have been; the other packs are unaffected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteItemError {
    pub message: String,
}

/// The paste as a whole cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteError {
    Empty,
    TooLarge {
        bytes: usize,
    },
    /// More than [`MAX_PACKS`] JSON values. Reading stops at the first value
    /// over the limit, so a paste of many tiny values (`1 1 1 …`) can never
    /// turn into a huge report.
    TooManyPacks,
    /// More than [`MAX_PASTE_VALUES`] JSON values in all packs together.
    TooManyValues,
}

impl std::error::Error for PasteError {}

impl std::fmt::Display for PasteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("Nothing to import: the paste contains no JSON."),
            Self::TooLarge { bytes } => write!(
                f,
                "The paste is too large ({bytes} bytes; at most {MAX_PASTE_BYTES}). Import fewer packs at a time."
            ),
            Self::TooManyPacks => write!(
                f,
                "The paste holds more than {MAX_PACKS} packs (JSON values). Import at most {MAX_PACKS} at a time."
            ),
            Self::TooManyValues => write!(
                f,
                "The paste holds more than {MAX_PASTE_VALUES} JSON values. Import fewer packs at a time."
            ),
        }
    }
}

/// Split `text` into packs, in paste order.
pub fn split_paste(text: &str) -> Result<Vec<Result<RawPack, PasteItemError>>, PasteError> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(PasteError::TooLarge { bytes: text.len() });
    }
    let segments = match fenced_blocks(text) {
        Some(blocks) => blocks,
        None => vec![Segment {
            label: "the paste".to_owned(),
            text,
        }],
    };
    let mut items = Vec::new();
    let mut values_left = MAX_PASTE_VALUES;
    for segment in &segments {
        read_values(segment, &mut items, &mut values_left)?;
    }
    if items.is_empty() {
        return Err(PasteError::Empty);
    }
    Ok(items)
}

struct Segment<'a> {
    /// Where the segment is, for messages: "code block 2" or "the paste".
    label: String,
    text: &'a str,
}

/// The contents of all fenced code blocks, or `None` when the text has no
/// fence at all. A fence is a line starting (after indentation) with three or
/// more backticks or tildes; it closes at a line holding only at least as
/// many of the same character. An unclosed block runs to the end.
fn fenced_blocks(text: &str) -> Option<Vec<Segment<'_>>> {
    let mut blocks = Vec::new();
    let mut open: Option<(char, usize, usize)> = None; // (fence char, length, content start)
    let mut found_fence = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed = line.trim();
        match open {
            None => {
                if let Some((fence, len)) = fence_of(trimmed) {
                    found_fence = true;
                    open = Some((fence, len, offset));
                }
            }
            Some((fence, len, start)) => {
                let closes = trimmed.len() >= len && trimmed.chars().all(|c| c == fence);
                if closes {
                    blocks.push(Segment {
                        label: format!("code block {}", blocks.len() + 1),
                        text: &text[start..line_start],
                    });
                    open = None;
                }
            }
        }
    }
    if let Some((_, _, start)) = open {
        blocks.push(Segment {
            label: format!("code block {}", blocks.len() + 1),
            text: &text[start..],
        });
    }
    found_fence.then_some(blocks)
}

fn fence_of(trimmed_line: &str) -> Option<(char, usize)> {
    let first = trimmed_line.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let len = trimmed_line.chars().take_while(|&c| c == first).count();
    (len >= 3).then_some((first, len))
}

/// Size of one JSON value, measured while skipping over it: nothing is
/// built, whatever the value holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    /// Values in it, itself included (object keys are not counted).
    values: usize,
    /// 1 for a scalar or an empty container.
    depth: usize,
}

impl Shape {
    const SCALAR: Self = Self {
        values: 1,
        depth: 1,
    };

    fn hold(&mut self, child: Self) {
        self.values = self.values.saturating_add(child.values);
        self.depth = self.depth.max(child.depth.saturating_add(1));
    }
}

impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ShapeVisitor)
    }
}

struct ShapeVisitor;

impl<'de> Visitor<'de> for ShapeVisitor {
    type Value = Shape;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Shape, E> {
        Ok(Shape::SCALAR)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Shape, E> {
        Ok(Shape::SCALAR)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Shape, E> {
        Ok(Shape::SCALAR)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Shape, E> {
        Ok(Shape::SCALAR)
    }

    fn visit_str<E>(self, _: &str) -> Result<Shape, E> {
        Ok(Shape::SCALAR)
    }

    fn visit_unit<E>(self) -> Result<Shape, E> {
        Ok(Shape::SCALAR)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
        let mut shape = Shape::SCALAR;
        while let Some(child) = seq.next_element::<Shape>()? {
            shape.hold(child);
        }
        Ok(shape)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
        let mut shape = Shape::SCALAR;
        while map.next_key::<IgnoredAny>()?.is_some() {
            shape.hold(map.next_value::<Shape>()?);
        }
        Ok(shape)
    }
}

/// Append the values of `segment` to `items`; fails as soon as the paste
/// holds more than [`MAX_PACKS`] values, or its packs together more than
/// `values_left` JSON values. A value is only built when it is within the
/// per-pack limits; otherwise it is reported in place.
fn read_values(
    segment: &Segment<'_>,
    items: &mut Vec<Result<RawPack, PasteItemError>>,
    values_left: &mut usize,
) -> Result<(), PasteError> {
    let mut stream = Deserializer::from_str(segment.text).into_iter::<Shape>();
    let mut previous_end = 0;
    loop {
        let next = stream.next();
        if next.is_some() && items.len() >= MAX_PACKS {
            return Err(PasteError::TooManyPacks);
        }
        match next {
            None => return Ok(()),
            Some(Ok(shape)) => {
                let end = stream.byte_offset();
                let text = &segment.text[previous_end..end];
                previous_end = end;
                items.push(build(text, shape, &segment.label, values_left)?);
            }
            Some(Err(err)) => {
                // The stream cannot resynchronise after a syntax error, so the
                // rest of this segment is part of the reported error.
                items.push(Err(PasteItemError {
                    message: format!("{} is not valid JSON: {err}.", capitalise(&segment.label)),
                }));
                return Ok(());
            }
        }
    }
}

/// Turn the measured JSON `text` into a pack, or say why it is too big.
fn build(
    text: &str,
    shape: Shape,
    label: &str,
    values_left: &mut usize,
) -> Result<Result<RawPack, PasteItemError>, PasteError> {
    let refuse = |message: String| Ok(Err(PasteItemError { message }));
    let bytes = text.trim_start().len();
    if bytes > MAX_PACK_BYTES {
        return refuse(format!(
            "A pack in {label} is too large ({bytes} bytes; at most {MAX_PACK_BYTES})."
        ));
    }
    if shape.depth > MAX_PACK_DEPTH {
        return refuse(format!(
            "A JSON value in {label} is not a pack: it is nested {} levels deep (a pack has at most {MAX_PACK_DEPTH}).",
            shape.depth
        ));
    }
    if shape.values > MAX_PACK_VALUES {
        return refuse(format!(
            "A JSON value in {label} is not a pack: it holds {} values (a pack has at most {MAX_PACK_VALUES}).",
            shape.values
        ));
    }
    *values_left = values_left
        .checked_sub(shape.values)
        .ok_or(PasteError::TooManyValues)?;
    match serde_json::from_str::<Value>(text) {
        Ok(value) => {
            let content_sha256 = content_sha256(&value);
            Ok(Ok(RawPack {
                value,
                content_sha256,
            }))
        }
        // Cannot happen: the same text was just read as JSON.
        Err(err) => refuse(format!("{} is not valid JSON: {err}.", capitalise(label))),
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(text: &str) -> Vec<Result<Value, String>> {
        split_paste(text)
            .expect("paste usable")
            .into_iter()
            .map(|item| item.map(|p| p.value).map_err(|e| e.message))
            .collect()
    }

    #[test]
    fn r11_bare_json_and_streams_of_values() {
        assert_eq!(values(r#"{"a":1}"#), vec![Ok(serde_json::json!({"a": 1}))]);
        assert_eq!(
            values("{\"a\":1}\n\n{\"b\":2} "),
            vec![
                Ok(serde_json::json!({"a": 1})),
                Ok(serde_json::json!({"b": 2}))
            ]
        );
    }

    #[test]
    fn r11_fenced_blocks_with_prose_between_them() {
        let paste = "Here are the packs.\n\n```json\n{\"a\":1}\n```\n\nSome words about part 2 {not json}.\n\n```json\n{\"b\":2}\n```\n\n2 packs, 30 words. I was unsure about one word.\n";
        assert_eq!(
            values(paste),
            vec![
                Ok(serde_json::json!({"a": 1})),
                Ok(serde_json::json!({"b": 2}))
            ]
        );
    }

    #[test]
    fn r11_fences_without_language_tildes_indentation_and_unclosed_blocks() {
        let paste = "  ```\n{\"a\":1}\n  ```\n~~~~JSON\n{\"b\":2}\n~~~~\n````json\n{\"c\":3}";
        assert_eq!(values(paste).len(), 3);
        assert!(values(paste).iter().all(Result::is_ok));
    }

    #[test]
    fn r23_invalid_json_is_reported_in_place_and_other_blocks_survive() {
        let paste = "```json\n{\"a\":1,}\n```\ntext\n```json\n{\"b\":2}\n```";
        let got = values(paste);
        assert_eq!(got.len(), 2);
        let err = got[0].as_ref().expect_err("trailing comma");
        assert!(err.starts_with("Code block 1 is not valid JSON"), "{err}");
        assert_eq!(got[1], Ok(serde_json::json!({"b": 2})));
    }

    #[test]
    fn r23_prose_without_fences_is_an_error() {
        let got = values("Here is your pack: {\"a\":1}");
        assert_eq!(got.len(), 1);
        assert!(
            got[0]
                .as_ref()
                .expect_err("prose")
                .starts_with("The paste is not valid JSON")
        );
    }

    #[test]
    fn r84_empty_and_oversized_pastes_are_rejected() {
        assert_eq!(split_paste(""), Err(PasteError::Empty));
        assert_eq!(split_paste("  \n "), Err(PasteError::Empty));
        assert_eq!(
            split_paste("text\n```json\n\n```\n"),
            Err(PasteError::Empty)
        );
        let big = " ".repeat(MAX_PASTE_BYTES + 1);
        assert_eq!(
            split_paste(&big),
            Err(PasteError::TooLarge {
                bytes: MAX_PASTE_BYTES + 1
            })
        );
    }

    #[test]
    fn r84_oversized_pack_is_an_item_error_and_too_many_packs_a_paste_error() {
        let big = format!("{{\"a\":\"{}\"}}", "x".repeat(MAX_PACK_BYTES));
        let got = values(&format!("{big}\n{{\"b\":1}}"));
        assert!(got[0].as_ref().expect_err("too big").contains("too large"));
        assert!(got[1].is_ok());

        assert_eq!(values(&"{}\n".repeat(MAX_PACKS)).len(), MAX_PACKS);
        assert_eq!(
            split_paste(&"{}\n".repeat(MAX_PACKS + 1)),
            Err(PasteError::TooManyPacks)
        );
        // Many tiny values stop reading at the limit (no per-value output).
        let tiny = "1 ".repeat(MAX_PASTE_BYTES / 2);
        assert_eq!(split_paste(&tiny), Err(PasteError::TooManyPacks));
        // Invalid JSON after the limit is not even looked at.
        let text = format!("{}{{oops", "{}\n".repeat(MAX_PACKS));
        assert_eq!(split_paste(&text), Err(PasteError::TooManyPacks));
    }

    /// B5 (R-84, review probes p5/p6): hostile JSON is measured, never
    /// built. The assertions are on structure: what comes back holds no
    /// `Value` for an oversized shape, whatever its byte length.
    #[test]
    fn r84_values_over_the_structure_limits_are_refused_before_being_built() {
        // About 2 MiB of tiny values in one array: a million of them.
        let unit_count = (MAX_PASTE_BYTES - 8) / 2;
        let flat = format!("[{}0]", "0,".repeat(unit_count));
        assert!(flat.len() <= MAX_PASTE_BYTES);
        let got = values(&flat);
        assert_eq!(got.len(), 1);
        assert!(got[0].as_ref().expect_err("too big").contains("too large"));

        // Under the byte limit of a pack, but far more values than a pack has.
        let many = format!("[{}0]", "0,".repeat(MAX_PACK_VALUES));
        assert!(many.len() < MAX_PACK_BYTES);
        let got = values(&format!("{many}\n{{\"b\":1}}"));
        let err = got[0].as_ref().expect_err("too many values");
        assert!(
            err.contains("not a pack") && err.contains("values"),
            "{err}"
        );
        assert_eq!(
            got[1],
            Ok(serde_json::json!({"b": 1})),
            "the next pack survives"
        );
        // Other shapes of the same trick; some are refused for their length.
        for unit in ["{},", "[\"\"],", "{\"\":0},", "{\"\":{}},"] {
            let text = format!("[{}0]", unit.repeat(MAX_PACK_VALUES));
            assert!(values(&text)[0].is_err(), "{unit}");
        }

        // Exactly at the limit is fine (the array itself counts as one).
        let at_limit = format!("[{}0]", "0,".repeat(MAX_PACK_VALUES - 2));
        assert!(values(&at_limit)[0].is_ok());

        // Deep nesting.
        let deep = format!(
            "{}{}",
            "[".repeat(MAX_PACK_DEPTH + 1),
            "]".repeat(MAX_PACK_DEPTH + 1)
        );
        let err = values(&deep).remove(0).expect_err("too deep");
        assert!(err.contains("nested"), "{err}");
        let ok = format!(
            "{}{}",
            "[".repeat(MAX_PACK_DEPTH),
            "]".repeat(MAX_PACK_DEPTH)
        );
        assert!(values(&ok)[0].is_ok());
        // Deeper than the parser's own limit is a syntax error, not a crash.
        let abyss = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert!(values(&abyss)[0].is_err());
    }

    #[test]
    fn r84_all_packs_of_a_paste_share_one_value_budget() {
        // Each pack is within the per-pack limits; together they are not.
        let pack = format!("[{}0]", "0,".repeat(MAX_PACK_VALUES - 2));
        let fitting = MAX_PASTE_VALUES / MAX_PACK_VALUES;
        let paste = format!("{pack}\n").repeat(fitting);
        assert_eq!(values(&paste).len(), fitting);
        let paste = format!("{pack}\n").repeat(fitting + 1);
        assert!(paste.len() <= MAX_PASTE_BYTES);
        assert_eq!(split_paste(&paste), Err(PasteError::TooManyValues));
    }

    #[test]
    fn r84_shape_counts_values_and_depth_without_building_them() {
        let shape = |text: &str| serde_json::from_str::<Shape>(text).expect("json");
        assert_eq!(
            shape("1"),
            Shape {
                values: 1,
                depth: 1
            }
        );
        assert_eq!(
            shape("\"a\\nb\""),
            Shape {
                values: 1,
                depth: 1
            }
        );
        assert_eq!(
            shape("null"),
            Shape {
                values: 1,
                depth: 1
            }
        );
        assert_eq!(
            shape("[]"),
            Shape {
                values: 1,
                depth: 1
            }
        );
        assert_eq!(
            shape("[1, [2, 3], {\"a\": [true]}]"),
            Shape {
                values: 8,
                depth: 4
            }
        );
        assert_eq!(
            shape("{\"a\": {\"b\": null}, \"c\": 1.5}"),
            Shape {
                values: 4,
                depth: 3
            }
        );
    }

    #[test]
    fn r11_each_pack_carries_its_content_hash() {
        let items = split_paste("{\"a\":1}\n{ \"a\" : 1 }").expect("paste");
        let hashes: Vec<String> = items
            .into_iter()
            .map(|i| i.expect("pack").content_sha256)
            .collect();
        assert_eq!(hashes[0], hashes[1]);
    }
}
