//! Built-in dark and light themes, following the system appearance.
//!
//! Views read colors from the active [`Palette`] only. GPUI Kit components
//! get the same tokens mapped onto their roles.

use std::rc::Rc;

use copiale_core::ServiceId;
use gpui_kit::component::{self, ThemeConfig, ThemeConfigColors};
use gpui_kit::{App, Global, Hsla, Rgba, SharedString};

use crate::settings::Settings;
use serde::{Deserialize, Deserializer};

mod fonts;
mod system;

pub use fonts::{Fonts, InstalledFonts};
pub(crate) use system::watch_settings;
pub use system::{Scheme, follow, follow_system, follow_window};

const DARK: &str = include_str!("../../themes/dark.toml");
const LIGHT: &str = include_str!("../../themes/light.toml");

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

/// Color tokens; see `themes/dark.toml` for what each one colors.
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

impl Theme {
    pub fn parse(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
    }

    pub fn dark() -> Self {
        Self::parse(DARK).expect("built-in dark theme is valid")
    }

    pub fn light() -> Self {
        Self::parse(LIGHT).expect("built-in light theme is valid")
    }
}

/// Makes `theme` active, with the settings' fonts and opacity, and redraws
/// all windows.
pub fn apply(theme: &Theme, cx: &mut App) {
    let settings = Settings::get(cx);
    let fonts = Fonts::resolve(&settings.typography, cx);
    let palette = theme.colors.translucent(settings.window.opacity as f32);
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
        (&mut k.scrollbar, c.background),
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

    #[test]
    fn built_in_themes_parse_with_their_appearance() {
        assert_eq!(Theme::dark().appearance, Appearance::Dark);
        assert_eq!(Theme::light().appearance, Appearance::Light);
    }

    #[test]
    fn text_is_readable() {
        for theme in [Theme::dark(), Theme::light()] {
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
    fn colors_reach_components_exactly() {
        for theme in [Theme::dark(), Theme::light()] {
            let source = if theme.appearance == Appearance::Dark {
                DARK
            } else {
                LIGHT
            };
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
        for theme in [Theme::dark(), Theme::light()] {
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
