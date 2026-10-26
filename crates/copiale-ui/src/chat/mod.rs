//! The main window once linked: chat list, conversation log, composer.

mod content;
mod emoji;
mod emoji_grid;
mod jumbo;
mod layout;
mod media;
mod model;
mod new_chat;
mod notices;
mod palette;
mod settings_panel;
mod text;
mod time;
mod view;

pub use emoji::{EmojiTyping, MAX_MATCHES, Picker, Shortcode};
pub use emoji_grid::{CATEGORIES, COLUMNS, EmojiGrid, GridRow, RECENT};
pub use layout::{COMPACT_BELOW, Layout};
pub use model::{
    Actions, Chat, Compose, Connection, Effects, LogChange, Overlay, PAGE, RequestBanner, Row,
    Viewed, member_list, title,
};
pub use new_chat::{Candidate, MAX_CANDIDATES};
pub use notices::{Notice, NoticeAction, NoticeKey, Notices};
pub use palette::{Entry, MAX_CHATS, PaletteAction};
pub use settings_panel::{
    Activation, FIELDS, Field, FontRole, MAX_FONTS, PickerEntry, Picking, Section, SettingPicker,
    SettingsPanel, Value,
};
pub use time::LocalClock;
pub use view::{ChatView, FocusSearch, NewChat, init};
