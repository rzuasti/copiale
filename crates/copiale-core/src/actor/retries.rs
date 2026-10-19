//! Limits retry requests per device, both ways: a message is asked for, or
//! resent, once, so two devices that keep failing don't loop.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use tokio::time::Instant;

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
