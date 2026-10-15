//! Built-in dark and light themes, following the system appearance.
//!
//! Views read colors from the active [`Palette`] only. GPUI Kit components
//! get the same tokens mapped onto their roles.

use std::rc::Rc;

use gpui_kit::component::{self, ThemeConfig, ThemeConfigColors};
use gpui_kit::{App, Global, Hsla, Rgba, SharedString, Window, WindowAppearance};
use serde::{Deserialize, Deserializer};

const DARK: &str = include_str!("../themes/dark.toml");
const LIGHT: &str = include_str!("../themes/light.toml");

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
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
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
    pub text_muted: Hsla,
    #[serde(deserialize_with = "color")]
    pub accent: Hsla,
    #[serde(deserialize_with = "color")]
    pub accent_text: Hsla,
    #[serde(deserialize_with = "color")]
    pub success: Hsla,
    #[serde(deserialize_with = "color")]
    pub danger: Hsla,
    #[serde(deserialize_with = "color")]
    pub link: Hsla,
    #[serde(deserialize_with = "color")]
    pub selection: Hsla,
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

    /// The built-in theme for a system appearance.
    pub fn for_system(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }
}

/// Applies the theme matching `window`'s appearance, and again whenever the
/// system appearance changes.
pub fn init(window: &mut Window, cx: &mut App) {
    follow_system(window.appearance(), cx);
    window
        .observe_window_appearance(|window, cx| follow_system(window.appearance(), cx))
        .detach();
}

/// Applies the built-in theme for `appearance`.
pub fn follow_system(appearance: WindowAppearance, cx: &mut App) {
    apply(&Theme::for_system(appearance), cx);
}

/// Makes `theme` active and redraws all windows.
pub fn apply(theme: &Theme, cx: &mut App) {
    cx.set_global(theme.colors);
    let config = Rc::new(component_config(theme));
    component::Theme::update(cx, |kit| kit.apply_config(&config));
}

/// Maps the palette onto GPUI Kit roles; the rest derive from these.
fn component_config(theme: &Theme) -> ThemeConfig {
    let c = &theme.colors;
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
        (&mut k.popover, c.background),
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
    let text = String::deserialize(deserializer)?;
    Rgba::try_from(text.as_str())
        .map(Hsla::from)
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(palette: &Palette) -> [(&'static str, Hsla); 12] {
        [
            ("background", palette.background),
            ("sidebar", palette.sidebar),
            ("surface", palette.surface),
            ("border", palette.border),
            ("text", palette.text),
            ("text_muted", palette.text_muted),
            ("accent", palette.accent),
            ("accent_text", palette.accent_text),
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
    fn system_appearance_picks_the_matching_theme() {
        for (system, expected) in [
            (WindowAppearance::Dark, Appearance::Dark),
            (WindowAppearance::VibrantDark, Appearance::Dark),
            (WindowAppearance::Light, Appearance::Light),
            (WindowAppearance::VibrantLight, Appearance::Light),
        ] {
            assert_eq!(Theme::for_system(system).appearance, expected);
        }
    }

    #[test]
    fn text_is_readable() {
        for theme in [Theme::dark(), Theme::light()] {
            let c = theme.colors;
            for (fg, bg, min) in [
                (c.text, c.background, 7.),
                (c.text, c.sidebar, 7.),
                (c.text, c.surface, 4.5),
                (c.text_muted, c.background, 4.5),
                (c.text_muted, c.sidebar, 4.5),
                (c.accent_text, c.accent, 4.5),
                (c.accent_text, c.danger, 4.5),
                (c.link, c.background, 4.5),
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
            assert_eq!(colors.len(), 12, "every token is set");
            for (name, color) in tokens(&theme.colors) {
                let written = colors[name].as_str().unwrap();
                let opaque = format!("{written}ff");
                let expected = if written.len() == 7 { &opaque } else { written };
                assert_eq!(hex(color), expected, "{name}");
            }
        }
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
        ] {
            assert!(Theme::parse(&source).is_err(), "{source}");
        }
    }
}
