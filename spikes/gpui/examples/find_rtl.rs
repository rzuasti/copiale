//! Prints ids of messages with Arabic or Hebrew text, for `--jump`.

fn main() {
    let dataset = fixtures::Dataset::standard();
    let rtl = |c: char| matches!(c, '\u{0590}'..='\u{06FF}');
    for conversation in dataset.conversations.iter().take(4) {
        let ids: Vec<(u64, &str)> = conversation
            .messages
            .iter()
            .rev()
            .filter(|m| m.body.chars().any(rtl))
            .take(3)
            .map(|m| (m.id.0, m.body.as_str()))
            .collect();
        println!("{:<20} {ids:?}", conversation.title);
    }
}
