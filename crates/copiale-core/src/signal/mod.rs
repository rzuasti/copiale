//! Signal through presage: the [`Linker`] and [`SignalBackend`] the app uses.
//!
//! Talks to Signal's production servers; tests use the testkit fakes, and
//! only the mapping to core types is tested here.

mod store;

use futures::future::LocalBoxFuture;
use futures::stream::LocalBoxStream;
use futures::{FutureExt as _, StreamExt as _};
use presage::Manager;
use presage::libsignal_service::configuration::SignalServers;
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::proto::sync_message::{Content as SyncContent, Sent};
use presage::libsignal_service::proto::{DataMessage, GroupContextV2, SyncMessage};
use presage::libsignal_service::protocol::{Aci, ServiceId as SignalServiceId};
use presage::libsignal_service::push_service::ServiceError;
use presage::libsignal_service::sender::MessageSenderError;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentsStore as _, StateStore as _};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::backend::{BackendError, Incoming, Linker, SignalBackend};
use crate::model::{
    Destination, GroupInfo, GroupMasterKey, GroupRef, IncomingMessage, Member, MessageBody,
    SentMessage, ServiceId, Timestamp,
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
    receiver: Option<JoinHandle<()>>,
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
            receiver.abort();
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
        let (items_tx, items_rx) = mpsc::unbounded_channel();
        let receiver = tokio::task::spawn_local(async move {
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
            futures::pin_mut!(stream);
            while let Some(received) = stream.next().await {
                if let Some(item) = incoming(received)
                    && items_tx.send(item).is_err()
                {
                    break;
                }
            }
        });
        if let Some(previous) = self.receiver.replace(receiver) {
            previous.abort();
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

    /// Sends on a clone of the manager; libsignal-service adds the sync
    /// copy for the other devices.
    fn send_text(
        &mut self,
        destination: Destination,
        body: MessageBody,
        timestamp: Timestamp,
    ) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let mut manager = self.manager.clone();
        let message = DataMessage {
            body: Some(body.as_str().into()),
            timestamp: Some(timestamp.as_millis()),
            ..Default::default()
        };
        async move {
            match destination {
                Destination::Contact(recipient) => {
                    manager
                        .send_message(Aci::from(recipient.uuid()), message, timestamp.as_millis())
                        .await
                }
                Destination::Group(group) => {
                    // presage's copy may be newer than the core's.
                    let revision = match manager.store().group(*group.key.as_bytes()).await {
                        Ok(Some(stored)) => stored.revision.max(group.revision),
                        _ => group.revision,
                    };
                    let message = DataMessage {
                        group_v2: Some(GroupContextV2 {
                            master_key: Some(group.key.as_bytes().to_vec()),
                            revision: Some(revision),
                            ..Default::default()
                        }),
                        ..message
                    };
                    manager
                        .send_message_to_group(group.key.as_bytes(), message, timestamp.as_millis())
                        .await
                }
            }
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

    /// Asks who this account is, over the connection receiving messages.
    fn probe(&mut self) -> LocalBoxFuture<'static, Result<(), BackendError>> {
        let manager = self.manager.clone();
        async move {
            manager.whoami().await.map_err(presage_error)?;
            Ok(())
        }
        .boxed_local()
    }

    async fn request_contact_sync(&mut self) -> Result<(), BackendError> {
        self.manager.request_contacts().await.map_err(presage_error)
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
        Received::Content(content) => text_message(&content)
            .map(Incoming::Message)
            .or_else(|| sent_text(&content).map(Incoming::Sent)),
        Received::Contacts => Some(Incoming::ContactsSynced),
        Received::DecryptionError(_) => {
            tracing::warn!("a message could not be decrypted");
            None
        }
    }
}

/// A 1:1 or group text message from another account.
fn text_message(content: &Content) -> Option<IncomingMessage> {
    let ContentBody::DataMessage(
        message @ DataMessage {
            body: Some(body),
            timestamp,
            ..
        },
    ) = &content.body
    else {
        return None;
    };
    let group = match &message.group_v2 {
        Some(context) => Some(group_ref(context)?),
        None => None,
    };
    let timestamp = timestamp
        .unwrap_or_else(|| content.metadata.client_timestamp.timestamp_millis().max(0) as u64);
    Some(IncomingMessage {
        sender: ServiceId::new(content.metadata.sender.raw_uuid()),
        group,
        timestamp: Timestamp::from_millis(timestamp),
        body: MessageBody::new(body.clone()),
    })
}

/// A 1:1 or group text message this account sent from another device.
fn sent_text(content: &Content) -> Option<SentMessage> {
    let ContentBody::SynchronizeMessage(SyncMessage {
        content: Some(SyncContent::Sent(sent)),
        ..
    }) = &content.body
    else {
        return None;
    };
    let Sent {
        message:
            Some(
                message @ DataMessage {
                    body: Some(body), ..
                },
            ),
        timestamp: Some(timestamp),
        ..
    } = sent
    else {
        return None;
    };
    let destination = match &message.group_v2 {
        Some(context) => Destination::Group(group_ref(context)?),
        None => {
            let recipient = sent
                .destination_service_id_binary
                .as_deref()
                .and_then(SignalServiceId::parse_from_service_id_binary)
                .or_else(|| {
                    sent.destination_service_id
                        .as_deref()
                        .and_then(SignalServiceId::parse_from_service_id_string)
                })?;
            Destination::Contact(ServiceId::new(recipient.raw_uuid()))
        }
    };
    Some(SentMessage {
        destination,
        timestamp: Timestamp::from_millis(*timestamp),
        body: MessageBody::new(body.clone()),
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
    use presage::libsignal_service::proto::{GroupContextV2, TypingMessage};
    use presage::libsignal_service::protocol::DeviceId;
    use uuid::Uuid;

    use super::*;

    const SENDER: Uuid = Uuid::from_u128(0x1234);

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

    fn text(body: &str, timestamp: Option<u64>) -> ContentBody {
        ContentBody::DataMessage(DataMessage {
            body: Some(body.into()),
            timestamp,
            ..Default::default()
        })
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
                body: MessageBody::new("hi"),
            }))
        );
    }

    #[test]
    fn falls_back_to_client_timestamp() {
        let message = text_message(&content(text("hi", None))).unwrap();
        assert_eq!(message.timestamp, Timestamp::from_millis(1_700_000_000_123));
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
            group_v2: group_context(Some(vec![9; 32])),
            ..Default::default()
        });

        let message = text_message(&content(body)).unwrap();

        assert_eq!(message.sender, ServiceId::new(SENDER));
        assert_eq!(
            message.group,
            Some(GroupRef {
                key: GroupMasterKey::new([9; 32]),
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
            assert_eq!(text_message(&content(body)), None);
        }
    }

    #[test]
    fn skips_messages_without_text() {
        let body = ContentBody::DataMessage(DataMessage::default());
        assert_eq!(text_message(&content(body)), None);
        let typing = ContentBody::TypingMessage(TypingMessage::default());
        assert_eq!(text_message(&content(typing)), None);
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
                group_context(Some(vec![9; 32]))
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
                body: MessageBody::new("from phone"),
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

        let sent = sent_text(&content(body)).unwrap();
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

        let sent = sent_text(&content(body)).unwrap();

        assert_eq!(
            sent.destination,
            Destination::Group(GroupRef {
                key: GroupMasterKey::new([9; 32]),
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

        assert_eq!(sent_text(&content(no_recipient)), None);
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
        assert!(
            !presage_error(Error::from(MessageSenderError::MaximumRetriesLimitExceeded))
                .is_network()
        );
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
