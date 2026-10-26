//! Fake Signal backend and fixtures for Copiale tests.
//!
//! Dev-dependency only; tests never reach real Signal servers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use copiale_core::storage::{Database, SecretError, SecretStore};
use copiale_core::{
    Ack, Attachment, BackendError, BlockedList, ChatRef, Clock, Contact, Destination, GroupInfo,
    GroupMasterKey, GroupMembers, IdentityKey, Incoming, Linker, MessageBody, Outgoing,
    PeerIdentity, RemoteAttachment, RequestResponse, Resendable, RetryRequest, SafetyNumber,
    ServiceId, SignalBackend, StoredChats, Timestamp,
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
    let (identity_changes_tx, identity_changes) = unbounded_channel();
    let backend = FakeBackend {
        state: state.clone(),
        link: link.clone(),
        changes: changes.clone(),
        identity_changes: Some(identity_changes),
    };
    let network = FakeNetwork {
        state,
        link,
        changes,
        identity_changes: identity_changes_tx,
    };
    (backend, network)
}

/// Deterministic identity key for fixtures.
pub fn identity_key(n: u8) -> IdentityKey {
    IdentityKey::new(vec![5, n])
}

/// The safety number the [`FakeBackend`] reports for a key.
pub fn safety_number(key: &IdentityKey) -> SafetyNumber {
    let seed: u32 = key.as_bytes().iter().map(|byte| u32::from(*byte)).sum();
    SafetyNumber::new(
        (0..60)
            .map(|ix| char::from(b'0' + ((seed + ix) % 10) as u8))
            .collect::<String>(),
    )
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
        attachments: Vec::new(),
    }
}

#[derive(Clone, Copy, Default)]
struct Link {
    sends_paused: bool,
    downloads_paused: bool,
    uploads_paused: bool,
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
    /// Items the core acknowledged.
    acknowledged: usize,
    receive_failures: VecDeque<BackendError>,
    sent: Vec<SentText>,
    sends_started: usize,
    send_failures: VecDeque<BackendError>,
    contact_sync_requests: usize,
    /// Stored group states.
    groups: HashMap<GroupMasterKey, GroupInfo>,
    /// Groups this account isn't a member of.
    groups_left: HashSet<GroupMasterKey>,
    /// Group states on the server, stored once fetched.
    server_groups: HashMap<GroupMasterKey, GroupInfo>,
    group_fetches: usize,
    /// Profile names, known once fetched.
    profile_names: HashMap<ServiceId, String>,
    profiles_fetched: HashSet<ServiceId>,
    profile_fetches: usize,
    settings_sync_requests: usize,
    /// Times this device was removed from the account.
    device_removals: usize,
    /// Scripted failures of the next device removals, in order.
    device_removal_failures: VecDeque<BackendError>,
    read_receipts: Vec<(ServiceId, Vec<Timestamp>)>,
    read_syncs: Vec<Vec<(ServiceId, Timestamp)>>,
    typing: Vec<(Destination, bool)>,
    deleted_for_me: Vec<DeletedForMe>,
    /// Every retry request asked for, failed or not.
    retry_requests: Vec<(ServiceId, RetryRequest)>,
    retry_request_failures: VecDeque<BackendError>,
    /// Texts sent again, by recipient.
    resent: Vec<(ServiceId, SentText)>,
    null_messages: Vec<ServiceId>,
    /// presage's store in the core's database, once the account is loaded.
    store: Option<SqliteStore>,
    /// Disappearing messages timers, by conversation.
    expire_timers: Vec<(Destination, u32)>,
    /// Set when linked; [`DEVICE_NAME`] for an account linked before.
    device_name: Option<String>,
    /// Identity keys sessions use, by account.
    identities: HashMap<ServiceId, IdentityKey>,
    /// Devices of each account, if not only device 1.
    devices: HashMap<ServiceId, Vec<u32>>,
    /// Answers to message requests synced to other devices.
    request_responses: Vec<(ChatRef, RequestResponse)>,
    /// Blocked lists synced to other devices.
    blocked_syncs: Vec<BlockedList>,
    blocked_sync_requests: usize,
    /// Accounts this account's profile was shared with.
    profile_keys_sent: Vec<ServiceId>,
    /// Storage Service's chats, if any.
    stored: Option<StoredChats>,
    stored_reads: usize,
    /// Attachments on the CDN, by pointer.
    attachments: HashMap<Vec<u8>, Vec<u8>>,
    /// Attachments downloaded, in order, including failed ones.
    downloads: Vec<RemoteAttachment>,
    download_failures: VecDeque<BackendError>,
    /// Attachments uploaded, in order, as described and sent.
    uploads: Vec<(Attachment, Vec<u8>)>,
    uploads_started: usize,
    upload_failures: VecDeque<BackendError>,
}

type DeletedForMe = (Destination, Vec<(Option<ServiceId>, Timestamp)>);

/// [`SignalBackend`] driven by a [`FakeNetwork`]. Each `receive` opens a
/// new connection, replacing any previous one.
pub struct FakeBackend {
    state: Arc<Mutex<State>>,
    link: Arc<watch::Sender<Link>>,
    /// Counts what the backend recorded; see [`FakeNetwork::wait_until`].
    changes: Arc<watch::Sender<u64>>,
    /// Until [`SignalBackend::identity_changes`] takes it.
    identity_changes: Option<UnboundedReceiver<ServiceId>>,
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
    ) -> LocalBoxFuture<'static, Result<LocalBoxStream<'static, (Incoming, Ack)>, BackendError>>
    {
        let state = self.state.clone();
        let changes = self.changes.clone();
        let mut link = self.link.subscribe();
        async move {
            link.wait_for(|link| !link.connections_stalled).await.ok();
            if link.borrow().offline {
                return Err(BackendError::network("offline"));
            }
            let rx = {
                let mut state = state.lock().unwrap();
                if let Some(error) = state.receive_failures.pop_front() {
                    return Err(error);
                }
                let (tx, rx) = unbounded_channel();
                state.connection = Some(tx);
                state.connections += 1;
                rx
            };
            let stream = futures::stream::unfold(rx, move |mut rx| {
                let (state, changes) = (state.clone(), changes.clone());
                async move {
                    let item = rx.recv().await?;
                    let ack = Ack::new(move || {
                        record(&state, &changes, |state| state.acknowledged += 1);
                    });
                    Some(((item, ack), rx))
                }
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
    /// the network is up. Reaches the contact, or the scripted group's
    /// members, on their devices; the content is its place among
    /// [`FakeNetwork::sent`].
    fn send(
        &mut self,
        destination: Destination,
        outgoing: Outgoing,
        timestamp: Timestamp,
    ) -> LocalBoxFuture<'static, Result<Resendable, BackendError>> {
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
            Ok(record(&state, &changes, |state| {
                let recipients: Vec<ServiceId> = match destination {
                    Destination::Contact(peer) if peer == service_id(OWN_ID) => Vec::new(),
                    Destination::Contact(peer) => vec![peer],
                    Destination::Group(group) => state
                        .groups
                        .get(&group.key)
                        .map(|info| info.members.iter().map(|member| member.id).collect())
                        .unwrap_or_default(),
                };
                let recipients = recipients
                    .into_iter()
                    .map(|peer| {
                        let devices = state.devices.get(&peer).cloned().unwrap_or(vec![1]);
                        (peer, devices)
                    })
                    .collect();
                let content = (state.sent.len() as u64).to_le_bytes().to_vec();
                state.sent.push(SentText {
                    destination,
                    outgoing,
                    timestamp,
                });
                Resendable {
                    recipients,
                    content,
                }
            }))
        }
        .boxed_local()
    }

    /// Puts the file on the CDN as `upload-N`, once uploads aren't paused
    /// and the network is up.
    fn upload(
        &mut self,
        attachment: Attachment,
        contents: LocalBoxFuture<'static, std::io::Result<Vec<u8>>>,
    ) -> LocalBoxFuture<'static, Result<RemoteAttachment, BackendError>> {
        let failure = self.record(|state| {
            state.uploads_started += 1;
            state.upload_failures.pop_front()
        });
        let state = self.state.clone();
        let changes = self.changes.clone();
        let mut link = self.link.subscribe();
        async move {
            link.wait_for(|link| !link.uploads_paused && !link.offline)
                .await
                .ok();
            if let Some(error) = failure {
                return Err(error);
            }
            let contents = contents
                .await
                .map_err(|error| BackendError::new(error.to_string()))?;
            Ok(record(&state, &changes, |state| {
                let name = format!("upload-{}", state.uploads.len()).into_bytes();
                state.attachments.insert(name.clone(), contents.clone());
                state.uploads.push((attachment, contents));
                RemoteAttachment::new(name)
            }))
        }
        .boxed_local()
    }

    /// Gets what [`FakeNetwork::upload`] put there, once downloads aren't
    /// paused and the network is up; gone otherwise.
    fn download(
        &mut self,
        attachment: RemoteAttachment,
    ) -> LocalBoxFuture<'static, Result<Vec<u8>, BackendError>> {
        let (failure, contents) = self.record(|state| {
            state.downloads.push(attachment.clone());
            let contents = state.attachments.get(attachment.as_bytes()).cloned();
            (state.download_failures.pop_front(), contents)
        });
        let mut link = self.link.subscribe();
        async move {
            link.wait_for(|link| !link.downloads_paused && !link.offline)
                .await
                .ok();
            if let Some(error) = failure {
                return Err(error);
            }
            contents.ok_or_else(|| BackendError::gone("not on the CDN"))
        }
        .boxed_local()
    }

    fn send_retry_request(
        &mut self,
        sender: ServiceId,
        retry: RetryRequest,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let result = self.record(|state| {
            state.retry_requests.push((sender, retry));
            state.retry_request_failures.pop_front().map_or(Ok(()), Err)
        });
        async move { result }.boxed_local()
    }

    /// Sends the text whose content [`Self::send`] returned again.
    fn resend(
        &mut self,
        recipient: ServiceId,
        content: Vec<u8>,
        sent_at: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let result = self.record(|state| {
            let index = <[u8; 8]>::try_from(content.as_slice())
                .map(|bytes| u64::from_le_bytes(bytes) as usize)
                .map_err(|_| BackendError::new("not sent here"))?;
            let sent = state
                .sent
                .get(index)
                .filter(|sent| sent.timestamp == sent_at)
                .cloned()
                .ok_or_else(|| BackendError::new("not sent here"))?;
            state.resent.push((recipient, sent));
            Ok(())
        });
        async move { result }.boxed_local()
    }

    fn send_null_message(
        &mut self,
        recipient: ServiceId,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.null_messages.push(recipient));
        async { Ok(()) }.boxed_local()
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

    /// The scripted key, with [`safety_number`].
    fn identity(
        &mut self,
        peer: ServiceId,
    ) -> LocalBoxFuture<'static, Result<Option<PeerIdentity>, BackendError>> {
        let key = self.state.lock().unwrap().identities.get(&peer).cloned();
        let identity = key.map(|key| PeerIdentity {
            safety_number: safety_number(&key),
            key,
        });
        async move { Ok(identity) }.boxed_local()
    }

    /// Changes from [`FakeNetwork::change_identity`].
    fn identity_changes(&mut self) -> LocalBoxStream<'static, ServiceId> {
        match self.identity_changes.take() {
            Some(changes) => futures::stream::unfold(changes, |mut changes| async move {
                let peer = changes.recv().await?;
                Some((peer, changes))
            })
            .boxed_local(),
            None => futures::stream::empty().boxed_local(),
        }
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

    fn sync_request_response(
        &mut self,
        chat: ChatRef,
        response: RequestResponse,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.request_responses.push((chat, response)));
        async { Ok(()) }.boxed_local()
    }

    fn sync_blocked(
        &mut self,
        list: BlockedList,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.blocked_syncs.push(list));
        async { Ok(()) }.boxed_local()
    }

    /// Counted once sent: hangs while the network is down.
    fn request_blocked_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.sync_request(|state| state.blocked_sync_requests += 1)
    }

    fn send_profile_key(
        &mut self,
        peer: ServiceId,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.record(|state| state.profile_keys_sent.push(peer));
        async { Ok(()) }.boxed_local()
    }

    /// The scripted chats, unless at `known`; fails while offline.
    fn stored_chats(
        &mut self,
        known: Option<u64>,
    ) -> LocalBoxFuture<'static, Result<Option<StoredChats>, BackendError>> {
        let offline = self.link.borrow().offline;
        let stored = self.record(|state| {
            state.stored_reads += 1;
            state.stored.clone()
        });
        async move {
            if offline {
                return Err(BackendError::network("offline"));
            }
            Ok(stored.filter(|stored| Some(stored.version) != known))
        }
        .boxed_local()
    }

    /// Fails as scripted; otherwise counted once sent, hanging while the
    /// network is down.
    fn remove_device(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        if let Some(error) = self
            .state
            .lock()
            .unwrap()
            .device_removal_failures
            .pop_front()
        {
            return async { Err(error) }.boxed_local();
        }
        self.sync_request(|state| state.device_removals += 1)
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

    /// The scripted group's members and this account, unless it left.
    fn group_members(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<Option<GroupMembers>, BackendError>> {
        let state = self.state.clone();
        async move {
            let state = state.lock().unwrap();
            let Some(info) = state.groups.get(&key) else {
                return Ok(None);
            };
            let mut members: Vec<_> = info.members.iter().map(|member| member.id).collect();
            if !state.groups_left.contains(&key) {
                members.push(service_id(OWN_ID));
            }
            Ok(Some(GroupMembers {
                revision: info.revision,
                members,
            }))
        }
        .boxed_local()
    }

    /// Stores the server's state of the group; fails while offline, and
    /// without one.
    fn refresh_group(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let (state, changes) = (self.state.clone(), self.changes.clone());
        let offline = self.link.borrow().offline;
        async move {
            if offline {
                return Err(BackendError::network("offline"));
            }
            record(&state, &changes, |state| {
                state.group_fetches += 1;
                let info = state
                    .server_groups
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| BackendError::new("not a member"))?;
                state.groups.insert(key, info);
                Ok(())
            })
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
    identity_changes: UnboundedSender<ServiceId>,
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

    /// Makes the next retry request fail; queues up for repeated calls.
    pub fn fail_next_retry_request(&self, error: BackendError) {
        self.state
            .lock()
            .unwrap()
            .retry_request_failures
            .push_back(error);
    }

    /// Puts an attachment on the CDN; returns where it is.
    pub fn upload(&self, name: &str, contents: &[u8]) -> RemoteAttachment {
        let remote = RemoteAttachment::new(name.as_bytes().to_vec());
        self.state
            .lock()
            .unwrap()
            .attachments
            .insert(name.as_bytes().to_vec(), contents.to_vec());
        remote
    }

    /// Files uploaded so far, oldest first, as described and sent.
    pub fn uploads(&self) -> Vec<(Attachment, Vec<u8>)> {
        self.state.lock().unwrap().uploads.clone()
    }

    /// Uploads begun so far, finished or not.
    pub fn uploads_started(&self) -> usize {
        self.state.lock().unwrap().uploads_started
    }

    /// Makes the next upload fail; queues up for repeated calls.
    pub fn fail_next_upload(&self, error: BackendError) {
        self.state.lock().unwrap().upload_failures.push_back(error);
    }

    /// Holds uploads in flight until called with `false`.
    pub fn pause_uploads(&self, paused: bool) {
        self.link.send_modify(|link| link.uploads_paused = paused);
    }

    /// Attachments downloads asked for so far, oldest first.
    pub fn downloads(&self) -> Vec<RemoteAttachment> {
        self.state.lock().unwrap().downloads.clone()
    }

    /// Makes the next download fail; queues up for repeated calls.
    pub fn fail_next_download(&self, error: BackendError) {
        self.state
            .lock()
            .unwrap()
            .download_failures
            .push_back(error);
    }

    /// Holds downloads in flight until called with `false`.
    pub fn pause_downloads(&self, paused: bool) {
        self.link.send_modify(|link| link.downloads_paused = paused);
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

    /// Items the core acknowledged so far.
    pub fn acknowledged(&self) -> usize {
        self.state.lock().unwrap().acknowledged
    }

    pub fn contact_sync_requests(&self) -> usize {
        self.state.lock().unwrap().contact_sync_requests
    }

    /// Sets `peer`'s devices, which later sends reach; device 1 alone by
    /// default.
    pub fn set_devices(&self, peer: ServiceId, devices: &[u32]) {
        self.state
            .lock()
            .unwrap()
            .devices
            .insert(peer, devices.to_vec());
    }

    /// Sets the group state the backend reports; members' names are their
    /// contact names, if any.
    pub fn set_group(&self, key: GroupMasterKey, info: GroupInfo) {
        self.state.lock().unwrap().groups.insert(key, info);
    }

    /// Sets the group state on the server, stored once the backend fetches
    /// it.
    pub fn set_server_group(&self, key: GroupMasterKey, info: GroupInfo) {
        self.state.lock().unwrap().server_groups.insert(key, info);
    }

    /// Takes this account out of the group's stored state.
    pub fn leave_group(&self, key: GroupMasterKey) {
        self.state.lock().unwrap().groups_left.insert(key);
    }

    /// Groups fetched from the server so far.
    pub fn group_fetches(&self) -> usize {
        self.state.lock().unwrap().group_fetches
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

    pub fn device_removals(&self) -> usize {
        self.state.lock().unwrap().device_removals
    }

    /// Makes a device removal fail, after those already scripted to.
    pub fn fail_device_removal(&self, error: BackendError) {
        self.state
            .lock()
            .unwrap()
            .device_removal_failures
            .push_back(error);
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

    /// Requests to resend messages that couldn't be decrypted, by sender.
    pub fn retry_requests(&self) -> Vec<(ServiceId, RetryRequest)> {
        self.state.lock().unwrap().retry_requests.clone()
    }

    /// Texts sent again on request, by recipient.
    pub fn resent(&self) -> Vec<(ServiceId, SentText)> {
        self.state.lock().unwrap().resent.clone()
    }

    /// Null messages sent so far, by recipient.
    pub fn null_messages(&self) -> Vec<ServiceId> {
        self.state.lock().unwrap().null_messages.clone()
    }

    /// Answers to message requests synced to the account's other devices.
    pub fn request_responses(&self) -> Vec<(ChatRef, RequestResponse)> {
        self.state.lock().unwrap().request_responses.clone()
    }

    /// Blocked lists synced to the account's other devices.
    pub fn blocked_syncs(&self) -> Vec<BlockedList> {
        self.state.lock().unwrap().blocked_syncs.clone()
    }

    pub fn blocked_sync_requests(&self) -> usize {
        self.state.lock().unwrap().blocked_sync_requests
    }

    /// Accounts this account's profile was shared with.
    pub fn profile_keys_sent(&self) -> Vec<ServiceId> {
        self.state.lock().unwrap().profile_keys_sent.clone()
    }

    /// Sets what Storage Service holds.
    pub fn set_stored_chats(&self, stored: StoredChats) {
        self.state.lock().unwrap().stored = Some(stored);
    }

    /// Storage Service reads so far.
    pub fn stored_reads(&self) -> usize {
        self.state.lock().unwrap().stored_reads
    }

    /// Saves contacts to presage's store, as a sync of all the phone's
    /// contacts does, and delivers [`Incoming::ContactsSynced`].
    ///
    /// Panics before the account is loaded or linked.
    pub async fn sync_contacts(&self, contacts: &[Contact]) {
        self.save_contacts(contacts).await;
        let all = contacts.iter().map(|contact| contact.id).collect();
        self.deliver(Incoming::ContactsSynced { all: Some(all) });
    }

    /// As [`Self::sync_contacts`], for a sync not known to hold all the
    /// phone's contacts.
    pub async fn sync_some_contacts(&self, contacts: &[Contact]) {
        self.save_contacts(contacts).await;
        self.deliver(Incoming::ContactsSynced { all: None });
    }

    async fn save_contacts(&self, contacts: &[Contact]) {
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
    }

    /// Sets the identity key sessions with `peer` use, as their first
    /// session does: no change is reported.
    pub fn set_identity(&self, peer: ServiceId, key: IdentityKey) {
        self.state.lock().unwrap().identities.insert(peer, key);
    }

    /// Replaces `peer`'s identity key and reports the change, as a session
    /// with their new key does.
    pub fn change_identity(&self, peer: ServiceId, key: IdentityKey) {
        self.set_identity(peer, key);
        self.identity_changes.send(peer).ok();
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
