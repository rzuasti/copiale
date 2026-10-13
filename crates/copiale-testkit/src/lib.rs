//! Fake Signal backend and fixtures for Copiale tests.
//!
//! Dev-dependency only; tests never reach real Signal servers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use copiale_core::storage::{Database, SecretError, SecretStore};
use copiale_core::{
    BackendError, Incoming, Linker, MessageBody, ServiceId, SignalBackend, Timestamp,
};
use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use futures::{FutureExt as _, StreamExt as _};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{oneshot, watch};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Deterministic service ID for fixtures.
pub fn service_id(n: u128) -> ServiceId {
    ServiceId::new(Uuid::from_u128(n))
}

/// Creates a fake backend and the network controller that scripts it.
pub fn fake_backend() -> (FakeBackend, FakeNetwork) {
    let state = Arc::new(Mutex::new(State::default()));
    let (link, _) = watch::channel(Link::default());
    let link = Arc::new(link);
    let backend = FakeBackend {
        state: state.clone(),
        link: link.clone(),
    };
    let network = FakeNetwork { state, link };
    (backend, network)
}

/// A text message the backend was asked to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentText {
    pub recipient: ServiceId,
    pub body: MessageBody,
    pub timestamp: Timestamp,
}

#[derive(Clone, Copy, Default)]
struct Link {
    sends_paused: bool,
    offline: bool,
    connections_stalled: bool,
}

#[derive(Default)]
struct State {
    /// The open connection, if any.
    connection: Option<UnboundedSender<Incoming>>,
    connections: usize,
    receive_failures: VecDeque<BackendError>,
    sent: Vec<SentText>,
    sends_started: usize,
    send_failures: VecDeque<BackendError>,
    contact_sync_requests: usize,
}

/// [`SignalBackend`] driven by a [`FakeNetwork`]. Each `receive` opens a
/// new connection, replacing any previous one.
pub struct FakeBackend {
    state: Arc<Mutex<State>>,
    link: Arc<watch::Sender<Link>>,
}

impl SignalBackend for FakeBackend {
    /// Opens once connections aren't stalled.
    fn receive(
        &mut self,
    ) -> LocalBoxFuture<'static, Result<LocalBoxStream<'static, Incoming>, BackendError>> {
        let state = self.state.clone();
        let mut link = self.link.subscribe();
        async move {
            link.wait_for(|link| !link.connections_stalled).await.ok();
            if link.borrow().offline {
                return Err(BackendError::network("offline"));
            }
            let mut state = state.lock().unwrap();
            if let Some(error) = state.receive_failures.pop_front() {
                return Err(error);
            }
            let (tx, rx) = unbounded_channel();
            state.connection = Some(tx);
            state.connections += 1;
            let stream = futures::stream::unfold(rx, |mut rx| async move {
                let item = rx.recv().await?;
                Some((item, rx))
            });
            Ok(stream.boxed_local())
        }
        .boxed_local()
    }

    /// Counts as started at once; completes when sends aren't paused and
    /// the network is up.
    fn send_text(
        &mut self,
        recipient: ServiceId,
        body: MessageBody,
        timestamp: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let failure = {
            let mut state = self.state.lock().unwrap();
            state.sends_started += 1;
            state.send_failures.pop_front()
        };
        let state = self.state.clone();
        let mut link = self.link.subscribe();
        async move {
            // Fails only once the network is gone; then nothing pauses.
            link.wait_for(|link| !link.sends_paused && !link.offline)
                .await
                .ok();
            if let Some(error) = failure {
                return Err(error);
            }
            state.lock().unwrap().sent.push(SentText {
                recipient,
                body,
                timestamp,
            });
            Ok(())
        }
        .boxed_local()
    }

    /// Answers unless the network is down.
    fn probe(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut link = self.link.subscribe();
        async move {
            link.wait_for(|link| !link.offline).await.ok();
            Ok(())
        }
        .boxed_local()
    }

    async fn request_contact_sync(&mut self) -> Result<(), BackendError> {
        self.state.lock().unwrap().contact_sync_requests += 1;
        Ok(())
    }
}

/// Scripts what the [`FakeBackend`] receives and how sends behave.
pub struct FakeNetwork {
    state: Arc<Mutex<State>>,
    link: Arc<watch::Sender<Link>>,
}

impl FakeNetwork {
    /// Delivers content on the open connection.
    ///
    /// Panics if there is none; wait for `Event::Connected` first.
    pub fn deliver(&self, incoming: Incoming) {
        self.state
            .lock()
            .unwrap()
            .connection
            .as_ref()
            .expect("not connected")
            .send(incoming)
            .expect("backend dropped the connection");
    }

    /// Closes the open connection, as if the network dropped it.
    pub fn disconnect(&self) {
        self.state.lock().unwrap().connection = None;
    }

    /// Makes the next connection attempt fail; queues up for repeated calls.
    pub fn fail_receive(&self, error: BackendError) {
        self.state.lock().unwrap().receive_failures.push_back(error);
    }

    /// Makes the next send fail; queues up for repeated calls.
    pub fn fail_next_send(&self, error: BackendError) {
        self.state.lock().unwrap().send_failures.push_back(error);
    }

    /// Holds sends in flight until [`Self::resume_sends`], like a slow
    /// network.
    pub fn pause_sends(&self) {
        self.link.send_modify(|link| link.sends_paused = true);
    }

    pub fn resume_sends(&self) {
        self.link.send_modify(|link| link.sends_paused = false);
    }

    /// Drops all traffic, like a lost network: the open connection stays
    /// open but probes and sends hang, and new connections fail.
    pub fn go_offline(&self) {
        self.link.send_modify(|link| link.offline = true);
    }

    pub fn go_online(&self) {
        self.link.send_modify(|link| link.offline = false);
    }

    /// Holds connection attempts until called with `false`.
    pub fn stall_connections(&self, stall: bool) {
        self.link
            .send_modify(|link| link.connections_stalled = stall);
    }

    /// Texts sent successfully so far, oldest first.
    pub fn sent(&self) -> Vec<SentText> {
        self.state.lock().unwrap().sent.clone()
    }

    /// Sends started so far, including failed and paused ones.
    pub fn sends_started(&self) -> usize {
        self.state.lock().unwrap().sends_started
    }

    /// Connections opened so far.
    pub fn connections(&self) -> usize {
        self.state.lock().unwrap().connections
    }

    pub fn contact_sync_requests(&self) -> usize {
        self.state.lock().unwrap().contact_sync_requests
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
