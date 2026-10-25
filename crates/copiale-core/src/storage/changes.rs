//! Edits and deletes of earlier messages, including ones that arrive first.

use std::cmp::Reverse;
use std::time::Duration;

use super::StorageError;
use super::attachments::keep_attachments;
use super::database::{Database, database_error};
use super::messages::{
    Removed, RemovedRow, author_bytes, content_columns, content_from, load_message, millis,
    removed, removed_columns, timestamp,
};
use super::retries::placeholder_json;
use crate::model::{
    Change, ChatRef, Content, ConversationId, Destination, Message, MessageChange, MessageId,
    MessageRef, Timestamp,
};

/// How long after the server got a message others may edit it, as in
/// Signal; from the original message, not the last edit.
const EDIT_WINDOW: Duration = Duration::from_secs(48 * 3600);
/// How long after the server got a message others may delete it, as in
/// Signal: a day plus a day's grace.
const DELETE_WINDOW: Duration = Duration::from_secs(48 * 3600);
/// Most edits a message takes, as in Signal.
const MAX_EDITS: i64 = 100;
/// Most changes kept per author for messages not stored yet; older ones
/// are dropped first.
const MAX_PENDING_PER_AUTHOR: i64 = 100;

impl Database {
    /// Applies an edit or delete by the message's author; returns the
    /// changed message. One for a message not stored yet is kept until it
    /// is, from `now`, up to [`MAX_PENDING_PER_AUTHOR`]; `None` then, and
    /// when there's nothing to change. Others' changes apply only within [`EDIT_WINDOW`] or
    /// [`DELETE_WINDOW`] of the message, and edits up to [`MAX_EDITS`].
    pub(crate) async fn apply_change(
        &self,
        change: &MessageChange,
        now: Timestamp,
    ) -> Result<Option<Message>, StorageError> {
        let mut tx = self.begin_write().await?;
        let author = author_bytes(change.author);
        let conversation: Option<i64> = match change.destination {
            Destination::Contact(peer) => {
                sqlx::query_scalar("SELECT id FROM conversations WHERE peer = ?")
                    .bind(peer.uuid().as_bytes().as_slice())
                    .fetch_optional(&mut *tx)
                    .await
            }
            Destination::Group(group) => {
                sqlx::query_scalar("SELECT id FROM conversations WHERE group_master_key = ?")
                    .bind(group.key.as_bytes().as_slice())
                    .fetch_optional(&mut *tx)
                    .await
            }
        }
        .map_err(database_error)?;
        // Later edits may target the previous edit's time. Changes to a
        // message that couldn't be decrypted wait for it.
        let target: Option<i64> = match conversation {
            Some(conversation) => sqlx::query_scalar(
                "SELECT id FROM messages
                 WHERE conversation_id = ?1 AND author = ?2 AND (sent_at = ?3 OR edited_at = ?3)
                   AND coalesce(content, '') != ?4
                 ORDER BY id LIMIT 1",
            )
            .bind(conversation)
            .bind(&author)
            .bind(millis(change.target))
            .bind(placeholder_json()?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?,
            None => None,
        };
        let Some(id) = target else {
            let (content, edited_at) = match &change.change {
                Change::Edit { content, at } => (Some(json(content)?), Some(millis(*at))),
                Change::Delete => (None, None),
            };
            sqlx::query(
                "INSERT INTO pending_changes
                     (author, target, content, edited_at, server_at, received_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&author)
            .bind(millis(change.target))
            .bind(content)
            .bind(edited_at)
            .bind(millis(change.server_at))
            .bind(millis(now))
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
            sqlx::query(
                "DELETE FROM pending_changes WHERE author = ?1 AND id NOT IN (
                     SELECT id FROM pending_changes WHERE author = ?1
                     ORDER BY id DESC LIMIT ?2
                 )",
            )
            .bind(&author)
            .bind(MAX_PENDING_PER_AUTHOR)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
            tx.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let server_at = change.author.map(|_| change.server_at);
        let changed = update(&mut tx, MessageId::new(id), &change.change, server_at).await?;
        let message = match changed {
            true => load_message(&mut tx, MessageId::new(id)).await?,
            false => None,
        };
        tx.commit().await.map_err(database_error)?;
        if changed && change.change == Change::Delete {
            self.remove_deleted_files().await;
        }
        Ok(message)
    }

    /// Deletes messages for good; returns the ones that were stored.
    pub(crate) async fn delete_messages(
        &self,
        messages: &[MessageRef],
    ) -> Result<Vec<Removed>, StorageError> {
        let mut deleted = Vec::new();
        for message in messages {
            let Some(conversation) = self.chat_conversation(message.chat).await? else {
                continue;
            };
            let row: Option<RemovedRow> = sqlx::query_as(concat!(
                "DELETE FROM messages WHERE conversation_id = ? AND author = ? AND sent_at = ?
                 RETURNING ",
                removed_columns!()
            ))
            .bind(conversation.get())
            .bind(author_bytes(message.author))
            .bind(millis(message.sent_at))
            .fetch_optional(&self.app)
            .await
            .map_err(database_error)?;
            deleted.extend(row.map(removed).transpose()?);
        }
        self.remove_deleted_files().await;
        Ok(deleted)
    }

    /// Deletes a conversation and its messages; returns it if it existed.
    pub(crate) async fn delete_conversation(
        &self,
        chat: ChatRef,
    ) -> Result<Option<ConversationId>, StorageError> {
        let Some(conversation) = self.chat_conversation(chat).await? else {
            return Ok(None);
        };
        sqlx::query("DELETE FROM conversations WHERE id = ?")
            .bind(conversation.get())
            .execute(&self.app)
            .await
            .map_err(database_error)?;
        self.remove_deleted_files().await;
        Ok(Some(conversation))
    }

    /// The conversation `chat` names, if any.
    pub(crate) async fn chat_conversation(
        &self,
        chat: ChatRef,
    ) -> Result<Option<ConversationId>, StorageError> {
        let id: Option<i64> = match chat {
            ChatRef::Contact(peer) => {
                sqlx::query_scalar("SELECT id FROM conversations WHERE peer = ?")
                    .bind(peer.uuid().as_bytes().as_slice())
                    .fetch_optional(&self.app)
                    .await
            }
            ChatRef::Group(group) => {
                sqlx::query_scalar("SELECT id FROM conversations WHERE group_id = ?")
                    .bind(group.as_bytes().as_slice())
                    .fetch_optional(&self.app)
                    .await
            }
        }
        .map_err(database_error)?;
        Ok(id.map(ConversationId::new))
    }

    /// Drops kept changes that arrived before `before`; returns how many.
    pub(crate) async fn expire_pending_changes(
        &self,
        before: Timestamp,
    ) -> Result<u64, StorageError> {
        let result = sqlx::query("DELETE FROM pending_changes WHERE received_at < ?")
            .bind(millis(before))
            .execute(&self.app)
            .await
            .map_err(database_error)?;
        Ok(result.rows_affected())
    }
}

/// Applies changes kept for a message just stored: a delete wins, else the
/// latest edit, of those that apply. Returns the message as it now is.
pub(super) async fn apply_pending(
    tx: &mut sqlx::SqliteConnection,
    message: Message,
) -> Result<Message, StorageError> {
    let author = author_bytes(message.sender);
    let target = millis(message.sent_at);
    let mut pending: Vec<(Option<String>, Option<i64>, Option<i64>)> = sqlx::query_as(
        "DELETE FROM pending_changes WHERE author = ? AND target = ?
         RETURNING content, edited_at, server_at",
    )
    .bind(&author)
    .bind(target)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    // Deletes first, then edits from the latest.
    pending.sort_by_key(|(content, edited_at, _)| (content.is_some(), Reverse(*edited_at)));
    for (content, edited_at, server_at) in pending {
        let change = match (content, edited_at) {
            (None, _) => Change::Delete,
            (Some(content), Some(at)) => Change::Edit {
                content: serde_json::from_str(&content)
                    .map_err(|_| StorageError::Database("invalid kept edit".into()))?,
                at: timestamp(at),
            },
            (Some(_), None) => continue,
        };
        let server_at = match (message.sender, server_at) {
            (None, _) => None,
            (Some(_), Some(at)) => Some(timestamp(at)),
            // Others' are always kept with one.
            (Some(_), None) => continue,
        };
        if update(tx, message.id, &change, server_at).await? {
            return Ok(load_message(tx, message.id).await?.unwrap_or(message));
        }
    }
    Ok(message)
}

/// Applies `change` to a stored message; `false` if it doesn't apply: an
/// edit to or of anything but text, e.g. a deleted message or a system
/// line, an older edit than the stored one, or one past [`MAX_EDITS`].
/// With `server_at`, when the server got someone else's change, it must
/// also be within its window of the message's.
async fn update(
    tx: &mut sqlx::SqliteConnection,
    id: MessageId,
    change: &Change,
    server_at: Option<Timestamp>,
) -> Result<bool, StorageError> {
    let deleted = json(&Content::Deleted)?;
    let window = |window: Duration| i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
    let result = match change {
        Change::Delete => {
            let result = sqlx::query(
                "UPDATE messages SET body = '', content = ?1
                 WHERE id = ?2 AND coalesce(content, '') != ?1
                   AND (?3 IS NULL OR ?3 <= server_at + ?4)",
            )
            .bind(&deleted)
            .bind(id.get())
            .bind(server_at.map(millis))
            .bind(window(DELETE_WINDOW))
            .execute(&mut *tx)
            .await;
            if result
                .as_ref()
                .is_ok_and(|result| result.rows_affected() > 0)
            {
                sqlx::query("DELETE FROM attachments WHERE message_id = ?")
                    .bind(id.get())
                    .execute(&mut *tx)
                    .await
                    .map_err(database_error)?;
            }
            result
        }
        Change::Edit { content, .. } if !matches!(content, Content::Text { .. }) => {
            return Ok(false);
        }
        Change::Edit { content, at } => {
            let stored: Option<(String, Option<String>)> =
                sqlx::query_as("SELECT body, content FROM messages WHERE id = ?")
                    .bind(id.get())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(database_error)?;
            let stored = stored
                .map(|(body, json)| content_from(body, json))
                .transpose()?;
            let mut content = content.clone();
            keep_attachments(&mut content, stored.as_ref());
            let (body, content) = content_columns(&content)?;
            // Only text is edited: plain, or kept as JSON.
            sqlx::query(
                "UPDATE messages SET body = ?1, content = ?2, edited_at = ?3, edits = edits + 1
                 WHERE id = ?4 AND (content IS NULL OR json_type(content, '$.text') IS NOT NULL)
                   AND (edited_at IS NULL OR edited_at < ?3) AND edits < ?5
                   AND (?6 IS NULL OR ?6 <= server_at + ?7)",
            )
            .bind(body)
            .bind(content)
            .bind(millis(*at))
            .bind(id.get())
            .bind(MAX_EDITS)
            .bind(server_at.map(millis))
            .bind(window(EDIT_WINDOW))
            .execute(&mut *tx)
            .await
        }
    }
    .map_err(database_error)?;
    Ok(result.rows_affected() > 0)
}

fn json(content: &Content) -> Result<String, StorageError> {
    serde_json::to_string(content).map_err(|error| StorageError::Database(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupMasterKey, GroupRef, GroupUpdate, MessageBody, Quote, Quoted};
    use crate::storage::testing::{at, db, peer, removed_of};

    async fn incoming(db: &Database, from: u128, sent_at: u64, body: &str) -> Option<Message> {
        db.add_message(
            &Destination::Contact(peer(from)),
            Some(peer(from)),
            at(sent_at),
            &Content::text(body),
            None,
            Some(at(sent_at)),
            Timestamp::from_millis(0),
        )
        .await
        .unwrap()
    }

    fn change(from: u128, target: u64, change: Change) -> MessageChange {
        MessageChange {
            destination: Destination::Contact(peer(from)),
            author: Some(peer(from)),
            target: at(target),
            server_at: at(target),
            change,
        }
    }

    fn edit(body: &str, edited_at: u64) -> Change {
        Change::Edit {
            content: Content::text(body),
            at: at(edited_at),
        }
    }

    #[tokio::test]
    async fn edits_replace_the_text_and_mark_it_edited() {
        let (_dir, db) = db().await;
        let original = incoming(&db, 1, 100, "teh").await.unwrap();

        let edited = db
            .apply_change(&change(1, 100, edit("the", 200)), at(0))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(edited.id, original.id);
        assert_eq!(edited.content, Content::text("the"));
        assert!(edited.edited());
        assert_eq!(edited.sent_at, original.sent_at);
        // A second edit may target the first edit's time.
        let again = db
            .apply_change(&change(1, 200, edit("the end", 300)), at(0))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.content, Content::text("the end"));
        // An older edit arriving late changes nothing.
        let stale = db
            .apply_change(&change(1, 100, edit("teh", 250)), at(0))
            .await
            .unwrap();
        assert_eq!(stale, None);
        let stored = db.messages(original.conversation, None, 10).await.unwrap();
        assert_eq!(stored, [again]);
        db.close().await;
    }

    #[tokio::test]
    async fn only_text_is_edited_and_only_into_text() {
        let (_dir, db) = db().await;
        let line = db
            .add_message(
                &Destination::Contact(peer(1)),
                Some(peer(1)),
                at(100),
                &Content::TimerChanged(Some(60)),
                None,
                Some(at(100)),
                Timestamp::from_millis(0),
            )
            .await
            .unwrap()
            .unwrap();
        let text = incoming(&db, 1, 101, "hi").await.unwrap();

        let into_text = db
            .apply_change(&change(1, 100, edit("removed you", 200)), at(0))
            .await
            .unwrap();
        assert_eq!(into_text, None);
        let into_line = Change::Edit {
            content: Content::GroupChanged(vec![GroupUpdate::Left]),
            at: at(200),
        };
        let line_edit = db
            .apply_change(&change(1, 101, into_line), at(0))
            .await
            .unwrap();
        assert_eq!(line_edit, None);

        let stored = db.messages(text.conversation, None, 10).await.unwrap();
        assert_eq!(stored, [line, text]);
        db.close().await;
    }

    #[tokio::test]
    async fn only_the_author_changes_a_message() {
        let (_dir, db) = db().await;
        let original = incoming(&db, 1, 100, "mine").await.unwrap();

        let forged = MessageChange {
            author: Some(peer(2)),
            ..change(1, 100, Change::Delete)
        };
        assert_eq!(db.apply_change(&forged, at(0)).await.unwrap(), None);

        let stored = db.messages(original.conversation, None, 10).await.unwrap();
        assert_eq!(stored[0].content, Content::text("mine"));
        db.close().await;
    }

    #[tokio::test]
    async fn deletes_replace_the_message_for_good() {
        let (_dir, db) = db().await;
        incoming(&db, 1, 100, "oops").await.unwrap();

        let deleted = db
            .apply_change(&change(1, 100, Change::Delete), at(0))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(deleted.content, Content::Deleted);
        assert_eq!(
            db.apply_change(&change(1, 100, Change::Delete), at(0))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            db.apply_change(&change(1, 100, edit("back", 200)), at(0))
                .await
                .unwrap(),
            None
        );
        let summary = db
            .conversation(deleted.conversation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            summary.last_message.map(|preview| preview.content),
            Some(Content::Deleted)
        );
        db.close().await;
    }

    #[tokio::test]
    async fn changes_arriving_first_apply_when_the_message_does() {
        let (_dir, db) = db().await;
        assert_eq!(
            db.apply_change(&change(1, 100, edit("v2", 200)), at(0))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            db.apply_change(&change(1, 100, edit("v3", 300)), at(0))
                .await
                .unwrap(),
            None
        );

        let message = incoming(&db, 1, 100, "v1").await.unwrap();

        assert_eq!(message.content, Content::text("v3"));
        assert!(message.edited());

        db.apply_change(&change(2, 50, edit("x", 60)), at(0))
            .await
            .unwrap();
        db.apply_change(&change(2, 50, Change::Delete), at(0))
            .await
            .unwrap();
        let message = incoming(&db, 2, 50, "secret").await.unwrap();
        assert_eq!(message.content, Content::Deleted, "a delete wins");
        db.close().await;
    }

    #[tokio::test]
    async fn own_messages_and_groups_can_change() {
        let (_dir, db) = db().await;
        let group = Destination::Group(GroupRef {
            key: GroupMasterKey::new([1; 32]),
            revision: 0,
        });
        let mine = db
            .add_message(
                &group,
                None,
                at(100),
                &Content::text("hi all"),
                None,
                None,
                Timestamp::from_millis(0),
            )
            .await
            .unwrap()
            .unwrap();

        let edited = db
            .apply_change(
                &MessageChange {
                    destination: group,
                    author: None,
                    target: at(100),
                    server_at: at(100),
                    change: edit("hi everyone", 200),
                },
                at(0),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(edited.id, mine.id);
        assert_eq!(edited.status, mine.status);
        db.close().await;
    }

    /// A reply from `from` at `sent_at` quoting `author`'s message at
    /// `quoted`, claiming it said `claimed`.
    async fn reply(
        db: &Database,
        from: u128,
        sent_at: u64,
        author: Option<u128>,
        quoted: u64,
        claimed: &str,
    ) -> Message {
        let content = Content::Text {
            body: MessageBody::new("agreed"),
            attachments: Vec::new(),
            quote: Some(Quote {
                author: author.map(peer),
                sent_at: at(quoted),
                original: Some(Quoted {
                    text: claimed.into(),
                    attachment: None,
                }),
            }),
        };
        db.add_message(
            &Destination::Contact(peer(from)),
            Some(peer(from)),
            at(sent_at),
            &content,
            None,
            Some(at(sent_at)),
            at(0),
        )
        .await
        .unwrap()
        .unwrap()
    }

    /// What `message` quotes, as shown: `None` if not found.
    fn quoted(message: &Message) -> Option<String> {
        match &message.content {
            Content::Text {
                quote: Some(quote), ..
            } => quote
                .original
                .as_ref()
                .map(|original| original.text.clone()),
            _ => panic!("expected a reply"),
        }
    }

    #[tokio::test]
    async fn quotes_show_the_stored_original() {
        let (_dir, db) = db().await;
        let original = incoming(&db, 1, 50, "ship it?").await.unwrap();
        db.add_message(
            &Destination::Contact(peer(1)),
            None,
            at(70),
            &Content::text("mine"),
            None,
            None,
            at(0),
        )
        .await
        .unwrap();
        incoming(&db, 2, 50, "elsewhere").await.unwrap();

        let theirs = reply(&db, 1, 100, Some(1), 50, "send money").await;
        let wrong_author = reply(&db, 1, 101, Some(2), 50, "send money").await;
        let missing = reply(&db, 1, 102, Some(1), 60, "send money").await;
        let ours = reply(&db, 1, 103, None, 70, "send money").await;
        let other_chat = reply(&db, 2, 104, Some(1), 50, "send money").await;

        assert_eq!(quoted(&theirs).as_deref(), Some("ship it?"));
        assert_eq!(quoted(&wrong_author), None);
        assert_eq!(quoted(&missing), None);
        assert_eq!(quoted(&ours).as_deref(), Some("mine"));
        assert_eq!(quoted(&other_chat), None, "only quotes its own chat");
        let stored = db.messages(original.conversation, None, 10).await.unwrap();
        assert_eq!(stored.iter().find(|m| m.id == theirs.id), Some(&theirs));
        let json: String = sqlx::query_scalar("SELECT content FROM messages WHERE id = ?")
            .bind(theirs.id.get())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert!(!json.contains("send money") && !json.contains("ship it?"));
        db.close().await;
    }

    #[tokio::test]
    async fn replies_follow_their_original() {
        let (_dir, db) = db().await;
        let conversation = incoming(&db, 1, 50, "teh").await.unwrap().conversation;
        incoming(&db, 1, 51, "oops").await.unwrap();
        let removed = incoming(&db, 1, 52, "bye").await.unwrap();
        let to_edited = reply(&db, 1, 100, Some(1), 50, "").await;
        reply(&db, 1, 101, Some(1), 51, "").await;
        reply(&db, 1, 102, Some(1), 52, "").await;
        let replies = |sent_at| db.replies(conversation, Some(peer(1)), at(sent_at));

        db.apply_change(&change(1, 50, edit("the", 60)), at(0))
            .await
            .unwrap();
        db.apply_change(&change(1, 51, Change::Delete), at(0))
            .await
            .unwrap();
        db.delete_messages(&[MessageRef {
            chat: ChatRef::Contact(peer(1)),
            author: Some(peer(1)),
            sent_at: removed.sent_at,
        }])
        .await
        .unwrap();

        let to_edited_now = replies(50).await.unwrap();
        assert_eq!(to_edited_now.len(), 1);
        assert_eq!(to_edited_now[0].id, to_edited.id);
        assert_eq!(quoted(&to_edited_now[0]).as_deref(), Some("the"));
        assert_eq!(quoted(&replies(51).await.unwrap()[0]), None);
        assert_eq!(quoted(&replies(52).await.unwrap()[0]), None);
        assert_eq!(db.replies(conversation, None, at(50)).await.unwrap(), []);
        db.close().await;
    }

    #[tokio::test]
    async fn deleting_for_me_removes_messages_and_conversations() {
        let (_dir, db) = db().await;
        let first = incoming(&db, 1, 100, "a").await.unwrap();
        incoming(&db, 1, 101, "b").await.unwrap();
        let mine = db
            .add_message(
                &Destination::Contact(peer(1)),
                None,
                at(102),
                &Content::text("c"),
                None,
                None,
                Timestamp::from_millis(0),
            )
            .await
            .unwrap()
            .unwrap();
        let chat = ChatRef::Contact(peer(1));

        let deleted = db
            .delete_messages(&[
                MessageRef {
                    chat,
                    author: Some(peer(1)),
                    sent_at: at(100),
                },
                MessageRef {
                    chat,
                    author: None,
                    sent_at: at(102),
                },
                MessageRef {
                    chat: ChatRef::Contact(peer(9)),
                    author: Some(peer(9)),
                    sent_at: at(100),
                },
            ])
            .await
            .unwrap();

        assert_eq!(deleted, [removed_of(&first), removed_of(&mine)]);
        let left = db.messages(first.conversation, None, 10).await.unwrap();
        assert_eq!(left.len(), 1);

        assert_eq!(
            db.delete_conversation(chat).await.unwrap(),
            Some(first.conversation)
        );
        assert_eq!(db.conversation(first.conversation).await.unwrap(), None);
        assert_eq!(db.delete_conversation(chat).await.unwrap(), None);
        db.close().await;
    }

    #[tokio::test]
    async fn kept_changes_expire() {
        let (_dir, db) = db().await;
        db.apply_change(&change(1, 100, Change::Delete), at(10))
            .await
            .unwrap();

        assert_eq!(db.expire_pending_changes(at(10)).await.unwrap(), 0);
        assert_eq!(db.expire_pending_changes(at(11)).await.unwrap(), 1);

        let message = incoming(&db, 1, 100, "kept").await.unwrap();
        assert_eq!(message.content, Content::text("kept"));
        db.close().await;
    }

    #[tokio::test]
    async fn kept_changes_are_capped_per_author() {
        let (_dir, db) = db().await;
        let cap = MAX_PENDING_PER_AUTHOR as u64;
        for target in 0..=cap {
            db.apply_change(&change(1, 100 + target, Change::Delete), at(0))
                .await
                .unwrap();
        }
        db.apply_change(&change(2, 100, Change::Delete), at(0))
            .await
            .unwrap();

        // The oldest of 1's was dropped; 2's is kept.
        let oldest = incoming(&db, 1, 100, "kept").await.unwrap();
        assert_eq!(oldest.content, Content::text("kept"));
        let newest = incoming(&db, 1, 100 + cap, "gone").await.unwrap();
        assert_eq!(newest.content, Content::Deleted);
        let other = incoming(&db, 2, 100, "gone").await.unwrap();
        assert_eq!(other.content, Content::Deleted);
        db.close().await;
    }

    /// `change`, as the server got it at `server_at`.
    fn got_at(server_at: u64, change: MessageChange) -> MessageChange {
        MessageChange {
            server_at: at(server_at),
            ..change
        }
    }

    const WINDOW: u64 = 48 * 3600 * 1000;

    #[tokio::test]
    async fn others_change_messages_only_within_the_window() {
        let (_dir, db) = db().await;
        incoming(&db, 1, 100, "v1").await.unwrap();
        incoming(&db, 1, 101, "old").await.unwrap();
        incoming(&db, 1, 102, "gone").await.unwrap();

        let last = got_at(100 + WINDOW, change(1, 100, edit("v2", 200)));
        assert!(db.apply_change(&last, at(0)).await.unwrap().is_some());
        // From the original, not the last edit.
        let past = got_at(101 + WINDOW, change(1, 200, edit("v3", 300)));
        assert_eq!(db.apply_change(&past, at(0)).await.unwrap(), None);
        let late_edit = got_at(102 + WINDOW, change(1, 101, edit("new", 300)));
        assert_eq!(db.apply_change(&late_edit, at(0)).await.unwrap(), None);
        let late_delete = got_at(102 + WINDOW, change(1, 101, Change::Delete));
        assert_eq!(db.apply_change(&late_delete, at(0)).await.unwrap(), None);
        let delete = got_at(102 + WINDOW, change(1, 102, Change::Delete));
        assert!(db.apply_change(&delete, at(0)).await.unwrap().is_some());

        let stored = db.messages(ConversationId::new(1), None, 10).await.unwrap();
        let contents: Vec<_> = stored.into_iter().map(|m| m.content).collect();
        assert_eq!(
            contents,
            [Content::text("v2"), Content::text("old"), Content::Deleted]
        );
        db.close().await;
    }

    #[tokio::test]
    async fn own_changes_have_no_window() {
        let (_dir, db) = db().await;
        let destination = Destination::Contact(peer(1));
        for sent_at in [100, 101] {
            db.add_message(
                &destination,
                None,
                at(sent_at),
                &Content::text("mine"),
                None,
                None,
                at(0),
            )
            .await
            .unwrap();
        }
        let own = |target, change| MessageChange {
            destination,
            author: None,
            target: at(target),
            server_at: at(10 * WINDOW),
            change,
        };

        let edited = own(100, edit("still mine", 200));
        assert!(db.apply_change(&edited, at(0)).await.unwrap().is_some());
        let deleted = own(101, Change::Delete);
        assert!(db.apply_change(&deleted, at(0)).await.unwrap().is_some());
        db.close().await;
    }

    #[tokio::test]
    async fn edits_stop_at_the_limit() {
        let (_dir, db) = db().await;
        incoming(&db, 1, 100, "v0").await.unwrap();

        for n in 1..=MAX_EDITS as u64 {
            let edit = change(1, 100, edit(&format!("v{n}"), 100 + n));
            assert!(db.apply_change(&edit, at(0)).await.unwrap().is_some());
        }
        let over = change(1, 100, edit("one more", 1000));
        assert_eq!(db.apply_change(&over, at(0)).await.unwrap(), None);
        let delete = change(1, 100, Change::Delete);
        assert!(db.apply_change(&delete, at(0)).await.unwrap().is_some());
        db.close().await;
    }

    #[tokio::test]
    async fn kept_changes_apply_only_within_the_window() {
        let (_dir, db) = db().await;
        let late_delete = got_at(100 + WINDOW + 1, change(1, 100, Change::Delete));
        db.apply_change(&late_delete, at(0)).await.unwrap();
        let late_edit = got_at(100 + WINDOW + 1, change(1, 100, edit("v3", 300)));
        db.apply_change(&late_edit, at(0)).await.unwrap();
        let in_time = got_at(100 + WINDOW, change(1, 100, edit("v2", 200)));
        db.apply_change(&in_time, at(0)).await.unwrap();
        let other_late = got_at(50 + WINDOW + 1, change(2, 50, edit("x", 60)));
        db.apply_change(&other_late, at(0)).await.unwrap();

        let message = incoming(&db, 1, 100, "v1").await.unwrap();
        assert_eq!(message.content, Content::text("v2"));
        let message = incoming(&db, 2, 50, "kept").await.unwrap();
        assert_eq!(message.content, Content::text("kept"));
        db.close().await;
    }

    #[tokio::test]
    async fn migration_bounds_older_messages_by_their_arrival() {
        let (_dir, db) = db().await;
        let theirs = incoming(&db, 1, 100, "teh").await.unwrap();
        db.apply_change(&change(1, 100, edit("the", 200)), at(0))
            .await
            .unwrap();
        // One connection, so the migration sees the dropped columns.
        let mut connection = db.pool().acquire().await.unwrap();
        sqlx::raw_sql(
            "ALTER TABLE messages DROP COLUMN server_at;
             ALTER TABLE messages DROP COLUMN edits;
             ALTER TABLE pending_changes DROP COLUMN server_at;
             UPDATE messages SET received_at = 5000;",
        )
        .execute(&mut *connection)
        .await
        .unwrap();

        let migration = include_str!("../../migrations/20261027120000_change_limits.sql");
        sqlx::raw_sql(migration)
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);

        let row: (Option<i64>, i64) =
            sqlx::query_as("SELECT server_at, edits FROM messages WHERE id = ?")
                .bind(theirs.id.get())
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row, (Some(5000), 1));
        db.close().await;
    }
}
