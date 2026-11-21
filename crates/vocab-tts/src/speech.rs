//! What to say and how: the validated vocabulary of a speak request.
//!
//! Every value that reaches the engine went through here: the text (plain
//! text only, bounded length, no control characters, no markup; R-84-style
//! limits for untrusted input), the voice (one of the two voices the app
//! offers, R-42) and the speed.

use std::fmt;

use sha2::{Digest, Sha256};

/// Longest text accepted, in characters (design §10.2). The app sends a
/// headword or one book sentence; 600 characters is several long sentences.
pub const MAX_CHARS: usize = 600;

/// Text to speak: trimmed, non-empty, at most [`MAX_CHARS`] characters, no
/// control or invisible formatting characters, no markup.
///
/// The text may be a sentence of a book (private, R-24): `Debug` and
/// `Display` print only its length and fingerprint, so it cannot end up in a
/// log line by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct SpeakText(String);

/// Why a text is refused (the message is safe to return to the client: it
/// never quotes the text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error("the text is empty")]
    Empty,
    #[error("the text is longer than {MAX_CHARS} characters")]
    TooLong,
    #[error("the text contains a control or invisible formatting character")]
    ControlCharacter,
    #[error("the text contains markup ('<' or '>'); only plain text is spoken (no SSML)")]
    Markup,
}

impl SpeakText {
    /// Validate `text` (surrounding whitespace is trimmed first).
    pub fn new(text: &str) -> Result<Self, TextError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(TextError::Empty);
        }
        if text.chars().count() > MAX_CHARS {
            return Err(TextError::TooLong);
        }
        if text.chars().any(is_forbidden_char) {
            return Err(TextError::ControlCharacter);
        }
        if text.contains(['<', '>']) {
            return Err(TextError::Markup);
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Length in characters (for logs).
    pub fn char_count(&self) -> usize {
        self.0.chars().count()
    }

    /// The first 12 hex digits of the text's SHA-256: lets a log line be
    /// matched to a request without revealing the text.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        hex::encode(&digest[..6])
    }
}

impl fmt::Debug for SpeakText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpeakText({self})")
    }
}

impl fmt::Display for SpeakText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} chars, sha256 {}…",
            self.char_count(),
            self.fingerprint()
        )
    }
}

/// Control characters (C0, C1, DEL; also tab and line breaks: a sentence is
/// one line), line/paragraph separators and the invisible bidirectional and
/// zero-width formatting characters, which can hide text from a reviewer.
fn is_forbidden_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200B}'..='\u{200F}' // zero-width space/joiners, LRM/RLM
            | '\u{2028}' | '\u{2029}' // line and paragraph separator
            | '\u{202A}'..='\u{202E}' // bidi embeddings and overrides
            | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
            | '\u{2066}'..='\u{2069}' // bidi isolates
            | '\u{FEFF}' // byte order mark / zero-width no-break space
            | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation
        )
}

/// The voices the app offers (design §10.1, decision 7, R-42).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Voice {
    /// American English, female ("af_heart"); the default.
    AfHeart,
    /// British English, female ("bf_emma").
    BfEmma,
}

/// Pronunciation variant of a voice: selects the lexicon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Accent {
    Us,
    Gb,
}

impl Voice {
    pub const ALL: [Voice; 2] = [Voice::AfHeart, Voice::BfEmma];
    pub const DEFAULT: Voice = Voice::AfHeart;

    /// The voice id of the API (Kokoro's voice name).
    pub fn id(self) -> &'static str {
        match self {
            Voice::AfHeart => "af_heart",
            Voice::BfEmma => "bf_emma",
        }
    }

    pub fn parse(id: &str) -> Option<Voice> {
        Voice::ALL.into_iter().find(|v| v.id() == id)
    }

    pub fn accent(self) -> Accent {
        match self {
            Voice::AfHeart => Accent::Us,
            Voice::BfEmma => Accent::Gb,
        }
    }
}

/// Speaking rate, 0.5–1.5 (1.0 = the voice's natural rate). The app plays
/// clips at other speeds client-side (`playbackRate`), so it normally asks
/// for 1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speed(f32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("speed must be a number from {} to {}", Speed::MIN, Speed::MAX)]
pub struct SpeedError;

impl Speed {
    pub const MIN: f32 = 0.5;
    pub const MAX: f32 = 1.5;
    pub const NORMAL: Speed = Speed(1.0);

    pub fn new(value: f64) -> Result<Self, SpeedError> {
        if value.is_finite() && (f64::from(Self::MIN)..=f64::from(Self::MAX)).contains(&value) {
            // In range, so the narrowing conversion cannot overflow.
            Ok(Self(value as f32))
        } else {
            Err(SpeedError)
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r84_plain_sentence_is_accepted_and_trimmed() {
        let text = SpeakText::new("  The cat sat on the mat.  ").expect("test setup");
        assert_eq!(text.as_str(), "The cat sat on the mat.");
        assert_eq!(text.char_count(), 23);
    }

    #[test]
    fn r84_empty_and_blank_text_is_rejected() {
        assert_eq!(SpeakText::new(""), Err(TextError::Empty));
        assert_eq!(SpeakText::new(" \u{a0} "), Err(TextError::Empty));
    }

    #[test]
    fn r84_length_limit_counts_characters_not_bytes() {
        let at_limit = "é".repeat(MAX_CHARS);
        assert!(SpeakText::new(&at_limit).is_ok());
        let over = "a".repeat(MAX_CHARS + 1);
        assert_eq!(SpeakText::new(&over), Err(TextError::TooLong));
    }

    #[test]
    fn r84_control_and_invisible_characters_are_rejected() {
        for bad in [
            "line\nbreak",
            "tab\there",
            "nul\0",
            "esc\u{1b}[31m",
            "c1\u{85}next",
            "bidi\u{202e}txet",
            "zero\u{200b}width",
            "iso\u{2066}late",
            "bom\u{feff}",
        ] {
            assert_eq!(
                SpeakText::new(bad),
                Err(TextError::ControlCharacter),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn r84_markup_is_rejected_no_ssml() {
        assert_eq!(
            SpeakText::new("<speak>hello</speak>"),
            Err(TextError::Markup)
        );
        assert_eq!(SpeakText::new("a > b"), Err(TextError::Markup));
    }

    #[test]
    fn r84_debug_and_display_never_show_the_text() {
        let text = SpeakText::new("A private sentence from a book.").expect("test setup");
        let shown = format!("{text} {text:?}");
        assert!(!shown.contains("private"), "{shown}");
        assert!(shown.contains("31 chars"), "{shown}");
        assert_eq!(text.fingerprint().len(), 12);
    }

    #[test]
    fn r42_us_and_uk_voices() {
        assert_eq!(Voice::parse("af_heart"), Some(Voice::AfHeart));
        assert_eq!(Voice::parse("bf_emma"), Some(Voice::BfEmma));
        assert_eq!(Voice::parse("AF_HEART"), None);
        assert_eq!(Voice::parse("am_adam"), None);
        assert_eq!(Voice::DEFAULT.accent(), Accent::Us);
        assert_eq!(Voice::BfEmma.accent(), Accent::Gb);
    }

    #[test]
    fn r84_speed_range() {
        assert_eq!(Speed::new(1.0), Ok(Speed::NORMAL));
        assert!(Speed::new(0.5).is_ok());
        assert!(Speed::new(1.5).is_ok());
        for bad in [0.49, 1.51, 0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(Speed::new(bad), Err(SpeedError), "{bad}");
        }
    }
}
