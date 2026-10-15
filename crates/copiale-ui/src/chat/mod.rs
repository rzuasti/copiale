//! The main window once linked: chat list, conversation log, composer.

mod content;
mod model;
mod text;
mod time;
mod view;

pub use model::{Chat, Connection, Effects, LogChange, PAGE, Row, member_list, title};
pub use time::Clock;
pub use view::{ChatView, FocusSearch, init};
