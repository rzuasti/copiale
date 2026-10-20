//! Conversations and messages in Copiale's tables.

use std::collections::HashMap;

use uuid::Uuid;

use super::StorageError;
use super::changes::apply_pending;
use super::contacts::display_name;
use super::database::{Database, database_error};
use crate::model::{
    ChatState, Content, ConversationId, ConversationSummary, Destination, Direction, Expiry,
    GroupId, GroupInfo, GroupMasterKey, GroupRef, Member, Message, MessageBody, MessageId,
    MessagePreview, MessageStatus, Peer, ServiceId, Timestamp,
};

/// [`SummaryRow`] columns; the peer's names come from presage's tables. A
/// conversation is a request once the other side wrote, a changed safety
/// number aside, until accepted (`chat_states`). A macro so queries stay
/// string literals, which sqlx requires.
macro_rules! summary_query {
    () => {
        "SELECT c.id, c.peer, c.group_id, c.group_title, c.last_activity,
                (SELECT count(*) FROM messages u WHERE u.conversation_id = c.id AND u.read = 0)
                    AS unread,
                m.outgoing, m.body, m.content,
                ct.name AS contact_name, p.given_name, p.family_name,
                CASE
                    WHEN coalesce(s.blocked, 0) THEN 'blocked'
                    WHEN coalesce(s.accepted, 0) OR NOT EXISTS (
                        SELECT 1 FROM messages i
                        WHERE i.conversation_id = c.id AND i.outgoing = 0
                          AND i.content IS NOT '\"safety_number_changed\"'
                    ) THEN 'accepted'
                    ELSE 'request'
                END AS state
         FROM conversations c
         LEFT JOIN messages m ON m.id = (
             SELECT id FROM messages WHERE conversation_id = c.id
             ORDER BY sent_at DESC, id DESC LIMIT 1
         )
         LEFT JOIN contacts ct ON ct.uuid = c.peer
         LEFT JOIN profiles p ON p.uuid = c.peer
         LEFT JOIN chat_states s ON s.chat = coalesce(c.peer, c.group_id)"
    };
}

/// [`MessageRow`] columns.
macro_rules! message_columns {
    () => {
        "id, conversation_id, outgoing, author, sent_at, received_at, body, content, read, status, \
         delivery, edited_at, expires_in, expires_at"
    };
}
pub(super) use message_columns;

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: i64,
    peer: Option<Vec<u8>>,
    group_id: Option<Vec<u8>>,
    group_title: Option<String>,
    last_activity: Option<i64>,
    unread: i64,
    /// The last message's; `None` without one.
    outgoing: Option<bool>,
    body: Option<String>,
    content: Option<String>,
    contact_name: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
    state: String,
}

#[derive(sqlx::FromRow)]
pub(super) struct MessageRow {
    id: i64,
    conversation_id: i64,
    outgoing: bool,
    /// Empty for this account.
    author: Vec<u8>,
    sent_at: i64,
    received_at: i64,
    body: String,
    content: Option<String>,
    read: bool,
    status: Option<String>,
    delivery: Option<String>,
    edited_at: Option<i64>,
    expires_in: Option<i64>,
    expires_at: Option<i64>,
}

type DestinationRow = (Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>);

impl Database {
    /// Stores a received message, or one sent from another device, unless
    /// it's already stored (same conversation, author and sent time).
    /// `author` is `None` for this account. Returns it if new.
    ///
    /// With `expire_timer`, the countdown starts at once for messages that
    /// arrive read: at `sent_at` for ours, `now` for others'. System lines
    /// don't disappear.
    pub(crate) async fn add_message(
        &self,
        destination: &Destination,
        author: Option<ServiceId>,
        sent_at: Timestamp,
        content: &Content,
        expire_timer: Option<u32>,
        now: Timestamp,
    ) -> Result<Option<Message>, StorageError> {
        let mut tx = self.begin_write().await?;
        let conversation = conversation_id(&mut tx, destination).await?;
        let status = (author.is_none() && !content.is_system()).then_some(MessageStatus::Sent);
        let start = if author.is_none() {
            Some(sent_at)
        } else {
            content.is_quiet().then_some(now)
        };
        let expiry = expire_timer
            .filter(|_| !content.is_system())
            .map(|timer| Expiry {
                timer,
                at: start.map(|start| expires_at(start, timer)),
            });
        let message = insert_message(
            &mut tx,
            conversation,
            author,
            status,
            sent_at,
            content,
            expiry,
            now,
        )
        .await?;
        let message = match message {
            Some(message) => Some(apply_pending(&mut tx, message).await?),
            None => None,
        };
        tx.commit().await.map_err(database_error)?;
        Ok(message)
    }

    /// Stores a message this device is about to send; with `expire_timer`,
    /// its countdown starts once sent. `None` if one with the same sent time
    /// exists, or there is no such conversation.
    pub(crate) async fn add_pending(
        &self,
        conversation: ConversationId,
        sent_at: Timestamp,
        content: &Content,
        expire_timer: Option<u32>,
        now: Timestamp,
    ) -> Result<Option<Message>, StorageError> {
        let mut tx = self.begin_write().await?;
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM conversations WHERE id = ?")
            .bind(conversation.get())
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?;
        let Some(conversation) = exists else {
            return Ok(None);
        };
        let status = Some(MessageStatus::Pending);
        let expiry = expire_timer.map(|timer| Expiry { timer, at: None });
        let message = insert_message(
            &mut tx,
            conversation,
            None,
            status,
            sent_at,
            content,
            expiry,
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(message)
    }

    /// The conversation with `destination`, created if new.
    pub(crate) async fn open_conversation(
        &self,
        destination: &Destination,
    ) -> Result<ConversationId, StorageError> {
        let mut tx = self.begin_write().await?;
        let id = conversation_id(&mut tx, destination).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ConversationId::new(id))
    }

    /// Where messages in `conversation` go.
    pub(crate) async fn destination(
        &self,
        conversation: ConversationId,
    ) -> Result<Option<Destination>, StorageError> {
        load_destination(&self.app, conversation.get()).await
    }

    /// Sets a pending message's status, sent or failed; returns it, or
    /// `None` if there is no such pending message. Sent at `now` starts its
    /// countdown, if it disappears.
    pub(crate) async fn set_status(
        &self,
        id: MessageId,
        status: MessageStatus,
        now: Timestamp,
    ) -> Result<Option<Message>, StorageError> {
        let row: Option<MessageRow> = sqlx::query_as(concat!(
            "UPDATE messages SET status = ?1,
                 expires_at = CASE WHEN ?1 = 'sent' THEN ?3 + expires_in * 1000 END
             WHERE id = ?2 AND status = 'pending' RETURNING ",
            message_columns!()
        ))
        .bind(status_name(status))
        .bind(id.get())
        .bind(millis(now))
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
        let mut tx = self.begin_write().await?;
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
        let destination = load_destination(&mut *tx, message.conversation.get())
            .await?
            .ok_or_else(|| database_error(sqlx::Error::RowNotFound))?;
        tx.commit().await.map_err(database_error)?;
        Ok(Some((destination, message)))
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
        let mut tx = self.begin_write().await?;
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
        let mut members = self.all_members().await?;
        rows.into_iter()
            .map(|row| {
                let members = members.remove(&row.id).unwrap_or_default();
                summary(row, members)
            })
            .collect()
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
            Some(row) => {
                let members = self.members(row.id).await?;
                Ok(Some(summary(row, members)?))
            }
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

    /// Marks incoming messages read at `now`, starting the countdown of
    /// disappearing ones; returns the newly read ones.
    pub(crate) async fn mark_read(
        &self,
        conversation: ConversationId,
        now: Timestamp,
    ) -> Result<Vec<Message>, StorageError> {
        let rows: Vec<MessageRow> = sqlx::query_as(concat!(
            "UPDATE messages SET read = 1, expires_at = ?2 + expires_in * 1000
             WHERE conversation_id = ?1 AND read = 0
             RETURNING ",
            message_columns!()
        ))
        .bind(conversation.get())
        .bind(millis(now))
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        let mut messages = rows
            .into_iter()
            .map(message)
            .collect::<Result<Vec<_>, _>>()?;
        messages.sort_by_key(|message| (message.sent_at, message.id));
        Ok(messages)
    }

    /// Deletes messages whose countdown ended by `now`; returns them by
    /// conversation.
    pub(crate) async fn expire(
        &self,
        now: Timestamp,
    ) -> Result<Vec<(ConversationId, MessageId)>, StorageError> {
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "DELETE FROM messages WHERE expires_at <= ? RETURNING conversation_id, id",
        )
        .bind(millis(now))
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        let mut expired: Vec<_> = rows
            .into_iter()
            .map(|(conversation, id)| (ConversationId::new(conversation), MessageId::new(id)))
            .collect();
        expired.sort();
        Ok(expired)
    }

    /// When the next disappearing message is due.
    pub(crate) async fn next_expiry(&self) -> Result<Option<Timestamp>, StorageError> {
        let at: Option<i64> =
            sqlx::query_scalar("SELECT min(expires_at) FROM messages WHERE expires_at IS NOT NULL")
                .fetch_one(&self.app)
                .await
                .map_err(database_error)?;
        Ok(at.map(timestamp))
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

    /// Every group's members, by conversation.
    async fn all_members(&self) -> Result<HashMap<i64, Vec<Member>>, StorageError> {
        let rows: Vec<(i64, Vec<u8>, Option<String>)> = sqlx::query_as(
            "SELECT conversation_id, member, name FROM group_members
             ORDER BY conversation_id, position",
        )
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        let mut members: HashMap<i64, Vec<Member>> = HashMap::new();
        for (conversation, member, name) in rows {
            members.entry(conversation).or_default().push(Member {
                id: service_id(&member)?,
                name,
            });
        }
        Ok(members)
    }
}

/// A conversation's summary from its row and, for a group, its members.
fn summary(row: SummaryRow, members: Vec<Member>) -> Result<ConversationSummary, StorageError> {
    let last_message = match row.outgoing.zip(row.body) {
        Some((outgoing, body)) => Some(MessagePreview {
            direction: direction(outgoing),
            content: content_from(body, row.content)?,
        }),
        None => None,
    };
    let (peer, title, members) = match (row.peer, row.group_id) {
        (Some(peer), _) => {
            let peer = service_id(&peer)?;
            let title = display_name(row.contact_name, row.given_name, row.family_name);
            (Peer::Contact(peer), title, Vec::new())
        }
        (None, Some(group_id)) => {
            let bytes = <[u8; 32]>::try_from(group_id.as_slice())
                .map_err(|_| StorageError::Database("invalid group ID".into()))?;
            let title = row.group_title.filter(|title| !title.trim().is_empty());
            (Peer::Group(GroupId::new(bytes)), title, members)
        }
        (None, None) => return Err(StorageError::Database("conversation without peer".into())),
    };
    let state = match row.state.as_str() {
        "blocked" => ChatState::Blocked,
        "request" => ChatState::Request,
        _ => ChatState::Accepted,
    };
    Ok(ConversationSummary {
        id: ConversationId::new(row.id),
        peer,
        state,
        title,
        members,
        last_activity: row.last_activity.map(timestamp),
        unread: u32::try_from(row.unread).unwrap_or(u32::MAX),
        last_message,
    })
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
pub(super) async fn conversation_id(
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

/// Inserts a message into `conversation`, stored at `received_at`, unless
/// it's a duplicate, and bumps the conversation's activity.
#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_message(
    tx: &mut sqlx::SqliteConnection,
    conversation: i64,
    author: Option<ServiceId>,
    status: Option<MessageStatus>,
    sent_at: Timestamp,
    content: &Content,
    expiry: Option<Expiry>,
    received_at: Timestamp,
) -> Result<Option<Message>, StorageError> {
    let outgoing = author.is_none();
    let read = outgoing || content.is_quiet();
    let author_bytes = author.map(|author| author.uuid().as_bytes().to_vec());
    let (body, json) = content_columns(content)?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO messages
             (conversation_id, outgoing, author, sent_at, received_at, body, content, read, status,
              expires_in, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
    .bind(expiry.map(|expiry| i64::from(expiry.timer)))
    .bind(expiry.and_then(|expiry| expiry.at).map(millis))
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
        expiry,
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

/// Where messages in conversation `id` go; `None` if there's no such
/// conversation.
async fn load_destination(
    executor: impl sqlx::SqliteExecutor<'_>,
    id: i64,
) -> Result<Option<Destination>, StorageError> {
    let row: Option<DestinationRow> = sqlx::query_as(
        "SELECT peer, group_master_key, group_revision FROM conversations WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(executor)
    .await
    .map_err(database_error)?;
    row.map(destination).transpose()
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

pub(super) fn message(row: MessageRow) -> Result<Message, StorageError> {
    let status = match (row.status.as_deref(), row.delivery.as_deref()) {
        (Some("sent"), Some("delivered")) => Some(MessageStatus::Delivered),
        (Some("sent"), Some("read")) => Some(MessageStatus::Read),
        (status, _) => status.map(parse_status).transpose()?,
    };
    let sender = if row.author.is_empty() {
        None
    } else {
        Some(service_id(&row.author)?)
    };
    Ok(Message {
        id: MessageId::new(row.id),
        conversation: ConversationId::new(row.conversation_id),
        direction: direction(row.outgoing),
        sender,
        sent_at: timestamp(row.sent_at),
        received_at: timestamp(row.received_at),
        content: content_from(row.body, row.content)?,
        edited: row.edited_at.is_some(),
        read: row.read,
        status,
        expiry: row.expires_in.map(|timer| Expiry {
            timer: u32::try_from(timer).unwrap_or(u32::MAX),
            at: row.expires_at.map(timestamp),
        }),
    })
}

pub(super) fn service_id(peer: &[u8]) -> Result<ServiceId, StorageError> {
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

/// When a countdown of `timer` seconds from `start` ends.
fn expires_at(start: Timestamp, timer: u32) -> Timestamp {
    Timestamp::from_millis(start.as_millis() + u64::from(timer) * 1000)
}

pub(super) fn timestamp(millis: i64) -> Timestamp {
    Timestamp::from_millis(u64::try_from(millis).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attachment, GroupUpdate, Unsupported};
    use crate::storage::DatabaseKey;
    use crate::storage::testing::{at, db, peer};

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
            None,
            Timestamp::from_millis(sent_at),
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
            None,
            Timestamp::from_millis(0),
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
            None,
            Timestamp::from_millis(sent_at),
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
            .set_status(message.id, MessageStatus::Sent, Timestamp::from_millis(0))
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
            .set_status(message.id, MessageStatus::Sent, Timestamp::from_millis(0))
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
        db.set_status(message.id, MessageStatus::Failed, Timestamp::from_millis(0))
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
        db.set_status(second.id, MessageStatus::Sent, Timestamp::from_millis(0))
            .await
            .unwrap();

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

        assert_eq!(
            db.mark_read(message.conversation, Timestamp::from_millis(0))
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(
            db.mark_read(message.conversation, Timestamp::from_millis(0))
                .await
                .unwrap()
                .is_empty()
        );

        let summary = db
            .conversation(message.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.unread, 0);
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
    async fn conversations_list_each_groups_members() {
        let (_dir, db) = db().await;
        let member = |n, name: Option<&str>| Member {
            id: peer(n),
            name: name.map(Into::into),
        };
        let groups = [
            vec![member(3, Some("Carol")), member(1, None)],
            vec![member(2, None)],
        ];
        for (n, members) in (1u8..).zip(&groups) {
            add_to_group(&db, group(n), Some(1), 100).await.unwrap();
            let info = GroupInfo {
                title: format!("Group {n}"),
                revision: 1,
                members: members.clone(),
            };
            db.set_group(group(n).key, &info).await.unwrap();
        }
        add(&db, 1, Direction::Incoming, 100, "hi").await;

        let summaries = db.conversations().await.unwrap();

        let mut listed: Vec<_> = summaries
            .iter()
            .map(|summary| (summary.title.clone(), summary.members.clone()))
            .collect();
        listed.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            listed,
            [
                (None, Vec::new()),
                (Some("Group 1".into()), groups[0].clone()),
                (Some("Group 2".into()), groups[1].clone()),
            ]
        );
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
                None,
                Timestamp::from_millis(1),
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
            None,
            Timestamp::from_millis(0),
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
                None,
                Timestamp::from_millis(0),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mine.status, None, "system lines have no send status");
        db.close().await;
    }

    async fn timed(
        db: &Database,
        author: Option<u128>,
        sent_at: u64,
        content: Content,
        now: u64,
    ) -> Message {
        db.add_message(
            &Destination::Contact(peer(1)),
            author.map(peer),
            at(sent_at),
            &content,
            Some(60),
            at(now),
        )
        .await
        .unwrap()
        .unwrap()
    }

    fn expiry(message: &Message) -> Option<Option<u64>> {
        message
            .expiry
            .map(|expiry| expiry.at.map(|at| at.as_millis()))
    }

    #[tokio::test]
    async fn countdowns_start_on_reading_sending_or_arrival() {
        let (_dir, db) = db().await;
        let unread = timed(&db, Some(1), 100, Content::text("in"), 1_000).await;
        let reaction = Content::Reaction {
            emoji: "👍".into()
        };
        let quiet = timed(&db, Some(1), 101, reaction, 2_000).await;
        let from_phone = timed(&db, None, 102, Content::text("out"), 3_000).await;
        let system = timed(&db, Some(1), 103, Content::TimerChanged(Some(60)), 4_000).await;

        assert_eq!(unread.expiry.map(|expiry| expiry.timer), Some(60));
        assert_eq!(expiry(&unread), Some(None));
        assert_eq!(expiry(&quiet), Some(Some(62_000)));
        assert_eq!(expiry(&from_phone), Some(Some(60_102)));
        assert_eq!(expiry(&system), None, "system lines stay");

        let read = db.mark_read(unread.conversation, at(5_000)).await.unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(expiry(&read[0]), Some(Some(65_000)));
        assert_eq!(db.message(unread.id).await.unwrap(), Some(read[0].clone()));

        let conversation = unread.conversation;
        let pending = db
            .add_pending(
                conversation,
                at(200),
                &Content::text("out"),
                Some(30),
                at(200),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(expiry(&pending), Some(None));
        let sent = db
            .set_status(pending.id, MessageStatus::Sent, at(7_000))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(expiry(&sent), Some(Some(37_000)));
        db.close().await;
    }

    #[tokio::test]
    async fn failed_sends_start_no_countdown() {
        let (_dir, db) = db().await;
        let conversation = conversation_with(&db, Destination::Contact(peer(1))).await;
        let pending = db
            .add_pending(
                conversation,
                at(200),
                &Content::text("out"),
                Some(30),
                at(200),
            )
            .await
            .unwrap()
            .unwrap();
        let failed = db
            .set_status(pending.id, MessageStatus::Failed, at(7_000))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(expiry(&failed), Some(None));
        db.close().await;
    }

    #[tokio::test]
    async fn due_messages_are_deleted() {
        let (_dir, db) = db().await;
        let first = timed(&db, None, 1_000, Content::text("a"), 0).await;
        let second = timed(&db, None, 2_000, Content::text("b"), 0).await;
        let kept = add(&db, 1, Direction::Outgoing, 3_000, "kept")
            .await
            .unwrap();
        assert_eq!(db.next_expiry().await.unwrap(), Some(at(61_000)));

        assert!(db.expire(at(60_999)).await.unwrap().is_empty());
        assert_eq!(
            db.expire(at(61_000)).await.unwrap(),
            [(first.conversation, first.id)]
        );
        assert_eq!(db.message(first.id).await.unwrap(), None);
        assert_eq!(db.next_expiry().await.unwrap(), Some(at(62_000)));
        assert_eq!(
            db.expire(at(100_000)).await.unwrap(),
            [(second.conversation, second.id)]
        );
        assert_eq!(db.next_expiry().await.unwrap(), None);
        assert!(db.message(kept.id).await.unwrap().is_some());
        db.close().await;
    }

    #[tokio::test]
    async fn deletes_overwrite_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let db = Database::open(&path, &DatabaseKey::generate())
            .await
            .unwrap();
        let secure: i64 = sqlx::query_scalar("PRAGMA secure_delete")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(secure, 1);
        db.close().await;
    }

    #[test]
    fn summary_debug_hides_the_title() {
        let summary = ConversationSummary {
            id: ConversationId::new(1),
            peer: Peer::Contact(peer(1)),
            state: ChatState::Accepted,
            title: Some("Alice Secret".into()),
            members: Vec::new(),
            last_activity: None,
            unread: 0,
            last_message: None,
        };
        assert!(!format!("{summary:?}").contains("Alice"));
    }
}
