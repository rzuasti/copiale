//! Deterministic fake chat data and themes shared by the UI toolkit spike
//! prototypes (see `docs/spike-ui-toolkit.md`).

mod color;
mod dataset;
mod image;
mod model;
mod rng;
mod text;
mod theme;

pub use color::Rgb;
pub use dataset::{Config, DEFAULT_SEED, Dataset, LiveFeed, NOW_MS};
pub use model::{
    Contact, ContactId, Conversation, ConversationId, ConversationKind, ImageAttachment, Message,
    MessageId, Reaction, Sender,
};
pub use theme::{Appearance, Palette, Theme, ThemeError};
