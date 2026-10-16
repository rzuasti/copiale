//! Full or compact layout, by window width.

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

#[cfg(test)]
mod tests {
    use super::*;

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
