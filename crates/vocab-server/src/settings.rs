//! Owner preferences (design §3.2 `settings`, §7.1 Settings screen).
//!
//! Owned table: `setting` (`key` → JSON value). Unknown or invalid stored
//! values fall back to the default, so an older or newer row never breaks
//! the app. Besides the preferences the table holds one day-scoped flag:
//! "add new words anyway today" (design §6.6), which overrides the backlog
//! pause for one study day.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clock::StudyDay;

/// Range of `new_per_day` (R-54, design §6.6).
pub const NEW_PER_DAY_MAX: u32 = 100;
/// Range of `block_size`.
pub const BLOCK_SIZE_MIN: u32 = 5;
pub const BLOCK_SIZE_MAX: u32 = 50;

const KEY_NEW_ANYWAY: &str = "new_words_anyway_day";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Voice {
    Us,
    Uk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Dark,
    Light,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Legend {
    Auto,
    Always,
    Off,
}

/// Playback speed of audio clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Speed {
    #[serde(rename = "1")]
    Normal,
    #[serde(rename = "0.75")]
    Slow,
}

/// All preferences with their effective values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Settings {
    /// New Understand cards introduced per study day (0–100, default 5).
    pub new_per_day: u32,
    /// Items per practice block (default 20).
    pub block_size: u32,
    pub voice: Voice,
    /// Play the word once when the answer is revealed (off by default: the
    /// first sound of a visit must come from a tap anyway, R-41).
    pub autoplay: bool,
    pub speed: Speed,
    pub theme: Theme,
    pub legend: Legend,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            new_per_day: 5,
            block_size: 20,
            voice: Voice::Us,
            autoplay: false,
            speed: Speed::Normal,
            theme: Theme::Dark,
            legend: Legend::Auto,
        }
    }
}

/// A change to some preferences; absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    pub new_per_day: Option<u32>,
    pub block_size: Option<u32>,
    pub voice: Option<Voice>,
    pub autoplay: Option<bool>,
    pub speed: Option<Speed>,
    pub theme: Option<Theme>,
    pub legend: Option<Legend>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl SettingsPatch {
    fn validate(&self) -> Result<(), SettingsError> {
        if self.new_per_day.is_some_and(|n| n > NEW_PER_DAY_MAX) {
            return Err(SettingsError::Invalid(format!(
                "new_per_day must be 0-{NEW_PER_DAY_MAX}"
            )));
        }
        if self
            .block_size
            .is_some_and(|n| !(BLOCK_SIZE_MIN..=BLOCK_SIZE_MAX).contains(&n))
        {
            return Err(SettingsError::Invalid(format!(
                "block_size must be {BLOCK_SIZE_MIN}-{BLOCK_SIZE_MAX}"
            )));
        }
        Ok(())
    }

    fn entries(&self) -> Vec<(&'static str, Value)> {
        let mut out = Vec::new();
        let mut put = |key: &'static str, value: Option<Value>| {
            if let Some(value) = value {
                out.push((key, value));
            }
        };
        put("new_per_day", self.new_per_day.map(Value::from));
        put("block_size", self.block_size.map(Value::from));
        put(
            "voice",
            self.voice.and_then(|v| serde_json::to_value(v).ok()),
        );
        put("autoplay", self.autoplay.map(Value::from));
        put(
            "speed",
            self.speed.and_then(|v| serde_json::to_value(v).ok()),
        );
        put(
            "theme",
            self.theme.and_then(|v| serde_json::to_value(v).ok()),
        );
        put(
            "legend",
            self.legend.and_then(|v| serde_json::to_value(v).ok()),
        );
        out
    }
}

fn stored(conn: &Connection, key: &str) -> rusqlite::Result<Option<Value>> {
    let text: Option<String> = conn
        .query_row(
            "SELECT value_json FROM setting WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
}

fn stored_as<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    key: &str,
) -> rusqlite::Result<Option<T>> {
    Ok(stored(conn, key)?.and_then(|v| serde_json::from_value(v).ok()))
}

fn put(tx: &Transaction<'_>, key: &str, value: &Value) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO setting (key, value_json) VALUES (?1, ?2) \
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
        params![key, value.to_string()],
    )?;
    Ok(())
}

/// The effective preferences (defaults for anything unset or invalid).
pub fn get(conn: &Connection) -> rusqlite::Result<Settings> {
    let d = Settings::default();
    Ok(Settings {
        new_per_day: stored_as::<u32>(conn, "new_per_day")?
            .filter(|n| *n <= NEW_PER_DAY_MAX)
            .unwrap_or(d.new_per_day),
        block_size: stored_as::<u32>(conn, "block_size")?
            .filter(|n| (BLOCK_SIZE_MIN..=BLOCK_SIZE_MAX).contains(n))
            .unwrap_or(d.block_size),
        voice: stored_as(conn, "voice")?.unwrap_or(d.voice),
        autoplay: stored_as(conn, "autoplay")?.unwrap_or(d.autoplay),
        speed: stored_as(conn, "speed")?.unwrap_or(d.speed),
        theme: stored_as(conn, "theme")?.unwrap_or(d.theme),
        legend: stored_as(conn, "legend")?.unwrap_or(d.legend),
    })
}

/// Apply `patch` (validated as a whole first) and return the result.
pub fn update(tx: &Transaction<'_>, patch: &SettingsPatch) -> Result<Settings, SettingsError> {
    patch.validate()?;
    for (key, value) in patch.entries() {
        put(tx, key, &value)?;
    }
    Ok(get(tx)?)
}

/// The owner chose "Add new words anyway" on `day` (design §6.6).
pub fn new_words_anyway_on(conn: &Connection, day: StudyDay) -> rusqlite::Result<bool> {
    Ok(stored_as::<String>(conn, KEY_NEW_ANYWAY)?.is_some_and(|d| d == day.to_string()))
}

/// Override the backlog pause for the rest of study day `day`.
pub fn allow_new_words_anyway(tx: &Transaction<'_>, day: StudyDay) -> rusqlite::Result<()> {
    put(tx, KEY_NEW_ANYWAY, &Value::from(day.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(dir.path()).expect("db");
        (dir, db)
    }

    async fn update_with(db: &Db, patch: SettingsPatch) -> Result<Settings, SettingsError> {
        db.write(move |tx| Ok::<_, crate::db::DbError>(update(tx, &patch)))
            .await
            .expect("db")
    }

    #[tokio::test]
    async fn r54_defaults_are_five_new_words_and_blocks_of_twenty() {
        let (_dir, db) = db();
        let s = db
            .read(|c| get(c).map_err(crate::db::DbError::from))
            .await
            .expect("get");
        assert_eq!(s, Settings::default());
        assert_eq!(s.new_per_day, 5);
        assert_eq!(s.block_size, 20);
    }

    #[tokio::test]
    async fn r54_patch_changes_only_given_fields_and_validates() {
        let (_dir, db) = db();
        let s = update_with(
            &db,
            SettingsPatch {
                new_per_day: Some(8),
                theme: Some(Theme::Light),
                ..SettingsPatch::default()
            },
        )
        .await
        .expect("valid");
        assert_eq!(s.new_per_day, 8);
        assert_eq!(s.theme, Theme::Light);
        assert_eq!(s.block_size, 20);
        let err = update_with(
            &db,
            SettingsPatch {
                new_per_day: Some(101),
                block_size: Some(30),
                ..SettingsPatch::default()
            },
        )
        .await;
        assert!(matches!(err, Err(SettingsError::Invalid(_))));
        let err = update_with(
            &db,
            SettingsPatch {
                block_size: Some(4),
                ..SettingsPatch::default()
            },
        )
        .await;
        assert!(matches!(err, Err(SettingsError::Invalid(_))));
        let s = db
            .read(|c| get(c).map_err(crate::db::DbError::from))
            .await
            .expect("get");
        // The rejected patches changed nothing (also not block_size 30).
        assert_eq!((s.new_per_day, s.block_size), (8, 20));
    }

    #[tokio::test]
    async fn r54_new_words_anyway_holds_for_one_study_day() {
        let (_dir, db) = db();
        let day: StudyDay = "2026-11-20".parse().expect("day");
        db.write(move |tx| allow_new_words_anyway(tx, day).map_err(crate::db::DbError::from))
            .await
            .expect("write");
        let (today, tomorrow) = db
            .read(move |c| {
                Ok::<_, crate::db::DbError>((
                    new_words_anyway_on(c, day)?,
                    new_words_anyway_on(c, day.add_days(1))?,
                ))
            })
            .await
            .expect("read");
        assert!(today);
        assert!(!tomorrow);
    }

    #[test]
    fn r54_patch_json_rejects_unknown_fields_and_bad_values() {
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"new_per_day": 3}"#).is_ok());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"speed": "0.75"}"#).is_ok());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"nope": 1}"#).is_err());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"theme": "pink"}"#).is_err());
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"new_per_day": -1}"#).is_err());
    }
}
