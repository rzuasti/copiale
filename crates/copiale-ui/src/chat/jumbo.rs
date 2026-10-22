//! Emoji-only messages, drawn larger the fewer they are.

use unicode_properties::{EmojiStatus, UnicodeEmoji as _};
use unicode_segmentation::UnicodeSegmentation as _;

/// Text sizes in rems for 1 to 5 emoji; more draw at the normal size.
const SIZES: [f32; 5] = [3.5, 3., 2.5, 2.25, 2.];

/// The size in rems for `text` when it's only emoji (whitespace aside) and
/// at most five of them.
pub fn jumbo_size(text: &str) -> Option<f32> {
    let mut count = 0;
    for grapheme in text.graphemes(true) {
        if grapheme.chars().all(char::is_whitespace) {
            continue;
        }
        if !is_emoji(grapheme) || count == SIZES.len() {
            return None;
        }
        count += 1;
    }
    count.checked_sub(1).map(|ix| SIZES[ix])
}

/// Whether `grapheme` draws as emoji: emoji presentation by default, or
/// asked for with U+FE0F or as a keycap.
fn is_emoji(grapheme: &str) -> bool {
    let Some(first) = grapheme.chars().next() else {
        return false;
    };
    let presentation = matches!(
        first.emoji_status(),
        EmojiStatus::EmojiPresentation
            | EmojiStatus::EmojiPresentationAndModifierBase
            | EmojiStatus::EmojiPresentationAndEmojiComponent
            | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
    );
    let requested = first.is_emoji_char_or_emoji_component()
        && grapheme
            .chars()
            .any(|ch| matches!(ch, '\u{FE0F}' | '\u{20E3}'));
    presentation || requested
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fewer_emoji_draw_larger() {
        assert_eq!(jumbo_size("😀"), Some(3.5));
        assert_eq!(jumbo_size(" 😀😀 "), Some(3.));
        assert_eq!(jumbo_size("😀 😀 😀"), Some(2.5));
        assert_eq!(jumbo_size("😀😀😀😀😀"), Some(2.));
        assert_eq!(jumbo_size("😀😀😀😀😀😀"), None);
    }

    #[test]
    fn sequences_count_once() {
        // Family (ZWJ), skin tone, flag, keycap, tag flag, FE0F heart.
        assert_eq!(
            jumbo_size("👨‍👩‍👧👍🏽🇦🇷1️⃣🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}"),
            Some(2.)
        );
        assert_eq!(jumbo_size("❤️"), Some(3.5));
    }

    #[test]
    fn text_is_not_jumbo() {
        assert_eq!(jumbo_size(""), None);
        assert_eq!(jumbo_size("  "), None);
        assert_eq!(jumbo_size("hi 😀"), None);
        assert_eq!(jumbo_size("1"), None);
        assert_eq!(jumbo_size("#"), None);
        // Text presentation by default.
        assert_eq!(jumbo_size("❤"), None);
        assert_eq!(jumbo_size("©"), None);
    }
}
