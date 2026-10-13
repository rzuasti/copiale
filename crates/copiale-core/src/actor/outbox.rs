//! Outgoing messages, sent in the background one at a time.
//!
//! One at a time keeps them in order and stops concurrent sends from racing
//! on shared session state.

use std::collections::VecDeque;

use tokio::task::JoinHandle;

use crate::backend::{BackendError, SignalBackend};
use crate::model::{Message, MessageBody, MessageId, ServiceId, Timestamp};

#[derive(Default)]
pub(super) struct Outbox {
    queue: VecDeque<Outgoing>,
    sending: Option<Sending>,
}

struct Outgoing {
    id: MessageId,
    recipient: ServiceId,
    body: MessageBody,
    sent_at: Timestamp,
}

struct Sending {
    id: MessageId,
    task: JoinHandle<Result<(), BackendError>>,
}

impl Outbox {
    /// Queues a stored message and starts sending if idle. Must run inside
    /// a `LocalSet`.
    pub(super) fn push<B: SignalBackend>(
        &mut self,
        recipient: ServiceId,
        message: &Message,
        backend: &mut B,
    ) {
        self.queue.push_back(Outgoing {
            id: message.id,
            recipient,
            body: message.body.clone(),
            sent_at: message.sent_at,
        });
        self.start_next(backend);
    }

    /// The finished send's message and result; never resolves while idle.
    /// Call [`Self::start_next`] afterwards.
    pub(super) async fn finished(&mut self) -> (MessageId, Result<(), BackendError>) {
        let Some(sending) = &mut self.sending else {
            return std::future::pending().await;
        };
        // Only a panic fails the join; the panic hook has logged it.
        let result = (&mut sending.task)
            .await
            .unwrap_or_else(|_| Err(BackendError::new("sending crashed")));
        let id = sending.id;
        self.sending = None;
        (id, result)
    }

    /// Starts the next queued send unless one is running.
    pub(super) fn start_next<B: SignalBackend>(&mut self, backend: &mut B) {
        if self.sending.is_some() {
            return;
        }
        let Some(next) = self.queue.pop_front() else {
            return;
        };
        let send = backend.send_text(next.recipient, next.body, next.sent_at);
        self.sending = Some(Sending {
            id: next.id,
            task: tokio::task::spawn_local(send),
        });
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
