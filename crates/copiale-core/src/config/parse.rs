//! Reads `config.toml` setting by setting; never fails.

use std::ops::RangeInclusive;

use toml_edit::{DocumentMut, Item, Value};

use super::schema::{Choice, Typography, WindowConfig, in_range};
use super::{Config, ConfigWarning, Loaded};

/// Every table and key the file may contain.
const SCHEMA: &[(&str, &[&str])] = &[
    ("appearance", &["theme"]),
    (
        "typography",
        &["ui_font", "mono_font", "body_font", "size", "line_height"],
    ),
    (
        "layout",
        &["density", "message_style", "timestamps", "clock", "avatars"],
    ),
    ("window", &["opacity", "blur"]),
    ("notifications", &["content"]),
];

pub(crate) fn parse(text: &str) -> Loaded {
    let doc = match text.parse::<DocumentMut>() {
        Ok(doc) => doc,
        Err(error) => {
            return Loaded {
                config: Config::default(),
                warnings: vec![ConfigWarning {
                    setting: None,
                    message: format!("syntax error, using all defaults: {error}"),
                }],
            };
        }
    };

    let mut reader = Reader {
        doc: &doc,
        warnings: Vec::new(),
    };
    reader.check_unknown();

    let defaults = Config::default();
    let mut config = Config::default();
    config.appearance.theme = reader.choice("appearance", "theme", defaults.appearance.theme);

    let typography = &mut config.typography;
    typography.ui_font = reader.font("typography", "ui_font");
    typography.mono_font = reader.font("typography", "mono_font");
    typography.body_font = reader.font("typography", "body_font");
    typography.size = reader.integer(
        "typography",
        "size",
        Typography::SIZE_RANGE,
        defaults.typography.size,
    );
    typography.line_height = reader.float(
        "typography",
        "line_height",
        Typography::LINE_HEIGHT_RANGE,
        defaults.typography.line_height,
    );

    let layout = &mut config.layout;
    layout.density = reader.choice("layout", "density", defaults.layout.density);
    layout.message_style = reader.choice("layout", "message_style", defaults.layout.message_style);
    layout.timestamps = reader.choice("layout", "timestamps", defaults.layout.timestamps);
    layout.clock = reader.choice("layout", "clock", defaults.layout.clock);
    layout.avatars = reader.boolean("layout", "avatars", defaults.layout.avatars);

    config.window.opacity = reader.float(
        "window",
        "opacity",
        WindowConfig::OPACITY_RANGE,
        defaults.window.opacity,
    );
    config.window.blur = reader.boolean("window", "blur", defaults.window.blur);

    config.notifications.content =
        reader.choice("notifications", "content", defaults.notifications.content);

    Loaded {
        config,
        warnings: reader.warnings,
    }
}

struct Reader<'a> {
    doc: &'a DocumentMut,
    warnings: Vec<ConfigWarning>,
}

impl<'a> Reader<'a> {
    fn warn(&mut self, setting: String, message: String) {
        self.warnings.push(ConfigWarning {
            setting: Some(setting),
            message,
        });
    }

    fn check_unknown(&mut self) {
        for (table_name, item) in self.doc.iter() {
            let Some(keys) = SCHEMA
                .iter()
                .find(|(name, _)| *name == table_name)
                .map(|(_, keys)| keys)
            else {
                self.warn(table_name.into(), "unknown setting, ignored".into());
                continue;
            };
            let Some(table) = item.as_table_like() else {
                self.warn(table_name.into(), "expected a table, ignored".into());
                continue;
            };
            for (key, _) in table.iter() {
                if !keys.contains(&key) {
                    self.warn(
                        format!("{table_name}.{key}"),
                        "unknown setting, ignored".into(),
                    );
                }
            }
        }
    }

    /// The value at `table.key`, or `None` if absent or not a value.
    fn value(&mut self, table: &str, key: &str) -> Option<&'a Value> {
        let item = self.doc.get(table)?.as_table_like()?.get(key)?;
        match item {
            Item::Value(value) => Some(value),
            _ => {
                self.warn(
                    format!("{table}.{key}"),
                    "expected a value, using the default".into(),
                );
                None
            }
        }
    }

    fn choice<T: Choice>(&mut self, table: &str, key: &str, default: T) -> T {
        let Some(value) = self.value(table, key) else {
            return default;
        };
        if let Some(choice) = value.as_str().and_then(T::from_name) {
            return choice;
        }
        let names: Vec<_> = T::CHOICES.iter().map(|(name, _)| *name).collect();
        self.warn(
            format!("{table}.{key}"),
            format!(
                "expected one of {}, got {}; using {}",
                names.join(", "),
                value.to_string().trim(),
                default.name()
            ),
        );
        default
    }

    fn boolean(&mut self, table: &str, key: &str, default: bool) -> bool {
        let Some(value) = self.value(table, key) else {
            return default;
        };
        value.as_bool().unwrap_or_else(|| {
            self.warn(
                format!("{table}.{key}"),
                format!(
                    "expected true or false, got {}; using {default}",
                    value.to_string().trim()
                ),
            );
            default
        })
    }

    fn integer(&mut self, table: &str, key: &str, range: RangeInclusive<u32>, default: u32) -> u32 {
        let Some(value) = self.value(table, key) else {
            return default;
        };
        let checked = match value.as_integer().map(u32::try_from) {
            Some(Ok(number)) => in_range(&number, &range).map(|()| number),
            _ => Err(format!(
                "expected a whole number, got {}",
                value.to_string().trim()
            )),
        };
        self.or_default(table, key, checked, default)
    }

    /// Accepts integers too: `line_height = 1` means 1.0.
    fn float(&mut self, table: &str, key: &str, range: RangeInclusive<f64>, default: f64) -> f64 {
        let Some(value) = self.value(table, key) else {
            return default;
        };
        let number = value
            .as_float()
            .or_else(|| value.as_integer().map(|number| number as f64));
        let checked = match number {
            Some(number) => in_range(&number, &range).map(|()| number),
            None => Err(format!(
                "expected a number, got {}",
                value.to_string().trim()
            )),
        };
        self.or_default(table, key, checked, default)
    }

    /// Empty means the default font.
    fn font(&mut self, table: &str, key: &str) -> Option<String> {
        let value = self.value(table, key)?;
        match value.as_str() {
            Some("") => None,
            Some(font) => Some(font.to_string()),
            None => {
                self.warn(
                    format!("{table}.{key}"),
                    format!(
                        "expected a font name, got {}; using the default",
                        value.to_string().trim()
                    ),
                );
                None
            }
        }
    }

    fn or_default<T: std::fmt::Display>(
        &mut self,
        table: &str,
        key: &str,
        checked: Result<T, String>,
        default: T,
    ) -> T {
        checked.unwrap_or_else(|message| {
            self.warn(
                format!("{table}.{key}"),
                format!("{message}; using {default}"),
            );
            default
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Clock, Density, MessageStyle, NotificationContent, ThemeMode, TimestampVisibility,
    };

    fn settings(loaded: &Loaded) -> Vec<Option<&str>> {
        loaded
            .warnings
            .iter()
            .map(|w| w.setting.as_deref())
            .collect()
    }

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(parse(""), Loaded::default());
    }

    #[test]
    fn reads_every_setting() {
        let loaded = parse(
            r#"
            [appearance]
            theme = "dark"

            [typography]
            ui_font = "Inter"
            mono_font = "JetBrains Mono"
            body_font = ""
            size = 16
            line_height = 1.5

            [layout]
            density = "compact"
            message_style = "bubbles"
            timestamps = "hover"
            clock = "12h"
            avatars = true

            [window]
            opacity = 0.85
            blur = true

            [notifications]
            content = "off"
            "#,
        );

        assert_eq!(loaded.warnings, vec![]);
        let config = loaded.config;
        assert_eq!(config.appearance.theme, ThemeMode::Dark);
        assert_eq!(config.typography.ui_font.as_deref(), Some("Inter"));
        assert_eq!(
            config.typography.mono_font.as_deref(),
            Some("JetBrains Mono")
        );
        assert_eq!(config.typography.body_font, None);
        assert_eq!(config.typography.size, 16);
        assert_eq!(config.typography.line_height, 1.5);
        assert_eq!(config.layout.density, Density::Compact);
        assert_eq!(config.layout.message_style, MessageStyle::Bubbles);
        assert_eq!(config.layout.timestamps, TimestampVisibility::Hover);
        assert_eq!(config.layout.clock, Clock::H12);
        assert!(config.layout.avatars);
        assert_eq!(config.window.opacity, 0.85);
        assert!(config.window.blur);
        assert_eq!(config.notifications.content, NotificationContent::Off);
    }

    #[test]
    fn invalid_values_fall_back_per_setting() {
        let loaded = parse(
            r#"
            [appearance]
            theme = "solarized"

            [typography]
            size = 100
            line_height = "tall"
            mono_font = 3

            [layout]
            density = "compact"
            avatars = "yes"

            [window]
            opacity = 0.1
            "#,
        );

        let defaults = Config::default();
        let config = &loaded.config;
        assert_eq!(config.appearance.theme, defaults.appearance.theme);
        assert_eq!(config.typography.size, defaults.typography.size);
        assert_eq!(
            config.typography.line_height,
            defaults.typography.line_height
        );
        assert_eq!(config.typography.mono_font, None);
        assert_eq!(config.layout.avatars, defaults.layout.avatars);
        assert_eq!(config.window.opacity, defaults.window.opacity);
        // Valid neighbours still apply.
        assert_eq!(config.layout.density, Density::Compact);
        assert_eq!(
            settings(&loaded),
            [
                "appearance.theme",
                "typography.mono_font",
                "typography.size",
                "typography.line_height",
                "layout.avatars",
                "window.opacity",
            ]
            .map(Some)
        );
    }

    #[test]
    fn warning_explains_choices_and_fallback() {
        let loaded = parse("[layout]\ndensity = \"tight\"\n");
        assert_eq!(
            loaded.warnings[0].to_string(),
            "layout.density: expected one of compact, cozy, roomy, got \"tight\"; using cozy"
        );
    }

    #[test]
    fn integer_line_height_is_accepted() {
        let loaded = parse("[typography]\nline_height = 2\n");
        assert_eq!(loaded.warnings, vec![]);
        assert_eq!(loaded.config.typography.line_height, 2.0);
    }

    #[test]
    fn unknown_tables_and_keys_are_reported() {
        let loaded = parse("colour = 1\n[layout]\nfoo = 1\n[plugins]\nx = 1\n");
        assert_eq!(
            settings(&loaded),
            [Some("colour"), Some("layout.foo"), Some("plugins")]
        );
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn non_table_section_is_reported() {
        let loaded = parse("layout = \"compact\"\n");
        assert_eq!(settings(&loaded), [Some("layout")]);
    }

    #[test]
    fn syntax_error_means_defaults_and_one_warning() {
        let loaded = parse("[layout\ndensity = ");
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(loaded.warnings[0].setting, None);
        assert!(loaded.warnings[0].message.starts_with("syntax error"));
    }

    #[test]
    fn dotted_keys_and_inline_tables_work() {
        let loaded = parse("window.blur = true\nlayout = { density = \"roomy\" }\n");
        assert_eq!(loaded.warnings, vec![]);
        assert!(loaded.config.window.blur);
        assert_eq!(loaded.config.layout.density, Density::Roomy);
    }
}
