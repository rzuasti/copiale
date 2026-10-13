//! Fake Signal backend and fixtures for Copiale tests.
//!
//! Dev-dependency only; tests never reach real Signal servers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use copiale_core::{BackendError, Incoming, MessageBody, ServiceId, SignalBackend, Timestamp};
use futures::StreamExt as _;
use futures::stream::LocalBoxStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use uuid::Uuid;

/// Deterministic service ID for fixtures.
pub fn service_id(n: u128) -> ServiceId {
    ServiceId::new(Uuid::from_u128(n))
}

/// Creates a fake backend and the network controller that scripts it.
pub fn fake_backend() -> (FakeBackend, FakeNetwork) {
    let (incoming_tx, incoming_rx) = unbounded_channel();
    let state = Arc::new(Mutex::new(State::default()));
    let backend = FakeBackend {
        incoming: Some(incoming_rx),
        state: state.clone(),
    };
    let network = FakeNetwork {
        incoming: Some(incoming_tx),
        state,
    };
    (backend, network)
}

/// A text message the backend was asked to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentText {
    pub recipient: ServiceId,
    pub body: MessageBody,
    pub timestamp: Timestamp,
}

#[derive(Default)]
struct State {
    sent: Vec<SentText>,
    send_failures: VecDeque<BackendError>,
    receive_failure: Option<BackendError>,
}

/// [`SignalBackend`] driven by a [`FakeNetwork`].
pub struct FakeBackend {
    incoming: Option<UnboundedReceiver<Incoming>>,
    state: Arc<Mutex<State>>,
}

impl SignalBackend for FakeBackend {
    async fn receive(&mut self) -> Result<LocalBoxStream<'static, Incoming>, BackendError> {
        if let Some(error) = self.state.lock().unwrap().receive_failure.take() {
            return Err(error);
        }
        let incoming = self
            .incoming
            .take()
            .ok_or_else(|| BackendError::new("already receiving"))?;
        let stream = futures::stream::unfold(incoming, |mut incoming| async move {
            let item = incoming.recv().await?;
            Some((item, incoming))
        });
        Ok(stream.boxed_local())
    }

    async fn send_text(
        &mut self,
        recipient: ServiceId,
        body: &MessageBody,
        timestamp: Timestamp,
    ) -> Result<(), BackendError> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.send_failures.pop_front() {
            return Err(error);
        }
        state.sent.push(SentText {
            recipient,
            body: body.clone(),
            timestamp,
        });
        Ok(())
    }
}

/// Scripts what the [`FakeBackend`] receives and how sends behave.
pub struct FakeNetwork {
    incoming: Option<UnboundedSender<Incoming>>,
    state: Arc<Mutex<State>>,
}

impl FakeNetwork {
    /// Delivers content as if it arrived from the server.
    ///
    /// Panics after [`Self::disconnect`].
    pub fn deliver(&self, incoming: Incoming) {
        self.incoming
            .as_ref()
            .expect("network disconnected")
            .send(incoming)
            .expect("backend dropped");
    }

    /// Closes the incoming stream, as if the connection dropped.
    pub fn disconnect(&mut self) {
        self.incoming = None;
    }

    /// Makes opening the connection fail. Set before the core starts.
    pub fn fail_receive(&self, error: BackendError) {
        self.state.lock().unwrap().receive_failure = Some(error);
    }

    /// Makes the next send fail; queues up for repeated calls.
    pub fn fail_next_send(&self, error: BackendError) {
        self.state.lock().unwrap().send_failures.push_back(error);
    }

    /// Texts sent so far, oldest first.
    pub fn sent(&self) -> Vec<SentText> {
        self.state.lock().unwrap().sent.clone()
    }
}
