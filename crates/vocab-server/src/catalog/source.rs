//! The `source` table: creation, filling empty fields, reading position
//! (design §5.4 "Source record" and "Reading position", R-02, R-59).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use vocab_pack::types::{PackSource, SourceType};

use super::SourceId;

/// Title of a new `general` source that arrives without one.
const GENERAL_TITLE: &str = "General Vocabulary";

/// The reading position a pack brings, with the date that decides whether
/// it is newer than the stored one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingUpdate {
    /// The pack's `reading_position` (`None`: the pack says nothing).
    pub position: Option<String>,
    /// The date the position counts for (`YYYY-MM-DD`): the pack's
    /// `exported_at`, or the study day of the import when it has none or
    /// lies in the future (decided by the caller).
    pub effective_date: String,
}

/// What [`upsert_source`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceUpsert {
    pub id: SourceId,
    pub created: bool,
    pub reading_position_updated: bool,
    /// The pack's reading position was not applied because the stored one
    /// counts for this later date.
    pub reading_position_newer: Option<String>,
    /// A new source had no title; this one was used.
    pub default_title: Option<String>,
    /// The stand-in title of the source was replaced by the pack's title.
    pub title_replaced: bool,
    /// The pack gave another `type` than the stored one, which was kept.
    pub kept_type: Option<String>,
}

struct Existing {
    id: SourceId,
    kind: String,
    title: String,
    /// The title is a stand-in chosen by the app (R-02).
    title_defaulted: bool,
    author: Option<String>,
    series: Option<String>,
    series_number: Option<i64>,
    url: Option<String>,
    reading_position: Option<String>,
    reading_position_exported_at: Option<String>,
}

fn find(conn: &Connection, slug: &str) -> rusqlite::Result<Option<Existing>> {
    conn.query_row(
        "SELECT id, type, title, author, series, series_number, url, reading_position, \
         reading_position_exported_at, title_defaulted FROM source WHERE slug = ?1",
        [slug],
        |row| {
            Ok(Existing {
                id: SourceId(row.get(0)?),
                kind: row.get(1)?,
                title: row.get(2)?,
                author: row.get(3)?,
                series: row.get(4)?,
                series_number: row.get(5)?,
                url: row.get(6)?,
                reading_position: row.get(7)?,
                reading_position_exported_at: row.get(8)?,
                title_defaulted: row.get(9)?,
            })
        },
    )
    .optional()
}

/// The stored position is replaced when the pack's date is the same or
/// later (ties: the later import wins).
fn reading_wins(existing: Option<&Existing>, reading: &ReadingUpdate) -> bool {
    reading.position.is_some()
        && existing
            .and_then(|e| e.reading_position_exported_at.as_deref())
            .is_none_or(|stored| reading.effective_date.as_str() >= stored)
}

/// Create the source, or fill its empty fields (non-empty ones are never
/// overwritten; the type never changes), and apply the reading position.
/// A new source without a title is named "General Vocabulary" (type
/// `general`) or after its id; that stand-in is remembered as such and the
/// first later pack with a title replaces it (R-02).
pub fn upsert_source(
    tx: &Transaction<'_>,
    source: &PackSource,
    reading: &ReadingUpdate,
    now: &str,
) -> rusqlite::Result<SourceUpsert> {
    let existing = find(tx, &source.id)?;
    let reading_updated = reading_wins(existing.as_ref(), reading);
    let reading_position_newer = (reading.position.is_some() && !reading_updated)
        .then(|| {
            existing
                .as_ref()
                .and_then(|e| e.reading_position_exported_at.clone())
        })
        .flatten();
    let (position, position_date) = if reading_updated {
        (
            reading.position.clone(),
            Some(reading.effective_date.clone()),
        )
    } else {
        (None, None)
    };
    let series_number = source.series_number.map(i64::from);
    let Some(existing) = existing else {
        let (title, default_title) = match &source.title {
            Some(title) => (title.clone(), None),
            None => {
                let title = if source.kind == SourceType::General {
                    GENERAL_TITLE.to_owned()
                } else {
                    source.id.clone()
                };
                (title.clone(), Some(title))
            }
        };
        tx.execute(
            "INSERT INTO source (slug, type, title, author, series, series_number, url, \
             reading_position, reading_position_exported_at, created_at, updated_at, \
             title_defaulted) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
            params![
                source.id,
                source.kind.as_str(),
                title,
                source.author,
                source.series,
                series_number,
                source.url,
                position,
                position_date,
                now,
                default_title.is_some(),
            ],
        )?;
        return Ok(SourceUpsert {
            id: SourceId(tx.last_insert_rowid()),
            created: true,
            reading_position_updated: reading_updated,
            reading_position_newer,
            default_title,
            title_replaced: false,
            kept_type: None,
        });
    };

    fn fill<T: Clone>(current: Option<T>, incoming: Option<&T>) -> Option<T> {
        current.or_else(|| incoming.cloned())
    }
    let blank_to_none = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
    let (title, title_defaulted) = match &source.title {
        Some(incoming) if existing.title_defaulted || existing.title.trim().is_empty() => {
            (incoming.clone(), false)
        }
        _ => (existing.title.clone(), existing.title_defaulted),
    };
    let title_replaced = existing.title_defaulted && !title_defaulted;
    let author = fill(
        blank_to_none(existing.author.clone()),
        source.author.as_ref(),
    );
    let series = fill(
        blank_to_none(existing.series.clone()),
        source.series.as_ref(),
    );
    let series_number = fill(existing.series_number, series_number.as_ref());
    let url = fill(blank_to_none(existing.url.clone()), source.url.as_ref());
    let (position, position_date) = if reading_updated {
        (position, position_date)
    } else {
        (
            existing.reading_position.clone(),
            existing.reading_position_exported_at.clone(),
        )
    };
    let changed = title != existing.title
        || title_defaulted != existing.title_defaulted
        || author != existing.author
        || series != existing.series
        || series_number != existing.series_number
        || url != existing.url
        || position != existing.reading_position
        || position_date != existing.reading_position_exported_at;
    if changed {
        tx.execute(
            "UPDATE source SET title = ?2, author = ?3, series = ?4, series_number = ?5, url = ?6, \
             reading_position = ?7, reading_position_exported_at = ?8, updated_at = ?9, \
             title_defaulted = ?10 WHERE id = ?1",
            params![
                existing.id.0,
                title,
                author,
                series,
                series_number,
                url,
                position,
                position_date,
                now,
                title_defaulted,
            ],
        )?;
    }
    let kept_type = (existing.kind != source.kind.as_str()).then(|| existing.kind.clone());
    Ok(SourceUpsert {
        id: existing.id,
        created: false,
        reading_position_updated: reading_updated,
        reading_position_newer,
        default_title: None,
        title_replaced,
        kept_type,
    })
}

/// Titles of the sources with these slugs (unknown slugs are left out).
pub fn source_titles(
    conn: &Connection,
    slugs: &[String],
) -> rusqlite::Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT title FROM source WHERE slug = ?1")?;
    let mut out = HashMap::new();
    for slug in slugs {
        if out.contains_key(slug) {
            continue;
        }
        if let Some(title) = stmt
            .query_row([slug], |row| row.get::<_, String>(0))
            .optional()?
        {
            out.insert(slug.clone(), title);
        }
    }
    Ok(out)
}
