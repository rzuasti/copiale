use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::color::Rgb;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    pub colors: Palette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

/// Semantic color tokens.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub background: Rgb,
    pub sidebar: Rgb,
    pub surface: Rgb,
    pub border: Rgb,
    pub text: Rgb,
    pub text_muted: Rgb,
    pub accent: Rgb,
    pub accent_text: Rgb,
    pub bubble_incoming: Rgb,
    pub bubble_incoming_text: Rgb,
    pub bubble_outgoing: Rgb,
    pub bubble_outgoing_text: Rgb,
    pub link: Rgb,
    pub selection: Rgb,
}

#[derive(Debug)]
pub enum ThemeError {
    Io(PathBuf, std::io::Error),
    Parse(toml::de::Error),
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "reading {}: {error}", path.display()),
            Self::Parse(error) => write!(f, "parsing theme: {error}"),
        }
    }
}

impl std::error::Error for ThemeError {}

impl Theme {
    pub fn from_toml(source: &str) -> Result<Self, ThemeError> {
        toml::from_str(source).map_err(ThemeError::Parse)
    }

    pub fn load(path: &Path) -> Result<Self, ThemeError> {
        let source =
            std::fs::read_to_string(path).map_err(|e| ThemeError::Io(path.to_path_buf(), e))?;
        Self::from_toml(&source)
    }

    /// Directory with the bundled theme files, for testing file loading and
    /// live reload.
    pub fn bundled_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("themes")
    }

    /// Bundled themes, compiled in.
    pub fn bundled() -> Vec<Self> {
        [
            include_str!("../themes/catppuccin-mocha.toml"),
            include_str!("../themes/catppuccin-latte.toml"),
            include_str!("../themes/gruvbox-dark.toml"),
        ]
        .into_iter()
        .map(|source| Self::from_toml(source).expect("bundled themes are valid"))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_parse() {
        let themes = Theme::bundled();
        assert_eq!(themes.len(), 3);
        assert!(themes.iter().any(|t| t.appearance == Appearance::Light));
        assert!(themes.iter().any(|t| t.appearance == Appearance::Dark));
    }

    #[test]
    fn bundled_dir_matches_compiled_themes() {
        let mut from_files: Vec<Theme> = std::fs::read_dir(Theme::bundled_dir())
            .unwrap()
            .map(|entry| Theme::load(&entry.unwrap().path()).unwrap())
            .collect();
        from_files.sort_by(|a, b| a.name.cmp(&b.name));
        let mut compiled = Theme::bundled();
        compiled.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(from_files, compiled);
    }

    #[test]
    fn rejects_invalid_color_and_unknown_token() {
        let valid = include_str!("../themes/gruvbox-dark.toml");
        assert!(Theme::from_toml(&valid.replace("#282828", "#28282")).is_err());
        assert!(Theme::from_toml(&valid.replace("link =", "lnk =")).is_err());
    }
}
