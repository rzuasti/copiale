//! Connection checks: a dead network can leave the connection looking open
//! for minutes, so the core asks the server something every so often.

use std::time::Duration;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use tokio::time::Instant;

use crate::backend::{BackendError, SignalBackend};

pub(super) struct Probe {
    interval: Duration,
    timeout: Duration,
    state: State,
}

enum State {
    Stopped,
    Waiting(Instant),
    Running(LocalBoxFuture<'static, Result<(), BackendError>>),
}

pub(super) enum Check {
    /// Time to probe; call [`Probe::run`].
    Due,
    Failed(BackendError),
}

impl Probe {
    pub(super) fn new(interval: Duration, timeout: Duration) -> Self {
        Self {
            interval,
            timeout,
            state: State::Stopped,
        }
    }

    /// Schedules probes, after connecting.
    pub(super) fn start(&mut self) {
        self.state = State::Waiting(Instant::now() + self.interval);
    }

    pub(super) fn stop(&mut self) {
        self.state = State::Stopped;
    }

    pub(super) fn run<B: SignalBackend>(&mut self, backend: &mut B) {
        let timeout = self.timeout;
        let probe = tokio::time::timeout(timeout, backend.probe()).map(|result| {
            result.unwrap_or_else(|_| Err(BackendError::network("no answer from the server")))
        });
        self.state = State::Running(probe.boxed_local());
    }

    /// The next step; never resolves while stopped. Successful probes
    /// schedule the next one.
    pub(super) async fn next(&mut self) -> Check {
        loop {
            match &mut self.state {
                State::Stopped => return std::future::pending().await,
                State::Waiting(at) => {
                    tokio::time::sleep_until(*at).await;
                    return Check::Due;
                }
                State::Running(probe) => match probe.await {
                    Ok(()) => self.start(),
                    Err(error) => {
                        self.stop();
                        return Check::Failed(error);
                    }
                },
            }
        }
    }
}
