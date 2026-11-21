//! Mechanical identity keys (design §5.3). No language knowledge: only
//! Unicode normalisation, whitespace, case and a few typographic variants.
//!
//! * [`headword_key`]: NFC → trim → collapse inner whitespace → lowercase →
//!   typographic apostrophes to `'`, hyphen variants U+2010/U+2011 to `-`.
//!   `Pore over` = `pore  over`, but `carpet-bomb` ≠ `carpet bomb`.
//! * [`sentence_key`]: markers removed, then the headword steps, plus the
//!   typographic variants a model changes freely between two exports of the
//!   same sentence: typographic quotes become straight ones, `...` and `…`
//!   are one ellipsis, en and em dashes are a hyphen, the spaces around a
//!   dash or an ellipsis do not matter, and quotes, ellipses, whitespace and
//!   closing sentence punctuation (`.`, `!`, `?`) at the ends are ignored
//!   (Claude may cut or punctuate a quote differently between exports).
//! * [`item_key`]: NFC → trim → collapse whitespace → lowercase (Czech
//!   equivalents, accepted answers, confusables, forms).
//!
//! All three first remove invisible characters that copy-paste or a model
//! can slip into a word without changing how it looks: zero-width space,
//! non-joiner and joiner (U+200B–U+200D), word joiner (U+2060), byte order
//! mark (U+FEFF) and soft hyphen (U+00AD).

use unicode_normalization::UnicodeNormalization;

use crate::marked::{split, strip_markers};

/// NFC normalisation; all stored pack text goes through this.
pub fn nfc(text: &str) -> String {
    text.nfc().collect()
}

/// Merge identity of a word.
pub fn headword_key(headword: &str) -> String {
    item_key(headword)
        .chars()
        .map(|c| match c {
            '\u{2019}' | '\u{2018}' | '\u{02BC}' => '\'',
            '\u{2010}' | '\u{2011}' => '-',
            other => other,
        })
        .collect()
}

/// Dedupe key of a sentence within one word (R-22).
pub fn sentence_key(marked_text: &str) -> String {
    // One ellipsis character for both spellings, one hyphen for every dash.
    let folded: String = headword_key(&strip_markers(marked_text))
        .replace("...", "\u{2026}")
        .chars()
        .map(|c| match c {
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{00AB}' | '\u{00BB}' => '"',
            '\u{201A}' | '\u{201B}' => '\'',
            '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' => '-',
            other => other,
        })
        .collect();
    // `a — b`, `a—b`, `a - b` and `a-b` are the same; so are `a… b` and `a …b`.
    let joins = |c: char| c == '-' || c == '\u{2026}';
    let chars: Vec<char> = folded.chars().collect();
    let key: String = chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| {
            let beside_join =
                (i > 0 && joins(chars[i - 1])) || chars.get(i + 1).is_some_and(|&next| joins(next));
            !(c == ' ' && beside_join)
        })
        .map(|(_, &c)| c)
        .collect();
    let edge = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\u{2026}');
    key.trim_start_matches(edge)
        .trim_end_matches(|c: char| edge(c) || matches!(c, '.' | '!' | '?'))
        .to_owned()
}

/// Where the marked span of a marked sentence is, in key terms: the
/// [`sentence_key`] of the text before it and of the span (`None` when the
/// text has no valid span). Two versions of a sentence with the same key
/// but another span identity mark another part of it (R-22).
pub fn span_key(marked_text: &str) -> Option<(String, String)> {
    let parts = split(marked_text).ok()?;
    Some((sentence_key(&parts.pre), sentence_key(&parts.target)))
}

/// Union key of a list item.
pub fn item_key(item: &str) -> String {
    collapse_whitespace(&nfc(&strip_invisible(item))).to_lowercase()
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}'
    )
}

fn strip_invisible(text: &str) -> String {
    text.chars().filter(|&c| !is_invisible(c)).collect()
}

/// Trim, then replace every run of whitespace with one space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r03_headword_key_is_mechanical() {
        assert_eq!(headword_key("Pore over"), headword_key("pore  over"));
        assert_eq!(headword_key("  Pore\tOVER \n"), "pore over");
        assert_ne!(headword_key("carpet-bomb"), headword_key("carpet bomb"));
        assert_eq!(headword_key("carpet\u{2010}bomb"), "carpet-bomb");
        assert_eq!(headword_key("carpet\u{2011}bomb"), "carpet-bomb");
        assert_eq!(headword_key("o\u{2019}clock"), "o'clock");
        assert_eq!(headword_key("o\u{2018}clock"), "o'clock");
        assert_eq!(headword_key("o\u{02BC}clock"), "o'clock");
        // NFC: decomposed é equals precomposed é.
        assert_eq!(headword_key("cafe\u{301}"), headword_key("caf\u{e9}"));
    }

    #[test]
    fn r22_sentence_key_ignores_markers_case_quotes_and_cut_ellipses() {
        let a = sentence_key("The cat [[purred]] softly.");
        assert_eq!(a, sentence_key("the cat purred  softly."));
        assert_eq!(a, sentence_key("…The cat [[purred]] softly."));
        assert_eq!(a, sentence_key("... The cat purred softly. ..."));
        assert_eq!(
            sentence_key("\u{201C}Stop,\u{201D} she [[hissed]]."),
            sentence_key("\"Stop,\" she hissed.")
        );
        assert_eq!(
            sentence_key("It\u{2019}s [[fine]]."),
            sentence_key("It's fine.")
        );
        // A different span is the same sentence (the owner may have edited it).
        assert_eq!(
            sentence_key("[[The cat]] purred softly."),
            sentence_key("The cat [[purred]] softly.")
        );
        assert_ne!(a, sentence_key("The dog [[purred]] softly."));
    }

    #[test]
    fn r03_invisible_characters_do_not_split_identities() {
        assert_eq!(headword_key("loi\u{200B}ter"), headword_key("loiter"));
        assert_eq!(headword_key("well\u{AD}being"), headword_key("wellbeing"));
        assert_eq!(headword_key("\u{FEFF}hum\u{2060}"), "hum");
        assert_eq!(headword_key("a\u{200C}b\u{200D}c"), "abc");
        assert_eq!(item_key("bzu\u{AD}kot"), "bzukot");
        assert_eq!(
            sentence_key("The [[loi\u{200B}ter]]ing cat."),
            sentence_key("The loitering cat.")
        );
        // A real hyphen still matters.
        assert_ne!(headword_key("well-being"), headword_key("wellbeing"));
    }

    #[test]
    fn r03_item_key_folds_case_and_whitespace_only() {
        assert_eq!(item_key("  Tupá   Rána "), "tupá rána");
        assert_ne!(item_key("o\u{2019}clock"), item_key("o'clock"));
    }

    /// R-22/R-53 (review finding): the same sentence must not be stored
    /// twice because an export punctuates it differently.
    #[test]
    fn r22_sentence_key_folds_dashes_ellipses_quotes_and_closing_punctuation() {
        let a = sentence_key("He paused\u{2014}then [[thudded]] down\u{2026} and left.");
        for same in [
            "He paused - then [[thudded]] down\u{2026} and left.",
            "He paused\u{2013}then [[thudded]] down\u{2026} and left.",
            "He paused \u{2014} then [[thudded]] down\u{2026} and left.",
            "He paused\u{2014}then [[thudded]] down... and left.",
            "He paused\u{2014}then [[thudded]] down\u{2026} and left",
            "He paused\u{2014}then [[thudded]] down\u{2026} and left!",
            "He paused\u{2014}then [[thudded]] down\u{2026} and left.\u{201d}",
            "\u{201c}He paused\u{2014}then [[thudded]] down\u{2026} and left.",
            "\"He paused\u{2014}then [[thudded]] down\u{2026} and left.\"",
            "He paused\u{2014}then [[thudded]] down\u{2026} and left. \u{2026}",
            "He paused\u{2014}then [[thudded]] down\u{a0}\u{2026} and left.",
            "He  paused\u{2014}then\t[[thudded]] down \u{2026}and left.",
        ] {
            assert_eq!(a, sentence_key(same), "{same:?}");
        }
        assert_eq!(a, "he paused-then thudded down\u{2026}and left");
        // Words still matter, and so does punctuation inside the sentence.
        assert_ne!(
            a,
            sentence_key("He paused\u{2014}then [[thudded]] down and left.")
        );
        assert_ne!(
            a,
            sentence_key("He paused, then [[thudded]] down\u{2026} and left.")
        );
        assert_ne!(
            sentence_key("Is it [[fine]]? Yes."),
            sentence_key("Is it [[fine]]. Yes.")
        );
        assert_eq!(
            sentence_key("\u{201a}Stop,\u{2018} she [[hissed]]."),
            sentence_key("'Stop,' she hissed")
        );
        // The key never changes when it is computed again from itself.
        assert_eq!(sentence_key(&a), a);
    }
}
