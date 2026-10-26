//! Attachments' downloads and their encrypted copies on disk.
//!
//! A message's `content` describes its attachments; rows here, by position
//! in that list, track the downloadable ones, and for videos their first
//! frame. Deleting a row, also with its message, queues its copies in
//! `deleted_files` until they're gone from disk.

use std::collections::HashSet;

use super::StorageError;
use super::database::{Database, database_error};
use super::files::{FileKey, Files, StoredFile};
use crate::model::{
    Attachment, AttachmentId, AttachmentState, Content, Message, MessageId, RemoteAttachment,
};

impl Database {
    /// Where downloads are written before [`Self::finish_download`].
    pub(crate) fn files(&self) -> Files {
        self.files.clone()
    }

    /// Marks an attachment downloading, unless it's here, going or gone;
    /// returns where to get it, and its message.
    pub(crate) async fn start_download(
        &self,
        id: AttachmentId,
    ) -> Result<Option<(MessageId, RemoteAttachment)>, StorageError> {
        let row: Option<(i64, Vec<u8>)> = sqlx::query_as(
            "UPDATE attachments SET state = 'downloading'
             WHERE id = ? AND state IN ('remote', 'failed')
             RETURNING message_id, pointer",
        )
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        Ok(row.map(|(message, pointer)| (MessageId::new(message), RemoteAttachment::new(pointer))))
    }

    /// Records a download's copy; returns the attachment's message, or
    /// `None`, deleting the copy, if it's gone or no longer downloading.
    pub(crate) async fn finish_download(
        &self,
        id: AttachmentId,
        file: StoredFile,
    ) -> Result<Option<MessageId>, StorageError> {
        let message: Option<i64> = sqlx::query_scalar(
            "UPDATE attachments SET state = 'ready', file = ?, file_key = ?
             WHERE id = ? AND state = 'downloading'
             RETURNING message_id",
        )
        .bind(&file.name)
        .bind(file.key.as_bytes())
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        if message.is_none() {
            self.files.remove(&file.name)?;
        }
        Ok(message.map(MessageId::new))
    }

    /// Ends a download that didn't finish, leaving the attachment in
    /// `state`; returns its message, if it was downloading.
    pub(crate) async fn stop_download(
        &self,
        id: AttachmentId,
        state: AttachmentState,
    ) -> Result<Option<MessageId>, StorageError> {
        let message: Option<i64> = sqlx::query_scalar(
            "UPDATE attachments SET state = ? WHERE id = ? AND state = 'downloading'
             RETURNING message_id",
        )
        .bind(state_name(state))
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        Ok(message.map(MessageId::new))
    }

    /// A downloaded attachment's contents; `None` if it isn't here.
    pub(crate) async fn attachment_contents(
        &self,
        id: AttachmentId,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let row: Option<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT file, file_key FROM attachments
             WHERE id = ? AND state = 'ready' AND file IS NOT NULL AND file_key IS NOT NULL",
        )
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        self.read_file(row).await
    }

    /// A video's first frame as a PNG; `None` if there's none yet.
    pub(crate) async fn preview_contents(
        &self,
        id: AttachmentId,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let row: Option<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT preview_file, preview_key FROM attachments
             WHERE id = ? AND preview = 'ready'
               AND preview_file IS NOT NULL AND preview_key IS NOT NULL",
        )
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        self.read_file(row).await
    }

    /// Reads and decrypts a file by its name and key.
    async fn read_file(
        &self,
        file: Option<(String, Vec<u8>)>,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let Some((name, key)) = file else {
            return Ok(None);
        };
        let key = FileKey::from_slice(&key)
            .ok_or_else(|| StorageError::Database("invalid file key".into()))?;
        let files = self.files.clone();
        let contents = tokio::task::spawn_blocking(move || files.read(&name, &key))
            .await
            .map_err(std::io::Error::other)??;
        Ok(Some(contents))
    }

    /// Downloaded videos whose first frame is still wanted.
    pub(crate) async fn pending_previews(&self) -> Result<Vec<AttachmentId>, StorageError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM attachments WHERE state = 'ready' AND preview = 'wanted' ORDER BY id",
        )
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        Ok(ids.into_iter().map(AttachmentId::new).collect())
    }

    /// Whether a downloaded video's first frame is still wanted.
    pub(crate) async fn preview_wanted(&self, id: AttachmentId) -> Result<bool, StorageError> {
        let wanted: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM attachments WHERE id = ? AND state = 'ready' AND preview = 'wanted'",
        )
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        Ok(wanted.is_some())
    }

    /// Records a video's first frame; returns its message, or `None`,
    /// deleting the copy, if it's gone or no longer wanted.
    pub(crate) async fn set_preview(
        &self,
        id: AttachmentId,
        file: StoredFile,
    ) -> Result<Option<MessageId>, StorageError> {
        let message: Option<i64> = sqlx::query_scalar(
            "UPDATE attachments SET preview = 'ready', preview_file = ?, preview_key = ?
             WHERE id = ? AND state = 'ready' AND preview = 'wanted'
             RETURNING message_id",
        )
        .bind(&file.name)
        .bind(file.key.as_bytes())
        .bind(id.get())
        .fetch_optional(&self.app)
        .await
        .map_err(database_error)?;
        if message.is_none() {
            self.files.remove(&file.name)?;
        }
        Ok(message.map(MessageId::new))
    }

    /// Notes a video's first frame couldn't be had, so it isn't tried
    /// again.
    pub(crate) async fn preview_failed(&self, id: AttachmentId) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE attachments SET preview = 'failed' WHERE id = ? AND preview = 'wanted'",
        )
        .bind(id.get())
        .execute(&self.app)
        .await
        .map_err(database_error)?;
        Ok(())
    }

    /// A stored attachment as its message describes it.
    pub(crate) async fn attachment(
        &self,
        id: AttachmentId,
    ) -> Result<Option<Attachment>, StorageError> {
        let row: Option<(i64, i64)> =
            sqlx::query_as("SELECT message_id, position FROM attachments WHERE id = ?")
                .bind(id.get())
                .fetch_optional(&self.app)
                .await
                .map_err(database_error)?;
        let Some((message, position)) = row else {
            return Ok(None);
        };
        let Some(message) = self.message(MessageId::new(message)).await? else {
            return Ok(None);
        };
        let Content::Text { attachments, .. } = message.content else {
            return Ok(None);
        };
        Ok(usize::try_from(position)
            .ok()
            .and_then(|position| attachments.into_iter().nth(position)))
    }

    /// Deletes the copies of deleted attachments. Failures are logged; the
    /// copies are tried again next time.
    pub(super) async fn remove_deleted_files(&self) {
        if let Err(error) = self.try_remove_deleted_files().await {
            tracing::warn!(%error, "deleting attachment files failed");
        }
    }

    async fn try_remove_deleted_files(&self) -> Result<(), StorageError> {
        let names: Vec<String> = sqlx::query_scalar("SELECT file FROM deleted_files")
            .fetch_all(&self.app)
            .await
            .map_err(database_error)?;
        for name in names {
            self.files.remove(&name)?;
            sqlx::query("DELETE FROM deleted_files WHERE file = ?")
                .bind(&name)
                .execute(&self.app)
                .await
                .map_err(database_error)?;
        }
        Ok(())
    }

    /// Puts attachment files and their rows back in step, after a crash or
    /// quit: deletes deleted attachments' copies and files nothing refers
    /// to, forgets missing copies, and stops downloads cut off. Returns the
    /// attachments that were downloading, to download again.
    pub(crate) async fn check_files(&self) -> Result<Vec<AttachmentId>, StorageError> {
        self.try_remove_deleted_files().await?;
        let kept: HashSet<String> = sqlx::query_scalar(
            "SELECT file FROM attachments WHERE file IS NOT NULL
             UNION SELECT preview_file FROM attachments WHERE preview_file IS NOT NULL",
        )
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?
        .into_iter()
        .collect();
        let on_disk: HashSet<String> = self.files.list()?.into_iter().collect();
        for name in on_disk.difference(&kept) {
            self.files.remove(name)?;
        }
        for name in kept.difference(&on_disk) {
            tracing::warn!("an attachment's file is missing");
            sqlx::query(
                "UPDATE attachments SET state = 'remote', file = NULL, file_key = NULL
                 WHERE file = ?",
            )
            .bind(name)
            .execute(&self.app)
            .await
            .map_err(database_error)?;
            sqlx::query(
                "UPDATE attachments SET preview = 'wanted', preview_file = NULL, preview_key = NULL
                 WHERE preview_file = ?",
            )
            .bind(name)
            .execute(&self.app)
            .await
            .map_err(database_error)?;
        }
        let interrupted: Vec<i64> = sqlx::query_scalar(
            "UPDATE attachments SET state = 'remote' WHERE state = 'downloading' RETURNING id",
        )
        .fetch_all(&self.app)
        .await
        .map_err(database_error)?;
        Ok(interrupted.into_iter().map(AttachmentId::new).collect())
    }
}

/// Adds rows for the downloadable attachments of `message`'s `content`.
pub(super) async fn insert_attachments(
    tx: &mut sqlx::SqliteConnection,
    message: MessageId,
    content: &Content,
) -> Result<(), StorageError> {
    let Content::Text { attachments, .. } = content else {
        return Ok(());
    };
    for (position, attachment) in attachments.iter().enumerate() {
        let Some(remote) = &attachment.remote else {
            continue;
        };
        sqlx::query(
            "INSERT INTO attachments (message_id, position, pointer, state, preview)
             VALUES (?, ?, ?, 'remote', ?)",
        )
        .bind(message.get())
        .bind(position as i64)
        .bind(remote.as_bytes())
        .bind(attachment.is_video().then_some("wanted"))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

/// Fills in the IDs, state and previews of `message`'s attachments.
pub(super) async fn resolve_attachments(
    conn: &mut sqlx::SqliteConnection,
    message: &mut Message,
) -> Result<(), StorageError> {
    let Content::Text { attachments, .. } = &mut message.content else {
        return Ok(());
    };
    if attachments.is_empty() {
        return Ok(());
    }
    let rows: Vec<(i64, i64, String, Option<String>)> =
        sqlx::query_as("SELECT id, position, state, preview FROM attachments WHERE message_id = ?")
            .bind(message.id.get())
            .fetch_all(&mut *conn)
            .await
            .map_err(database_error)?;
    for (id, position, state, preview) in rows {
        let attachment = usize::try_from(position)
            .ok()
            .and_then(|position| attachments.get_mut(position));
        if let Some(attachment) = attachment {
            attachment.id = Some(AttachmentId::new(id));
            attachment.state = parse_state(&state)?;
            attachment.has_preview = preview.as_deref() == Some("ready");
        }
    }
    Ok(())
}

/// `content` without where to download its attachments, which stays in
/// storage.
pub(super) fn without_remotes(content: &Content) -> Content {
    let mut content = content.clone();
    if let Content::Text { attachments, .. } = &mut content {
        for attachment in attachments.iter_mut() {
            attachment.remote = None;
            attachment.state = AttachmentState::Unavailable;
            attachment.has_preview = false;
        }
    }
    content
}

/// Replaces the attachments of an edit's `content` with the stored
/// message's: edits change text only.
pub(super) fn keep_attachments(content: &mut Content, stored: Option<&Content>) {
    if let Content::Text { attachments, .. } = content {
        *attachments = match stored {
            Some(Content::Text { attachments, .. }) => attachments.clone(),
            _ => Vec::new(),
        };
    }
}

fn state_name(state: AttachmentState) -> &'static str {
    match state {
        AttachmentState::Unavailable => "unavailable",
        AttachmentState::Remote => "remote",
        AttachmentState::Downloading => "downloading",
        AttachmentState::Ready => "ready",
        AttachmentState::Failed => "failed",
        AttachmentState::Expired => "expired",
    }
}

fn parse_state(name: &str) -> Result<AttachmentState, StorageError> {
    match name {
        "remote" => Ok(AttachmentState::Remote),
        "downloading" => Ok(AttachmentState::Downloading),
        "ready" => Ok(AttachmentState::Ready),
        "failed" => Ok(AttachmentState::Failed),
        "expired" => Ok(AttachmentState::Expired),
        _ => Err(StorageError::Database("invalid attachment state".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Attachment, AttachmentKind, Change, ChatRef, Destination, MessageBody, MessageChange,
        MessageRef, Timestamp,
    };
    use crate::storage::testing::{at, db, peer};

    fn remote(name: &str) -> Attachment {
        Attachment {
            size: Some(5),
            state: AttachmentState::Remote,
            remote: Some(RemoteAttachment::new(name.as_bytes().to_vec())),
            ..Attachment::new(AttachmentKind::Image)
        }
    }

    fn with_attachments(body: &str, attachments: Vec<Attachment>) -> Content {
        Content::Text {
            body: MessageBody::new(body),
            attachments,
            quote: None,
        }
    }

    async fn incoming(db: &Database, sent_at: u64, content: &Content) -> Message {
        db.add_message(
            &Destination::Contact(peer(1)),
            Some(peer(1)),
            at(sent_at),
            content,
            Some(60),
            Some(at(sent_at)),
            Timestamp::from_millis(0),
        )
        .await
        .unwrap()
        .unwrap()
    }

    fn attachments(message: &Message) -> &[Attachment] {
        match &message.content {
            Content::Text { attachments, .. } => attachments,
            _ => panic!("not text"),
        }
    }

    fn id(message: &Message, position: usize) -> AttachmentId {
        attachments(message)[position].id.unwrap()
    }

    /// Downloads `message`'s first attachment as `contents`.
    async fn download(db: &Database, message: &Message, contents: &[u8]) {
        let id = id(message, 0);
        db.start_download(id).await.unwrap().unwrap();
        let file = db.files().store(contents.to_vec()).await.unwrap();
        db.finish_download(id, file).await.unwrap().unwrap();
    }

    async fn files_on_disk(db: &Database) -> usize {
        db.files.list().unwrap().len()
    }

    #[tokio::test]
    async fn downloadable_attachments_are_tracked_and_others_described() {
        let (_dir, db) = db().await;
        let content = with_attachments(
            "caption",
            vec![Attachment::new(AttachmentKind::Video), remote("cdn-1")],
        );

        let added = incoming(&db, 1, &content).await;

        let [video, image] = attachments(&added) else {
            panic!("two attachments");
        };
        assert_eq!(
            (video.id, video.state),
            (None, AttachmentState::Unavailable)
        );
        assert!(image.id.is_some());
        assert_eq!(image.state, AttachmentState::Remote);
        assert_eq!(image.remote, None, "where to get it stays in storage");
        let loaded = db.message(added.id).await.unwrap().unwrap();
        assert_eq!(loaded, added);
        let (message, pointer) = db.start_download(image.id.unwrap()).await.unwrap().unwrap();
        assert_eq!(message, added.id);
        assert_eq!(pointer, RemoteAttachment::new(b"cdn-1".to_vec()));
    }

    #[tokio::test]
    async fn attachments_stored_before_downloads_still_load() {
        let (_dir, db) = db().await;
        let added = incoming(&db, 1, &Content::text("x")).await;
        sqlx::query("UPDATE messages SET body = '', content = ? WHERE id = ?")
            .bind(r#"{"text":{"body":"","attachments":["image",{"file":{"name":"a.pdf"}}]}}"#)
            .bind(added.id.get())
            .execute(db.pool())
            .await
            .unwrap();

        let loaded = db.message(added.id).await.unwrap().unwrap();

        let expected = vec![
            Attachment::new(AttachmentKind::Image),
            Attachment {
                name: Some("a.pdf".into()),
                ..Attachment::new(AttachmentKind::File)
            },
        ];
        assert_eq!(attachments(&loaded), expected);
    }

    #[tokio::test]
    async fn downloads_are_stored_encrypted_and_read_back() {
        let (dir, db) = db().await;
        let added = incoming(&db, 1, &with_attachments("", vec![remote("cdn-1")])).await;
        let id = id(&added, 0);

        assert!(db.start_download(id).await.unwrap().is_some());
        assert_eq!(db.start_download(id).await.unwrap(), None, "already going");
        let downloading = db.message(added.id).await.unwrap().unwrap();
        assert_eq!(
            attachments(&downloading)[0].state,
            AttachmentState::Downloading
        );
        assert_eq!(db.attachment_contents(id).await.unwrap(), None);
        let file = db.files().store(b"a picture".to_vec()).await.unwrap();
        assert_eq!(db.finish_download(id, file).await.unwrap(), Some(added.id));

        let ready = db.message(added.id).await.unwrap().unwrap();
        assert_eq!(attachments(&ready)[0].state, AttachmentState::Ready);
        assert_eq!(
            db.attachment_contents(id).await.unwrap().as_deref(),
            Some(b"a picture".as_slice())
        );
        assert_eq!(db.start_download(id).await.unwrap(), None, "already here");
        for entry in std::fs::read_dir(dir.path().join(super::super::files::DIR)).unwrap() {
            let on_disk = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(!on_disk.windows(9).any(|window| window == b"a picture"));
        }
    }

    #[tokio::test]
    async fn failed_downloads_can_be_tried_again_and_expired_ones_not() {
        let (_dir, db) = db().await;
        let content = with_attachments("", vec![remote("cdn-1"), remote("cdn-2")]);
        let added = incoming(&db, 1, &content).await;
        let (first, second) = (id(&added, 0), id(&added, 1));

        assert_eq!(
            db.stop_download(first, AttachmentState::Failed)
                .await
                .unwrap(),
            None
        );
        db.start_download(first).await.unwrap().unwrap();
        db.start_download(second).await.unwrap().unwrap();
        db.stop_download(first, AttachmentState::Failed)
            .await
            .unwrap()
            .unwrap();
        db.stop_download(second, AttachmentState::Expired)
            .await
            .unwrap()
            .unwrap();

        let stopped = db.message(added.id).await.unwrap().unwrap();
        let states: Vec<_> = attachments(&stopped).iter().map(|a| a.state).collect();
        assert_eq!(states, [AttachmentState::Failed, AttachmentState::Expired]);
        assert!(db.start_download(first).await.unwrap().is_some());
        assert_eq!(db.start_download(second).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_download_for_a_deleted_message_leaves_no_file() {
        let (_dir, db) = db().await;
        let added = incoming(&db, 1, &with_attachments("", vec![remote("cdn-1")])).await;
        let id = id(&added, 0);
        db.start_download(id).await.unwrap().unwrap();
        let file = db.files().store(b"late".to_vec()).await.unwrap();
        db.delete_messages(&[MessageRef {
            chat: ChatRef::Contact(peer(1)),
            author: Some(peer(1)),
            sent_at: at(1),
        }])
        .await
        .unwrap();

        assert_eq!(db.finish_download(id, file).await.unwrap(), None);
        assert_eq!(files_on_disk(&db).await, 0);
    }

    #[tokio::test]
    async fn deleting_messages_deletes_their_files() {
        let (_dir, db) = db().await;
        let content = with_attachments("", vec![remote("cdn")]);
        let mut messages = Vec::new();
        for sent_at in 1..=4 {
            let message = incoming(&db, sent_at, &content).await;
            download(&db, &message, b"file").await;
            messages.push(message);
        }
        assert_eq!(files_on_disk(&db).await, 4);

        // For this account only.
        db.delete_messages(&[MessageRef {
            chat: ChatRef::Contact(peer(1)),
            author: Some(peer(1)),
            sent_at: at(1),
        }])
        .await
        .unwrap();
        assert_eq!(files_on_disk(&db).await, 3);
        // By its author.
        let delete = MessageChange {
            destination: Destination::Contact(peer(1)),
            author: Some(peer(1)),
            target: at(2),
            server_at: at(2),
            change: Change::Delete,
        };
        db.apply_change(&delete, at(2)).await.unwrap().unwrap();
        assert_eq!(files_on_disk(&db).await, 2);
        // Disappearing.
        db.mark_read(messages[2].conversation, at(10))
            .await
            .unwrap();
        db.expire(at(10 + 60_000)).await.unwrap();
        assert_eq!(files_on_disk(&db).await, 0);

        let message = incoming(&db, 5, &content).await;
        download(&db, &message, b"file").await;
        // With the whole chat.
        db.delete_conversation(ChatRef::Contact(peer(1)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(files_on_disk(&db).await, 0);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM deleted_files")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn edits_keep_the_attachments() {
        let (_dir, db) = db().await;
        let added = incoming(&db, 1, &with_attachments("teh", vec![remote("cdn-1")])).await;
        download(&db, &added, b"file").await;

        // Edits may carry attachments, which are ignored.
        let edit = MessageChange {
            destination: Destination::Contact(peer(1)),
            author: Some(peer(1)),
            target: at(1),
            server_at: at(2),
            change: Change::Edit {
                content: with_attachments("the", vec![remote("other")]),
                at: at(2),
            },
        };
        let edited = db.apply_change(&edit, at(2)).await.unwrap().unwrap();

        assert_eq!(edited.content.body().unwrap().as_str(), "the");
        assert_eq!(attachments(&edited).len(), 1);
        assert_eq!(attachments(&edited)[0].state, AttachmentState::Ready);
        assert_eq!(attachments(&edited)[0].id, attachments(&added)[0].id);
        // Text gains none.
        let plain = incoming(&db, 3, &Content::text("plain")).await;
        let edit = MessageChange {
            target: plain.sent_at,
            ..edit
        };
        let edited = db.apply_change(&edit, at(4)).await.unwrap().unwrap();
        assert_eq!(edited.content, Content::text("the"));
    }

    #[tokio::test]
    async fn checking_files_puts_disk_and_rows_back_in_step() {
        let (_dir, db) = db().await;
        let content = with_attachments("", vec![remote("a"), remote("b"), remote("c")]);
        let added = incoming(&db, 1, &content).await;
        download(&db, &added, b"kept").await;
        let gone = db.files().store(b"gone".to_vec()).await.unwrap();
        let (second, third) = (id(&added, 1), id(&added, 2));
        db.start_download(second).await.unwrap().unwrap();
        db.finish_download(second, gone).await.unwrap().unwrap();
        let name: String = sqlx::query_scalar("SELECT file FROM attachments WHERE id = ?")
            .bind(second.get())
            .fetch_one(db.pool())
            .await
            .unwrap();
        db.files.remove(&name).unwrap();
        let orphan = db.files().store(b"orphan".to_vec()).await.unwrap();
        db.start_download(third).await.unwrap().unwrap();

        assert_eq!(db.check_files().await.unwrap(), [third]);

        assert_eq!(files_on_disk(&db).await, 1);
        assert!(!db.files.list().unwrap().contains(&orphan.name));
        let checked = db.message(added.id).await.unwrap().unwrap();
        let states: Vec<_> = attachments(&checked).iter().map(|a| a.state).collect();
        assert_eq!(
            states,
            [
                AttachmentState::Ready,
                AttachmentState::Remote,
                AttachmentState::Remote
            ]
        );
    }

    fn video(name: &str) -> Attachment {
        Attachment {
            kind: AttachmentKind::Video,
            ..remote(name)
        }
    }

    #[tokio::test]
    async fn downloaded_videos_want_a_first_frame() {
        let (_dir, db) = db().await;
        let content = with_attachments("", vec![video("v"), remote("i")]);
        let added = incoming(&db, 1, &content).await;
        let (video, image) = (id(&added, 0), id(&added, 1));

        assert!(
            db.pending_previews().await.unwrap().is_empty(),
            "not downloaded"
        );
        download(&db, &added, b"mp4").await;
        db.start_download(image).await.unwrap().unwrap();
        let file = db.files().store(b"jpg".to_vec()).await.unwrap();
        db.finish_download(image, file).await.unwrap();

        assert_eq!(db.pending_previews().await.unwrap(), [video]);
        assert!(db.preview_wanted(video).await.unwrap());
        assert!(!db.preview_wanted(image).await.unwrap());
        assert_eq!(db.preview_contents(video).await.unwrap(), None);

        let frame = db.files().store(b"png".to_vec()).await.unwrap();
        assert_eq!(db.set_preview(video, frame).await.unwrap(), Some(added.id));

        let shown = db.message(added.id).await.unwrap().unwrap();
        assert!(attachments(&shown)[0].has_preview);
        assert!(!attachments(&shown)[1].has_preview);
        assert_eq!(
            db.preview_contents(video).await.unwrap().as_deref(),
            Some(b"png".as_slice())
        );
        assert!(db.pending_previews().await.unwrap().is_empty());
        // Not wanted any more: the copy goes.
        let again = db.files().store(b"png".to_vec()).await.unwrap();
        assert_eq!(db.set_preview(video, again).await.unwrap(), None);
        assert_eq!(files_on_disk(&db).await, 3);
    }

    #[tokio::test]
    async fn failed_frames_arent_tried_again() {
        let (_dir, db) = db().await;
        let added = incoming(&db, 1, &with_attachments("", vec![video("v")])).await;
        download(&db, &added, b"mp4").await;

        db.preview_failed(id(&added, 0)).await.unwrap();

        assert!(db.pending_previews().await.unwrap().is_empty());
        let shown = db.message(added.id).await.unwrap().unwrap();
        assert!(!attachments(&shown)[0].has_preview);
    }

    #[tokio::test]
    async fn frames_go_with_their_video_and_come_back_if_missing() {
        let (_dir, db) = db().await;
        let content = with_attachments("", vec![video("v")]);
        let added = incoming(&db, 1, &content).await;
        download(&db, &added, b"mp4").await;
        let frame = db.files().store(b"png".to_vec()).await.unwrap();
        let name = frame.name.clone();
        db.set_preview(id(&added, 0), frame).await.unwrap();

        db.files.remove(&name).unwrap();
        db.check_files().await.unwrap();
        assert_eq!(db.pending_previews().await.unwrap(), [id(&added, 0)]);

        let frame = db.files().store(b"png".to_vec()).await.unwrap();
        db.set_preview(id(&added, 0), frame).await.unwrap();
        assert_eq!(files_on_disk(&db).await, 2);
        db.delete_messages(&[MessageRef {
            chat: ChatRef::Contact(peer(1)),
            author: Some(peer(1)),
            sent_at: at(1),
        }])
        .await
        .unwrap();
        assert_eq!(files_on_disk(&db).await, 0);
    }
}
