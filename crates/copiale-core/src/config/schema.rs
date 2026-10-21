//! Settings, their TOML names and defaults.

use std::ops::RangeInclusive;

use toml_edit::Value;

use super::parse::Reader;

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
    pub clock: TimeFormat,
    pub avatars: bool,
}

/// `[window]`
#[derive(Clone, Debug, PartialEq)]
pub struct WindowConfig {
    /// Background opacity; text stays opaque.
    pub opacity: f64,
    /// Blur behind a translucent background, where the compositor allows.
    pub blur: bool,
    /// Hidden from screenshots and screen shares, where the OS allows.
    pub capture_protection: bool,
}

impl WindowConfig {
    pub const OPACITY_RANGE: RangeInclusive<f64> = 0.5..=1.0;
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blur: false,
            capture_protection: false,
        }
    }
}

/// `[notifications]`
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notifications {
    pub content: NotificationContent,
}

/// A setting stored as one of a fixed set of names.
pub trait Choice: Copy + PartialEq + 'static {
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
choice!(TimeFormat { #[default] H24 = "24h", H12 = "12h" });
choice!(
    /// What notifications reveal.
    NotificationContent { Off = "off", #[default] Name = "name", Message = "message" }
);

/// Declares every setting once: its `Setting` variant and payload, and its
/// `Config` field, whose path is also its TOML table and key. Numbers give
/// their range.
macro_rules! settings {
    (@check $v:ident) => {{
        let _ = $v;
        Ok(())
    }};
    (@check $v:ident $range:expr) => {
        in_range($v, &$range)
    };
    ($($(#[$meta:meta])* $variant:ident($type:ty): $table:ident.$key:ident $(in $range:expr)?,)+) => {
        /// One change to `config.toml`, as made by the settings UI.
        #[derive(Clone, Debug, PartialEq)]
        pub enum Setting {
            $($(#[$meta])* $variant($type),)+
        }

        impl Setting {
            /// `(table, key)` of every setting, in declaration order.
            pub const KEYS: &'static [(&'static str, &'static str)] =
                &[$((stringify!($table), stringify!($key))),+];

            /// `(table, key)` in the file.
            pub fn key(&self) -> (&'static str, &'static str) {
                match self {
                    $(Self::$variant(_) => (stringify!($table), stringify!($key)),)+
                }
            }

            /// TOML value; `None` removes the key.
            pub(crate) fn value(&self) -> Option<Value> {
                match self {
                    $(Self::$variant(v) => v.to_toml(),)+
                }
            }

            /// Error message if the value is out of range.
            pub(crate) fn check(&self) -> Result<(), String> {
                match self {
                    $(Self::$variant(v) => settings!(@check v $($range)?),)+
                }
            }
        }

        impl Config {
            /// Applies a change as writing it to the file and reloading would.
            pub fn set(&mut self, setting: &Setting) {
                match setting {
                    $(Setting::$variant(v) => self.$table.$key = v.clone().normalized(),)+
                }
            }

            /// Reads every setting, in declaration order.
            pub(crate) fn read(reader: &mut Reader) -> Self {
                let mut config = Self::default();
                $(
                    config.$table.$key = reader.read(
                        stringify!($table),
                        stringify!($key),
                        config.$table.$key.clone(),
                        |v| settings!(@check v $($range)?),
                    );
                )+
                config
            }
        }
    };
}

settings! {
    Theme(ThemeMode): appearance.theme,
    /// `None` removes the key, restoring the default font.
    UiFont(Option<String>): typography.ui_font,
    MonoFont(Option<String>): typography.mono_font,
    BodyFont(Option<String>): typography.body_font,
    FontSize(u32): typography.size in Typography::SIZE_RANGE,
    LineHeight(f64): typography.line_height in Typography::LINE_HEIGHT_RANGE,
    Density(Density): layout.density,
    MessageStyle(MessageStyle): layout.message_style,
    Timestamps(TimestampVisibility): layout.timestamps,
    Clock(TimeFormat): layout.clock,
    Avatars(bool): layout.avatars,
    Opacity(f64): window.opacity in WindowConfig::OPACITY_RANGE,
    Blur(bool): window.blur,
    CaptureProtection(bool): window.capture_protection,
    NotificationContent(NotificationContent): notifications.content,
}

/// How a setting's type is read from and written to TOML.
pub(crate) trait Kind: Clone {
    /// What a warning says was expected.
    fn expected() -> String;

    fn from_toml(value: &Value) -> Option<Self>;

    /// `None` removes the key.
    fn to_toml(&self) -> Option<Value>;

    /// As a warning names the fallback.
    fn describe(&self) -> String;

    /// As stored after a round trip through the file.
    fn normalized(self) -> Self {
        self
    }
}

impl<T: Choice> Kind for T {
    fn expected() -> String {
        let names: Vec<_> = T::CHOICES.iter().map(|(name, _)| *name).collect();
        format!("one of {}", names.join(", "))
    }

    fn from_toml(value: &Value) -> Option<Self> {
        value.as_str().and_then(T::from_name)
    }

    fn to_toml(&self) -> Option<Value> {
        Some(self.name().into())
    }

    fn describe(&self) -> String {
        self.name().into()
    }
}

impl Kind for bool {
    fn expected() -> String {
        "true or false".into()
    }

    fn from_toml(value: &Value) -> Option<Self> {
        value.as_bool()
    }

    fn to_toml(&self) -> Option<Value> {
        Some((*self).into())
    }

    fn describe(&self) -> String {
        self.to_string()
    }
}

impl Kind for u32 {
    fn expected() -> String {
        "a whole number".into()
    }

    fn from_toml(value: &Value) -> Option<Self> {
        value.as_integer().and_then(|number| number.try_into().ok())
    }

    fn to_toml(&self) -> Option<Value> {
        Some(i64::from(*self).into())
    }

    fn describe(&self) -> String {
        self.to_string()
    }
}

/// Accepts integers too: `line_height = 1` means 1.0.
impl Kind for f64 {
    fn expected() -> String {
        "a number".into()
    }

    fn from_toml(value: &Value) -> Option<Self> {
        value
            .as_float()
            .or_else(|| value.as_integer().map(|number| number as f64))
    }

    fn to_toml(&self) -> Option<Value> {
        Some((*self).into())
    }

    fn describe(&self) -> String {
        self.to_string()
    }
}

/// A font name; `None` or empty means the default font.
impl Kind for Option<String> {
    fn expected() -> String {
        "a font name".into()
    }

    fn from_toml(value: &Value) -> Option<Self> {
        Some(Some(value.as_str()?.to_owned()).normalized())
    }

    fn to_toml(&self) -> Option<Value> {
        self.as_deref()
            .filter(|font| !font.is_empty())
            .map(Value::from)
    }

    fn describe(&self) -> String {
        self.as_deref().unwrap_or("the default").into()
    }

    fn normalized(self) -> Self {
        self.filter(|font| !font.is_empty())
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
