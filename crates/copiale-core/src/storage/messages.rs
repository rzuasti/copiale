//! Conversations and messages in Copiale's tables.

use presage::libsignal_service::protocol::{Aci, ServiceId as SignalServiceId};
use presage::store::ContentsStore as _;
use uuid::Uuid;

use super::StorageError;
use super::database::{Database, database_error};
use crate::model::{
    ConversationId, ConversationSummary, Direction, Message, MessageBody, MessageId,
    MessagePreview, Peer, ServiceId, Timestamp,
};

/// Summary columns: id, peer, last_activity, unread, last message
/// direction and body. A macro so queries stay string literals, which sqlx
/// requires.
macro_rules! summary_query {
    () => {
        "SELECT c.id, c.peer, c.last_activity,
                (SELECT count(*) FROM messages u WHERE u.conversation_id = c.id AND u.read = 0),
                m.outgoing, m.body
         FROM conversations c
         LEFT JOIN messages m ON m.id = (
             SELECT id FROM messages WHERE conversation_id = c.id
             ORDER BY sent_at DESC, id DESC LIMIT 1
         )"
    };
}

type SummaryRow = (i64, Vec<u8>, Option<i64>, i64, Option<bool>, Option<String>);
type MessageRow = (i64, i64, bool, i64, i64, String, bool);

impl Database {
    /// Stores a 1:1 message unless it's already stored (same peer,
    /// direction and sent time). Returns it if new.
    pub(crate) async fn add_message(
        &self,
        peer: ServiceId,
        direction: Direction,
        sent_at: Timestamp,
        body: &MessageBody,
    ) -> Result<Option<Message>, StorageError> {
        let received_at = Timestamp::now();
        let outgoing = direction == Direction::Outgoing;
        let mut tx = self.app.begin().await.map_err(database_error)?;

        // `DO UPDATE` rather than `DO NOTHING` so `RETURNING` yields the id.
        let conversation: i64 = sqlx::query_scalar(
            "INSERT INTO conversations (peer) VALUES (?)
             ON CONFLICT (peer) DO UPDATE SET peer = excluded.peer
             RETURNING id",
        )
        .bind(peer.uuid().as_bytes().as_slice())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;

        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO messages (conversation_id, outgoing, sent_at, received_at, body, read)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT DO NOTHING
             RETURNING id",
        )
        .bind(conversation)
        .bind(outgoing)
        .bind(millis(sent_at))
        .bind(millis(received_at))
        .bind(body.as_str())
        .bind(outgoing)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;

        let Some(id) = id else {
            tx.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        sqlx::query(
            "UPDATE conversations SET last_activity = max(coalesce(last_activity, 0), ?)
             WHERE id = ?",
        )
        .bind(millis(received_at))
        .bind(conversation)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;

        Ok(Some(Message {
            id: MessageId::new(id),
            conversation: ConversationId::new(conversation),
            direction,
            sent_at,
            received_at,
            body: body.clone(),
            read: outgoing,
        }))
    }

    /// All conversations, most recently active first.
    pub(crate) async fn conversations(&self) -> Result<Vec<ConversationSummary>, StorageError> {
        let rows: Vec<SummaryRow> = sqlx::query_as(concat!(
            summary_query!(),
            " ORDER BY c.last_activity DESC, c.id DESC"
        ))
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            summaries.push(self.summary(row).await?);
        }
        Ok(summaries)
    }

    pub(crate) async fn conversation(
        &self,
        id: ConversationId,
    ) -> Result<Option<ConversationSummary>, StorageError> {
        let row: Option<SummaryRow> = sqlx::query_as(concat!(summary_query!(), " WHERE c.id = ?"))
            .bind(id.get())
            .fetch_optional(&self.app)
            .await
            .map_err(database_error)?;
        match row {
            Some(row) => Ok(Some(self.summary(row).await?)),
            None => Ok(None),
        }
    }

    /// Up to `limit` messages older than `before` (or the newest), oldest
    /// first.
    pub(crate) async fn messages(
        &self,
        conversation: ConversationId,
        before: Option<MessageId>,
        limit: u32,
    ) -> Result<Vec<Message>, StorageError> {
        let rows: Vec<MessageRow> = sqlx::query_as(
            "SELECT id, conversation_id, outgoing, sent_at, received_at, body, read
             FROM messages
             WHERE conversation_id = ?1
               AND (?2 IS NULL OR (sent_at, id) < (SELECT sent_at, id FROM messages WHERE id = ?2))
             ORDER BY sent_at DESC, id DESC
             LIMIT ?3",
        )
        .bind(conversation.get())
        .bind(before.map(MessageId::get))
        .bind(limit)
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        Ok(rows.into_iter().rev().map(message).collect())
    }

    /// Marks incoming messages read; `true` if any were unread.
    pub(crate) async fn mark_read(
        &self,
        conversation: ConversationId,
    ) -> Result<bool, StorageError> {
        let result =
            sqlx::query("UPDATE messages SET read = 1 WHERE conversation_id = ? AND read = 0")
                .bind(conversation.get())
                .execute(&self.app)
                .await
                .map_err(database_error)?;
        Ok(result.rows_affected() > 0)
    }

    /// Whether any contacts have been synced from the phone.
    pub(crate) async fn has_contacts(&self) -> Result<bool, StorageError> {
        Ok(self.contact_count().await? > 0)
    }

    /// Contacts synced from the phone.
    pub(crate) async fn contact_count(&self) -> Result<usize, StorageError> {
        let contacts = self
            .presage
            .contacts()
            .await
            .map_err(|error| StorageError::Database(error.to_string()))?;
        Ok(contacts.count())
    }

    async fn summary(&self, row: SummaryRow) -> Result<ConversationSummary, StorageError> {
        let (id, peer, last_activity, unread, outgoing, body) = row;
        let peer = Uuid::from_slice(&peer)
            .map(ServiceId::new)
            .map_err(|_| StorageError::Database("invalid peer in conversations".into()))?;
        let last_message = outgoing.zip(body).map(|(outgoing, body)| MessagePreview {
            direction: direction(outgoing),
            body: MessageBody::new(body),
        });
        Ok(ConversationSummary {
            id: ConversationId::new(id),
            peer: Peer::Contact(peer),
            title: self.contact_name(peer).await,
            last_activity: last_activity.map(timestamp),
            unread: u32::try_from(unread).unwrap_or(u32::MAX),
            last_message,
        })
    }

    /// The synced contact name; `None` if unknown or blank.
    async fn contact_name(&self, peer: ServiceId) -> Option<String> {
        let id: SignalServiceId = Aci::from(peer.uuid()).into();
        match self.presage.contact_by_id(&id).await {
            Ok(Some(contact)) if !contact.name.trim().is_empty() => Some(contact.name),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "reading a contact failed");
                None
            }
        }
    }
}

fn message(row: MessageRow) -> Message {
    let (id, conversation, outgoing, sent_at, received_at, body, read) = row;
    Message {
        id: MessageId::new(id),
        conversation: ConversationId::new(conversation),
        direction: direction(outgoing),
        sent_at: timestamp(sent_at),
        received_at: timestamp(received_at),
        body: MessageBody::new(body),
        read,
    }
}

fn direction(outgoing: bool) -> Direction {
    if outgoing {
        Direction::Outgoing
    } else {
        Direction::Incoming
    }
}

/// SQLite integers are signed; millisecond timestamps fit.
fn millis(timestamp: Timestamp) -> i64 {
    i64::try_from(timestamp.as_millis()).unwrap_or(i64::MAX)
}

fn timestamp(millis: i64) -> Timestamp {
    Timestamp::from_millis(u64::try_from(millis).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use presage::model::contacts::Contact;
    use tempfile::TempDir;

    use super::*;
    use crate::storage::DatabaseKey;

    async fn db() -> (TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db"), &DatabaseKey::generate())
            .await
            .unwrap();
        (dir, db)
    }

    fn peer(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    async fn add(
        db: &Database,
        from: u128,
        direction: Direction,
        sent_at: u64,
        body: &str,
    ) -> Option<Message> {
        db.add_message(
            peer(from),
            direction,
            Timestamp::from_millis(sent_at),
            &MessageBody::new(body),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn incoming_message_creates_an_unread_conversation() {
        let (_dir, db) = db().await;

        let message = add(&db, 1, Direction::Incoming, 100, "hello")
            .await
            .unwrap();

        assert!(!message.read);
        assert_eq!(message.sent_at, Timestamp::from_millis(100));
        let summary = db
            .conversation(message.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.peer, Peer::Contact(peer(1)));
        assert_eq!(summary.unread, 1);
        assert_eq!(summary.last_activity, Some(message.received_at));
        assert_eq!(
            summary.last_message,
            Some(MessagePreview {
                direction: Direction::Incoming,
                body: MessageBody::new("hello"),
            })
        );
        db.close().await;
    }

    #[tokio::test]
    async fn duplicates_are_ignored() {
        let (_dir, db) = db().await;
        let first = add(&db, 1, Direction::Incoming, 100, "hello")
            .await
            .unwrap();

        assert_eq!(add(&db, 1, Direction::Incoming, 100, "hello").await, None);

        let summary = db.conversation(first.conversation).await.unwrap().unwrap();
        assert_eq!(summary.unread, 1);
        assert_eq!(
            db.messages(first.conversation, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        db.close().await;
    }

    #[tokio::test]
    async fn same_time_in_both_directions_is_two_messages() {
        let (_dir, db) = db().await;
        let incoming = add(&db, 1, Direction::Incoming, 100, "hi").await.unwrap();

        let outgoing = add(&db, 1, Direction::Outgoing, 100, "hi").await.unwrap();

        assert_eq!(incoming.conversation, outgoing.conversation);
        assert!(outgoing.read);
        let summary = db
            .conversation(incoming.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.unread, 1);
        db.close().await;
    }

    #[tokio::test]
    async fn conversations_are_ordered_by_last_activity() {
        let (_dir, db) = db().await;
        add(&db, 1, Direction::Incoming, 100, "a").await;
        add(&db, 2, Direction::Incoming, 100, "b").await;
        add(&db, 1, Direction::Outgoing, 200, "c").await;

        let peers: Vec<_> = db
            .conversations()
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.peer)
            .collect();

        assert_eq!(peers, [Peer::Contact(peer(1)), Peer::Contact(peer(2))]);
        db.close().await;
    }

    #[tokio::test]
    async fn messages_page_backwards_in_sent_order() {
        let (_dir, db) = db().await;
        // Arrival order differs from sent order.
        for sent_at in [3, 1, 5, 2, 4] {
            add(&db, 1, Direction::Incoming, sent_at, &sent_at.to_string()).await;
        }
        let conversation = db.conversations().await.unwrap()[0].id;
        let sent = |page: &[Message]| {
            page.iter()
                .map(|m| m.sent_at.as_millis())
                .collect::<Vec<_>>()
        };

        let newest = db.messages(conversation, None, 2).await.unwrap();
        assert_eq!(sent(&newest), [4, 5]);
        let older = db
            .messages(conversation, Some(newest[0].id), 2)
            .await
            .unwrap();
        assert_eq!(sent(&older), [2, 3]);
        let oldest = db
            .messages(conversation, Some(older[0].id), 2)
            .await
            .unwrap();
        assert_eq!(sent(&oldest), [1]);
        db.close().await;
    }

    #[tokio::test]
    async fn mark_read_clears_unread_once() {
        let (_dir, db) = db().await;
        let message = add(&db, 1, Direction::Incoming, 100, "a").await.unwrap();
        add(&db, 1, Direction::Incoming, 101, "b").await;

        assert!(db.mark_read(message.conversation).await.unwrap());
        assert!(!db.mark_read(message.conversation).await.unwrap());

        let summary = db
            .conversation(message.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.unread, 0);
        db.close().await;
    }

    #[tokio::test]
    async fn titles_come_from_synced_contacts() {
        let (_dir, db) = db().await;
        let named = add(&db, 1, Direction::Incoming, 100, "a").await.unwrap();
        let unnamed = add(&db, 2, Direction::Incoming, 100, "b").await.unwrap();
        assert!(!db.has_contacts().await.unwrap());

        let mut store = db.presage.clone();
        for (uuid, name) in [(1, "Alice"), (2, "  ")] {
            let contact = Contact {
                uuid: Uuid::from_u128(uuid),
                phone_number: None,
                name: name.into(),
                verified: Default::default(),
                profile_key: Vec::new(),
                expire_timer: 0,
                expire_timer_version: 1,
                inbox_position: 0,
                avatar: None,
            };
            store.save_contact(&contact).await.unwrap();
        }

        assert!(db.has_contacts().await.unwrap());
        assert_eq!(db.contact_count().await.unwrap(), 2);
        let named = db.conversation(named.conversation).await.unwrap().unwrap();
        let unnamed = db
            .conversation(unnamed.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(named.title.as_deref(), Some("Alice"));
        assert_eq!(unnamed.title, None);
        db.close().await;
    }

    #[test]
    fn summary_debug_hides_the_title() {
        let summary = ConversationSummary {
            id: ConversationId::new(1),
            peer: Peer::Contact(peer(1)),
            title: Some("Alice Secret".into()),
            last_activity: None,
            unread: 0,
            last_message: None,
        };
        assert!(!format!("{summary:?}").contains("Alice"));
    }
}
