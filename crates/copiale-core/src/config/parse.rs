//! Reads `config.toml` setting by setting; never fails.

use toml_edit::{DocumentMut, Item, Value};

use super::schema::{CUSTOM_THEME, Kind};
use super::{Config, ConfigWarning, Loaded, Setting};

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
    let mut config = Config::read(&mut reader);
    config.custom_theme = reader.custom_theme(text);
    Loaded {
        config,
        warnings: reader.warnings,
    }
}

pub(crate) struct Reader<'a> {
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
            if table_name == CUSTOM_THEME {
                continue;
            }
            if !Setting::KEYS.iter().any(|(table, _)| *table == table_name) {
                self.warn(table_name.into(), "unknown setting, ignored".into());
                continue;
            }
            let Some(table) = item.as_table_like() else {
                self.warn(table_name.into(), "expected a table, ignored".into());
                continue;
            };
            for (key, _) in table.iter() {
                if !Setting::KEYS.contains(&(table_name, key)) {
                    self.warn(
                        format!("{table_name}.{key}"),
                        "unknown setting, ignored".into(),
                    );
                }
            }
        }
    }

    /// The `[theme]` table, as is.
    fn custom_theme(&mut self, text: &str) -> Option<toml::Table> {
        if !self.doc.get(CUSTOM_THEME)?.is_table_like() {
            self.warn(CUSTOM_THEME.into(), "expected a table, ignored".into());
            return None;
        }
        // Valid TOML already, as `doc` parsed.
        match toml::from_str::<toml::Table>(text)
            .ok()?
            .remove(CUSTOM_THEME)
        {
            Some(toml::Value::Table(table)) => Some(table),
            _ => None,
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

    /// The setting at `table.key`, or `default` if absent, of the wrong type
    /// or rejected by `check`.
    pub(crate) fn read<T: Kind>(
        &mut self,
        table: &str,
        key: &str,
        default: T,
        check: impl Fn(&T) -> Result<(), String>,
    ) -> T {
        let Some(value) = self.value(table, key) else {
            return default;
        };
        let checked = match T::from_toml(value) {
            Some(read) => check(&read).map(|()| read),
            None => Err(format!(
                "expected {}, got {}",
                T::expected(),
                value.to_string().trim()
            )),
        };
        checked.unwrap_or_else(|message| {
            self.warn(
                format!("{table}.{key}"),
                format!("{message}; using {}", default.describe()),
            );
            default
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Density, MessageStyle, NotificationContent, ThemeName, TimeFormat, TimestampVisibility,
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

            [composer]
            convert_emoticons = false

            [window]
            opacity = 0.85
            blur = true
            capture_protection = true

            [notifications]
            content = "off"
            "#,
        );

        assert_eq!(loaded.warnings, vec![]);
        let config = loaded.config;
        assert_eq!(config.appearance.theme, ThemeName::CopialeDark);
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
        assert_eq!(config.layout.clock, TimeFormat::H12);
        assert!(config.layout.avatars);
        assert!(!config.composer.convert_emoticons);
        assert_eq!(config.window.opacity, 0.85);
        assert!(config.window.blur);
        assert!(config.window.capture_protection);
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
    fn warnings_name_the_expected_type_and_fallback() {
        let loaded =
            parse("[typography]\nsize = -1\nline_height = 3\nui_font = 1\n[layout]\navatars = 1\n");
        let warnings: Vec<_> = loaded.warnings.iter().map(|w| w.to_string()).collect();
        assert_eq!(
            warnings,
            [
                "typography.ui_font: expected a font name, got 1; using the default",
                "typography.size: expected a whole number, got -1; using 14",
                "typography.line_height: must be between 1 and 2, got 3; using 1.4",
                "layout.avatars: expected true or false, got 1; using false",
            ]
        );
    }

    #[test]
    fn keys_are_unique() {
        for (ix, key) in Setting::KEYS.iter().enumerate() {
            assert!(!Setting::KEYS[..ix].contains(key), "{key:?} repeats");
        }
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
    fn custom_theme_table_is_kept_as_is() {
        let loaded = parse(
            "[appearance]\ntheme = \"custom\"\n[theme]\nappearance = \"dark\"\n\
             [theme.colors]\nbackground = \"#000\"\nanything = 1\n",
        );
        assert_eq!(loaded.warnings, vec![]);
        assert_eq!(loaded.config.appearance.theme, ThemeName::Custom);
        let theme = loaded.config.custom_theme.unwrap();
        assert_eq!(theme["appearance"].as_str(), Some("dark"));
        assert_eq!(theme["colors"]["background"].as_str(), Some("#000"));
        assert_eq!(theme["colors"]["anything"].as_integer(), Some(1));

        let loaded = parse("theme = \"nord\"\n");
        assert_eq!(settings(&loaded), [Some("theme")]);
        assert_eq!(loaded.config.custom_theme, None);
    }

    #[test]
    fn old_theme_names_still_read() {
        let loaded = parse("[appearance]\ntheme = \"light\"\n");
        assert_eq!(loaded.warnings, vec![]);
        assert_eq!(loaded.config.appearance.theme, ThemeName::CopialeLight);
        let loaded = parse("[appearance]\ntheme = \"rose-pine-dawn\"\n");
        assert_eq!(loaded.config.appearance.theme, ThemeName::RosePineDawn);
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
