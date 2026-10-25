//! Attachment downloads, a few at a time while connected, and which
//! attachments download on their own.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::backend::{BackendError, SignalBackend};
use crate::model::{
    Attachment, AttachmentId, AttachmentKind, AttachmentState, RemoteAttachment, ServiceId,
};
use crate::storage::{Files, StoredFile};

/// Downloads running at once.
const MAX_RUNNING: usize = 2;
/// Largest image downloaded without asking.
const AUTO_MAX_SIZE: u32 = 10 * 1024 * 1024;
/// Most attachments one sender's messages download without asking per
/// [`AUTO_WINDOW`].
const AUTO_PER_SENDER: usize = 50;
const AUTO_WINDOW: Duration = Duration::from_secs(3600);
/// Senders tracked before idle ones are dropped.
const MAX_TRACKED: usize = 256;

pub(super) struct Downloads {
    files: Files,
    queue: VecDeque<Job>,
    /// Cut off by a network error; they go again on the next connection.
    waiting: Vec<Job>,
    running: FuturesUnordered<JoinHandle<Finished>>,
    connected: bool,
    /// When each sender's messages last started downloads on their own.
    auto: HashMap<ServiceId, VecDeque<Instant>>,
}

pub(super) struct Job {
    pub id: AttachmentId,
    pub remote: RemoteAttachment,
}

/// A download that ended, and how.
pub(super) struct Finished {
    pub job: Job,
    pub result: Result<StoredFile, Failure>,
}

pub(super) enum Failure {
    Backend(BackendError),
    /// Downloaded, but writing it failed.
    Storage(std::io::Error),
}

impl Downloads {
    pub(super) fn new(files: Files) -> Self {
        Self {
            files,
            queue: VecDeque::new(),
            waiting: Vec::new(),
            running: FuturesUnordered::new(),
            connected: false,
            auto: HashMap::new(),
        }
    }

    /// Queues a download, which starts once connected.
    pub(super) fn push(&mut self, job: Job, backend: &mut impl SignalBackend) {
        self.queue.push_back(job);
        self.start(backend);
    }

    /// Queues a download cut off by a network error for the next
    /// connection.
    pub(super) fn wait(&mut self, job: Job) {
        self.waiting.push(job);
    }

    pub(super) fn connected(&mut self, backend: &mut impl SignalBackend) {
        self.connected = true;
        for job in self.waiting.drain(..).rev() {
            self.queue.push_front(job);
        }
        self.start(backend);
    }

    /// Running downloads go on; no more start.
    pub(super) fn disconnected(&mut self) {
        self.connected = false;
    }

    /// The next download to end, once one does; then more may start.
    pub(super) async fn finished(&mut self) -> Finished {
        loop {
            match self.running.next().await {
                Some(Ok(finished)) => return finished,
                // Left downloading until the next start.
                Some(Err(error)) => tracing::error!(%error, "a download task failed"),
                None => futures::future::pending().await,
            }
        }
    }

    pub(super) fn start(&mut self, backend: &mut impl SignalBackend) {
        while self.connected && self.running.len() < MAX_RUNNING {
            let Some(job) = self.queue.pop_front() else {
                return;
            };
            let download = backend.download(job.remote.clone());
            let files = self.files.clone();
            self.running.push(tokio::task::spawn_local(async move {
                let result = match download.await {
                    Ok(contents) => files.store(contents).await.map_err(Failure::Storage),
                    Err(error) => Err(Failure::Backend(error)),
                };
                Finished { job, result }
            }));
        }
    }

    /// Whether a message by `sender` (`None` for this account) downloads
    /// `attachment` without asking: images and GIFs up to
    /// [`AUTO_MAX_SIZE`], up to [`AUTO_PER_SENDER`] per window from others.
    /// Counts it if so.
    pub(super) fn auto(
        &mut self,
        attachment: &Attachment,
        sender: Option<ServiceId>,
        now: Instant,
    ) -> bool {
        let wanted = attachment.state == AttachmentState::Remote
            && matches!(attachment.kind, AttachmentKind::Image | AttachmentKind::Gif)
            && attachment.size.is_some_and(|size| size <= AUTO_MAX_SIZE);
        let Some(sender) = sender.filter(|_| wanted) else {
            return wanted;
        };
        let fresh = |at: &Instant| now.duration_since(*at) < AUTO_WINDOW;
        if self.auto.len() > MAX_TRACKED {
            self.auto
                .retain(|_, recent| recent.back().is_some_and(fresh));
        }
        let recent = self.auto.entry(sender).or_default();
        while recent.front().is_some_and(|at| !fresh(at)) {
            recent.pop_front();
        }
        if recent.len() >= AUTO_PER_SENDER {
            return false;
        }
        recent.push_back(now);
        true
    }
}

impl Drop for Downloads {
    fn drop(&mut self) {
        for task in self.running.iter() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn downloads() -> (tempfile::TempDir, Downloads) {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::beside(&dir.path().join("test.db"));
        (dir, Downloads::new(files))
    }

    fn image(size: u32) -> Attachment {
        Attachment {
            size: Some(size),
            state: AttachmentState::Remote,
            ..Attachment::new(AttachmentKind::Image)
        }
    }

    fn sender(n: u128) -> Option<ServiceId> {
        Some(ServiceId::new(Uuid::from_u128(n)))
    }

    #[test]
    fn only_small_remote_images_download_on_their_own() {
        let (_dir, mut downloads) = downloads();
        let now = Instant::now();
        let gif = Attachment {
            kind: AttachmentKind::Gif,
            ..image(100)
        };
        let video = Attachment {
            kind: AttachmentKind::Video,
            ..image(100)
        };
        let no_size = Attachment {
            size: None,
            ..image(100)
        };
        let here = Attachment {
            state: AttachmentState::Ready,
            ..image(100)
        };

        assert!(downloads.auto(&image(AUTO_MAX_SIZE), sender(1), now));
        assert!(downloads.auto(&gif, sender(1), now));
        assert!(!downloads.auto(&image(AUTO_MAX_SIZE + 1), sender(1), now));
        assert!(!downloads.auto(&video, sender(1), now));
        assert!(!downloads.auto(&no_size, sender(1), now));
        assert!(!downloads.auto(&here, None, now));
    }

    #[test]
    fn each_sender_starts_only_so_many_an_hour() {
        let (_dir, mut downloads) = downloads();
        let now = Instant::now();
        for _ in 0..AUTO_PER_SENDER {
            assert!(downloads.auto(&image(100), sender(1), now));
        }

        assert!(!downloads.auto(&image(100), sender(1), now));
        assert!(downloads.auto(&image(100), sender(2), now));
        // Ours aren't limited.
        assert!(downloads.auto(&image(100), None, now));
        assert!(downloads.auto(&image(100), sender(1), now + AUTO_WINDOW));
    }
}
