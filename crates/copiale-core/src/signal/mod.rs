//! Signal through presage: the [`Linker`] and [`SignalBackend`] the app uses.
//!
//! Talks to Signal's production servers; tests use the testkit fakes, and
//! only the mapping to core types is tested here.

mod store;

use futures::future::LocalBoxFuture;
use futures::stream::{LocalBoxStream, Stream};
use futures::{FutureExt as _, StreamExt as _};
use presage::Manager;
use presage::libsignal_service::configuration::SignalServers;
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::groups_v2::{GroupChange, GroupOperations, Role};
use presage::libsignal_service::prelude::{GroupSecretParams, ProtobufMessage as _};
use presage::libsignal_service::proto;
use presage::libsignal_service::proto::call_message::offer;
use presage::libsignal_service::proto::sync_message::{self, Content as SyncContent, Sent};
use presage::libsignal_service::proto::{
    AddressableMessage, AttachmentPointer, CallMessage, ConversationIdentifier, DataMessage,
    EditMessage, GroupContextV2, ReceiptMessage, SyncMessage, TypingMessage, addressable_message,
    attachment_pointer, conversation_identifier, data_message, receipt_message, typing_message,
};
use presage::libsignal_service::protocol::{Aci, ServiceId as SignalServiceId};
use presage::libsignal_service::push_service::ServiceError;
use presage::libsignal_service::sender::MessageSenderError;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentsStore as _, StateStore as _, Thread};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};

use crate::backend::{BackendError, Incoming, Linker, SignalBackend};
use crate::model::{
    self, Attachment, Change, ChatRef, Destination, GroupId, GroupInfo, GroupMasterKey, GroupRef,
    GroupUpdate, IncomingMessage, Member, MessageBody, MessageChange, MessageRef, Outgoing, Quote,
    ReceiptKind, SentMessage, ServiceId, Timestamp, Unsupported,
};
use crate::storage::Database;
use store::AppStore;

/// Loads or links the account in the app's database.
#[derive(Default)]
pub struct PresageLinker;

impl Linker for PresageLinker {
    type Backend = PresageBackend;

    async fn load(&mut self, db: &Database) -> Result<Option<PresageBackend>, BackendError> {
        let store = AppStore::new(db.presage_store().clone());
        if !store.is_registered().await {
            return Ok(None);
        }
        let manager = Manager::load_registered(store)
            .await
            .map_err(presage_error)?;
        Ok(Some(PresageBackend::new(manager)))
    }

    async fn link(
        &mut self,
        db: &Database,
        device_name: &str,
        url: oneshot::Sender<String>,
    ) -> Result<PresageBackend, BackendError> {
        let store = AppStore::new(db.presage_store().clone());
        let (url_tx, url_rx) = futures::channel::oneshot::channel();
        let link = Manager::link_secondary_device(
            store,
            SignalServers::Production,
            device_name.into(),
            url_tx,
        );
        let forward_url = async move {
            if let Ok(provisioning_url) = url_rx.await {
                url.send(provisioning_url.to_string()).ok();
            }
        };
        let (manager, ()) = futures::join!(link, forward_url);
        let manager = manager.map_err(presage_error)?;
        tracing::info!("linked as a secondary device");
        Ok(PresageBackend::new(manager))
    }
}

/// A linked account.
pub struct PresageBackend {
    manager: Manager<AppStore, Registered>,
    receiver: Option<Receiver>,
}

/// The task receiving on the open connection.
struct Receiver {
    task: JoinHandle<()>,
    /// Asks it to stop; see [`relay`].
    stop: Option<oneshot::Sender<()>>,
}

impl PresageBackend {
    fn new(manager: Manager<AppStore, Registered>) -> Self {
        Self {
            manager,
            receiver: None,
        }
    }
}

impl Drop for PresageBackend {
    fn drop(&mut self) {
        if let Some(receiver) = self.receiver.take() {
            receiver.task.abort();
        }
    }
}

impl SignalBackend for PresageBackend {
    /// Receives on a clone of the manager in a local task, so sending
    /// doesn't wait for the stream. Must run inside a `LocalSet`.
    fn receive(
        &mut self,
    ) -> LocalBoxFuture<'static, Result<LocalBoxStream<'static, Incoming>, BackendError>> {
        let mut manager = self.manager.clone();
        let (opened_tx, opened_rx) = oneshot::channel();
        let (items_tx, items_rx) = mpsc::channel(RECEIVE_BUFFER);
        let (stop, stop_rx) = oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            let stream = match manager.receive_messages().await {
                Ok(stream) => {
                    opened_tx.send(Ok(())).ok();
                    stream
                }
                Err(error) => {
                    opened_tx.send(Err(presage_error(error))).ok();
                    return;
                }
            };
            relay(stream, items_tx, stop_rx).await;
        });
        let receiver = Receiver {
            task,
            stop: Some(stop),
        };
        if let Some(previous) = self.receiver.replace(receiver) {
            previous.task.abort();
        }
        async move {
            opened_rx
                .await
                .map_err(|_| BackendError::new("receiver stopped"))??;
            let stream = futures::stream::unfold(items_rx, |mut items| async move {
                let item = items.recv().await?;
                Some((item, items))
            });
            Ok(stream.boxed_local())
        }
        .boxed_local()
    }

    fn stop_receiving(&mut self) {
        if let Some(stop) = self
            .receiver
            .as_mut()
            .and_then(|receiver| receiver.stop.take())
        {
            stop.send(()).ok();
        }
    }

    /// Sends on a clone of the manager; libsignal-service adds the sync
    /// copy for the other devices.
    fn send(
        &mut self,
        destination: Destination,
        outgoing: Outgoing,
        timestamp: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let me = manager.registration_data().service_ids.aci;
        let at = timestamp.as_millis();
        async move {
            let group_v2 = match destination {
                Destination::Group(group) => {
                    // presage's copy may be newer than the core's.
                    let revision = match manager.store().group(*group.key.as_bytes()).await {
                        Ok(Some(stored)) => stored.revision.max(group.revision),
                        _ => group.revision,
                    };
                    Some(GroupContextV2 {
                        master_key: Some(group.key.as_bytes().to_vec()),
                        revision: Some(revision),
                        ..Default::default()
                    })
                }
                Destination::Contact(_) => None,
            };
            let text = |body: MessageBody, quote: Option<Quote>| DataMessage {
                body: Some(body.as_str().into()),
                timestamp: Some(at),
                quote: quote.map(|quote| proto_quote(quote, me)),
                group_v2: group_v2.clone(),
                ..Default::default()
            };
            let content: ContentBody = match outgoing {
                Outgoing::Text { body, quote } => text(body, quote).into(),
                Outgoing::Edit {
                    target,
                    body,
                    quote,
                } => EditMessage {
                    target_sent_timestamp: Some(target.as_millis()),
                    data_message: Some(text(body, quote)),
                }
                .into(),
                Outgoing::Delete { target } => DataMessage {
                    delete: Some(data_message::Delete {
                        target_sent_timestamp: Some(target.as_millis()),
                    }),
                    timestamp: Some(at),
                    group_v2: group_v2.clone(),
                    ..Default::default()
                }
                .into(),
            };
            match destination {
                Destination::Contact(recipient) => {
                    manager
                        .send_message(Aci::from(recipient.uuid()), content, at)
                        .await
                }
                Destination::Group(group) => {
                    manager
                        .send_message_to_group(group.key.as_bytes(), content, at)
                        .await
                }
            }
            .map_err(presage_error)
        }
        .boxed_local()
    }

    fn sync_deleted_for_me(
        &mut self,
        destination: Destination,
        messages: Vec<(Option<ServiceId>, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let me = manager.registration_data().service_ids.aci;
        let conversation = ConversationIdentifier {
            identifier: Some(match destination {
                Destination::Contact(peer) => {
                    conversation_identifier::Identifier::ThreadServiceId(peer.uuid().to_string())
                }
                Destination::Group(group) => conversation_identifier::Identifier::ThreadGroupId(
                    group.key.id().as_bytes().to_vec(),
                ),
            }),
        };
        let messages = messages
            .into_iter()
            .map(|(author, sent_at)| AddressableMessage {
                author: Some(addressable_message::Author::AuthorServiceId(
                    author.map_or(me, |author| author.uuid()).to_string(),
                )),
                sent_timestamp: Some(sent_at.as_millis()),
            })
            .collect();
        let sync = SyncMessage {
            content: Some(SyncContent::DeleteForMe(sync_message::DeleteForMe {
                message_deletes: vec![sync_message::delete_for_me::MessageDeletes {
                    conversation: Some(conversation),
                    messages,
                }],
                ..Default::default()
            })),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        async move {
            let me = manager.registration_data().service_ids.aci();
            manager
                .send_message(me, sync, Timestamp::now().as_millis())
                .await
                .map_err(presage_error)
        }
        .boxed_local()
    }

    /// Reads presage's copy, which it refreshes when a message shows a newer
    /// revision.
    fn group(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<Option<GroupInfo>, BackendError>> {
        let manager = self.manager.clone();
        async move {
            let store = manager.store();
            let Some(group) = store.group(*key.as_bytes()).await.map_err(store_error)? else {
                return Ok(None);
            };
            let me = manager.registration_data().service_ids.aci;
            let mut members = Vec::with_capacity(group.members.len());
            for member in group
                .members
                .iter()
                .filter(|member| member.aci != Aci::from(me))
            {
                members.push(Member {
                    id: ServiceId::new(member.aci.into()),
                    name: member_name(store, member).await,
                });
            }
            Ok(Some(GroupInfo {
                title: group.title,
                revision: group.revision,
                members,
            }))
        }
        .boxed_local()
    }

    /// One request per member, one at a time, up to [`MAX_PROFILE_FETCHES`].
    fn fetch_member_profiles(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        async move {
            let Some(group) = manager
                .store()
                .group(*key.as_bytes())
                .await
                .map_err(store_error)?
            else {
                return Ok(());
            };
            let me = Aci::from(manager.registration_data().service_ids.aci);
            let mut fetched = 0;
            for member in &group.members {
                if member.aci == me || member_name(manager.store(), member).await.is_some() {
                    continue;
                }
                if fetched == MAX_PROFILE_FETCHES {
                    tracing::info!("profile fetches capped for a large group");
                    break;
                }
                fetched += 1;
                if let Err(error) = manager
                    .retrieve_profile_by_uuid(member.aci, member.profile_key)
                    .await
                {
                    let error = presage_error(error);
                    if error.is_network() {
                        return Err(error);
                    }
                    tracing::debug!(%error, "fetching a member profile failed");
                }
            }
            Ok(())
        }
        .boxed_local()
    }

    /// presage's copy: synced contacts' timers, kept current from their
    /// messages, and groups' from their state.
    fn expire_timer(
        &mut self,
        destination: Destination,
    ) -> LocalBoxFuture<'static, Result<Option<u32>, BackendError>> {
        let manager = self.manager.clone();
        let thread = match destination {
            Destination::Contact(peer) => Thread::Contact(Aci::from(peer.uuid()).into()),
            Destination::Group(group) => Thread::Group(*group.key.as_bytes()),
        };
        async move {
            let timer = manager
                .store()
                .expire_timer(&thread)
                .await
                .map_err(store_error)?;
            Ok(timer
                .map(|(seconds, _version)| seconds)
                .filter(|seconds| *seconds > 0))
        }
        .boxed_local()
    }

    /// Asks who this account is, over the connection receiving messages.
    fn probe(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let manager = self.manager.clone();
        async move {
            manager.whoami().await.map_err(presage_error)?;
            Ok(())
        }
        .boxed_local()
    }

    fn own_id(&self) -> ServiceId {
        ServiceId::new(self.manager.registration_data().service_ids.aci)
    }

    fn device_name(&self) -> Option<String> {
        self.manager
            .registration_data()
            .device_name()
            .map(str::to_owned)
    }

    async fn request_contact_sync(&mut self) -> Result<(), BackendError> {
        self.manager.request_contacts().await.map_err(presage_error)
    }

    async fn request_settings_sync(&mut self) -> Result<(), BackendError> {
        let request = SyncMessage {
            content: Some(SyncContent::Request(sync_message::Request {
                r#type: Some(sync_message::request::Type::Configuration.into()),
            })),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        let me = self.manager.registration_data().service_ids.aci();
        self.manager
            .send_message(me, request, Timestamp::now().as_millis())
            .await
            .map_err(presage_error)
    }

    fn send_read_receipts(
        &mut self,
        recipient: ServiceId,
        timestamps: Vec<Timestamp>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let receipt = ReceiptMessage {
            r#type: Some(receipt_message::Type::Read.into()),
            timestamp: timestamps.iter().map(Timestamp::as_millis).collect(),
        };
        async move {
            manager
                .send_message(
                    Aci::from(recipient.uuid()),
                    receipt,
                    Timestamp::now().as_millis(),
                )
                .await
                .map_err(presage_error)
        }
        .boxed_local()
    }

    fn send_read_sync(
        &mut self,
        read: Vec<(ServiceId, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let sync = SyncMessage {
            read: read
                .into_iter()
                .map(|(author, sent_at)| sync_message::Read {
                    sender_aci: Some(author.uuid().to_string()),
                    timestamp: Some(sent_at.as_millis()),
                    ..Default::default()
                })
                .collect(),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        async move {
            let me = manager.registration_data().service_ids.aci();
            manager
                .send_message(me, sync, Timestamp::now().as_millis())
                .await
                .map_err(presage_error)
        }
        .boxed_local()
    }

    fn send_typing(
        &mut self,
        destination: Destination,
        typing: bool,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let now = Timestamp::now().as_millis();
        let action = if typing {
            typing_message::Action::Started
        } else {
            typing_message::Action::Stopped
        };
        async move {
            match destination {
                Destination::Contact(recipient) => {
                    let message = TypingMessage {
                        timestamp: Some(now),
                        action: Some(action.into()),
                        group_id: None,
                    };
                    manager
                        .send_message(Aci::from(recipient.uuid()), message, now)
                        .await
                }
                Destination::Group(group) => {
                    let message = TypingMessage {
                        timestamp: Some(now),
                        action: Some(action.into()),
                        group_id: Some(group.key.id().as_bytes().to_vec()),
                    };
                    manager
                        .send_message_to_group(group.key.as_bytes(), message, now)
                        .await
                }
            }
            .map_err(presage_error)
        }
        .boxed_local()
    }
}

/// Messages decrypted ahead of the core storing them: few, as stopping must
/// wait for them.
const RECEIVE_BUFFER: usize = 8;

/// While stopping, a read pending this long is waiting for the server, not
/// decrypting, so cancelling it loses nothing.
const DRAIN_IDLE: Duration = Duration::from_secs(2);

/// While stopping, no read starts after this long.
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

/// While stopping, how long after the last read the websocket gets to send
/// its queued acknowledgements; the server sends unacknowledged messages
/// again, which then fail to decrypt.
const ACK_FLUSH: Duration = Duration::from_millis(250);

/// Passes on what `stream` receives until it ends or `stop` fires, then
/// what was already on its way: presage acknowledges messages before
/// decrypting them, and cancelling a decryption may lose the message.
async fn relay(
    stream: impl Stream<Item = Received>,
    items: mpsc::Sender<Incoming>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut stream = Box::pin(stream);
    let mut stop_by = None;
    let mut last_read = None;
    loop {
        if stop_by.is_some_and(|at| Instant::now() >= at) {
            break;
        }
        let started = Instant::now();
        let mut next = stream.next();
        // `None`: stopping, and only waiting for the server.
        let received = match stop_by {
            Some(_) => tokio::time::timeout_at(started + DRAIN_IDLE, &mut next)
                .await
                .ok(),
            None => tokio::select! {
                received = &mut next => Some(received),
                _ = &mut stop => {
                    stop_by = Some(Instant::now() + DRAIN_LIMIT);
                    tokio::time::timeout_at(started + DRAIN_IDLE, &mut next).await.ok()
                }
            },
        };
        let Some(Some(received)) = received else {
            break;
        };
        last_read = Some(Instant::now());
        if let Some(item) = incoming(received)
            && items.send(item).await.is_err()
        {
            break;
        }
    }
    drop(stream);
    // `items` stays open meanwhile, so the backend, and its websocket, live.
    if stop_by.is_some()
        && let Some(at) = last_read
    {
        tokio::time::sleep_until(at + ACK_FLUSH).await;
    }
}

/// Most profiles one [`SignalBackend::fetch_member_profiles`] call fetches.
const MAX_PROFILE_FETCHES: usize = 100;

/// A member's contact name, else cached profile name; blank ones count as
/// unknown.
async fn member_name(store: &AppStore, member: &presage::model::groups::Member) -> Option<String> {
    let id: SignalServiceId = member.aci.into();
    if let Ok(Some(contact)) = store.contact_by_id(&id).await
        && !contact.name.trim().is_empty()
    {
        return Some(contact.name);
    }
    let profile = store
        .profile(member.aci.into(), member.profile_key)
        .await
        .ok()
        .flatten()?;
    let name = profile.name?.to_string();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn store_error(error: impl std::error::Error) -> BackendError {
    BackendError::new(error.to_string())
}

/// Marks connection failures as network errors.
fn presage_error<S: std::error::Error>(error: presage::Error<S>) -> BackendError {
    let unauthorized = match &error {
        presage::Error::ServiceError(error) => is_unauthorized(error),
        presage::Error::MessageSenderError(error) => {
            matches!(&**error, MessageSenderError::ServiceError(error) if is_unauthorized(error))
        }
        _ => false,
    };
    if unauthorized {
        return BackendError::unauthorized(error.to_string());
    }
    let network = match &error {
        presage::Error::ServiceError(error) => is_network(error),
        presage::Error::MessageSenderError(error) => {
            matches!(&**error, MessageSenderError::ServiceError(error) if is_network(error))
        }
        presage::Error::IoError(_) | presage::Error::Timeout(_) => true,
        _ => false,
    };
    if network {
        BackendError::network(error.to_string())
    } else {
        BackendError::new(error.to_string())
    }
}

/// The server rejected our credentials: on a request, or when opening the
/// connection.
fn is_unauthorized(error: &ServiceError) -> bool {
    match error {
        ServiceError::Unauthorized => true,
        ServiceError::WsError(error) => matches!(
            &**error,
            reqwest_websocket::Error::Handshake(
                reqwest_websocket::HandshakeError::UnexpectedStatusCode(status)
            ) if matches!(status.as_u16(), 401 | 403)
        ),
        _ => false,
    }
}

fn is_network(error: &ServiceError) -> bool {
    matches!(
        error,
        ServiceError::Timeout { .. }
            | ServiceError::SendError { .. }
            | ServiceError::IO(_)
            | ServiceError::WsError(_)
            | ServiceError::WsClosing { .. }
    )
}

/// What the core cares about so far; everything else is dropped.
fn incoming(received: Received) -> Option<Incoming> {
    match received {
        Received::QueueEmpty => Some(Incoming::QueueEmpty),
        Received::Content(content) => signal(&content)
            .or_else(|| message_change(&content).map(Incoming::Change))
            .or_else(|| received_message(&content).map(Incoming::Message))
            .or_else(|| sent_message(&content).map(Incoming::Sent)),
        Received::Contacts => Some(Incoming::ContactsSynced),
        Received::DecryptionError(_) => {
            tracing::warn!("a message could not be decrypted");
            None
        }
    }
}

/// Receipts, typing, and reads and settings synced from other devices.
fn signal(content: &Content) -> Option<Incoming> {
    let sender = ServiceId::new(content.metadata.sender.raw_uuid());
    match &content.body {
        ContentBody::ReceiptMessage(ReceiptMessage { r#type, timestamp }) => {
            let kind = match receipt_message::Type::try_from(r#type.unwrap_or_default()) {
                Ok(receipt_message::Type::Delivery) => ReceiptKind::Delivered,
                Ok(receipt_message::Type::Read | receipt_message::Type::Viewed) => {
                    ReceiptKind::Read
                }
                Err(_) => return None,
            };
            Some(Incoming::Receipt {
                sender,
                kind,
                timestamps: timestamp
                    .iter()
                    .copied()
                    .map(Timestamp::from_millis)
                    .collect(),
            })
        }
        ContentBody::TypingMessage(TypingMessage {
            action, group_id, ..
        }) => {
            let group = match group_id {
                Some(id) => Some(GroupId::new(<[u8; 32]>::try_from(id.as_slice()).ok()?)),
                None => None,
            };
            let typing = *action == Some(typing_message::Action::Started.into());
            Some(Incoming::Typing {
                sender,
                group,
                typing,
            })
        }
        ContentBody::SynchronizeMessage(sync) if !sync.read.is_empty() => {
            let read = sync
                .read
                .iter()
                .filter_map(|read| {
                    let author = read
                        .sender_aci_binary
                        .as_deref()
                        .and_then(|bytes| uuid::Uuid::from_slice(bytes).ok())
                        .or_else(|| read.sender_aci.as_deref()?.parse().ok())?;
                    Some((
                        ServiceId::new(author),
                        Timestamp::from_millis(read.timestamp?),
                    ))
                })
                .collect();
            let at = content.metadata.client_timestamp.timestamp_millis().max(0) as u64;
            Some(Incoming::ReadElsewhere {
                read,
                at: Timestamp::from_millis(at),
            })
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::DeleteForMe(delete)),
            ..
        }) => {
            let messages = delete
                .message_deletes
                .iter()
                .filter_map(|deletes| {
                    let chat = chat_ref(deletes.conversation.as_ref()?)?;
                    Some(deletes.messages.iter().filter_map(move |message| {
                        Some(MessageRef {
                            chat,
                            author: Some(message_author(message)?),
                            sent_at: Timestamp::from_millis(message.sent_timestamp?),
                        })
                    }))
                })
                .flatten()
                .collect();
            let whole = delete
                .conversation_deletes
                .iter()
                .map(|delete| delete.conversation.as_ref());
            let local = delete
                .local_only_conversation_deletes
                .iter()
                .map(|delete| delete.conversation.as_ref());
            let conversations = whole.chain(local).flatten().filter_map(chat_ref).collect();
            Some(Incoming::DeletedForMe {
                messages,
                conversations,
            })
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Configuration(configuration)),
            ..
        }) => Some(Incoming::Settings {
            read_receipts: configuration.read_receipts,
            typing_indicators: configuration.typing_indicators,
        }),
        _ => None,
    }
}

/// A conversation as another device names it; `None` for phone numbers
/// alone, which conversations aren't keyed by.
fn chat_ref(conversation: &ConversationIdentifier) -> Option<ChatRef> {
    match conversation.identifier.as_ref()? {
        conversation_identifier::Identifier::ThreadServiceId(id) => {
            let id = SignalServiceId::parse_from_service_id_string(id)?;
            Some(ChatRef::Contact(ServiceId::new(id.raw_uuid())))
        }
        conversation_identifier::Identifier::ThreadServiceIdBinary(id) => {
            let id = SignalServiceId::parse_from_service_id_binary(id)?;
            Some(ChatRef::Contact(ServiceId::new(id.raw_uuid())))
        }
        conversation_identifier::Identifier::ThreadGroupId(id) => Some(ChatRef::Group(
            GroupId::new(<[u8; 32]>::try_from(id.as_slice()).ok()?),
        )),
        conversation_identifier::Identifier::ThreadE164(_) => None,
    }
}

fn message_author(message: &AddressableMessage) -> Option<ServiceId> {
    let id = match message.author.as_ref()? {
        addressable_message::Author::AuthorServiceId(id) => {
            SignalServiceId::parse_from_service_id_string(id)?
        }
        addressable_message::Author::AuthorServiceIdBinary(id) => {
            SignalServiceId::parse_from_service_id_binary(id)?
        }
        addressable_message::Author::AuthorE164(_) => return None,
    };
    Some(ServiceId::new(id.raw_uuid()))
}

/// A message from another account, 1:1 or in a group, or a call to it.
fn received_message(content: &Content) -> Option<IncomingMessage> {
    let client_timestamp = || content.metadata.client_timestamp.timestamp_millis().max(0) as u64;
    let (group, timestamp, message, expire_timer) = match &content.body {
        ContentBody::DataMessage(message) => {
            let group = match &message.group_v2 {
                Some(context) => Some(group_ref(context)?),
                None => None,
            };
            let timestamp = message.timestamp.unwrap_or_else(client_timestamp);
            let content = data_content(message, group)?;
            (group, timestamp, content, expire_timer(message))
        }
        ContentBody::CallMessage(CallMessage {
            offer: Some(offer), ..
        }) => {
            let video = offer.r#type == Some(offer::Type::OfferVideoCall as i32);
            let call = model::Content::Call { video };
            (None, client_timestamp(), call, None)
        }
        _ => return None,
    };
    Some(IncomingMessage {
        sender: ServiceId::new(content.metadata.sender.raw_uuid()),
        group,
        timestamp: Timestamp::from_millis(timestamp),
        content: message,
        expire_timer,
    })
}

/// A message this account sent from another device, 1:1 or in a group.
fn sent_message(content: &Content) -> Option<SentMessage> {
    let sent = sync_sent(content)?;
    let message = sent.message.as_ref()?;
    let destination = sent_destination(sent, message)?;
    Some(SentMessage {
        destination,
        timestamp: Timestamp::from_millis(sent.timestamp?),
        content: data_content(message, group_of(destination))?,
        expire_timer: expire_timer(message),
    })
}

/// The message's disappearing timer in seconds; `None` if off.
fn expire_timer(message: &DataMessage) -> Option<u32> {
    message.expire_timer.filter(|seconds| *seconds > 0)
}

/// A sync copy of a message sent from another of this account's devices.
fn sync_sent(content: &Content) -> Option<&Sent> {
    match &content.body {
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Sent(sent)),
            ..
        }) => Some(sent),
        _ => None,
    }
}

/// Where a message sent from another device went.
fn sent_destination(sent: &Sent, message: &DataMessage) -> Option<Destination> {
    if let Some(context) = &message.group_v2 {
        return Some(Destination::Group(group_ref(context)?));
    }
    let recipient = sent
        .destination_service_id_binary
        .as_deref()
        .and_then(SignalServiceId::parse_from_service_id_binary)
        .or_else(|| {
            sent.destination_service_id
                .as_deref()
                .and_then(SignalServiceId::parse_from_service_id_string)
        })?;
    Some(Destination::Contact(ServiceId::new(recipient.raw_uuid())))
}

fn group_of(destination: Destination) -> Option<GroupRef> {
    match destination {
        Destination::Group(group) => Some(group),
        Destination::Contact(_) => None,
    }
}

/// An edit or delete of an earlier message, from its author or from
/// another of this account's devices.
fn message_change(content: &Content) -> Option<MessageChange> {
    let sender = ServiceId::new(content.metadata.sender.raw_uuid());
    let client_timestamp = || content.metadata.client_timestamp.timestamp_millis().max(0) as u64;
    let in_conversation = |message: &DataMessage| -> Option<Destination> {
        match &message.group_v2 {
            Some(context) => Some(Destination::Group(group_ref(context)?)),
            None => Some(Destination::Contact(sender)),
        }
    };
    let (destination, author, target, change) = match &content.body {
        ContentBody::DataMessage(
            message @ DataMessage {
                delete: Some(delete),
                ..
            },
        ) => (
            in_conversation(message)?,
            Some(sender),
            delete.target_sent_timestamp?,
            Change::Delete,
        ),
        ContentBody::EditMessage(EditMessage {
            target_sent_timestamp: Some(target),
            data_message: Some(message),
        }) => {
            let destination = in_conversation(message)?;
            let edit = Change::Edit {
                content: data_content(message, group_of(destination))?,
                at: Timestamp::from_millis(message.timestamp.unwrap_or_else(client_timestamp)),
            };
            (destination, Some(sender), *target, edit)
        }
        ContentBody::SynchronizeMessage(_) => {
            let sent = sync_sent(content)?;
            if let Some(EditMessage {
                target_sent_timestamp: Some(target),
                data_message: Some(message),
            }) = &sent.edit_message
            {
                let destination = sent_destination(sent, message)?;
                let edit = Change::Edit {
                    content: data_content(message, group_of(destination))?,
                    at: Timestamp::from_millis(message.timestamp.or(sent.timestamp)?),
                };
                (destination, None, *target, edit)
            } else {
                let message = sent.message.as_ref()?;
                let target = message.delete.as_ref()?.target_sent_timestamp?;
                (
                    sent_destination(sent, message)?,
                    None,
                    target,
                    Change::Delete,
                )
            }
        }
        _ => return None,
    };
    Some(MessageChange {
        destination,
        author,
        target: Timestamp::from_millis(target),
        change,
    })
}

/// What a data message shows; `None` for ones that show nothing, such as
/// receipts of edits, deletes and votes, which later issues handle.
fn data_content(message: &DataMessage, group: Option<GroupRef>) -> Option<model::Content> {
    if let (Some(group), Some(change)) = (
        group,
        message
            .group_v2
            .as_ref()
            .and_then(|context| context.group_change.as_deref()),
    ) {
        let updates = group_updates(group.key, change)?;
        return (!updates.is_empty()).then_some(model::Content::GroupChanged(updates));
    }
    let flags = message.flags.unwrap_or(0);
    if flags & data_message::Flags::ExpirationTimerUpdate as u32 != 0 {
        let seconds = message.expire_timer.filter(|seconds| *seconds > 0);
        return Some(model::Content::TimerChanged(seconds));
    }
    if let Some(reaction) = &message.reaction {
        return match (&reaction.emoji, reaction.remove) {
            (Some(emoji), Some(false) | None) => Some(model::Content::Reaction {
                emoji: emoji.clone(),
            }),
            _ => None,
        };
    }
    if let Some(sticker) = &message.sticker {
        return Some(model::Content::Sticker {
            emoji: sticker.emoji.clone(),
        });
    }
    if message.is_view_once == Some(true) {
        return Some(model::Content::Unsupported(Unsupported::ViewOnce));
    }
    let body = message.body.clone().unwrap_or_default();
    let attachments: Vec<_> = message.attachments.iter().map(attachment).collect();
    if !body.is_empty() || !attachments.is_empty() {
        return Some(model::Content::Text {
            body: MessageBody::new(body),
            attachments,
            quote: message.quote.as_ref().and_then(quote),
        });
    }
    let unsupported = if !message.contact.is_empty() {
        Unsupported::Contact
    } else if message.payment.is_some() {
        Unsupported::Payment
    } else if message.poll_create.is_some() {
        Unsupported::Poll
    } else if message.gift_badge.is_some() {
        Unsupported::GiftBadge
    } else {
        return None;
    };
    Some(model::Content::Unsupported(unsupported))
}

fn attachment(pointer: &AttachmentPointer) -> Attachment {
    attachment_kind(
        pointer.content_type.as_deref().unwrap_or(""),
        pointer.flags.unwrap_or(0),
        pointer.file_name.as_deref(),
    )
}

fn attachment_kind(kind: &str, flags: u32, file_name: Option<&str>) -> Attachment {
    let has = |flag: attachment_pointer::Flags| flags & flag as u32 != 0;
    if has(attachment_pointer::Flags::Gif) || kind == "image/gif" {
        Attachment::Gif
    } else if kind.starts_with("image/") {
        Attachment::Image
    } else if kind.starts_with("video/") {
        Attachment::Video
    } else if kind.starts_with("audio/") && has(attachment_pointer::Flags::VoiceMessage) {
        Attachment::Voice
    } else if kind.starts_with("audio/") {
        Attachment::Audio
    } else {
        Attachment::File {
            name: file_name.map(Into::into),
        }
    }
}

/// How a reply quotes a message; `None` authors are this account.
fn proto_quote(quote: Quote, me: uuid::Uuid) -> data_message::Quote {
    data_message::Quote {
        id: Some(quote.sent_at.as_millis()),
        author_aci: Some(quote.author.map_or(me, |author| author.uuid()).to_string()),
        text: (!quote.text.is_empty()).then_some(quote.text),
        r#type: Some(data_message::quote::Type::Normal.into()),
        ..Default::default()
    }
}

/// The message a reply quotes; `None` without its author or time.
fn quote(quote: &data_message::Quote) -> Option<Quote> {
    let author = quote
        .author_aci_binary
        .as_deref()
        .and_then(|bytes| uuid::Uuid::from_slice(bytes).ok())
        .or_else(|| quote.author_aci.as_deref()?.parse().ok())?;
    let attachment = quote.attachments.first().map(|attachment| {
        attachment_kind(
            attachment.content_type.as_deref().unwrap_or(""),
            attachment
                .thumbnail
                .as_ref()
                .and_then(|t| t.flags)
                .unwrap_or(0),
            attachment.file_name.as_deref(),
        )
    });
    Some(Quote {
        author: Some(ServiceId::new(author)),
        sent_at: Timestamp::from_millis(quote.id?),
        text: quote.text.clone().unwrap_or_default(),
        attachment,
    })
}

/// The changes in an encrypted group change; `None` if it can't be read.
fn group_updates(key: GroupMasterKey, change: &[u8]) -> Option<Vec<GroupUpdate>> {
    let change = proto::GroupChange::decode(change).ok()?;
    let params = GroupSecretParams::derive_from_master_key(
        presage::libsignal_service::prelude::GroupMasterKey::new(*key.as_bytes()),
    );
    let changes = match GroupOperations::new(params).decrypt_group_change(change) {
        Ok(changes) => changes,
        Err(error) => {
            tracing::warn!(?error, "a group change could not be decrypted");
            return None;
        }
    };
    let editor = changes.editor;
    let mut updates: Vec<_> = changes
        .changes
        .into_iter()
        .filter_map(|change| group_update(change, editor))
        .collect();
    updates.dedup();
    Some(updates)
}

/// How a change reads in the log; `None` for ones not worth a line.
fn group_update(change: GroupChange, editor: Aci) -> Option<GroupUpdate> {
    let id = |aci: Aci| ServiceId::new(aci.into());
    let editor_id = SignalServiceId::from(editor);
    Some(match change {
        GroupChange::NewMember(member) if member.aci == editor => GroupUpdate::Joined,
        GroupChange::NewMember(member) => GroupUpdate::Added(id(member.aci)),
        GroupChange::DeleteMember(aci) if aci == editor => GroupUpdate::Left,
        GroupChange::DeleteMember(aci) => GroupUpdate::Removed(id(aci)),
        GroupChange::ModifyMemberRole { aci, role } => GroupUpdate::Admin {
            member: id(aci),
            admin: role == Role::Administrator,
        },
        GroupChange::NewPendingMember(pending) => {
            GroupUpdate::Invited(ServiceId::new(pending.address.raw_uuid()))
        }
        GroupChange::PromotePendingMember { address, .. } if address == editor_id => {
            GroupUpdate::Joined
        }
        GroupChange::PromotePendingMember { address, .. } => {
            GroupUpdate::Added(ServiceId::new(address.raw_uuid()))
        }
        GroupChange::NewRequestingMember(_) => GroupUpdate::Requested,
        GroupChange::PromoteRequestingMember { aci, .. } => GroupUpdate::Added(id(aci)),
        GroupChange::Title(title) => GroupUpdate::Title(title),
        GroupChange::Description(_) => GroupUpdate::Description,
        GroupChange::Avatar(_) => GroupUpdate::Avatar,
        GroupChange::Timer(timer) => GroupUpdate::Timer(
            timer
                .map(|timer| timer.duration)
                .filter(|seconds| *seconds > 0),
        ),
        GroupChange::AttributeAccess(_)
        | GroupChange::MemberAccess(_)
        | GroupChange::InviteLinkAccess(_)
        | GroupChange::InviteLinkPassword(_)
        | GroupChange::AnnouncementOnly(_) => GroupUpdate::Settings,
        _ => return None,
    })
}

/// The group a message belongs to; `None` if its master key is malformed.
fn group_ref(context: &GroupContextV2) -> Option<GroupRef> {
    let key = <[u8; 32]>::try_from(context.master_key.as_deref()?).ok()?;
    Some(GroupRef {
        key: GroupMasterKey::new(key),
        revision: context.revision.unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use presage::libsignal_service::groups_v2::Timer;
    use presage::libsignal_service::proto::{call_message, group_change};
    use presage::libsignal_service::protocol::DeviceId;
    use uuid::Uuid;

    use super::*;

    const SENDER: Uuid = Uuid::from_u128(0x1234);
    const OTHER: Uuid = Uuid::from_u128(0x4321);
    const KEY: [u8; 32] = [9; 32];

    fn content(body: ContentBody) -> Content {
        let at = Utc.timestamp_millis_opt(1_700_000_000_123).unwrap();
        Content {
            metadata: presage::libsignal_service::content::Metadata {
                sender: Aci::from(SENDER).into(),
                destination: Aci::from(Uuid::from_u128(0x99)).into(),
                sender_device: DeviceId::try_from(1u32).unwrap(),
                pni_verified: None,
                client_timestamp: at,
                server_timestamp: at,
                needs_receipt: false,
                unidentified_sender: true,
                was_plaintext: false,
                server_guid: None,
            },
            body,
        }
    }

    fn data(message: DataMessage) -> Option<model::Content> {
        received_message(&content(ContentBody::DataMessage(message))).map(|m| m.content)
    }

    fn text(body: &str, timestamp: Option<u64>) -> ContentBody {
        ContentBody::DataMessage(DataMessage {
            body: Some(body.into()),
            timestamp,
            ..Default::default()
        })
    }

    fn pointer(content_type: &str, flags: u32, name: Option<&str>) -> AttachmentPointer {
        AttachmentPointer {
            content_type: Some(content_type.into()),
            flags: Some(flags),
            file_name: name.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn maps_one_to_one_text() {
        let received = Received::Content(Box::new(content(text("hi", Some(42)))));

        assert_eq!(
            incoming(received),
            Some(Incoming::Message(IncomingMessage {
                sender: ServiceId::new(SENDER),
                group: None,
                timestamp: Timestamp::from_millis(42),
                content: model::Content::text("hi"),
                expire_timer: None,
            }))
        );
    }

    #[test]
    fn messages_carry_their_disappearing_timer() {
        let timed = |seconds| {
            let body = ContentBody::DataMessage(DataMessage {
                body: Some("hi".into()),
                expire_timer: Some(seconds),
                ..Default::default()
            });
            received_message(&content(body)).unwrap().expire_timer
        };
        assert_eq!(timed(3600), Some(3600));
        assert_eq!(timed(0), None);

        let body = sent(Sent {
            destination_service_id: Some(RECIPIENT.to_string()),
            timestamp: Some(77),
            message: Some(DataMessage {
                body: Some("from phone".into()),
                expire_timer: Some(60),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(sent_message(&content(body)).unwrap().expire_timer, Some(60));
    }

    #[test]
    fn falls_back_to_client_timestamp() {
        let message = received_message(&content(text("hi", None))).unwrap();
        assert_eq!(message.timestamp, Timestamp::from_millis(1_700_000_000_123));
    }

    #[test]
    fn attachments_keep_their_caption() {
        let message = DataMessage {
            body: Some("the graph".into()),
            attachments: vec![
                pointer("image/png", 0, None),
                pointer("image/gif", 0, None),
                pointer("video/mp4", 8, None),
                pointer("video/mp4", 0, None),
                pointer("audio/aac", 1, None),
                pointer("audio/mpeg", 0, None),
                pointer("application/pdf", 0, Some("report.pdf")),
            ],
            ..Default::default()
        };

        assert_eq!(
            data(message),
            Some(model::Content::Text {
                body: MessageBody::new("the graph"),
                attachments: vec![
                    Attachment::Image,
                    Attachment::Gif,
                    Attachment::Gif,
                    Attachment::Video,
                    Attachment::Voice,
                    Attachment::Audio,
                    Attachment::File {
                        name: Some("report.pdf".into())
                    },
                ],
                quote: None,
            })
        );
    }

    #[test]
    fn attachment_without_text() {
        let message = DataMessage {
            attachments: vec![pointer("image/jpeg", 0, None)],
            ..Default::default()
        };
        assert_eq!(
            data(message),
            Some(model::Content::Text {
                body: MessageBody::new(""),
                attachments: vec![Attachment::Image],
                quote: None,
            })
        );
    }

    #[test]
    fn stickers_reactions_and_unsupported_content() {
        let sticker = DataMessage {
            sticker: Some(data_message::Sticker {
                emoji: Some("🐱".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            data(sticker),
            Some(model::Content::Sticker {
                emoji: Some("🐱".into())
            })
        );

        let reaction = |remove| DataMessage {
            reaction: Some(data_message::Reaction {
                emoji: Some("👍".into()),
                remove: Some(remove),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            data(reaction(false)),
            Some(model::Content::Reaction {
                emoji: "👍".into()
            })
        );
        assert_eq!(data(reaction(true)), None);

        let view_once = DataMessage {
            is_view_once: Some(true),
            attachments: vec![pointer("image/jpeg", 0, None)],
            ..Default::default()
        };
        let contact = DataMessage {
            contact: vec![data_message::Contact::default()],
            ..Default::default()
        };
        let payment = DataMessage {
            payment: Some(data_message::Payment::default()),
            ..Default::default()
        };
        let poll = DataMessage {
            poll_create: Some(data_message::PollCreate::default()),
            ..Default::default()
        };
        let gift = DataMessage {
            gift_badge: Some(data_message::GiftBadge::default()),
            ..Default::default()
        };
        for (message, kind) in [
            (view_once, Unsupported::ViewOnce),
            (contact, Unsupported::Contact),
            (payment, Unsupported::Payment),
            (poll, Unsupported::Poll),
            (gift, Unsupported::GiftBadge),
        ] {
            assert_eq!(data(message), Some(model::Content::Unsupported(kind)));
        }
    }

    #[test]
    fn timer_updates() {
        let update = |seconds| DataMessage {
            flags: Some(data_message::Flags::ExpirationTimerUpdate as u32),
            expire_timer: Some(seconds),
            ..Default::default()
        };
        assert_eq!(
            data(update(604_800)),
            Some(model::Content::TimerChanged(Some(604_800)))
        );
        assert_eq!(data(update(0)), Some(model::Content::TimerChanged(None)));
    }

    #[test]
    fn call_offers() {
        let call = |kind: offer::Type| {
            let body = ContentBody::CallMessage(CallMessage {
                offer: Some(call_message::Offer {
                    r#type: Some(kind as i32),
                    ..Default::default()
                }),
                ..Default::default()
            });
            received_message(&content(body)).map(|message| message.content)
        };
        assert_eq!(
            call(offer::Type::OfferAudioCall),
            Some(model::Content::Call { video: false })
        );
        assert_eq!(
            call(offer::Type::OfferVideoCall),
            Some(model::Content::Call { video: true })
        );
        let hangup = ContentBody::CallMessage(CallMessage {
            hangup: Some(call_message::Hangup::default()),
            ..Default::default()
        });
        assert_eq!(received_message(&content(hangup)), None);
    }

    #[test]
    fn skips_messages_that_show_nothing() {
        let empty = ContentBody::DataMessage(DataMessage::default());
        assert_eq!(received_message(&content(empty)), None);
        let delete = DataMessage {
            delete: Some(data_message::Delete::default()),
            ..Default::default()
        };
        assert_eq!(data(delete), None);
        let typing = ContentBody::TypingMessage(TypingMessage::default());
        assert_eq!(received_message(&content(typing)), None);
    }

    fn group_context(key: Option<Vec<u8>>) -> Option<GroupContextV2> {
        Some(GroupContextV2 {
            master_key: key,
            revision: Some(3),
            ..Default::default()
        })
    }

    #[test]
    fn maps_group_text() {
        let body = ContentBody::DataMessage(DataMessage {
            body: Some("hi all".into()),
            group_v2: group_context(Some(KEY.to_vec())),
            ..Default::default()
        });

        let message = received_message(&content(body)).unwrap();

        assert_eq!(message.sender, ServiceId::new(SENDER));
        assert_eq!(
            message.group,
            Some(GroupRef {
                key: GroupMasterKey::new(KEY),
                revision: 3,
            })
        );
    }

    #[test]
    fn skips_group_messages_with_a_bad_master_key() {
        for key in [None, Some(vec![9; 31])] {
            let body = ContentBody::DataMessage(DataMessage {
                body: Some("hi all".into()),
                group_v2: group_context(key),
                ..Default::default()
            });
            assert_eq!(received_message(&content(body)), None);
        }
    }

    /// An encrypted group change by `editor` renaming the group and
    /// removing [`OTHER`].
    fn encrypted_change(editor: Uuid, title: &str) -> Vec<u8> {
        let params = GroupSecretParams::derive_from_master_key(
            presage::libsignal_service::prelude::GroupMasterKey::new(KEY),
        );
        let encrypt = |uuid: Uuid| {
            let id = SignalServiceId::from(Aci::from(uuid));
            presage::libsignal_service::zkgroup::serialize(&params.encrypt_service_id(id))
        };
        let actions = group_change::Actions {
            group_id: params.get_group_identifier().to_vec(),
            source_user_id: encrypt(editor),
            version: 4,
            modify_title: Some(group_change::actions::ModifyTitleAction {
                title: GroupOperations::new(params).encrypt_title(title, &mut rand::rng()),
            }),
            delete_members: vec![group_change::actions::DeleteMemberAction {
                deleted_user_id: encrypt(OTHER),
            }],
            ..Default::default()
        };
        proto::GroupChange {
            actions: actions.encode_to_vec(),
            ..Default::default()
        }
        .encode_to_vec()
    }

    #[test]
    fn group_changes_become_updates() {
        let message = DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(KEY.to_vec()),
                revision: Some(4),
                group_change: Some(encrypted_change(SENDER, "Dev team")),
            }),
            ..Default::default()
        };

        assert_eq!(
            data(message),
            Some(model::Content::GroupChanged(vec![
                GroupUpdate::Removed(ServiceId::new(OTHER)),
                GroupUpdate::Title("Dev team".into()),
            ]))
        );
    }

    #[test]
    fn unreadable_group_changes_are_skipped() {
        let message = DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(KEY.to_vec()),
                revision: Some(4),
                group_change: Some(vec![1, 2, 3]),
            }),
            ..Default::default()
        };
        assert_eq!(data(message), None);
    }

    #[test]
    fn group_update_mapping() {
        let editor = Aci::from(SENDER);
        let other = Aci::from(OTHER);
        let member = |aci| presage::libsignal_service::groups_v2::Member {
            aci,
            role: Role::Default,
            profile_key: presage::libsignal_service::prelude::ProfileKey::create([0; 32]),
            joined_at_version: 0,
            label: None,
            label_emoji: None,
        };
        let cases = [
            (
                GroupChange::NewMember(member(editor)),
                Some(GroupUpdate::Joined),
            ),
            (
                GroupChange::NewMember(member(other)),
                Some(GroupUpdate::Added(ServiceId::new(OTHER))),
            ),
            (GroupChange::DeleteMember(editor), Some(GroupUpdate::Left)),
            (
                GroupChange::ModifyMemberRole {
                    aci: other,
                    role: Role::Administrator,
                },
                Some(GroupUpdate::Admin {
                    member: ServiceId::new(OTHER),
                    admin: true,
                }),
            ),
            (
                GroupChange::Timer(Some(Timer { duration: 3600 })),
                Some(GroupUpdate::Timer(Some(3600))),
            ),
            (GroupChange::Timer(None), Some(GroupUpdate::Timer(None))),
            (
                GroupChange::Description(None),
                Some(GroupUpdate::Description),
            ),
            (
                GroupChange::AnnouncementOnly(true),
                Some(GroupUpdate::Settings),
            ),
            (
                GroupChange::ModifyMemberProfileKey {
                    aci: other,
                    profile_key: presage::libsignal_service::prelude::ProfileKey::create([0; 32]),
                },
                None,
            ),
        ];
        for (change, expected) in cases {
            assert_eq!(group_update(change, editor), expected);
        }
    }

    fn received(body: ContentBody) -> Option<Incoming> {
        incoming(Received::Content(Box::new(content(body))))
    }

    #[test]
    fn maps_receipts() {
        let receipt = |kind: receipt_message::Type| {
            received(ContentBody::ReceiptMessage(ReceiptMessage {
                r#type: Some(kind.into()),
                timestamp: vec![10, 11],
            }))
        };
        let expected = |kind| {
            Some(Incoming::Receipt {
                sender: ServiceId::new(SENDER),
                kind,
                timestamps: vec![Timestamp::from_millis(10), Timestamp::from_millis(11)],
            })
        };
        assert_eq!(
            receipt(receipt_message::Type::Delivery),
            expected(ReceiptKind::Delivered)
        );
        assert_eq!(
            receipt(receipt_message::Type::Read),
            expected(ReceiptKind::Read)
        );
        assert_eq!(
            receipt(receipt_message::Type::Viewed),
            expected(ReceiptKind::Read)
        );
    }

    #[test]
    fn maps_typing() {
        let typing = |action: typing_message::Action, group_id: Option<Vec<u8>>| {
            received(ContentBody::TypingMessage(TypingMessage {
                timestamp: Some(1),
                action: Some(action.into()),
                group_id,
            }))
        };
        assert_eq!(
            typing(typing_message::Action::Started, None),
            Some(Incoming::Typing {
                sender: ServiceId::new(SENDER),
                group: None,
                typing: true,
            })
        );
        assert_eq!(
            typing(typing_message::Action::Stopped, Some(vec![4; 32])),
            Some(Incoming::Typing {
                sender: ServiceId::new(SENDER),
                group: Some(GroupId::new([4; 32])),
                typing: false,
            })
        );
        assert_eq!(
            typing(typing_message::Action::Started, Some(vec![4; 3])),
            None
        );
    }

    #[test]
    fn maps_reads_and_settings_from_other_devices() {
        let read = received(ContentBody::SynchronizeMessage(SyncMessage {
            read: vec![
                sync_message::Read {
                    sender_aci: Some(OTHER.to_string()),
                    timestamp: Some(5),
                    ..Default::default()
                },
                sync_message::Read {
                    sender_aci_binary: Some(SENDER.as_bytes().to_vec()),
                    timestamp: Some(6),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }));
        assert_eq!(
            read,
            Some(Incoming::ReadElsewhere {
                read: vec![
                    (ServiceId::new(OTHER), Timestamp::from_millis(5)),
                    (ServiceId::new(SENDER), Timestamp::from_millis(6)),
                ],
                at: Timestamp::from_millis(1_700_000_000_123),
            })
        );

        let settings = received(ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Configuration(sync_message::Configuration {
                read_receipts: Some(true),
                typing_indicators: Some(false),
                ..Default::default()
            })),
            ..Default::default()
        }));
        assert_eq!(
            settings,
            Some(Incoming::Settings {
                read_receipts: Some(true),
                typing_indicators: Some(false),
            })
        );
    }

    #[test]
    fn replies_carry_their_quote() {
        let message = DataMessage {
            body: Some("agreed".into()),
            quote: Some(data_message::Quote {
                id: Some(50),
                author_aci: Some(OTHER.to_string()),
                text: Some("ship it?".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let attachment_only = data_message::Quote {
            id: Some(51),
            author_aci_binary: Some(OTHER.as_bytes().to_vec()),
            attachments: vec![data_message::quote::QuotedAttachment {
                content_type: Some("image/jpeg".into()),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(
            data(message),
            Some(model::Content::Text {
                body: MessageBody::new("agreed"),
                attachments: Vec::new(),
                quote: Some(Quote {
                    author: Some(ServiceId::new(OTHER)),
                    sent_at: Timestamp::from_millis(50),
                    text: "ship it?".into(),
                    attachment: None,
                }),
            })
        );
        let quoted = quote(&attachment_only).unwrap();
        assert_eq!(quoted.text, "");
        assert_eq!(quoted.attachment, Some(Attachment::Image));
        assert_eq!(quote(&data_message::Quote::default()), None);
    }

    #[test]
    fn sent_quotes_name_their_author() {
        let quote = |author| Quote {
            author,
            sent_at: Timestamp::from_millis(50),
            text: "ship it?".into(),
            attachment: None,
        };
        let theirs = proto_quote(quote(Some(ServiceId::new(OTHER))), SENDER);
        assert_eq!(theirs.author_aci, Some(OTHER.to_string()));
        assert_eq!(theirs.id, Some(50));
        assert_eq!(theirs.text.as_deref(), Some("ship it?"));
        let ours = proto_quote(quote(None), SENDER);
        assert_eq!(
            ours.author_aci,
            Some(SENDER.to_string()),
            "we are the author"
        );
        let roundtrip = super::quote(&ours).unwrap();
        assert_eq!(roundtrip.author, Some(ServiceId::new(SENDER)));
    }

    #[test]
    fn maps_deletes_and_edits() {
        let delete = ContentBody::DataMessage(DataMessage {
            delete: Some(data_message::Delete {
                target_sent_timestamp: Some(40),
            }),
            group_v2: group_context(Some(KEY.to_vec())),
            ..Default::default()
        });
        assert_eq!(
            received(delete),
            Some(Incoming::Change(MessageChange {
                destination: Destination::Group(GroupRef {
                    key: GroupMasterKey::new(KEY),
                    revision: 3,
                }),
                author: Some(ServiceId::new(SENDER)),
                target: Timestamp::from_millis(40),
                change: Change::Delete,
            }))
        );

        let edit = ContentBody::EditMessage(EditMessage {
            target_sent_timestamp: Some(40),
            data_message: Some(DataMessage {
                body: Some("fixed".into()),
                timestamp: Some(45),
                ..Default::default()
            }),
        });
        assert_eq!(
            received(edit),
            Some(Incoming::Change(MessageChange {
                destination: Destination::Contact(ServiceId::new(SENDER)),
                author: Some(ServiceId::new(SENDER)),
                target: Timestamp::from_millis(40),
                change: Change::Edit {
                    content: model::Content::text("fixed"),
                    at: Timestamp::from_millis(45),
                },
            }))
        );
    }

    #[test]
    fn maps_edits_and_deletes_from_the_phone() {
        let recipient: SignalServiceId = Aci::from(RECIPIENT).into();
        let edit = sent(Sent {
            destination_service_id_binary: Some(recipient.service_id_binary()),
            timestamp: Some(90),
            edit_message: Some(EditMessage {
                target_sent_timestamp: Some(80),
                data_message: sent_data("better", false),
            }),
            ..Default::default()
        });
        assert_eq!(
            received(edit),
            Some(Incoming::Change(MessageChange {
                destination: Destination::Contact(ServiceId::new(RECIPIENT)),
                author: None,
                target: Timestamp::from_millis(80),
                change: Change::Edit {
                    content: model::Content::text("better"),
                    at: Timestamp::from_millis(90),
                },
            }))
        );

        let delete = sent(Sent {
            timestamp: Some(91),
            message: Some(DataMessage {
                delete: Some(data_message::Delete {
                    target_sent_timestamp: Some(80),
                }),
                group_v2: group_context(Some(KEY.to_vec())),
                ..Default::default()
            }),
            ..Default::default()
        });
        let Some(Incoming::Change(change)) = received(delete) else {
            panic!("expected a change");
        };
        assert_eq!(change.author, None);
        assert_eq!(change.change, Change::Delete);
        assert!(matches!(change.destination, Destination::Group(_)));
    }

    #[test]
    fn maps_deletes_for_me() {
        use presage::libsignal_service::proto::sync_message::{DeleteForMe, delete_for_me};

        let contact = ConversationIdentifier {
            identifier: Some(conversation_identifier::Identifier::ThreadServiceId(
                OTHER.to_string(),
            )),
        };
        let group = ConversationIdentifier {
            identifier: Some(conversation_identifier::Identifier::ThreadGroupId(vec![
                4;
                32
            ])),
        };
        let phone_only = ConversationIdentifier {
            identifier: Some(conversation_identifier::Identifier::ThreadE164(
                "+15550100".into(),
            )),
        };
        let message = |author: Uuid, sent_at| AddressableMessage {
            sent_timestamp: Some(sent_at),
            author: Some(addressable_message::Author::AuthorServiceIdBinary(
                SignalServiceId::from(Aci::from(author)).service_id_binary(),
            )),
        };
        let delete = DeleteForMe {
            message_deletes: vec![
                delete_for_me::MessageDeletes {
                    conversation: Some(contact.clone()),
                    messages: vec![message(OTHER, 10), message(SENDER, 11)],
                },
                delete_for_me::MessageDeletes {
                    conversation: Some(phone_only.clone()),
                    messages: vec![message(OTHER, 12)],
                },
            ],
            conversation_deletes: vec![delete_for_me::ConversationDelete {
                conversation: Some(group),
                ..Default::default()
            }],
            local_only_conversation_deletes: vec![delete_for_me::LocalOnlyConversationDelete {
                conversation: Some(phone_only),
            }],
            ..Default::default()
        };

        let chat = ChatRef::Contact(ServiceId::new(OTHER));
        assert_eq!(
            received(ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::DeleteForMe(delete)),
                ..Default::default()
            })),
            Some(Incoming::DeletedForMe {
                messages: vec![
                    MessageRef {
                        chat,
                        author: Some(ServiceId::new(OTHER)),
                        sent_at: Timestamp::from_millis(10),
                    },
                    MessageRef {
                        chat,
                        author: Some(ServiceId::new(SENDER)),
                        sent_at: Timestamp::from_millis(11),
                    },
                ],
                conversations: vec![ChatRef::Group(GroupId::new([4; 32]))],
            })
        );
    }

    #[test]
    fn maps_queue_empty_and_contacts_and_drops_the_rest() {
        assert_eq!(incoming(Received::QueueEmpty), Some(Incoming::QueueEmpty));
        assert_eq!(incoming(Received::Contacts), Some(Incoming::ContactsSynced));
        assert_eq!(
            incoming(Received::DecryptionError(Aci::from(SENDER).into())),
            None
        );
    }

    const RECIPIENT: Uuid = Uuid::from_u128(0x5678);

    fn sent(sent: Sent) -> ContentBody {
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Sent(sent)),
            ..Default::default()
        })
    }

    fn sent_data(body: &str, group: bool) -> Option<DataMessage> {
        Some(DataMessage {
            body: Some(body.into()),
            group_v2: if group {
                group_context(Some(KEY.to_vec()))
            } else {
                None
            },
            ..Default::default()
        })
    }

    #[test]
    fn maps_text_sent_from_the_phone() {
        let recipient: SignalServiceId = Aci::from(RECIPIENT).into();
        let body = sent(Sent {
            destination_service_id_binary: Some(recipient.service_id_binary()),
            timestamp: Some(77),
            message: sent_data("from phone", false),
            ..Default::default()
        });

        assert_eq!(
            incoming(Received::Content(Box::new(content(body)))),
            Some(Incoming::Sent(SentMessage {
                destination: Destination::Contact(ServiceId::new(RECIPIENT)),
                timestamp: Timestamp::from_millis(77),
                content: model::Content::text("from phone"),
                expire_timer: None,
            }))
        );
    }

    #[test]
    fn sent_recipient_can_be_a_string() {
        let body = sent(Sent {
            destination_service_id: Some(RECIPIENT.to_string()),
            timestamp: Some(77),
            message: sent_data("x", false),
            ..Default::default()
        });

        let sent = sent_message(&content(body)).unwrap();
        assert_eq!(
            sent.destination,
            Destination::Contact(ServiceId::new(RECIPIENT))
        );
    }

    #[test]
    fn maps_group_text_sent_from_the_phone() {
        let body = sent(Sent {
            timestamp: Some(77),
            message: sent_data("to all", true),
            ..Default::default()
        });

        let sent = sent_message(&content(body)).unwrap();

        assert_eq!(
            sent.destination,
            Destination::Group(GroupRef {
                key: GroupMasterKey::new(KEY),
                revision: 3,
            })
        );
    }

    #[test]
    fn skips_sent_messages_without_a_recipient() {
        let no_recipient = sent(Sent {
            timestamp: Some(77),
            message: sent_data("x", false),
            ..Default::default()
        });

        assert_eq!(sent_message(&content(no_recipient)), None);
    }

    #[tokio::test]
    async fn fresh_database_has_no_account() {
        let dir = tempfile::tempdir().unwrap();
        let key = crate::storage::DatabaseKey::generate();
        let db = Database::open(&dir.path().join("test.db"), &key)
            .await
            .unwrap();

        let loaded = PresageLinker.load(&db).await.unwrap();

        assert!(loaded.is_none());
        db.close().await;
    }

    #[tokio::test]
    async fn store_keeps_no_messages_but_delegates_the_rest() {
        use presage::store::{ContentsStore as _, Thread};

        let dir = tempfile::tempdir().unwrap();
        let key = crate::storage::DatabaseKey::generate();
        let db = Database::open(&dir.path().join("test.db"), &key)
            .await
            .unwrap();
        let store = AppStore::new(db.presage_store().clone());
        let thread = Thread::Contact(Aci::from(SENDER).into());

        store
            .save_message(&thread, content(text("hi", Some(42))))
            .await
            .unwrap();

        assert_eq!(store.messages(&thread, ..).await.unwrap().count(), 0);
        assert!(!store.is_registered().await);
        db.close().await;
    }

    #[test]
    fn connection_failures_are_network_errors() {
        type Error = presage::Error<std::io::Error>;
        let closing = || ServiceError::WsClosing { reason: "gone" };

        assert!(presage_error(Error::ServiceError(closing())).is_network());
        assert!(
            presage_error(Error::from(MessageSenderError::ServiceError(closing()))).is_network()
        );
        assert!(!presage_error(Error::ServiceError(ServiceError::Unauthorized)).is_network());
        assert!(presage_error(Error::ServiceError(ServiceError::Unauthorized)).is_unauthorized());
        let handshake = |status: u16| {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            let error = reqwest_websocket::HandshakeError::UnexpectedStatusCode(status);
            ServiceError::WsError(Box::new(error.into()))
        };
        assert!(presage_error(Error::ServiceError(handshake(401))).is_unauthorized());
        assert!(presage_error(Error::ServiceError(handshake(403))).is_unauthorized());
        assert!(!presage_error(Error::ServiceError(handshake(500))).is_unauthorized());
        assert!(!presage_error(Error::ServiceError(closing())).is_unauthorized());
        assert!(
            !presage_error(Error::from(MessageSenderError::MaximumRetriesLimitExceeded))
                .is_network()
        );
    }

    /// Relays `stream`, asking it to stop after `stop_after`; returns when
    /// each item, and the end, came through.
    async fn relayed(
        stream: impl Stream<Item = Received>,
        stop_after: Duration,
    ) -> (Vec<Duration>, Duration) {
        let start = Instant::now();
        let (items_tx, mut items) = mpsc::channel(RECEIVE_BUFFER);
        let (stop, stop_rx) = oneshot::channel();
        let stopper = async {
            tokio::time::sleep(stop_after).await;
            stop.send(()).ok();
        };
        let collect = async {
            let mut arrived = Vec::new();
            while items.recv().await.is_some() {
                arrived.push(start.elapsed());
            }
            (arrived, start.elapsed())
        };
        let ((), (), relayed) = tokio::join!(relay(stream, items_tx, stop_rx), stopper, collect);
        relayed
    }

    /// `count` messages, one per `period`, on a connection that stays open.
    fn arrivals(period: Duration, count: usize) -> impl Stream<Item = Received> {
        futures::stream::unfold(0, move |n| async move {
            if n == count {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(period).await;
            Some((Received::QueueEmpty, n + 1))
        })
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[tokio::test(start_paused = true)]
    async fn relays_until_the_connection_closes() {
        let stream = futures::stream::iter([Received::QueueEmpty, Received::QueueEmpty]);

        let (arrived, ended) = relayed(stream, 3600 * SECOND).await;

        assert_eq!(arrived, [Duration::ZERO; 2]);
        assert_eq!(ended, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_lets_messages_already_received_through() {
        let received = futures::stream::iter([Received::QueueEmpty, Received::QueueEmpty]);
        let stream = received.chain(futures::stream::pending());

        let (arrived, ended) = relayed(stream, Duration::ZERO).await;

        assert_eq!(arrived.len(), 2);
        assert_eq!(ended, DRAIN_IDLE);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_waits_for_a_message_being_decrypted() {
        let (arrived, ended) = relayed(arrivals(SECOND, 1), Duration::ZERO).await;

        assert_eq!(arrived, [SECOND]);
        assert_eq!(ended, SECOND + DRAIN_IDLE);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_an_idle_connection_ends_at_once() {
        let (arrived, ended) = relayed(futures::stream::pending(), 10 * SECOND).await;

        assert_eq!(arrived, []);
        assert_eq!(ended, 10 * SECOND);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_reads_for_a_limited_time_then_flushes_acknowledgements() {
        let (arrived, ended) = relayed(arrivals(SECOND, usize::MAX), Duration::ZERO).await;

        assert_eq!(arrived.len(), 5);
        assert_eq!(arrived.last(), Some(&DRAIN_LIMIT));
        assert_eq!(ended, DRAIN_LIMIT + ACK_FLUSH);
    }

    /// Linking generates Kyber prekeys deep in async frames; leave them
    /// three quarters of the core stack.
    #[test]
    fn kyber_prekeys_fit_on_the_core_stack() {
        use presage::libsignal_service::protocol::{IdentityKeyPair, KyberPreKeyRecord, kem};

        std::thread::Builder::new()
            .stack_size(crate::actor::STACK_SIZE / 4)
            .spawn(|| {
                let identity = IdentityKeyPair::generate(&mut rand::rng());
                KyberPreKeyRecord::generate(
                    kem::KeyType::Kyber1024,
                    1.into(),
                    identity.private_key(),
                )
                .unwrap();
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
