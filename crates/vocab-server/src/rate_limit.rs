//! Sliding-window request limits (design §9.4): at most `max` requests in
//! any `window`. Used for the paste import and the connector routes.

use std::collections::VecDeque;
use std::sync::Mutex;

use jiff::SignedDuration;

use crate::clock::Timestamp;

#[derive(Debug)]
pub struct RateLimit {
    max: usize,
    window: SignedDuration,
    /// Times of the requests inside the window, oldest first.
    hits: Mutex<VecDeque<Timestamp>>,
}

impl RateLimit {
    pub fn new(max: usize, window: SignedDuration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(VecDeque::new()),
        }
    }

    /// Count a request at `now`; `Err(retry_after)` when the limit is reached
    /// (the refused request is not counted).
    pub fn hit(&self, now: Timestamp) -> Result<(), SignedDuration> {
        let mut hits = self
            .hits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while hits
            .front()
            .is_some_and(|first| now.duration_since(*first) >= self.window)
        {
            hits.pop_front();
        }
        if hits.len() >= self.max {
            let oldest = hits.front().copied().unwrap_or(now);
            return Err(self.window - now.duration_since(oldest));
        }
        hits.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        let start: Timestamp = "2026-11-20T09:00:00Z".parse().expect("ts");
        start
            .saturating_add(SignedDuration::from_secs(secs))
            .expect("in range")
    }

    #[test]
    fn r83_thirty_requests_per_ten_minutes() {
        let limit = RateLimit::new(30, SignedDuration::from_mins(10));
        for i in 0..30 {
            assert!(limit.hit(at(i)).is_ok(), "request {i}");
        }
        assert_eq!(limit.hit(at(30)), Err(SignedDuration::from_secs(570)));
        assert_eq!(limit.hit(at(599)), Err(SignedDuration::from_secs(1)));
        // The first request leaves the window after 10 minutes.
        assert!(limit.hit(at(600)).is_ok());
        assert!(limit.hit(at(600)).is_err());
        assert!(limit.hit(at(601)).is_ok());
    }
}
