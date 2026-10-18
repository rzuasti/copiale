//! Fake Signal backend and fixtures for Copiale tests.
//!
//! Dev-dependency only; tests never reach real Signal servers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use copiale_core::storage::{Database, SecretError, SecretStore};
use copiale_core::{
    BackendError, Clock, Contact, Destination, GroupInfo, GroupMasterKey, Incoming, Linker,
    MessageBody, Outgoing, ServiceId, SignalBackend, Timestamp,
};
use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use futures::{FutureExt as _, StreamExt as _};
use presage::libsignal_service::prelude::phonenumber;
use presage::model::contacts::Contact as StoredContact;
use presage::store::ContentsStore as _;
use presage_store_sqlite::SqliteStore;
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
    let changes = Arc::new(watch::Sender::new(0));
    let backend = FakeBackend {
        state: state.clone(),
        link: link.clone(),
        changes: changes.clone(),
    };
    let network = FakeNetwork {
        state,
        link,
        changes,
    };
    (backend, network)
}

/// The fake account's own ID.
pub const OWN_ID: u128 = 0x5e1f;

/// The device name of an account linked before the core started.
pub const DEVICE_NAME: &str = "Copiale on testbox";

/// Deterministic group master key for fixtures.
pub fn group_key(n: u8) -> GroupMasterKey {
    GroupMasterKey::new([n; 32])
}

/// A message, or change to one, the backend was asked to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentText {
    pub destination: Destination,
    pub outgoing: Outgoing,
    pub timestamp: Timestamp,
}

/// Plain text, as sent.
pub fn text(body: &str) -> Outgoing {
    Outgoing::Text {
        body: MessageBody::new(body),
        quote: None,
    }
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
    /// [`SignalBackend::stop_receiving`] leaves the connection open.
    ignore_stops: bool,
    connections: usize,
    receive_failures: VecDeque<BackendError>,
    sent: Vec<SentText>,
    sends_started: usize,
    send_failures: VecDeque<BackendError>,
    contact_sync_requests: usize,
    groups: HashMap<GroupMasterKey, GroupInfo>,
    /// Profile names, known once fetched.
    profile_names: HashMap<ServiceId, String>,
    profiles_fetched: HashSet<ServiceId>,
    profile_fetches: usize,
    settings_sync_requests: usize,
    read_receipts: Vec<(ServiceId, Vec<Timestamp>)>,
    read_syncs: Vec<Vec<(ServiceId, Timestamp)>>,
    typing: Vec<(Destination, bool)>,
    deleted_for_me: Vec<DeletedForMe>,
    /// presage's store in the core's database, once the account is loaded.
    store: Option<SqliteStore>,
    /// Disappearing messages timers, by conversation.
    expire_timers: Vec<(Destination, u32)>,
    /// Set when linked; [`DEVICE_NAME`] for an account linked before.
    device_name: Option<String>,
}

type DeletedForMe = (Destination, Vec<(Option<ServiceId>, Timestamp)>);

/// [`SignalBackend`] driven by a [`FakeNetwork`]. Each `receive` opens a
/// new connection, replacing any previous one.
pub struct FakeBackend {
    state: Arc<Mutex<State>>,
    link: Arc<watch::Sender<Link>>,
    /// Counts what the backend recorded; see [`FakeNetwork::wait_until`].
    changes: Arc<watch::Sender<u64>>,
}

impl FakeBackend {
    /// Changes the state, waking [`FakeNetwork::wait_until`].
    fn record<R>(&self, change: impl FnOnce(&mut State) -> R) -> R {
        record(&self.state, &self.changes, change)
    }

    /// A request to the phone: `count` once the network is up.
    fn sync_request(
        &self,
        count: impl FnOnce(&mut State) + 'static,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let state = self.state.clone();
        let changes = self.changes.clone();
        let mut link = self.link.subscribe();
        async move {
            link.wait_for(|link| !link.offline).await.ok();
            record(&state, &changes, count);
            Ok(())
        }
        .boxed_local()
    }
}

fn record<R>(
    state: &Mutex<State>,
    changes: &watch::Sender<u64>,
    change: impl FnOnce(&mut State) -> R,
) -> R {
    let result = change(&mut state.lock().unwrap());
    changes.send_modify(|count| *count += 1);
    result
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

    /// Ends the connection after what was delivered on it.
    fn stop_receiving(&mut self) {
        let mut state = self.state.lock().unwrap();
        if !state.ignore_stops {
            state.connection = None;
        }
    }

    /// Counts as started at once; completes when sends aren't paused and
    /// the network is up.
    fn send(
        &mut self,
        destination: Destination,
        outgoing: Outgoing,
        timestamp: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let failure = self.record(|state| {
            state.sends_started += 1;
            state.send_failures.pop_front()
        });
        let state = self.state.clone();
        let changes = self.changes.clone();
        let mut link = self.link.subscribe();
        async move {
            // Fails only once the network is gone; then nothing pauses.
            link.wait_for(|link| !link.sends_paused && !link.offline)
                .await
                .ok();
            if let Some(error) = failure {
                return Err(error);
            }
            record(&state, &changes, |state| {
                state.sent.push(SentText {
                    destination,
                    outgoing,
                    timestamp,
                })
            });
            Ok(())
        }
        .boxed_local()
    }

    /// The scripted timer.
    fn expire_timer(
        &mut self,
        destination: Destination,
    ) -> LocalBoxFuture<'static, Result<Option<u32>, BackendError>> {
        let timer = self
            .state
            .lock()
            .unwrap()
            .expire_timers
            .iter()
            .find(|(known, _)| same_conversation(known, &destination))
            .map(|(_, seconds)| *seconds);
        async move { Ok(timer) }.boxed_local()
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

    /// Counted once sent: hangs while the network is down.
    fn request_contact_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.sync_request(|state| state.contact_sync_requests += 1)
    }

    fn own_id(&self) -> ServiceId {
        service_id(OWN_ID)
    }

    fn device_name(&self) -> Option<String> {
        let name = self.state.lock().unwrap().device_name.clone();
        Some(name.unwrap_or_else(|| DEVICE_NAME.into()))
    }

    /// Counted once sent: hangs while the network is down.
    fn request_settings_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.sync_request(|state| state.settings_sync_requests += 1)
    }

    fn send_read_receipts(
        &mut self,
        recipient: ServiceId,
        timestamps: Vec<Timestamp>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.read_receipts.push((recipient, timestamps)));
        async { Ok(()) }.boxed_local()
    }

    fn send_read_sync(
        &mut self,
        read: Vec<(ServiceId, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.read_syncs.push(read));
        async { Ok(()) }.boxed_local()
    }

    fn sync_deleted_for_me(
        &mut self,
        destination: Destination,
        messages: Vec<(Option<ServiceId>, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.deleted_for_me.push((destination, messages)));
        async { Ok(()) }.boxed_local()
    }

    fn send_typing(
        &mut self,
        destination: Destination,
        typing: bool,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.typing.push((destination, typing)));
        async { Ok(()) }.boxed_local()
    }

    /// The scripted group, with fetched profile names filled in.
    fn group(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<Option<GroupInfo>, BackendError>> {
        let state = self.state.clone();
        async move {
            let state = state.lock().unwrap();
            let Some(mut info) = state.groups.get(&key).cloned() else {
                return Ok(None);
            };
            for member in &mut info.members {
                if member.name.is_none() && state.profiles_fetched.contains(&member.id) {
                    member.name = state.profile_names.get(&member.id).cloned();
                }
            }
            Ok(Some(info))
        }
        .boxed_local()
    }

    /// Fails while offline.
    fn fetch_member_profiles(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let (state, changes) = (self.state.clone(), self.changes.clone());
        let offline = self.link.borrow().offline;
        async move {
            if offline {
                return Err(BackendError::network("offline"));
            }
            let mut state = state.lock().unwrap();
            changes.send_modify(|count| *count += 1);
            let unnamed: Vec<_> = state
                .groups
                .get(&key)
                .into_iter()
                .flat_map(|info| &info.members)
                .filter(|member| member.name.is_none())
                .map(|member| member.id)
                .collect();
            for id in unnamed {
                if state.profiles_fetched.insert(id) {
                    state.profile_fetches += 1;
                }
            }
            Ok(())
        }
        .boxed_local()
    }
}

/// Scripts what the [`FakeBackend`] receives and how sends behave.
pub struct FakeNetwork {
    state: Arc<Mutex<State>>,
    link: Arc<watch::Sender<Link>>,
    changes: Arc<watch::Sender<u64>>,
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

    /// Keeps connections open when the core asks to stop receiving, like a
    /// backend that never finishes.
    pub fn ignore_stops(&self) {
        self.state.lock().unwrap().ignore_stops = true;
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

    /// Waits until `done` holds, checking whenever the backend records
    /// something. For what the core does after the events a test can await,
    /// such as sending an edit or syncing a delete.
    pub async fn wait_until(&self, done: impl Fn(&Self) -> bool) {
        let mut changes = self.changes.subscribe();
        while !done(self) {
            changes
                .changed()
                .await
                .expect("the network outlives its waits");
        }
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

    /// Sets the group state the backend reports; members' names are their
    /// contact names, if any.
    pub fn set_group(&self, key: GroupMasterKey, info: GroupInfo) {
        self.state.lock().unwrap().groups.insert(key, info);
    }

    /// Gives an account a profile name, known once fetched.
    pub fn set_profile_name(&self, id: ServiceId, name: &str) {
        self.state
            .lock()
            .unwrap()
            .profile_names
            .insert(id, name.into());
    }

    /// Profiles fetched so far.
    pub fn profile_fetches(&self) -> usize {
        self.state.lock().unwrap().profile_fetches
    }

    pub fn settings_sync_requests(&self) -> usize {
        self.state.lock().unwrap().settings_sync_requests
    }

    /// Read receipts sent so far, per recipient.
    pub fn read_receipts(&self) -> Vec<(ServiceId, Vec<Timestamp>)> {
        self.state.lock().unwrap().read_receipts.clone()
    }

    /// Reads synced to the account's other devices so far.
    pub fn read_syncs(&self) -> Vec<Vec<(ServiceId, Timestamp)>> {
        self.state.lock().unwrap().read_syncs.clone()
    }

    /// Deletes for this account synced to its other devices so far.
    pub fn deleted_for_me(&self) -> Vec<DeletedForMe> {
        self.state.lock().unwrap().deleted_for_me.clone()
    }

    /// Typing indicators sent so far.
    pub fn typing(&self) -> Vec<(Destination, bool)> {
        self.state.lock().unwrap().typing.clone()
    }

    /// Saves contacts to presage's store, as a sync from the phone does, and
    /// delivers [`Incoming::ContactsSynced`].
    ///
    /// Panics before the account is loaded or linked.
    pub async fn sync_contacts(&self, contacts: &[Contact]) {
        let mut store = self
            .state
            .lock()
            .unwrap()
            .store
            .clone()
            .expect("account not loaded");
        for contact in contacts {
            let stored = StoredContact {
                uuid: contact.id.uuid(),
                phone_number: contact
                    .phone
                    .as_ref()
                    .map(|phone| phonenumber::parse(None, phone.as_str()).expect("E.164 number")),
                name: contact.name.clone().unwrap_or_default(),
                verified: Default::default(),
                profile_key: Vec::new(),
                expire_timer: 0,
                expire_timer_version: 1,
                inbox_position: 0,
                avatar: None,
            };
            store.save_contact(&stored).await.expect("contact saved");
        }
        self.deliver(Incoming::ContactsSynced);
    }

    /// Sets the conversation's disappearing messages timer in seconds;
    /// `None` turns it off.
    pub fn set_expire_timer(&self, destination: Destination, seconds: Option<u32>) {
        let timers = &mut self.state.lock().unwrap().expire_timers;
        timers.retain(|(known, _)| !same_conversation(known, &destination));
        timers.extend(seconds.map(|seconds| (destination, seconds)));
    }
}

/// Same contact, or same group at any revision.
fn same_conversation(a: &Destination, b: &Destination) -> bool {
    match (a, b) {
        (Destination::Contact(a), Destination::Contact(b)) => a == b,
        (Destination::Group(a), Destination::Group(b)) => a.key == b.key,
        _ => false,
    }
}

/// [`Clock`] that moves only when told. Clones share the time.
#[derive(Clone, Debug)]
pub struct ManualClock {
    now: Arc<watch::Sender<Timestamp>>,
}

impl ManualClock {
    pub fn new(now: Timestamp) -> Self {
        Self {
            now: Arc::new(watch::Sender::new(now)),
        }
    }

    pub fn set(&self, now: Timestamp) {
        self.now.send_replace(now);
    }

    pub fn advance(&self, by: Duration) {
        self.now.send_modify(|now| {
            *now = Timestamp::from_millis(now.as_millis() + by.as_millis() as u64)
        });
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.now.borrow()
    }

    fn sleep_until(&self, at: Timestamp) -> LocalBoxFuture<'static, ()> {
        let mut now = self.now.subscribe();
        async move {
            now.wait_for(|now| *now >= at).await.ok();
        }
        .boxed_local()
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

    async fn load(&mut self, db: &Database) -> Result<Option<FakeBackend>, BackendError> {
        let linked = {
            let mut state = self.state.lock().unwrap();
            if let Some(error) = state.load_failure.take() {
                return Err(error);
            }
            state.linked
        };
        Ok(if linked { self.take_backend(db) } else { None })
    }

    async fn link(
        &mut self,
        db: &Database,
        device_name: &str,
        url: oneshot::Sender<String>,
    ) -> Result<FakeBackend, BackendError> {
        self.state.lock().unwrap().device_name = Some(device_name.into());
        if let Some(backend) = &self.backend {
            backend.state.lock().unwrap().device_name = Some(device_name.into());
        }
        url.send(FAKE_LINK_URL.into()).ok();
        match self.approvals.recv().await {
            Some(Ok(())) => {
                self.state.lock().unwrap().linked = true;
                self.take_backend(db)
                    .ok_or_else(|| BackendError::new("already linked"))
            }
            Some(Err(error)) => Err(error),
            None => Err(BackendError::new("phone gone")),
        }
    }
}

impl FakeLinker {
    /// The backend, once, sharing presage's store in `db`.
    fn take_backend(&mut self, db: &Database) -> Option<FakeBackend> {
        let backend = self.backend.take()?;
        backend.state.lock().unwrap().store = Some(db.presage_store().clone());
        Some(backend)
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
