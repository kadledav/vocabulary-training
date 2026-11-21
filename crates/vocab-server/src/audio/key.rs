//! Clip identity (design §10.3 step 2): what exactly is spoken, and the
//! content hash that names the clip and its file.

use sha2::{Digest, Sha256};

use crate::settings::Voice;

/// Longest text the TTS service accepts (its API limit).
const MAX_CHARS: usize = 600;

/// The TTS service's front-end and engine versions (`GET /v1/voices`): part
/// of every clip's identity, so a new pronunciation front-end or model makes
/// new clips (and the old ones garbage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versions {
    pub frontend: String,
    pub engine: String,
}

/// The TTS voice of a voice setting (R-42).
pub fn tts_voice(voice: Voice) -> &'static str {
    match voice {
        Voice::Us => "af_heart",
        Voice::Uk => "bf_emma",
    }
}

/// Every TTS voice the app uses (clips of all of them are kept while their
/// text exists, so switching back and forth does not regenerate).
pub const VOICES: [Voice; 2] = [Voice::Us, Voice::Uk];

/// The text to speak for a catalog text: whitespace collapsed, a leading or
/// trailing ellipsis (`…`, `...`) removed (a book excerpt often starts or
/// ends mid-sentence), angle brackets dropped (the service speaks plain text
/// only). `None` if nothing is left or the text is longer than the service
/// accepts (then the browser's speech is the fallback, R-41).
pub fn spoken(raw: &str) -> Option<String> {
    let mut text: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['<', '>'], " ");
    loop {
        let trimmed = text
            .trim()
            .trim_start_matches(['…', '.'])
            .trim_end_matches('…')
            .trim();
        let trimmed = trimmed.strip_suffix("...").unwrap_or(trimmed).trim();
        if trimmed.len() == text.len() {
            break;
        }
        text = trimmed.to_owned();
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = text.chars().count();
    (count > 0 && count <= MAX_CHARS && text.chars().any(char::is_alphanumeric)).then_some(text)
}

/// The clip hash: `sha256("v1|frontend|engine|voice|1.0|text")`, first 32 hex
/// digits. Identical text and voice share one clip across words.
pub fn clip_hash(versions: &Versions, voice: Voice, text: &str) -> String {
    let input = format!(
        "v1|{}|{}|{}|1.0|{text}",
        versions.frontend,
        versions.engine,
        tts_voice(voice)
    );
    hex::encode(&Sha256::digest(input.as_bytes())[..16])
}

/// A clip hash as it appears in URLs and file names: 32 lowercase hex digits.
pub fn is_clip_hash(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions() -> Versions {
        Versions {
            frontend: "g2p-2".into(),
            engine: "kokoro".into(),
        }
    }

    #[test]
    fn r40_spoken_text_is_trimmed_of_ellipses_and_markup() {
        assert_eq!(spoken("  The cat  sat. ").as_deref(), Some("The cat sat."));
        assert_eq!(
            spoken("…and then he left.").as_deref(),
            Some("and then he left.")
        );
        assert_eq!(
            spoken("...and he waited...").as_deref(),
            Some("and he waited")
        );
        assert_eq!(spoken("He paused…").as_deref(), Some("He paused"));
        assert_eq!(spoken("a <b> c").as_deref(), Some("a b c"));
        assert_eq!(spoken("…"), None);
        assert_eq!(spoken("  "), None);
        assert_eq!(spoken(&"word ".repeat(130)), None);
    }

    #[test]
    fn r42_hash_depends_on_voice_versions_and_text() {
        let v = versions();
        let us = clip_hash(&v, Voice::Us, "hello");
        assert!(is_clip_hash(&us));
        assert_eq!(us, clip_hash(&v, Voice::Us, "hello"));
        assert_ne!(us, clip_hash(&v, Voice::Uk, "hello"));
        assert_ne!(us, clip_hash(&v, Voice::Us, "Hello"));
        let newer = Versions {
            frontend: "g2p-3".into(),
            ..v
        };
        assert_ne!(us, clip_hash(&newer, Voice::Us, "hello"));
        assert!(!is_clip_hash("../etc/passwd"));
        assert!(!is_clip_hash(&us.to_uppercase()));
    }
}
