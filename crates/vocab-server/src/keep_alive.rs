//! Per-connection request-head deadline with a longer keep-alive idle time
//! (design §9.4, R-83).
//!
//! hyper has one timer for HTTP/1 heads: `header_read_timeout` starts when
//! it begins to wait for a request head, which on a keep-alive connection
//! includes the idle time before the next request. A reverse proxy keeps
//! idle upstream connections for about 90 s; closing them after the 10 s
//! head deadline makes a reused connection fail now and then (a 502 for
//! the browser). This module tells the two apart, per connection:
//!
//! * once bytes of a head arrived, the head must be complete within the
//!   head deadline (counted from its first byte): slow heads still end fast;
//! * a connection that has answered a request may then sit idle for the
//!   keep-alive time before the next head begins;
//! * a fresh connection (nothing answered yet) gets only the head deadline,
//!   so silent connections are not held longer than before.
//!
//! [`wrap`] returns the stream (which notes reads and writes) and the
//! [`hyper::rt::Timer`] to give that connection's builder. hyper's only use
//! of the timer on an HTTP/1 server connection is the head deadline.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// What happened on one connection.
#[derive(Debug, Default)]
struct Activity {
    /// When the first byte of the head being waited for arrived.
    head_started: Mutex<Option<Instant>>,
    /// Something was written: a request was answered.
    responded: AtomicBool,
}

impl Activity {
    fn head_started(&self) -> Option<Instant> {
        *self
            .head_started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set_head_started(&self, value: Option<Instant>, only_if_unset: bool) {
        let mut slot = self
            .head_started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !only_if_unset || slot.is_none() {
            *slot = value;
        }
    }
}

/// The connection's stream; notes when data is read and written.
#[derive(Debug)]
pub struct Tracked<S> {
    inner: S,
    activity: Arc<Activity>,
}

/// The head deadline timer of one connection (see the module docs).
#[derive(Debug, Clone)]
pub struct HeadTimer {
    activity: Arc<Activity>,
    head: Duration,
    idle: Duration,
}

/// Wrap a new connection: its stream and the timer for its builder. `head`
/// is the time a head may take from its first byte (and the wait for the
/// first head), `idle` the keep-alive time between requests.
pub fn wrap<S>(stream: S, head: Duration, idle: Duration) -> (Tracked<S>, HeadTimer) {
    let activity = Arc::new(Activity::default());
    (
        Tracked {
            inner: stream,
            activity: Arc::clone(&activity),
        },
        HeadTimer {
            activity,
            head,
            idle: idle.max(head),
        },
    )
}

impl<S: AsyncRead + Unpin> AsyncRead for Tracked<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.activity.set_head_started(Some(Instant::now()), true);
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Tracked<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.activity.responded.store(true, Ordering::Relaxed);
        }
        result
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.activity.responded.store(true, Ordering::Relaxed);
        }
        result
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl hyper::rt::Timer for HeadTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        self.sleep_until(Instant::now() + duration)
    }

    /// hyper starts waiting for a head: `deadline` is now + `head`.
    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        // Bytes read before belong to the previous request (or are already
        // buffered by hyper and need no deadline).
        self.activity.set_head_started(None, false);
        let start = deadline.checked_sub(self.head).unwrap_or(deadline);
        Box::pin(HeadSleep {
            timer: self.clone(),
            start,
            sleep: Box::pin(tokio::time::sleep_until(deadline.into())),
        })
    }
}

impl HeadTimer {
    /// When the head wait that began at `start` ends, given what happened.
    fn deadline(&self, start: Instant) -> Instant {
        match self.activity.head_started() {
            Some(first_byte) => first_byte + self.head,
            None if self.activity.responded.load(Ordering::Relaxed) => start + self.idle,
            None => start + self.head,
        }
    }
}

/// The head deadline of one wait; moves when the first byte arrives.
struct HeadSleep {
    timer: HeadTimer,
    start: Instant,
    sleep: Pin<Box<tokio::time::Sleep>>,
}

impl Future for HeadSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let deadline = tokio::time::Instant::from_std(self.timer.deadline(self.start));
        if self.sleep.deadline() != deadline {
            self.sleep.as_mut().reset(deadline);
        }
        self.sleep.as_mut().poll(cx)
    }
}

impl hyper::rt::Sleep for HeadSleep {}

#[cfg(test)]
mod tests {
    use hyper::rt::Timer;

    use super::*;

    const HEAD: Duration = Duration::from_secs(10);
    const IDLE: Duration = Duration::from_secs(120);

    fn deadline_of(timer: &HeadTimer, start: Instant) -> Duration {
        timer.deadline(start) - start
    }

    #[tokio::test]
    async fn r83_idle_keep_alive_is_long_but_a_started_head_must_finish_fast() {
        let (io, timer) = wrap((), HEAD, IDLE);
        let start = Instant::now();
        drop(timer.sleep_until(start + HEAD));
        // A fresh connection: only the head deadline.
        assert_eq!(deadline_of(&timer, start), HEAD);
        // After an answer: the keep-alive time between requests.
        io.activity.responded.store(true, Ordering::Relaxed);
        assert_eq!(deadline_of(&timer, start), IDLE);
        // The first byte of the next head arrives after 100 s: 10 s more.
        let first = start + Duration::from_secs(100);
        io.activity.set_head_started(Some(first), true);
        io.activity
            .set_head_started(Some(first + Duration::from_secs(5)), true);
        assert_eq!(timer.deadline(start), first + HEAD);
        // The next wait starts afresh.
        drop(timer.sleep_until(first + HEAD));
        assert_eq!(io.activity.head_started(), None);
        assert_eq!(deadline_of(&timer, first), IDLE);
    }

    #[test]
    fn r83_idle_time_is_never_shorter_than_the_head_deadline() {
        let (_io, timer) = wrap((), HEAD, Duration::from_secs(1));
        assert_eq!(timer.idle, HEAD);
    }
}
