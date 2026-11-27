//! The two exports (design §3.2 `exports`, §8.1): the feedback for Claude
//! (R-57, D-12) and the full export of all vocabulary and progress (R-86,
//! D-16). Read-only; built from the read and export functions of the
//! modules that own the data (R-92).
//!
//! * **Feedback** is small (it lists only what the owner changed and his
//!   problem words) and is built in memory.
//! * **Full** can be large (every review ever made): it is written row by
//!   row to any [`Write`], never collected, so the HTTP layer can stream it.

use std::collections::HashSet;
use std::io::Write;

use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

use crate::catalog::{
    self, FeedbackAccepted, FeedbackConflict, FeedbackOverride, FeedbackSentence,
};
use crate::clock::{Moment, StudyDay};
use crate::db::{Db, TableError, TableSink};
use crate::{importer, scheduler, settings};

/// `format` of the feedback export.
pub const FEEDBACK_FORMAT: &str = "vocab-feedback";
/// `format` of the full export.
pub const FULL_FORMAT: &str = "vocab-full-export";
/// Version of both formats (design §8.1).
pub const FORMAT_VERSION: u32 = 1;

/// What the feedback asks of Claude: it travels inside the export, so the
/// round trip works before the project instructions mention it (D-12).
pub const INSTRUCTIONS_FOR_CLAUDE: &str = "This is feedback from my vocabulary training app \
     (format vocab-feedback, version 1). Use it for the next vocabulary export: re-export the \
     words named here in the usual vocab-pack format, grouped by their source, with every field \
     as before except what this asks you to improve. needs_hint: words I keep failing that have \
     no memory hook; write a short `hint` for each. hard_words: words I keep failing although \
     they have a hint; write a better hint, and add `confusables` or clearer sentences if they \
     help. hidden_sentences: sentences I hid; do not export them again and give those words \
     other sentences instead. edited_sentences: sentences I corrected; export them as they are \
     now. owner_accepted: answers I accepted when typing the word; add them to `accepted` if \
     they are right in this meaning. overrides: values I set myself (goal, usefulness, ...); \
     keep them, or explain why you would suggest another. open_conflicts: values of a later \
     pack I have not decided on; say which one you think is right.";

/// A problem word as the feedback names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedbackWord {
    pub word: String,
    pub sources: Vec<String>,
    pub czech: Vec<String>,
    /// The current hint (only in `hard_words`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub confusables: Vec<String>,
}

/// The feedback export (R-57, design §8.1, D-12): vocabulary data only.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackExport {
    pub format: &'static str,
    pub format_version: u32,
    pub generated_on: StudyDay,
    pub instructions_for_claude: &'static str,
    /// Problem words without a hint.
    pub needs_hint: Vec<FeedbackWord>,
    /// Problem words that have a hint.
    pub hard_words: Vec<FeedbackWord>,
    pub hidden_sentences: Vec<FeedbackSentence>,
    pub edited_sentences: Vec<FeedbackSentence>,
    pub owner_accepted: Vec<FeedbackAccepted>,
    pub overrides: Vec<FeedbackOverride>,
    pub open_conflicts: Vec<FeedbackConflict>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Db(#[from] crate::db::DbError),
    #[error("writing the export: {0}")]
    Io(#[from] std::io::Error),
    #[error("encoding the export: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<TableError> for ExportError {
    fn from(err: TableError) -> Self {
        match err {
            TableError::Sqlite(e) => Self::Sqlite(e),
            TableError::Io(e) => Self::Io(e),
        }
    }
}

/// The problem words now (design §6.5: a flagged card, or an acknowledged
/// one not yet remembered for three weeks), suspended words left out —
/// the list the Progress screen shows.
fn problem_word_ids(conn: &Connection) -> rusqlite::Result<Vec<catalog::WordId>> {
    let mut ids: Vec<catalog::WordId> = scheduler::leech_cards(conn)?
        .iter()
        .filter(|c| c.is_problem())
        .map(|c| c.word_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let traits = catalog::traits(conn, &ids)?;
    ids.retain(|id| traits.get(id).is_some_and(|t| !t.suspended));
    Ok(ids)
}

/// The feedback export on study day `day`.
pub fn feedback(conn: &Connection, day: StudyDay) -> Result<FeedbackExport, ExportError> {
    let problem = catalog::problem_words(conn, &problem_word_ids(conn)?)?;
    let (with_hint, without_hint): (Vec<_>, Vec<_>) =
        problem.into_iter().partition(|w| w.hint.is_some());
    let word = |w: catalog::ProblemWord| FeedbackWord {
        word: w.headword,
        sources: w.sources,
        czech: w.czech,
        hint: w.hint,
        confusables: w.confusables,
    };
    let items = catalog::feedback_items(conn)?;
    Ok(FeedbackExport {
        format: FEEDBACK_FORMAT,
        format_version: FORMAT_VERSION,
        generated_on: day,
        instructions_for_claude: INSTRUCTIONS_FOR_CLAUDE,
        needs_hint: without_hint.into_iter().map(word).collect(),
        hard_words: with_hint.into_iter().map(word).collect(),
        hidden_sentences: items.hidden_sentences,
        edited_sentences: items.edited_sentences,
        owner_accepted: items.owner_accepted,
        overrides: items.overrides,
        open_conflicts: items.open_conflicts,
    })
}

/// Writes tables as members of the export's JSON object:
/// `"name":{"columns":[…],"rows":[` one row (an array of values) per line
/// `]}`.
#[derive(Debug)]
struct JsonTables<W: Write> {
    out: W,
    first_row: bool,
}

impl<W: Write> TableSink for JsonTables<W> {
    fn begin_table(&mut self, name: &str, columns: &[String]) -> std::io::Result<()> {
        write!(self.out, ",\n{}:{{\"columns\":", Value::from(name))?;
        serde_json::to_writer(&mut self.out, columns)?;
        self.out.write_all(b",\"rows\":[")?;
        self.first_row = true;
        Ok(())
    }

    fn row(&mut self, values: &[Value]) -> std::io::Result<()> {
        self.out
            .write_all(if self.first_row { b"\n" } else { b",\n" })?;
        self.first_row = false;
        serde_json::to_writer(&mut self.out, values)?;
        Ok(())
    }

    fn end_table(&mut self) -> std::io::Result<()> {
        self.out.write_all(b"\n]}")
    }
}

/// Write the full export (design §8.1) to `out`: a header, then every
/// table row by row. Nothing is collected in memory; an error stops the
/// output midway (the caller makes sure a cut-off file cannot pass for a
/// complete one: its closing `}` is missing).
pub fn write_full(conn: &Connection, out: impl Write, now: &Moment) -> Result<(), ExportError> {
    let header: [(&str, Value); 7] = [
        ("format", Value::from(FULL_FORMAT)),
        ("format_version", Value::from(FORMAT_VERSION)),
        ("exported_at", Value::from(now.stamp())),
        ("study_day", Value::from(now.day.to_string())),
        ("app_version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("schema_version", Value::from(Db::schema_version())),
        ("settings", serde_json::to_value(settings::get(conn)?)?),
    ];
    let mut tables = JsonTables {
        out,
        first_row: true,
    };
    tables.out.write_all(b"{")?;
    for (i, (key, value)) in header.iter().enumerate() {
        if i > 0 {
            tables.out.write_all(b",\n")?;
        }
        write!(tables.out, "{}:", Value::from(*key))?;
        serde_json::to_writer(&mut tables.out, value)?;
    }
    catalog::export(conn, &mut tables)?;
    importer::export(conn, &mut tables)?;
    scheduler::export(conn, &mut tables)?;
    tables.out.write_all(b"\n}\n")?;
    tables.out.flush()?;
    Ok(())
}
