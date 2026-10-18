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
}

impl Spacing {
    pub fn for_density(density: Density) -> Self {
        match density {
            Density::Compact => Self {
                group: px(1.),
                message: px(0.),
                chat: px(2.),
            },
            Density::Cozy => Self {
                group: px(4.),
                message: px(0.),
                chat: px(4.),
            },
            Density::Roomy => Self {
                group: px(10.),
                message: px(3.),
                chat: px(8.),
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
