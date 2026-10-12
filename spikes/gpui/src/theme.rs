//! Fixture TOML themes applied to GPUI Kit's theme, plus the chat-specific
//! colors GPUI Kit has no role for.

use std::path::Path;
use std::rc::Rc;

use fixtures::{Appearance, Rgb, ThemeError};
use gpui_kit::component::{Theme, ThemeConfig};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, rgb};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    /// First theme matching the system appearance.
    System,
    Named(usize),
}

/// Loaded themes and the active selection.
pub struct Themes {
    themes: Vec<fixtures::Theme>,
    choice: ThemeChoice,
    active: usize,
}

impl Global for Themes {}

/// Chat colors read by the views.
#[derive(Debug, Clone, Copy)]
pub struct ChatPalette {
    pub bubble_incoming: Hsla,
    pub bubble_incoming_text: Hsla,
    pub bubble_outgoing: Hsla,
    pub bubble_outgoing_text: Hsla,
    pub text_muted: Hsla,
    pub accent: Hsla,
}

impl Global for ChatPalette {}

impl Themes {
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn themes(&self) -> &[fixtures::Theme] {
        &self.themes
    }

    pub fn choice(&self) -> ThemeChoice {
        self.choice
    }

    pub fn active(&self) -> &fixtures::Theme {
        &self.themes[self.active]
    }
}

/// Loads the theme files and applies the system-matching theme.
pub fn init(window: &mut Window, cx: &mut App) {
    let themes = load_dir(&fixtures::Theme::bundled_dir()).unwrap_or_else(|error| {
        eprintln!("theme files unavailable ({error}); using compiled themes");
        fixtures::Theme::bundled()
    });
    cx.set_global(Themes {
        themes,
        choice: ThemeChoice::System,
        active: 0,
    });
    select(ThemeChoice::System, window, cx);
}

/// Switches theme at runtime.
pub fn select(choice: ThemeChoice, window: &mut Window, cx: &mut App) {
    let appearance = window.appearance();
    let themes = cx.global_mut::<Themes>();
    themes.choice = choice;
    themes.active = resolve(&themes.themes, choice, appearance);
    let theme = themes.themes[themes.active].clone();
    apply(&theme, cx);
}

/// Re-resolves a `System` choice after the window appearance changed.
pub fn appearance_changed(window: &mut Window, cx: &mut App) {
    if Themes::global(cx).choice == ThemeChoice::System {
        select(ThemeChoice::System, window, cx);
    }
}

/// Loads a theme file and switches to it.
pub fn load_file(path: &Path, window: &mut Window, cx: &mut App) -> Result<(), ThemeError> {
    let theme = fixtures::Theme::load(path)?;
    let themes = cx.global_mut::<Themes>();
    let ix = match themes.themes.iter().position(|t| t.name == theme.name) {
        Some(ix) => {
            themes.themes[ix] = theme;
            ix
        }
        None => {
            themes.themes.push(theme);
            themes.themes.len() - 1
        }
    };
    select(ThemeChoice::Named(ix), window, cx);
    Ok(())
}

fn load_dir(dir: &Path) -> Result<Vec<fixtures::Theme>, ThemeError> {
    let entries = std::fs::read_dir(dir).map_err(|e| ThemeError::Io(dir.to_path_buf(), e))?;
    let mut themes = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| ThemeError::Io(dir.to_path_buf(), e))?
            .path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            themes.push(fixtures::Theme::load(&path)?);
        }
    }
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(themes)
}

fn resolve(themes: &[fixtures::Theme], choice: ThemeChoice, system: WindowAppearance) -> usize {
    match choice {
        ThemeChoice::Named(ix) => ix.min(themes.len() - 1),
        ThemeChoice::System => {
            let wanted = match system {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            };
            themes
                .iter()
                .position(|t| t.appearance == wanted)
                .unwrap_or(0)
        }
    }
}

fn apply(theme: &fixtures::Theme, cx: &mut App) {
    let config = Rc::new(theme_config(theme));
    Theme::update(cx, |gpui_theme| gpui_theme.apply_config(&config));

    let colors = &theme.colors;
    cx.set_global(ChatPalette {
        bubble_incoming: hsla(colors.bubble_incoming),
        bubble_incoming_text: hsla(colors.bubble_incoming_text),
        bubble_outgoing: hsla(colors.bubble_outgoing),
        bubble_outgoing_text: hsla(colors.bubble_outgoing_text),
        text_muted: hsla(colors.text_muted),
        accent: hsla(colors.accent),
    });
}

/// Maps the fixture palette onto GPUI Kit roles; unmapped roles derive from
/// these through GPUI Kit's fallbacks.
fn theme_config(theme: &fixtures::Theme) -> ThemeConfig {
    let c = &theme.colors;
    let hex = |color: Rgb| color.to_string();
    let value = json!({
        "name": theme.name,
        "mode": match theme.appearance {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        },
        "colors": {
            "background": hex(c.background),
            "foreground": hex(c.text),
            "border": hex(c.border),
            "input.border": hex(c.border),
            "muted.background": hex(c.surface),
            "muted.foreground": hex(c.text_muted),
            "accent.background": hex(c.surface),
            "accent.foreground": hex(c.text),
            "primary.background": hex(c.accent),
            "primary.foreground": hex(c.accent_text),
            "secondary.background": hex(c.surface),
            "secondary.foreground": hex(c.text),
            "popover.background": hex(c.background),
            "popover.foreground": hex(c.text),
            "list.background": hex(c.sidebar),
            "list.hover.background": hex(c.surface),
            "list.active.background": hex(c.surface),
            "list.active.border": hex(c.accent),
            "sidebar.background": hex(c.sidebar),
            "sidebar.foreground": hex(c.text),
            "sidebar.border": hex(c.border),
            "sidebar.accent.background": hex(c.surface),
            "sidebar.accent.foreground": hex(c.text),
            "sidebar.primary.background": hex(c.accent),
            "sidebar.primary.foreground": hex(c.accent_text),
            "title_bar.background": hex(c.sidebar),
            "title_bar.border": hex(c.border),
            "selection.background": hex(c.selection),
            "link": hex(c.link),
            "link.hover": hex(c.link),
            "link.active": hex(c.link),
            "ring": hex(c.accent),
            "caret": hex(c.accent),
            "scrollbar.background": hex(c.background),
            "scrollbar.thumb.background": hex(c.border),
            "scrollbar.thumb.hover.background": hex(c.text_muted),
        },
    });
    serde_json::from_value(value).expect("theme config schema")
}

pub fn hsla(color: Rgb) -> Hsla {
    rgb(u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b)).into()
}
