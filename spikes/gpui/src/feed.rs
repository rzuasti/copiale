//! Simulated incoming messages produced on a tokio runtime and bridged to
//! GPUI: the pattern presage's receive loop will need.

use std::thread;
use std::time::Duration;

use fixtures::{ConversationId, Dataset, Message};
use tokio::sync::{mpsc, watch};

/// Delay between generated messages: 50 msg/s.
pub const INTERVAL: Duration = Duration::from_millis(20);

/// Controls a feed running on its own thread; dropping it stops the feed.
pub struct FeedHandle {
    target: watch::Sender<Option<ConversationId>>,
}

impl FeedHandle {
    /// Starts the feed paused; returns the handle and the message stream.
    pub fn spawn(dataset: &Dataset, seed: u64) -> (Self, mpsc::UnboundedReceiver<Message>) {
        let mut feed = dataset.live_feed(seed);
        let (target_tx, mut target_rx) = watch::channel(None);
        let (message_tx, message_rx) = mpsc::unbounded_channel();

        thread::Builder::new()
            .name("live-feed".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("tokio runtime");
                runtime.block_on(async move {
                    let mut interval = tokio::time::interval(INTERVAL);
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        let target = *target_rx.borrow_and_update();
                        let Some(conversation) = target else {
                            // Paused: wait for a target or for the handle to drop.
                            if target_rx.changed().await.is_err() {
                                return;
                            }
                            continue;
                        };
                        tokio::select! {
                            _ = interval.tick() => {
                                if message_tx.send(feed.next(conversation)).is_err() {
                                    return;
                                }
                            }
                            changed = target_rx.changed() => {
                                if changed.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            })
            .expect("spawn feed thread");

        (Self { target: target_tx }, message_rx)
    }

    /// Sends messages to `conversation`, or pauses with `None`.
    pub fn set_target(&self, conversation: Option<ConversationId>) {
        self.target.send_replace(conversation);
    }

    pub fn target(&self) -> Option<ConversationId> {
        *self.target.borrow()
    }
}
