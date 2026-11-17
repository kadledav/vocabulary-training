//! Mechanical identity keys (design §5.3). No language knowledge: only
//! Unicode normalisation, whitespace, case and a few typographic variants.
//!
//! * [`headword_key`]: NFC → trim → collapse inner whitespace → lowercase →
//!   typographic apostrophes to `'`, hyphen variants U+2010/U+2011 to `-`.
//!   `Pore over` = `pore  over`, but `carpet-bomb` ≠ `carpet bomb`.
//! * [`sentence_key`]: markers removed, then the headword steps, plus
//!   typographic double quotes to `"`, and leading/trailing `…`, `...` and
//!   whitespace trimmed (Claude may cut a quote differently between exports).
//! * [`item_key`]: NFC → trim → collapse whitespace → lowercase (Czech
//!   equivalents, accepted answers, confusables, forms).
//!
//! All three first remove invisible characters that copy-paste or a model
//! can slip into a word without changing how it looks: zero-width space,
//! non-joiner and joiner (U+200B–U+200D), word joiner (U+2060), byte order
//! mark (U+FEFF) and soft hyphen (U+00AD).

use unicode_normalization::UnicodeNormalization;

use crate::marked::strip_markers;

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
    let key: String = headword_key(&strip_markers(marked_text))
        .chars()
        .map(|c| match c {
            '\u{201C}' | '\u{201D}' | '\u{201E}' => '"',
            other => other,
        })
        .collect();
    trim_ellipses(&key).to_owned()
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

fn trim_ellipses(mut text: &str) -> &str {
    loop {
        let before = text.len();
        text = text
            .trim()
            .trim_start_matches('…')
            .trim_end_matches('…')
            .trim_start_matches("...")
            .trim_end_matches("...");
        if text.len() == before {
            return text;
        }
    }
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
}
