//! Global login throttle (design §9.4, decision D-8) and its stored state.
//!
//! Single user, so the throttle is global rather than per client address
//! (`X-Forwarded-For` can be forged by any container on the shared Docker
//! network). Two rules, both about failed **logins** only:
//!
//! * After [`MAX_FAILURES`] failed logins within [`FAILURE_WINDOW`], every
//!   login attempt is refused for [`FIRST_LOCKOUT`]; each further lockout
//!   doubles that, up to [`MAX_LOCKOUT`]. The doubling starts over after
//!   [`DECAY`] without any failed login (an attempt refused during a
//!   lockout counts as one).
//! * More than [`MAX_DAY_FAILURES`] failed logins within [`DAY_WINDOW`]
//!   lock logins for [`DAY_LOCKOUT`], whatever their spacing: slow guessing
//!   just under the first rule gets nowhere either.
//!
//! A successful login resets everything. Sessions that already exist are
//! not affected, and neither are the password checks a logged-in session
//! makes (they have their own per-session counter) nor setup-code attempts.
//!
//! The state is kept in the one row of `auth_throttle` (migration 0005),
//! which only this module touches, so a restart does not hand out a fresh
//! set of guesses. It is written when a login fails or succeeds, and for an
//! attempt refused during a lockout at most once per
//! [`REFUSED_STORE_INTERVAL`] (so the decay survives a restart without a
//! write per refused attempt).
//!
//! Clock errors: before every use the state is checked against the current
//! time ([`LoginThrottle::check`], [`LoginThrottle::record_failure`]): a
//! lock never ends later than [`MAX_LOCKOUT`] from now, and failures dated
//! in the future (the clock was ahead, then corrected) are dropped, so a
//! wrong clock cannot lock the owner out for longer than a day.

use std::collections::VecDeque;

use jiff::SignedDuration;
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

use crate::clock::{Timestamp, format_timestamp};

pub const MAX_FAILURES: usize = 5;
pub const FAILURE_WINDOW: SignedDuration = SignedDuration::from_mins(15);
pub const FIRST_LOCKOUT: SignedDuration = SignedDuration::from_mins(15);
pub const MAX_LOCKOUT: SignedDuration = SignedDuration::from_hours(24);
pub const DECAY: SignedDuration = SignedDuration::from_hours(24);
pub const MAX_DAY_FAILURES: usize = 20;
pub const DAY_WINDOW: SignedDuration = SignedDuration::from_hours(24);
pub const DAY_LOCKOUT: SignedDuration = SignedDuration::from_hours(24);
/// A refused attempt is stored when the stored latest failure is at least
/// this old.
pub const REFUSED_STORE_INTERVAL: SignedDuration = SignedDuration::from_mins(1);

/// Throttle state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LoginThrottle {
    /// Times of the recent failures (within [`FAILURE_WINDOW`]), oldest first.
    failures: VecDeque<Timestamp>,
    /// Times of the failures within [`DAY_WINDOW`], oldest first.
    day_failures: VecDeque<Timestamp>,
    /// Logins are refused until then.
    locked_until: Option<Timestamp>,
    /// Lockouts since the last successful login (or the last quiet day).
    lockouts: u32,
    /// The latest failed or refused login.
    last_failure: Option<Timestamp>,
}

fn prune(times: &mut VecDeque<Timestamp>, now: Timestamp, window: SignedDuration) {
    while times
        .front()
        .is_some_and(|first| now.duration_since(*first) >= window)
    {
        times.pop_front();
    }
}

impl LoginThrottle {
    /// `Err(retry_after)` while logins are locked (the refused attempt
    /// counts as a failed login for the decay).
    ///
    /// The second value of the error says whether the state should be
    /// stored (see [`REFUSED_STORE_INTERVAL`]).
    pub fn check(&mut self, now: Timestamp) -> Result<(), (SignedDuration, bool)> {
        self.sanitize(now);
        match self.locked_until {
            Some(until) if until > now => {
                let store = self
                    .last_failure
                    .is_none_or(|last| now.duration_since(last) >= REFUSED_STORE_INTERVAL);
                self.last_failure = Some(now);
                Err((until.duration_since(now), store))
            }
            _ => Ok(()),
        }
    }

    /// Make the state consistent with `now` (clock errors): the lock ends
    /// at most [`MAX_LOCKOUT`] from now; failure times later than now are
    /// dropped (the latest failure is dated now at the latest).
    fn sanitize(&mut self, now: Timestamp) {
        let latest_end = now.saturating_add(MAX_LOCKOUT).unwrap_or(now);
        if let Some(until) = self.locked_until
            && until > latest_end
        {
            self.locked_until = Some(latest_end);
        }
        self.failures.retain(|t| *t <= now);
        self.day_failures.retain(|t| *t <= now);
        if self.last_failure.is_some_and(|t| t > now) {
            self.last_failure = Some(now);
        }
    }

    /// Record a failed login; returns the lockout it started, if any.
    pub fn record_failure(&mut self, now: Timestamp) -> Option<SignedDuration> {
        self.sanitize(now);
        if self
            .last_failure
            .is_some_and(|last| now.duration_since(last) >= DECAY)
        {
            self.lockouts = 0;
        }
        self.last_failure = Some(now);
        prune(&mut self.failures, now, FAILURE_WINDOW);
        prune(&mut self.day_failures, now, DAY_WINDOW);
        self.failures.push_back(now);
        self.day_failures.push_back(now);
        let lockout = if self.day_failures.len() > MAX_DAY_FAILURES {
            self.day_failures.clear();
            DAY_LOCKOUT
        } else if self.failures.len() >= MAX_FAILURES {
            let doublings = i32::try_from(self.lockouts).unwrap_or(i32::MAX).min(16);
            self.lockouts = self.lockouts.saturating_add(1);
            FIRST_LOCKOUT
                .checked_mul(1 << doublings)
                .map_or(MAX_LOCKOUT, |d| d.min(MAX_LOCKOUT))
        } else {
            return None;
        };
        self.failures.clear();
        self.locked_until = Some(now.saturating_add(lockout).unwrap_or(now));
        Some(lockout)
    }

    /// A successful login resets the throttle. Returns whether there was
    /// anything to reset (and so to store).
    pub fn record_success(&mut self) -> bool {
        let changed = *self != Self::default();
        *self = Self::default();
        changed
    }

    /// The state as stored in `auth_throttle.state_json`.
    pub fn to_json(&self) -> String {
        let times =
            |times: &VecDeque<Timestamp>| times.iter().copied().map(format_timestamp).collect();
        let stored = Stored {
            failures: times(&self.failures),
            day_failures: times(&self.day_failures),
            locked_until: self.locked_until.map(format_timestamp),
            lockouts: self.lockouts,
            last_failure: self.last_failure.map(format_timestamp),
        };
        serde_json::to_string(&stored).unwrap_or_else(|_| "{}".to_owned())
    }

    /// From the stored state. Anything unreadable counts as "no failures":
    /// the throttle must never keep the owner out because of a bad row.
    pub fn from_json(json: &str) -> Self {
        let Ok(stored) = serde_json::from_str::<Stored>(json) else {
            return Self::default();
        };
        let times = |texts: &[String]| -> VecDeque<Timestamp> {
            let mut times: Vec<Timestamp> = texts.iter().filter_map(|t| t.parse().ok()).collect();
            times.sort();
            times.into()
        };
        Self {
            failures: times(&stored.failures),
            day_failures: times(&stored.day_failures),
            locked_until: stored.locked_until.and_then(|t| t.parse().ok()),
            lockouts: stored.lockouts,
            last_failure: stored.last_failure.and_then(|t| t.parse().ok()),
        }
    }
}

/// JSON form of the state (timestamps as stored everywhere else).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    failures: Vec<String>,
    day_failures: Vec<String>,
    locked_until: Option<String>,
    lockouts: u32,
    last_failure: Option<String>,
}

// ── the auth_throttle table ─────────────────────────────────────────────────

/// The stored throttle state.
pub fn load(conn: &Connection) -> rusqlite::Result<LoginThrottle> {
    let json: String = conn.query_row(
        "SELECT state_json FROM auth_throttle WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(LoginThrottle::from_json(&json))
}

pub fn store(tx: &Transaction<'_>, throttle: &LoginThrottle) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE auth_throttle SET state_json = ?1 WHERE id = 1",
        [throttle.to_json()],
    )?;
    Ok(())
}

/// Forget every failed login (a successful login, `reset-password`).
pub fn clear(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE auth_throttle SET state_json = '{}' WHERE id = 1",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: i64) -> Timestamp {
        let start: Timestamp = "2026-11-20T09:00:00Z".parse().expect("ts");
        start
            .saturating_add(SignedDuration::from_mins(minutes))
            .expect("in range")
    }

    #[test]
    fn r82_five_failures_within_15_minutes_lock_for_15_minutes() {
        let mut t = LoginThrottle::default();
        for i in 0..4 {
            assert_eq!(t.record_failure(at(i)), None);
            assert!(t.check(at(i)).is_ok());
        }
        assert_eq!(t.record_failure(at(4)), Some(SignedDuration::from_mins(15)));
        assert_eq!(
            t.check(at(4)).map_err(|e| e.0),
            Err(SignedDuration::from_mins(15))
        );
        assert_eq!(
            t.check(at(18)).map_err(|e| e.0),
            Err(SignedDuration::from_mins(1))
        );
        assert!(t.check(at(19)).is_ok());
    }

    #[test]
    fn r82_failures_older_than_the_window_do_not_count() {
        let mut t = LoginThrottle::default();
        for i in 0..4 {
            t.record_failure(at(i));
        }
        // 15 minutes after the first failure it has left the window.
        assert_eq!(t.record_failure(at(15)), None);
        assert!(t.check(at(15)).is_ok());
        // Failures at 1, 2, 3, 15 and now 15 again: five within the window.
        assert!(t.record_failure(at(15)).is_some());
    }

    /// Five failures at `now`; the lockout they start, in minutes.
    fn lock(t: &mut LoginThrottle, now: i64) -> i64 {
        let mut lockout = None;
        for _ in 0..MAX_FAILURES {
            lockout = t.record_failure(at(now));
        }
        lockout.expect("locked").as_mins()
    }

    #[test]
    fn r82_lockouts_double_up_to_24_hours_and_success_resets() {
        let mut t = LoginThrottle::default();
        let mut now = 0;
        let mut lockouts = Vec::new();
        for _ in 0..9 {
            let lockout = lock(&mut t, now);
            lockouts.push(lockout);
            // Someone keeps trying until the end: no quiet day, no decay.
            assert!(t.check(at(now + lockout - 1)).is_err());
            // The rounds are at least six hours apart, so never more than
            // 20 failures fall into 24 hours (the other rule).
            now += lockout.max(6 * 60);
            assert!(t.check(at(now - 1)).is_ok() || lockout >= 6 * 60);
            assert!(t.check(at(now)).is_ok());
        }
        assert_eq!(lockouts, [15, 30, 60, 120, 240, 480, 960, 1440, 1440]);
        assert!(t.record_success());
        assert_eq!(lock(&mut t, now), 15);
        let mut clear = LoginThrottle::default();
        assert!(
            !clear.record_success(),
            "nothing to reset, nothing to store"
        );
    }

    #[test]
    fn r82_the_doubling_starts_over_after_24_quiet_hours() {
        let mut t = LoginThrottle::default();
        assert_eq!(lock(&mut t, 0), 15);
        assert_eq!(lock(&mut t, 15), 30);
        // Quiet for just under a day: the level stays.
        assert_eq!(lock(&mut t, 15 + 1439), 60);
        // Quiet for a whole day: back to 15 minutes.
        assert_eq!(lock(&mut t, 15 + 1439 + 1440), 15);
    }

    /// B3(b): the long-window rule.
    #[test]
    fn r82_more_than_twenty_failures_in_24_hours_lock_for_24_hours() {
        // Fast guessing: four lockouts (15, 30, 60, 120 minutes) take 20
        // failures; the 21st, right after the fourth lockout, locks a day.
        let mut t = LoginThrottle::default();
        let mut now = 0;
        for expected in [15, 30, 60, 120] {
            assert_eq!(lock(&mut t, now), expected);
            now += expected;
        }
        assert_eq!(t.record_failure(at(now)), Some(DAY_LOCKOUT));
        assert_eq!(
            t.check(at(now + 1439)).map_err(|e| e.0),
            Err(SignedDuration::from_mins(1))
        );
        assert!(t.check(at(now + 1440)).is_ok());

        // Slow guessing under the first rule: one failure every 16 minutes
        // never has five in 15 minutes, but the 21st within a day locks.
        let mut t = LoginThrottle::default();
        for i in 0..20 {
            assert_eq!(t.record_failure(at(i * 16)), None, "failure {}", i + 1);
        }
        assert_eq!(t.record_failure(at(20 * 16)), Some(DAY_LOCKOUT));
        // After the lock the count starts again.
        let after = 20 * 16 + 1440;
        assert!(t.check(at(after)).is_ok());
        assert_eq!(t.record_failure(at(after)), None);

        // Twenty a day, spread out, is still allowed (and useless).
        let mut t = LoginThrottle::default();
        for i in 0..200 {
            assert_eq!(t.record_failure(at(i * 72)), None, "failure {}", i + 1);
        }
    }

    /// B3(d): the state survives being stored and read back.
    #[test]
    fn r82_state_round_trips_through_its_stored_form() {
        let mut t = LoginThrottle::default();
        assert_eq!(LoginThrottle::from_json(&t.to_json()), t);
        assert_eq!(LoginThrottle::from_json("{}"), t);
        lock(&mut t, 0);
        t.record_failure(at(20));
        t.record_failure(at(21));
        let restored = LoginThrottle::from_json(&t.to_json());
        assert_eq!(restored, t);
        let mut restored = restored;
        assert!(
            restored.check(at(5)).is_err(),
            "still locked after a restart"
        );
        assert!(!t.to_json().contains("password"));
        // A damaged row never locks the owner out.
        for bad in ["", "null", "[]", "{\"failures\": 3}", "not json"] {
            assert_eq!(
                LoginThrottle::from_json(bad),
                LoginThrottle::default(),
                "{bad}"
            );
        }
        let partly = LoginThrottle::from_json(r#"{"failures":["garbage","2026-11-20T09:00:00Z"]}"#);
        assert_eq!(partly.failures.len(), 1);
    }

    /// Clock errors: a clock that ran ahead (a lock dated far in the
    /// future, failures in the future) and was then set back.
    #[test]
    fn r82_a_clock_that_jumps_forward_and_back_never_locks_for_more_than_a_day() {
        // Locked while the clock was a year ahead.
        let mut t = LoginThrottle::default();
        let year = 365 * 24 * 60;
        lock(&mut t, year);
        t.record_failure(at(year + 1));
        let mut restored = LoginThrottle::from_json(&t.to_json());
        // Clock corrected: the lock ends at most 24 hours from now.
        let (retry, _) = restored.check(at(0)).expect_err("still locked");
        assert_eq!(retry, MAX_LOCKOUT);
        assert!(restored.failures.is_empty() && restored.day_failures.is_empty());
        assert_eq!(restored.last_failure, Some(at(0)));
        assert!(restored.check(at(24 * 60)).is_ok());
        // The future failures no longer count: four new ones do not lock.
        let mut t = LoginThrottle::default();
        for i in 0..4 {
            t.record_failure(at(1000 + i));
        }
        for i in 0..4 {
            assert_eq!(
                t.record_failure(at(i)),
                None,
                "failure {i} after the jump back"
            );
        }
        // Jumping forward past the lock simply ends it.
        let mut t = LoginThrottle::default();
        lock(&mut t, 0);
        assert!(t.check(at(10 * year)).is_ok());
    }

    /// Refused attempts keep the decay going across a restart, stored at
    /// most once a minute.
    #[test]
    fn r82_refused_attempts_are_stored_at_most_once_a_minute() {
        let mut t = LoginThrottle::default();
        lock(&mut t, 0);
        assert_eq!(t.check(at(0)).map_err(|e| e.1), Err(false), "just stored");
        assert_eq!(t.check(at(1)).map_err(|e| e.1), Err(true));
        assert_eq!(t.check(at(1)).map_err(|e| e.1), Err(false));
        assert_eq!(t.check(at(2)).map_err(|e| e.1), Err(true));
        let restored = LoginThrottle::from_json(&t.to_json());
        assert_eq!(restored.last_failure, Some(at(2)));
    }
}
