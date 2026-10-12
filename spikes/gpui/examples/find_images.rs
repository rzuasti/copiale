//! Prints ids of recent image messages, for `--jump`.

fn main() {
    let dataset = fixtures::Dataset::standard();
    for conversation in &dataset.conversations {
        let ids: Vec<u64> = conversation
            .messages
            .iter()
            .rev()
            .filter(|m| m.image.is_some())
            .take(3)
            .map(|m| m.id.0)
            .collect();
        println!("{:<40} {ids:?}", conversation.title);
    }
}
