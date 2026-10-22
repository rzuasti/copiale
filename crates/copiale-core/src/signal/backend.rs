//! The [`Linker`] and the [`SignalBackend`] of a linked account.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use futures::future::LocalBoxFuture;
use futures::stream::{LocalBoxStream, Stream};
use futures::{FutureExt as _, StreamExt as _};
use presage::Manager;
use presage::libsignal_service::configuration::{ServiceConfiguration, SignalServers};
use presage::libsignal_service::content::ContentBody;
use presage::libsignal_service::prelude::ProtobufMessage as _;
use presage::libsignal_service::proto::manifest_record::identifier;
use presage::libsignal_service::proto::sync_message::{
    self, Content as SyncContent, message_request_response,
};
use presage::libsignal_service::proto::{
    self, AddressableMessage, ContactRecord, ConversationIdentifier, DataMessage, EditMessage,
    GroupContextV2, ReceiptMessage, StorageRecord, SyncMessage, TypingMessage, addressable_message,
    contact_record, conversation_identifier, data_message, receipt_message, storage_record,
    typing_message,
};
use presage::libsignal_service::protocol::{
    Aci, DecryptionErrorMessage, Fingerprint, IdentityKeyStore as _, ProtocolAddress,
    ServiceId as SignalServiceId,
};
use presage::libsignal_service::push_service::DEFAULT_DEVICE_ID;
use presage::libsignal_service::zkgroup::ServerPublicParams;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentsStore as _, StateStore as _, Store as _, Thread};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};

use super::errors::{presage_error, storage_service_error, store_error};
use super::incoming::{identity_key, incoming, service_id};
use super::store::AppStore;
use crate::backend::{
    Ack, BackendError, Incoming, Linker, PeerIdentity, Resendable, RetryRequest, SignalBackend,
    StoredChats,
};
use crate::model::{
    BlockedList, ChatRef, Destination, GroupInfo, GroupMasterKey, GroupMembers, IdentityKey,
    Member, MessageBody, Outgoing, Quote, RequestResponse, SafetyNumber, ServiceId, StoredChat,
    StoredIdentity, Timestamp,
};
use crate::storage::{Database, synced};
use crate::text::clean_name;

/// Loads or links the account in the app's database.
#[derive(Default)]
pub struct PresageLinker;

impl Linker for PresageLinker {
    type Backend = PresageBackend;

    async fn load(&mut self, db: &Database) -> Result<Option<PresageBackend>, BackendError> {
        let (identity_changes_tx, identity_changes) = unbounded_channel();
        let store = AppStore::new(
            db.presage_store().clone(),
            db.verified_keys(),
            db.peer_timers(),
            identity_changes_tx,
        );
        if !store.is_registered().await {
            return Ok(None);
        }
        let manager = Manager::load_registered(store)
            .await
            .map_err(presage_error)?;
        Ok(Some(PresageBackend::new(manager, identity_changes)))
    }

    async fn link(
        &mut self,
        db: &Database,
        device_name: &str,
        url: oneshot::Sender<String>,
    ) -> Result<PresageBackend, BackendError> {
        let (identity_changes_tx, identity_changes) = unbounded_channel();
        let store = AppStore::new(
            db.presage_store().clone(),
            db.verified_keys(),
            db.peer_timers(),
            identity_changes_tx,
        );
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
        Ok(PresageBackend::new(manager, identity_changes))
    }
}

/// A linked account.
pub struct PresageBackend {
    manager: Manager<AppStore, Registered>,
    receiver: Option<Receiver>,
    /// Until [`SignalBackend::identity_changes`] takes it.
    identity_changes: Option<UnboundedReceiver<ServiceId>>,
    profile_fetches: ProfileFetches,
}

/// The task receiving on the open connection.
struct Receiver {
    task: JoinHandle<()>,
    /// Asks it to stop; see [`relay`].
    stop: Option<oneshot::Sender<()>>,
}

impl PresageBackend {
    fn new(
        manager: Manager<AppStore, Registered>,
        identity_changes: UnboundedReceiver<ServiceId>,
    ) -> Self {
        Self {
            manager,
            receiver: None,
            identity_changes: Some(identity_changes),
            profile_fetches: ProfileFetches::default(),
        }
    }
}

impl PresageBackend {
    /// Sends `sync` to this account's other devices.
    fn send_sync(&self, sync: SyncMessage) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        async move {
            let me = manager.registration_data().service_ids.aci();
            manager
                .send_message(me, sync, Timestamp::now().as_millis())
                .await
                .map(drop)
                .map_err(presage_error)
        }
        .boxed_local()
    }
}

impl Drop for PresageBackend {
    fn drop(&mut self) {
        if let Some(receiver) = self.receiver.take() {
            receiver.task.abort();
        }
    }
}

// Timestamps of syncs, receipts and typing messages made here use the
// system clock, not `CoreOptions::clock`: they go to Signal.
impl SignalBackend for PresageBackend {
    /// Receives on a clone of the manager in a local task, so sending
    /// doesn't wait for the stream. Must run inside a `LocalSet`.
    fn receive(
        &mut self,
    ) -> LocalBoxFuture<'static, Result<LocalBoxStream<'static, (Incoming, Ack)>, BackendError>>
    {
        let mut manager = self.manager.clone();
        let me = manager.registration_data().service_ids.aci();
        let server = ServiceConfiguration::from(manager.registration_data().signal_servers)
            .zkgroup_server_public_params;
        let (opened_tx, opened_rx) = oneshot::channel();
        let (items_tx, items_rx) = mpsc::channel(RECEIVE_BUFFER);
        let (stop, stop_rx) = oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            let stream = match manager.receive_messages_unacknowledged().await {
                Ok(stream) => {
                    opened_tx.send(Ok(())).ok();
                    stream
                }
                Err(error) => {
                    opened_tx.send(Err(presage_error(error))).ok();
                    return;
                }
            };
            let stream = stream.map(|(received, acknowledgement)| {
                let ack = Ack::new(move || {
                    if let Err(error) = acknowledgement.acknowledge() {
                        tracing::debug!(%error, "acknowledging a message failed");
                    }
                });
                (received, ack)
            });
            relay(stream, me, &server, items_tx, stop_rx).await;
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
    ) -> LocalBoxFuture<'static, Result<Resendable, BackendError>> {
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
            let sent = match destination {
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
            .map_err(presage_error)?;
            Ok(Resendable {
                recipients: sent
                    .recipients
                    .iter()
                    .map(|(recipient, devices)| {
                        let devices = devices.iter().map(|device| u32::from(*device)).collect();
                        (ServiceId::new(recipient.raw_uuid()), devices)
                    })
                    .collect(),
                content: sent.content.encode_to_vec(),
            })
        }
        .boxed_local()
    }

    fn send_retry_request(
        &mut self,
        sender: ServiceId,
        retry: RetryRequest,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        async move {
            let message = DecryptionErrorMessage::try_from(retry.message())
                .map_err(|error| BackendError::new(format!("invalid retry request: {error}")))?;
            let sender = Aci::from(sender.uuid()).into();
            manager
                .send_retry_request(&sender, message, retry.group_id())
                .await
                .map_err(presage_error)
        }
        .boxed_local()
    }

    fn resend(
        &mut self,
        recipient: ServiceId,
        content: Vec<u8>,
        sent_at: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        async move {
            let content = proto::Content::decode(content.as_slice())
                .map_err(|error| BackendError::new(format!("invalid sent message: {error}")))?;
            let recipient = Aci::from(recipient.uuid()).into();
            manager
                .resend_content(&recipient, content, sent_at.as_millis())
                .await
                .map_err(presage_error)
        }
        .boxed_local()
    }

    fn send_null_message(
        &mut self,
        recipient: ServiceId,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        async move {
            let recipient = Aci::from(recipient.uuid()).into();
            manager
                .send_null_message(&recipient, Timestamp::now().as_millis())
                .await
                .map_err(presage_error)
        }
        .boxed_local()
    }

    fn sync_deleted_for_me(
        &mut self,
        destination: Destination,
        messages: Vec<(Option<ServiceId>, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let me = self.manager.registration_data().service_ids.aci;
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
        self.send_sync(sync)
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

    fn group_members(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<Option<GroupMembers>, BackendError>> {
        let manager = self.manager.clone();
        async move {
            let group = manager
                .store()
                .group(*key.as_bytes())
                .await
                .map_err(store_error)?;
            Ok(group.map(|group| GroupMembers {
                revision: group.revision,
                members: group
                    .members
                    .iter()
                    .map(|member| ServiceId::new(member.aci.into()))
                    .collect(),
            }))
        }
        .boxed_local()
    }

    fn refresh_group(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let manager = self.manager.clone();
        async move {
            manager
                .refresh_group(key.as_bytes())
                .await
                .map_err(presage_error)
        }
        .boxed_local()
    }

    /// One request per member, one at a time, up to [`MAX_PROFILE_FETCHES`];
    /// each member's at most once per [`PROFILE_FETCH_INTERVAL`].
    fn fetch_member_profiles(
        &mut self,
        key: GroupMasterKey,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let attempts = self.profile_fetches.clone();
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
                let id = ServiceId::new(member.aci.into());
                if !attempts.may_fetch(id, Instant::now()) {
                    continue;
                }
                fetched += 1;
                if let Err(error) = manager
                    .retrieve_profile_by_uuid(member.aci, member.profile_key)
                    .await
                {
                    let error = presage_error(error);
                    if error.is_network() {
                        attempts.forget(id);
                        return Err(error);
                    }
                    tracing::debug!(%error, "fetching a member profile failed");
                }
            }
            Ok(())
        }
        .boxed_local()
    }

    /// presage's copy: 1:1 timers, kept current from messages and contact
    /// syncs, and groups' from their state.
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

    /// The key presage's store holds for the peer's ACI, and the safety
    /// number from it as Signal's clients compute it.
    fn identity(
        &mut self,
        peer: ServiceId,
    ) -> LocalBoxFuture<'static, Result<Option<PeerIdentity>, BackendError>> {
        let manager = self.manager.clone();
        async move {
            let store = manager.store().aci_protocol_store();
            let address = ProtocolAddress::new(
                Aci::from(peer.uuid()).service_id_string(),
                *DEFAULT_DEVICE_ID,
            );
            let Some(key) = store.get_identity(&address).await.map_err(store_error)? else {
                return Ok(None);
            };
            let own = store.get_identity_key_pair().await.map_err(store_error)?;
            let me = manager.registration_data().service_ids.aci;
            let fingerprint = Fingerprint::new(
                FINGERPRINT_VERSION,
                FINGERPRINT_ITERATIONS,
                me.as_bytes(),
                own.identity_key(),
                peer.uuid().as_bytes(),
                &key,
            )
            .and_then(|fingerprint| fingerprint.display_string())
            .map_err(|error| BackendError::new(format!("computing a safety number: {error}")))?;
            Ok(Some(PeerIdentity {
                key: IdentityKey::new(key.serialize().into_vec()),
                safety_number: SafetyNumber::new(fingerprint),
            }))
        }
        .boxed_local()
    }

    fn identity_changes(&mut self) -> LocalBoxStream<'static, ServiceId> {
        match self.identity_changes.take() {
            Some(mut changes) => {
                futures::stream::poll_fn(move |cx| changes.poll_recv(cx)).boxed_local()
            }
            None => futures::stream::empty().boxed_local(),
        }
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

    fn remove_device(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let manager = self.manager.clone();
        async move { manager.unlink_self().await.map_err(presage_error) }.boxed_local()
    }

    fn request_contact_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        async move { manager.request_contacts().await.map_err(presage_error) }.boxed_local()
    }

    fn request_settings_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.send_sync(sync_request(sync_message::request::Type::Configuration))
    }

    fn request_blocked_sync(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.send_sync(sync_request(sync_message::request::Type::Blocked))
    }

    fn sync_request_response(
        &mut self,
        chat: ChatRef,
        response: RequestResponse,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        use message_request_response::Type;
        let (thread_aci, group_id) = match chat {
            ChatRef::Contact(peer) => (Some(peer.uuid().to_string()), None),
            ChatRef::Group(group) => (None, Some(group.as_bytes().to_vec())),
        };
        let r#type = match response {
            RequestResponse::Accept => Type::Accept,
            RequestResponse::Delete => Type::Delete,
            RequestResponse::Block => Type::Block,
            RequestResponse::BlockAndDelete => Type::BlockAndDelete,
        };
        self.send_sync(SyncMessage {
            content: Some(SyncContent::MessageRequestResponse(
                sync_message::MessageRequestResponse {
                    thread_aci,
                    group_id,
                    r#type: Some(r#type.into()),
                    ..Default::default()
                },
            )),
            ..SyncMessage::with_padding(&mut rand::rng())
        })
    }

    fn sync_blocked(
        &mut self,
        list: BlockedList,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        self.send_sync(SyncMessage {
            content: Some(SyncContent::Blocked(sync_message::Blocked {
                numbers: list
                    .numbers
                    .iter()
                    .map(|number| number.as_str().to_owned())
                    .collect(),
                acis: list
                    .accounts
                    .iter()
                    .map(|account| account.uuid().to_string())
                    .collect(),
                acis_binary: Vec::new(),
                group_ids: list
                    .groups
                    .iter()
                    .map(|group| group.as_bytes().to_vec())
                    .collect(),
            })),
            ..SyncMessage::with_padding(&mut rand::rng())
        })
    }

    /// An empty data message: presage adds this account's profile key.
    fn send_profile_key(
        &mut self,
        peer: ServiceId,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let now = Timestamp::now().as_millis();
        let message = DataMessage {
            flags: Some(data_message::Flags::ProfileKeyUpdate as u32),
            timestamp: Some(now),
            ..Default::default()
        };
        async move {
            manager
                .send_message(Aci::from(peer.uuid()), message, now)
                .await
                .map(drop)
                .map_err(presage_error)
        }
        .boxed_local()
    }

    /// Reads contact and group records, [`STORAGE_READ_BATCH`] at a time.
    fn stored_chats(
        &mut self,
        known: Option<u64>,
    ) -> LocalBoxFuture<'static, Result<Option<StoredChats>, BackendError>> {
        let manager = self.manager.clone();
        async move {
            let Some(service) = manager.storage_service().await.map_err(presage_error)? else {
                return Ok(None);
            };
            let manifest = match known {
                Some(version) => match service
                    .manifest_if_changed(version)
                    .await
                    .map_err(storage_service_error)?
                {
                    Some(manifest) => manifest,
                    None => return Ok(None),
                },
                None => service.manifest().await.map_err(storage_service_error)?,
            };
            let keys: Vec<_> = manifest
                .identifiers
                .iter()
                .filter(|id| {
                    matches!(
                        id.r#type(),
                        identifier::Type::Contact | identifier::Type::Groupv2
                    )
                })
                .map(|id| id.raw.clone())
                .collect();
            let ikm = (!manifest.record_ikm.is_empty()).then_some(manifest.record_ikm.as_slice());
            let mut stored = StoredChats {
                version: manifest.version,
                ..StoredChats::default()
            };
            for batch in keys.chunks(STORAGE_READ_BATCH) {
                let records = service
                    .read_items(batch.to_vec(), ikm)
                    .await
                    .map_err(storage_service_error)?;
                for record in &records {
                    stored.chats.extend(stored_chat(record));
                    stored.identities.extend(stored_identity(record));
                }
            }
            Ok(Some(stored))
        }
        .boxed_local()
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
                .map(drop)
                .map_err(presage_error)
        }
        .boxed_local()
    }

    fn send_read_sync(
        &mut self,
        read: Vec<(ServiceId, Timestamp)>,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
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
        self.send_sync(sync)
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
            .map(drop)
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

/// How long after the last acknowledgement the websocket gets to send it
/// before closing; the server sends unacknowledged messages again, and those
/// decrypted already then fail to decrypt.
const ACK_FLUSH: Duration = Duration::from_millis(250);

/// Passes on what `stream` receives until it ends or `stop` fires, then the
/// read in progress, as cancelling a decryption may lose the message; the
/// server sends what's left unread again. Acknowledges what the core doesn't
/// care about, and returns once the core acknowledged or dropped the rest and
/// the websocket had time to send the acknowledgements.
async fn relay(
    stream: impl Stream<Item = (Received, Ack)>,
    me: Aci,
    server: &ServerPublicParams,
    items: mpsc::Sender<(Incoming, Ack)>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut stream = Box::pin(stream);
    // Each ack handed out holds a sender until it's acknowledged or dropped.
    let (unacked, mut all_acked) = mpsc::channel::<()>(1);
    let last_ack = Rc::new(Cell::new(None));
    let mut stopping = false;
    while !stopping {
        let started = Instant::now();
        let mut next = stream.next();
        let received = tokio::select! {
            biased;
            _ = &mut stop => {
                stopping = true;
                tokio::time::timeout_at(started + DRAIN_IDLE, &mut next)
                    .await
                    .ok()
                    .flatten()
            }
            received = &mut next => received,
        };
        let Some((received, ack)) = received else {
            break;
        };
        let ack = noted(ack, unacked.clone(), last_ack.clone());
        match incoming(received, me, server) {
            Some(item) => {
                if items.send((item, ack)).await.is_err() {
                    break;
                }
            }
            None => ack.ack(),
        }
    }
    drop(unacked);
    // `items` stays open meanwhile, so the backend, and its websocket, live.
    all_acked.recv().await;
    if let Some(at) = last_ack.get() {
        tokio::time::sleep_until(at + ACK_FLUSH).await;
    }
}

/// `ack`, noting when it went out; holds `unacked` until then.
fn noted(ack: Ack, unacked: mpsc::Sender<()>, last_ack: Rc<Cell<Option<Instant>>>) -> Ack {
    Ack::new(move || {
        ack.ack();
        last_ack.set(Some(Instant::now()));
        drop(unacked);
    })
}

/// Most Storage Service records read per request.
const STORAGE_READ_BATCH: usize = 1000;

/// A request to the phone for what it syncs.
fn sync_request(kind: sync_message::request::Type) -> SyncMessage {
    SyncMessage {
        content: Some(SyncContent::Request(sync_message::Request {
            r#type: Some(kind.into()),
        })),
        ..SyncMessage::with_padding(&mut rand::rng())
    }
}

/// A contact's or group's state in a Storage Service record; `None` for
/// other records, and contacts without an ACI.
fn stored_chat(record: &StorageRecord) -> Option<StoredChat> {
    match record.record.as_ref()? {
        storage_record::Record::Contact(contact) => Some(StoredChat {
            chat: ChatRef::Contact(contact_id(contact)?),
            accepted: contact.whitelisted,
            blocked: contact.blocked,
        }),
        storage_record::Record::GroupV2(group) => {
            let key = <[u8; 32]>::try_from(group.master_key.as_slice()).ok()?;
            Some(StoredChat {
                chat: ChatRef::Group(GroupMasterKey::new(key).id()),
                accepted: group.whitelisted,
                blocked: group.blocked,
            })
        }
        _ => None,
    }
}

/// A contact record's verification, if it has a key and isn't the phone's
/// mark of a verified key that changed (as with [`Incoming::Verified`]).
fn stored_identity(record: &StorageRecord) -> Option<StoredIdentity> {
    let storage_record::Record::Contact(contact) = record.record.as_ref()? else {
        return None;
    };
    let verified = match contact_record::IdentityState::try_from(contact.identity_state) {
        Ok(contact_record::IdentityState::Verified) => true,
        Ok(contact_record::IdentityState::Default) => false,
        Ok(contact_record::IdentityState::Unverified) | Err(_) => return None,
    };
    Some(StoredIdentity {
        peer: contact_id(contact)?,
        key: identity_key(&contact.identity_key)?,
        verified,
    })
}

fn contact_id(contact: &ContactRecord) -> Option<ServiceId> {
    let binary = (!contact.aci_binary.is_empty()).then_some(contact.aci_binary.as_slice());
    let string = (!contact.aci.is_empty()).then_some(contact.aci.as_str());
    service_id(binary, string)
}

/// Safety numbers as Signal's clients compute them: from ACIs, version 2.
const FINGERPRINT_VERSION: u32 = 2;
const FINGERPRINT_ITERATIONS: u32 = 5200;

/// Most profiles one [`SignalBackend::fetch_member_profiles`] call fetches.
const MAX_PROFILE_FETCHES: usize = 100;
/// How often a member's profile may be fetched while it has no name.
const PROFILE_FETCH_INTERVAL: Duration = Duration::from_secs(3600);

/// When members' profiles were last fetched.
#[derive(Clone, Default)]
struct ProfileFetches(Rc<RefCell<HashMap<ServiceId, Instant>>>);

impl ProfileFetches {
    /// Whether `member`'s profile may be fetched at `now`: once per
    /// [`PROFILE_FETCH_INTERVAL`]. Notes the fetch if so.
    fn may_fetch(&self, member: ServiceId, now: Instant) -> bool {
        let mut fetched = self.0.borrow_mut();
        fetched.retain(|_, at| now.saturating_duration_since(*at) < PROFILE_FETCH_INTERVAL);
        if fetched.contains_key(&member) {
            return false;
        }
        fetched.insert(member, now);
        true
    }

    /// Forgets a fetch that didn't reach the server.
    fn forget(&self, member: ServiceId) {
        self.0.borrow_mut().remove(&member);
    }
}

/// A member's synced contact name, else cached profile name, else the name
/// presage saved on first sight, cleaned; blank ones count as unknown.
async fn member_name(store: &AppStore, member: &presage::model::groups::Member) -> Option<String> {
    let id: SignalServiceId = member.aci.into();
    let contact = store.contact_by_id(&id).await.ok().flatten();
    if let Some(contact) = &contact
        && synced(contact)
        && let Some(name) = clean_name(&contact.name)
    {
        return Some(name);
    }
    if let Ok(Some(profile)) = store.profile(member.aci.into(), member.profile_key).await
        && let Some(name) = profile.name.and_then(|name| clean_name(&name.to_string()))
    {
        return Some(name);
    }
    contact
        .filter(|contact| !synced(contact))
        .and_then(|contact| clean_name(&contact.name))
}

/// How a reply quotes a message; `None` authors are this account.
fn proto_quote(quote: Quote, me: uuid::Uuid) -> data_message::Quote {
    data_message::Quote {
        id: Some(quote.sent_at.as_millis()),
        author_aci: Some(quote.author.map_or(me, |author| author.uuid()).to_string()),
        text: quote
            .original
            .map(|original| original.text)
            .filter(|text| !text.is_empty()),
        r#type: Some(data_message::quote::Type::Normal.into()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use presage::model::contacts::Contact;

    use crate::signal::incoming::tests::{OTHER, SENDER, content, server, text};

    #[test]
    fn member_profiles_are_fetched_once_an_hour() {
        let fetches = ProfileFetches::default();
        let (a, b) = (ServiceId::new(SENDER), ServiceId::new(OTHER));
        let start = Instant::now();

        assert!(fetches.may_fetch(a, start));
        assert!(!fetches.may_fetch(a, start + PROFILE_FETCH_INTERVAL / 2));
        assert!(fetches.may_fetch(b, start + PROFILE_FETCH_INTERVAL / 2));
        fetches.forget(b);
        assert!(fetches.may_fetch(b, start + PROFILE_FETCH_INTERVAL / 2));
        assert!(fetches.may_fetch(a, start + PROFILE_FETCH_INTERVAL));
    }

    #[test]
    fn sent_quotes_name_their_author() {
        let quote = |author| Quote {
            author,
            sent_at: Timestamp::from_millis(50),
            original: Some(crate::model::Quoted {
                text: "ship it?".into(),
                attachment: None,
            }),
        };
        let theirs = proto_quote(quote(Some(ServiceId::new(OTHER))), SENDER);
        assert_eq!(theirs.author_aci, Some(OTHER.to_string()));
        assert_eq!(theirs.id, Some(50));
        assert_eq!(theirs.text.as_deref(), Some("ship it?"));
        let gone = Quote {
            original: None,
            ..quote(None)
        };
        assert_eq!(proto_quote(gone, SENDER).text, None);
        let ours = proto_quote(quote(None), SENDER);
        assert_eq!(
            ours.author_aci,
            Some(SENDER.to_string()),
            "we are the author"
        );
        let roundtrip = crate::signal::incoming::quote(&ours).unwrap();
        assert_eq!(roundtrip.author, Some(ServiceId::new(SENDER)));
    }

    #[test]
    fn storage_records_name_their_chat() {
        use presage::libsignal_service::proto::{AccountRecord, ContactRecord, GroupV2Record};

        let record = |record| StorageRecord {
            record: Some(record),
        };
        let contact = |aci: String, aci_binary: Vec<u8>| {
            record(storage_record::Record::Contact(ContactRecord {
                aci,
                aci_binary,
                whitelisted: true,
                blocked: false,
                ..Default::default()
            }))
        };
        let peer = ChatRef::Contact(ServiceId::new(OTHER));
        let accepted = |chat| StoredChat {
            chat,
            accepted: true,
            blocked: false,
        };
        assert_eq!(
            stored_chat(&contact(OTHER.to_string(), Vec::new())),
            Some(accepted(peer))
        );
        assert_eq!(
            stored_chat(&contact(String::new(), OTHER.as_bytes().to_vec())),
            Some(accepted(peer))
        );
        assert_eq!(stored_chat(&contact(String::new(), Vec::new())), None);

        let group = record(storage_record::Record::GroupV2(GroupV2Record {
            master_key: vec![7; 32],
            blocked: true,
            ..Default::default()
        }));
        assert_eq!(
            stored_chat(&group),
            Some(StoredChat {
                chat: ChatRef::Group(GroupMasterKey::new([7; 32]).id()),
                accepted: false,
                blocked: true,
            })
        );
        let account = record(storage_record::Record::Account(AccountRecord::default()));
        assert_eq!(stored_chat(&account), None);
        assert_eq!(stored_chat(&StorageRecord { record: None }), None);
    }

    #[test]
    fn storage_records_carry_verifications() {
        use presage::libsignal_service::proto::ContactRecord;
        use presage::libsignal_service::protocol::IdentityKeyPair;

        let key = IdentityKeyPair::generate(&mut rand::rng())
            .identity_key()
            .serialize()
            .into_vec();
        let contact = |state: contact_record::IdentityState, key: Vec<u8>| StorageRecord {
            record: Some(storage_record::Record::Contact(ContactRecord {
                aci: OTHER.to_string(),
                identity_key: key,
                identity_state: state.into(),
                ..Default::default()
            })),
        };
        let expected = |verified| StoredIdentity {
            peer: ServiceId::new(OTHER),
            key: IdentityKey::new(key.clone()),
            verified,
        };
        assert_eq!(
            stored_identity(&contact(
                contact_record::IdentityState::Verified,
                key.clone()
            )),
            Some(expected(true))
        );
        assert_eq!(
            stored_identity(&contact(
                contact_record::IdentityState::Default,
                key.clone()
            )),
            Some(expected(false))
        );
        assert_eq!(
            stored_identity(&contact(
                contact_record::IdentityState::Unverified,
                key.clone()
            )),
            None
        );
        assert_eq!(
            stored_identity(&contact(
                contact_record::IdentityState::Verified,
                Vec::new()
            )),
            None
        );
        assert_eq!(
            stored_identity(&contact(
                contact_record::IdentityState::Verified,
                vec![5, 1]
            )),
            None
        );
        let mut unknown = contact(contact_record::IdentityState::Verified, key.clone());
        if let Some(storage_record::Record::Contact(contact)) = &mut unknown.record {
            contact.identity_state = 7;
        }
        assert_eq!(stored_identity(&unknown), None);
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
        let (changes, _) = unbounded_channel();
        let store = AppStore::new(
            db.presage_store().clone(),
            db.verified_keys(),
            db.peer_timers(),
            changes,
        );
        let thread = Thread::Contact(Aci::from(SENDER).into());

        store
            .save_message(&thread, content(text("hi", Some(42))))
            .await
            .unwrap();

        assert_eq!(store.messages(&thread, ..).await.unwrap().count(), 0);
        assert!(!store.is_registered().await);
        db.close().await;
    }

    fn stored_contact(name: &str, profile_key: Vec<u8>, timer: u32, version: u32) -> Contact {
        Contact {
            uuid: SENDER,
            phone_number: None,
            name: name.into(),
            verified: Default::default(),
            profile_key,
            expire_timer: timer,
            expire_timer_version: version,
            inbox_position: 0,
            avatar: None,
        }
    }

    async fn app_store(db: &Database) -> AppStore {
        let (changes, _) = unbounded_channel();
        AppStore::new(
            db.presage_store().clone(),
            db.verified_keys(),
            db.peer_timers(),
            changes,
        )
    }

    #[tokio::test]
    async fn store_keeps_timers_for_contacts_and_others() {
        use presage::store::{ContentsStore as _, Thread};

        let dir = tempfile::tempdir().unwrap();
        let key = crate::storage::DatabaseKey::generate();
        let db = Database::open(&dir.path().join("test.db"), &key)
            .await
            .unwrap();
        let mut store = app_store(&db).await;
        let stranger = Thread::Contact(Aci::from(SENDER).into());

        store.update_expire_timer(&stranger, 60, 2).await.unwrap();
        store.update_expire_timer(&stranger, 30, 1).await.unwrap();
        store
            .save_contact(&stored_contact("Ali", vec![1; 32], 10, 9))
            .await
            .unwrap();

        assert_eq!(store.expire_timer(&stranger).await.unwrap(), Some((60, 2)));
        store
            .save_contact(&stored_contact("Ali", Vec::new(), 300, 3))
            .await
            .unwrap();
        assert_eq!(store.expire_timer(&stranger).await.unwrap(), Some((300, 3)));
        let other = Thread::Contact(Aci::from(OTHER).into());
        assert_eq!(store.expire_timer(&other).await.unwrap(), None);
        db.close().await;
    }

    #[tokio::test]
    async fn first_sight_contacts_dont_replace_synced_ones() {
        use presage::store::ContentsStore as _;

        let dir = tempfile::tempdir().unwrap();
        let key = crate::storage::DatabaseKey::generate();
        let db = Database::open(&dir.path().join("test.db"), &key)
            .await
            .unwrap();
        let mut store = app_store(&db).await;
        let id = Aci::from(SENDER).into();
        let name = async |store: &AppStore| store.contact_by_id(&id).await.unwrap().unwrap().name;

        store
            .save_contact(&stored_contact("Seen", vec![1; 32], 0, 1))
            .await
            .unwrap();
        store
            .save_contact(&stored_contact("Seen again", vec![2; 32], 0, 1))
            .await
            .unwrap();
        assert_eq!(name(&store).await, "Seen again");

        store
            .save_contact(&stored_contact("Alice", Vec::new(), 0, 1))
            .await
            .unwrap();
        store
            .save_contact(&stored_contact("Mallory", vec![3; 32], 0, 1))
            .await
            .unwrap();
        assert_eq!(name(&store).await, "Alice");
        db.close().await;
    }

    /// Relays `stream`, asking it to stop after `stop_after`, the core
    /// acknowledging each item after `handling` unless `None`; returns when
    /// each item came through, and when relaying ended.
    async fn relayed(
        stream: impl Stream<Item = (Received, Ack)>,
        stop_after: Duration,
        handling: Option<Duration>,
    ) -> (Vec<Duration>, Duration) {
        let start = Instant::now();
        let (items_tx, mut items) = mpsc::channel::<(Incoming, Ack)>(RECEIVE_BUFFER);
        let (stop, stop_rx) = oneshot::channel();
        let stopper = async {
            tokio::time::sleep(stop_after).await;
            stop.send(()).ok();
        };
        let core = async {
            let mut arrived = Vec::new();
            while let Some((_, ack)) = items.recv().await {
                arrived.push(start.elapsed());
                if let Some(handling) = handling {
                    tokio::time::sleep(handling).await;
                    ack.ack();
                }
            }
            arrived
        };
        let relay = async {
            relay(
                stream,
                Aci::from(uuid::Uuid::nil()),
                server(),
                items_tx,
                stop_rx,
            )
            .await;
            start.elapsed()
        };
        let (ended, (), arrived) = tokio::join!(relay, stopper, core);
        (arrived, ended)
    }

    /// `items` arriving together after `at`, with acks counted in `acked`.
    fn counted(
        items: impl IntoIterator<Item = Received>,
        at: Duration,
        acked: &Rc<Cell<usize>>,
    ) -> impl Stream<Item = (Received, Ack)> {
        let acked = acked.clone();
        let items = futures::stream::iter(items).map(move |received| {
            let acked = acked.clone();
            (received, Ack::new(move || acked.set(acked.get() + 1)))
        });
        futures::stream::once(tokio::time::sleep(at))
            .filter_map(|()| async { None })
            .chain(items)
    }

    /// `count` messages, one per `period`, on a connection that stays open.
    fn arrivals(period: Duration, count: usize) -> impl Stream<Item = (Received, Ack)> {
        futures::stream::unfold(0, move |n| async move {
            if n == count {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(period).await;
            Some(((Received::QueueEmpty, Ack::new(|| {})), n + 1))
        })
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[tokio::test(start_paused = true)]
    async fn relays_until_the_connection_closes() {
        let acked = Rc::default();
        let stream = counted(
            [Received::QueueEmpty, Received::QueueEmpty],
            Duration::ZERO,
            &acked,
        );

        let (arrived, ended) = relayed(stream, 3600 * SECOND, Some(Duration::ZERO)).await;

        assert_eq!(arrived, [Duration::ZERO; 2]);
        assert_eq!(acked.get(), 2);
        assert_eq!(ended, ACK_FLUSH);
    }

    #[tokio::test(start_paused = true)]
    async fn acknowledges_what_the_core_ignores() {
        let acked = Rc::default();
        let foreign_sync = content(ContentBody::SynchronizeMessage(SyncMessage::default()));
        let stream = counted(
            [Received::Content(Box::new(foreign_sync))],
            Duration::ZERO,
            &acked,
        );

        let (arrived, ended) = relayed(stream, 3600 * SECOND, None).await;

        assert_eq!(arrived, []);
        assert_eq!(acked.get(), 1);
        assert_eq!(ended, ACK_FLUSH);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_leaves_unread_messages_to_the_server() {
        let acked = Rc::default();
        let stream = counted([Received::QueueEmpty, Received::QueueEmpty], SECOND, &acked);

        let (arrived, ended) = relayed(stream, Duration::ZERO, Some(Duration::ZERO)).await;

        assert_eq!(arrived, [SECOND]);
        assert_eq!(acked.get(), 1);
        assert_eq!(ended, SECOND + ACK_FLUSH);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_waits_for_a_message_being_decrypted() {
        let (arrived, ended) =
            relayed(arrivals(SECOND, 1), Duration::ZERO, Some(Duration::ZERO)).await;

        assert_eq!(arrived, [SECOND]);
        assert_eq!(ended, SECOND + ACK_FLUSH);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_an_idle_connection_ends_at_once() {
        let (arrived, ended) = relayed(
            futures::stream::pending(),
            10 * SECOND,
            Some(Duration::ZERO),
        )
        .await;

        assert_eq!(arrived, []);
        assert_eq!(ended, 10 * SECOND);
    }

    #[tokio::test(start_paused = true)]
    async fn ends_once_the_core_acknowledged_what_it_got() {
        let acked = Rc::default();
        let stream = counted(
            [Received::QueueEmpty, Received::QueueEmpty],
            Duration::ZERO,
            &acked,
        );

        let (arrived, ended) = relayed(stream, 3600 * SECOND, Some(3 * SECOND)).await;

        assert_eq!(arrived, [Duration::ZERO, 3 * SECOND]);
        assert_eq!(acked.get(), 2);
        assert_eq!(ended, 6 * SECOND + ACK_FLUSH);
    }

    #[tokio::test(start_paused = true)]
    async fn messages_the_core_dropped_are_not_waited_for() {
        let acked = Rc::default();
        let stream = counted([Received::QueueEmpty], Duration::ZERO, &acked);

        let (arrived, ended) = relayed(stream, 3600 * SECOND, None).await;

        assert_eq!(arrived, [Duration::ZERO]);
        assert_eq!(acked.get(), 0);
        assert_eq!(ended, Duration::ZERO);
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
