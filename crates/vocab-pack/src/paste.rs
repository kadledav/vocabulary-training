//! Splitting a paste into packs (design §3.1, §5.1, §5.2 "Paste" row).
//!
//! The owner copies Claude's answer, which usually holds one or several
//! ```` ```json ```` code blocks with prose between them. When the paste
//! contains code fences, only the fenced blocks are read and the prose around
//! them is ignored. Without fences the whole paste must be JSON. Each block
//! (or the whole paste) is read as a stream of JSON values; every value
//! becomes one [`RawPack`] with its content hash.

use serde_json::{Deserializer, Value};

use crate::hash::content_sha256;

/// Largest accepted paste (bytes of UTF-8 text).
pub const MAX_PASTE_BYTES: usize = 2 * 1024 * 1024;
/// Largest accepted single pack (bytes of its JSON text).
pub const MAX_PACK_BYTES: usize = 1024 * 1024;
/// Most packs read from one paste.
pub const MAX_PACKS: usize = 20;

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
    for segment in &segments {
        read_values(segment, &mut items)?;
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

/// Append the values of `segment` to `items`; fails as soon as the paste
/// holds more than [`MAX_PACKS`] values.
fn read_values(
    segment: &Segment<'_>,
    items: &mut Vec<Result<RawPack, PasteItemError>>,
) -> Result<(), PasteError> {
    let mut stream = Deserializer::from_str(segment.text).into_iter::<Value>();
    let mut previous_end = 0;
    loop {
        let next = stream.next();
        if next.is_some() && items.len() >= MAX_PACKS {
            return Err(PasteError::TooManyPacks);
        }
        match next {
            None => return Ok(()),
            Some(Ok(value)) => {
                let end = stream.byte_offset();
                let bytes = end - previous_end;
                previous_end = end;
                if bytes > MAX_PACK_BYTES {
                    items.push(Err(PasteItemError {
                        message: format!(
                            "A pack in {} is too large ({bytes} bytes; at most {MAX_PACK_BYTES}).",
                            segment.label
                        ),
                    }));
                } else {
                    let content_sha256 = content_sha256(&value);
                    items.push(Ok(RawPack {
                        value,
                        content_sha256,
                    }));
                }
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
