//! The main window once linked: chat list, conversation log, composer.

mod content;
mod layout;
mod model;
mod new_chat;
mod notices;
mod palette;
mod settings_panel;
mod text;
mod time;
mod view;

pub use layout::{COMPACT_BELOW, Layout};
pub use model::{
    Actions, Chat, Compose, Connection, Effects, LogChange, Overlay, PAGE, Row, member_list, title,
};
pub use new_chat::{Candidate, MAX_CANDIDATES};
pub use notices::{Notice, NoticeAction, NoticeKey, Notices};
pub use palette::{Entry, MAX_CHATS, PaletteAction};
pub use settings_panel::{
    Activation, FIELDS, Field, FontEntry, FontPicker, FontRole, MAX_FONTS, Section, SettingsPanel,
    Value,
};
pub use time::LocalClock;
pub use view::{ChatView, FocusSearch, NewChat, init};
