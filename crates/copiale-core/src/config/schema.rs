//! Settings, their TOML names and defaults.

use std::ops::RangeInclusive;

use toml_edit::Value;

/// All settings in `config.toml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub appearance: Appearance,
    pub typography: Typography,
    pub layout: Layout,
    pub window: WindowConfig,
    pub notifications: Notifications,
}

/// `[appearance]`
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Appearance {
    pub theme: ThemeMode,
}

/// `[typography]`
#[derive(Clone, Debug, PartialEq)]
pub struct Typography {
    /// `None`: the built-in default font.
    pub ui_font: Option<String>,
    pub mono_font: Option<String>,
    pub body_font: Option<String>,
    /// Base size in pixels.
    pub size: u32,
    /// Multiple of the font size.
    pub line_height: f64,
}

impl Typography {
    pub const SIZE_RANGE: RangeInclusive<u32> = 9..=32;
    pub const LINE_HEIGHT_RANGE: RangeInclusive<f64> = 1.0..=2.0;
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            ui_font: None,
            mono_font: None,
            body_font: None,
            size: 14,
            line_height: 1.4,
        }
    }
}

/// `[layout]`
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub density: Density,
    pub message_style: MessageStyle,
    pub timestamps: TimestampVisibility,
    pub clock: Clock,
    pub avatars: bool,
}

/// `[window]`
#[derive(Clone, Debug, PartialEq)]
pub struct WindowConfig {
    /// Background opacity; text stays opaque.
    pub opacity: f64,
    /// Blur behind a translucent background, where the compositor allows.
    pub blur: bool,
}

impl WindowConfig {
    pub const OPACITY_RANGE: RangeInclusive<f64> = 0.5..=1.0;
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blur: false,
        }
    }
}

/// `[notifications]`
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notifications {
    pub content: NotificationContent,
}

/// A setting stored as one of a fixed set of names.
pub(crate) trait Choice: Copy + PartialEq + 'static {
    const CHOICES: &'static [(&'static str, Self)];

    fn name(self) -> &'static str {
        Self::CHOICES
            .iter()
            .find(|(_, value)| *value == self)
            .map(|(name, _)| *name)
            .expect("every variant is listed in CHOICES")
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::CHOICES
            .iter()
            .find(|(choice, _)| *choice == name)
            .map(|(_, value)| *value)
    }
}

macro_rules! choice {
    ($(#[$meta:meta])* $name:ident { $($(#[$default:ident])? $variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub enum $name {
            $($(#[$default])? $variant),+
        }

        impl Choice for $name {
            const CHOICES: &'static [(&'static str, Self)] = &[$(($text, Self::$variant)),+];
        }
    };
}

choice!(
    /// Follow the system appearance, or force one.
    ThemeMode { #[default] System = "system", Dark = "dark", Light = "light" }
);
choice!(Density { Compact = "compact", #[default] Cozy = "cozy", Roomy = "roomy" });
choice!(MessageStyle { #[default] Log = "log", Blocks = "blocks", Bubbles = "bubbles" });
choice!(TimestampVisibility { #[default] Always = "always", Hover = "hover", Never = "never" });
choice!(Clock { #[default] H24 = "24h", H12 = "12h" });
choice!(
    /// What notifications reveal.
    NotificationContent { Off = "off", #[default] Name = "name", Message = "message" }
);

/// One change to `config.toml`, as made by the settings UI.
#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    Theme(ThemeMode),
    /// `None` removes the key, restoring the default font.
    UiFont(Option<String>),
    MonoFont(Option<String>),
    BodyFont(Option<String>),
    FontSize(u32),
    LineHeight(f64),
    Density(Density),
    MessageStyle(MessageStyle),
    Timestamps(TimestampVisibility),
    Clock(Clock),
    Avatars(bool),
    Opacity(f64),
    Blur(bool),
    NotificationContent(NotificationContent),
}

impl Setting {
    /// `(table, key)` in the file.
    pub(crate) fn key(&self) -> (&'static str, &'static str) {
        match self {
            Self::Theme(_) => ("appearance", "theme"),
            Self::UiFont(_) => ("typography", "ui_font"),
            Self::MonoFont(_) => ("typography", "mono_font"),
            Self::BodyFont(_) => ("typography", "body_font"),
            Self::FontSize(_) => ("typography", "size"),
            Self::LineHeight(_) => ("typography", "line_height"),
            Self::Density(_) => ("layout", "density"),
            Self::MessageStyle(_) => ("layout", "message_style"),
            Self::Timestamps(_) => ("layout", "timestamps"),
            Self::Clock(_) => ("layout", "clock"),
            Self::Avatars(_) => ("layout", "avatars"),
            Self::Opacity(_) => ("window", "opacity"),
            Self::Blur(_) => ("window", "blur"),
            Self::NotificationContent(_) => ("notifications", "content"),
        }
    }

    /// TOML value; `None` removes the key.
    pub(crate) fn value(&self) -> Option<Value> {
        Some(match self {
            Self::Theme(v) => v.name().into(),
            Self::UiFont(font) | Self::MonoFont(font) | Self::BodyFont(font) => {
                font.as_deref().filter(|font| !font.is_empty())?.into()
            }
            Self::FontSize(size) => i64::from(*size).into(),
            Self::LineHeight(v) | Self::Opacity(v) => (*v).into(),
            Self::Density(v) => v.name().into(),
            Self::MessageStyle(v) => v.name().into(),
            Self::Timestamps(v) => v.name().into(),
            Self::Clock(v) => v.name().into(),
            Self::Avatars(v) | Self::Blur(v) => (*v).into(),
            Self::NotificationContent(v) => v.name().into(),
        })
    }

    /// Error message if the value is out of range.
    pub(crate) fn check(&self) -> Result<(), String> {
        match self {
            Self::FontSize(size) => in_range(size, &Typography::SIZE_RANGE),
            Self::LineHeight(v) => in_range(v, &Typography::LINE_HEIGHT_RANGE),
            Self::Opacity(v) => in_range(v, &WindowConfig::OPACITY_RANGE),
            _ => Ok(()),
        }
    }
}

pub(crate) fn in_range<T: PartialOrd + std::fmt::Display>(
    value: &T,
    range: &RangeInclusive<T>,
) -> Result<(), String> {
    if range.contains(value) {
        Ok(())
    } else {
        Err(format!(
            "must be between {} and {}, got {value}",
            range.start(),
            range.end()
        ))
    }
}
