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

use rusqlite::Transaction;
use vocab_pack::paste::PasteError;
use vocab_pack::types::WordResult;

use crate::catalog::{self, ImportLink, ReadingUpdate};
use crate::clock::{Timestamp, format_timestamp, utc_date};
use crate::db::{Db, DbError};
use crate::importer::{
    self, ImportLogRow, ImportReport, Mode, PACK_ID_REUSED, PackReport, PackStatus, PreparedPack,
    Via,
};

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
}

/// Time-dependent values shared by every pack of one import.
#[derive(Debug, Clone)]
struct ImportContext {
    via: Via,
    /// RFC 3339 timestamp of the import.
    now: String,
    /// Import date; stands in for a missing `exported_at`.
    today: String,
}

impl ImportContext {
    fn new(via: Via, now: Timestamp) -> Self {
        Self {
            via,
            now: format_timestamp(now),
            today: utc_date(now),
        }
    }
}

/// What committing `text` would do: the commit's own per-pack code, run in
/// one transaction that is always rolled back. Nothing is stored.
pub async fn preview_import(
    db: &Db,
    text: &str,
    now: Timestamp,
) -> Result<ImportReport, WorkflowError> {
    let packs = importer::prepare(text)?;
    let ctx = ImportContext::new(Via::Paste, now);
    let reports = db
        .dry_run(move |tx| {
            let mut reports: Vec<PackReport> = packs.iter().map(PackReport::new).collect();
            for index in importer::commit_order(&packs) {
                let mut report = import_pack(tx, &packs[index], reports[index].clone(), &ctx)?;
                if report.status == PackStatus::Imported {
                    report.set_status(PackStatus::Ready);
                }
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
    now: Timestamp,
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
        reports[index] = db
            .write(move |tx| import_pack(tx, &pack, report, &ctx))
            .await?;
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
    let reading = ReadingUpdate {
        position: header.reading_position.clone(),
        effective_date: header
            .exported_at
            .clone()
            .unwrap_or_else(|| ctx.today.clone()),
    };
    let source = catalog::upsert_source(tx, &header.source, &reading, &ctx.now)?;
    report.set_reading_position_updated(source.reading_position_updated);
    if let Some(title) = &source.default_title {
        report.add_warning(
            "/source/title",
            format!("Source title missing; using \"{title}\"."),
        );
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
    for word in &validation.words {
        if let WordResult::Valid(word) = word {
            let merge = catalog::merge_word(tx, &link, word)?;
            // M3 adds `scheduler::ensure_cards` and M6 `audio::request` for
            // the merged word here, in the same transaction (design §5.1 step 4).
            report.record_merge(word.index, merge, None);
        }
    }
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
