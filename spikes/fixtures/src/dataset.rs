use std::cmp::Reverse;

use crate::color::Rgb;
use crate::model::{
    Contact, ContactId, Conversation, ConversationId, ConversationKind, ImageAttachment, Message,
    MessageId, Reaction, Sender,
};
use crate::rng::Rng;
use crate::text;

pub const DEFAULT_SEED: u64 = 0xC0_91A1E;

/// Fixed "now" so timestamps are deterministic: 2026-10-01T12:00:00Z.
pub const NOW_MS: i64 = 1_790_856_000_000;

const SECOND_MS: i64 = 1000;
const MINUTE_MS: i64 = 60 * SECOND_MS;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

const IMAGE_RATE: f64 = 0.05;
const REPLY_RATE: f64 = 0.08;
const REACTION_RATE: f64 = 0.10;
/// Chance the next message has the same sender (consecutive-message grouping).
const SAME_SENDER_RATE: f64 = 0.55;
/// Share of messages in the first conversation (long-list stress test).
const HEAVY_SHARE: f64 = 0.4;
/// Interval between live feed messages (50 msg/s).
const LIVE_INTERVAL_MS: i64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub seed: u64,
    pub contacts: usize,
    pub conversations: usize,
    pub messages: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            contacts: text::CONTACT_NAMES.len(),
            conversations: 20,
            messages: 10_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dataset {
    pub contacts: Vec<Contact>,
    /// Most recently active first.
    pub conversations: Vec<Conversation>,
}

impl Dataset {
    /// The spike dataset: 10k messages across 20 conversations.
    pub fn standard() -> Self {
        Self::generate(&Config::default())
    }

    pub fn generate(config: &Config) -> Self {
        assert!(config.contacts >= 3, "groups need at least 3 contacts");
        assert!(config.conversations >= 1);

        let mut rng = Rng::new(config.seed);
        let contacts = generate_contacts(&mut rng, config.contacts);
        let mut conversations = generate_conversations(&mut rng, &contacts, config.conversations);
        let counts = split_messages(&mut rng, config.messages, conversations.len());

        let drafts: Vec<Vec<Draft>> = conversations
            .iter()
            .zip(counts)
            .enumerate()
            .map(|(index, (conversation, count))| {
                let mut rng = rng.fork();
                // The heavy conversation is the most recent, so prototypes
                // open on it.
                let end_ms = if index == 0 {
                    NOW_MS
                } else {
                    NOW_MS - rng.range(MINUTE_MS as u64, 3 * DAY_MS as u64) as i64
                };
                generate_drafts(&mut rng, conversation, count, end_ms)
            })
            .collect();
        assign_messages(&mut conversations, drafts);
        conversations.sort_by_key(|c| Reverse(c.last_message().map(|m| m.sent_at_ms)));

        Self {
            contacts,
            conversations,
        }
    }

    pub fn contact(&self, id: ContactId) -> Option<&Contact> {
        self.contacts.get(id.0 as usize)
    }

    pub fn conversation(&self, id: ConversationId) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.id == id)
    }

    /// Conversation containing `id` and the message's index in it.
    pub fn find_message(&self, id: MessageId) -> Option<(&Conversation, usize)> {
        self.conversations
            .iter()
            .find_map(|c| c.position(id).map(|index| (c, index)))
    }

    pub fn sender_name(&self, sender: Sender) -> &str {
        match sender {
            Sender::Me => "You",
            Sender::Contact(id) => self.contact(id).map_or("Unknown", |c| c.name.as_str()),
        }
    }

    pub fn message_count(&self) -> usize {
        self.conversations.iter().map(|c| c.messages.len()).sum()
    }

    /// Simulated incoming messages that continue after the dataset's newest
    /// message.
    pub fn live_feed(&self, seed: u64) -> LiveFeed {
        let newest = self
            .conversations
            .iter()
            .filter_map(Conversation::last_message)
            .max_by_key(|m| m.id);
        LiveFeed {
            rng: Rng::new(seed),
            next_id: newest.map_or(1, |m| m.id.0 + 1),
            clock_ms: newest.map_or(NOW_MS, |m| m.sent_at_ms.max(NOW_MS)),
            targets: self
                .conversations
                .iter()
                .map(|c| (c.id, c.members.clone()))
                .collect(),
        }
    }
}

/// Generates incoming messages, one every 20 ms of simulated time. `Send`, so
/// it can live in an async task.
#[derive(Debug, Clone)]
pub struct LiveFeed {
    rng: Rng,
    next_id: u64,
    clock_ms: i64,
    targets: Vec<(ConversationId, Vec<ContactId>)>,
}

impl LiveFeed {
    /// Next incoming message for `conversation`, from one of its members.
    ///
    /// Panics if `conversation` is not in the dataset.
    pub fn next(&mut self, conversation: ConversationId) -> Message {
        let (_, members) = self
            .targets
            .iter()
            .find(|(id, _)| *id == conversation)
            .expect("conversation is in the dataset");
        let sender = Sender::Contact(*self.rng.pick(members));
        let image = self
            .rng
            .chance(IMAGE_RATE)
            .then(|| ImageAttachment::random(&mut self.rng));
        let body = text::body(&mut self.rng);
        self.clock_ms += LIVE_INTERVAL_MS;
        let id = MessageId(self.next_id);
        self.next_id += 1;
        Message {
            id,
            conversation,
            sender,
            sent_at_ms: self.clock_ms,
            body,
            image,
            reply_to: None,
            reactions: Vec::new(),
        }
    }
}

/// A message before global ids are assigned.
struct Draft {
    sender: Sender,
    sent_at_ms: i64,
    body: String,
    image: Option<ImageAttachment>,
    /// Index of an earlier draft in the same conversation.
    reply_to: Option<usize>,
    reactions: Vec<Reaction>,
}

fn generate_contacts(rng: &mut Rng, count: usize) -> Vec<Contact> {
    (0..count)
        .map(|i| {
            let base = text::CONTACT_NAMES[i % text::CONTACT_NAMES.len()];
            let name = match i / text::CONTACT_NAMES.len() {
                0 => base.to_string(),
                round => format!("{base} {}", round + 1),
            };
            Contact {
                id: ContactId(i as u32),
                name,
                color: Rgb::from_hsv(rng.range(0, 360) as f32, 0.5, 0.8),
            }
        })
        .collect()
}

/// About 70% direct chats, the rest groups.
fn generate_conversations(rng: &mut Rng, contacts: &[Contact], count: usize) -> Vec<Conversation> {
    let mut direct_partners: Vec<&Contact> = contacts.iter().collect();
    rng.shuffle(&mut direct_partners);
    let direct_count = (count * 7 / 10).min(contacts.len());

    (0..count)
        .map(|i| {
            let id = ConversationId(i as u32);
            if i < direct_count {
                let partner = direct_partners[i];
                Conversation {
                    id,
                    kind: ConversationKind::Direct,
                    title: partner.name.clone(),
                    members: vec![partner.id],
                    messages: Vec::new(),
                }
            } else {
                let group_index = i - direct_count;
                let base = text::GROUP_NAMES[group_index % text::GROUP_NAMES.len()];
                let title = match group_index / text::GROUP_NAMES.len() {
                    0 => base.to_string(),
                    round => format!("{base} {}", round + 1),
                };
                let mut members: Vec<ContactId> = contacts.iter().map(|c| c.id).collect();
                rng.shuffle(&mut members);
                let size = rng.range(2, contacts.len().min(12) as u64 + 1) as usize;
                members.truncate(size);
                members.sort();
                Conversation {
                    id,
                    kind: ConversationKind::Group,
                    title,
                    members,
                    messages: Vec::new(),
                }
            }
        })
        .collect()
}

/// Splits `total` across conversations: the first gets `HEAVY_SHARE`, the
/// rest random weights.
fn split_messages(rng: &mut Rng, total: usize, conversations: usize) -> Vec<usize> {
    if conversations == 1 {
        return vec![total];
    }
    let heavy = (total as f64 * HEAVY_SHARE) as usize;
    let rest = (total - heavy) as u64;
    let weights: Vec<u64> = (1..conversations).map(|_| rng.range(1, 10)).collect();
    let weight_sum: u64 = weights.iter().sum();

    let mut counts = vec![heavy];
    counts.extend(weights.iter().map(|w| (rest * w / weight_sum) as usize));
    let assigned: usize = counts.iter().sum();
    counts[0] += total - assigned;
    counts
}

fn generate_drafts(
    rng: &mut Rng,
    conversation: &Conversation,
    count: usize,
    end_ms: i64,
) -> Vec<Draft> {
    // Walk backwards from the newest message.
    let mut time = end_ms;
    let mut times: Vec<i64> = (0..count)
        .map(|_| {
            let sent_at = time;
            time -= gap(rng);
            sent_at
        })
        .collect();
    times.reverse();

    let mut previous_sender = None;
    times
        .into_iter()
        .enumerate()
        .map(|(index, sent_at_ms)| {
            let sender = match previous_sender {
                Some(sender) if rng.chance(SAME_SENDER_RATE) => sender,
                _ => pick_sender(rng, conversation),
            };
            previous_sender = Some(sender);

            let image = rng.chance(IMAGE_RATE).then(|| ImageAttachment::random(rng));
            let body = if image.is_some() && rng.chance(0.5) {
                String::new()
            } else {
                text::body(rng)
            };
            // Reply to one of the previous 50 messages.
            let reply_to =
                (index > 0 && rng.chance(REPLY_RATE)).then(|| index - 1 - rng.index(index.min(50)));
            let reactions = if rng.chance(REACTION_RATE) {
                text::reactions(rng, conversation.members.len() as u32 + 1)
            } else {
                Vec::new()
            };

            Draft {
                sender,
                sent_at_ms,
                body,
                image,
                reply_to,
                reactions,
            }
        })
        .collect()
}

/// Bursty gaps: mostly seconds to minutes, occasionally days.
fn gap(rng: &mut Rng) -> i64 {
    let (low, high) = *rng.weighted(&[
        ((2 * SECOND_MS, 3 * MINUTE_MS), 60),
        ((3 * MINUTE_MS, 4 * HOUR_MS), 30),
        ((4 * HOUR_MS, 36 * HOUR_MS), 8),
        ((2 * DAY_MS, 14 * DAY_MS), 2),
    ]);
    rng.range(low as u64, high as u64) as i64
}

fn pick_sender(rng: &mut Rng, conversation: &Conversation) -> Sender {
    let me_rate = match conversation.kind {
        ConversationKind::Direct => 0.45,
        ConversationKind::Group => 0.2,
    };
    if rng.chance(me_rate) {
        Sender::Me
    } else {
        Sender::Contact(*rng.pick(&conversation.members))
    }
}

/// Assigns ids in global time order and resolves reply indices to ids.
fn assign_messages(conversations: &mut [Conversation], drafts: Vec<Vec<Draft>>) {
    let mut order: Vec<(i64, usize, usize)> = drafts
        .iter()
        .enumerate()
        .flat_map(|(conversation, drafts)| {
            drafts
                .iter()
                .enumerate()
                .map(move |(index, draft)| (draft.sent_at_ms, conversation, index))
        })
        .collect();
    order.sort_unstable();

    let mut ids: Vec<Vec<MessageId>> = drafts.iter().map(|d| vec![MessageId(0); d.len()]).collect();
    for (next_id, (_, conversation, index)) in order.into_iter().enumerate() {
        ids[conversation][index] = MessageId(next_id as u64 + 1);
    }

    for ((conversation, drafts), ids) in conversations.iter_mut().zip(drafts).zip(ids) {
        conversation.messages = drafts
            .into_iter()
            .zip(&ids)
            .map(|(draft, id)| Message {
                id: *id,
                conversation: conversation.id,
                sender: draft.sender,
                sent_at_ms: draft.sent_at_ms,
                body: draft.body,
                image: draft.image,
                reply_to: draft.reply_to.map(|index| ids[index]),
                reactions: draft.reactions,
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn standard_shape() {
        let dataset = Dataset::standard();
        assert_eq!(dataset.conversations.len(), 20);
        assert_eq!(dataset.message_count(), 10_000);
        let groups = dataset
            .conversations
            .iter()
            .filter(|c| c.kind == ConversationKind::Group)
            .count();
        assert_eq!(groups, 6);
        let busiest = dataset.conversations.iter().map(|c| c.messages.len()).max();
        assert_eq!(dataset.conversations[0].messages.len(), busiest.unwrap());
    }

    #[test]
    fn deterministic() {
        assert_eq!(Dataset::standard(), Dataset::standard());
        let other = Dataset::generate(&Config {
            seed: 1,
            ..Config::default()
        });
        assert_ne!(Dataset::standard(), other);
    }

    #[test]
    fn ids_unique_and_ordered() {
        let dataset = Dataset::standard();
        let mut seen = HashSet::new();
        for conversation in &dataset.conversations {
            for pair in conversation.messages.windows(2) {
                assert!(pair[0].id < pair[1].id);
                assert!(pair[0].sent_at_ms < pair[1].sent_at_ms);
            }
            for message in &conversation.messages {
                assert!(seen.insert(message.id));
                assert_eq!(message.conversation, conversation.id);
            }
        }
    }

    #[test]
    fn conversations_sorted_by_activity() {
        let dataset = Dataset::standard();
        let last: Vec<i64> = dataset
            .conversations
            .iter()
            .map(|c| c.last_message().unwrap().sent_at_ms)
            .collect();
        assert!(last.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn replies_point_to_earlier_messages_in_same_conversation() {
        let dataset = Dataset::standard();
        let mut replies = 0;
        for conversation in &dataset.conversations {
            for (index, message) in conversation.messages.iter().enumerate() {
                if let Some(target) = message.reply_to {
                    replies += 1;
                    assert!(conversation.position(target).unwrap() < index);
                }
            }
        }
        assert!(replies > 0);
    }

    #[test]
    fn senders_are_members() {
        let dataset = Dataset::standard();
        for conversation in &dataset.conversations {
            for message in &conversation.messages {
                if let Sender::Contact(id) = message.sender {
                    assert!(conversation.members.contains(&id));
                }
            }
        }
    }

    #[test]
    fn find_message_round_trip() {
        let dataset = Dataset::standard();
        let target = &dataset.conversations[3].messages[7];
        let (conversation, index) = dataset.find_message(target.id).unwrap();
        assert_eq!(&conversation.messages[index], target);
        assert!(dataset.find_message(MessageId(u64::MAX)).is_none());
    }

    #[test]
    fn covers_test_scripts() {
        let dataset = Dataset::standard();
        let bodies: Vec<&str> = dataset
            .conversations
            .iter()
            .flat_map(|c| c.messages.iter().map(|m| m.body.as_str()))
            .collect();
        let has = |predicate: fn(char) -> bool| bodies.iter().any(|b| b.chars().any(predicate));
        assert!(
            has(|c| ('\u{3040}'..='\u{30FF}').contains(&c)),
            "Japanese kana"
        );
        assert!(has(|c| ('\u{0600}'..='\u{06FF}').contains(&c)), "Arabic");
        assert!(has(|c| ('\u{0590}'..='\u{05FF}').contains(&c)), "Hebrew");
        assert!(has(|c| c == '\u{200D}'), "ZWJ emoji");
        assert!(
            bodies.iter().any(|b| b.len() > 200 && !b.contains(' ')),
            "long URL"
        );
        let images = dataset
            .conversations
            .iter()
            .flat_map(|c| &c.messages)
            .filter(|m| m.image.is_some())
            .count();
        assert!((300..700).contains(&images), "~5% images, got {images}");
    }

    #[test]
    fn live_feed_continues_after_dataset() {
        let dataset = Dataset::standard();
        let newest = dataset.conversations[0].last_message().unwrap().clone();
        let target = dataset.conversations[5].id;
        let mut feed = dataset.live_feed(42);
        let first = feed.next(target);
        let second = feed.next(target);
        assert!(first.id > newest.id);
        assert_eq!(second.id.0, first.id.0 + 1);
        assert_eq!(second.sent_at_ms - first.sent_at_ms, LIVE_INTERVAL_MS);
        assert_eq!(first.conversation, target);
        assert!(matches!(first.sender, Sender::Contact(_)));
    }
}
