//! Prints an overview of the standard dataset: `cargo run --example summary`.

use std::time::Instant;

use fixtures::{ConversationKind, Dataset, Theme};

fn main() {
    let start = Instant::now();
    let dataset = Dataset::standard();
    let elapsed = start.elapsed();

    println!(
        "{} messages, {} conversations, {} contacts (generated in {elapsed:.1?})\n",
        dataset.message_count(),
        dataset.conversations.len(),
        dataset.contacts.len(),
    );

    println!(
        "{:<44} {:>6} {:>7} {:>6} {:>8}  last",
        "conversation", "kind", "msgs", "imgs", "replies"
    );
    for conversation in &dataset.conversations {
        let messages = &conversation.messages;
        let kind = match conversation.kind {
            ConversationKind::Direct => "direct",
            ConversationKind::Group => "group",
        };
        let title: String = conversation.title.chars().take(42).collect();
        let last = conversation
            .last_message()
            .map(|m| format!("{} {}", m.date_label(), m.time_label()))
            .unwrap_or_default();
        println!(
            "{title:<44} {kind:>6} {:>7} {:>6} {:>8}  {last}",
            messages.len(),
            messages.iter().filter(|m| m.image.is_some()).count(),
            messages.iter().filter(|m| m.reply_to.is_some()).count(),
        );
    }

    let busiest = dataset
        .conversations
        .iter()
        .max_by_key(|c| c.messages.len())
        .expect("dataset has conversations");
    println!("\nLast 8 messages in {:?}:", busiest.title);
    for message in busiest.messages.iter().rev().take(8).rev() {
        let body: String = message.body.chars().take(70).collect();
        let image = message
            .image
            .map(|i| format!(" [image {}x{}]", i.width, i.height))
            .unwrap_or_default();
        println!(
            "  {} {:<20} {body}{image}",
            message.time_label(),
            dataset.sender_name(message.sender),
        );
    }

    println!("\nThemes:");
    for theme in Theme::bundled() {
        println!(
            "  {} ({:?}) accent {}",
            theme.name, theme.appearance, theme.colors.accent
        );
    }
}
