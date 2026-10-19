//! The [`Linker`] and the [`SignalBackend`] of a linked account.

use futures::future::LocalBoxFuture;
use futures::stream::{LocalBoxStream, Stream};
use futures::{FutureExt as _, StreamExt as _};
use presage::Manager;
use presage::libsignal_service::configuration::{ServiceConfiguration, SignalServers};
use presage::libsignal_service::content::ContentBody;
use presage::libsignal_service::prelude::ProtobufMessage as _;
use presage::libsignal_service::proto::sync_message::{self, Content as SyncContent};
use presage::libsignal_service::proto::{
    self, AddressableMessage, ConversationIdentifier, DataMessage, EditMessage, GroupContextV2,
    ReceiptMessage, SyncMessage, TypingMessage, addressable_message, conversation_identifier,
    data_message, receipt_message, typing_message,
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

use super::errors::{presage_error, store_error};
use super::incoming::incoming;
use super::store::AppStore;
use crate::backend::{
    BackendError, Incoming, Linker, PeerIdentity, Resendable, RetryRequest, SignalBackend,
};
use crate::model::{
    Destination, GroupInfo, GroupMasterKey, GroupMembers, IdentityKey, Member, MessageBody,
    Outgoing, Quote, SafetyNumber, ServiceId, Timestamp,
};
use crate::storage::Database;

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

// Timestamps of syncs, receipts and typing messages made here use the
// system clock, not `CoreOptions::clock`: they go to Signal.
impl SignalBackend for PresageBackend {
    /// Receives on a clone of the manager in a local task, so sending
    /// doesn't wait for the stream. Must run inside a `LocalSet`.
    fn receive(
        &mut self,
    ) -> LocalBoxFuture<'static, Result<LocalBoxStream<'static, Incoming>, BackendError>> {
        let mut manager = self.manager.clone();
        let me = manager.registration_data().service_ids.aci();
        let server = ServiceConfiguration::from(manager.registration_data().signal_servers)
            .zkgroup_server_public_params;
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
                    .map(|recipient| ServiceId::new(recipient.raw_uuid()))
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
                .map(drop)
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
        let mut manager = self.manager.clone();
        let request = SyncMessage {
            content: Some(SyncContent::Request(sync_message::Request {
                r#type: Some(sync_message::request::Type::Configuration.into()),
            })),
            ..SyncMessage::with_padding(&mut rand::rng())
        };
        async move {
            let me = manager.registration_data().service_ids.aci();
            manager
                .send_message(me, request, Timestamp::now().as_millis())
                .await
                .map(drop)
                .map_err(presage_error)
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
                .map(drop)
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
    me: Aci,
    server: &ServerPublicParams,
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
        if let Some(item) = incoming(received, me, server)
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

/// Safety numbers as Signal's clients compute them: from ACIs, version 2.
const FINGERPRINT_VERSION: u32 = 2;
const FINGERPRINT_ITERATIONS: u32 = 5200;

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

#[cfg(test)]
mod tests {

    use super::*;
    use crate::signal::incoming::tests::{OTHER, SENDER, content, server, text};

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
        let roundtrip = crate::signal::incoming::quote(&ours).unwrap();
        assert_eq!(roundtrip.author, Some(ServiceId::new(SENDER)));
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
        let store = AppStore::new(db.presage_store().clone(), db.verified_keys(), changes);
        let thread = Thread::Contact(Aci::from(SENDER).into());

        store
            .save_message(&thread, content(text("hi", Some(42))))
            .await
            .unwrap();

        assert_eq!(store.messages(&thread, ..).await.unwrap().count(), 0);
        assert!(!store.is_registered().await);
        db.close().await;
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
        let ((), (), relayed) = tokio::join!(
            relay(
                stream,
                Aci::from(uuid::Uuid::nil()),
                server(),
                items_tx,
                stop_rx
            ),
            stopper,
            collect
        );
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
