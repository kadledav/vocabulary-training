//! Time source and study-day arithmetic (design §2, §3.2).
//!
//! A *study day* is the local calendar day in the configured time zone, but it
//! starts at the rollover hour (default 04:00) instead of midnight, so a late
//! evening session still counts for the day it started. All "same day", "due
//! today" and "new per day" rules use [`StudyDay`]. The time zone database is
//! bundled into the binary (the runtime image has no `/usr/share/zoneinfo`).

use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;

use jiff::Span;
use jiff::civil::Date;
use jiff::tz::TimeZone;

/// An instant in time (UTC based).
pub type Timestamp = jiff::Timestamp;

/// How timestamps are stored and sent: RFC 3339 UTC with whole seconds,
/// e.g. `2026-11-17T10:00:00Z` (design §4).
pub fn format_timestamp(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The UTC calendar date of `ts` as `YYYY-MM-DD`.
pub fn utc_date(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%d").to_string()
}

/// Errors from building a [`DayRule`] or parsing a [`StudyDay`].
#[derive(Debug, thiserror::Error)]
pub enum ClockError {
    #[error("unknown time zone {name:?}")]
    UnknownTimeZone { name: String },
    #[error("day rollover hour must be 0-23, got {0}")]
    RolloverHour(u8),
    #[error("invalid study day {0:?} (expected YYYY-MM-DD)")]
    InvalidDay(String),
}

/// Source of the current time; injectable so tests can control it.
pub trait Clock: Send + Sync + fmt::Debug {
    fn now(&self) -> Timestamp;
}

/// The real wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that only moves when told to (tests).
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<Timestamp>,
}

impl ManualClock {
    pub fn new(start: Timestamp) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    pub fn set(&self, ts: Timestamp) {
        *self
            .now
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ts;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self
            .now
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Where study days begin: an IANA time zone and the local rollover hour.
#[derive(Debug, Clone)]
pub struct DayRule {
    tz: TimeZone,
    /// Local hour (0-23) at which a new study day starts.
    rollover_hour: i8,
}

impl DayRule {
    pub fn new(time_zone: &str, rollover_hour: u8) -> Result<Self, ClockError> {
        let hour = i8::try_from(rollover_hour)
            .ok()
            .filter(|h| (0..=23).contains(h))
            .ok_or(ClockError::RolloverHour(rollover_hour))?;
        let tz = TimeZone::get(time_zone).map_err(|_| ClockError::UnknownTimeZone {
            name: time_zone.to_owned(),
        })?;
        Ok(Self {
            tz,
            rollover_hour: hour,
        })
    }

    pub fn time_zone_name(&self) -> Option<&str> {
        self.tz.iana_name()
    }
}

/// A study day; stored as `YYYY-MM-DD` text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StudyDay(Date);

impl StudyDay {
    /// The study day that `ts` belongs to under `rule`.
    pub fn of(ts: Timestamp, rule: &DayRule) -> Self {
        let local = ts.to_zoned(rule.tz.clone()).datetime();
        let date = local.date();
        if local.hour() < rule.rollover_hour {
            // Before the rollover hour the previous day is still running.
            Self(date.saturating_sub(Span::new().days(1)))
        } else {
            Self(date)
        }
    }

    /// The day `days` later (earlier when negative); saturates at the ends
    /// of the supported calendar range.
    pub fn add_days(self, days: i32) -> Self {
        Self(self.0.saturating_add(Span::new().days(days)))
    }

    /// Whole days from `self` to `later` (negative when `later` is earlier).
    pub fn days_between(self, later: Self) -> i32 {
        (later.0 - self.0).get_days()
    }
}

/// One instant together with the study day it belongs to: what every
/// time-dependent write (reviews, sessions, card unlocks) needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub at: Timestamp,
    pub day: StudyDay,
}

impl Moment {
    pub fn of(at: Timestamp, rule: &DayRule) -> Self {
        Self {
            at,
            day: StudyDay::of(at, rule),
        }
    }

    /// `at` in the stored form (see [`format_timestamp`]).
    pub fn stamp(&self) -> String {
        format_timestamp(self.at)
    }
}

impl fmt::Display for StudyDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Stored as `YYYY-MM-DD` text (design §4).
impl rusqlite::types::ToSql for StudyDay {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.to_string()))
    }
}

impl rusqlite::types::FromSql for StudyDay {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        text.parse()
            .map_err(|err: ClockError| rusqlite::types::FromSqlError::Other(Box::new(err)))
    }
}

impl serde::Serialize for StudyDay {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl FromStr for StudyDay {
    type Err = ClockError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Only the canonical 10-character form; jiff alone would accept more.
        let canonical = s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-';
        match s.parse::<Date>() {
            Ok(date) if canonical => Ok(Self(date)),
            _ => Err(ClockError::InvalidDay(s.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prague() -> DayRule {
        DayRule::new("Europe/Prague", 4).expect("bundled tzdb knows Europe/Prague")
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().expect("valid timestamp")
    }

    fn day(s: &str) -> StudyDay {
        s.parse().expect("valid day")
    }

    #[test]
    fn r54_study_day_rolls_over_at_four_local_time() {
        let rule = prague();
        // 2026-11-17 03:59 CET = 02:59Z -> still the 16th.
        assert_eq!(
            StudyDay::of(ts("2026-11-17T02:59:00Z"), &rule),
            day("2026-11-16")
        );
        // 04:00 CET = 03:00Z -> the 17th.
        assert_eq!(
            StudyDay::of(ts("2026-11-17T03:00:00Z"), &rule),
            day("2026-11-17")
        );
        // 23:30 CET on the 17th -> the 17th.
        assert_eq!(
            StudyDay::of(ts("2026-11-17T22:30:00Z"), &rule),
            day("2026-11-17")
        );
    }

    #[test]
    fn r54_study_day_follows_daylight_saving_time() {
        let rule = prague();
        // Summer (CEST, UTC+2): 04:00 local = 02:00Z.
        assert_eq!(
            StudyDay::of(ts("2026-07-01T01:59:00Z"), &rule),
            day("2026-06-30")
        );
        assert_eq!(
            StudyDay::of(ts("2026-07-01T02:00:00Z"), &rule),
            day("2026-07-01")
        );
    }

    #[test]
    fn r54_rollover_at_midnight_is_the_calendar_day() {
        let rule = DayRule::new("UTC", 0).expect("UTC");
        assert_eq!(
            StudyDay::of(ts("2026-01-01T00:00:00Z"), &rule),
            day("2026-01-01")
        );
        assert_eq!(
            StudyDay::of(ts("2025-12-31T23:59:59Z"), &rule),
            day("2025-12-31")
        );
    }

    #[test]
    fn r54_rollover_on_the_spring_daylight_saving_change() {
        // 2026-03-29: clocks jump 02:00 CET -> 03:00 CEST. 04:00 CEST = 02:00Z.
        let rule = prague();
        assert_eq!(
            StudyDay::of(ts("2026-03-29T01:59:59Z"), &rule),
            day("2026-03-28")
        );
        assert_eq!(
            StudyDay::of(ts("2026-03-29T02:00:00Z"), &rule),
            day("2026-03-29")
        );
        // The previous evening is still CET: 04:00 CET = 03:00Z.
        assert_eq!(
            StudyDay::of(ts("2026-03-28T02:59:59Z"), &rule),
            day("2026-03-27")
        );
    }

    #[test]
    fn r54_rollover_on_the_autumn_daylight_saving_change() {
        // 2026-10-25: clocks fall back 03:00 CEST -> 02:00 CET. 04:00 CET = 03:00Z.
        let rule = prague();
        assert_eq!(
            StudyDay::of(ts("2026-10-25T02:59:59Z"), &rule),
            day("2026-10-24")
        );
        assert_eq!(
            StudyDay::of(ts("2026-10-25T03:00:00Z"), &rule),
            day("2026-10-25")
        );
        // Both 02:xx local hours (CEST and then CET) are still the 24th.
        assert_eq!(
            StudyDay::of(ts("2026-10-25T00:30:00Z"), &rule),
            day("2026-10-24")
        );
        assert_eq!(
            StudyDay::of(ts("2026-10-25T01:30:00Z"), &rule),
            day("2026-10-24")
        );
    }

    #[test]
    fn r54_moment_carries_its_study_day() {
        let moment = Moment::of(ts("2026-11-17T02:30:00Z"), &prague());
        assert_eq!(moment.day, day("2026-11-16"));
        assert_eq!(moment.stamp(), "2026-11-17T02:30:00Z");
    }

    #[test]
    fn r54_day_arithmetic() {
        let d = day("2026-02-27");
        assert_eq!(d.add_days(2), day("2026-03-01"));
        assert_eq!(d.add_days(-58), day("2025-12-31"));
        assert_eq!(d.days_between(day("2026-03-01")), 2);
        assert_eq!(day("2026-03-01").days_between(d), -2);
        assert_eq!(d.to_string(), "2026-02-27");
    }

    #[test]
    fn r54_invalid_inputs_are_rejected() {
        assert!(DayRule::new("Mars/Olympus_Mons", 4).is_err());
        assert!(DayRule::new("Europe/Prague", 24).is_err());
        assert!("2026-2-3".parse::<StudyDay>().is_err());
        assert!("2026-02-30".parse::<StudyDay>().is_err());
        assert!("20260203".parse::<StudyDay>().is_err());
    }

    #[test]
    fn r11_import_timestamps_are_rfc3339_utc_seconds() {
        let t = ts("2026-11-17T23:30:15.123456Z");
        assert_eq!(format_timestamp(t), "2026-11-17T23:30:15Z");
        assert_eq!(utc_date(t), "2026-11-17");
    }

    #[test]
    fn r54_manual_clock_is_controllable() {
        let clock = ManualClock::new(ts("2026-11-17T10:00:00Z"));
        assert_eq!(clock.now(), ts("2026-11-17T10:00:00Z"));
        clock.set(ts("2026-11-18T10:00:00Z"));
        assert_eq!(clock.now(), ts("2026-11-18T10:00:00Z"));
    }
}
