//! Fake Signal backend and fixtures for Copiale tests.
//!
//! Dev-dependency only; tests never reach real Signal servers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use copiale_core::storage::{Database, SecretError, SecretStore};
use copiale_core::{
    BackendError, Incoming, Linker, MessageBody, ServiceId, SignalBackend, Timestamp,
};
use futures::StreamExt as _;
use futures::stream::LocalBoxStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use uuid::Uuid;
use zeroize::Zeroizing;

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

/// In-memory [`SecretStore`] standing in for the OS keychain.
///
/// Clones share state, so a test can keep one to inspect or reconfigure.
#[derive(Clone, Default)]
pub struct MemorySecretStore {
    state: Arc<Mutex<SecretState>>,
}

#[derive(Default)]
struct SecretState {
    secret: Option<Vec<u8>>,
    unavailable: bool,
}

impl MemorySecretStore {
    /// A store that fails like a missing or locked keychain.
    pub fn unavailable() -> Self {
        let store = Self::default();
        store.set_available(false);
        store
    }

    pub fn set_available(&self, available: bool) {
        self.state.lock().unwrap().unavailable = !available;
    }

    /// The stored secret, regardless of availability.
    pub fn secret(&self) -> Option<Vec<u8>> {
        self.state.lock().unwrap().secret.clone()
    }

    pub fn put_secret(&self, secret: &[u8]) {
        self.state.lock().unwrap().secret = Some(secret.to_vec());
    }

    pub fn clear_secret(&self) {
        self.state.lock().unwrap().secret = None;
    }

    fn check_available(state: &SecretState) -> Result<(), SecretError> {
        if state.unavailable {
            return Err(SecretError::Unavailable("no keychain".into()));
        }
        Ok(())
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
        let state = self.state.lock().unwrap();
        Self::check_available(&state)?;
        Ok(state.secret.clone().map(Zeroizing::new))
    }

    fn set(&self, secret: &[u8]) -> Result<(), SecretError> {
        let mut state = self.state.lock().unwrap();
        Self::check_available(&state)?;
        state.secret = Some(secret.to_vec());
        Ok(())
    }

    fn delete(&self) -> Result<(), SecretError> {
        let mut state = self.state.lock().unwrap();
        Self::check_available(&state)?;
        state.secret = None;
        Ok(())
    }
}

/// Provisioning URL the [`FakeLinker`] hands out.
pub const FAKE_LINK_URL: &str = "sgnl://linkdevice?uuid=fake&pub_key=fake";

/// Creates a fake linker that yields `backend`, and the phone that drives it.
pub fn fake_linker(backend: FakeBackend) -> (FakeLinker, FakePhone) {
    let (approvals_tx, approvals_rx) = unbounded_channel();
    let state = Arc::new(Mutex::new(PhoneState::default()));
    let linker = FakeLinker {
        backend: Some(backend),
        approvals: approvals_rx,
        state: state.clone(),
    };
    let phone = FakePhone {
        approvals: approvals_tx,
        state,
    };
    (linker, phone)
}

#[derive(Default)]
struct PhoneState {
    linked: bool,
    device_name: Option<String>,
    load_failure: Option<BackendError>,
}

/// [`Linker`] driven by a [`FakePhone`]. Hands out its backend once.
pub struct FakeLinker {
    backend: Option<FakeBackend>,
    approvals: UnboundedReceiver<Result<(), BackendError>>,
    state: Arc<Mutex<PhoneState>>,
}

impl Linker for FakeLinker {
    type Backend = FakeBackend;

    async fn load(&mut self, _db: &Database) -> Result<Option<FakeBackend>, BackendError> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.load_failure.take() {
            return Err(error);
        }
        Ok(if state.linked {
            self.backend.take()
        } else {
            None
        })
    }

    async fn link(
        &mut self,
        _db: &Database,
        device_name: &str,
        url: oneshot::Sender<String>,
    ) -> Result<FakeBackend, BackendError> {
        self.state.lock().unwrap().device_name = Some(device_name.into());
        url.send(FAKE_LINK_URL.into()).ok();
        match self.approvals.recv().await {
            Some(Ok(())) => {
                self.state.lock().unwrap().linked = true;
                self.backend
                    .take()
                    .ok_or_else(|| BackendError::new("already linked"))
            }
            Some(Err(error)) => Err(error),
            None => Err(BackendError::new("phone gone")),
        }
    }
}

/// The user's phone: confirms or rejects linking.
pub struct FakePhone {
    approvals: UnboundedSender<Result<(), BackendError>>,
    state: Arc<Mutex<PhoneState>>,
}

impl FakePhone {
    /// Marks the account as linked already, as on a later start.
    pub fn set_linked(&self) {
        self.state.lock().unwrap().linked = true;
    }

    pub fn is_linked(&self) -> bool {
        self.state.lock().unwrap().linked
    }

    /// Device name of the last link attempt.
    pub fn device_name(&self) -> Option<String> {
        self.state.lock().unwrap().device_name.clone()
    }

    /// Confirms the pending link, as if the QR code was scanned.
    pub fn approve(&self) {
        self.approvals.send(Ok(())).ok();
    }

    /// Fails the pending link.
    pub fn reject(&self, error: BackendError) {
        self.approvals.send(Err(error)).ok();
    }

    /// Makes the next account load fail.
    pub fn fail_load(&self, error: BackendError) {
        self.state.lock().unwrap().load_failure = Some(error);
    }
}
