//! Applies settings to `config.toml` text, keeping everything else intact.

use toml_edit::{DocumentMut, Item};

use super::{ConfigError, Setting};

pub(crate) fn apply(text: &str, settings: &[Setting]) -> Result<String, ConfigError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| ConfigError::Syntax(error.to_string()))?;

    for setting in settings {
        let (table_name, key) = setting.key();
        setting.check().map_err(|message| ConfigError::Invalid {
            setting: format!("{table_name}.{key}"),
            message,
        })?;
        if doc.as_table().is_empty() {
            keep_header_on_top(&mut doc, table_name);
        }
        let table = doc
            .entry(table_name)
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()
            .ok_or(ConfigError::NotATable(table_name))?;

        match (setting.value(), table.get_mut(key)) {
            (None, _) => {
                table.remove(key);
            }
            // Replace in place: keeps the key's comments and position, and
            // the old value's trailing comment.
            (Some(mut value), Some(item)) => {
                if let Some(old) = item.as_value() {
                    *value.decor_mut() = old.decor().clone();
                }
                *item = Item::Value(value);
            }
            (Some(value), None) => {
                table.insert(key, Item::Value(value));
            }
        }
    }
    Ok(doc.to_string())
}

/// Adds the first table below a comment-only file's text, which toml_edit
/// otherwise keeps at the end.
fn keep_header_on_top(doc: &mut DocumentMut, table_name: &str) {
    let header = doc
        .trailing()
        .as_str()
        .map(str::trim_end)
        .filter(|header| !header.is_empty())
        .map(str::to_owned);
    let Some(header) = header else {
        return;
    };
    let mut table = toml_edit::Table::new();
    table.decor_mut().set_prefix(format!("{header}\n\n"));
    doc.insert(table_name, Item::Table(table));
    doc.set_trailing("");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse::parse;
    use crate::config::{Density, MessageStyle, ThemeName};

    #[test]
    fn creates_tables_in_an_empty_file() {
        let text = apply(
            "",
            &[Setting::Theme(ThemeName::CopialeLight), Setting::Blur(true)],
        )
        .unwrap();
        assert_eq!(
            text,
            "[appearance]\ntheme = \"copiale-light\"\n\n[window]\nblur = true\n"
        );
    }

    #[test]
    fn keeps_comments_order_and_unrelated_content() {
        let original = "\
# My config
[layout]
# How tight things are
density = \"cozy\" # was roomy
message_style = \"log\"

[custom]
keep = \"me\"
";
        let text = apply(original, &[Setting::Density(Density::Compact)]).unwrap();
        assert_eq!(text, original.replace("\"cozy\"", "\"compact\""));
    }

    #[test]
    fn adds_missing_key_to_existing_table() {
        let text = apply(
            "[layout]\ndensity = \"cozy\"\n",
            &[Setting::MessageStyle(MessageStyle::Blocks)],
        )
        .unwrap();
        assert_eq!(
            text,
            "[layout]\ndensity = \"cozy\"\nmessage_style = \"blocks\"\n"
        );
    }

    #[test]
    fn comment_only_file_keeps_comment_on_top() {
        let text = apply("# my settings\n", &[Setting::Blur(true)]).unwrap();
        assert_eq!(text, "# my settings\n\n[window]\nblur = true\n");
    }

    #[test]
    fn default_font_removes_key() {
        let text = apply(
            "[typography]\nui_font = \"Inter\"\nsize = 15\n",
            &[Setting::UiFont(None)],
        )
        .unwrap();
        assert_eq!(text, "[typography]\nsize = 15\n");
    }

    #[test]
    fn written_values_read_back() {
        let settings = [
            Setting::FontSize(18),
            Setting::LineHeight(1.25),
            Setting::Opacity(0.9),
            Setting::MonoFont(Some("Iosevka".into())),
            Setting::Hotkey("Super+Shift+K".parse().ok()),
        ];
        let loaded = parse(&apply("", &settings).unwrap());
        assert_eq!(loaded.warnings, vec![]);
        assert_eq!(loaded.config.typography.size, 18);
        assert_eq!(loaded.config.typography.line_height, 1.25);
        assert_eq!(loaded.config.window.opacity, 0.9);
        assert_eq!(
            loaded.config.typography.mono_font.as_deref(),
            Some("Iosevka")
        );
        assert_eq!(
            loaded.config.window.hotkey.map(|hotkey| hotkey.to_string()),
            Some("Shift+Super+K".into())
        );
    }

    #[test]
    fn rejects_out_of_range_values() {
        let result = apply("", &[Setting::Opacity(0.2)]);
        assert!(
            matches!(result, Err(ConfigError::Invalid { setting, .. }) if setting == "window.opacity")
        );
        let result = apply("", &[Setting::Hotkey("Shift+K".parse().ok())]);
        assert!(
            matches!(result, Err(ConfigError::Invalid { setting, .. }) if setting == "window.hotkey")
        );
    }

    #[test]
    fn refuses_to_edit_a_broken_file() {
        assert!(matches!(
            apply("[layout\n", &[Setting::Blur(true)]),
            Err(ConfigError::Syntax(_))
        ));
    }

    #[test]
    fn refuses_to_replace_a_non_table() {
        assert!(matches!(
            apply("layout = 3\n", &[Setting::Density(Density::Roomy)]),
            Err(ConfigError::NotATable("layout"))
        ));
    }
}
