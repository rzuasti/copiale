//! Outgoing messages and changes to them, sent in the background one at a
//! time while connected.
//!
//! One at a time keeps them in order and stops concurrent sends from racing
//! on shared session state.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::backend::{BackendError, SignalBackend};
use crate::model::{Destination, MessageId, Outgoing, Timestamp};

pub(super) struct Outbox {
    queue: VecDeque<Job>,
    sending: Option<Sending>,
    connected: bool,
    /// How long a message may wait before it fails.
    timeout: Duration,
}

struct Job {
    /// The stored message sent, or changed.
    id: MessageId,
    destination: Destination,
    outgoing: Outgoing,
    /// The send's own timestamp.
    sent_at: Timestamp,
    deadline: Instant,
}

impl Job {
    fn change(&self) -> bool {
        !matches!(self.outgoing, Outgoing::Text { .. })
    }
}

struct Sending {
    outgoing: Job,
    task: JoinHandle<Result<(), BackendError>>,
}

/// How a queued send left the outbox, or why sending stopped. `change` is
/// set for edits and deletes, rather than new messages.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Sent {
        id: MessageId,
        change: bool,
    },
    Failed {
        id: MessageId,
        change: bool,
        error: BackendError,
    },
    /// The connection failed mid-send: the message is queued again and
    /// sending waits for [`Outbox::connected`].
    Interrupted(BackendError),
}

impl Outbox {
    pub(super) fn new(timeout: Duration) -> Self {
        Self {
            queue: VecDeque::new(),
            sending: None,
            connected: false,
            timeout,
        }
    }

    /// Queues a send for stored message `id`, sent at `sent_at`, and starts
    /// sending if idle. Must run inside a `LocalSet`.
    pub(super) fn push<B: SignalBackend>(
        &mut self,
        id: MessageId,
        destination: Destination,
        outgoing: Outgoing,
        sent_at: Timestamp,
        backend: &mut B,
    ) {
        self.queue.push_back(Job {
            id,
            destination,
            outgoing,
            sent_at,
            deadline: Instant::now() + self.timeout,
        });
        self.start_next(backend);
    }

    /// Starts sending.
    pub(super) fn connected<B: SignalBackend>(&mut self, backend: &mut B) {
        self.connected = true;
        self.start_next(backend);
    }

    /// Stops sending until connected again. A send in flight is queued
    /// again; resending with the same timestamp lets recipients drop a copy
    /// that got through.
    pub(super) fn disconnected(&mut self) {
        self.connected = false;
        if let Some(sending) = self.sending.take() {
            sending.task.abort();
            self.queue.push_front(sending.outgoing);
        }
    }

    /// The next finished send or expired message; never resolves while
    /// there is neither. Call [`Self::start_next`] afterwards.
    pub(super) async fn finished(&mut self) -> Outcome {
        let deadline = self
            .sending
            .iter()
            .map(|sending| &sending.outgoing)
            .chain(&self.queue)
            .map(|outgoing| outgoing.deadline)
            .min();
        let busy = self.sending.is_some();
        let task = self.sending.as_mut().map(|sending| &mut sending.task);
        tokio::select! {
            result = async { task?.await.ok() }, if busy => {
                let sending = self.sending.take().expect("sending");
                let (id, change) = (sending.outgoing.id, sending.outgoing.change());
                match result {
                    Some(Ok(())) => Outcome::Sent { id, change },
                    Some(Err(error)) if error.is_network() => {
                        self.connected = false;
                        self.queue.push_front(sending.outgoing);
                        Outcome::Interrupted(error)
                    }
                    Some(Err(error)) => Outcome::Failed { id, change, error },
                    // Only a panic fails the join; the panic hook has logged it.
                    None => Outcome::Failed { id, change, error: BackendError::new("sending crashed") },
                }
            }
            () = sleep_until(deadline) => {
                let job = self.expire(deadline.expect("deadline"));
                Outcome::Failed {
                    id: job.id,
                    change: job.change(),
                    error: BackendError::new("timed out"),
                }
            }
        }
    }

    /// Starts the next queued send if connected and idle.
    pub(super) fn start_next<B: SignalBackend>(&mut self, backend: &mut B) {
        if !self.connected || self.sending.is_some() {
            return;
        }
        let Some(next) = self.queue.pop_front() else {
            return;
        };
        let send = backend.send(next.destination, next.outgoing.clone(), next.sent_at);
        self.sending = Some(Sending {
            outgoing: next,
            task: tokio::task::spawn_local(send),
        });
    }

    /// Drops the send due at `deadline`.
    fn expire(&mut self, deadline: Instant) -> Job {
        if let Some(sending) = self
            .sending
            .take_if(|sending| sending.outgoing.deadline == deadline)
        {
            sending.task.abort();
            return sending.outgoing;
        }
        let index = self
            .queue
            .iter()
            .position(|outgoing| outgoing.deadline == deadline)
            .expect("deadline from the queue");
        self.queue.remove(index).expect("index in range")
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

impl Drop for Outbox {
    /// Stops the running send; its message stays pending until the next
    /// start marks it failed.
    fn drop(&mut self) {
        if let Some(sending) = &self.sending {
            sending.task.abort();
        }
    }
}
