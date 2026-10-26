//! Full or compact layout, by window width, and spacing, by density.

use copiale_core::config::Density;
use gpui_kit::{Pixels, px};

/// Windows narrower than this, in pixels, hide the chat list.
pub const COMPACT_BELOW: f32 = 640.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    /// Chat list and conversation side by side.
    #[default]
    Full,
    /// The conversation alone; chats switch through the palette.
    Compact,
}

impl Layout {
    pub fn for_width(width: f32) -> Self {
        if width < COMPACT_BELOW {
            Self::Compact
        } else {
            Self::Full
        }
    }

    pub fn shows_chat_list(self) -> bool {
        self == Self::Full
    }
}

/// Vertical spacing for the `layout.density` setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    /// Above a group of messages.
    pub group: Pixels,
    /// Between messages in a group.
    pub message: Pixels,
    /// Above and below a chat in the list.
    pub chat: Pixels,
    /// Between bubbles in a group; never zero, so bubbles don't touch.
    pub bubble: Pixels,
    /// Inside a bubble, horizontal and vertical; never zero.
    pub bubble_padding: (Pixels, Pixels),
    /// Above and below each message, inside its hover highlight; never
    /// zero, so messages don't touch.
    pub row: Pixels,
    /// Above and below a message's pictures and videos.
    pub media: Pixels,
}

impl Spacing {
    pub fn for_density(density: Density) -> Self {
        match density {
            Density::Compact => Self {
                group: px(1.),
                message: px(0.),
                chat: px(2.),
                bubble: px(2.),
                bubble_padding: (px(8.), px(3.)),
                row: px(1.),
                media: px(3.),
            },
            Density::Cozy => Self {
                group: px(4.),
                message: px(0.),
                chat: px(4.),
                bubble: px(4.),
                bubble_padding: (px(10.), px(5.)),
                row: px(2.),
                media: px(4.),
            },
            Density::Roomy => Self {
                group: px(10.),
                message: px(3.),
                chat: px(8.),
                bubble: px(6.),
                bubble_padding: (px(12.), px(8.)),
                row: px(3.),
                media: px(6.),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denser_is_tighter() {
        let [compact, cozy, roomy] =
            [Density::Compact, Density::Cozy, Density::Roomy].map(Spacing::for_density);
        assert!(compact.group < cozy.group && cozy.group < roomy.group);
        assert!(compact.chat < cozy.chat && cozy.chat < roomy.chat);
        assert!(compact.message <= cozy.message && cozy.message < roomy.message);
        assert!(compact.bubble < cozy.bubble && cozy.bubble < roomy.bubble);
        assert!(compact.bubble_padding.0 < cozy.bubble_padding.0);
        assert!(cozy.bubble_padding.0 < roomy.bubble_padding.0);
        assert!(compact.bubble_padding.1 < cozy.bubble_padding.1);
        assert!(cozy.bubble_padding.1 < roomy.bubble_padding.1);
        assert!(compact.row < cozy.row && cozy.row < roomy.row);
        assert!(compact.media < cozy.media && cozy.media < roomy.media);
    }

    #[test]
    fn messages_never_touch() {
        for density in [Density::Compact, Density::Cozy, Density::Roomy] {
            let spacing = Spacing::for_density(density);
            assert!(spacing.row > px(0.));
            assert!(spacing.media > px(0.));
        }
    }

    #[test]
    fn bubbles_never_touch() {
        for density in [Density::Compact, Density::Cozy, Density::Roomy] {
            let spacing = Spacing::for_density(density);
            assert!(spacing.bubble > px(0.));
            assert!(spacing.bubble_padding.0 > px(0.) && spacing.bubble_padding.1 > px(0.));
            assert!(spacing.group * 2. >= spacing.bubble);
        }
    }

    #[test]
    fn narrow_windows_are_compact() {
        assert_eq!(Layout::for_width(320.), Layout::Compact);
        assert_eq!(Layout::for_width(639.9), Layout::Compact);
        assert_eq!(Layout::for_width(640.), Layout::Full);
        assert_eq!(Layout::for_width(1920.), Layout::Full);
        assert!(!Layout::Compact.shows_chat_list());
        assert!(Layout::Full.shows_chat_list());
    }
}
