//! Content hash of a pack (design §3.1): SHA-256 over a canonical
//! serialisation, so the same pack pasted again (re-indented, keys in another
//! order) has the same hash. Together with `pack_id` it is the idempotency
//! key of an import (R-11).
//!
//! The canonical form is written here rather than relying on
//! `serde_json::Value`'s map order, which depends on a cargo feature
//! (`preserve_order`) that another crate in the build could switch on.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of the canonical serialisation of `value`.
pub fn content_sha256(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    hex::encode(Sha256::digest(out.as_bytes()))
}

/// Compact JSON with object keys sorted by code point (= UTF-8 byte order).
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(item, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).expect("json")
    }

    #[test]
    fn r11_hash_ignores_formatting_and_key_order() {
        let a = parse(r#"{"b": [1, {"y": "ž", "x": null}], "a": true}"#);
        let b = parse("{\n  \"a\": true,\n  \"b\": [1, {\"x\": null, \"y\": \"\\u017e\"}]\n}");
        assert_eq!(content_sha256(&a), content_sha256(&b));
        assert_eq!(content_sha256(&a).len(), 64);
    }

    #[test]
    fn r11_hash_changes_with_content() {
        let a = parse(r#"{"a": [1, 2]}"#);
        let b = parse(r#"{"a": [2, 1]}"#);
        let c = parse(r#"{"a": [1, 2], "b": null}"#);
        assert_ne!(content_sha256(&a), content_sha256(&b));
        assert_ne!(content_sha256(&a), content_sha256(&c));
    }

    #[test]
    fn r11_canonical_form_is_compact_and_sorted() {
        let mut out = String::new();
        write_canonical(&parse(r#"{"é": 1, "z": "q\"", "a": [1.5, -2]}"#), &mut out);
        assert_eq!(out, r#"{"a":[1.5,-2],"z":"q\"","é":1}"#);
    }
}
