//! Conversations and messages in Copiale's tables.

use presage::libsignal_service::protocol::{Aci, ServiceId as SignalServiceId};
use presage::store::ContentsStore as _;
use uuid::Uuid;

use super::StorageError;
use super::changes::apply_pending;
use super::database::{Database, database_error};
use crate::model::{
    Content, ConversationId, ConversationSummary, Destination, Direction, GroupId, GroupInfo,
    GroupMasterKey, GroupRef, Member, Message, MessageBody, MessageId, MessagePreview,
    MessageStatus, Peer, ServiceId, Timestamp,
};

/// Summary columns: id, peer, group ID, group title, last_activity,
/// unread, last message direction and body. A macro so queries stay string
/// literals, which sqlx requires.
macro_rules! summary_query {
    () => {
        "SELECT c.id, c.peer, c.group_id, c.group_title, c.last_activity,
                (SELECT count(*) FROM messages u WHERE u.conversation_id = c.id AND u.read = 0),
                m.outgoing, m.body, m.content
         FROM conversations c
         LEFT JOIN messages m ON m.id = (
             SELECT id FROM messages WHERE conversation_id = c.id
             ORDER BY sent_at DESC, id DESC LIMIT 1
         )"
    };
}

/// Message columns, in [`MessageRow`] order.
macro_rules! message_columns {
    () => {
        "id, conversation_id, outgoing, author, sent_at, received_at, body, content, read, status, \
         delivery, edited_at"
    };
}

type SummaryRow = (
    i64,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<String>,
    Option<i64>,
    i64,
    Option<bool>,
    Option<String>,
    Option<String>,
);
type MessageRow = (
    i64,
    i64,
    bool,
    Vec<u8>,
    i64,
    i64,
    String,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
    Option<i64>,
);
type DestinationRow = (Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>);

impl Database {
    /// Stores a received message, or one sent from another device, unless
    /// it's already stored (same conversation, author and sent time).
    /// `author` is `None` for this account. Returns it if new.
    pub(crate) async fn add_message(
        &self,
        destination: &Destination,
        author: Option<ServiceId>,
        sent_at: Timestamp,
        content: &Content,
    ) -> Result<Option<Message>, StorageError> {
        let mut tx = self.app.begin().await.map_err(database_error)?;
        let conversation = conversation_id(&mut tx, destination).await?;
        let status = (author.is_none() && !content.is_system()).then_some(MessageStatus::Sent);
        let message =
            insert_message(&mut tx, conversation, author, status, sent_at, content).await?;
        let message = match message {
            Some(message) => Some(apply_pending(&mut tx, message).await?),
            None => None,
        };
        tx.commit().await.map_err(database_error)?;
        Ok(message)
    }

    /// Stores a message this device is about to send. `None` if one with the
    /// same sent time exists, or there is no such conversation.
    pub(crate) async fn add_pending(
        &self,
        conversation: ConversationId,
        sent_at: Timestamp,
        content: &Content,
    ) -> Result<Option<Message>, StorageError> {
        let mut tx = self.app.begin().await.map_err(database_error)?;
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM conversations WHERE id = ?")
            .bind(conversation.get())
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?;
        let Some(conversation) = exists else {
            return Ok(None);
        };
        let status = Some(MessageStatus::Pending);
        let message = insert_message(&mut tx, conversation, None, status, sent_at, content).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(message)
    }

    /// The conversation with `destination`, created if new.
    pub(crate) async fn open_conversation(
        &self,
        destination: &Destination,
    ) -> Result<ConversationId, StorageError> {
        let mut tx = self.app.begin().await.map_err(database_error)?;
        let id = conversation_id(&mut tx, destination).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ConversationId::new(id))
    }

    /// Where messages in `conversation` go.
    pub(crate) async fn destination(
        &self,
        conversation: ConversationId,
    ) -> Result<Option<Destination>, StorageError> {
        let row: Option<DestinationRow> = sqlx::query_as(
            "SELECT peer, group_master_key, group_revision FROM conversations WHERE id = ?",
        )
        .bind(conversation.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        row.map(destination).transpose()
    }

    /// Sets a pending message's status, sent or failed; returns it, or
    /// `None` if there is no such pending message.
    pub(crate) async fn set_status(
        &self,
        id: MessageId,
        status: MessageStatus,
    ) -> Result<Option<Message>, StorageError> {
        let row: Option<MessageRow> = sqlx::query_as(concat!(
            "UPDATE messages SET status = ? WHERE id = ? AND status = 'pending' RETURNING ",
            message_columns!()
        ))
        .bind(status_name(status))
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        row.map(message).transpose()
    }

    /// Marks a failed message pending again; returns it with where it goes,
    /// or `None` if it hasn't failed.
    pub(crate) async fn retry_send(
        &self,
        id: MessageId,
    ) -> Result<Option<(Destination, Message)>, StorageError> {
        let mut tx = self.app.begin().await.map_err(database_error)?;
        let row: Option<MessageRow> = sqlx::query_as(concat!(
            "UPDATE messages SET status = 'pending' WHERE id = ? AND status = 'failed' RETURNING ",
            message_columns!()
        ))
        .bind(id.get())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let Some(message) = row.map(message).transpose()? else {
            return Ok(None);
        };
        let row: DestinationRow = sqlx::query_as(
            "SELECT peer, group_master_key, group_revision FROM conversations WHERE id = ?",
        )
        .bind(message.conversation.get())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(Some((destination(row)?, message)))
    }

    /// Marks messages left pending, e.g. by a quit mid-send, failed;
    /// returns how many.
    pub(crate) async fn fail_pending(&self) -> Result<u64, StorageError> {
        let result = sqlx::query("UPDATE messages SET status = 'failed' WHERE status = 'pending'")
            .execute(&self.app)
            .await
            .map_err(database_error)?;
        Ok(result.rows_affected())
    }

    /// The stored revision of a known group's state: `Some(None)` if it
    /// hasn't been fetched, `None` if there is no such group.
    pub(crate) async fn group_revision(
        &self,
        key: GroupMasterKey,
    ) -> Result<Option<Option<u32>>, StorageError> {
        let revision: Option<Option<i64>> = sqlx::query_scalar(
            "SELECT group_revision FROM conversations WHERE group_master_key = ?",
        )
        .bind(key.as_bytes().as_slice())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        Ok(revision.map(|revision| revision.and_then(|r| u32::try_from(r).ok())))
    }

    /// All groups' master keys.
    pub(crate) async fn group_keys(&self) -> Result<Vec<GroupMasterKey>, StorageError> {
        let keys: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT group_master_key FROM conversations WHERE group_master_key IS NOT NULL",
        )
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        keys.iter().map(|key| master_key(key)).collect()
    }

    /// Stores a group's fetched state; returns its conversation, or `None`
    /// if there is no such group.
    pub(crate) async fn set_group(
        &self,
        key: GroupMasterKey,
        info: &GroupInfo,
    ) -> Result<Option<ConversationId>, StorageError> {
        let mut tx = self.app.begin().await.map_err(database_error)?;
        let id: Option<i64> = sqlx::query_scalar(
            "UPDATE conversations SET group_title = ?, group_revision = ?
             WHERE group_master_key = ? RETURNING id",
        )
        .bind(&info.title)
        .bind(i64::from(info.revision))
        .bind(key.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let Some(id) = id else {
            return Ok(None);
        };
        sqlx::query("DELETE FROM group_members WHERE conversation_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        for (position, member) in info.members.iter().enumerate() {
            sqlx::query(
                "INSERT OR IGNORE INTO group_members (conversation_id, member, name, position)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(member.id.uuid().as_bytes().as_slice())
            .bind(member.name.as_deref())
            .bind(position as i64)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        }
        tx.commit().await.map_err(database_error)?;
        Ok(Some(ConversationId::new(id)))
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
        let rows: Vec<MessageRow> = sqlx::query_as(concat!(
            "SELECT ",
            message_columns!(),
            " FROM messages
             WHERE conversation_id = ?1
               AND (?2 IS NULL OR (sent_at, id) < (SELECT sent_at, id FROM messages WHERE id = ?2))
             ORDER BY sent_at DESC, id DESC
             LIMIT ?3"
        ))
        .bind(conversation.get())
        .bind(before.map(MessageId::get))
        .bind(limit)
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        rows.into_iter().rev().map(message).collect()
    }

    pub(crate) async fn message(&self, id: MessageId) -> Result<Option<Message>, StorageError> {
        let mut conn = self.app.acquire().await.map_err(database_error)?;
        load_message(&mut conn, id).await
    }

    /// Marks incoming messages read; returns the newly read ones by author
    /// and sent time.
    pub(crate) async fn mark_read(
        &self,
        conversation: ConversationId,
    ) -> Result<Vec<(ServiceId, Timestamp)>, StorageError> {
        let rows: Vec<(Vec<u8>, i64)> = sqlx::query_as(
            "UPDATE messages SET read = 1 WHERE conversation_id = ? AND read = 0
             RETURNING author, sent_at",
        )
        .bind(conversation.get())
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|(author, sent_at)| Ok((service_id(&author)?, timestamp(sent_at))))
            .collect()
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
        let (id, peer, group_id, group_title, last_activity, unread, outgoing, body, content) = row;
        let last_message = match outgoing.zip(body) {
            Some((outgoing, body)) => Some(MessagePreview {
                direction: direction(outgoing),
                content: content_from(body, content)?,
            }),
            None => None,
        };
        let (peer, title, members) = match (peer, group_id) {
            (Some(peer), _) => {
                let peer = service_id(&peer)?;
                (
                    Peer::Contact(peer),
                    self.contact_name(peer).await,
                    Vec::new(),
                )
            }
            (None, Some(group_id)) => {
                let bytes = <[u8; 32]>::try_from(group_id.as_slice())
                    .map_err(|_| StorageError::Database("invalid group ID".into()))?;
                let title = group_title.filter(|title| !title.trim().is_empty());
                (
                    Peer::Group(GroupId::new(bytes)),
                    title,
                    self.members(id).await?,
                )
            }
            (None, None) => return Err(StorageError::Database("conversation without peer".into())),
        };
        Ok(ConversationSummary {
            id: ConversationId::new(id),
            peer,
            title,
            members,
            last_activity: last_activity.map(timestamp),
            unread: u32::try_from(unread).unwrap_or(u32::MAX),
            last_message,
        })
    }

    async fn members(&self, conversation: i64) -> Result<Vec<Member>, StorageError> {
        let rows: Vec<(Vec<u8>, Option<String>)> = sqlx::query_as(
            "SELECT member, name FROM group_members WHERE conversation_id = ? ORDER BY position",
        )
        .bind(conversation)
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        rows.into_iter()
            .map(|(member, name)| {
                Ok(Member {
                    id: service_id(&member)?,
                    name,
                })
            })
            .collect()
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

pub(super) async fn load_message(
    conn: &mut sqlx::SqliteConnection,
    id: MessageId,
) -> Result<Option<Message>, StorageError> {
    let row: Option<MessageRow> = sqlx::query_as(concat!(
        "SELECT ",
        message_columns!(),
        " FROM messages WHERE id = ?"
    ))
    .bind(id.get())
    .fetch_optional(&mut *conn)
    .await
    .map_err(database_error)?;
    row.map(message).transpose()
}

/// The conversation with `destination`, created if new.
async fn conversation_id(
    tx: &mut sqlx::SqliteConnection,
    destination: &Destination,
) -> Result<i64, StorageError> {
    match destination {
        Destination::Contact(peer) => {
            // `DO UPDATE` rather than `DO NOTHING` so `RETURNING` yields the id.
            sqlx::query_scalar(
                "INSERT INTO conversations (peer) VALUES (?)
                 ON CONFLICT (peer) DO UPDATE SET peer = excluded.peer
                 RETURNING id",
            )
            .bind(peer.uuid().as_bytes().as_slice())
            .fetch_one(&mut *tx)
            .await
        }
        Destination::Group(group) => {
            sqlx::query_scalar(
                "INSERT INTO conversations (group_master_key, group_id) VALUES (?, ?)
                 ON CONFLICT (group_master_key) DO UPDATE
                     SET group_master_key = excluded.group_master_key
                 RETURNING id",
            )
            .bind(group.key.as_bytes().as_slice())
            .bind(group.key.id().as_bytes().as_slice())
            .fetch_one(&mut *tx)
            .await
        }
    }
    .map_err(database_error)
}

/// Inserts a message into `conversation` unless it's a duplicate, and
/// bumps the conversation's activity.
async fn insert_message(
    tx: &mut sqlx::SqliteConnection,
    conversation: i64,
    author: Option<ServiceId>,
    status: Option<MessageStatus>,
    sent_at: Timestamp,
    content: &Content,
) -> Result<Option<Message>, StorageError> {
    let received_at = Timestamp::now();
    let outgoing = author.is_none();
    let read = outgoing || content.is_quiet();
    let author_bytes = author.map(|author| author.uuid().as_bytes().to_vec());
    let (body, json) = content_columns(content)?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO messages
             (conversation_id, outgoing, author, sent_at, received_at, body, content, read, status)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO NOTHING
         RETURNING id",
    )
    .bind(conversation)
    .bind(outgoing)
    .bind(author_bytes.unwrap_or_default())
    .bind(millis(sent_at))
    .bind(millis(received_at))
    .bind(body)
    .bind(json)
    .bind(read)
    .bind(status.map(status_name))
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;

    let Some(id) = id else {
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

    Ok(Some(Message {
        id: MessageId::new(id),
        conversation: ConversationId::new(conversation),
        direction: direction(outgoing),
        sender: author,
        sent_at,
        received_at,
        content: content.clone(),
        edited: false,
        read,
        status,
    }))
}

/// The `body` and `content` columns for `content`.
pub(super) fn content_columns(content: &Content) -> Result<(&str, Option<String>), StorageError> {
    match content {
        Content::Text {
            body,
            attachments,
            quote: None,
        } if attachments.is_empty() => Ok((body.as_str(), None)),
        other => {
            let body = other.body().map_or("", MessageBody::as_str);
            let json = serde_json::to_string(other)
                .map_err(|error| StorageError::Database(error.to_string()))?;
            Ok((body, Some(json)))
        }
    }
}

fn content_from(body: String, json: Option<String>) -> Result<Content, StorageError> {
    match json {
        None => Ok(Content::text(body)),
        Some(json) => serde_json::from_str(&json)
            .map_err(|_| StorageError::Database("invalid message content".into())),
    }
}

fn destination(row: DestinationRow) -> Result<Destination, StorageError> {
    match row {
        (Some(peer), _, _) => Ok(Destination::Contact(service_id(&peer)?)),
        (None, Some(key), revision) => Ok(Destination::Group(GroupRef {
            key: master_key(&key)?,
            revision: revision.and_then(|r| u32::try_from(r).ok()).unwrap_or(0),
        })),
        (None, None, _) => Err(StorageError::Database("conversation without peer".into())),
    }
}

fn master_key(bytes: &[u8]) -> Result<GroupMasterKey, StorageError> {
    <[u8; 32]>::try_from(bytes)
        .map(GroupMasterKey::new)
        .map_err(|_| StorageError::Database("invalid group master key".into()))
}

fn message(row: MessageRow) -> Result<Message, StorageError> {
    let (
        id,
        conversation,
        outgoing,
        author,
        sent_at,
        received_at,
        body,
        content,
        read,
        status,
        delivery,
        edited_at,
    ) = row;
    let status = match (status.as_deref(), delivery.as_deref()) {
        (Some("sent"), Some("delivered")) => Some(MessageStatus::Delivered),
        (Some("sent"), Some("read")) => Some(MessageStatus::Read),
        (status, _) => status.map(parse_status).transpose()?,
    };
    let sender = if author.is_empty() {
        None
    } else {
        Some(service_id(&author)?)
    };
    Ok(Message {
        id: MessageId::new(id),
        conversation: ConversationId::new(conversation),
        direction: direction(outgoing),
        sender,
        sent_at: timestamp(sent_at),
        received_at: timestamp(received_at),
        content: content_from(body, content)?,
        edited: edited_at.is_some(),
        read,
        status,
    })
}

fn service_id(peer: &[u8]) -> Result<ServiceId, StorageError> {
    Uuid::from_slice(peer)
        .map(ServiceId::new)
        .map_err(|_| StorageError::Database("invalid service ID".into()))
}

/// The `status` column's value; delivery is in its own column.
fn status_name(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Pending => "pending",
        MessageStatus::Sent | MessageStatus::Delivered | MessageStatus::Read => "sent",
        MessageStatus::Failed => "failed",
    }
}

fn parse_status(name: &str) -> Result<MessageStatus, StorageError> {
    match name {
        "pending" => Ok(MessageStatus::Pending),
        "sent" => Ok(MessageStatus::Sent),
        "failed" => Ok(MessageStatus::Failed),
        _ => Err(StorageError::Database("invalid message status".into())),
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
pub(super) fn millis(timestamp: Timestamp) -> i64 {
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
    use crate::model::{Attachment, GroupUpdate, Unsupported};
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
        let author = (direction == Direction::Incoming).then(|| peer(from));
        db.add_message(
            &Destination::Contact(peer(from)),
            author,
            Timestamp::from_millis(sent_at),
            &Content::text(body),
        )
        .await
        .unwrap()
    }

    fn group(n: u8) -> GroupRef {
        GroupRef {
            key: GroupMasterKey::new([n; 32]),
            revision: 1,
        }
    }

    async fn add_to_group(
        db: &Database,
        group: GroupRef,
        from: Option<u128>,
        sent_at: u64,
    ) -> Option<Message> {
        db.add_message(
            &Destination::Group(group),
            from.map(peer),
            Timestamp::from_millis(sent_at),
            &Content::text("hi all"),
        )
        .await
        .unwrap()
    }

    async fn conversation_with(db: &Database, destination: Destination) -> ConversationId {
        db.open_conversation(&destination).await.unwrap()
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
                content: Content::text("hello"),
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
    async fn only_outgoing_messages_have_a_status() {
        let (_dir, db) = db().await;

        let incoming = add(&db, 1, Direction::Incoming, 100, "a").await.unwrap();
        let from_phone = add(&db, 1, Direction::Outgoing, 101, "b").await.unwrap();

        assert_eq!(incoming.status, None);
        assert_eq!(from_phone.status, Some(MessageStatus::Sent));
        let stored = db.messages(incoming.conversation, None, 10).await.unwrap();
        assert_eq!(stored, [incoming, from_phone]);
        db.close().await;
    }

    async fn pending(db: &Database, to: u128, sent_at: u64) -> Option<Message> {
        let conversation = conversation_with(db, Destination::Contact(peer(to))).await;
        db.add_pending(
            conversation,
            Timestamp::from_millis(sent_at),
            &Content::text("out"),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn pending_message_is_outgoing_and_read() {
        let (_dir, db) = db().await;

        let message = pending(&db, 1, 100).await.unwrap();

        assert_eq!(message.direction, Direction::Outgoing);
        assert_eq!(message.status, Some(MessageStatus::Pending));
        assert!(message.read);
        let summary = db
            .conversation(message.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.unread, 0);
        assert_eq!(summary.last_activity, Some(message.received_at));
        assert_eq!(pending(&db, 1, 100).await, None);
        db.close().await;
    }

    #[tokio::test]
    async fn status_changes_are_stored() {
        let (_dir, db) = db().await;
        let message = pending(&db, 1, 100).await.unwrap();

        let sent = db
            .set_status(message.id, MessageStatus::Sent)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(sent.status, Some(MessageStatus::Sent));
        assert_eq!(sent.id, message.id);
        let stored = db.messages(message.conversation, None, 10).await.unwrap();
        assert_eq!(stored, [sent]);
        db.close().await;
    }

    #[tokio::test]
    async fn incoming_messages_get_no_status() {
        let (_dir, db) = db().await;
        let message = add(&db, 1, Direction::Incoming, 100, "a").await.unwrap();

        let updated = db
            .set_status(message.id, MessageStatus::Sent)
            .await
            .unwrap();

        assert_eq!(updated, None);
        db.close().await;
    }

    #[tokio::test]
    async fn only_failed_messages_can_be_retried() {
        let (_dir, db) = db().await;
        let message = pending(&db, 7, 100).await.unwrap();
        assert_eq!(db.retry_send(message.id).await.unwrap(), None);
        db.set_status(message.id, MessageStatus::Failed)
            .await
            .unwrap();

        let (destination, retried) = db.retry_send(message.id).await.unwrap().unwrap();

        assert_eq!(destination, Destination::Contact(peer(7)));
        assert_eq!(retried.status, Some(MessageStatus::Pending));
        assert_eq!(retried.sent_at, message.sent_at);
        assert_eq!(db.retry_send(message.id).await.unwrap(), None);
        db.close().await;
    }

    #[tokio::test]
    async fn leftover_pending_messages_fail() {
        let (_dir, db) = db().await;
        let first = pending(&db, 1, 100).await.unwrap();
        let second = pending(&db, 2, 100).await.unwrap();
        db.set_status(second.id, MessageStatus::Sent).await.unwrap();

        assert_eq!(db.fail_pending().await.unwrap(), 1);

        let stored = db.messages(first.conversation, None, 10).await.unwrap();
        assert_eq!(stored[0].status, Some(MessageStatus::Failed));
        let stored = db.messages(second.conversation, None, 10).await.unwrap();
        assert_eq!(stored[0].status, Some(MessageStatus::Sent));
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

        assert_eq!(db.mark_read(message.conversation).await.unwrap().len(), 2);
        assert!(db.mark_read(message.conversation).await.unwrap().is_empty());

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

    #[tokio::test]
    async fn incoming_messages_keep_their_author() {
        let (_dir, db) = db().await;

        let incoming = add(&db, 1, Direction::Incoming, 100, "a").await.unwrap();
        let outgoing = add(&db, 1, Direction::Outgoing, 101, "b").await.unwrap();

        assert_eq!(incoming.sender, Some(peer(1)));
        assert_eq!(outgoing.sender, None);
        let stored = db.messages(incoming.conversation, None, 10).await.unwrap();
        assert_eq!(stored, [incoming, outgoing]);
        db.close().await;
    }

    #[tokio::test]
    async fn group_messages_share_a_conversation_by_master_key() {
        let (_dir, db) = db().await;

        let from_alice = add_to_group(&db, group(1), Some(1), 100).await.unwrap();
        // Same time, other author: a different message.
        let from_bob = add_to_group(&db, group(1), Some(2), 100).await.unwrap();
        let mine = add_to_group(&db, group(1), None, 100).await.unwrap();
        let elsewhere = add_to_group(&db, group(2), Some(1), 100).await.unwrap();

        assert_eq!(add_to_group(&db, group(1), Some(1), 100).await, None);
        assert_eq!(from_alice.conversation, from_bob.conversation);
        assert_eq!(from_alice.conversation, mine.conversation);
        assert_ne!(from_alice.conversation, elsewhere.conversation);
        assert_eq!(from_bob.sender, Some(peer(2)));
        assert_eq!(mine.direction, Direction::Outgoing);
        assert_eq!(mine.status, Some(MessageStatus::Sent));

        let summary = db
            .conversation(from_alice.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.peer, Peer::Group(group(1).key.id()));
        assert_eq!(summary.title, None);
        assert_eq!(summary.members, []);
        assert_eq!(summary.unread, 2);
        let destination = db.destination(from_alice.conversation).await.unwrap();
        assert_eq!(
            destination,
            Some(Destination::Group(GroupRef {
                key: group(1).key,
                revision: 0,
            }))
        );
        db.close().await;
    }

    #[tokio::test]
    async fn group_state_is_stored() {
        let (_dir, db) = db().await;
        let key = group(1).key;
        let info = GroupInfo {
            title: "Dev".into(),
            revision: 4,
            members: vec![
                Member {
                    id: peer(2),
                    name: Some("Bob".into()),
                },
                Member {
                    id: peer(1),
                    name: None,
                },
            ],
        };
        assert_eq!(db.group_revision(key).await.unwrap(), None);
        assert_eq!(db.set_group(key, &info).await.unwrap(), None);

        let message = add_to_group(&db, group(1), Some(1), 100).await.unwrap();
        assert_eq!(db.group_revision(key).await.unwrap(), Some(None));
        assert_eq!(db.group_keys().await.unwrap(), [key]);

        assert_eq!(
            db.set_group(key, &info).await.unwrap(),
            Some(message.conversation)
        );
        assert_eq!(db.group_revision(key).await.unwrap(), Some(Some(4)));
        let summary = db
            .conversation(message.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.title.as_deref(), Some("Dev"));
        assert_eq!(summary.members, info.members);
        let Some(Destination::Group(group)) = db.destination(message.conversation).await.unwrap()
        else {
            panic!("not a group");
        };
        assert_eq!(group.revision, 4);

        let fewer = GroupInfo {
            members: info.members[..1].to_vec(),
            ..info
        };
        db.set_group(key, &fewer).await.unwrap();
        let summary = db
            .conversation(message.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.members, fewer.members);
        db.close().await;
    }

    #[tokio::test]
    async fn pending_needs_an_existing_conversation() {
        let (_dir, db) = db().await;
        let missing = db
            .add_pending(
                ConversationId::new(42),
                Timestamp::from_millis(1),
                &Content::text("x"),
            )
            .await
            .unwrap();
        assert_eq!(missing, None);
        db.close().await;
    }

    async fn add_content(db: &Database, sent_at: u64, content: Content) -> Message {
        db.add_message(
            &Destination::Group(group(1)),
            Some(peer(1)),
            Timestamp::from_millis(sent_at),
            &content,
        )
        .await
        .unwrap()
        .unwrap()
    }

    #[tokio::test]
    async fn non_text_content_round_trips() {
        let (_dir, db) = db().await;
        let contents = [
            Content::Text {
                body: MessageBody::new("the graph"),
                attachments: vec![
                    Attachment::Image,
                    Attachment::File {
                        name: Some("a.pdf".into()),
                    },
                ],
                quote: None,
            },
            Content::Sticker { emoji: None },
            Content::Call { video: true },
            Content::Unsupported(Unsupported::Poll),
            Content::GroupChanged(vec![
                GroupUpdate::Title("Dev".into()),
                GroupUpdate::Added(peer(2)),
            ]),
            Content::TimerChanged(Some(60)),
        ];
        let mut stored = Vec::new();
        for (sent_at, content) in (1..).zip(contents.clone()) {
            stored.push(add_content(&db, sent_at, content).await);
        }

        let loaded = db.messages(stored[0].conversation, None, 10).await.unwrap();

        assert_eq!(loaded, stored);
        let contents_loaded: Vec<_> = loaded.into_iter().map(|m| m.content).collect();
        assert_eq!(contents_loaded, contents);
        let summary = db
            .conversation(stored[0].conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            summary.last_message.map(|preview| preview.content),
            Some(Content::TimerChanged(Some(60)))
        );
        db.close().await;
    }

    #[tokio::test]
    async fn reactions_and_system_lines_arrive_read() {
        let (_dir, db) = db().await;

        let reaction = add_content(
            &db,
            1,
            Content::Reaction {
                emoji: "👍".into()
            },
        )
        .await;
        let update = add_content(&db, 2, Content::GroupChanged(vec![GroupUpdate::Avatar])).await;
        let call = add_content(&db, 3, Content::Call { video: false }).await;

        assert!(reaction.read && update.read);
        assert!(!call.read);
        let summary = db.conversation(call.conversation).await.unwrap().unwrap();
        assert_eq!(summary.unread, 1);
        let mine = db
            .add_message(
                &Destination::Group(group(1)),
                None,
                Timestamp::from_millis(4),
                &Content::TimerChanged(None),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mine.status, None, "system lines have no send status");
        db.close().await;
    }

    #[test]
    fn summary_debug_hides_the_title() {
        let summary = ConversationSummary {
            id: ConversationId::new(1),
            peer: Peer::Contact(peer(1)),
            title: Some("Alice Secret".into()),
            members: Vec::new(),
            last_activity: None,
            unread: 0,
            last_message: None,
        };
        assert!(!format!("{summary:?}").contains("Alice"));
    }
}
