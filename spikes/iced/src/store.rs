//! In-memory chat data: the fixture dataset plus sent and live messages.

use fixtures::{Conversation, ConversationId, Dataset, Message, MessageId, NOW_MS, Sender};

pub struct ChatStore {
    dataset: Dataset,
    next_id: u64,
    clock_ms: i64,
}

impl ChatStore {
    pub fn new(dataset: Dataset) -> Self {
        let newest = dataset
            .conversations
            .iter()
            .filter_map(Conversation::last_message)
            .max_by_key(|m| m.id);
        Self {
            next_id: newest.map_or(1, |m| m.id.0 + 1),
            clock_ms: newest.map_or(NOW_MS, |m| m.sent_at_ms.max(NOW_MS)),
            dataset,
        }
    }

    pub fn dataset(&self) -> &Dataset {
        &self.dataset
    }

    pub fn conversation(&self, id: ConversationId) -> &Conversation {
        self.dataset
            .conversation(id)
            .expect("conversation ids come from the dataset")
    }

    /// Appends an outgoing message.
    pub fn send(&mut self, conversation: ConversationId, body: String) {
        let message = Message {
            id: MessageId(0),
            conversation,
            sender: Sender::Me,
            sent_at_ms: self.clock_ms + 1000,
            body,
            image: None,
            reply_to: None,
            reactions: Vec::new(),
        };
        self.append(vec![message]);
    }

    /// Appends messages; returns `(conversation, count)` per run of messages.
    pub fn append(&mut self, messages: Vec<Message>) -> Vec<(ConversationId, usize)> {
        let mut runs: Vec<(ConversationId, usize)> = Vec::new();
        for mut message in messages {
            let conversation = message.conversation;
            // Store-assigned ids keep `messages` sorted for binary search.
            message.id = MessageId(self.next_id);
            self.next_id += 1;
            self.clock_ms = self.clock_ms.max(message.sent_at_ms);
            self.dataset
                .conversations
                .iter_mut()
                .find(|c| c.id == conversation)
                .expect("conversation ids come from the dataset")
                .messages
                .push(message);
            match runs.last_mut() {
                Some((id, count)) if *id == conversation => *count += 1,
                _ => runs.push((conversation, 1)),
            }
        }
        runs
    }
}
