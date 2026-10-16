//! The main window once linked: chat list, conversation log, composer.

mod content;
mod model;
mod new_chat;
mod notices;
mod palette;
mod text;
mod time;
mod view;

pub use model::{
    Actions, Chat, Compose, Connection, Effects, LogChange, Overlay, PAGE, Row, member_list, title,
};
pub use new_chat::{Candidate, MAX_CANDIDATES};
pub use notices::{Notice, NoticeAction, NoticeKey, Notices};
pub use palette::{Entry, MAX_CHATS, PaletteAction};
pub use time::Clock;
pub use view::{ChatView, FocusSearch, NewChat, init};
