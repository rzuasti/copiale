//! Signal through presage: the [`Linker`] and [`SignalBackend`] the app uses.
//!
//! Talks to Signal's production servers; tests use the testkit fakes, and
//! only the mapping to core types is tested here.

mod store;

use futures::StreamExt as _;
use futures::stream::LocalBoxStream;
use presage::Manager;
use presage::libsignal_service::configuration::SignalServers;
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::proto::DataMessage;
use presage::libsignal_service::protocol::Aci;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::StateStore as _;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::backend::{BackendError, Incoming, Linker, SignalBackend};
use crate::model::{IncomingMessage, MessageBody, ServiceId, Timestamp};
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
            .map_err(backend_error)?;
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
        let manager = manager.map_err(backend_error)?;
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
    async fn receive(&mut self) -> Result<LocalBoxStream<'static, Incoming>, BackendError> {
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
                    opened_tx.send(Err(backend_error(error))).ok();
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
        opened_rx
            .await
            .map_err(|_| BackendError::new("receiver stopped"))??;

        let stream = futures::stream::unfold(items_rx, |mut items| async move {
            let item = items.recv().await?;
            Some((item, items))
        });
        Ok(stream.boxed_local())
    }

    async fn send_text(
        &mut self,
        recipient: ServiceId,
        body: &MessageBody,
        timestamp: Timestamp,
    ) -> Result<(), BackendError> {
        let message = DataMessage {
            body: Some(body.as_str().into()),
            timestamp: Some(timestamp.as_millis()),
            ..Default::default()
        };
        self.manager
            .send_message(Aci::from(recipient.uuid()), message, timestamp.as_millis())
            .await
            .map_err(backend_error)
    }
}

fn backend_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::new(error.to_string())
}

/// What the core cares about so far; everything else is dropped.
fn incoming(received: Received) -> Option<Incoming> {
    match received {
        Received::QueueEmpty => Some(Incoming::QueueEmpty),
        Received::Content(content) => text_message(&content).map(Incoming::Message),
        Received::Contacts => None,
        Received::DecryptionError(_) => {
            tracing::warn!("a message could not be decrypted");
            None
        }
    }
}

/// A 1:1 text message from another account.
fn text_message(content: &Content) -> Option<IncomingMessage> {
    let ContentBody::DataMessage(DataMessage {
        body: Some(body),
        group_v2: None,
        timestamp,
        ..
    }) = &content.body
    else {
        return None;
    };
    let timestamp = timestamp
        .unwrap_or_else(|| content.metadata.client_timestamp.timestamp_millis().max(0) as u64);
    Some(IncomingMessage {
        sender: ServiceId::new(content.metadata.sender.raw_uuid()),
        timestamp: Timestamp::from_millis(timestamp),
        body: MessageBody::new(body.clone()),
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

    #[test]
    fn skips_group_messages_for_now() {
        let body = ContentBody::DataMessage(DataMessage {
            body: Some("hi all".into()),
            group_v2: Some(GroupContextV2::default()),
            ..Default::default()
        });
        assert_eq!(text_message(&content(body)), None);
    }

    #[test]
    fn skips_messages_without_text() {
        let body = ContentBody::DataMessage(DataMessage::default());
        assert_eq!(text_message(&content(body)), None);
        let typing = ContentBody::TypingMessage(TypingMessage::default());
        assert_eq!(text_message(&content(typing)), None);
    }

    #[test]
    fn maps_queue_empty_and_drops_the_rest() {
        assert_eq!(incoming(Received::QueueEmpty), Some(Incoming::QueueEmpty));
        assert_eq!(incoming(Received::Contacts), None);
        assert_eq!(
            incoming(Received::DecryptionError(Aci::from(SENDER).into())),
            None
        );
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
}
