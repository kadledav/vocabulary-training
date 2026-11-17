//! Global login throttle (design §9.4, decision D-8).
//!
//! Single user, so the throttle is global rather than per client address
//! (`X-Forwarded-For` can be forged by any container on the shared Docker
//! network). After [`MAX_FAILURES`] failed logins within [`FAILURE_WINDOW`],
//! every login attempt is refused for [`FIRST_LOCKOUT`]; each further
//! lockout doubles that, up to [`MAX_LOCKOUT`]. The doubling starts over
//! after [`DECAY`] without any failed login (an attempt refused during a
//! lockout counts as one), and a successful login resets everything.
//! Sessions that already exist are not affected.

use std::collections::VecDeque;

use jiff::SignedDuration;

use crate::clock::Timestamp;

pub const MAX_FAILURES: usize = 5;
pub const FAILURE_WINDOW: SignedDuration = SignedDuration::from_mins(15);
pub const FIRST_LOCKOUT: SignedDuration = SignedDuration::from_mins(15);
pub const MAX_LOCKOUT: SignedDuration = SignedDuration::from_hours(24);
pub const DECAY: SignedDuration = SignedDuration::from_hours(24);

/// In-memory throttle state (lost on restart, which only helps the owner).
#[derive(Debug, Default)]
pub struct LoginThrottle {
    /// Times of the recent failures (within the window), oldest first.
    failures: VecDeque<Timestamp>,
    /// Logins are refused until then.
    locked_until: Option<Timestamp>,
    /// Lockouts since the last successful login (or the last quiet day).
    lockouts: u32,
    /// The latest failed or refused login.
    last_failure: Option<Timestamp>,
}

impl LoginThrottle {
    /// `Err(retry_after)` while logins are locked (the refused attempt
    /// counts as a failed login for the decay).
    pub fn check(&mut self, now: Timestamp) -> Result<(), SignedDuration> {
        match self.locked_until {
            Some(until) if until > now => {
                self.last_failure = Some(now);
                Err(until.duration_since(now))
            }
            _ => Ok(()),
        }
    }

    /// Record a failed login; returns the lockout it started, if any.
    pub fn record_failure(&mut self, now: Timestamp) -> Option<SignedDuration> {
        if self
            .last_failure
            .is_some_and(|last| now.duration_since(last) >= DECAY)
        {
            self.lockouts = 0;
        }
        self.last_failure = Some(now);
        while self
            .failures
            .front()
            .is_some_and(|first| now.duration_since(*first) >= FAILURE_WINDOW)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        if self.failures.len() < MAX_FAILURES {
            return None;
        }
        self.failures.clear();
        let doublings = i32::try_from(self.lockouts).unwrap_or(i32::MAX).min(16);
        let lockout = FIRST_LOCKOUT
            .checked_mul(1 << doublings)
            .map_or(MAX_LOCKOUT, |d| d.min(MAX_LOCKOUT));
        self.lockouts = self.lockouts.saturating_add(1);
        self.locked_until = Some(now.saturating_add(lockout).unwrap_or(now));
        Some(lockout)
    }

    /// A successful login resets the throttle.
    pub fn record_success(&mut self) {
        *self = Self::default();
    }
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
        assert_eq!(t.check(at(4)), Err(SignedDuration::from_mins(15)));
        assert_eq!(t.check(at(18)), Err(SignedDuration::from_mins(1)));
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

    #[test]
    fn r82_lockouts_double_up_to_24_hours_and_success_resets() {
        let mut t = LoginThrottle::default();
        let mut now = 0;
        let mut lockouts = Vec::new();
        for _ in 0..9 {
            let mut lockout = None;
            for _ in 0..MAX_FAILURES {
                lockout = t.record_failure(at(now));
            }
            let lockout = lockout.expect("locked");
            lockouts.push(lockout.as_mins());
            // Someone keeps trying until the end: no quiet day, no decay.
            assert!(t.check(at(now + lockout.as_mins() - 1)).is_err());
            now += lockout.as_mins();
            assert!(t.check(at(now)).is_ok());
        }
        assert_eq!(lockouts, [15, 30, 60, 120, 240, 480, 960, 1440, 1440]);
        t.record_success();
        for _ in 0..MAX_FAILURES - 1 {
            t.record_failure(at(now));
        }
        assert_eq!(
            t.record_failure(at(now)),
            Some(SignedDuration::from_mins(15))
        );
    }

    #[test]
    fn r82_the_doubling_starts_over_after_24_quiet_hours() {
        let mut t = LoginThrottle::default();
        let lock = |t: &mut LoginThrottle, now: i64| {
            let mut lockout = None;
            for _ in 0..MAX_FAILURES {
                lockout = t.record_failure(at(now));
            }
            lockout.expect("locked").as_mins()
        };
        assert_eq!(lock(&mut t, 0), 15);
        assert_eq!(lock(&mut t, 15), 30);
        // Quiet for just under a day: the level stays.
        assert_eq!(lock(&mut t, 15 + 1439), 60);
        // Quiet for a whole day: back to 15 minutes.
        assert_eq!(lock(&mut t, 15 + 1439 + 1440), 15);
    }
}
