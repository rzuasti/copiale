//! Limits retry requests per device, both ways: a message is asked for, or
//! resent, once, so two devices that keep failing don't loop. Holds those
//! asked for while the server rate limits us.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use tokio::time::Instant;

use crate::backend::RetryRequest;
use crate::model::{ServiceId, Timestamp};

pub(super) struct Limiter {
    window: Duration,
    /// Most requests per device within `window`.
    max: usize,
    /// Recent requests by device: when, and for which message.
    recent: HashMap<(ServiceId, u32), VecDeque<(Instant, Timestamp)>>,
}

impl Limiter {
    pub(super) fn new(window: Duration, max: usize) -> Self {
        Self {
            window,
            max,
            recent: HashMap::new(),
        }
    }

    /// Whether to act on a request about the message sent at `sent_at` by,
    /// or to, `account`'s `device`: not if done within the window, nor past
    /// the device's limit.
    pub(super) fn allow(
        &mut self,
        account: ServiceId,
        device: u32,
        sent_at: Timestamp,
        now: Instant,
    ) -> bool {
        let window = self.window;
        let fresh = |at: &Instant| now.duration_since(*at) < window;
        if self.recent.len() > MAX_TRACKED {
            self.recent
                .retain(|_, recent| recent.back().is_some_and(|(at, _)| fresh(at)));
        }
        let recent = self.recent.entry((account, device)).or_default();
        while recent.front().is_some_and(|(at, _)| !fresh(at)) {
            recent.pop_front();
        }
        if recent.len() >= self.max || recent.iter().any(|(_, seen)| *seen == sent_at) {
            return false;
        }
        recent.push_back((now, sent_at));
        true
    }
}

/// Devices tracked before idle ones are dropped.
const MAX_TRACKED: usize = 256;

/// A retry request to send: for `sender`'s `device`, about the message
/// sent at `sent_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingRetry {
    pub sender: ServiceId,
    pub device: u32,
    pub sent_at: Timestamp,
    pub retry: RetryRequest,
}

/// Retry requests waiting for a rate limit to end, oldest first; each
/// message once, at most [`MAX_WAITING`].
#[derive(Default)]
pub(super) struct Waiting {
    requests: VecDeque<PendingRetry>,
}

impl Waiting {
    /// Queues `request`, unless it's queued already or the queue is full;
    /// returns whether it did.
    pub(super) fn push(&mut self, request: PendingRetry) -> bool {
        let queued = self.requests.iter().any(|queued| {
            (queued.sender, queued.device, queued.sent_at)
                == (request.sender, request.device, request.sent_at)
        });
        if queued || self.requests.len() >= MAX_WAITING {
            return false;
        }
        self.requests.push_back(request);
        true
    }

    pub(super) fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Takes them all, oldest first.
    pub(super) fn take(&mut self) -> Vec<PendingRetry> {
        self.requests.drain(..).collect()
    }
}

/// Most retry requests waiting; later ones are dropped.
const MAX_WAITING: usize = 100;

/// Retry requests held back after the server rate limited one: none go out
/// before `until`, and those asked for meanwhile wait.
#[derive(Default)]
pub(super) struct Paused {
    pub until: Option<Timestamp>,
    pub waiting: Waiting,
}

impl Paused {
    pub(super) fn at(&self, now: Timestamp) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// Pauses until `until`, unless paused longer already; returns whether
    /// it did.
    pub(super) fn extend(&mut self, until: Timestamp) -> bool {
        if self.until.is_some_and(|paused| paused >= until) {
            return false;
        }
        self.until = Some(until);
        true
    }

    /// Queues `request` to send once the pause ends.
    pub(super) fn wait(&mut self, request: PendingRetry) {
        if self.waiting.push(request) {
            tracing::debug!("retry request waits while rate limited");
        } else {
            tracing::info!("retry request dropped: waiting already, or too many waiting");
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn account(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    #[test]
    fn allows_each_message_once_per_device() {
        let mut limiter = Limiter::new(HOUR, 10);
        let now = Instant::now();

        assert!(limiter.allow(account(1), 1, at(10), now));
        assert!(!limiter.allow(account(1), 1, at(10), now));
        assert!(limiter.allow(account(1), 2, at(10), now), "other device");
        assert!(limiter.allow(account(2), 1, at(10), now), "other account");
        assert!(limiter.allow(account(1), 1, at(11), now), "other message");
    }

    #[test]
    fn limits_requests_per_device_within_the_window() {
        let mut limiter = Limiter::new(HOUR, 2);
        let now = Instant::now();

        assert!(limiter.allow(account(1), 1, at(1), now));
        assert!(limiter.allow(account(1), 1, at(2), now));
        assert!(!limiter.allow(account(1), 1, at(3), now));
        assert!(limiter.allow(account(1), 2, at(3), now));

        let later = now + HOUR;
        assert!(limiter.allow(account(1), 1, at(3), later));
        assert!(limiter.allow(account(1), 1, at(1), later), "forgotten");
    }

    fn pending(sender: u128, device: u32, sent_at: u64) -> PendingRetry {
        PendingRetry {
            sender: account(sender),
            device,
            sent_at: at(sent_at),
            retry: RetryRequest::new(vec![sent_at as u8], None),
        }
    }

    #[test]
    fn waiting_requests_are_kept_once_in_order() {
        let mut waiting = Waiting::default();

        assert!(waiting.push(pending(1, 1, 10)));
        assert!(waiting.push(pending(2, 1, 5)));
        assert!(!waiting.push(pending(1, 1, 10)), "queued already");
        assert!(waiting.push(pending(1, 2, 10)), "other device");

        assert_eq!(
            waiting.take(),
            [pending(1, 1, 10), pending(2, 1, 5), pending(1, 2, 10)]
        );
        assert!(waiting.is_empty());
        assert!(waiting.push(pending(1, 1, 10)), "taken");
    }

    #[test]
    fn waiting_requests_are_capped() {
        let mut waiting = Waiting::default();
        for n in 0..MAX_WAITING as u64 {
            assert!(waiting.push(pending(1, 1, n)));
        }

        assert!(!waiting.push(pending(2, 1, 1)));
        assert_eq!(waiting.take().len(), MAX_WAITING);
    }

    #[test]
    fn pauses_extend_only() {
        let mut paused = Paused::default();
        assert!(!paused.at(at(0)));

        assert!(paused.extend(at(100)));
        assert!(!paused.extend(at(50)), "shorter");
        assert!(paused.at(at(99)));
        assert!(!paused.at(at(100)));
        assert!(paused.extend(at(200)));
        assert!(paused.at(at(150)));
    }

    #[test]
    fn drops_idle_devices() {
        let mut limiter = Limiter::new(HOUR, 1);
        let now = Instant::now();
        for n in 0..=MAX_TRACKED as u128 {
            limiter.allow(account(n), 1, at(1), now);
        }

        limiter.allow(account(0), 2, at(1), now + HOUR);

        assert_eq!(limiter.recent.len(), 1);
    }
}
