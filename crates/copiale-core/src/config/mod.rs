//! User settings in `config.toml`.
//!
//! The file is the source of truth: the settings UI edits it in place,
//! keeping comments, and external edits apply live. Problems never fail a
//! load; each becomes a [`ConfigWarning`] and the setting keeps its default.
//!
//! Account settings (device name, receipts, typing) live with the account,
//! not here.

mod edit;
mod file;
mod parse;
mod schema;

pub use file::{ConfigFile, ConfigWatcher};
pub use schema::{
    Appearance, Clock, Config, Density, Layout, MessageStyle, NotificationContent, Notifications,
    Setting, ThemeMode, TimestampVisibility, Typography, WindowConfig,
};

use std::fmt;

/// A config file with any problems found while reading it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<ConfigWarning>,
}

/// A problem in the config file; the affected setting uses its default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWarning {
    /// Dotted path such as `layout.density`; `None` for the whole file.
    pub setting: Option<String>,
    pub message: String,
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.setting {
            Some(setting) => write!(f, "{setting}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file can't be parsed, so it can't be edited safely.
    #[error("config file has a syntax error; fix it first: {0}")]
    Syntax(String),
    #[error("{setting}: {message}")]
    Invalid { setting: String, message: String },
    #[error("`{0}` in the config file is not a table")]
    NotATable(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("watching the config file: {0}")]
    Watch(#[from] notify::Error),
}
