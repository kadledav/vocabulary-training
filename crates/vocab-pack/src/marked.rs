//! The `[[…]]` target-span scanner (design §5.2, R-20, R-23).
//!
//! A marked text contains exactly one target span: the scanner finds the
//! first `[[`, then the first `]]` after it, and requires that no other `[[`
//! or `]]` occurs anywhere in the text. The span must not be empty, must
//! not start or end with whitespace and must not start with `]` (`[[]x]]`
//! is a typing slip, not a target). Text outside the markers may be empty.
//! This is the same rule as the `markedText` pattern of the JSON Schema.

use std::fmt;

const OPEN: &str = "[[";
const CLOSE: &str = "]]";

/// A text split at its target span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkedText {
    /// Text before the span (may be empty).
    pub pre: String,
    /// The marked span, without the brackets.
    pub target: String,
    /// Text after the span (may be empty).
    pub post: String,
}

/// Why a text is not a valid marked text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkError {
    /// No `[[` at all.
    NoSpan,
    /// A `[[` without a `]]` after it.
    Unclosed,
    /// A `]]` before the first `[[`.
    CloseBeforeOpen,
    /// A second `[[` or `]]` somewhere (two spans, nesting, stray brackets).
    MoreThanOneSpan,
    /// `[[]]`.
    EmptySpan,
    /// `[[ word]]` or `[[word ]]`.
    SpanWhitespace,
    /// `[[]word]]`.
    SpanBracket,
}

impl fmt::Display for MarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoSpan => "the target is not marked: exactly one [[…]] span is required",
            Self::Unclosed => "the [[ of the target span is never closed with ]]",
            Self::CloseBeforeOpen => "]] appears before the [[ of the target span",
            Self::MoreThanOneSpan => {
                "more than one [[ or ]]: exactly one [[…]] span is allowed (use … for omissions)"
            }
            Self::EmptySpan => "the target span [[]] is empty",
            Self::SpanWhitespace => {
                "the target span starts or ends with whitespace inside the brackets"
            }
            Self::SpanBracket => "the target span starts with ] (write [[word]])",
        })
    }
}

impl std::error::Error for MarkError {}

impl MarkedText {
    /// The text without the markers.
    pub fn plain(&self) -> String {
        let mut out = String::with_capacity(self.pre.len() + self.target.len() + self.post.len());
        out.push_str(&self.pre);
        out.push_str(&self.target);
        out.push_str(&self.post);
        out
    }
}

/// Split `text` at its single `[[…]]` span.
pub fn split(text: &str) -> Result<MarkedText, MarkError> {
    let Some(open) = text.find(OPEN) else {
        return Err(if text.contains(CLOSE) {
            MarkError::CloseBeforeOpen
        } else {
            MarkError::NoSpan
        });
    };
    let after_open = open + OPEN.len();
    let Some(close_rel) = text[after_open..].find(CLOSE) else {
        return Err(MarkError::Unclosed);
    };
    let close = after_open + close_rel;
    let pre = &text[..open];
    let target = &text[after_open..close];
    let post = &text[close + CLOSE.len()..];
    if pre.contains(CLOSE) {
        return Err(MarkError::CloseBeforeOpen);
    }
    // `pre` cannot contain `[[` (first occurrence) and `target` cannot
    // contain `]]` (first occurrence after the opening).
    if target.contains(OPEN) || post.contains(OPEN) || post.contains(CLOSE) {
        return Err(MarkError::MoreThanOneSpan);
    }
    if target.is_empty() {
        return Err(MarkError::EmptySpan);
    }
    if target.starts_with(char::is_whitespace) || target.ends_with(char::is_whitespace) {
        return Err(MarkError::SpanWhitespace);
    }
    if target.starts_with(']') {
        return Err(MarkError::SpanBracket);
    }
    Ok(MarkedText {
        pre: pre.to_owned(),
        target: target.to_owned(),
        post: post.to_owned(),
    })
}

/// `text` with every `[[` and `]]` removed (for keys and letter checks).
pub fn strip_markers(text: &str) -> String {
    text.replace(OPEN, "").replace(CLOSE, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> (String, String, String) {
        let m = split(text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        (m.pre, m.target, m.post)
    }

    fn s(v: &str) -> String {
        v.to_owned()
    }

    #[test]
    fn r23_single_span_is_split() {
        assert_eq!(
            ok("Heavy boots [[thudded]] on the stairs."),
            (s("Heavy boots "), s("thudded"), s(" on the stairs."))
        );
        assert_eq!(ok("[[x]]"), (s(""), s("x"), s("")));
        assert_eq!(
            ok("She was [[poring over]] maps"),
            (s("She was "), s("poring over"), s(" maps"))
        );
        let m = split("A [[b]] c").expect("valid");
        assert_eq!(m.plain(), "A b c");
    }

    #[test]
    fn r23_single_brackets_and_overlaps_follow_the_scanner_rule() {
        // Single brackets are ordinary text.
        assert_eq!(ok("a [b] [[c]] d"), (s("a [b] "), s("c"), s(" d")));
        // First `[[`, first `]]` after it; a third bracket is ordinary text.
        assert_eq!(ok("[[[word]]"), (s(""), s("[word"), s("")));
        assert_eq!(ok("[[word]]]"), (s(""), s("word"), s("]")));
    }

    #[test]
    fn r23_missing_or_broken_spans_are_rejected() {
        assert_eq!(split("No marker here."), Err(MarkError::NoSpan));
        assert_eq!(split(""), Err(MarkError::NoSpan));
        assert_eq!(split("Open [[only"), Err(MarkError::Unclosed));
        assert_eq!(split("Close only]] here"), Err(MarkError::CloseBeforeOpen));
        assert_eq!(split("x]] then [[y]]"), Err(MarkError::CloseBeforeOpen));
        assert_eq!(split("[[]]"), Err(MarkError::EmptySpan));
        assert_eq!(split("a [[ word]] b"), Err(MarkError::SpanWhitespace));
        assert_eq!(split("a [[word ]] b"), Err(MarkError::SpanWhitespace));
        assert_eq!(split("a [[ ]] b"), Err(MarkError::SpanWhitespace));
    }

    /// A8 (review finding): the schema's pattern never allowed a span that
    /// starts with `]`; the scanner agrees now.
    #[test]
    fn r23_span_starting_with_a_closing_bracket_is_rejected() {
        assert_eq!(split("A loud [[]thud]] came."), Err(MarkError::SpanBracket));
        assert_eq!(split("[[]x]]"), Err(MarkError::SpanBracket));
        // A bracket elsewhere in the span is ordinary text.
        assert_eq!(ok("[[x[]]"), (s(""), s("x["), s("")));
    }

    #[test]
    fn r23_sentence_with_two_spans_is_rejected() {
        assert_eq!(split("[[a]] and [[b]]"), Err(MarkError::MoreThanOneSpan));
        assert_eq!(split("[[a [[b]] c]]"), Err(MarkError::MoreThanOneSpan));
        assert_eq!(split("[[a]] b]]"), Err(MarkError::MoreThanOneSpan));
        assert_eq!(split("[[a]] [[b"), Err(MarkError::MoreThanOneSpan));
        // Bracketed elision is a second marker pair: rejected (P-3).
        assert_eq!(
            split("He [[thudded]] down […] the hall [[x"),
            Err(MarkError::MoreThanOneSpan)
        );
    }

    /// Property-style check over generated strings: whenever `split`
    /// succeeds, re-assembling the parts gives the input back and the parts
    /// contain no markers; whenever the input has exactly one well-formed
    /// span, `split` succeeds.
    #[test]
    fn r23_scanner_properties_on_generated_strings() {
        let alphabet = ['a', ' ', '[', ']', 'é'];
        let mut checked = 0;
        for len in 0..=7u32 {
            let total = alphabet.len().pow(len);
            for mut n in 0..total {
                let mut text = String::new();
                for _ in 0..len {
                    text.push(alphabet[n % alphabet.len()]);
                    n /= alphabet.len();
                }
                checked += 1;
                match split(&text) {
                    Ok(m) => {
                        assert_eq!(format!("{}[[{}]]{}", m.pre, m.target, m.post), text);
                        assert!(!m.pre.contains("[[") && !m.pre.contains("]]"), "{text:?}");
                        assert!(!m.post.contains("[[") && !m.post.contains("]]"), "{text:?}");
                        assert!(!m.target.contains("[[") && !m.target.contains("]]"));
                        assert!(!m.target.is_empty());
                        assert!(well_formed(&text), "{text:?} should be rejected");
                    }
                    Err(_) => {
                        assert!(!well_formed(&text), "{text:?} should be accepted");
                    }
                }
            }
        }
        assert!(checked > 90_000);
    }

    /// Independent definition: exactly one way to read the text as
    /// pre + "[[" + target + "]]" + post with the stated constraints, where
    /// the opening is the first `[[` and the closing the first `]]` after it.
    fn well_formed(text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let at = |i: usize, a: char| chars.get(i) == Some(&a) && chars.get(i + 1) == Some(&a);
        let Some(open) = (0..chars.len()).find(|&i| at(i, '[')) else {
            return false;
        };
        let Some(close) = (open + 2..chars.len()).find(|&i| at(i, ']')) else {
            return false;
        };
        let target = &chars[open + 2..close];
        if target.is_empty()
            || target.first() == Some(&']')
            || target.first().is_some_and(|c| c.is_whitespace())
            || target.last().is_some_and(|c| c.is_whitespace())
        {
            return false;
        }
        // No `]]` before the opening, no `[[` inside the target, none after.
        let any = |from: usize, to: usize, c: char| (from..to.saturating_sub(1)).any(|i| at(i, c));
        !(any(0, open + 1, ']')
            || any(open + 2, close, '[')
            || any(close + 2, chars.len(), '[')
            || any(close + 2, chars.len(), ']'))
    }
}
