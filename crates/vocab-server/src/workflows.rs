//! Atomic operations that span several modules (design §3.2 `workflows`).
//!
//! Import (design §5.1): each pack of a paste is committed in **its own
//! transaction**, which holds the `pack_import` row (importer), the source
//! and all merged words (catalog). A pack is therefore imported completely
//! or not at all, and the `(pack_id, content_sha256)` key makes a repeated
//! commit a no-op.
//!
//! The commit loop runs in its own task, detached from the HTTP request: if
//! the request times out, only the waiting stops and every pack of the
//! paste is still imported. Re-pasting the same text afterwards is safe
//! because packs that were already imported are skipped.
//!
//! The preview runs exactly the same per-pack code in one transaction that
//! is always rolled back, so it reports what the commit will do (including
//! several packs of one paste touching the same word) and stores nothing.
//!
//! A pack whose import fails (an unexpected database error, or a hard cap
//! of the catalog) is reported as `failed` in the normal report; its
//! transaction is rolled back and the other packs of the paste are imported
//! as usual (R-23: never a bare error for a paste that was partly stored).
//!
//! Every merged word gets its cards in the same transaction
//! (`scheduler::ensure_cards`, design §5.1 step 4); [`ensure_all_cards`]
//! does the same for every stored word at startup, so databases imported
//! before cards existed are backfilled.

use std::collections::HashMap;

use rusqlite::Transaction;
use vocab_pack::paste::PasteError;
use vocab_pack::types::WordResult;

use crate::catalog::{self, CapacityError, EditError, ImportLink, ListKind, ReadingUpdate, WordId};
use crate::clock::Moment;
use crate::db::{Db, DbError};
use crate::importer::{
    self, ImportLogRow, ImportReport, Mode, PACK_ID_REUSED, PackReport, PackStatus, PreparedPack,
    Via,
};
use crate::scheduler::{self, CardKind};
use crate::undo::{self, UndoError, UndoToken};

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The paste as a whole is unusable (empty, too large, too many packs);
    /// not a failure of the app.
    #[error("{0}")]
    Paste(#[from] PasteError),
    #[error("import task failed: {0}")]
    Task(String),
    /// Importing the pack would pass a hard cap of the catalog.
    #[error(transparent)]
    Capacity(#[from] CapacityError),
    /// An owner edit was refused (the message is for the owner).
    #[error(transparent)]
    Edit(#[from] EditError),
    /// "Reset card" for a card the word does not have (yet).
    #[error("This word has no {0} card yet.")]
    NoSuchCard(CardKind),
    /// The Undo is gone or too old.
    #[error(transparent)]
    Undo(#[from] UndoError),
    /// The word is no longer as the change left it: undoing would guess.
    #[error(
        "This change can no longer be undone: the word was changed since (in another tab, by an import or by practice)."
    )]
    ChangedSince,
    /// A snapshot could not be written or read back (a bug, never the owner's).
    #[error("undo state: {0}")]
    UndoState(String),
}

/// What the owner is told when a pack could not be imported.
const PACK_FAILED: &str = "This pack could not be imported because of an unexpected error; \
     nothing of it was stored. Importing it again is safe.";

/// Mark `report` as failed because of `err` (which is logged, not shown:
/// it may quote pack text).
fn failed(mut report: PackReport, err: &WorkflowError) -> PackReport {
    match err {
        WorkflowError::Capacity(cap) => report.set_failed(
            "capacity",
            format!("This pack was not imported: {cap}. Nothing of it was stored."),
        ),
        other => {
            tracing::error!(error = %other, position = report.position, "pack import failed");
            report.set_failed("import_failed", PACK_FAILED);
        }
    }
    report
}

/// Time-dependent values shared by every pack of one import.
#[derive(Debug, Clone)]
struct ImportContext {
    via: Via,
    /// When the import happens (card creation uses its study day).
    moment: Moment,
    /// RFC 3339 timestamp of the import.
    now: String,
}

impl ImportContext {
    fn new(via: Via, moment: Moment) -> Self {
        Self {
            via,
            moment,
            now: moment.stamp(),
        }
    }
}

/// What committing `text` would do: the commit's own per-pack code, run in
/// one transaction that is always rolled back. Nothing is stored.
pub async fn preview_import(
    db: &Db,
    text: &str,
    now: Moment,
) -> Result<ImportReport, WorkflowError> {
    let packs = importer::prepare(text)?;
    let ctx = ImportContext::new(Via::Paste, now);
    let reports = db
        .dry_run(move |tx| {
            let mut reports: Vec<PackReport> = packs.iter().map(PackReport::new).collect();
            for index in importer::commit_order(&packs) {
                // A savepoint per pack: a pack that fails leaves nothing
                // behind for the packs after it, as in the real commit.
                tx.execute_batch("SAVEPOINT preview_pack")?;
                let report = match import_pack(tx, &packs[index], reports[index].clone(), &ctx) {
                    Ok(mut report) => {
                        if report.status == PackStatus::Imported {
                            report.set_status(PackStatus::Ready);
                        }
                        report
                    }
                    Err(err) => {
                        tx.execute_batch("ROLLBACK TO preview_pack")?;
                        failed(reports[index].clone(), &err)
                    }
                };
                tx.execute_batch("RELEASE preview_pack")?;
                reports[index] = report;
            }
            Ok::<_, WorkflowError>(reports)
        })
        .await?;
    Ok(ImportReport::new(Mode::Preview, reports))
}

/// Import every importable pack of `text`, each in its own transaction, in
/// commit order (design §5.5). The work runs in a separate task: dropping
/// the returned future (e.g. on a request timeout) does not stop it.
pub async fn commit_import(
    db: &Db,
    text: &str,
    via: Via,
    now: Moment,
) -> Result<ImportReport, WorkflowError> {
    let packs = importer::prepare(text)?;
    let db = db.clone();
    let ctx = ImportContext::new(via, now);
    let task = tokio::spawn(async move { commit_packs(&db, packs, &ctx).await });
    task.await
        .map_err(|err| WorkflowError::Task(err.to_string()))?
}

async fn commit_packs(
    db: &Db,
    packs: Vec<PreparedPack>,
    ctx: &ImportContext,
) -> Result<ImportReport, WorkflowError> {
    let mut reports: Vec<PackReport> = packs.iter().map(PackReport::new).collect();
    for index in importer::commit_order(&packs) {
        let pack = packs[index].clone();
        let report = reports[index].clone();
        let ctx = ctx.clone();
        let outcome = db
            .write(move |tx| import_pack(tx, &pack, report, &ctx))
            .await;
        reports[index] = match outcome {
            Ok(report) => report,
            // Rolled back as a whole; the packs before it stay imported and
            // the ones after it are still tried.
            Err(err) => failed(reports[index].clone(), &err),
        };
    }
    Ok(ImportReport::new(Mode::Commit, reports))
}

/// One pack, inside the caller's transaction.
fn import_pack(
    tx: &Transaction<'_>,
    pack: &PreparedPack,
    mut report: PackReport,
    ctx: &ImportContext,
) -> Result<PackReport, WorkflowError> {
    let (Some(header), Some(sha), Ok(validation)) =
        (pack.header(), pack.content_sha256.as_deref(), &pack.checked)
    else {
        // Not importable; `commit_order` never yields such packs.
        return Ok(report);
    };
    if let Some(prior) = importer::find_import(tx, &header.pack_id, sha)? {
        report.set_already_imported(Some(prior.imported_at), None);
        return Ok(report);
    }
    if importer::pack_id_reused(tx, header, sha)? {
        report.add_note(PACK_ID_REUSED);
    }
    let import_id = importer::begin_import(tx, header, sha, ctx.via, &ctx.now)?;
    // The date the reading position counts for (R-59): the export date,
    // but never later than tomorrow's study day (a typo like next year's
    // date would otherwise block every later position), and the study day
    // of the import when the pack has none.
    let today = ctx.moment.day.to_string();
    let latest = ctx.moment.day.add_days(1).to_string();
    let effective_date = match &header.exported_at {
        Some(date) if *date > latest => {
            report.add_warning(
                "/exported_at",
                format!(
                    "exported_at ({date}) is in the future; the reading position is dated today ({today}) instead."
                ),
            );
            today
        }
        Some(date) => date.clone(),
        None => today,
    };
    let reading = ReadingUpdate {
        position: header.reading_position.clone(),
        effective_date,
    };
    let source = catalog::upsert_source(tx, &header.source, &reading, &ctx.now)?;
    let not_applied = source.reading_position_newer.as_ref().map(|stored| {
        format!(
            "Not applied: the source already has a reading position dated {stored}, which is \
             newer than this pack's ({}).",
            reading.effective_date
        )
    });
    report.set_reading_position(source.reading_position_updated, not_applied);
    if let Some(title) = &source.default_title {
        report.add_warning(
            "/source/title",
            format!("Source title missing; using \"{title}\" until a pack brings one."),
        );
    }
    if source.title_replaced
        && let Some(title) = &header.source.title
    {
        report.add_note(format!("The source is now titled \"{title}\"."));
    }
    if let Some(stored) = &source.kept_type {
        report.add_warning(
            "/source/type",
            format!(
                "This source is stored with type \"{stored}\"; the pack's type \"{}\" is ignored.",
                header.source.kind
            ),
        );
    }
    let link = ImportLink {
        source_id: source.id,
        pack_import_id: import_id,
        now: &ctx.now,
    };
    // Pack entries that turn out to be one word in the app (a pack spelling
    // and the owner's new headword of the same word): both are merged, and
    // the report says so (nothing is lost, but the owner should know).
    let mut merged_into: HashMap<WordId, (usize, &str)> = HashMap::new();
    for word in &validation.words {
        if let WordResult::Valid(word) = word {
            let merge = catalog::merge_word(tx, &link, word)?;
            if let Some(id) = merge.word_id {
                match merged_into.get(&id) {
                    Some((index, earlier)) => report.add_warning(
                        &format!("/words/{}/headword", word.index),
                        format!(
                            "\"{earlier}\" (word {}) and \"{}\" (word {}) are the same word in the app; \
                             both were merged into it. Check the word for doubled values.",
                            index + 1,
                            word.headword,
                            word.index + 1,
                        ),
                    ),
                    None => {
                        merged_into.insert(id, (word.index, word.headword.as_str()));
                    }
                }
            }
            // Cards for the merged word, in the same transaction (design
            // §5.1 step 4); M6 adds `audio::request` here.
            if let Some(id) = merge.word_id
                && let Some(traits) = catalog::word_traits(tx, id)?
            {
                scheduler::ensure_cards(tx, &traits, &ctx.moment)?;
            }
            report.record_merge(word.index, merge, None);
        }
    }
    catalog::check_capacity(tx)??;
    report.set_status(PackStatus::Imported);
    importer::finish_import(tx, import_id, &report)?;
    let s = report.summary;
    tracing::info!(
        pack_id = %header.pack_id,
        new_words = s.new_words,
        merged_words = s.merged_words,
        unchanged_words = s.unchanged_words,
        invalid_words = s.invalid_words,
        new_sentences = s.new_sentences,
        updated_sentences = s.updated_sentences,
        conflicts = s.conflicts,
        "pack imported"
    );
    Ok(report)
}

/// The import log with the titles of the sources (read from the catalog).
pub async fn recent_imports(db: &Db, limit: u32) -> Result<Vec<ImportLogRow>, WorkflowError> {
    db.read(move |conn| {
        let mut rows = importer::recent(conn, limit)?;
        let slugs: Vec<String> = rows.iter().map(|r| r.source_slug.clone()).collect();
        let titles = catalog::source_titles(conn, &slugs)?;
        for row in &mut rows {
            row.source_title = titles.get(&row.source_slug).cloned();
        }
        Ok(rows)
    })
    .await
}

// ── the owner's word edits (design §7.1 screen 7; R-04, R-25, R-51) ────────
//
// Each runs inside the caller's transaction: the catalog change and the
// scheduler's follow-up (`ensure_cards`: a goal switched to `recognise`
// suspends the Produce card, enough visible sentences upgrade a word-only
// card, an include unlocks nothing new) are stored together or not at all.

/// What an owner action on a word does (design §8 `POST
/// /api/words/{id}/actions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordAction {
    Suspend,
    Unsuspend,
    /// Practise a low-priority word (R-51: recognise-only, lowest retention).
    Include,
    Exclude,
    /// Forget one card's learning state (its history stays).
    ResetCard(CardKind),
}

/// Bring the word's cards in line with its (changed) traits.
fn refresh_cards(tx: &Transaction<'_>, id: WordId, now: &Moment) -> Result<(), WorkflowError> {
    let traits = catalog::word_traits(tx, id)?.ok_or(EditError::WordNotFound)?;
    scheduler::ensure_cards(tx, &traits, now)?;
    Ok(())
}

/// Edit word fields (`PATCH /api/words/{id}`). Returns the fields whose
/// value changed.
pub fn edit_word(
    tx: &Transaction<'_>,
    id: WordId,
    edit: &serde_json::Value,
    now: &Moment,
) -> Result<Vec<&'static str>, WorkflowError> {
    let changed = catalog::update_word(tx, id, edit, &now.stamp())?;
    refresh_cards(tx, id, now)?;
    Ok(changed)
}

/// Suspend, include, reset a card (`POST /api/words/{id}/actions`).
pub fn word_action(
    tx: &Transaction<'_>,
    id: WordId,
    action: WordAction,
    now: &Moment,
) -> Result<(), WorkflowError> {
    let stamp = now.stamp();
    match action {
        WordAction::Suspend | WordAction::Unsuspend => {
            catalog::set_suspended(tx, id, action == WordAction::Suspend, &stamp)?;
        }
        WordAction::Include | WordAction::Exclude => {
            catalog::set_included(tx, id, action == WordAction::Include, &stamp)?;
        }
        WordAction::ResetCard(kind) => {
            if catalog::word_traits(tx, id)?.is_none() {
                return Err(EditError::WordNotFound.into());
            }
            if scheduler::reset_card(tx, id, kind, now)?.is_none() {
                return Err(WorkflowError::NoSuchCard(kind));
            }
        }
    }
    refresh_cards(tx, id, now)
}

/// Add an item to a list field (`POST /api/words/{id}/items`).
pub fn add_word_item(
    tx: &Transaction<'_>,
    id: WordId,
    kind: ListKind,
    value: &str,
    common: Option<bool>,
    now: &Moment,
) -> Result<(), WorkflowError> {
    catalog::add_item(tx, id, kind, value, common, &now.stamp())?;
    // Confusables drive the interference rule; nothing else changes cards.
    refresh_cards(tx, id, now)
}

/// Remove an item from a list field (`DELETE /api/words/{id}/items`).
pub fn remove_word_item(
    tx: &Transaction<'_>,
    id: WordId,
    kind: ListKind,
    key: &str,
    now: &Moment,
) -> Result<(), WorkflowError> {
    catalog::remove_item(tx, id, kind, key, &now.stamp())?;
    refresh_cards(tx, id, now)
}

/// Forget a pack spelling of a word (`DELETE /api/words/{id}/spellings`).
pub fn remove_pack_spelling(
    tx: &Transaction<'_>,
    id: WordId,
    key: &str,
    now: &Moment,
) -> Result<(), WorkflowError> {
    catalog::remove_pack_spelling(tx, id, key, &now.stamp())?;
    Ok(())
}

/// Correct a sentence (`PATCH /api/sentences/{id}`). Returns its word.
pub fn edit_sentence(
    tx: &Transaction<'_>,
    sentence: i64,
    en: &str,
    cs: &str,
    now: &Moment,
) -> Result<WordId, WorkflowError> {
    let word = catalog::update_sentence(tx, sentence, en, cs, &now.stamp())?;
    refresh_cards(tx, word, now)?;
    Ok(word)
}

/// Hide or show a sentence (`POST /api/sentences/{id}/hidden`). Returns its
/// word. Showing one may give the word enough sentences for the sentence
/// format (one-way upgrade, design §6.4); hiding never downgrades a card.
pub fn hide_sentence(
    tx: &Transaction<'_>,
    sentence: i64,
    hidden: bool,
    now: &Moment,
) -> Result<WordId, WorkflowError> {
    let word = catalog::set_sentence_hidden(tx, sentence, hidden, &now.stamp())?;
    refresh_cards(tx, word, now)?;
    Ok(word)
}

/// Answer a pack's suggestion (`POST /api/conflicts/{id}`). Returns the word.
pub fn resolve_conflict(
    tx: &Transaction<'_>,
    conflict: i64,
    use_incoming: bool,
    now: &Moment,
) -> Result<WordId, WorkflowError> {
    let word = catalog::resolve_conflict(tx, conflict, use_incoming, &now.stamp())?;
    refresh_cards(tx, word, now)?;
    Ok(word)
}

// ── Undo of an owner change (design §8 `POST /api/words/{id}/undo`) ──────
//
// The Undo restores the exact state before the change, server side: the
// catalog's rows of the word (override rows, `edited`/`hidden` flags,
// removed items, suggestions, pack spellings) and its cards (a card the
// change unlocked is deleted again, a suspended one comes back), instead of
// a client sending "the opposite change" that leaves markers behind.

/// The state of one word an Undo returns to.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct WordState {
    catalog: catalog::WordSnapshot,
    cards: scheduler::CardsSnapshot,
}

/// The word's state as text (`None`: no such word).
fn word_state(tx: &Transaction<'_>, id: WordId) -> Result<Option<String>, WorkflowError> {
    let Some(catalog) = catalog::word_snapshot(tx, id)? else {
        return Ok(None);
    };
    let state = WordState {
        catalog,
        cards: scheduler::cards_snapshot(tx, id)?,
    };
    serde_json::to_string(&state)
        .map(Some)
        .map_err(|e| WorkflowError::UndoState(e.to_string()))
}

/// Run an owner change of word `id` and store its Undo. Returns the
/// change's result and the Undo's token (`None` when nothing changed: an
/// earlier Undo of the word then stays valid).
pub fn with_undo<T>(
    tx: &Transaction<'_>,
    id: WordId,
    now: &Moment,
    change: impl FnOnce(&Transaction<'_>) -> Result<T, WorkflowError>,
) -> Result<(T, Option<UndoToken>), WorkflowError> {
    let before = word_state(tx, id)?;
    let value = change(tx)?;
    let token = match (before, word_state(tx, id)?) {
        (Some(before), Some(after)) if before != after => {
            Some(undo::record(tx, id, &before, &after, now)?)
        }
        _ => None,
    };
    Ok((value, token))
}

/// Undo the owner's latest change of word `id` (`token` from its answer):
/// refused when the token is gone or too old, or when the word changed
/// since in any way (another tab, an import, a practice rating).
pub fn undo_change(
    tx: &Transaction<'_>,
    id: WordId,
    token: &UndoToken,
    now: &Moment,
) -> Result<(), WorkflowError> {
    let stored = undo::take(tx, id, token, now)??;
    let current = word_state(tx, id)?.ok_or(UndoError::Gone)?;
    if current != stored.after {
        return Err(WorkflowError::ChangedSince);
    }
    let before: WordState = serde_json::from_str(&stored.before)
        .map_err(|e| WorkflowError::UndoState(e.to_string()))?;
    catalog::restore_word(tx, &before.catalog)?;
    scheduler::restore_cards(tx, id, &before.cards)?;
    Ok(())
}

/// The word of a sentence, for an Undo of a sentence change.
pub fn sentence_word(tx: &Transaction<'_>, sentence: i64) -> Result<WordId, WorkflowError> {
    Ok(catalog::word_of_sentence(tx, sentence)?.ok_or(EditError::SentenceNotFound)?)
}

/// The word of a pack suggestion, for an Undo of its answer.
pub fn conflict_word(tx: &Transaction<'_>, conflict: i64) -> Result<WordId, WorkflowError> {
    Ok(catalog::word_of_conflict(tx, conflict)?.ok_or(EditError::ConflictNotFound)?)
}

/// Delete a word: its cards and review history (scheduler), then everything
/// the catalog holds about it. Its practice items go with its cards.
pub fn delete_word(tx: &Transaction<'_>, id: WordId) -> Result<(), WorkflowError> {
    if catalog::word_traits(tx, id)?.is_none() {
        return Err(EditError::WordNotFound.into());
    }
    scheduler::delete_cards(tx, id)?;
    catalog::delete_word(tx, id)?;
    Ok(())
}

/// Make every stored word's cards match its traits (idempotent; design
/// §6.4). Run at startup: creates the Understand card of words imported
/// before cards existed. Returns how many cards were created.
pub async fn ensure_all_cards(db: &Db, now: Moment) -> Result<usize, WorkflowError> {
    db.write(move |tx| {
        let mut created = 0;
        for traits in catalog::all_traits(tx)? {
            created += scheduler::ensure_cards(tx, &traits, &now)?.len();
        }
        Ok::<_, WorkflowError>(created)
    })
    .await
}
