//! Per-target pacing: each target gets the newest value within its own rate budget.
//!
//! A pacer holds at most one pending value. Offering a new value replaces the
//! pending one ("latest wins"), so a slow target never falls behind the song.
//! A value equal to the one last sent is not sent again.

use std::time::{Duration, Instant};

/// Longest interval rate-limit backoff can grow to.
pub const MAX_INTERVAL: Duration = Duration::from_secs(60);

/// Consecutive rate-limit responses after which a target is switched off.
pub const MAX_CONSECUTIVE_RATE_LIMITS: u32 = 3;

/// What to do after a target reports a rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitAction {
    /// Wait until this long from now before the next send; the interval was raised.
    Backoff(Duration),
    /// Too many rate limits in a row: stop using this target.
    Disable,
}

/// See the module docs.
#[derive(Debug)]
pub struct Pacer<T> {
    _value: Option<T>,
}

impl<T: Clone + PartialEq> Pacer<T> {
    /// A pacer that sends at most once per `min_interval`. A zero interval sends
    /// every distinct value immediately.
    pub fn new(min_interval: Duration) -> Self {
        let _ = min_interval;
        todo!()
    }

    /// Current minimum interval (grows with rate-limit backoff).
    pub fn interval(&self) -> Duration {
        todo!()
    }

    /// Offers a value. It becomes pending unless it equals the value last sent,
    /// in which case any pending value is dropped instead.
    pub fn offer(&mut self, value: T) {
        let _ = value;
        todo!()
    }

    /// Takes the pending value if it may be sent at `now`: the interval since the
    /// last send has passed and any backoff has ended. Marks it as sent at `now`.
    pub fn poll(&mut self, now: Instant) -> Option<T> {
        let _ = now;
        todo!()
    }

    /// When the pending value may be sent, or `None` when nothing is pending.
    pub fn next_ready_at(&self) -> Option<Instant> {
        todo!()
    }

    /// The value last sent, if any.
    pub fn last_sent(&self) -> Option<&T> {
        todo!()
    }

    /// Reports that sending the value returned by the last [`poll`](Self::poll)
    /// succeeded. Resets the consecutive rate-limit count.
    pub fn on_success(&mut self) {
        todo!()
    }

    /// Reports that sending `value` failed for a reason other than a rate limit.
    /// The value becomes pending again unless a newer one was offered meanwhile,
    /// and "last sent" goes back to what it was before that poll, so a retry is
    /// not suppressed as a duplicate.
    pub fn on_failure(&mut self, value: T) {
        let _ = value;
        todo!()
    }

    /// Reports a rate-limit response for `value`. Doubles the interval (capped at
    /// [`MAX_INTERVAL`], starting from 1 s if it was zero), blocks sends until
    /// `now + max(retry_after, new interval)`, and requeues `value` like
    /// [`on_failure`](Self::on_failure). Returns [`RateLimitAction::Disable`] on
    /// the [`MAX_CONSECUTIVE_RATE_LIMITS`]th consecutive rate limit.
    pub fn on_rate_limited(
        &mut self,
        value: T,
        now: Instant,
        retry_after: Option<Duration>,
    ) -> RateLimitAction {
        let _ = (value, now, retry_after);
        todo!()
    }

    /// Forgets the last sent value and any pending value, keeping the interval
    /// and backoff. Used after a target was cleared so the next value is sent.
    pub fn reset(&mut self) {
        todo!()
    }
}
