//! The command palette: chats to switch to and commands, filtered by a
//! fuzzy query.

use std::ops::Range;

use copiale_core::{ConversationId, ServiceId};

use crate::fuzzy::fuzzy_match;

/// Most chats the palette lists; commands always show when they match.
pub const MAX_CHATS: usize = 5;

/// What picking an entry does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    Open(ConversationId),
    /// Shows an account's safety number.
    SafetyNumber(ServiceId),
    /// Accepts a message request.
    Accept(ConversationId),
    Block(ConversationId),
    Unblock(ConversationId),
    /// Deletes a message request or blocked chat.
    DeleteRequest(ConversationId),
    NewChat,
    Settings,
    /// Opens the theme picker.
    Theme,
    Quit,
}

/// Commands, in the order they list after chats.
const COMMANDS: [(PaletteAction, &str); 4] = [
    (PaletteAction::NewChat, "New chat"),
    (PaletteAction::Settings, "Settings"),
    (PaletteAction::Theme, "Theme…"),
    (PaletteAction::Quit, "Quit"),
];

/// An entry as the palette lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub action: PaletteAction,
    pub label: String,
    /// Byte ranges of `label` the query matched.
    pub highlights: Vec<Range<usize>>,
}

/// `chats` (id and title, most recently active first), then commands for
/// the open chat (`contextual`), then the rest, matching `query`, best
/// first.
pub(super) fn entries(
    chats: impl IntoIterator<Item = (ConversationId, String)>,
    contextual: impl IntoIterator<Item = (PaletteAction, String)>,
    query: &str,
) -> Vec<Entry> {
    let commands = contextual.into_iter().chain(
        COMMANDS
            .iter()
            .map(|(action, label)| (*action, (*label).to_owned())),
    );
    let mut matched: Vec<_> = chats
        .into_iter()
        .map(|(id, title)| (PaletteAction::Open(id), title))
        .chain(commands)
        .filter_map(|(action, label)| {
            let m = fuzzy_match(query, &label)?;
            let entry = Entry {
                action,
                label,
                highlights: m.ranges,
            };
            Some((m.score, entry))
        })
        .collect();
    matched.sort_by_key(|(score, _)| -score);
    let mut chats = 0;
    matched
        .into_iter()
        .map(|(_, entry)| entry)
        .filter(|entry| {
            let chat = matches!(entry.action, PaletteAction::Open(_));
            chats += usize::from(chat);
            !chat || chats <= MAX_CHATS
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chats(titles: &[&str]) -> Vec<(ConversationId, String)> {
        titles
            .iter()
            .enumerate()
            .map(|(ix, title)| (ConversationId::new(ix as i64 + 1), (*title).to_owned()))
            .collect()
    }

    fn labels(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.label.as_str()).collect()
    }

    #[test]
    fn empty_query_lists_recent_chats_then_commands() {
        let entries = entries(chats(&["Alice", "Bob", "C", "D", "E", "F"]), [], "");
        assert_eq!(
            labels(&entries),
            [
                "Alice", "Bob", "C", "D", "E", "New chat", "Settings", "Theme…", "Quit",
            ]
        );
        assert_eq!(
            entries[0].action,
            PaletteAction::Open(ConversationId::new(1))
        );
    }

    #[test]
    fn query_ranks_chats_and_commands_together() {
        let entries = entries(chats(&["Bob", "Theo"]), [], "the");
        assert_eq!(labels(&entries), ["Theo", "Theme…"]);
        assert_eq!(entries[0].highlights, vec![0..3]);

        let entries = super::entries(chats(&["Bob", "Quinn"]), [], "q");
        assert_eq!(labels(&entries), ["Quinn", "Quit"]);
        assert!(super::entries(chats(&["Bob"]), [], "xyz").is_empty());
    }

    #[test]
    fn commands_for_the_open_chat_come_first() {
        let peer = ServiceId::new(uuid::Uuid::from_u128(1));
        let safety = (
            PaletteAction::SafetyNumber(peer),
            "Safety number".to_owned(),
        );
        let entries = entries(chats(&["Bob"]), [safety.clone()], "");
        assert_eq!(&labels(&entries)[..3], ["Bob", "Safety number", "New chat"]);

        let entries = super::entries(chats(&["Bob"]), [safety], "safety");
        assert_eq!(labels(&entries), ["Safety number"]);
        assert_eq!(entries[0].action, PaletteAction::SafetyNumber(peer));
    }
}
