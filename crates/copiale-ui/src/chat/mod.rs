//! The main window once linked: chat list, conversation log, composer.

mod model;
mod text;
mod time;
mod view;

pub use model::{Chat, Connection, Effects, LogChange, PAGE, Row, title};
pub use time::Clock;
pub use view::ChatView;
