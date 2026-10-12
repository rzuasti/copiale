//! In-memory chat data: the fixture dataset plus sent and live messages.

use fixtures::{Conversation, ConversationId, Dataset, Message, MessageId, NOW_MS, Sender};
use gpui_kit::{Context, EventEmitter};

use crate::feed::FeedHandle;

pub enum StoreEvent {
    /// Messages were appended to the end of `conversation`.
    Appended {
        conversation: ConversationId,
        count: usize,
    },
}

pub struct ChatStore {
    dataset: Dataset,
    next_id: u64,
    clock_ms: i64,
    /// `None` in tests: GPUI's test scheduler rejects wakeups from other threads.
    feed: Option<FeedHandle>,
}

impl EventEmitter<StoreEvent> for ChatStore {}

impl ChatStore {
    /// Store with the simulated live feed available (paused).
    pub fn new(dataset: Dataset, cx: &mut Context<Self>) -> Self {
        let mut store = Self::without_feed(dataset);
        store.feed = Some(store.spawn_feed(cx));
        store
    }

    pub fn without_feed(dataset: Dataset) -> Self {
        let newest = dataset
            .conversations
            .iter()
            .filter_map(Conversation::last_message)
            .max_by_key(|m| m.id);
        let next_id = newest.map_or(1, |m| m.id.0 + 1);
        let clock_ms = newest.map_or(NOW_MS, |m| m.sent_at_ms.max(NOW_MS));
        Self {
            dataset,
            next_id,
            clock_ms,
            feed: None,
        }
    }

    fn spawn_feed(&self, cx: &mut Context<Self>) -> FeedHandle {
        let (feed, mut messages) = FeedHandle::spawn(&self.dataset, 7);
        cx.spawn(async move |this, cx| {
            while let Some(first) = messages.recv().await {
                // Drain whatever else arrived so a burst costs one update.
                let mut batch = vec![first];
                while let Ok(message) = messages.try_recv() {
                    batch.push(message);
                }
                if this
                    .update(cx, |store, cx| store.receive(batch, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        feed
    }

    pub fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    pub fn conversation(&self, id: ConversationId) -> &Conversation {
        self.dataset
            .conversation(id)
            .expect("conversation ids come from the dataset")
    }

    pub fn feed_target(&self) -> Option<ConversationId> {
        self.feed.as_ref().and_then(FeedHandle::target)
    }

    pub fn set_feed_target(&mut self, target: Option<ConversationId>, cx: &mut Context<Self>) {
        if let Some(feed) = &self.feed {
            feed.set_target(target);
            cx.notify();
        }
    }

    /// Appends an outgoing message.
    pub fn send(&mut self, conversation: ConversationId, body: String, cx: &mut Context<Self>) {
        let sent_at_ms = self.clock_ms + 1000;
        let message = Message {
            id: MessageId(0),
            conversation,
            sender: Sender::Me,
            sent_at_ms,
            body,
            image: None,
            reply_to: None,
            reactions: Vec::new(),
        };
        self.append(conversation, vec![message], cx);
    }

    fn receive(&mut self, messages: Vec<Message>, cx: &mut Context<Self>) {
        // Batches normally target one conversation; split if the target changed.
        let mut start = 0;
        while start < messages.len() {
            let conversation = messages[start].conversation;
            let end = messages[start..]
                .iter()
                .position(|m| m.conversation != conversation)
                .map_or(messages.len(), |offset| start + offset);
            self.append(conversation, messages[start..end].to_vec(), cx);
            start = end;
        }
    }

    fn append(
        &mut self,
        conversation: ConversationId,
        messages: Vec<Message>,
        cx: &mut Context<Self>,
    ) {
        let count = messages.len();
        let target = self
            .dataset
            .conversations
            .iter_mut()
            .find(|c| c.id == conversation)
            .expect("conversation ids come from the dataset");
        for mut message in messages {
            // Store-assigned ids keep `messages` sorted for binary search.
            message.id = MessageId(self.next_id);
            self.next_id += 1;
            self.clock_ms = self.clock_ms.max(message.sent_at_ms);
            target.messages.push(message);
        }
        cx.emit(StoreEvent::Appended {
            conversation,
            count,
        });
        cx.notify();
    }
}
