//! Messages that failed to decrypt, and our sends kept to resend when a
//! recipient's didn't.

use super::StorageError;
use super::database::{Database, database_error};
use super::messages::{
    Removed, RemovedRow, conversation_id, insert_message, millis, removed, removed_columns,
};
use crate::backend::Resendable;
use crate::model::{Content, Destination, GroupId, Message, ServiceId, Timestamp};

impl Database {
    /// Stores a placeholder for `sender`'s message sent at `sent_at` that
    /// couldn't be decrypted: in `group` if it's known with them in it,
    /// else in the 1:1 conversation. `None` if the message is already
    /// stored.
    pub(crate) async fn add_placeholder(
        &self,
        sender: ServiceId,
        group: Option<GroupId>,
        sent_at: Timestamp,
        now: Timestamp,
    ) -> Result<Option<Message>, StorageError> {
        let mut tx = self.begin_write().await?;
        let author = sender.uuid().as_bytes().to_vec();
        let stored: Option<i64> =
            sqlx::query_scalar("SELECT id FROM messages WHERE author = ? AND sent_at = ?")
                .bind(&author)
                .bind(millis(sent_at))
                .fetch_optional(&mut *tx)
                .await
                .map_err(database_error)?;
        if stored.is_some() {
            return Ok(None);
        }
        let in_group: Option<i64> = match group {
            Some(group) => sqlx::query_scalar(
                "SELECT c.id FROM conversations c
                 WHERE c.group_id = ? AND EXISTS (
                     SELECT 1 FROM group_members g
                     WHERE g.conversation_id = c.id AND g.member = ?)",
            )
            .bind(group.as_bytes().as_slice())
            .bind(&author)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?,
            None => None,
        };
        let conversation = match in_group {
            Some(id) => id,
            None => conversation_id(&mut tx, &Destination::Contact(sender)).await?,
        };
        let message = insert_message(
            &mut tx,
            conversation,
            Some(sender),
            None,
            sent_at,
            &Content::Undecryptable,
            None,
            None,
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(message)
    }

    /// Deletes the placeholders for `author`'s message sent at `sent_at`,
    /// which has now arrived; returns them by conversation.
    pub(crate) async fn take_placeholders(
        &self,
        author: ServiceId,
        sent_at: Timestamp,
    ) -> Result<Vec<Removed>, StorageError> {
        let rows: Vec<RemovedRow> = sqlx::query_as(concat!(
            "DELETE FROM messages WHERE author = ? AND sent_at = ? AND content = ?
             RETURNING ",
            removed_columns!()
        ))
        .bind(author.uuid().as_bytes().as_slice())
        .bind(millis(sent_at))
        .bind(placeholder_json()?)
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        rows.into_iter().map(removed).collect()
    }

    /// Keeps a send, at `now`, to resend on request: `sent_at` is its own
    /// timestamp, `message_sent_at` the sent time of the message it sent or
    /// changed, forgotten with it. Nothing is kept without recipients.
    pub(crate) async fn log_send(
        &self,
        sent_at: Timestamp,
        message_sent_at: Timestamp,
        resendable: &Resendable,
        now: Timestamp,
    ) -> Result<(), StorageError> {
        if resendable.recipients.is_empty() {
            return Ok(());
        }
        let mut tx = self.begin_write().await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO send_log (sent_at, message_sent_at, content, logged_at)
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(millis(sent_at))
        .bind(millis(message_sent_at))
        .bind(resendable.content.as_slice())
        .bind(millis(now))
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        for (recipient, devices) in &resendable.recipients {
            for device in devices {
                sqlx::query(
                    "INSERT OR IGNORE INTO send_log_recipients (log_id, recipient, device)
                     VALUES (?, ?, ?)",
                )
                .bind(id)
                .bind(recipient.uuid().as_bytes().as_slice())
                .bind(device)
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
            }
        }
        tx.commit().await.map_err(database_error)?;
        Ok(())
    }

    /// What we sent `recipient`'s `device` at `sent_at`, if kept since
    /// `since`.
    pub(crate) async fn resendable(
        &self,
        recipient: ServiceId,
        device: u32,
        sent_at: Timestamp,
        since: Timestamp,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        sqlx::query_scalar(
            "SELECT l.content FROM send_log l
             JOIN send_log_recipients r ON r.log_id = l.id
             WHERE l.sent_at = ? AND r.recipient = ? AND r.device = ? AND l.logged_at >= ?
             ORDER BY l.id DESC LIMIT 1",
        )
        .bind(millis(sent_at))
        .bind(recipient.uuid().as_bytes().as_slice())
        .bind(device)
        .bind(millis(since))
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)
    }

    /// Forgets sends kept before `before`; returns how many.
    pub(crate) async fn forget_sends(&self, before: Timestamp) -> Result<u64, StorageError> {
        let result = sqlx::query("DELETE FROM send_log WHERE logged_at < ?")
            .bind(millis(before))
            .execute(&self.app)
            .await
            .map_err(database_error)?;
        Ok(result.rows_affected())
    }
}

pub(super) fn placeholder_json() -> Result<String, StorageError> {
    serde_json::to_string(&Content::Undecryptable)
        .map_err(|error| StorageError::Database(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Change, ConversationId, GroupInfo, GroupMasterKey, GroupRef, Member, MessageChange,
    };
    use crate::storage::testing::{at, db, peer, removed_of};

    /// Sent to device 1 of each recipient.
    fn resendable(recipients: &[u128], content: &[u8]) -> Resendable {
        Resendable {
            recipients: recipients.iter().map(|n| (peer(*n), vec![1])).collect(),
            content: content.to_vec(),
        }
    }

    async fn own_message(db: &Database, to: u128, sent_at: u64) -> Message {
        db.add_message(
            &Destination::Contact(peer(to)),
            None,
            at(sent_at),
            &Content::text("hi"),
            None,
            None,
            at(sent_at),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn group_with(db: &Database, key: GroupMasterKey, member: u128) -> ConversationId {
        let group = Destination::Group(GroupRef { key, revision: 1 });
        let conversation = db.open_conversation(&group).await.unwrap();
        let info = GroupInfo {
            title: "Team".into(),
            revision: 1,
            members: vec![Member {
                id: peer(member),
                name: None,
            }],
        };
        db.set_group(key, &info).await.unwrap();
        conversation
    }

    #[tokio::test]
    async fn placeholders_go_to_the_senders_group_or_chat() {
        let (_dir, db) = db().await;
        let key = GroupMasterKey::new([3; 32]);
        let group = group_with(&db, key, 1).await;

        let in_group = db
            .add_placeholder(peer(1), Some(key.id()), at(10), at(50))
            .await
            .unwrap()
            .unwrap();
        let not_member = db
            .add_placeholder(peer(2), Some(key.id()), at(11), at(50))
            .await
            .unwrap()
            .unwrap();
        let direct = db
            .add_placeholder(peer(1), None, at(12), at(50))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(in_group.conversation, group);
        assert_eq!(in_group.content, Content::Undecryptable);
        assert_eq!(in_group.sender, Some(peer(1)));
        assert!(in_group.read, "placeholders arrive read");
        let chat = async |n| {
            db.open_conversation(&Destination::Contact(peer(n)))
                .await
                .unwrap()
        };
        assert_eq!(not_member.conversation, chat(2).await);
        assert_eq!(direct.conversation, chat(1).await);
        db.close().await;
    }

    #[tokio::test]
    async fn placeholders_skip_stored_messages_and_repeats() {
        let (_dir, db) = db().await;
        db.add_message(
            &Destination::Contact(peer(1)),
            Some(peer(1)),
            at(10),
            &Content::text("hi"),
            None,
            Some(at(10)),
            at(10),
        )
        .await
        .unwrap();

        let stored = db.add_placeholder(peer(1), None, at(10), at(50)).await;
        let first = db.add_placeholder(peer(1), None, at(11), at(50)).await;
        let repeat = db.add_placeholder(peer(1), None, at(11), at(50)).await;

        assert_eq!(stored.unwrap(), None);
        assert!(first.unwrap().is_some());
        assert_eq!(repeat.unwrap(), None);
        db.close().await;
    }

    #[tokio::test]
    async fn arriving_messages_take_their_placeholders() {
        let (_dir, db) = db().await;
        let placeholder = db
            .add_placeholder(peer(1), None, at(10), at(50))
            .await
            .unwrap()
            .unwrap();
        db.add_placeholder(peer(1), None, at(11), at(50))
            .await
            .unwrap();

        let taken = db.take_placeholders(peer(1), at(10)).await.unwrap();
        let again = db.take_placeholders(peer(1), at(10)).await.unwrap();

        assert_eq!(taken, [removed_of(&placeholder)]);
        assert_eq!(again, []);
        let left = db
            .messages(placeholder.conversation, None, 10)
            .await
            .unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].sent_at, at(11));
        db.close().await;
    }

    #[tokio::test]
    async fn changes_wait_for_the_message_behind_a_placeholder() {
        let (_dir, db) = db().await;
        db.add_placeholder(peer(1), None, at(10), at(50))
            .await
            .unwrap();
        let edit = MessageChange {
            destination: Destination::Contact(peer(1)),
            author: Some(peer(1)),
            target: at(10),
            server_at: at(10),
            change: Change::Edit {
                content: Content::text("edited"),
                at: at(20),
            },
        };

        let applied = db.apply_change(&edit, at(60)).await.unwrap();

        assert_eq!(applied, None, "kept for the message");
        db.take_placeholders(peer(1), at(10)).await.unwrap();
        let message = db
            .add_message(
                &Destination::Contact(peer(1)),
                Some(peer(1)),
                at(10),
                &Content::text("original"),
                None,
                Some(at(10)),
                at(70),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.content, Content::text("edited"));
        db.close().await;
    }

    #[tokio::test]
    async fn sends_are_resent_only_to_their_recipients() {
        let (_dir, db) = db().await;
        db.log_send(at(10), at(10), &resendable(&[1, 2], b"group"), at(10))
            .await
            .unwrap();
        db.log_send(at(11), at(11), &resendable(&[1], b"direct"), at(11))
            .await
            .unwrap();

        let find = |n, sent_at| db.resendable(peer(n), 1, at(sent_at), at(0));
        assert_eq!(find(2, 10).await.unwrap().as_deref(), Some(&b"group"[..]));
        assert_eq!(find(1, 11).await.unwrap().as_deref(), Some(&b"direct"[..]));
        assert_eq!(find(2, 11).await.unwrap(), None, "not a recipient");
        assert_eq!(find(1, 12).await.unwrap(), None);
        db.close().await;
    }

    #[tokio::test]
    async fn sends_are_resent_only_to_devices_that_got_them() {
        let (_dir, db) = db().await;
        let sent = Resendable {
            recipients: vec![(peer(1), vec![1, 3]), (peer(2), vec![2])],
            content: b"group".to_vec(),
        };
        db.log_send(at(10), at(10), &sent, at(10)).await.unwrap();

        let find = |n, device| db.resendable(peer(n), device, at(10), at(0));
        assert!(find(1, 1).await.unwrap().is_some());
        assert!(find(1, 3).await.unwrap().is_some());
        assert!(find(2, 2).await.unwrap().is_some());
        assert_eq!(find(1, 2).await.unwrap(), None, "linked after the send");
        assert_eq!(find(2, 1).await.unwrap(), None, "another member's device");
        db.close().await;
    }

    #[tokio::test]
    async fn sends_without_recipients_are_not_kept() {
        let (_dir, db) = db().await;

        db.log_send(at(10), at(10), &resendable(&[], b"self"), at(10))
            .await
            .unwrap();

        assert_eq!(db.forget_sends(at(100)).await.unwrap(), 0);
        db.close().await;
    }

    #[tokio::test]
    async fn old_sends_are_forgotten() {
        let (_dir, db) = db().await;
        db.log_send(at(10), at(10), &resendable(&[1], b"old"), at(10))
            .await
            .unwrap();
        db.log_send(at(20), at(20), &resendable(&[1], b"new"), at(20))
            .await
            .unwrap();

        assert_eq!(
            db.resendable(peer(1), 1, at(10), at(15)).await.unwrap(),
            None
        );
        assert_eq!(db.forget_sends(at(15)).await.unwrap(), 1);
        assert_eq!(
            db.resendable(peer(1), 1, at(10), at(0)).await.unwrap(),
            None
        );
        assert!(
            db.resendable(peer(1), 1, at(20), at(0))
                .await
                .unwrap()
                .is_some()
        );
        db.close().await;
    }

    #[tokio::test]
    async fn sends_go_with_their_message() {
        let (_dir, db) = db().await;
        let deleted = own_message(&db, 1, 10).await;
        let expiring = own_message(&db, 1, 20).await;
        let kept = own_message(&db, 1, 30).await;
        for message in [&deleted, &expiring, &kept] {
            db.log_send(
                message.sent_at,
                message.sent_at,
                &resendable(&[1], b"x"),
                at(0),
            )
            .await
            .unwrap();
        }
        // An edit of the deleted message, sent later.
        db.log_send(at(15), at(10), &resendable(&[1], b"edit"), at(0))
            .await
            .unwrap();

        let delete = MessageChange {
            destination: Destination::Contact(peer(1)),
            author: None,
            target: at(10),
            server_at: at(10),
            change: Change::Delete,
        };
        db.apply_change(&delete, at(40)).await.unwrap().unwrap();
        db.delete_messages(&[crate::model::MessageRef {
            chat: crate::model::ChatRef::Contact(peer(1)),
            author: None,
            sent_at: at(20),
        }])
        .await
        .unwrap();

        let find = |sent_at| db.resendable(peer(1), 1, at(sent_at), at(0));
        assert_eq!(find(10).await.unwrap(), None, "deleted for everyone");
        assert_eq!(find(15).await.unwrap(), None, "its edit");
        assert_eq!(find(20).await.unwrap(), None, "deleted here");
        assert!(find(30).await.unwrap().is_some());
        db.close().await;
    }
}
