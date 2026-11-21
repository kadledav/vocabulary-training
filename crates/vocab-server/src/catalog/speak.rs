//! What can be spoken (design §10.3 step 1): per word the pack's `speak`
//! text or else the headword, and the plain English text of every visible
//! sentence (markers removed). The `audio` module decides how the text is
//! normalised for the TTS service; this is only the catalog's read.

use rusqlite::{Connection, OptionalExtension, params};
use vocab_pack::marked::strip_markers;

use super::WordId;

/// The speakable texts of one word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakSource {
    pub word_id: WordId,
    /// `speak` if set (non-blank), else the headword.
    pub word: String,
    /// (sentence id, English text without `[[…]]`) of the visible sentences.
    pub sentences: Vec<(i64, String)>,
}

fn word_text(headword: String, speak: Option<String>) -> String {
    speak.filter(|s| !s.trim().is_empty()).unwrap_or(headword)
}

/// Every word's speakable texts, in word id order.
pub fn speak_sources(conn: &Connection) -> rusqlite::Result<Vec<SpeakSource>> {
    let mut words = conn.prepare("SELECT id, headword, speak FROM word ORDER BY id")?;
    let mut out: Vec<SpeakSource> = words
        .query_map([], |row| {
            Ok(SpeakSource {
                word_id: WordId(row.get(0)?),
                word: word_text(row.get(1)?, row.get(2)?),
                sentences: Vec::new(),
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut sentences = conn.prepare(
        "SELECT word_id, id, en_marked FROM sentence WHERE hidden = 0 ORDER BY word_id, position, id",
    )?;
    let rows = sentences.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut index = 0;
    for row in rows {
        let (word_id, id, en) = row?;
        while index < out.len() && out[index].word_id.0 < word_id {
            index += 1;
        }
        if let Some(source) = out.get_mut(index).filter(|s| s.word_id.0 == word_id) {
            source.sentences.push((id, strip_markers(&en)));
        }
    }
    Ok(out)
}

/// One word's speakable texts (`None` if the word does not exist).
pub fn speak_source(conn: &Connection, id: WordId) -> rusqlite::Result<Option<SpeakSource>> {
    let Some((headword, speak)) = conn
        .query_row(
            "SELECT headword, speak FROM word WHERE id = ?1",
            [id.0],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT id, en_marked FROM sentence WHERE word_id = ?1 AND hidden = 0 ORDER BY position, id",
    )?;
    let sentences = stmt
        .query_map(params![id.0], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                strip_markers(&row.get::<_, String>(1)?),
            ))
        })?
        .collect::<Result<_, _>>()?;
    Ok(Some(SpeakSource {
        word_id: id,
        word: word_text(headword, speak),
        sentences,
    }))
}
