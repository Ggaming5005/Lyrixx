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
    /// The interval in use: starts at the `min_interval` given to
    /// [`Pacer::new`] and only grows, through rate-limit backoff.
    interval: Duration,
    /// The newest value waiting to be sent.
    pending: Option<T>,
    /// The value handed out by the last successful [`Pacer::poll`].
    last_sent: Option<T>,
    /// `Some(previous last_sent)` while a polled value has not been reported
    /// yet, so a failure can restore "last sent" to what it was before.
    rollback: Option<Option<T>>,
    /// When the last value was handed out.
    last_send_at: Option<Instant>,
    /// No send before this moment (rate-limit backoff).
    backoff_until: Option<Instant>,
    /// Rate-limit responses since the last success.
    consecutive_rate_limits: u32,
    /// The latest time passed to this pacer, used by [`Pacer::next_ready_at`]
    /// to answer "ready now" without reading the clock.
    last_seen: Option<Instant>,
}

impl<T: Clone + PartialEq> Pacer<T> {
    /// A pacer that sends at most once per `min_interval`. A zero interval sends
    /// every distinct value immediately.
    pub fn new(min_interval: Duration) -> Self {
        Self {
            interval: min_interval,
            pending: None,
            last_sent: None,
            rollback: None,
            last_send_at: None,
            backoff_until: None,
            consecutive_rate_limits: 0,
            last_seen: None,
        }
    }

    /// Current minimum interval (grows with rate-limit backoff).
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Offers a value. It becomes pending unless it equals the value last sent,
    /// in which case any pending value is dropped instead.
    pub fn offer(&mut self, value: T) {
        if self.last_sent.as_ref() == Some(&value) {
            self.pending = None;
        } else {
            self.pending = Some(value);
        }
    }

    /// Takes the pending value if it may be sent at `now`: the interval since the
    /// last send has passed and any backoff has ended. Marks it as sent at `now`.
    pub fn poll(&mut self, now: Instant) -> Option<T> {
        self.see(now);
        self.pending.as_ref()?;
        if let Some(ready_at) = self.constraint() {
            if now < ready_at {
                return None;
            }
        }
        let value = self.pending.take()?;
        let previous = self.last_sent.replace(value.clone());
        self.rollback = Some(previous);
        self.last_send_at = Some(now);
        Some(value)
    }

    /// When the pending value may be sent, or `None` when nothing is pending.
    ///
    /// A value that may be sent right away gives a moment that is not in the
    /// future: the latest time passed to this pacer, or the current time when
    /// no time has been passed in yet.
    pub fn next_ready_at(&self) -> Option<Instant> {
        self.pending.as_ref()?;
        if let Some(at) = self.constraint() {
            return Some(at);
        }
        // Nothing sent yet and no backoff: ready now. Prefer a time we were
        // given; only a pacer that has never seen a time reads the clock.
        Some(self.last_seen.unwrap_or_else(Instant::now))
    }

    /// The value last sent, if any.
    pub fn last_sent(&self) -> Option<&T> {
        self.last_sent.as_ref()
    }

    /// Reports that sending the value returned by the last [`poll`](Self::poll)
    /// succeeded. Resets the consecutive rate-limit count.
    pub fn on_success(&mut self) {
        self.rollback = None;
        self.consecutive_rate_limits = 0;
    }

    /// Reports that sending `value` failed for a reason other than a rate limit.
    /// The value becomes pending again unless a newer one was offered meanwhile,
    /// and "last sent" goes back to what it was before that poll, so a retry is
    /// not suppressed as a duplicate.
    ///
    /// The retry still waits for the interval since the failed attempt. A newer
    /// pending value that equals the restored "last sent" is dropped, as
    /// [`offer`](Self::offer) would. The consecutive rate-limit count is kept.
    /// A report about a value older than the last poll (a newer value was
    /// handed out since and has not been reported yet) does not requeue it.
    ///
    /// Each value from [`poll`](Self::poll) should be reported before the next
    /// poll; [`on_success`](Self::on_success) always refers to the last poll.
    pub fn on_failure(&mut self, value: T) {
        self.requeue(value);
    }

    /// Reports a rate-limit response for `value`. Doubles the interval (capped at
    /// [`MAX_INTERVAL`], starting from 1 s if it was zero), blocks sends until
    /// `now + max(retry_after, new interval)`, and requeues `value` like
    /// [`on_failure`](Self::on_failure). Returns [`RateLimitAction::Disable`] on
    /// the [`MAX_CONSECUTIVE_RATE_LIMITS`]th consecutive rate limit.
    ///
    /// A zero interval becomes 1 s. An interval already above [`MAX_INTERVAL`]
    /// (configured that way) is kept rather than lowered.
    pub fn on_rate_limited(
        &mut self,
        value: T,
        now: Instant,
        retry_after: Option<Duration>,
    ) -> RateLimitAction {
        self.see(now);
        self.consecutive_rate_limits = self.consecutive_rate_limits.saturating_add(1);

        let raised = if self.interval.is_zero() {
            Duration::from_secs(1)
        } else {
            self.interval.saturating_mul(2)
        };
        self.interval = raised.min(MAX_INTERVAL).max(self.interval);

        let mut wait = retry_after.unwrap_or(Duration::ZERO).max(self.interval);
        let until = add_saturating(now, wait);
        // Never shorten a backoff that is already longer.
        self.backoff_until = Some(match self.backoff_until {
            Some(existing) if existing > until => {
                wait = wait.max(existing.saturating_duration_since(now));
                existing
            }
            _ => until,
        });

        self.requeue(value);

        if self.consecutive_rate_limits >= MAX_CONSECUTIVE_RATE_LIMITS {
            RateLimitAction::Disable
        } else {
            RateLimitAction::Backoff(wait)
        }
    }

    /// Forgets the last sent value and any pending value, keeping the interval
    /// and backoff. Used after a target was cleared so the next value is sent.
    pub fn reset(&mut self) {
        self.pending = None;
        self.last_sent = None;
        self.rollback = None;
    }

    /// Remembers the latest time we were given.
    fn see(&mut self, now: Instant) {
        self.last_seen = Some(match self.last_seen {
            Some(seen) => seen.max(now),
            None => now,
        });
    }

    /// The earliest moment a send is allowed by the interval and backoff, or
    /// `None` when neither applies (nothing sent yet and no backoff).
    fn constraint(&self) -> Option<Instant> {
        let after_interval = self
            .last_send_at
            .map(|at| add_saturating(at, self.interval));
        match (after_interval, self.backoff_until) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Shared by failure and rate-limit reports: restores "last sent" to what it
    /// was before the poll that handed out `value`, and makes `value` pending
    /// again unless a newer value is already waiting.
    fn requeue(&mut self, value: T) {
        if self.rollback.is_some() && self.last_sent.as_ref() != Some(&value) {
            // A newer value was handed out after `value` and is still awaiting
            // its report, so `value` is out of date and must not be resent.
            // `value` never reached the target either, so if the newer send
            // was going to fall back to it, fall back to "nothing known" instead.
            if let Some(Some(previous)) = &self.rollback {
                if *previous == value {
                    self.rollback = Some(None);
                }
            }
            return;
        }
        if let Some(previous) = self.rollback.take() {
            // `value` is the one the last poll handed out.
            self.last_sent = previous;
        }
        let pending_is_duplicate = match &self.pending {
            Some(newer) => self.last_sent.as_ref() == Some(newer),
            None => false,
        };
        if pending_is_duplicate {
            // The newer value is what the target already shows.
            self.pending = None;
        } else if self.pending.is_none() && self.last_sent.as_ref() != Some(&value) {
            self.pending = Some(value);
        }
    }
}

/// `at + d`, or the latest representable moment when that would overflow.
fn add_saturating(at: Instant, d: Duration) -> Instant {
    if let Some(sum) = at.checked_add(d) {
        return sum;
    }
    // Find (nearly) the largest step that still fits by halving.
    let mut step = d;
    let mut result = at;
    while !step.is_zero() {
        if let Some(sum) = result.checked_add(step) {
            result = sum;
        } else {
            step /= 2;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn new_pacer_is_empty() {
        let p: Pacer<u32> = Pacer::new(secs(2));
        assert_eq!(p.interval(), secs(2));
        assert_eq!(p.last_sent(), None);
        assert_eq!(p.next_ready_at(), None);
    }

    #[test]
    fn poll_with_nothing_pending_returns_none() {
        let t0 = Instant::now();
        let mut p: Pacer<u32> = Pacer::new(secs(1));
        assert_eq!(p.poll(t0), None);
        assert_eq!(p.poll(t0 + secs(10)), None);
        assert_eq!(p.last_sent(), None);
    }

    #[test]
    fn first_value_is_sent_immediately() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        assert_eq!(p.last_sent(), Some(&"a"));
        assert_eq!(p.poll(t0), None);
    }

    #[test]
    fn latest_offer_wins() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(1);
        p.offer(2);
        p.offer(3);
        assert_eq!(p.poll(t0), Some(3));
        assert_eq!(p.poll(t0 + secs(5)), None);
    }

    #[test]
    fn latest_wins_while_waiting_for_interval() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.offer(2);
        assert_eq!(p.poll(t0 + ms(500)), None);
        p.offer(3);
        assert_eq!(p.poll(t0 + ms(1_000)), None);
        p.offer(4);
        assert_eq!(p.poll(t0 + secs(2)), Some(4));
    }

    #[test]
    fn duplicate_of_last_sent_is_not_sent_again() {
        let t0 = Instant::now();
        let mut p = Pacer::new(ms(100));
        p.offer("x");
        assert_eq!(p.poll(t0), Some("x"));
        p.on_success();
        p.offer("x");
        assert_eq!(p.next_ready_at(), None);
        assert_eq!(p.poll(t0 + secs(10)), None);
    }

    #[test]
    fn duplicate_of_last_sent_drops_pending_value() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.on_success();
        // Line changes, then changes back before the interval passes.
        p.offer("b");
        assert!(p.next_ready_at().is_some());
        p.offer("a");
        assert_eq!(p.next_ready_at(), None);
        assert_eq!(p.poll(t0 + secs(3)), None);
        assert_eq!(p.last_sent(), Some(&"a"));
    }

    #[test]
    fn interval_is_respected() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.on_success();
        p.offer(2);
        assert_eq!(p.next_ready_at(), Some(t0 + secs(2)));
        assert_eq!(p.poll(t0 + ms(1_999)), None);
        assert_eq!(p.poll(t0 + secs(2)), Some(2));
        p.on_success();
        p.offer(3);
        assert_eq!(p.next_ready_at(), Some(t0 + secs(4)));
        assert_eq!(p.poll(t0 + ms(3_999)), None);
        assert_eq!(p.poll(t0 + secs(4)), Some(3));
    }

    #[test]
    fn interval_counts_from_the_last_send_not_the_last_poll() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.on_success();
        // Quiet period long after the interval.
        assert_eq!(p.poll(t0 + secs(30)), None);
        p.offer(2);
        assert_eq!(p.poll(t0 + secs(31)), Some(2));
    }

    #[test]
    fn zero_interval_sends_every_distinct_value_immediately() {
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::ZERO);
        for i in 0..100u32 {
            p.offer(i);
            assert_eq!(p.poll(t0), Some(i));
            p.on_success();
        }
        p.offer(99);
        assert_eq!(p.poll(t0), None);
    }

    #[test]
    fn on_failure_requeues_and_allows_retry() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer("a");
        let v = p.poll(t0).unwrap();
        p.on_failure(v);
        assert_eq!(p.last_sent(), None);
        // The retry waits for the interval since the failed attempt.
        assert_eq!(p.next_ready_at(), Some(t0 + secs(1)));
        assert_eq!(p.poll(t0 + ms(500)), None);
        assert_eq!(p.poll(t0 + secs(1)), Some("a"));
    }

    #[test]
    fn on_failure_restores_last_sent_so_retry_is_not_a_duplicate() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.on_success();
        p.offer("b");
        assert_eq!(p.poll(t0 + secs(1)), Some("b"));
        assert_eq!(p.last_sent(), Some(&"b"));
        p.on_failure("b");
        assert_eq!(p.last_sent(), Some(&"a"));
        // The engine keeps offering the desired value every tick.
        p.offer("b");
        assert_eq!(p.poll(t0 + secs(2)), Some("b"));
        p.on_success();
        assert_eq!(p.last_sent(), Some(&"b"));
    }

    #[test]
    fn on_failure_keeps_newer_offer() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.offer(2);
        p.on_failure(1);
        assert_eq!(p.last_sent(), None);
        assert_eq!(p.poll(t0 + secs(1)), Some(2));
        assert_eq!(p.poll(t0 + secs(5)), None);
    }

    #[test]
    fn on_failure_requeues_even_if_same_value_was_offered_meanwhile() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        // Same value offered while in flight: dropped as a duplicate.
        p.offer(1);
        p.on_failure(1);
        assert_eq!(p.poll(t0 + secs(1)), Some(1));
    }

    #[test]
    fn on_failure_drops_newer_value_equal_to_restored_last_sent() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.on_success();
        p.offer("b");
        assert_eq!(p.poll(t0 + secs(1)), Some("b"));
        // Back to "a" while "b" is in flight, then "b" fails: the target still
        // shows "a", so nothing needs sending.
        p.offer("a");
        p.on_failure("b");
        assert_eq!(p.last_sent(), Some(&"a"));
        assert_eq!(p.next_ready_at(), None);
        assert_eq!(p.poll(t0 + secs(10)), None);
    }

    #[test]
    fn stale_failure_report_does_not_resend_an_older_value() {
        // Two sends overlap: "a" is still in flight when "b" is handed out.
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::ZERO);
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.offer("b");
        assert_eq!(p.poll(t0), Some("b"));
        // "a" fails after the newer "b" was handed out: "a" must not come back.
        p.on_failure("a");
        assert_eq!(p.last_sent(), Some(&"b"));
        assert_eq!(p.next_ready_at(), None);
        p.on_success();
        assert_eq!(p.poll(t0 + secs(1)), None);
        assert_eq!(p.last_sent(), Some(&"b"));
    }

    #[test]
    fn stale_failure_report_keeps_the_newer_send_retryable() {
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::ZERO);
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.offer("b");
        assert_eq!(p.poll(t0), Some("b"));
        p.on_failure("a");
        // Then "b" fails too: it is retried, and since "a" never arrived the
        // target is not assumed to show "a".
        p.on_failure("b");
        assert_eq!(p.last_sent(), None);
        assert_eq!(p.poll(t0), Some("b"));
        p.on_success();
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
    }

    #[test]
    fn late_failure_report_after_success_changes_nothing() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.on_success();
        p.offer("b");
        assert_eq!(p.poll(t0 + secs(1)), Some("b"));
        // A report about "a" arriving now is out of date: "b" is on its way.
        p.on_failure("a");
        assert_eq!(p.last_sent(), Some(&"b"));
        assert_eq!(p.next_ready_at(), None);
        // The report about "b" still requeues it.
        p.on_failure("b");
        assert_eq!(p.poll(t0 + secs(2)), Some("b"));
        p.on_success();
        assert_eq!(p.poll(t0 + secs(10)), None);
    }

    #[test]
    fn stale_rate_limit_report_backs_off_without_resending() {
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::ZERO);
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.offer(2);
        assert_eq!(p.poll(t0), Some(2));
        // The rate limit still counts and still slows the target down...
        assert_eq!(
            p.on_rate_limited(1, t0, None),
            RateLimitAction::Backoff(secs(1))
        );
        assert_eq!(p.interval(), secs(1));
        // ...but the older value is not queued behind the newer one.
        assert_eq!(p.last_sent(), Some(&2));
        assert_eq!(p.next_ready_at(), None);
        p.on_failure(2);
        assert_eq!(p.poll(t0 + ms(999)), None);
        assert_eq!(p.poll(t0 + secs(1)), Some(2));
    }

    #[test]
    fn on_failure_without_poll_requeues_value() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.on_failure(7);
        assert_eq!(p.poll(t0), Some(7));
    }

    #[test]
    fn repeated_failures_keep_retrying() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer("a");
        for i in 0..5u64 {
            let at = t0 + secs(i);
            assert_eq!(p.poll(at), Some("a"), "attempt {i}");
            p.on_failure("a");
            assert_eq!(p.last_sent(), None);
        }
        assert_eq!(p.poll(t0 + secs(5)), Some("a"));
        p.on_success();
        assert_eq!(p.last_sent(), Some(&"a"));
    }

    #[test]
    fn on_success_commits_last_sent() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.on_success();
        // A late failure report after success has nothing to roll back.
        p.on_failure(1);
        assert_eq!(p.last_sent(), Some(&1));
        assert_eq!(p.poll(t0 + secs(5)), None);
    }

    #[test]
    fn rate_limit_doubles_interval() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        let action = p.on_rate_limited(1, t0, None);
        assert_eq!(action, RateLimitAction::Backoff(secs(4)));
        assert_eq!(p.interval(), secs(4));
        assert_eq!(p.last_sent(), None);
        assert_eq!(p.next_ready_at(), Some(t0 + secs(4)));
        assert_eq!(p.poll(t0 + ms(3_999)), None);
        assert_eq!(p.poll(t0 + secs(4)), Some(1));
    }

    #[test]
    fn rate_limit_from_zero_interval_starts_at_one_second() {
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::ZERO);
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        assert_eq!(
            p.on_rate_limited(1, t0, None),
            RateLimitAction::Backoff(secs(1))
        );
        assert_eq!(p.interval(), secs(1));
        assert_eq!(p.poll(t0 + ms(999)), None);
        assert_eq!(p.poll(t0 + secs(1)), Some(1));
        p.on_success();
        p.offer(2);
        assert_eq!(p.poll(t0 + ms(1_500)), None);
        assert_eq!(p.poll(t0 + secs(2)), Some(2));
    }

    #[test]
    fn rate_limit_interval_is_capped() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(40));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        assert_eq!(
            p.on_rate_limited(1, t0, None),
            RateLimitAction::Backoff(MAX_INTERVAL)
        );
        assert_eq!(p.interval(), MAX_INTERVAL);
        assert_eq!(p.poll(t0 + MAX_INTERVAL), Some(1));
        p.on_success();
        p.on_rate_limited(1, t0 + MAX_INTERVAL, None);
        assert_eq!(p.interval(), MAX_INTERVAL);
    }

    #[test]
    fn rate_limit_doubling_sequence_reaches_cap() {
        let mut t = Instant::now();
        let mut p = Pacer::new(secs(1));
        let mut seen = Vec::new();
        for _ in 0..10 {
            p.offer(1);
            let v = p.poll(t + MAX_INTERVAL).unwrap();
            t += MAX_INTERVAL;
            let action = p.on_rate_limited(v, t, None);
            assert_ne!(action, RateLimitAction::Disable);
            p.on_success();
            seen.push(p.interval().as_secs());
        }
        assert_eq!(seen, vec![2, 4, 8, 16, 32, 60, 60, 60, 60, 60]);
    }

    #[test]
    fn interval_above_cap_is_not_lowered() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(120));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        assert_eq!(
            p.on_rate_limited(1, t0, None),
            RateLimitAction::Backoff(secs(120))
        );
        assert_eq!(p.interval(), secs(120));
    }

    #[test]
    fn longer_retry_after_wins() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        let action = p.on_rate_limited(1, t0, Some(secs(30)));
        assert_eq!(action, RateLimitAction::Backoff(secs(30)));
        assert_eq!(p.interval(), secs(4));
        assert_eq!(p.next_ready_at(), Some(t0 + secs(30)));
        assert_eq!(p.poll(t0 + secs(29)), None);
        assert_eq!(p.poll(t0 + secs(30)), Some(1));
    }

    #[test]
    fn shorter_retry_after_loses_to_interval() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        let action = p.on_rate_limited(1, t0, Some(ms(100)));
        assert_eq!(action, RateLimitAction::Backoff(secs(4)));
        assert_eq!(p.poll(t0 + ms(100)), None);
        assert_eq!(p.poll(t0 + secs(4)), Some(1));
    }

    #[test]
    fn longer_existing_backoff_is_kept_and_reported() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        assert_eq!(
            p.on_rate_limited(1, t0, Some(secs(100))),
            RateLimitAction::Backoff(secs(100))
        );
        // A second report (e.g. for a send already in flight) with a shorter
        // wait does not shorten the backoff.
        assert_eq!(
            p.on_rate_limited(1, t0 + secs(10), None),
            RateLimitAction::Backoff(secs(90))
        );
        assert_eq!(p.interval(), secs(4));
        assert_eq!(p.next_ready_at(), Some(t0 + secs(100)));
        assert_eq!(p.poll(t0 + secs(99)), None);
        assert_eq!(p.poll(t0 + secs(100)), Some(1));
    }

    #[test]
    fn backoff_counts_from_report_time() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        // The response arrives later than the send.
        let reported = t0 + secs(3);
        p.on_rate_limited(1, reported, None);
        assert_eq!(p.next_ready_at(), Some(reported + secs(2)));
        assert_eq!(p.poll(reported + ms(1_999)), None);
        assert_eq!(p.poll(reported + secs(2)), Some(1));
    }

    #[test]
    fn huge_retry_after_does_not_panic() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        let action = p.on_rate_limited(1, t0, Some(Duration::MAX));
        assert_eq!(action, RateLimitAction::Backoff(Duration::MAX));
        let ready = p.next_ready_at().unwrap();
        assert!(ready > t0 + secs(365 * 24 * 3600));
        assert_eq!(p.poll(t0 + secs(3600)), None);
    }

    #[test]
    fn huge_min_interval_does_not_panic() {
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::MAX);
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.on_success();
        p.offer(2);
        assert!(p.next_ready_at().unwrap() > t0);
        assert_eq!(p.poll(t0 + secs(1_000_000)), None);
        assert_eq!(
            p.on_rate_limited(2, t0, None),
            RateLimitAction::Backoff(Duration::MAX)
        );
        assert_eq!(p.interval(), Duration::MAX);
    }

    #[test]
    fn third_consecutive_rate_limit_disables() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        let mut t = t0;
        let mut actions = Vec::new();
        for _ in 0..MAX_CONSECUTIVE_RATE_LIMITS {
            let v = p.poll(t).expect("value ready after backoff");
            let action = p.on_rate_limited(v, t, None);
            actions.push(action);
            t += MAX_INTERVAL;
        }
        assert!(matches!(actions[0], RateLimitAction::Backoff(_)));
        assert!(matches!(actions[1], RateLimitAction::Backoff(_)));
        assert_eq!(actions[2], RateLimitAction::Disable);
    }

    #[test]
    fn rate_limits_after_disable_keep_disabling() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        let mut last = None;
        for i in 0..MAX_CONSECUTIVE_RATE_LIMITS + 5 {
            let t = t0 + secs(u64::from(i) * 60);
            last = Some(p.on_rate_limited(i, t, None));
            if i + 1 >= MAX_CONSECUTIVE_RATE_LIMITS {
                assert_eq!(last, Some(RateLimitAction::Disable), "report {i}");
            }
        }
        assert_eq!(last, Some(RateLimitAction::Disable));
        assert_eq!(p.interval(), MAX_INTERVAL);
    }

    #[test]
    fn zero_retry_after_uses_the_raised_interval() {
        let t0 = Instant::now();
        let mut p = Pacer::new(ms(500));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        assert_eq!(
            p.on_rate_limited(1, t0, Some(Duration::ZERO)),
            RateLimitAction::Backoff(secs(1))
        );
        assert_eq!(p.next_ready_at(), Some(t0 + secs(1)));
    }

    #[test]
    fn on_success_resets_rate_limit_count() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        let mut t = t0;
        for round in 0..3 {
            for _ in 0..MAX_CONSECUTIVE_RATE_LIMITS - 1 {
                p.offer(round);
                let v = p.poll(t).expect("ready");
                assert_ne!(p.on_rate_limited(v, t, None), RateLimitAction::Disable);
                t += MAX_INTERVAL;
            }
            let v = p.poll(t).expect("ready");
            assert_eq!(v, round);
            p.on_success();
            t += MAX_INTERVAL;
        }
        // Without a success in between, the next three in a row disable.
        p.offer(10);
        let mut last = None;
        for _ in 0..MAX_CONSECUTIVE_RATE_LIMITS {
            let v = p.poll(t).expect("ready");
            last = Some(p.on_rate_limited(v, t, None));
            t += MAX_INTERVAL;
        }
        assert_eq!(last, Some(RateLimitAction::Disable));
    }

    #[test]
    fn plain_failure_does_not_reset_rate_limit_count() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        let mut t = t0;
        p.offer(1);
        let v = p.poll(t).unwrap();
        assert_ne!(p.on_rate_limited(v, t, None), RateLimitAction::Disable);
        t += MAX_INTERVAL;
        let v = p.poll(t).unwrap();
        p.on_failure(v);
        t += MAX_INTERVAL;
        let v = p.poll(t).unwrap();
        assert_ne!(p.on_rate_limited(v, t, None), RateLimitAction::Disable);
        t += MAX_INTERVAL;
        let v = p.poll(t).unwrap();
        assert_eq!(p.on_rate_limited(v, t, None), RateLimitAction::Disable);
    }

    #[test]
    fn rate_limit_requeues_unless_newer_offered() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.offer("b");
        p.on_rate_limited("a", t0, None);
        assert_eq!(p.last_sent(), None);
        assert_eq!(p.poll(t0 + secs(2)), Some("b"));
    }

    #[test]
    fn reset_forgets_values_but_keeps_interval_and_backoff() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer("a");
        assert_eq!(p.poll(t0), Some("a"));
        p.on_rate_limited("a", t0, Some(secs(10)));
        assert_eq!(p.interval(), secs(2));
        p.reset();
        assert_eq!(p.last_sent(), None);
        assert_eq!(p.next_ready_at(), None);
        assert_eq!(p.poll(t0 + secs(20)), None);
        assert_eq!(p.interval(), secs(2));
        // Backoff still applies.
        p.offer("a");
        assert_eq!(p.next_ready_at(), Some(t0 + secs(10)));
        assert_eq!(p.poll(t0 + secs(9)), None);
        assert_eq!(p.poll(t0 + secs(10)), Some("a"));
    }

    #[test]
    fn reset_allows_resending_last_value() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(5);
        assert_eq!(p.poll(t0), Some(5));
        p.on_success();
        p.reset();
        p.offer(5);
        assert_eq!(p.poll(t0 + secs(1)), Some(5));
    }

    #[test]
    fn reset_keeps_the_interval_since_last_send() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(2));
        p.offer(5);
        assert_eq!(p.poll(t0), Some(5));
        p.on_success();
        p.reset();
        p.offer(6);
        assert_eq!(p.poll(t0 + secs(1)), None);
        assert_eq!(p.poll(t0 + secs(2)), Some(6));
    }

    #[test]
    fn failure_after_reset_requeues_without_restoring() {
        let t0 = Instant::now();
        let mut p = Pacer::new(Duration::ZERO);
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        p.on_success();
        p.offer(2);
        assert_eq!(p.poll(t0), Some(2));
        p.reset();
        p.on_failure(2);
        assert_eq!(p.last_sent(), None);
        assert_eq!(p.poll(t0), Some(2));
    }

    #[test]
    fn next_ready_at_uses_latest_time_seen_when_ready() {
        let t0 = Instant::now();
        let mut p: Pacer<u8> = Pacer::new(secs(1));
        assert_eq!(p.poll(t0 + secs(5)), None);
        assert_eq!(p.poll(t0 + secs(3)), None);
        p.offer(1);
        assert_eq!(p.next_ready_at(), Some(t0 + secs(5)));
    }

    #[test]
    fn next_ready_at_on_fresh_pacer_is_not_in_the_future() {
        let mut p = Pacer::new(secs(1));
        p.offer(1u8);
        let ready = p.next_ready_at().expect("pending value");
        assert!(ready <= Instant::now());
    }

    #[test]
    fn next_ready_at_is_none_after_pending_value_is_taken() {
        let t0 = Instant::now();
        let mut p = Pacer::new(secs(1));
        p.offer(1);
        assert_eq!(p.poll(t0), Some(1));
        assert_eq!(p.next_ready_at(), None);
    }

    #[test]
    fn works_with_option_values_like_the_engine() {
        // The engine paces `Option<Status>`, where `None` means "cleared".
        let t0 = Instant::now();
        let mut p: Pacer<Option<String>> = Pacer::new(secs(2));
        p.offer(None);
        assert_eq!(p.poll(t0), Some(None));
        p.on_success();
        p.offer(None);
        assert_eq!(p.poll(t0 + secs(5)), None);
        p.offer(Some("🎵 héllo wörld".to_string()));
        assert_eq!(
            p.poll(t0 + secs(5)),
            Some(Some("🎵 héllo wörld".to_string()))
        );
        p.on_success();
        p.offer(None);
        assert_eq!(p.poll(t0 + secs(6)), None);
        assert_eq!(p.poll(t0 + secs(7)), Some(None));
    }

    #[test]
    fn add_saturating_never_panics() {
        let t0 = Instant::now();
        assert_eq!(add_saturating(t0, secs(1)), t0 + secs(1));
        let far = add_saturating(t0, Duration::MAX);
        assert!(far > t0);
        assert_eq!(add_saturating(far, Duration::MAX), far);
        assert_eq!(add_saturating(far, Duration::ZERO), far);
    }
}
