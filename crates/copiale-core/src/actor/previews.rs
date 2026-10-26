//! Videos' first frames, from the sandboxed helper, one at a time.

use std::collections::VecDeque;

use tokio::task::JoinHandle;

use crate::model::AttachmentId;
use crate::storage::{Files, StoredFile};
use crate::video::FrameHelper;

pub(super) struct Previews {
    /// `None` where there's no helper: frames stay wanted.
    helper: Option<FrameHelper>,
    files: Files,
    queue: VecDeque<AttachmentId>,
    running: Option<JoinHandle<Made>>,
}

/// A frame made, or why not.
pub(super) struct Made {
    pub id: AttachmentId,
    pub result: Result<StoredFile, Failure>,
}

pub(super) enum Failure {
    /// The helper gave no frame: not a video it decodes.
    Helper(std::io::Error),
    /// Writing the frame failed.
    Storage(std::io::Error),
}

impl Previews {
    pub(super) fn new(helper: Option<FrameHelper>, files: Files) -> Self {
        Self {
            helper,
            files,
            queue: VecDeque::new(),
            running: None,
        }
    }

    /// Queues a video, unless there's no helper or it's queued.
    pub(super) fn push(&mut self, id: AttachmentId) {
        if self.helper.is_some() && !self.queue.contains(&id) {
            self.queue.push_back(id);
        }
    }

    /// The next video to make a frame of, if none is being made.
    pub(super) fn next(&mut self) -> Option<AttachmentId> {
        if self.running.is_some() {
            return None;
        }
        self.queue.pop_front()
    }

    /// Makes `id`'s frame from `video` in the background.
    pub(super) fn start(&mut self, id: AttachmentId, video: Vec<u8>) {
        let Some(helper) = self.helper.clone() else {
            return;
        };
        let files = self.files.clone();
        self.running = Some(tokio::task::spawn_local(async move {
            let result = match helper.first_frame(video).await {
                Ok(frame) => files.store(frame).await.map_err(Failure::Storage),
                Err(error) => Err(Failure::Helper(error)),
            };
            Made { id, result }
        }));
    }

    /// The frame being made, once it is.
    pub(super) async fn finished(&mut self) -> Option<Made> {
        let Some(running) = &mut self.running else {
            return futures::future::pending().await;
        };
        let made = running.await;
        self.running = None;
        made.ok()
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        if let Some(running) = &self.running {
            running.abort();
        }
    }
}
