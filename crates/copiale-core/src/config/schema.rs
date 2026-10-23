//! Settings, their TOML names and defaults.

use std::ops::RangeInclusive;

use toml_edit::Value;

use super::parse::Reader;

/// All settings in `config.toml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub appearance: Appearance,
    /// `[theme]`: the palette `appearance.theme = "custom"` uses; checked
    /// by the UI, which knows the tokens.
    pub custom_theme: Option<toml::Table>,
    pub typography: Typography,
    pub layout: Layout,
    pub composer: Composer,
    pub window: WindowConfig,
    pub notifications: Notifications,
}

/// `[appearance]`
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Appearance {
    pub theme: ThemeName,
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

/// `[composer]`
#[derive(Clone, Debug, PartialEq)]
pub struct Composer {
    /// Typed emoticons such as `:)` become emoji.
    pub convert_emoticons: bool,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            convert_emoticons: true,
        }
    }
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

/// The table `ThemeName::Custom` reads.
pub(crate) const CUSTOM_THEME: &str = "theme";

/// A setting stored as one of a fixed set of names.
pub trait Choice: Copy + PartialEq + 'static {
    const CHOICES: &'static [(&'static str, Self)];
    /// Other names read, never written.
    const ALIASES: &'static [(&'static str, Self)] = &[];

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
            .chain(Self::ALIASES)
            .find(|(choice, _)| *choice == name)
            .map(|(_, value)| *value)
    }
}

macro_rules! choice {
    (
        $(#[$meta:meta])* $name:ident { $($(#[$default:ident])? $variant:ident = $text:literal),+ $(,)? }
        $(aliases { $($alias:literal => $target:ident),+ $(,)? })?
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub enum $name {
            $($(#[$default])? $variant),+
        }

        impl Choice for $name {
            const CHOICES: &'static [(&'static str, Self)] = &[$(($text, Self::$variant)),+];
            $(const ALIASES: &'static [(&'static str, Self)] = &[$(($alias, Self::$target)),+];)?
        }
    };
}

choice!(
    /// Copiale's dark or light theme following the system appearance, a
    /// built-in theme, or the `[theme]` table.
    ThemeName {
        #[default] System = "system",
        CopialeDark = "copiale-dark",
        CopialeLight = "copiale-light",
        CatppuccinMocha = "catppuccin-mocha",
        CatppuccinMacchiato = "catppuccin-macchiato",
        CatppuccinFrappe = "catppuccin-frappe",
        CatppuccinLatte = "catppuccin-latte",
        GruvboxDark = "gruvbox-dark",
        GruvboxLight = "gruvbox-light",
        Nord = "nord",
        Dracula = "dracula",
        SolarizedDark = "solarized-dark",
        SolarizedLight = "solarized-light",
        TokyoNight = "tokyo-night",
        TokyoNightStorm = "tokyo-night-storm",
        TokyoNightDay = "tokyo-night-day",
        RosePine = "rose-pine",
        RosePineMoon = "rose-pine-moon",
        RosePineDawn = "rose-pine-dawn",
        EverforestDark = "everforest-dark",
        EverforestLight = "everforest-light",
        KanagawaWave = "kanagawa-wave",
        KanagawaDragon = "kanagawa-dragon",
        KanagawaLotus = "kanagawa-lotus",
        OneDark = "one-dark",
        OneLight = "one-light",
        Custom = "custom",
    }
    aliases { "dark" => CopialeDark, "light" => CopialeLight }
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
    (@range) => {
        None
    };
    (@range $range:expr) => {
        Some((
            json($range.start().to_toml()).expect("a number"),
            json($range.end().to_toml()).expect("a number"),
        ))
    };
    ($($(#[doc = $doc:literal])+ $variant:ident($type:ty): $table:ident.$key:ident $(in $range:expr)?,)+) => {
        /// One change to `config.toml`, as made by the settings UI.
        #[derive(Clone, Debug, PartialEq)]
        pub enum Setting {
            $($(#[doc = $doc])+ $variant($type),)+
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

            /// Every setting, in declaration order, as described to tools
            /// outside the app.
            pub fn info() -> Vec<SettingInfo> {
                let defaults = Config::default();
                vec![$(
                    SettingInfo {
                        table: stringify!($table),
                        key: stringify!($key),
                        description: description(&[$($doc),+]),
                        kind: <$type as Kind>::info(),
                        default: json(defaults.$table.$key.to_toml()),
                        range: settings!(@range $($range)?),
                    },
                )+]
            }
        }

        impl Config {
            /// Applies a change as writing it to the file and reloading would.
            pub fn set(&mut self, setting: &Setting) {
                match setting {
                    $(Setting::$variant(v) => self.$table.$key = v.clone().normalized(),)+
                }
            }

            /// Reads every setting, in declaration order; missing or
            /// invalid ones keep `base`'s value.
            pub(crate) fn read(reader: &mut Reader, base: &Self) -> Self {
                let mut config = base.clone();
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
    /// Color theme: `system` follows the system's dark or light appearance;
    /// `custom` uses the `[theme]` table.
    Theme(ThemeName): appearance.theme,
    /// Interface font; unset: the built-in default.
    UiFont(Option<String>): typography.ui_font,
    /// Monospaced font, for labels and hints; unset: the built-in default.
    MonoFont(Option<String>): typography.mono_font,
    /// Message font; unset: the built-in default.
    BodyFont(Option<String>): typography.body_font,
    /// Base font size in pixels.
    FontSize(u32): typography.size in Typography::SIZE_RANGE,
    /// Line height of messages, as a multiple of the font size.
    LineHeight(f64): typography.line_height in Typography::LINE_HEIGHT_RANGE,
    /// Spacing of messages and rows.
    Density(Density): layout.density,
    /// How messages are drawn: an IRC-style log, blocks or bubbles.
    MessageStyle(MessageStyle): layout.message_style,
    /// When message times show.
    Timestamps(TimestampVisibility): layout.timestamps,
    /// Clock format.
    Clock(TimeFormat): layout.clock,
    /// Avatars in the chat list.
    Avatars(bool): layout.avatars,
    /// Typed emoticons such as `:)` become emoji.
    ConvertEmoticons(bool): composer.convert_emoticons,
    /// Window background opacity; text stays opaque.
    Opacity(f64): window.opacity in WindowConfig::OPACITY_RANGE,
    /// Blur behind a translucent background, where the compositor allows.
    Blur(bool): window.blur,
    /// Hides the window from screenshots and screen shares; Windows and
    /// macOS only.
    CaptureProtection(bool): window.capture_protection,
    /// What notifications show: nothing, the sender's name, or the message
    /// too.
    NotificationContent(NotificationContent): notifications.content,
}

/// A setting as described to tools outside the app, such as the Nix module.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingInfo {
    pub table: &'static str,
    pub key: &'static str,
    pub description: String,
    pub kind: KindInfo,
    /// `None`: unset.
    pub default: Option<serde_json::Value>,
    /// Lowest and highest value, for numbers.
    pub range: Option<(serde_json::Value, serde_json::Value)>,
}

/// A setting's type.
#[derive(Clone, Debug, PartialEq)]
pub enum KindInfo {
    Choice(Vec<&'static str>),
    Bool,
    Integer,
    Float,
    /// A font name.
    Font,
}

/// Doc comment lines as one paragraph.
fn description(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A TOML value as JSON.
fn json(value: Option<Value>) -> Option<serde_json::Value> {
    match value? {
        Value::String(text) => Some(text.value().as_str().into()),
        Value::Integer(number) => Some((*number.value()).into()),
        Value::Float(number) => Some((*number.value()).into()),
        Value::Boolean(flag) => Some((*flag.value()).into()),
        _ => None,
    }
}

/// How a setting's type is read from and written to TOML.
pub(crate) trait Kind: Clone {
    /// What a warning says was expected.
    fn expected() -> String;

    fn info() -> KindInfo;

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

    fn info() -> KindInfo {
        KindInfo::Choice(T::CHOICES.iter().map(|(name, _)| *name).collect())
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

    fn info() -> KindInfo {
        KindInfo::Bool
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

    fn info() -> KindInfo {
        KindInfo::Integer
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

    fn info() -> KindInfo {
        KindInfo::Float
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

    fn info() -> KindInfo {
        KindInfo::Font
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_describes_every_setting() {
        let info = Setting::info();
        let keys: Vec<_> = info
            .iter()
            .map(|setting| (setting.table, setting.key))
            .collect();
        assert_eq!(keys, Setting::KEYS);
        assert!(info.iter().all(|setting| !setting.description.is_empty()));

        let size = &info[4];
        assert_eq!((size.table, size.key), ("typography", "size"));
        assert_eq!(size.kind, KindInfo::Integer);
        assert_eq!(size.default, Some(14.into()));
        assert_eq!(size.range, Some((9.into(), 32.into())));

        let theme = &info[0];
        assert_eq!(
            theme.description,
            "Color theme: `system` follows the system's dark or light appearance; `custom` uses the `[theme]` table."
        );
        let KindInfo::Choice(choices) = &theme.kind else {
            panic!("{:?}", theme.kind);
        };
        assert_eq!(choices.first(), Some(&"system"));
        assert!(!choices.contains(&"dark"), "aliases aren't listed");
        assert_eq!(theme.default, Some("system".into()));

        let ui_font = &info[1];
        assert_eq!(
            (ui_font.kind.clone(), ui_font.default.clone()),
            (KindInfo::Font, None)
        );
        let line_height = &info[5];
        assert_eq!(line_height.default, Some(1.4.into()));
        assert_eq!(line_height.range, Some((1.0.into(), 2.0.into())));
    }
}
