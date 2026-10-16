//! Wall-clock time for disappearing messages: the system's, or a test's.

use std::fmt;
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;

use crate::model::Timestamp;

pub trait Clock: fmt::Debug + Send + Sync {
    fn now(&self) -> Timestamp;

    /// Resolves at `at` or earlier; check [`Self::now`] again after.
    fn sleep_until(&self, at: Timestamp) -> LocalBoxFuture<'static, ()>;
}

/// The system clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

/// Longest single sleep: timers may not count time suspended, so the wall
/// clock is checked again at least this often.
const MAX_SLEEP: Duration = Duration::from_secs(60);

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn sleep_until(&self, at: Timestamp) -> LocalBoxFuture<'static, ()> {
        let millis = at.as_millis().saturating_sub(Timestamp::now().as_millis());
        tokio::time::sleep(Duration::from_millis(millis).min(MAX_SLEEP)).boxed_local()
    }
}
