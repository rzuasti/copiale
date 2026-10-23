//! Built-in themes, the system-following default and the custom one.
//!
//! Views read colors from the active [`Palette`] only. GPUI Kit components
//! get the same tokens mapped onto their roles.

use std::rc::Rc;

use copiale_core::ServiceId;
use copiale_core::config::{Config, ThemeName};
use gpui_kit::component::{self, ThemeConfig, ThemeConfigColors};
use gpui_kit::{App, Global, Hsla, Rgba, SharedString};

use crate::settings::Settings;
use serde::{Deserialize, Deserializer};

mod emoji;
mod fonts;
mod system;

pub(crate) use emoji::init as init_emoji;
pub use emoji::{EMOJI_FAMILY, fallbacks as emoji_fallbacks};
pub use fonts::{Fonts, InstalledFonts};
pub(crate) use system::watch_settings;
pub use system::{Scheme, follow, follow_system, follow_window};

macro_rules! built_in {
    ($name:ident, $label:literal, $file:literal) => {
        (
            ThemeName::$name,
            $label,
            include_str!(concat!("../../themes/", $file, ".toml")),
        )
    };
}

/// Built-in themes in listing order: name, label and file.
pub const BUILT_IN: [(ThemeName, &str, &str); 25] = [
    built_in!(CopialeDark, "Copiale Dark", "dark"),
    built_in!(CopialeLight, "Copiale Light", "light"),
    built_in!(CatppuccinMocha, "Catppuccin Mocha", "catppuccin-mocha"),
    built_in!(
        CatppuccinMacchiato,
        "Catppuccin Macchiato",
        "catppuccin-macchiato"
    ),
    built_in!(CatppuccinFrappe, "Catppuccin Frappé", "catppuccin-frappe"),
    built_in!(CatppuccinLatte, "Catppuccin Latte", "catppuccin-latte"),
    built_in!(Dracula, "Dracula", "dracula"),
    built_in!(EverforestDark, "Everforest Dark", "everforest-dark"),
    built_in!(EverforestLight, "Everforest Light", "everforest-light"),
    built_in!(GruvboxDark, "Gruvbox Dark", "gruvbox-dark"),
    built_in!(GruvboxLight, "Gruvbox Light", "gruvbox-light"),
    built_in!(KanagawaWave, "Kanagawa Wave", "kanagawa-wave"),
    built_in!(KanagawaDragon, "Kanagawa Dragon", "kanagawa-dragon"),
    built_in!(KanagawaLotus, "Kanagawa Lotus", "kanagawa-lotus"),
    built_in!(Nord, "Nord", "nord"),
    built_in!(OneDark, "One Dark", "one-dark"),
    built_in!(OneLight, "One Light", "one-light"),
    built_in!(RosePine, "Rosé Pine", "rose-pine"),
    built_in!(RosePineMoon, "Rosé Pine Moon", "rose-pine-moon"),
    built_in!(RosePineDawn, "Rosé Pine Dawn", "rose-pine-dawn"),
    built_in!(SolarizedDark, "Solarized Dark", "solarized-dark"),
    built_in!(SolarizedLight, "Solarized Light", "solarized-light"),
    built_in!(TokyoNight, "Tokyo Night", "tokyo-night"),
    built_in!(TokyoNightStorm, "Tokyo Night Storm", "tokyo-night-storm"),
    built_in!(TokyoNightDay, "Tokyo Night Day", "tokyo-night-day"),
];

/// Themes to pick from: `system`, then the built-in ones.
pub fn listed() -> Vec<ThemeName> {
    std::iter::once(ThemeName::System)
        .chain(BUILT_IN.iter().map(|(name, ..)| *name))
        .collect()
}

/// How `name` is listed.
pub fn label(name: ThemeName) -> &'static str {
    match name {
        ThemeName::System => "System",
        ThemeName::Custom => "Custom",
        _ => BUILT_IN
            .iter()
            .find(|(built_in, ..)| *built_in == name)
            .map_or("", |(_, label, _)| label),
    }
}

/// A theme file.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    pub colors: Palette,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

/// [`Palette`]'s colors other than `nicks`, with what each colors.
pub const TOKENS: [(&str, &str); 14] = [
    ("background", "Conversation pane, dialogs."),
    ("sidebar", "Chat list."),
    ("surface", "Hovered rows, controls."),
    ("border", "Dividers, rules, input outlines."),
    ("text", "Text."),
    ("text_secondary", "Your own name."),
    ("text_muted", "Timestamps, hints, secondary labels."),
    (
        "accent",
        "Unread markers, selected chat, primary buttons, focus.",
    ),
    ("accent_text", "Text on accent, success and danger fills."),
    ("action", "Things to click: message actions, retry, esc."),
    ("success", "Online."),
    ("danger", "Errors, failed messages, offline."),
    ("link", "Links."),
    (
        "selection",
        "Selected text; translucent, as it is drawn over the text.",
    ),
];

/// What [`Palette`]'s `nicks` color.
pub const NICKS: &str =
    "Sender names, picked per contact; quieter than text, louder than text_muted.";

/// Color tokens; see [`TOKENS`].
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    #[serde(deserialize_with = "color")]
    pub background: Hsla,
    #[serde(deserialize_with = "color")]
    pub sidebar: Hsla,
    #[serde(deserialize_with = "color")]
    pub surface: Hsla,
    #[serde(deserialize_with = "color")]
    pub border: Hsla,
    #[serde(deserialize_with = "color")]
    pub text: Hsla,
    #[serde(deserialize_with = "color")]
    pub text_secondary: Hsla,
    #[serde(deserialize_with = "color")]
    pub text_muted: Hsla,
    #[serde(deserialize_with = "color")]
    pub accent: Hsla,
    #[serde(deserialize_with = "color")]
    pub accent_text: Hsla,
    #[serde(deserialize_with = "color")]
    pub action: Hsla,
    #[serde(deserialize_with = "color")]
    pub success: Hsla,
    #[serde(deserialize_with = "color")]
    pub danger: Hsla,
    #[serde(deserialize_with = "color")]
    pub link: Hsla,
    #[serde(deserialize_with = "color")]
    pub selection: Hsla,
    #[serde(deserialize_with = "colors")]
    pub nicks: Vec<Hsla>,
}

impl Palette {
    /// A contact's name color, stable across runs.
    pub fn nick(&self, peer: ServiceId) -> Hsla {
        self.nick_for(peer.uuid().as_bytes())
    }

    /// A name color for any ID, e.g. a group's, stable across runs.
    pub fn nick_for(&self, id: &[u8]) -> Hsla {
        // FNV-1a.
        let hash = id.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
        self.nicks[(hash % self.nicks.len() as u64) as usize]
    }
}

impl Global for Palette {}

/// The active palette: `cx.palette()`.
pub trait ActivePalette {
    fn palette(&self) -> &Palette;
}

impl ActivePalette for App {
    fn palette(&self) -> &Palette {
        self.global::<Palette>()
    }
}

/// The `[theme]` table in `config.toml`: a theme file without a name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomTheme {
    appearance: Appearance,
    colors: Palette,
}

impl Theme {
    pub fn parse(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
    }

    pub fn dark() -> Self {
        Self::built_in(ThemeName::CopialeDark).expect("listed")
    }

    pub fn light() -> Self {
        Self::built_in(ThemeName::CopialeLight).expect("listed")
    }

    /// `None` for `system` and `custom`.
    pub fn built_in(name: ThemeName) -> Option<Self> {
        let (_, _, source) = BUILT_IN.iter().find(|(built_in, ..)| *built_in == name)?;
        Some(Self::parse(source).expect("built-in themes are valid"))
    }

    /// The theme `config` sets as `custom`, or why it can't be used.
    pub fn custom(config: &Config) -> Result<Self, String> {
        let table = config
            .custom_theme
            .clone()
            .ok_or("no [theme] table in the config file")?;
        let custom: CustomTheme = toml::Value::Table(table)
            .try_into()
            .map_err(|error: toml::de::Error| format!("[theme]: {}", error.message()))?;
        Ok(Self {
            name: label(ThemeName::Custom).into(),
            appearance: custom.appearance,
            colors: custom.colors,
        })
    }
}

/// Makes `theme` active, with the settings' fonts and opacity, and redraws
/// all windows.
pub fn apply(theme: &Theme, cx: &mut App) {
    let opacity = Settings::get(cx).window.opacity as f32;
    let typography = Settings::get(cx).typography.clone();
    let fonts = Fonts::resolve(&typography, cx);
    let palette = theme.colors.translucent(opacity);
    let config = Rc::new(component_config(theme, &palette, &fonts));
    cx.set_global(palette);
    component::Theme::update(cx, |kit| kit.apply_config(&config));
    let fonts = fonts.into_resolved(cx);
    cx.set_global(fonts);
}

impl Palette {
    /// Window backgrounds at `opacity`; text and controls stay opaque.
    pub fn translucent(&self, opacity: f32) -> Self {
        Self {
            background: self.background.opacity(opacity),
            sidebar: self.sidebar.opacity(opacity),
            ..self.clone()
        }
    }
}

/// Maps the palette onto GPUI Kit roles; the rest derive from these.
/// Popovers keep `theme`'s opaque background.
fn component_config(theme: &Theme, palette: &Palette, fonts: &Fonts) -> ThemeConfig {
    let c = palette;
    let mut k = ThemeConfigColors::default();
    for (role, color) in [
        (&mut k.background, c.background),
        (&mut k.foreground, c.text),
        (&mut k.border, c.border),
        (&mut k.window_border, c.border),
        (&mut k.input, c.border),
        (&mut k.muted, c.surface),
        (&mut k.muted_foreground, c.text_muted),
        (&mut k.accent, c.surface),
        (&mut k.accent_foreground, c.text),
        (&mut k.primary, c.accent),
        (&mut k.primary_foreground, c.accent_text),
        (&mut k.secondary, c.surface),
        (&mut k.secondary_foreground, c.text),
        (&mut k.popover, theme.colors.background),
        (&mut k.popover_foreground, c.text),
        (&mut k.list, c.sidebar),
        (&mut k.list_hover, c.surface),
        (&mut k.list_active, c.accent),
        (&mut k.list_active_border, c.accent),
        (&mut k.sidebar, c.sidebar),
        (&mut k.sidebar_foreground, c.text),
        (&mut k.sidebar_border, c.border),
        (&mut k.sidebar_accent, c.surface),
        (&mut k.sidebar_accent_foreground, c.text),
        (&mut k.sidebar_primary, c.accent),
        (&mut k.sidebar_primary_foreground, c.accent_text),
        (&mut k.title_bar, c.sidebar),
        (&mut k.title_bar_border, c.border),
        (&mut k.selection, c.selection),
        (&mut k.link, c.link),
        (&mut k.link_hover, c.link),
        (&mut k.link_active, c.link),
        (&mut k.ring, c.accent),
        (&mut k.caret, c.accent),
        (&mut k.drag_border, c.accent),
        // Scrollbars overlay the content; only the thumb shows.
        (&mut k.scrollbar, gpui_kit::transparent_black()),
        (&mut k.scrollbar_thumb, c.border),
        (&mut k.scrollbar_thumb_hover, c.text_muted),
        (&mut k.success, c.success),
        (&mut k.success_foreground, c.accent_text),
        (&mut k.danger, c.danger),
        (&mut k.danger_foreground, c.accent_text),
    ] {
        *role = Some(hex(color));
    }
    ThemeConfig {
        name: theme.name.clone().into(),
        mode: match theme.appearance {
            Appearance::Light => component::ThemeMode::Light,
            Appearance::Dark => component::ThemeMode::Dark,
        },
        font_family: Some(fonts.ui.clone()),
        mono_font_family: Some(fonts.mono.clone()),
        font_size: Some(fonts.size),
        colors: k,
        ..Default::default()
    }
}

/// `#rrggbbaa`, rounded so parsed colors round-trip exactly.
fn hex(color: Hsla) -> SharedString {
    let Rgba { r, g, b, a } = color.into();
    let byte = |v: f32| (v * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(r),
        byte(g),
        byte(b),
        byte(a)
    )
    .into()
}

fn color<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Hsla, D::Error> {
    parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

fn colors<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Hsla>, D::Error> {
    let texts = Vec::<String>::deserialize(deserializer)?;
    if texts.is_empty() {
        return Err(serde::de::Error::custom("needs at least one color"));
    }
    texts
        .iter()
        .map(|text| parse(text))
        .collect::<Result<_, _>>()
        .map_err(serde::de::Error::custom)
}

fn parse(text: &str) -> Result<Hsla, String> {
    Rgba::try_from(text)
        .map(Hsla::from)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use copiale_core::config::Choice as _;

    fn tokens(palette: &Palette) -> [(&'static str, Hsla); 14] {
        [
            ("background", palette.background),
            ("sidebar", palette.sidebar),
            ("surface", palette.surface),
            ("border", palette.border),
            ("text", palette.text),
            ("text_secondary", palette.text_secondary),
            ("text_muted", palette.text_muted),
            ("accent", palette.accent),
            ("accent_text", palette.accent_text),
            ("action", palette.action),
            ("success", palette.success),
            ("danger", palette.danger),
            ("link", palette.link),
            ("selection", palette.selection),
        ]
    }

    /// WCAG contrast ratio.
    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let luminance = |color: Hsla| {
            let Rgba { r, g, b, .. } = color.into();
            let linear = |v: f32| {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    const LIGHT: &str = BUILT_IN[1].2;

    fn built_in() -> impl Iterator<Item = (ThemeName, &'static str, Theme)> {
        BUILT_IN.iter().map(|(name, _, source)| {
            let theme = Theme::parse(source).unwrap_or_else(|error| panic!("{name:?}: {error}"));
            (*name, *source, theme)
        })
    }

    fn themes() -> impl Iterator<Item = Theme> {
        built_in().map(|(_, _, theme)| theme)
    }

    #[test]
    fn built_in_themes_parse_with_their_labels() {
        assert_eq!(Theme::dark().appearance, Appearance::Dark);
        assert_eq!(Theme::light().appearance, Appearance::Light);
        for (name, _, theme) in built_in() {
            assert_eq!(theme.name, label(name));
            assert_eq!(Theme::built_in(name), Some(theme));
        }
    }

    #[test]
    fn every_name_but_system_and_custom_is_built_in_once() {
        let mut names: Vec<_> = ThemeName::CHOICES.iter().map(|(_, name)| *name).collect();
        names.retain(|name| !matches!(name, ThemeName::System | ThemeName::Custom));
        assert_eq!(BUILT_IN.len(), names.len());
        for (ix, (name, ..)) in BUILT_IN.iter().enumerate() {
            assert!(!BUILT_IN[..ix].iter().any(|(n, ..)| n == name), "{name:?}");
        }
        for name in names {
            assert!(Theme::built_in(name).is_some(), "{name:?}");
        }
        assert_eq!(listed().len(), BUILT_IN.len() + 1);
        assert_eq!(listed()[0], ThemeName::System);
        assert!(!listed().contains(&ThemeName::Custom));
    }

    #[test]
    fn text_is_readable() {
        for theme in themes() {
            let c = &theme.colors;
            for (fg, bg, min) in [
                (c.text, c.background, 7.),
                (c.text, c.sidebar, 7.),
                (c.text, c.surface, 4.5),
                (c.text_muted, c.background, 4.5),
                (c.text_muted, c.sidebar, 4.5),
                (c.accent_text, c.accent, 4.5),
                (c.accent_text, c.danger, 4.5),
                (c.link, c.background, 4.5),
                (c.action, c.background, 4.5),
                (c.action, c.surface, 4.5),
                (c.danger, c.background, 4.5),
            ] {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= min,
                    "{}: {fg:?} on {bg:?} is {ratio:.2}",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn tokens_list_every_color() {
        let names = tokens(&Theme::dark().colors).map(|(name, _)| name);
        assert_eq!(names, TOKENS.map(|(name, _)| name));
    }

    #[test]
    fn colors_reach_components_exactly() {
        for (_, source, theme) in built_in() {
            let file: toml::Table = toml::from_str(source).unwrap();
            let colors = file["colors"].as_table().unwrap();
            let opaque = |color: &toml::Value| {
                let written = color.as_str().unwrap();
                if written.len() == 7 {
                    format!("{written}ff")
                } else {
                    written.to_string()
                }
            };
            let tokens = tokens(&theme.colors);
            assert_eq!(colors.len(), tokens.len() + 1, "every token is set");
            for (name, color) in tokens {
                assert_eq!(hex(color), opaque(&colors[name]), "{name}");
            }
            let nicks = colors["nicks"].as_array().unwrap();
            let parsed: Vec<_> = theme.colors.nicks.iter().map(|nick| hex(*nick)).collect();
            assert_eq!(parsed, nicks.iter().map(opaque).collect::<Vec<_>>());
        }
    }

    #[test]
    fn names_are_quieter_than_text_and_louder_than_timestamps() {
        for theme in themes() {
            let c = &theme.colors;
            let text = contrast(c.text, c.background);
            let muted = contrast(c.text_muted, c.background);
            for name in c.nicks.iter().chain([&c.text_secondary]) {
                let ratio = contrast(*name, c.background);
                assert!(
                    muted < ratio && ratio < text,
                    "{}: {name:?} is {ratio:.2}, between {muted:.2} and {text:.2}",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn nick_colors_are_stable_and_spread() {
        let palette = Theme::dark().colors;
        let peer = |n: u128| ServiceId::new(uuid::Uuid::from_u128(n));
        assert_eq!(palette.nick(peer(7)), palette.nick(peer(7)));
        let mut used: Vec<_> = (0..200).map(|n| hex(palette.nick(peer(n)))).collect();
        used.sort();
        used.dedup();
        assert_eq!(used.len(), palette.nicks.len());
    }

    /// `config.toml` with `appearance.theme = "custom"` and `table` as
    /// `[theme]`.
    fn custom_config(table: &str) -> Config {
        let mut config = Config::default();
        config.appearance.theme = ThemeName::Custom;
        config.custom_theme = Some(toml::from_str(table).unwrap());
        config
    }

    #[test]
    fn custom_theme_is_a_full_theme_file_without_a_name() {
        let table = LIGHT.replace("name = \"Copiale Light\"\n", "");
        let theme = Theme::custom(&custom_config(&table)).unwrap();
        assert_eq!(theme.name, "Custom");
        assert_eq!(theme.appearance, Appearance::Light);
        assert_eq!(theme.colors, Theme::light().colors);
    }

    #[test]
    fn custom_theme_needs_every_token() {
        let table = LIGHT.replace("name = \"Copiale Light\"\n", "");
        for (broken, expected) in [
            (
                table.replace("link = \"#2a5db0\"\n", ""),
                "missing field `link`",
            ),
            (table.replace("#fbfbfa", "blue"), "blue"),
            (table.clone() + "\n[extra]\n", "unknown field `extra`"),
            (LIGHT.to_owned(), "unknown field `name`"),
        ] {
            let error = Theme::custom(&custom_config(&broken)).unwrap_err();
            assert!(error.starts_with("[theme]: "), "{error}");
            assert!(error.contains(expected), "{error}");
        }
        let mut config = Config::default();
        config.appearance.theme = ThemeName::Custom;
        assert_eq!(
            Theme::custom(&config).unwrap_err(),
            "no [theme] table in the config file"
        );
    }

    #[test]
    fn short_and_translucent_colors_parse() {
        let source = LIGHT
            .replace("\"#fbfbfa\"", "\"#fff\"")
            .replace("\"#3461c94c\"", "\"#3461c980\"");
        let colors = Theme::parse(&source).unwrap().colors;
        assert_eq!(hex(colors.background), "#ffffffff");
        assert_eq!(hex(colors.selection), "#3461c980");
    }

    #[test]
    fn rejects_bad_colors_and_unknown_or_missing_tokens() {
        for source in [
            LIGHT.replace("#fbfbfa", "#fbfbf"),
            LIGHT.replace("#fbfbfa", "fbfbfa"),
            LIGHT.replace("link =", "lnk ="),
            LIGHT.replace("link = \"#2a5db0\"", ""),
            LIGHT.replace("\"light\"", "\"dim\""),
            LIGHT[..LIGHT.find("nicks").unwrap()].to_string() + "nicks = []\n",
        ] {
            assert!(Theme::parse(&source).is_err(), "{source}");
        }
    }
}
