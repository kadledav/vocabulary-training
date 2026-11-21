//! Pronunciation coverage with the real dictionaries (R-40): how many words
//! of a list the Kokoro lexicons know, how many the fallback dictionary
//! adds, and which would be spelled letter by letter.
//!
//! Needs the model files, so it is ignored by default:
//!   VOCAB_TTS_MODEL_DIR=<dir with the lexicons, tokens.txt, cmudict.dict> \
//!     cargo test -p vocab-tts --test coverage -- --ignored --nocapture
//! `VOCAB_TTS_COVERAGE_LIST=<file>` measures another list (one text per
//! line); it prints counts and the spelled words only.

use std::path::{Path, PathBuf};

use vocab_tts::phonemize::{Lexicon, Phonemizer, parse_symbols};
use vocab_tts::speech::{Accent, SpeakText};

const SYNTHETIC: &str = include_str!("data/uncommon-words.txt");

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("model file")
}

fn phonemizers(dir: &Path) -> (Phonemizer, Phonemizer) {
    let make = || {
        Phonemizer::new(
            Lexicon::parse(&read(dir, "lexicon-us-en.txt")).expect("lexicon"),
            Lexicon::parse(&read(dir, "lexicon-gb-en.txt")).expect("lexicon"),
            parse_symbols(&read(dir, "tokens.txt")).expect("tokens"),
        )
    };
    (make(), make().with_fallback(&read(dir, "cmudict.dict")))
}

/// (texts, texts with a spelled word, spelled texts) per accent.
fn measure(p: &Phonemizer, texts: &[String], accent: Accent) -> (usize, Vec<String>, usize) {
    let mut spelled = Vec::new();
    let mut approximate = 0;
    for text in texts {
        let Ok(t) = SpeakText::new(text) else {
            continue;
        };
        let tally = p.phonemize(&t, accent).tally();
        if tally.spelled > 0 {
            spelled.push(text.clone());
        }
        if tally.approximate() > 0 {
            approximate += 1;
        }
    }
    (texts.len(), spelled, approximate)
}

#[test]
#[ignore = "needs the model files (VOCAB_TTS_MODEL_DIR)"]
fn r40_pronunciation_coverage_of_uncommon_words() {
    let dir = PathBuf::from(std::env::var("VOCAB_TTS_MODEL_DIR").expect("VOCAB_TTS_MODEL_DIR"));
    let list = match std::env::var("VOCAB_TTS_COVERAGE_LIST") {
        Ok(path) => std::fs::read_to_string(path).expect("list"),
        Err(_) => SYNTHETIC.to_owned(),
    };
    let texts: Vec<String> = list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let (lexicon_only, with_fallback) = phonemizers(&dir);
    for (name, p) in [
        ("lexicon only", &lexicon_only),
        ("with fallback", &with_fallback),
    ] {
        for accent in [Accent::Us, Accent::Gb] {
            let (n, spelled, approximate) = measure(p, &texts, accent);
            println!(
                "{name:14} {accent:?}: {n} texts, {} with a spelled word, {approximate} approximate",
                spelled.len()
            );
            if std::env::var("VOCAB_TTS_COVERAGE_SHOW").is_ok() {
                println!("  spelled: {spelled:?}");
            }
        }
    }
    let (_, spelled, _) = measure(&with_fallback, &texts, Accent::Us);
    if std::env::var("VOCAB_TTS_COVERAGE_LIST").is_err() {
        // The synthetic list: at most 2 % may be spelled out.
        assert!(spelled.len() * 50 <= texts.len(), "spelled: {spelled:?}");
    }
}
