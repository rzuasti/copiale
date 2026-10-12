//! Fixture TOML themes mapped onto iced's palette, plus chat colors iced
//! has no role for.

use std::path::Path;

use fixtures::{Appearance, Rgb, ThemeError};
use iced::theme::{Mode, Palette};
use iced::{Color, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    /// First theme matching the system appearance.
    System,
    Named(usize),
}

/// Colors read by the views.
#[derive(Debug, Clone, Copy)]
pub struct ChatPalette {
    pub background: Color,
    pub sidebar: Color,
    pub surface: Color,
    pub border: Color,
    pub text: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub bubble_incoming: Color,
    pub bubble_incoming_text: Color,
    pub bubble_outgoing: Color,
    pub bubble_outgoing_text: Color,
    pub link: Color,
    pub selection: Color,
}

pub struct Themes {
    themes: Vec<fixtures::Theme>,
    choice: ThemeChoice,
    system: Mode,
    active: usize,
    iced: Theme,
    palette: ChatPalette,
}

impl Themes {
    /// Loads the theme files, falling back to the compiled-in copies.
    pub fn load() -> Self {
        let themes = load_dir(&fixtures::Theme::bundled_dir()).unwrap_or_else(|error| {
            eprintln!("theme files unavailable ({error}); using compiled themes");
            fixtures::Theme::bundled()
        });
        let mut this = Self {
            iced: to_iced(&themes[0]),
            palette: to_palette(&themes[0]),
            themes,
            choice: ThemeChoice::System,
            system: Mode::None,
            active: 0,
        };
        this.apply();
        this
    }

    pub fn themes(&self) -> &[fixtures::Theme] {
        &self.themes
    }

    pub fn choice(&self) -> ThemeChoice {
        self.choice
    }

    pub fn iced(&self) -> &Theme {
        &self.iced
    }

    pub fn palette(&self) -> &ChatPalette {
        &self.palette
    }

    pub fn select(&mut self, choice: ThemeChoice) {
        self.choice = choice;
        self.apply();
    }

    pub fn select_named(&mut self, name: &str) -> bool {
        match self
            .themes
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case(name))
        {
            Some(ix) => {
                self.select(ThemeChoice::Named(ix));
                true
            }
            None => false,
        }
    }

    pub fn set_system_mode(&mut self, mode: Mode) {
        self.system = mode;
        if self.choice == ThemeChoice::System {
            self.apply();
        }
    }

    /// Loads a theme file and switches to it.
    pub fn load_file(&mut self, path: &Path) -> Result<(), ThemeError> {
        let theme = fixtures::Theme::load(path)?;
        let ix = match self.themes.iter().position(|t| t.name == theme.name) {
            Some(ix) => {
                self.themes[ix] = theme;
                ix
            }
            None => {
                self.themes.push(theme);
                self.themes.len() - 1
            }
        };
        self.select(ThemeChoice::Named(ix));
        Ok(())
    }

    fn apply(&mut self) {
        self.active = match self.choice {
            ThemeChoice::Named(ix) => ix.min(self.themes.len() - 1),
            ThemeChoice::System => {
                let wanted = match self.system {
                    Mode::Dark => Appearance::Dark,
                    Mode::Light | Mode::None => Appearance::Light,
                };
                self.themes
                    .iter()
                    .position(|t| t.appearance == wanted)
                    .unwrap_or(0)
            }
        };
        let theme = &self.themes[self.active];
        self.iced = to_iced(theme);
        self.palette = to_palette(theme);
    }
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

/// iced derives its extended palette (hover, weak/strong shades) from these.
fn to_iced(theme: &fixtures::Theme) -> Theme {
    let c = &theme.colors;
    let base = match theme.appearance {
        Appearance::Light => Palette::LIGHT,
        Appearance::Dark => Palette::DARK,
    };
    Theme::custom(
        theme.name.clone(),
        Palette {
            background: color(c.background),
            text: color(c.text),
            primary: color(c.accent),
            ..base
        },
    )
}

fn to_palette(theme: &fixtures::Theme) -> ChatPalette {
    let c = &theme.colors;
    ChatPalette {
        background: color(c.background),
        sidebar: color(c.sidebar),
        surface: color(c.surface),
        border: color(c.border),
        text: color(c.text),
        text_muted: color(c.text_muted),
        accent: color(c.accent),
        accent_text: color(c.accent_text),
        bubble_incoming: color(c.bubble_incoming),
        bubble_incoming_text: color(c.bubble_incoming_text),
        bubble_outgoing: color(c.bubble_outgoing),
        bubble_outgoing_text: color(c.bubble_outgoing_text),
        link: color(c.link),
        selection: color(c.selection),
    }
}

pub fn color(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.r, rgb.g, rgb.b)
}
