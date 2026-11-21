//! Pack import: preparing a paste, the commit order, import reports and the
//! idempotency log (design §3.2, §5.1, §5.5, R-11, R-13, R-24).
//!
//! Owned table: `pack_import` (one row per imported pack, unique by
//! `(pack_id, content_sha256)`). The atomic per-pack import itself, which
//! also writes catalog tables, is `workflows::commit_import`; this module
//! provides its pieces.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::Value;
use vocab_pack::paste::{PasteError, split_paste};
use vocab_pack::types::{Issue, PackHeader, PackValidation, WordResult};
use vocab_pack::validate::validate;

use crate::catalog::{MergeOutcome, WordMerge};

/// How a pack arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Paste,
    Connector,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Self::Paste => "paste",
            Self::Connector => "connector",
        }
    }
}

// ── preparing a paste ───────────────────────────────────────────────────────

/// One pack of a paste after splitting and validation.
#[derive(Debug, Clone)]
pub struct PreparedPack {
    /// 1-based position in the paste.
    pub position: usize,
    pub content_sha256: Option<String>,
    /// `Err`: the text could not be read as JSON (or broke a paste limit).
    pub checked: Result<PackValidation, String>,
}

impl PreparedPack {
    /// The header of a pack that passed the pack-level rules.
    pub fn header(&self) -> Option<&PackHeader> {
        self.checked.as_ref().ok()?.pack.as_ref().ok()
    }
}

/// Split and validate a paste. Pure: no database access.
pub fn prepare(text: &str) -> Result<Vec<PreparedPack>, PasteError> {
    Ok(split_paste(text)?
        .into_iter()
        .enumerate()
        .map(|(i, item)| match item {
            Ok(raw) => PreparedPack {
                position: i + 1,
                checked: Ok(validate(&raw.value)),
                content_sha256: Some(raw.content_sha256),
            },
            Err(err) => PreparedPack {
                position: i + 1,
                content_sha256: None,
                checked: Err(err.message),
            },
        })
        .collect())
}

/// Indices (into `packs`) of the importable packs in the order they must be
/// committed (design §5.5): by source, export date, pack number and part,
/// so that pasting the parts of an export in any order gives the same
/// `seq`. Packs that cannot be imported are left out.
pub fn commit_order(packs: &[PreparedPack]) -> Vec<usize> {
    let mut order: Vec<(usize, &PackHeader)> = packs
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.header().map(|h| (i, h)))
        .collect();
    order.sort_by(|(ia, a), (ib, b)| {
        let date = |h: &PackHeader| h.exported_at.clone().unwrap_or_else(|| h.pack_date.clone());
        a.source
            .id
            .cmp(&b.source.id)
            .then_with(|| date(a).cmp(&date(b)))
            .then_with(|| a.pack_number.cmp(&b.pack_number))
            .then_with(|| a.part.index.cmp(&b.part.index))
            .then_with(|| ia.cmp(ib))
    });
    order.into_iter().map(|(i, _)| i).collect()
}

// ── reports ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Preview,
    Commit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackStatus {
    /// Preview: would be imported.
    Ready,
    Imported,
    /// Same `pack_id` and content imported before: nothing to do (R-11).
    AlreadyImported,
    /// Pack-level errors.
    Rejected,
    /// Not readable as JSON, or over a paste limit.
    Unreadable,
    /// Valid, but importing it failed (an unexpected error, or a hard cap
    /// of the app); nothing of this pack was stored. Other packs of the
    /// paste are not affected.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WordOutcome {
    New,
    Merged,
    Unchanged,
    /// Dropped because of word errors.
    Invalid,
    /// Valid, but its pack was not imported (rejected or already imported).
    NotImported,
}

/// A validation finding as sent to the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueView {
    pub pointer: String,
    pub code: &'static str,
    pub message: String,
}

impl From<&Issue> for IssueView {
    fn from(issue: &Issue) -> Self {
        Self {
            pointer: issue.pointer.clone(),
            code: issue.code.as_str(),
            message: issue.message.clone(),
        }
    }
}

fn views(issues: &[Issue]) -> Vec<IssueView> {
    issues.iter().map(IssueView::from).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceRef {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// `None` when the pack has no title (the source's stored or default
    /// title is used).
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PartRef {
    pub index: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadingPositionRef {
    pub value: String,
    /// The source's reading position is (or would be) set from this pack.
    pub updated: bool,
    /// Why it was not applied (`None` when it was).
    pub reason: Option<String>,
}

/// Counts for one pack or the whole paste.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub words: u32,
    pub new_words: u32,
    pub merged_words: u32,
    pub unchanged_words: u32,
    pub invalid_words: u32,
    pub new_sentences: u32,
    pub words_with_new_sentences: u32,
    /// Known sentences whose text was corrected by the pack (R-22).
    pub updated_sentences: u32,
    /// Newly recorded field conflicts (existing open ones not counted).
    pub conflicts: u32,
    pub sentence_errors: u32,
    pub warnings: u32,
}

impl Summary {
    fn add(&mut self, other: &Self) {
        self.words += other.words;
        self.new_words += other.new_words;
        self.merged_words += other.merged_words;
        self.unchanged_words += other.unchanged_words;
        self.invalid_words += other.invalid_words;
        self.new_sentences += other.new_sentences;
        self.words_with_new_sentences += other.words_with_new_sentences;
        self.updated_sentences += other.updated_sentences;
        self.conflicts += other.conflicts;
        self.sentence_errors += other.sentence_errors;
        self.warnings += other.warnings;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordReport {
    /// Position in the pack's `words` array (0-based).
    pub index: usize,
    pub headword: Option<String>,
    pub outcome: WordOutcome,
    pub errors: Vec<IssueView>,
    pub warnings: Vec<IssueView>,
    pub sentence_errors: Vec<IssueView>,
    /// What the merge did (or would do).
    pub merge: Option<WordMerge>,
    /// E.g. "added by an earlier pack of this paste" in a preview.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackReport {
    pub position: usize,
    pub pack_id: Option<String>,
    pub content_sha256: Option<String>,
    pub source: Option<SourceRef>,
    pub part: Option<PartRef>,
    pub exported_at: Option<String>,
    pub status: PackStatus,
    pub already_imported_at: Option<String>,
    pub notes: Vec<String>,
    pub errors: Vec<IssueView>,
    pub warnings: Vec<IssueView>,
    pub reading_position: Option<ReadingPositionRef>,
    pub summary: Summary,
    pub words: Vec<WordReport>,
}

impl PackReport {
    /// The report of a prepared pack before any database work: errors and
    /// warnings filled in; status `Ready` when the pack can be imported.
    pub fn new(pack: &PreparedPack) -> Self {
        let mut report = Self {
            position: pack.position,
            pack_id: None,
            content_sha256: pack.content_sha256.clone(),
            source: None,
            part: None,
            exported_at: None,
            status: PackStatus::Ready,
            already_imported_at: None,
            notes: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            reading_position: None,
            summary: Summary::default(),
            words: Vec::new(),
        };
        let validation = match &pack.checked {
            Ok(validation) => validation,
            Err(message) => {
                report.status = PackStatus::Unreadable;
                report.errors.push(IssueView {
                    pointer: String::new(),
                    code: "unreadable",
                    message: message.clone(),
                });
                report.finish();
                return report;
            }
        };
        match &validation.pack {
            Ok(header) => {
                report.pack_id = Some(header.pack_id.clone());
                report.source = Some(SourceRef {
                    id: header.source.id.clone(),
                    kind: header.source.kind.as_str(),
                    title: header.source.title.clone(),
                });
                report.part = Some(PartRef {
                    index: header.part.index,
                    total: header.part.total,
                });
                report.exported_at.clone_from(&header.exported_at);
                report.reading_position =
                    header
                        .reading_position
                        .clone()
                        .map(|value| ReadingPositionRef {
                            value,
                            updated: false,
                            reason: None,
                        });
            }
            Err(errors) => {
                report.status = PackStatus::Rejected;
                report.errors = views(errors);
            }
        }
        report.warnings = views(&validation.warnings);
        report.words = validation
            .words
            .iter()
            .map(|word| match word {
                WordResult::Valid(w) => WordReport {
                    index: w.index,
                    headword: Some(w.headword.clone()),
                    outcome: WordOutcome::NotImported,
                    errors: Vec::new(),
                    warnings: views(&w.warnings),
                    sentence_errors: views(&w.sentence_errors),
                    merge: None,
                    note: None,
                },
                WordResult::Invalid(w) => WordReport {
                    index: w.index,
                    headword: w.headword.clone(),
                    outcome: WordOutcome::Invalid,
                    errors: views(&w.errors),
                    warnings: views(&w.warnings),
                    sentence_errors: Vec::new(),
                    merge: None,
                    note: None,
                },
            })
            .collect();
        report.finish();
        report
    }

    pub fn set_status(&mut self, status: PackStatus) {
        self.status = status;
        self.finish();
    }

    /// The same pack (id and content) was imported before.
    pub fn set_already_imported(&mut self, imported_at: Option<String>, note: Option<String>) {
        self.already_imported_at = imported_at;
        self.notes.extend(note);
        self.set_status(PackStatus::AlreadyImported);
    }

    pub fn add_note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// A pack-level warning found while importing (not by validation).
    pub fn add_warning(&mut self, pointer: &str, message: impl Into<String>) {
        self.warnings.push(IssueView {
            pointer: pointer.to_owned(),
            code: "advice",
            message: message.into(),
        });
        self.finish();
    }

    /// Whether the pack's reading position was applied, and if not, why.
    pub fn set_reading_position(&mut self, updated: bool, reason: Option<String>) {
        if let Some(reading) = &mut self.reading_position {
            reading.updated = updated;
            reading.reason = reason;
        }
    }

    /// Importing the pack failed and nothing of it was stored: the report
    /// goes back to what validation found, with `message` as its error.
    pub fn set_failed(&mut self, code: &'static str, message: impl Into<String>) {
        for word in &mut self.words {
            if word.outcome != WordOutcome::Invalid {
                word.outcome = WordOutcome::NotImported;
            }
            word.merge = None;
            word.note = None;
        }
        self.set_reading_position(false, None);
        self.errors.push(IssueView {
            pointer: String::new(),
            code,
            message: message.into(),
        });
        self.set_status(PackStatus::Failed);
    }

    /// Record the merge result of the word at `index` of the pack.
    pub fn record_merge(&mut self, index: usize, merge: WordMerge, note: Option<String>) {
        if let Some(word) = self.words.iter_mut().find(|w| w.index == index) {
            word.outcome = match merge.outcome {
                MergeOutcome::New => WordOutcome::New,
                MergeOutcome::Merged => WordOutcome::Merged,
                MergeOutcome::Unchanged => WordOutcome::Unchanged,
            };
            word.merge = Some(merge);
            word.note = note;
        }
        self.finish();
    }

    /// Recompute the summary.
    fn finish(&mut self) {
        let mut s = Summary {
            words: count(self.words.len()),
            warnings: count(
                self.warnings.len() + self.words.iter().map(|w| w.warnings.len()).sum::<usize>(),
            ),
            ..Summary::default()
        };
        for word in &self.words {
            s.sentence_errors += count(word.sentence_errors.len());
            match word.outcome {
                WordOutcome::New => s.new_words += 1,
                WordOutcome::Merged => s.merged_words += 1,
                WordOutcome::Unchanged => s.unchanged_words += 1,
                WordOutcome::Invalid => s.invalid_words += 1,
                WordOutcome::NotImported => {}
            }
            if let Some(merge) = &word.merge {
                s.new_sentences += merge.sentences_added;
                if merge.sentences_added > 0 {
                    s.words_with_new_sentences += 1;
                }
                s.updated_sentences += merge.sentences_updated;
                s.conflicts += count(merge.conflicts.iter().filter(|c| !c.already_open).count());
            }
        }
        self.summary = s;
    }

    /// What is stored in `pack_import.report_json`: counts, headwords and
    /// per-word outcomes. No sentence text, no field values (R-24).
    fn stored_json(&self) -> String {
        let words: Vec<Value> = self
            .words
            .iter()
            .map(|w| {
                serde_json::json!({
                    "index": w.index,
                    "headword": w.headword,
                    "outcome": w.outcome,
                    "sentences_added": w.merge.as_ref().map_or(0, |m| m.sentences_added),
                    "sentences_updated": w.merge.as_ref().map_or(0, |m| m.sentences_updated),
                    "conflict_fields": w.merge.as_ref().map(|m| {
                        m.conflicts.iter().filter(|c| !c.already_open).map(|c| c.field).collect::<Vec<_>>()
                    }),
                    "errors": w.errors.iter().map(|e| e.pointer.clone()).collect::<Vec<_>>(),
                    "sentence_errors": w.sentence_errors.iter().map(|e| e.pointer.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "summary": self.summary,
            "reading_position_updated": self.reading_position.as_ref().is_some_and(|r| r.updated),
            "notes": self.notes,
            "words": words,
        })
        .to_string()
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The report for a whole paste.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportReport {
    pub mode: Mode,
    /// In paste order.
    pub packs: Vec<PackReport>,
    pub totals: Summary,
}

impl ImportReport {
    pub fn new(mode: Mode, packs: Vec<PackReport>) -> Self {
        let mut packs = packs;
        packs.sort_by_key(|p| p.position);
        let mut totals = Summary::default();
        for pack in &packs {
            totals.add(&pack.summary);
        }
        Self {
            mode,
            packs,
            totals,
        }
    }
}

// ── the pack_import table ───────────────────────────────────────────────────

/// A previous import of the same pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorImport {
    pub id: i64,
    pub imported_at: String,
}

/// The import of exactly this pack (same `pack_id` and content), if any.
pub fn find_import(
    conn: &Connection,
    pack_id: &str,
    content_sha256: &str,
) -> rusqlite::Result<Option<PriorImport>> {
    conn.query_row(
        "SELECT id, imported_at FROM pack_import WHERE pack_id = ?1 AND content_sha256 = ?2",
        params![pack_id, content_sha256],
        |row| {
            Ok(PriorImport {
                id: row.get(0)?,
                imported_at: row.get(1)?,
            })
        },
    )
    .optional()
}

/// Whether a pack with this id and part index but other content was
/// imported before. (The parts of one multi-pack export may share a
/// `pack_id`; they differ in `part.index`.)
pub fn pack_id_reused(
    conn: &Connection,
    header: &PackHeader,
    content_sha256: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pack_import WHERE pack_id = ?1 AND part_index = ?2 \
         AND content_sha256 != ?3)",
        params![header.pack_id, header.part.index, content_sha256],
        |row| row.get(0),
    )
}

/// The warning shown when a `pack_id` arrives again with other content.
pub const PACK_ID_REUSED: &str =
    "A different pack with the same pack_id was imported before; this one is imported as well.";

/// Start recording an import; returns the `pack_import.id` that catalog rows
/// reference. The report is stored by [`finish_import`] in the same
/// transaction.
pub fn begin_import(
    tx: &Transaction<'_>,
    header: &PackHeader,
    content_sha256: &str,
    via: Via,
    now: &str,
) -> rusqlite::Result<i64> {
    tx.execute(
        "INSERT INTO pack_import (pack_id, content_sha256, format_version, source_slug, exported_at, \
         part_index, part_total, via, imported_at, report_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, '{}')",
        params![
            header.pack_id,
            content_sha256,
            header.format_version,
            header.source.id,
            header.exported_at,
            header.part.index,
            header.part.total,
            via.as_str(),
            now,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Store the final report of an import started with [`begin_import`].
pub fn finish_import(tx: &Transaction<'_>, id: i64, report: &PackReport) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE pack_import SET report_json = ?2 WHERE id = ?1",
        params![id, report.stored_json()],
    )?;
    Ok(())
}

/// One row of the import log.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportLogRow {
    pub id: i64,
    pub pack_id: String,
    pub source_slug: String,
    /// Filled by the caller from the catalog (this module does not read
    /// catalog tables).
    pub source_title: Option<String>,
    pub exported_at: Option<String>,
    pub part: PartRef,
    pub via: String,
    pub imported_at: String,
    pub summary: Value,
}

/// The most recent imports, newest first.
pub fn recent(conn: &Connection, limit: u32) -> rusqlite::Result<Vec<ImportLogRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, pack_id, source_slug, exported_at, part_index, part_total, via, imported_at, \
         report_json FROM pack_import ORDER BY id DESC LIMIT ?1",
    )?;
    stmt.query_map([limit], |row| {
        let report: String = row.get(8)?;
        let summary = serde_json::from_str::<Value>(&report)
            .ok()
            .and_then(|mut v| v.get_mut("summary").map(Value::take))
            .unwrap_or(Value::Null);
        Ok(ImportLogRow {
            id: row.get(0)?,
            pack_id: row.get(1)?,
            source_slug: row.get(2)?,
            source_title: None,
            exported_at: row.get(3)?,
            part: PartRef {
                index: row.get(4)?,
                total: row.get(5)?,
            },
            via: row.get(6)?,
            imported_at: row.get(7)?,
            summary,
        })
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(source: &str, date: &str, n: u32, part: (u32, u32)) -> String {
        format!(
            r#"{{"format":"vocab-pack","format_version":1,"pack_id":"{source}--{date}--{n}","exported_at":"{date}",
               "part":{{"index":{},"total":{}}},"source":{{"id":"{source}","type":"book","title":"T"}},
               "words":[{{"headword":"w","meaning":"m","czech":["c"],"usefulness":5}}]}}"#,
            part.0, part.1
        )
    }

    #[test]
    fn r13_commit_order_sorts_parts_of_one_export() {
        let paste = [
            pack("test-b", "2026-11-17", 1, (3, 3)),
            "not json".to_owned(),
            pack("test-b", "2026-11-17", 1, (1, 3)),
            pack("test-a", "2026-11-18", 1, (1, 1)),
            pack("test-b", "2026-11-16", 2, (1, 1)),
            pack("test-b", "2026-11-17", 1, (2, 3)),
        ]
        .join("\n");
        let packs = prepare(&paste).expect("paste");
        // "not json" stops the stream: the rest of the paste is one error.
        assert_eq!(packs.len(), 2);

        let fenced: String = [
            pack("test-b", "2026-11-17", 1, (3, 3)),
            "{".to_owned(),
            pack("test-b", "2026-11-17", 1, (1, 3)),
            pack("test-a", "2026-11-18", 1, (1, 1)),
            pack("test-b", "2026-11-16", 2, (1, 1)),
            pack("test-b", "2026-11-17", 1, (2, 3)),
        ]
        .iter()
        .map(|p| format!("```json\n{p}\n```\nsome prose\n"))
        .collect();
        let packs = prepare(&fenced).expect("paste");
        assert_eq!(packs.len(), 6);
        let order: Vec<usize> = commit_order(&packs)
            .into_iter()
            .map(|i| packs[i].position)
            .collect();
        assert_eq!(order, vec![4, 5, 3, 6, 1]);
    }

    #[test]
    fn r11_report_counts_word_outcomes() {
        let packs = prepare(&pack("test-a", "2026-11-17", 1, (1, 1))).expect("paste");
        let mut report = PackReport::new(&packs[0]);
        assert_eq!(report.status, PackStatus::Ready);
        assert_eq!(report.summary.words, 1);
        assert_eq!(report.words[0].outcome, WordOutcome::NotImported);
        report.set_status(PackStatus::Rejected);
        assert_eq!(report.status, PackStatus::Rejected);
        let unreadable = PreparedPack {
            position: 2,
            content_sha256: None,
            checked: Err("bad".to_owned()),
        };
        let report = PackReport::new(&unreadable);
        assert_eq!(report.status, PackStatus::Unreadable);
        assert_eq!(report.errors[0].message, "bad");
    }

    #[test]
    fn r11_sort_is_stable_for_equal_keys() {
        let a = pack("test-a", "2026-11-17", 1, (1, 1));
        let packs = prepare(&format!("{a}\n{a}")).expect("paste");
        let order = commit_order(&packs);
        assert_eq!(order, vec![0, 1]);
    }
}
