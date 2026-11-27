//! What to say and how: the validated vocabulary of a speak request.
//!
//! Every value that reaches the engine went through here: the text (plain
//! text only, bounded length, no control characters, no markup, bounded
//! words and numbers; R-84-style limits for untrusted input), the voice (one
//! of the two voices the app offers, R-42) and the speed.
//!
//! Normalisation before the checks: invisible formatting characters are
//! removed (the set the pack keys ignore, `vocab_pack::keys::is_invisible`;
//! a soft hyphen joins its word again) and every whitespace character
//! (tab, line breaks) becomes a space.

use std::fmt;

use sha2::{Digest, Sha256};
use vocab_pack::keys::is_invisible;

/// Longest text accepted, in characters (design §10.2). The app sends a
/// headword or one book sentence; 600 characters is several long sentences.
pub const MAX_CHARS: usize = 600;
/// Longest run of non-space characters ("pneumonoultramicroscopicsilico-
/// volcanoconiosis" has 45): anything longer is not a word to speak and
/// would make one oversized piece for the engine.
pub const MAX_TOKEN_CHARS: usize = 64;
/// Longest run of digits (longer ones are read digit by digit anyway, and
/// only an id or a garbled text has more).
pub const MAX_DIGITS: usize = 15;

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
    #[error("the text contains a word longer than {MAX_TOKEN_CHARS} characters")]
    TokenTooLong,
    #[error("the text contains a number longer than {MAX_DIGITS} digits")]
    NumberTooLong,
}

impl TextError {
    /// The API error code: format problems are `invalid_text` (400); a text
    /// that is valid but cannot be spoken sensibly is unprocessable (422).
    pub fn code(self) -> &'static str {
        match self {
            Self::TokenTooLong => "token_too_long",
            Self::NumberTooLong => "number_too_long",
            _ => "invalid_text",
        }
    }

    pub fn unprocessable(self) -> bool {
        matches!(self, Self::TokenTooLong | Self::NumberTooLong)
    }
}

impl SpeakText {
    /// Normalise and validate `text` (see the module documentation).
    pub fn new(text: &str) -> Result<Self, TextError> {
        let normalised: String = text
            .chars()
            .filter(|c| !is_invisible(*c))
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        let text = normalised.trim();
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
        for token in text.split(' ') {
            if token.chars().count() > MAX_TOKEN_CHARS {
                return Err(TextError::TokenTooLong);
            }
            let mut run = 0;
            for c in token.chars() {
                run = if c.is_ascii_digit() { run + 1 } else { 0 };
                if run > MAX_DIGITS {
                    return Err(TextError::NumberTooLong);
                }
            }
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

/// Control characters left after normalisation (C0, C1, DEL; whitespace
/// ones became spaces) and the interlinear annotation characters, which
/// can hide text.
fn is_forbidden_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{FFF9}'..='\u{FFFB}')
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

    /// How much longer a phoneme sounds than at normal speed (at least 1:
    /// faster speech is not counted as shorter). The engine's working
    /// memory and the clip's length grow with the audio of a chunk, so the
    /// limits on symbols are divided by this.
    pub fn stretch(self) -> f32 {
        (1.0 / self.0).max(1.0)
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
        let at_limit = vec!["éé"; MAX_CHARS / 3].join(" ");
        assert_eq!(at_limit.chars().count(), MAX_CHARS - 1);
        assert!(SpeakText::new(&at_limit).is_ok());
        let over = "a".repeat(MAX_CHARS + 1);
        assert_eq!(SpeakText::new(&over), Err(TextError::TooLong));
    }

    #[test]
    fn r84_control_characters_are_rejected() {
        for bad in [
            "nul\0here",
            "esc\u{1b}[31m",
            "c1\u{9b}x",
            "del\u{7f}x",
            "ann\u{FFF9}x",
        ] {
            assert_eq!(
                SpeakText::new(bad),
                Err(TextError::ControlCharacter),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn r84_whitespace_is_normalised_and_invisible_characters_removed() {
        for (input, spoken) in [
            ("line\nbreak", "line break"),
            ("tab\there", "tab here"),
            ("cr\r\nlf", "cr  lf"),
            ("nel\u{85}x", "nel x"),
            ("ls\u{2028}x", "ls x"),
            ("loi\u{00AD}ter", "loiter"),
            ("zero\u{200b}width", "zerowidth"),
            ("bidi\u{202e}x\u{2069}", "bidix"),
            ("\u{feff}bom", "bom"),
            ("mark\u{200e}", "mark"),
        ] {
            assert_eq!(
                SpeakText::new(input).map(|t| t.as_str().to_owned()),
                Ok(spoken.to_owned()),
                "{input:?}"
            );
        }
        assert_eq!(SpeakText::new("\u{200b}\u{feff}"), Err(TextError::Empty));
    }

    #[test]
    fn r84_overlong_words_and_numbers_are_unprocessable() {
        let compound = vec!["internationalization"; 20].join("-");
        assert_eq!(SpeakText::new(&compound), Err(TextError::TokenTooLong));
        assert_eq!(
            SpeakText::new(&"9".repeat(16)),
            Err(TextError::NumberTooLong)
        );
        assert_eq!(
            SpeakText::new(&format!("call {}", "1".repeat(16))),
            Err(TextError::NumberTooLong)
        );
        assert!(SpeakText::new(&"9".repeat(15)).is_ok());
        assert!(SpeakText::new("pneumonoultramicroscopicsilicovolcanoconiosis").is_ok());
        assert!(SpeakText::new("1,000,000,000,000,000").is_ok());
        assert!(TextError::TokenTooLong.unprocessable());
        assert_eq!(TextError::NumberTooLong.code(), "number_too_long");
        assert_eq!(TextError::Markup.code(), "invalid_text");
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

    #[test]
    fn r84_slower_speech_stretches_every_phoneme() {
        let stretch = |v: f64| Speed::new(v).expect("test setup").stretch();
        assert_eq!(stretch(1.0), 1.0);
        assert_eq!(stretch(1.5), 1.0);
        assert_eq!(stretch(0.5), 2.0);
        assert!((stretch(0.8) - 1.25).abs() < 1e-6);
    }
}
