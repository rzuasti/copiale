//! Outgoing messages, sent in the background one at a time while connected.
//!
//! One at a time keeps them in order and stops concurrent sends from racing
//! on shared session state.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::backend::{BackendError, SignalBackend};
use crate::model::{Destination, Message, MessageBody, MessageId, Timestamp};

pub(super) struct Outbox {
    queue: VecDeque<Outgoing>,
    sending: Option<Sending>,
    connected: bool,
    /// How long a message may wait before it fails.
    timeout: Duration,
}

struct Outgoing {
    id: MessageId,
    destination: Destination,
    body: MessageBody,
    sent_at: Timestamp,
    deadline: Instant,
}

struct Sending {
    outgoing: Outgoing,
    task: JoinHandle<Result<(), BackendError>>,
}

/// How a queued message left the outbox, or why sending stopped.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Sent(MessageId),
    Failed(MessageId, BackendError),
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

    /// Queues a stored message and starts sending if idle. Must run inside
    /// a `LocalSet`.
    pub(super) fn push<B: SignalBackend>(
        &mut self,
        destination: Destination,
        message: &Message,
        backend: &mut B,
    ) {
        self.queue.push_back(Outgoing {
            id: message.id,
            destination,
            body: message.body.clone(),
            sent_at: message.sent_at,
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
                match result {
                    Some(Ok(())) => Outcome::Sent(sending.outgoing.id),
                    Some(Err(error)) if error.is_network() => {
                        self.connected = false;
                        self.queue.push_front(sending.outgoing);
                        Outcome::Interrupted(error)
                    }
                    Some(Err(error)) => Outcome::Failed(sending.outgoing.id, error),
                    // Only a panic fails the join; the panic hook has logged it.
                    None => Outcome::Failed(sending.outgoing.id, BackendError::new("sending crashed")),
                }
            }
            () = sleep_until(deadline) => {
                let id = self.expire(deadline.expect("deadline"));
                Outcome::Failed(id, BackendError::new("timed out"))
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
        let send = backend.send_text(next.destination, next.body.clone(), next.sent_at);
        self.sending = Some(Sending {
            outgoing: next,
            task: tokio::task::spawn_local(send),
        });
    }

    /// Drops the message due at `deadline`.
    fn expire(&mut self, deadline: Instant) -> MessageId {
        if let Some(sending) = self
            .sending
            .take_if(|sending| sending.outgoing.deadline == deadline)
        {
            sending.task.abort();
            return sending.outgoing.id;
        }
        let index = self
            .queue
            .iter()
            .position(|outgoing| outgoing.deadline == deadline)
            .expect("deadline from the queue");
        self.queue.remove(index).expect("index in range").id
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
