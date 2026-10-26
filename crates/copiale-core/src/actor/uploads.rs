//! Our messages' attachments, uploaded a few at a time while connected;
//! a message joins the outbox once all of its are up.

use std::collections::VecDeque;

use futures::FutureExt as _;
use tokio::task::JoinHandle;

use crate::backend::{BackendError, SignalBackend};
use crate::model::{Attachment, AttachmentId, MessageId, RemoteAttachment};
use crate::storage::{Files, StoredFile};

/// Uploads running at once.
const MAX_RUNNING: usize = 2;

pub(super) struct Uploads {
    files: Files,
    queue: VecDeque<Job>,
    running: Vec<Running>,
    connected: bool,
}

pub(super) struct Job {
    pub message: MessageId,
    pub id: AttachmentId,
    /// What recipients are told of it.
    pub attachment: Attachment,
    pub file: StoredFile,
}

struct Running {
    message: MessageId,
    id: AttachmentId,
    task: JoinHandle<Result<RemoteAttachment, BackendError>>,
}

/// An upload that ended, and how.
pub(super) struct Finished {
    pub message: MessageId,
    pub id: AttachmentId,
    pub result: Result<RemoteAttachment, BackendError>,
}

impl Uploads {
    pub(super) fn new(files: Files) -> Self {
        Self {
            files,
            queue: VecDeque::new(),
            running: Vec::new(),
            connected: false,
        }
    }

    /// Queues an upload, which starts once connected.
    pub(super) fn push(&mut self, job: Job, backend: &mut impl SignalBackend) {
        self.queue.push_back(job);
        self.start(backend);
    }

    pub(super) fn connected(&mut self, backend: &mut impl SignalBackend) {
        self.connected = true;
        self.start(backend);
    }

    /// Running uploads go on; no more start.
    pub(super) fn disconnected(&mut self) {
        self.connected = false;
    }

    /// Drops `message`'s uploads, queued or running.
    pub(super) fn cancel(&mut self, message: MessageId) {
        self.queue.retain(|job| job.message != message);
        self.running.retain(|running| {
            let keep = running.message != message;
            if !keep {
                running.task.abort();
            }
            keep
        });
    }

    /// Whether any of `message`'s attachments are still to upload.
    pub(super) fn uploading(&self, message: MessageId) -> bool {
        self.queue.iter().any(|job| job.message == message)
            || self
                .running
                .iter()
                .any(|running| running.message == message)
    }

    /// The next upload to end, once one does; then more may start.
    pub(super) async fn finished(&mut self) -> Finished {
        if self.running.is_empty() {
            return futures::future::pending().await;
        }
        let tasks = self.running.iter_mut().map(|running| &mut running.task);
        let (result, index, _) = futures::future::select_all(tasks).await;
        let running = self.running.swap_remove(index);
        Finished {
            message: running.message,
            id: running.id,
            result: result.unwrap_or_else(|error| Err(BackendError::new(error.to_string()))),
        }
    }

    pub(super) fn start(&mut self, backend: &mut impl SignalBackend) {
        while self.connected && self.running.len() < MAX_RUNNING {
            let Some(job) = self.queue.pop_front() else {
                return;
            };
            let files = self.files.clone();
            let contents = async move { files.load(job.file).await }.boxed_local();
            let upload = backend.upload(job.attachment, contents);
            self.running.push(Running {
                message: job.message,
                id: job.id,
                task: tokio::task::spawn_local(upload),
            });
        }
    }
}

impl Drop for Uploads {
    fn drop(&mut self) {
        for running in &self.running {
            running.task.abort();
        }
    }
}
