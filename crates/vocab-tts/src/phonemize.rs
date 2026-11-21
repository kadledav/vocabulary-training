//! Text to phonemes for Kokoro (grapheme-to-phoneme), done here rather than
//! in sherpa-onnx (docs/design/tts-spike.md):
//!
//! * sherpa-onnx 2.x has no espeak-ng any more; its own lexicon front-end
//!   silently drops every word that is not in the lexicon ("dogs", "1990")
//!   and prints the dropped word to stderr, which would put words of book
//!   sentences into the container log (R-24);
//! * one engine serves both voices, but the US and UK lexicons differ:
//!   the accent must follow the voice (R-42).
//!
//! So the text is split into words and pauses here, every word is looked up
//! in the lexicon of the voice's accent (Kokoro v1.0's `lexicon-us-en.txt` /
//! `lexicon-gb-en.txt`, misaki phonemes), and words that are not in it get,
//! in this order: numbers read as words, accents folded, the fallback
//! dictionary (CMUdict, converted to misaki notation per accent, see
//! `arpabet`), the possessive and the regular suffixes (-s, -ed, -ing, -ly)
//! derived from a known stem, hyphenated parts looked up one by one, and
//! finally letter-by-letter spelling. Every word is counted by where its
//! pronunciation came from ([`Tally`]): the API tells the app whether a clip
//! is approximate. The engine receives only phoneme symbols of its own
//! symbol table.

mod arpabet;
mod numbers;

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::speech::{Accent, SpeakText};

/// File names in the model directory.
const US_LEXICON: &str = "lexicon-us-en.txt";
const GB_LEXICON: &str = "lexicon-gb-en.txt";
const SYMBOLS: &str = "tokens.txt";
/// The fallback dictionary (CMUdict, ARPAbet).
const FALLBACK: &str = "cmudict.dict";
/// Version of this front-end: part of the app's clip identity, so a change
/// in how text becomes phonemes regenerates the clips. Bump it with every
/// change of the rules or the dictionaries.
pub const FRONTEND_VERSION: &str = "g2p-3";
/// Phoneme symbols per chunk the engine synthesises in one pass (Kokoro's
/// context is 510). The engine's memory grows with the chunk length and
/// with every new chunk shape (measured in the image: idle 576 MiB, after
/// one chunk of 150 symbols 753 MiB, of 300 symbols 998 MiB, after two
/// different 300-symbol chunks 1353 MiB — over the 1280 MB limit), so
/// chunks stay short; a sentence of up to ~25 words is one chunk.
const MAX_CHUNK_SYMBOLS: usize = 150;
/// Where a chunk that must be cut ends best: at its last pause.
const CUT_MARKS: [char; 5] = [',', ';', ':', '—', '…'];
/// Stress and length marks: not sounds (skipped when looking at the last
/// sound of a stem).
const MARKS: [char; 3] = ['ˈ', 'ˌ', 'ː'];

/// A pronunciation lexicon: lowercase word -> phoneme symbols.
///
/// Compact, because it is most of the service's own memory: all words and
/// phonemes are in one string, with an index sorted by word into it
/// (12 bytes per entry instead of two heap strings and a hash slot). Both
/// Kokoro lexicons and the fallback dictionaries take about 20 MiB of RSS
/// in the image, hash maps took about 80 MiB. Built without a heap
/// allocation per entry, so loading leaves no fragmented heap behind.
pub struct Lexicon {
    text: String,
    /// One entry per word, sorted by word.
    index: Vec<Entry>,
}

/// Where one entry's word and its phonemes (right after the word) are in
/// the lexicon's text.
#[derive(Debug, Clone, Copy)]
struct Entry {
    start: u32,
    word_len: u32,
    phonemes_len: u32,
}

impl Entry {
    fn word(self, text: &str) -> &str {
        let start = self.start as usize;
        &text[start..start + self.word_len as usize]
    }

    fn phonemes(self, text: &str) -> &str {
        let start = self.start as usize + self.word_len as usize;
        &text[start..start + self.phonemes_len as usize]
    }
}

impl fmt::Debug for Lexicon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lexicon({} entries)", self.len())
    }
}

/// Collects entries in order; [`LexiconBuilder::finish`] keeps the last
/// entry of a word, as a map would.
#[derive(Debug, Default)]
struct LexiconBuilder {
    text: String,
    index: Vec<Entry>,
}

/// Why an entry was not added.
enum Skipped {
    NoPhonemes,
    TooLarge,
}

impl LexiconBuilder {
    /// Add `word` (lowercased) with the concatenation of `phonemes`.
    fn insert<'a>(
        &mut self,
        word: &str,
        phonemes: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), Skipped> {
        let start = self.text.len();
        self.text.push_str(&word.to_lowercase());
        let word_end = self.text.len();
        for symbols in phonemes {
            self.text.push_str(symbols);
        }
        let end = self.text.len();
        let result = if end == word_end {
            Err(Skipped::NoPhonemes)
        } else if u32::try_from(end).is_err() {
            Err(Skipped::TooLarge)
        } else {
            // Every offset is at most `end`, which fits (checked above).
            let offset = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
            self.index.push(Entry {
                start: offset(start),
                word_len: offset(word_end - start),
                phonemes_len: offset(end - word_end),
            });
            Ok(())
        };
        if result.is_err() {
            self.text.truncate(start);
        }
        result
    }

    fn finish(self) -> Lexicon {
        let Self {
            mut text,
            mut index,
        } = self;
        // Stable: the entries of one word stay in insertion order.
        index.sort_by(|a, b| a.word(&text).cmp(b.word(&text)));
        let mut unique: Vec<Entry> = Vec::with_capacity(index.len());
        for entry in index {
            match unique.last_mut() {
                Some(last) if last.word(&text) == entry.word(&text) => *last = entry,
                _ => unique.push(entry),
            }
        }
        text.shrink_to_fit();
        Lexicon {
            text,
            index: unique,
        }
    }
}

/// A model file that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}:{line}: {message}", path.display())]
    Format {
        path: PathBuf,
        line: usize,
        message: &'static str,
    },
}

/// A malformed line of a lexicon or symbol table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatError {
    pub line: usize,
    pub message: &'static str,
}

impl Lexicon {
    /// Parse lines `word sym sym …` (one symbol per field; a later entry
    /// for the same word replaces an earlier one, as in sherpa-onnx).
    pub fn parse(text: &str) -> Result<Self, FormatError> {
        let mut builder = LexiconBuilder::default();
        builder.text.reserve(text.len());
        for (index, line) in text.lines().enumerate() {
            let mut fields = line.split_whitespace();
            let Some(word) = fields.next() else {
                continue; // blank line
            };
            builder
                .insert(word, fields)
                .map_err(|skipped| FormatError {
                    line: index + 1,
                    message: match skipped {
                        Skipped::NoPhonemes => "a word without phonemes",
                        Skipped::TooLarge => "the lexicon is larger than 4 GiB",
                    },
                })?;
        }
        if builder.index.is_empty() {
            return Err(FormatError {
                line: 0,
                message: "the lexicon is empty",
            });
        }
        Ok(builder.finish())
    }

    fn empty() -> Self {
        LexiconBuilder::default().finish()
    }

    fn len(&self) -> usize {
        self.index.len()
    }

    fn get(&self, word: &str) -> Option<&str> {
        self.index
            .binary_search_by(|entry| entry.word(&self.text).cmp(word))
            .ok()
            .map(|found| self.index[found].phonemes(&self.text))
    }
}

/// The phoneme symbols of a symbol table (`tokens.txt`: `symbol id` per
/// line; the symbol is one character and may be a space).
pub fn parse_symbols(text: &str) -> Result<HashSet<char>, FormatError> {
    let mut symbols = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = FormatError {
            line: index + 1,
            message: "expected `<symbol> <id>`",
        };
        let (symbol, id) = line.rsplit_once(' ').ok_or(bad)?;
        let mut chars = symbol.chars();
        match (chars.next(), chars.next(), id.parse::<u32>()) {
            (Some(c), None, Ok(_)) => {
                symbols.insert(c);
            }
            _ => return Err(bad),
        }
    }
    Ok(symbols)
}

/// The dictionaries of one accent: the Kokoro lexicon and, for words it
/// lacks, the fallback dictionary (only entries the lexicon does not have).
#[derive(Debug)]
struct Dictionaries {
    lexicon: Lexicon,
    fallback: Lexicon,
}

/// Text to phonemes, for both accents.
pub struct Phonemizer {
    us: Dictionaries,
    gb: Dictionaries,
    symbols: HashSet<char>,
}

impl fmt::Debug for Phonemizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Phonemizer")
            .field("us", &self.us)
            .field("gb", &self.gb)
            .field("symbols", &self.symbols.len())
            .finish()
    }
}

/// Where the pronunciations of a text's words came from (word counts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// From the voice's own lexicon (also numbers read as words).
    pub lexicon: usize,
    /// A regular inflection (-s, -ed, -ing, -ly, possessive) of a known stem.
    pub derived: usize,
    /// From the fallback dictionary (CMUdict, mapped to the accent).
    pub fallback: usize,
    /// Not found anywhere: spoken letter by letter.
    pub spelled: usize,
    /// Nothing speakable came out of it (another script, a symbol): it is
    /// not spoken at all.
    pub dropped: usize,
}

impl Tally {
    /// Words whose pronunciation is a guess rather than a dictionary entry
    /// for this accent: fallback-dictionary words and spelled words.
    pub fn approximate(&self) -> usize {
        self.fallback + self.spelled + self.dropped
    }
}

/// The phonemes of one text: chunks the engine speaks one after another.
///
/// Phonemes reveal the text, so `Debug` shows only sizes.
#[derive(Clone, PartialEq, Eq)]
pub struct Phonemes {
    chunks: Vec<String>,
    tally: Tally,
}

impl fmt::Debug for Phonemes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Phonemes({} chunks, {} symbols, {} spelled out)",
            self.chunks.len(),
            self.symbol_count(),
            self.tally.spelled
        )
    }
}

impl Phonemes {
    pub fn chunks(&self) -> &[String] {
        &self.chunks
    }

    /// Words that were not in any dictionary and are spoken letter by letter.
    pub fn spelled_out(&self) -> usize {
        self.tally.spelled
    }

    /// Where the words' pronunciations came from.
    pub fn tally(&self) -> Tally {
        self.tally
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn symbol_count(&self) -> usize {
        self.chunks.iter().map(|c| c.chars().count()).sum()
    }
}

/// Part of a text: a word to look up, or a pause (punctuation the model
/// knows: `, . ! ? ; : — …`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Word(String),
    Pause(char),
    /// A symbol that is not read (counted as approximate).
    Dropped,
}

impl Phonemizer {
    /// The Kokoro lexicons of both accents, no fallback dictionary.
    pub fn new(us: Lexicon, gb: Lexicon, symbols: HashSet<char>) -> Self {
        Self {
            us: Dictionaries {
                lexicon: us,
                fallback: Lexicon::empty(),
            },
            gb: Dictionaries {
                lexicon: gb,
                fallback: Lexicon::empty(),
            },
            symbols,
        }
    }

    /// Add the fallback dictionary (CMUdict text): for each accent the
    /// entries its lexicon lacks, converted to misaki notation.
    pub fn with_fallback(mut self, cmudict: &str) -> Self {
        for (dictionaries, accent) in [(&mut self.us, Accent::Us), (&mut self.gb, Accent::Gb)] {
            let mut fallback = LexiconBuilder::default();
            for line in cmudict.lines() {
                if let Some((word, phonemes)) = arpabet::entry(line, accent)
                    && dictionaries.lexicon.get(&word).is_none()
                {
                    // A converted entry always has phonemes (one or more
                    // per ARPAbet phone); the dictionary is far below 4 GiB.
                    let _ = fallback.insert(&word, [phonemes.as_str()]);
                }
            }
            dictionaries.fallback = fallback.finish();
        }
        self
    }

    /// Load both lexicons and the symbol table from the model directory.
    pub fn load(model_dir: &Path) -> Result<Self, LoadError> {
        let read = |name: &str| {
            let path = model_dir.join(name);
            std::fs::read_to_string(&path)
                .map_err(|source| LoadError::Read {
                    path: path.clone(),
                    source,
                })
                .map(|text| (path, text))
        };
        let format = |path: PathBuf| {
            move |err: FormatError| LoadError::Format {
                path,
                line: err.line,
                message: err.message,
            }
        };
        let (path, text) = read(US_LEXICON)?;
        let us = Lexicon::parse(&text).map_err(format(path))?;
        let (path, text) = read(GB_LEXICON)?;
        let gb = Lexicon::parse(&text).map_err(format(path))?;
        let (path, text) = read(SYMBOLS)?;
        let symbols = parse_symbols(&text).map_err(format(path))?;
        let (_, cmudict) = read(FALLBACK)?;
        Ok(Self::new(us, gb, symbols).with_fallback(&cmudict))
    }

    /// Entries of the fallback dictionaries (US, UK), for the start log.
    pub fn fallback_sizes(&self) -> (usize, usize) {
        (self.us.fallback.len(), self.gb.fallback.len())
    }

    /// The phonemes of `text` in the given accent.
    pub fn phonemize(&self, text: &SpeakText, accent: Accent) -> Phonemes {
        let dictionaries = match accent {
            Accent::Us => &self.us,
            Accent::Gb => &self.gb,
        };
        let mut tally = Tally::default();
        let mut chunks = Chunks::default();
        for piece in pieces(text.as_str()) {
            match piece {
                Piece::Word(word) => {
                    let phonemes =
                        self.known(&word_phonemes(dictionaries, accent, &word, &mut tally));
                    if phonemes.trim().is_empty() {
                        tally.dropped += 1;
                        continue;
                    }
                    // A number or a compound is several words: each is a
                    // unit of its own, so even a long one is chunked.
                    for part in phonemes.split(' ').filter(|p| !p.is_empty()) {
                        chunks.word(part);
                    }
                }
                Piece::Pause(mark) => {
                    if self.symbols.contains(&mark) {
                        chunks.pause(mark);
                    }
                }
                Piece::Dropped => tally.dropped += 1,
            }
        }
        Phonemes {
            chunks: chunks.finish(),
            tally,
        }
    }

    /// `phonemes` without symbols the model does not know.
    fn known(&self, phonemes: &str) -> String {
        phonemes
            .chars()
            .filter(|c| self.symbols.contains(c))
            .collect()
    }
}

/// Builds chunks of at most [`MAX_CHUNK_SYMBOLS`] symbols: breaks after a
/// sentence end, before a word that would not fit (at the chunk's last
/// pause if there is one, so the cut falls where the text breathes), and
/// inside a single word that alone is longer than a chunk (no chunk ever
/// exceeds the limit, whatever the input).
#[derive(Debug, Default)]
struct Chunks {
    done: Vec<String>,
    current: String,
    len: usize,
}

impl Chunks {
    fn close(&mut self) {
        if self.len > 0 {
            self.done.push(std::mem::take(&mut self.current));
            self.len = 0;
        }
    }

    /// Make room for `needed` more symbols (and a space): end the chunk at
    /// its last pause when the rest then fits, else end it here.
    fn make_room(&mut self, needed: usize) {
        if self.len == 0 || self.len + 1 + needed <= MAX_CHUNK_SYMBOLS {
            return;
        }
        if let Some(cut) = self.current.rfind(CUT_MARKS) {
            let end = cut + self.current[cut..].chars().next().map_or(1, char::len_utf8);
            let tail = self.current[end..].trim_start().to_owned();
            let tail_len = tail.chars().count();
            if tail_len > 0 && tail_len + 1 + needed <= MAX_CHUNK_SYMBOLS {
                self.current.truncate(end);
                self.done.push(std::mem::replace(&mut self.current, tail));
                self.len = tail_len;
                return;
            }
        }
        self.close();
    }

    fn word(&mut self, phonemes: &str) {
        let symbols: Vec<char> = phonemes.chars().collect();
        for piece in symbols.chunks(MAX_CHUNK_SYMBOLS) {
            self.make_room(piece.len());
            if self.len > 0 {
                self.current.push(' ');
                self.len += 1;
            }
            self.current.extend(piece);
            self.len += piece.len();
        }
    }

    fn pause(&mut self, mark: char) {
        if self.len == 0 {
            return; // nothing to pause after
        }
        if self.len + 1 > MAX_CHUNK_SYMBOLS {
            self.close();
            return;
        }
        self.current.push(mark);
        self.len += 1;
        if matches!(mark, '.' | '!' | '?') {
            self.close();
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.close();
        self.done
    }
}

/// Characters that separate words without being read and without counting
/// as dropped (quotes and typographic marks).
fn silent(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '“'
                | '”'
                | '„'
                | '‟'
                | '«'
                | '»'
                | '‹'
                | '›'
                | '‚'
                | '`'
                | '´'
                | '*'
                | '_'
                | '/'
                | '\\'
                | '•'
                | '·'
                | '|'
                | '~'
                | '^'
        )
}

/// Currency signs read after their amount ("$5" → "five dollars").
fn currency(c: char) -> Option<(&'static str, &'static str)> {
    match c {
        '$' => Some(("dollar", "dollars")),
        '€' => Some(("euro", "euros")),
        '£' => Some(("pound", "pounds")),
        _ => None,
    }
}

/// Split a text into words and pauses (Unicode NFC first). Apostrophes,
/// hyphens and digit separators (`1,000`, `3.5`) stay inside words; quotes,
/// brackets and other symbols separate words (brackets as a short pause).
fn pieces(text: &str) -> Vec<Piece> {
    let chars: Vec<char> = text.nfc().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<Piece>| {
        if !word.is_empty() {
            out.push(Piece::Word(std::mem::take(word)));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let between_digits =
            i > 0 && chars[i - 1].is_ascii_digit() && next.is_some_and(|n| n.is_ascii_digit());
        match c {
            '\'' | '’' | '‘' | 'ʼ' => word.push('\''),
            '.' if next == Some('.') && chars.get(i + 2) == Some(&'.') => {
                flush(&mut word, &mut out);
                out.push(Piece::Pause('…'));
                i += 3;
                continue;
            }
            '.' | ',' if between_digits && !word.is_empty() => word.push(c),
            '.' | ',' | '!' | '?' | ';' | ':' | '…' => {
                flush(&mut word, &mut out);
                out.push(Piece::Pause(c));
            }
            '—' | '–' | '―' => {
                flush(&mut word, &mut out);
                out.push(Piece::Pause('—'));
            }
            '-' if next == Some('-') => {
                flush(&mut word, &mut out);
                out.push(Piece::Pause('—'));
                i += 2;
                continue;
            }
            '(' | ')' | '[' | ']' | '{' | '}' => {
                flush(&mut word, &mut out);
                out.push(Piece::Pause(','));
            }
            '&' | '%' | '+' | '@' | '°' | '=' => {
                flush(&mut word, &mut out);
                let said = match c {
                    '&' => "and",
                    '%' => "percent",
                    '+' => "plus",
                    '°' => "degrees",
                    '=' => "equals",
                    _ => "at",
                };
                out.push(Piece::Word(said.to_owned()));
            }
            _ if currency(c).is_some() && next.is_some_and(|n| n.is_ascii_digit()) => {
                // "$5" / "$1,000.50": the amount, then the currency.
                flush(&mut word, &mut out);
                let mut j = i + 1;
                let mut amount = String::new();
                while let Some(&d) = chars.get(j) {
                    let inside = matches!(d, '.' | ',')
                        && chars.get(j + 1).is_some_and(char::is_ascii_digit);
                    if !(d.is_ascii_digit() || inside) {
                        break;
                    }
                    amount.push(d);
                    j += 1;
                }
                if let Some((one, many)) = currency(c) {
                    let name = if amount == "1" { one } else { many };
                    out.push(Piece::Word(amount));
                    out.push(Piece::Word(name.to_owned()));
                }
                i = j;
                continue;
            }
            _ if c.is_alphanumeric() || c == '-' || is_combining_mark(c) => word.push(c),
            _ if silent(c) => flush(&mut word, &mut out),
            _ => {
                flush(&mut word, &mut out);
                out.push(Piece::Dropped);
            }
        }
        i += 1;
    }
    flush(&mut word, &mut out);
    out
}

/// How one word's pronunciation was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Lexicon,
    Derived,
    Fallback,
}

/// The phonemes of one word (several words, separated by spaces, for a
/// number or a hyphenated compound), counted in `tally`; empty if nothing
/// in it is speakable.
fn word_phonemes(
    dictionaries: &Dictionaries,
    accent: Accent,
    raw: &str,
    tally: &mut Tally,
) -> String {
    let word = raw.trim_matches(|c| c == '\'' || c == '-').to_lowercase();
    if word.is_empty() {
        return String::new();
    }
    if let Some(words) = numbers::words(&word) {
        let said: Vec<String> = words
            .iter()
            .map(|w| {
                lookup(dictionaries, accent, w)
                    .map(|(p, _)| p)
                    .unwrap_or_else(|| spell(dictionaries, w))
            })
            .collect();
        tally.lexicon += 1;
        return said.join(" ");
    }
    if let Some((phonemes, source)) = lookup(dictionaries, accent, &word) {
        match source {
            Source::Lexicon => tally.lexicon += 1,
            Source::Derived => tally.derived += 1,
            Source::Fallback => tally.fallback += 1,
        }
        return phonemes;
    }
    if word.contains('-') {
        let parts: Vec<String> = word
            .split('-')
            .filter(|p| !p.is_empty())
            .map(|part| word_phonemes(dictionaries, accent, part, tally))
            .filter(|p| !p.is_empty())
            .collect();
        return parts.join(" ");
    }
    if let Some(phonemes) = compound(dictionaries, accent, &word) {
        tally.fallback += 1;
        return phonemes;
    }
    let spelled = spell(dictionaries, &word);
    if !spelled.is_empty() {
        tally.spelled += 1;
    }
    spelled
}

/// Shortest part of a compound ("mash" + "up"): shorter pieces would split
/// unknown names into accidental words.
const MIN_COMPOUND_PART: usize = 3;

/// An unknown word as two known words written together ("mashup",
/// "doorkeeper"): the split with the longest known first part. The second
/// part's primary stress becomes secondary.
fn compound(dictionaries: &Dictionaries, accent: Accent, word: &str) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 2 * MIN_COMPOUND_PART || !chars.iter().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    (MIN_COMPOUND_PART..=chars.len() - MIN_COMPOUND_PART)
        .rev()
        .find_map(|split| {
            let (head, tail) = word.split_at(split);
            let (first, _) = lookup(dictionaries, accent, head)?;
            let (second, _) = lookup(dictionaries, accent, tail)?;
            Some(format!("{first}{}", second.replace('ˈ', "ˌ")))
        })
}

/// A word from the dictionaries, with its source, in this order: the
/// lexicon (also with accents folded), a regular inflection of a lexicon
/// stem, the fallback dictionary, an inflection of a fallback stem.
fn lookup(dictionaries: &Dictionaries, accent: Accent, word: &str) -> Option<(String, Source)> {
    let folded = fold_accents(word);
    for key in [word, folded.as_str()] {
        if let Some(p) = dictionaries.lexicon.get(key) {
            return Some((p.to_owned(), Source::Lexicon));
        }
    }
    let inflected = |stems_from: Source| -> Option<String> {
        let stem = |s: &str| match stems_from {
            Source::Fallback => dictionaries.fallback.get(s),
            _ => dictionaries.lexicon.get(s),
        };
        match folded.strip_suffix("'s") {
            Some(base) => stem(base).map(|b| format!("{b}{}", plural_ending(b, accent))),
            None => derived(&stem, accent, &folded),
        }
    };
    if let Some(p) = inflected(Source::Lexicon) {
        return Some((p, Source::Derived));
    }
    for key in [word, folded.as_str()] {
        if let Some(p) = dictionaries.fallback.get(key) {
            return Some((p.to_owned(), Source::Fallback));
        }
    }
    inflected(Source::Fallback).map(|p| (p, Source::Fallback))
}

/// Regular inflections and -ly of a stem that `stem` knows.
fn derived<'a>(
    stem: &dyn Fn(&str) -> Option<&'a str>,
    accent: Accent,
    word: &str,
) -> Option<String> {
    let first = |stems: &[String]| {
        stems
            .iter()
            .find_map(|s| (s.len() > 1).then(|| stem(s)).flatten())
    };
    if let Some(rest) = word.strip_suffix("ing") {
        let mut stems = vec![rest.to_owned(), format!("{rest}e")];
        if let Some(undoubled) = undouble(rest) {
            stems.push(undoubled);
        }
        if let Some(base) = first(&stems) {
            return Some(format!("{base}ɪŋ"));
        }
    }
    if let Some(rest) = word.strip_suffix("ed") {
        let mut stems = vec![rest.to_owned(), format!("{rest}e")];
        if let Some(stem) = rest.strip_suffix('i') {
            stems.push(format!("{stem}y"));
        }
        if let Some(undoubled) = undouble(rest) {
            stems.push(undoubled);
        }
        if let Some(base) = first(&stems) {
            return Some(format!("{base}{}", past_ending(base, accent)));
        }
    }
    if let Some(rest) = word.strip_suffix("ly") {
        let mut stems = vec![rest.to_owned()];
        if let Some(stem) = rest.strip_suffix('i') {
            stems.push(format!("{stem}y"));
        }
        if let Some(base) = first(&stems) {
            return Some(format!("{base}li"));
        }
    }
    if let Some(rest) = word.strip_suffix('s') {
        if rest.ends_with('s') && !rest.ends_with("ss") {
            return None;
        }
        let mut stems = vec![rest.to_owned()];
        if let Some(stem) = rest.strip_suffix('e') {
            stems.push(stem.to_owned());
            if let Some(stem) = stem.strip_suffix('i') {
                stems.push(format!("{stem}y"));
            }
        }
        if let Some(base) = first(&stems) {
            return Some(format!("{base}{}", plural_ending(base, accent)));
        }
    }
    None
}

/// "stopp" -> "stop" (a doubled final consonant before -ing/-ed).
fn undouble(stem: &str) -> Option<String> {
    let mut chars = stem.chars().rev();
    let (last, before) = (chars.next()?, chars.next()?);
    (last == before && !"aeiouy".contains(last))
        .then(|| stem[..stem.len() - last.len_utf8()].to_owned())
}

/// The last sound of a phoneme string (stress and length marks skipped).
fn last_sound(phonemes: &str) -> Option<char> {
    phonemes
        .chars()
        .rev()
        .find(|c| !MARKS.contains(c) && *c != ' ')
}

/// The unstressed vowel of "-es"/"-ed" endings in each lexicon.
fn weak_vowel(accent: Accent) -> char {
    match accent {
        Accent::Us => 'ᵻ',
        Accent::Gb => 'ɪ',
    }
}

/// -s/-es/'s: /ɪz/ after sibilants, /s/ after other voiceless sounds, else /z/.
fn plural_ending(base: &str, accent: Accent) -> String {
    match last_sound(base) {
        Some('s' | 'z' | 'ʃ' | 'ʒ' | 'ʧ' | 'ʤ') => format!("{}z", weak_vowel(accent)),
        Some('p' | 't' | 'k' | 'f' | 'θ') => "s".to_owned(),
        _ => "z".to_owned(),
    }
}

/// -ed: /ɪd/ after t and d, /t/ after other voiceless sounds, else /d/.
fn past_ending(base: &str, accent: Accent) -> String {
    match last_sound(base) {
        Some('t' | 'd') => format!("{}d", weak_vowel(accent)),
        Some('p' | 'k' | 'f' | 'θ' | 's' | 'ʃ' | 'ʧ') => "t".to_owned(),
        _ => "d".to_owned(),
    }
}

/// "café" -> "cafe".
fn fold_accents(word: &str) -> String {
    word.nfd().filter(|c| !is_combining_mark(*c)).collect()
}

/// Letter by letter, with the lexicon's letter names (digits as numbers);
/// other characters are skipped.
fn spell(dictionaries: &Dictionaries, word: &str) -> String {
    let names: Vec<&str> = fold_accents(word)
        .chars()
        .filter_map(|c| {
            let name = match c.to_digit(10) {
                Some(d) => numbers::words(&d.to_string())?.into_iter().next()?,
                None => c.to_string(),
            };
            dictionaries.lexicon.get(&name)
        })
        .collect();
    names.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny synthetic lexicon in misaki notation (not the real one).
    const US: &str = "\
hello h ə l ˈ O
world w ˈ ɜ ɹ l d
the ð ə
cat k ˈ æ t
dog d ˈ ɔ ɡ
horse h ˈ ɔ ɹ s
want w ˈ ɑ n t
walk w ˈ ɔ k
play p l ˈ A
stop s t ˈ ɑ p
make m ˈ A k
quick k w ˈ ɪ k
happy h ˈ æ p i
cafe k æ f ˈ A
nineteen n ˌ I n t ˈ i n
ninety n ˈ I n t i
three θ ɹ ˈ i
third θ ˈ ɜ ɹ d
twenty t w ˈ ɛ n t i
one w ˈ ʌ n
a ˈ A
b b ˈ i
c s ˈ i
x ˈ ɛ k s
z z ˈ i
";
    const GB: &str = "\
dog d ˈ ɒ ɡ
horse h ˈ ɔ ː s
hello h ə l ˈ Q
a ˈ A
";
    /// Every symbol used above plus space and the pause marks.
    fn symbols() -> HashSet<char> {
        let mut set: HashSet<char> = format!("{US}{GB}")
            .lines()
            .flat_map(|l| {
                l.split_whitespace()
                    .skip(1)
                    .flat_map(str::chars)
                    .collect::<Vec<_>>()
            })
            .collect();
        set.extend([
            ' ', ',', '.', '!', '?', ';', ':', '—', '…', 'ᵻ', 'ɪ', 'ŋ', 'z', 's', 'd', 't', 'l',
            'i', 'u',
        ]);
        set
    }

    fn phonemizer() -> Phonemizer {
        Phonemizer::new(
            Lexicon::parse(US).expect("test setup"),
            Lexicon::parse(GB).expect("test setup"),
            symbols(),
        )
    }

    fn us(text: &str) -> Phonemes {
        phonemizer().phonemize(&SpeakText::new(text).expect("test setup"), Accent::Us)
    }

    #[test]
    fn r40_words_and_pauses() {
        let p = us("Hello, world! The cat.");
        assert_eq!(p.chunks(), ["həlˈO, wˈɜɹld!", "ðə kˈæt."]);
        assert_eq!(p.spelled_out(), 0);
        assert_eq!(p.symbol_count(), 14 + 8);
    }

    #[test]
    fn r42_the_accent_selects_the_lexicon() {
        let text = SpeakText::new("Hello dog").expect("test setup");
        let p = phonemizer();
        assert_eq!(p.phonemize(&text, Accent::Us).chunks(), ["həlˈO dˈɔɡ"]);
        assert_eq!(p.phonemize(&text, Accent::Gb).chunks(), ["həlˈQ dˈɒɡ"]);
    }

    #[test]
    fn r40_regular_inflections_are_derived() {
        assert_eq!(us("dogs cats horses").chunks(), ["dˈɔɡz kˈæts hˈɔɹsᵻz"]);
        assert_eq!(us("wanted walked played").chunks(), ["wˈɑntᵻd wˈɔkt plˈAd"]);
        assert_eq!(
            us("stopped stopping making").chunks(),
            ["stˈɑpt stˈɑpɪŋ mˈAkɪŋ"]
        );
        assert_eq!(us("quickly happily").chunks(), ["kwˈɪkli hˈæpili"]);
        assert_eq!(us("dog's").chunks(), ["dˈɔɡz"]);
        let gb = phonemizer().phonemize(&SpeakText::new("horses").expect("test setup"), Accent::Gb);
        assert_eq!(gb.chunks(), ["hˈɔːsɪz"]);
    }

    #[test]
    fn r40_numbers_are_read() {
        assert_eq!(us("1990").chunks(), ["nˌIntˈin nˈInti"]);
        assert_eq!(us("3rd").chunks(), ["θˈɜɹd"]);
    }

    #[test]
    fn r40_unknown_words_are_spelled_not_dropped() {
        let p = us("Zxc cat");
        assert_eq!(p.chunks(), ["zˈi ˈɛks sˈi kˈæt"]);
        assert_eq!(p.spelled_out(), 1);
    }

    #[test]
    fn r40_fallback_dictionary_covers_words_the_lexicon_lacks() {
        let cmu = "zxc Z IH1 K S\nsusurrus S UW0 S ER1 AH0 S\ncat K AA1 T\n";
        let p = phonemizer().with_fallback(cmu);
        let text = SpeakText::new("Zxc susurruses cat").expect("test setup");
        let us = p.phonemize(&text, Accent::Us);
        // "cat" stays the lexicon's; the fallback only fills gaps.
        assert_eq!(us.chunks(), ["zˈɪks susˈɜɹəsᵻz kˈæt"]);
        assert_eq!(
            us.tally(),
            Tally {
                lexicon: 1,
                derived: 0,
                fallback: 2,
                spelled: 0,
                dropped: 0
            }
        );
        assert_eq!(us.tally().approximate(), 2);
        let gb = p.phonemize(&text, Accent::Gb);
        assert_eq!(gb.chunks()[0].split(' ').next(), Some("zˈɪks"));
        assert_eq!(p.fallback_sizes(), (2, 3));
    }

    #[test]
    fn r40_unknown_compounds_are_split_into_known_words() {
        let p = us("catdog dogcats");
        assert_eq!(p.chunks(), ["kˈætdˌɔɡ dˈɔɡkˌæts"]);
        assert_eq!(p.tally().fallback, 2);
        // Parts shorter than three letters are not words of a compound.
        assert_eq!(us("catz").tally().spelled, 1);
    }

    #[test]
    fn r40_tally_counts_lexicon_derived_and_spelled_words() {
        let p = us("The dogs walked, zxc 1990.");
        assert_eq!(
            p.tally(),
            Tally {
                lexicon: 2,
                derived: 2,
                fallback: 0,
                spelled: 1,
                dropped: 0
            }
        );
        assert_eq!(p.tally().approximate(), 1);
    }

    #[test]
    fn r40_accents_curly_apostrophes_and_hyphens() {
        assert_eq!(us("café").chunks(), ["kæfˈA"]);
        assert_eq!(us("dog’s").chunks(), ["dˈɔɡz"]);
        assert_eq!(us("cat-dog").chunks(), ["kˈæt dˈɔɡ"]);
        assert_eq!(us("“Hello” (world)…").chunks(), ["həlˈO, wˈɜɹld,…"]);
        assert_eq!(us("cat -- dog...").chunks(), ["kˈæt— dˈɔɡ…"]);
    }

    #[test]
    fn r40_nothing_speakable_gives_no_chunks() {
        assert!(us("!!! ...").is_empty());
        assert!(us("¶ §").is_empty());
    }

    #[test]
    fn r40_long_text_is_chunked_within_the_model_context() {
        let text = vec!["hello"; 90].join(" ");
        let p = us(&text);
        assert!(p.chunks().len() > 1);
        assert!(
            p.chunks()
                .iter()
                .all(|c| c.chars().count() <= MAX_CHUNK_SYMBOLS)
        );
        assert_eq!(p.chunks().concat().matches("həlˈO").count(), 90);
    }

    #[test]
    fn r40_no_chunk_exceeds_the_limit_even_for_one_huge_word() {
        let mut chunks = Chunks::default();
        chunks.word("həlˈO");
        chunks.word(&"ab".repeat(400));
        chunks.pause(',');
        chunks.word("kˈæt");
        let done = chunks.finish();
        assert!(
            done.iter().all(|c| c.chars().count() <= MAX_CHUNK_SYMBOLS),
            "{done:?}"
        );
        let symbols: usize = done
            .iter()
            .map(|c| c.chars().filter(|c| *c != ' ').count())
            .sum();
        assert_eq!(symbols, 5 + 800 + 1 + 4);
        // Hyphenated compounds and long numbers are several words each.
        for text in [["zxc"; 10].join("-"), "zxc".repeat(21)] {
            let p = us(&text);
            assert!(!p.is_empty());
            assert!(
                p.chunks()
                    .iter()
                    .all(|c| c.chars().count() <= MAX_CHUNK_SYMBOLS),
                "{text}"
            );
        }
    }

    #[test]
    fn r40_a_long_chunk_is_cut_at_its_last_pause() {
        let mut chunks = Chunks::default();
        let word = "a".repeat(40);
        for _ in 0..2 {
            chunks.word(&word);
        }
        chunks.pause(',');
        chunks.word(&word);
        // The fourth word does not fit: the chunk ends at the comma and the
        // third word moves on with it.
        chunks.word(&word);
        let done = chunks.finish();
        assert_eq!(done.len(), 2, "{done:?}");
        assert!(done[0].ends_with(','), "{done:?}");
        assert_eq!(done[0].chars().count(), 82);
        assert_eq!(done[1], format!("{word} {word}"));
        // No pause to cut at: the chunk ends before the word.
        let mut chunks = Chunks::default();
        for _ in 0..4 {
            chunks.word(&word);
        }
        let done = chunks.finish();
        assert_eq!(
            done.iter().map(|c| c.chars().count()).collect::<Vec<_>>(),
            [122, 40]
        );
    }

    #[test]
    fn r40_invisible_characters_join_the_word_instead_of_spelling_it() {
        let p = us("do\u{00AD}g ca\u{200B}t");
        assert_eq!(p.chunks(), ["dˈɔɡ kˈæt"]);
        assert_eq!(p.tally().approximate(), 0);
    }

    #[test]
    fn r40_symbols_are_read_or_counted_as_dropped() {
        assert_eq!(
            pieces("$5 €1 £2.50"),
            ["5", "dollars", "1", "euro", "2.50", "pounds"].map(|w| Piece::Word(w.to_owned()))
        );
        assert_eq!(
            pieces("20° = x"),
            ["20", "degrees", "equals", "x"].map(|w| Piece::Word(w.to_owned()))
        );
        // A lone currency sign is not read; quotes are silent, not dropped.
        assert_eq!(
            pieces("$ \"cat\""),
            [Piece::Dropped, Piece::Word("cat".to_owned())]
        );
        let p = us("cat ¶ dog §");
        assert_eq!(p.chunks(), ["kˈæt dˈɔɡ"]);
        assert_eq!(p.tally().dropped, 2);
        assert_eq!(p.tally().approximate(), 2);
        assert_eq!(us("“cat”").tally().approximate(), 0);
    }

    #[test]
    fn r24_phonemes_debug_shows_sizes_only() {
        let shown = format!("{:?}", us("hello world"));
        assert_eq!(shown, "Phonemes(1 chunks, 12 symbols, 0 spelled out)");
    }

    #[test]
    fn r40_model_file_formats() {
        let symbols = parse_symbols("$ 0\n; 1\n  16\nˈ 156\n").expect("test setup");
        assert_eq!(symbols, HashSet::from(['$', ';', ' ', 'ˈ']));
        assert_eq!(
            parse_symbols("ab 1\n"),
            Err(FormatError {
                line: 1,
                message: "expected `<symbol> <id>`"
            })
        );
        assert!(Lexicon::parse("word\n").is_err());
        assert!(Lexicon::parse("\n\n").is_err());
        let lexicon = Lexicon::parse("Word w ɜ d\nword w ˈ ɜ d\n").expect("test setup");
        assert_eq!(lexicon.get("word"), Some("wˈɜd"));
    }
}
