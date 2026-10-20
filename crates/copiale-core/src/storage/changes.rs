//! Edits and deletes of earlier messages, including ones that arrive first.

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::{content_columns, load_message, millis};
use super::retries::placeholder_json;
use crate::model::{
    Change, ChatRef, Content, ConversationId, Destination, Message, MessageChange, MessageId,
    MessageRef, Timestamp,
};

impl Database {
    /// Applies an edit or delete by the message's author; returns the
    /// changed message. One for a message not stored yet is kept until it
    /// is, from `now`; `None` then, and when there's nothing to change.
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
                "INSERT INTO pending_changes (author, target, content, edited_at, received_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&author)
            .bind(millis(change.target))
            .bind(content)
            .bind(edited_at)
            .bind(millis(now))
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
            tx.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let changed = update(&mut tx, MessageId::new(id), &change.change).await?;
        let message = match changed {
            true => load_message(&mut tx, MessageId::new(id)).await?,
            false => None,
        };
        tx.commit().await.map_err(database_error)?;
        Ok(message)
    }

    /// Deletes messages for good; returns the ones that were stored, by
    /// conversation.
    pub(crate) async fn delete_messages(
        &self,
        messages: &[MessageRef],
    ) -> Result<Vec<(ConversationId, MessageId)>, StorageError> {
        let mut deleted = Vec::new();
        for message in messages {
            let Some(conversation) = self.chat_conversation(message.chat).await? else {
                continue;
            };
            let id: Option<i64> = sqlx::query_scalar(
                "DELETE FROM messages WHERE conversation_id = ? AND author = ? AND sent_at = ?
                 RETURNING id",
            )
            .bind(conversation.get())
            .bind(author_bytes(message.author))
            .bind(millis(message.sent_at))
            .fetch_optional(&self.app)
            .await
            .map_err(database_error)?;
            deleted.extend(id.map(|id| (conversation, MessageId::new(id))));
        }
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
/// latest edit. Returns the message as it now is.
pub(super) async fn apply_pending(
    tx: &mut sqlx::SqliteConnection,
    message: Message,
) -> Result<Message, StorageError> {
    let author = author_bytes(message.sender);
    let target = millis(message.sent_at);
    let pending: Vec<(Option<String>, Option<i64>)> = sqlx::query_as(
        "DELETE FROM pending_changes WHERE author = ? AND target = ?
         RETURNING content, edited_at",
    )
    .bind(&author)
    .bind(target)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    let change = if pending.iter().any(|(content, _)| content.is_none()) {
        Some(Change::Delete)
    } else {
        pending
            .into_iter()
            .filter_map(|(content, at)| Some((content?, at?)))
            .max_by_key(|(_, at)| *at)
            .map(|(content, at)| {
                let content = serde_json::from_str(&content)
                    .map_err(|_| StorageError::Database("invalid kept edit".into()))?;
                let at = Timestamp::from_millis(u64::try_from(at).unwrap_or(0));
                Ok::<_, StorageError>(Change::Edit { content, at })
            })
            .transpose()?
    };
    let Some(change) = change else {
        return Ok(message);
    };
    update(tx, message.id, &change).await?;
    Ok(load_message(tx, message.id).await?.unwrap_or(message))
}

/// Applies `change` to a stored message; `false` if it doesn't apply: an
/// edit to or of anything but text, e.g. a deleted message or a system
/// line, or an older edit than the stored one.
async fn update(
    tx: &mut sqlx::SqliteConnection,
    id: MessageId,
    change: &Change,
) -> Result<bool, StorageError> {
    let deleted = json(&Content::Deleted)?;
    let result = match change {
        Change::Delete => {
            sqlx::query(
                "UPDATE messages SET body = '', content = ?1
             WHERE id = ?2 AND coalesce(content, '') != ?1",
            )
            .bind(&deleted)
            .bind(id.get())
            .execute(&mut *tx)
            .await
        }
        Change::Edit { content, .. } if !matches!(content, Content::Text { .. }) => {
            return Ok(false);
        }
        Change::Edit { content, at } => {
            let (body, content) = content_columns(content)?;
            // Only text is edited: plain, or kept as JSON.
            sqlx::query(
                "UPDATE messages SET body = ?1, content = ?2, edited_at = ?3
                 WHERE id = ?4 AND (content IS NULL OR json_type(content, '$.text') IS NOT NULL)
                   AND (edited_at IS NULL OR edited_at < ?3)",
            )
            .bind(body)
            .bind(content)
            .bind(millis(*at))
            .bind(id.get())
            .execute(&mut *tx)
            .await
        }
    }
    .map_err(database_error)?;
    Ok(result.rows_affected() > 0)
}

fn author_bytes(author: Option<crate::model::ServiceId>) -> Vec<u8> {
    author.map_or_else(Vec::new, |author| author.uuid().as_bytes().to_vec())
}

fn json(content: &Content) -> Result<String, StorageError> {
    serde_json::to_string(content).map_err(|error| StorageError::Database(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupMasterKey, GroupRef, GroupUpdate, MessageBody, Quote};
    use crate::storage::testing::{at, db, peer};

    async fn incoming(db: &Database, from: u128, sent_at: u64, body: &str) -> Option<Message> {
        db.add_message(
            &Destination::Contact(peer(from)),
            Some(peer(from)),
            at(sent_at),
            &Content::text(body),
            None,
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
        assert!(edited.edited);
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
        assert!(message.edited);

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

    #[tokio::test]
    async fn quotes_are_kept() {
        let (_dir, db) = db().await;
        let reply = Content::Text {
            body: MessageBody::new("agreed"),
            attachments: Vec::new(),
            quote: Some(Quote {
                author: Some(peer(2)),
                sent_at: at(50),
                text: "ship it?".into(),
                attachment: None,
            }),
        };

        let message = db
            .add_message(
                &Destination::Contact(peer(1)),
                Some(peer(1)),
                at(100),
                &reply,
                None,
                Timestamp::from_millis(0),
            )
            .await
            .unwrap()
            .unwrap();

        let stored = db.messages(message.conversation, None, 10).await.unwrap();
        assert_eq!(stored[0].content, reply);
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

        assert_eq!(
            deleted,
            [
                (first.conversation, first.id),
                (first.conversation, mine.id)
            ]
        );
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
}
