//! The settings overlay: every setting in one list, by section, filtered by
//! a query and changed from the keyboard.

use std::ops::Range;

use copiale_core::Account;
use copiale_core::config::{
    Choice, Config, Density, MessageStyle, NotificationContent, Setting, ThemeName, TimeFormat,
    TimestampVisibility, Typography, WindowConfig,
};

use crate::capture;
use crate::fuzzy::fuzzy_match;
use crate::theme::{self, Theme};

/// Most fonts the font picker lists.
pub const MAX_FONTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Typography,
    Layout,
    Composer,
    Window,
    Notifications,
    Privacy,
    Account,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Typography => "Typography",
            Self::Layout => "Layout",
            Self::Composer => "Composer",
            Self::Window => "Window",
            Self::Notifications => "Notifications",
            Self::Privacy => "Privacy",
            Self::Account => "Account",
        }
    }
}

/// A row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Theme,
    UiFont,
    MonoFont,
    BodyFont,
    FontSize,
    LineHeight,
    Density,
    MessageStyle,
    Timestamps,
    Clock,
    Avatars,
    Emoticons,
    Opacity,
    Blur,
    CaptureProtection,
    NotificationContent,
    ReadReceipts,
    TypingIndicators,
    DeviceName,
    Unlink,
}

/// All rows, in order.
pub const FIELDS: [Field; 20] = [
    Field::Theme,
    Field::UiFont,
    Field::MonoFont,
    Field::BodyFont,
    Field::FontSize,
    Field::LineHeight,
    Field::Density,
    Field::MessageStyle,
    Field::Timestamps,
    Field::Clock,
    Field::Avatars,
    Field::Emoticons,
    Field::Opacity,
    Field::Blur,
    Field::CaptureProtection,
    Field::NotificationContent,
    Field::ReadReceipts,
    Field::TypingIndicators,
    Field::DeviceName,
    Field::Unlink,
];

/// A row's value as shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// One of `options`; the current one at `selected`.
    Choice {
        options: Vec<&'static str>,
        selected: usize,
    },
    Number(String),
    Toggle(bool),
    /// Picked from a list; `None`: the built-in default.
    Pick(Option<String>),
    /// Read-only: set on the phone.
    Info(String),
    /// Something `Enter` does, not a value.
    Action(&'static str),
}

/// What `Enter` on a row does.
#[derive(Clone, Debug, PartialEq)]
pub enum Activation {
    Change(Setting),
    Pick(Picking),
    /// Ask before unlinking and wiping.
    ConfirmUnlink,
    Nothing,
}

/// What a picker picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picking {
    Font(FontRole),
    Theme,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontRole {
    Ui,
    Mono,
    Body,
}

impl FontRole {
    pub fn setting(self, font: Option<String>) -> Setting {
        match self {
            Self::Ui => Setting::UiFont(font),
            Self::Mono => Setting::MonoFont(font),
            Self::Body => Setting::BodyFont(font),
        }
    }

    pub fn current(self, config: &Config) -> Option<String> {
        let typography = &config.typography;
        match self {
            Self::Ui => typography.ui_font.clone(),
            Self::Mono => typography.mono_font.clone(),
            Self::Body => typography.body_font.clone(),
        }
    }
}

impl Field {
    pub fn section(self) -> Section {
        match self {
            Self::Theme => Section::Appearance,
            Self::UiFont | Self::MonoFont | Self::BodyFont | Self::FontSize | Self::LineHeight => {
                Section::Typography
            }
            Self::Density | Self::MessageStyle | Self::Timestamps | Self::Clock | Self::Avatars => {
                Section::Layout
            }
            Self::Emoticons => Section::Composer,
            Self::Opacity | Self::Blur | Self::CaptureProtection => Section::Window,
            Self::NotificationContent => Section::Notifications,
            Self::ReadReceipts | Self::TypingIndicators => Section::Privacy,
            Self::DeviceName | Self::Unlink => Section::Account,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::UiFont => "UI font",
            Self::MonoFont => "Mono font",
            Self::BodyFont => "Message font",
            Self::FontSize => "Size",
            Self::LineHeight => "Line height",
            Self::Density => "Density",
            Self::MessageStyle => "Message style",
            Self::Timestamps => "Timestamps",
            Self::Clock => "Clock",
            Self::Avatars => "Avatars",
            Self::Emoticons => "Convert emoticons",
            Self::Opacity => "Opacity",
            Self::Blur => "Blur",
            Self::CaptureProtection => "Capture protection",
            Self::NotificationContent => "Show in notifications",
            Self::ReadReceipts => "Read receipts",
            Self::TypingIndicators => "Typing indicators",
            Self::DeviceName => "Device name",
            Self::Unlink => "Unlink and wipe",
        }
    }

    /// Can't be changed here: set on the phone, or unsupported.
    pub fn read_only(self) -> bool {
        match self {
            Self::ReadReceipts | Self::TypingIndicators | Self::DeviceName => true,
            Self::CaptureProtection => !capture::SUPPORTED,
            _ => false,
        }
    }

    /// A short note under the row, if any.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::ReadReceipts | Self::TypingIndicators | Self::DeviceName => {
                Some("set on your phone")
            }
            Self::Blur => Some("where the compositor allows"),
            Self::CaptureProtection if capture::SUPPORTED => {
                Some("hides from screenshots and shares")
            }
            Self::CaptureProtection => Some(capture::unsupported_hint()),
            Self::Avatars => Some("in the chat list"),
            Self::Emoticons => Some(":) becomes 🙂 as you type"),
            Self::Theme => Some("custom ones in the config file"),
            Self::Unlink => Some("deletes messages on this device"),
            _ => None,
        }
    }

    /// Why the row's value isn't in effect, if it isn't.
    pub fn problem(self, config: &Config) -> Option<String> {
        match (self, config.appearance.theme) {
            (Self::Theme, ThemeName::Custom) => Theme::custom(config)
                .err()
                .map(|error| format!("{error}; using system")),
            _ => None,
        }
    }

    /// `account` is `None` until the core reports it.
    pub fn value(self, config: &Config, account: Option<&Account>) -> Value {
        let typography = &config.typography;
        let layout = &config.layout;
        let on_phone = |known: Option<bool>| {
            Value::Info(match known {
                Some(true) => "on".into(),
                Some(false) => "off".into(),
                None => "…".into(),
            })
        };
        match self {
            Self::Theme => Value::Pick(Some(theme::label(config.appearance.theme).into())),
            Self::UiFont => Value::Pick(typography.ui_font.clone()),
            Self::MonoFont => Value::Pick(typography.mono_font.clone()),
            Self::BodyFont => Value::Pick(typography.body_font.clone()),
            Self::FontSize => Value::Number(format!("{} px", typography.size)),
            Self::LineHeight => Value::Number(format!("{:.1}", typography.line_height)),
            Self::Density => choice(layout.density),
            Self::MessageStyle => choice(layout.message_style),
            Self::Timestamps => choice(layout.timestamps),
            Self::Clock => choice(layout.clock),
            Self::Avatars => Value::Toggle(layout.avatars),
            Self::Emoticons => Value::Toggle(config.composer.convert_emoticons),
            Self::Opacity => Value::Number(format!("{:.0}%", config.window.opacity * 100.)),
            Self::Blur => Value::Toggle(config.window.blur),
            Self::CaptureProtection if capture::SUPPORTED => {
                Value::Toggle(config.window.capture_protection)
            }
            Self::CaptureProtection => Value::Info("n/a".into()),
            Self::NotificationContent => choice(config.notifications.content),
            Self::ReadReceipts => on_phone(account.map(|a| a.settings.read_receipts)),
            Self::TypingIndicators => on_phone(account.map(|a| a.settings.typing_indicators)),
            Self::DeviceName => Value::Info(match account {
                Some(Account {
                    device_name: Some(name),
                    ..
                }) => name.clone(),
                Some(_) => "unknown".into(),
                None => "…".into(),
            }),
            Self::Unlink => Value::Action("↵"),
        }
    }

    /// The setting `step` places from the current value (`←`/`→`); `None`
    /// at either end, or if it can't change here.
    pub fn step(self, config: &Config, step: i32) -> Option<Setting> {
        let typography = &config.typography;
        let layout = &config.layout;
        let window = &config.window;
        let setting = match self {
            Self::Theme => {
                let themes = theme::listed();
                let ix = themes.iter().position(|t| *t == config.appearance.theme)?;
                Setting::Theme(*themes.get(ix.checked_add_signed(step as isize)?)?)
            }
            Self::FontSize => {
                let size = typography.size.checked_add_signed(step)?;
                Typography::SIZE_RANGE
                    .contains(&size)
                    .then_some(Setting::FontSize(size))?
            }
            Self::LineHeight => {
                let tenths = (typography.line_height * 10.).round() + f64::from(step);
                let value = tenths / 10.;
                within(value, &Typography::LINE_HEIGHT_RANGE)
                    .then_some(Setting::LineHeight(value))?
            }
            Self::Density => Setting::Density(nudge(layout.density, step)?),
            Self::MessageStyle => Setting::MessageStyle(nudge(layout.message_style, step)?),
            Self::Timestamps => Setting::Timestamps(nudge(layout.timestamps, step)?),
            Self::Clock => Setting::Clock(nudge(layout.clock, step)?),
            Self::Avatars => Setting::Avatars(!layout.avatars),
            Self::Emoticons => Setting::ConvertEmoticons(!config.composer.convert_emoticons),
            Self::Opacity => {
                let twentieths = (window.opacity * 20.).round() + f64::from(step);
                let value = twentieths / 20.;
                within(value, &WindowConfig::OPACITY_RANGE).then_some(Setting::Opacity(value))?
            }
            Self::Blur => Setting::Blur(!window.blur),
            Self::CaptureProtection if capture::SUPPORTED => {
                Setting::CaptureProtection(!window.capture_protection)
            }
            Self::NotificationContent => {
                Setting::NotificationContent(nudge(config.notifications.content, step)?)
            }
            Self::UiFont
            | Self::MonoFont
            | Self::BodyFont
            | Self::ReadReceipts
            | Self::TypingIndicators
            | Self::DeviceName
            | Self::CaptureProtection
            | Self::Unlink => return None,
        };
        Some(setting)
    }

    /// `Enter`: toggles, cycles choices, or opens a picker.
    pub fn activate(self, config: &Config) -> Activation {
        match self {
            Self::UiFont => Activation::Pick(Picking::Font(FontRole::Ui)),
            Self::MonoFont => Activation::Pick(Picking::Font(FontRole::Mono)),
            Self::BodyFont => Activation::Pick(Picking::Font(FontRole::Body)),
            Self::Theme => Activation::Pick(Picking::Theme),
            Self::Density => Activation::Change(Setting::Density(cycle(config.layout.density))),
            Self::MessageStyle => {
                Activation::Change(Setting::MessageStyle(cycle(config.layout.message_style)))
            }
            Self::Timestamps => {
                Activation::Change(Setting::Timestamps(cycle(config.layout.timestamps)))
            }
            Self::Clock => Activation::Change(Setting::Clock(cycle(config.layout.clock))),
            Self::NotificationContent => Activation::Change(Setting::NotificationContent(cycle(
                config.notifications.content,
            ))),
            Self::Avatars | Self::Emoticons | Self::Blur | Self::CaptureProtection => self
                .step(config, 1)
                .map_or(Activation::Nothing, Activation::Change),
            Self::Unlink => Activation::ConfirmUnlink,
            _ => Activation::Nothing,
        }
    }

    /// The setting picking option `ix` of a choice makes.
    pub fn choose(self, ix: usize) -> Option<Setting> {
        Some(match self {
            Self::Density => Setting::Density(nth::<Density>(ix)?),
            Self::MessageStyle => Setting::MessageStyle(nth::<MessageStyle>(ix)?),
            Self::Timestamps => Setting::Timestamps(nth::<TimestampVisibility>(ix)?),
            Self::Clock => Setting::Clock(nth::<TimeFormat>(ix)?),
            Self::NotificationContent => {
                Setting::NotificationContent(nth::<NotificationContent>(ix)?)
            }
            _ => return None,
        })
    }
}

fn choice<T: Choice>(current: T) -> Value {
    Value::Choice {
        options: T::CHOICES.iter().map(|(name, _)| *name).collect(),
        selected: position(current),
    }
}

fn position<T: Choice>(current: T) -> usize {
    T::CHOICES
        .iter()
        .position(|(_, value)| *value == current)
        .unwrap_or(0)
}

fn nth<T: Choice>(ix: usize) -> Option<T> {
    T::CHOICES.get(ix).map(|(_, value)| *value)
}

/// `step` places along, stopping at the ends.
fn nudge<T: Choice>(current: T, step: i32) -> Option<T> {
    let ix = position(current).checked_add_signed(step as isize)?;
    nth(ix)
}

/// The next choice, wrapping around.
fn cycle<T: Choice>(current: T) -> T {
    nth((position(current) + 1) % T::CHOICES.len()).unwrap_or(current)
}

/// In range, allowing for float error at the ends.
fn within(value: f64, range: &std::ops::RangeInclusive<f64>) -> bool {
    value > range.start() - 1e-9 && value < range.end() + 1e-9
}

/// The list's query and highlighted row, and the picker over it.
#[derive(Debug, Default)]
pub struct SettingsPanel {
    query: String,
    selected: usize,
    picker: Option<SettingPicker>,
    /// Asking whether to unlink and wipe.
    confirming_unlink: bool,
}

/// Picks a font or theme, previewing as it goes.
#[derive(Debug)]
pub struct SettingPicker {
    pub picking: Picking,
    /// The setting before picking, restored on cancel.
    pub original: Setting,
    query: String,
    selected: usize,
}

/// A picker entry.
#[derive(Clone, Debug, PartialEq)]
pub struct PickerEntry {
    pub setting: Setting,
    pub label: String,
    pub highlights: Vec<Range<usize>>,
}

impl SettingsPanel {
    /// Empty query, first row.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.selected = 0;
    }

    /// Rows matching the query, by label or section.
    pub fn fields(&self) -> Vec<Field> {
        FIELDS
            .into_iter()
            .filter(|field| {
                fuzzy_match(&self.query, field.label()).is_some()
                    || fuzzy_match(&self.query, field.section().label()).is_some()
            })
            .collect()
    }

    pub fn selected(&self) -> Option<Field> {
        self.fields().get(self.selected).copied()
    }

    /// Highlights `field`, if listed.
    pub fn select(&mut self, field: Field) {
        if let Some(ix) = self.fields().iter().position(|f| *f == field) {
            self.selected = ix;
        }
    }

    /// Moves the highlight, stopping at the ends.
    pub fn step(&mut self, step: isize) {
        let count = self.fields().len();
        self.selected = self
            .selected
            .saturating_add_signed(step)
            .min(count.saturating_sub(1));
    }

    /// Opens the picker on the current value.
    pub fn pick(&mut self, picking: Picking, config: &Config) {
        let original = match picking {
            Picking::Font(role) => role.setting(role.current(config)),
            Picking::Theme => Setting::Theme(config.appearance.theme),
        };
        self.picker = Some(SettingPicker {
            picking,
            original,
            query: String::new(),
            selected: 0,
        });
    }

    pub fn confirm_unlink(&mut self) {
        self.confirming_unlink = true;
    }

    pub fn cancel_unlink(&mut self) {
        self.confirming_unlink = false;
    }

    pub fn confirming_unlink(&self) -> bool {
        self.confirming_unlink
    }

    pub fn picker(&self) -> Option<&SettingPicker> {
        self.picker.as_ref()
    }

    pub fn picker_mut(&mut self) -> Option<&mut SettingPicker> {
        self.picker.as_mut()
    }

    /// Closes the picker; the setting restoring its original value, unless
    /// `keep`.
    pub fn close_picker(&mut self, keep: bool) -> Option<Setting> {
        let picker = self.picker.take()?;
        (!keep).then_some(picker.original)
    }
}

impl SettingPicker {
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.selected = 0;
    }

    /// Fonts: the default, then `installed` fonts matching the query, best
    /// first. Themes: all but custom, matching the query, best first.
    pub fn entries(&self, installed: &[String]) -> Vec<PickerEntry> {
        let (first, rest, max) = match self.picking {
            Picking::Font(role) => (
                Some((role.setting(None), "Default".to_owned())),
                installed
                    .iter()
                    .map(|font| (role.setting(Some(font.clone())), font.clone()))
                    .collect(),
                MAX_FONTS,
            ),
            Picking::Theme => (
                None,
                theme::listed()
                    .into_iter()
                    .map(|name| (Setting::Theme(name), theme::label(name).to_owned()))
                    .collect::<Vec<_>>(),
                usize::MAX,
            ),
        };
        let entry = |(setting, label): (Setting, String)| {
            let m = fuzzy_match(&self.query, &label)?;
            Some((
                m.score,
                PickerEntry {
                    setting,
                    label,
                    highlights: m.ranges,
                },
            ))
        };
        let mut matched: Vec<_> = rest.into_iter().filter_map(entry).collect();
        matched.sort_by_key(|(score, _)| -score);
        first
            .and_then(entry)
            .into_iter()
            .chain(matched.into_iter().take(max))
            .map(|(_, entry)| entry)
            .collect()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Moves the highlight among `count` entries, stopping at the ends.
    pub fn step(&mut self, step: isize, count: usize) {
        self.selected = self
            .selected
            .saturating_add_signed(step)
            .min(count.saturating_sub(1));
    }

    pub fn select(&mut self, ix: usize) {
        self.selected = ix;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use copiale_core::AccountSettings;
    use copiale_core::config::ConfigFile;

    use super::*;

    /// Applies `field`'s step, which must change it.
    fn stepped(config: &mut Config, field: Field, step: i32) {
        let setting = field.step(config, step).expect("a change");
        config.set(&setting);
    }

    /// Every change `field` offers from `config`.
    fn changes(field: Field, config: &Config) -> Vec<Setting> {
        let mut settings: Vec<_> = [-1, 1]
            .into_iter()
            .filter_map(|step| field.step(config, step))
            .collect();
        settings.extend((0..).map_while(|ix| field.choose(ix)));
        match field.activate(config) {
            Activation::Change(setting) => settings.push(setting),
            Activation::Pick(Picking::Font(role)) => {
                settings.push(role.setting(Some("Inter".into())));
                settings.push(role.setting(None));
            }
            Activation::Pick(Picking::Theme) => {
                settings.extend(theme::listed().into_iter().map(Setting::Theme));
                settings.push(Setting::Theme(ThemeName::Custom));
            }
            Activation::ConfirmUnlink | Activation::Nothing => {}
        }
        settings
    }

    #[test]
    fn every_change_round_trips_and_every_setting_has_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let file = ConfigFile::new(dir.path().join("config.toml"));
        let mut expected = Config::default();
        let mut rows = HashMap::new();
        for field in FIELDS {
            for setting in changes(field, &expected.clone()) {
                let row = rows.entry(setting.key()).or_insert(field);
                assert_eq!(*row, field, "{:?} is on two rows", setting.key());
                file.write(std::slice::from_ref(&setting)).unwrap();
                expected.set(&setting);
                let loaded = file.load();
                assert_eq!(loaded.warnings, vec![], "{setting:?}");
                assert_eq!(loaded.config, expected, "{setting:?}");
            }
        }
        let mut keys: Vec<_> = rows.into_keys().collect();
        let mut all = Setting::KEYS.to_vec();
        if !capture::SUPPORTED {
            all.retain(|key| *key != Setting::CaptureProtection(true).key());
        }
        keys.sort();
        all.sort();
        assert_eq!(keys, all);
    }

    #[test]
    fn empty_query_lists_every_field_by_section() {
        assert_eq!(SettingsPanel::default().fields(), FIELDS);
        let mut sections: Vec<_> = FIELDS.iter().map(|field| field.section()).collect();
        sections.dedup();
        for (ix, section) in sections.iter().enumerate() {
            assert!(!sections[..ix].contains(section), "{section:?} is split");
        }
    }

    #[test]
    fn query_filters_by_label_or_section() {
        let mut panel = SettingsPanel::default();
        panel.set_query("font");
        assert_eq!(
            panel.fields(),
            [Field::UiFont, Field::MonoFont, Field::BodyFont]
        );
        panel.set_query("privacy");
        assert_eq!(
            panel.fields(),
            [Field::ReadReceipts, Field::TypingIndicators]
        );
        panel.set_query("zzz");
        assert!(panel.fields().is_empty());
        assert_eq!(panel.selected(), None);
    }

    #[test]
    fn highlight_moves_within_the_list() {
        let mut panel = SettingsPanel::default();
        assert_eq!(panel.selected(), Some(Field::Theme));
        panel.step(-1);
        assert_eq!(panel.selected(), Some(Field::Theme));
        panel.step(2);
        assert_eq!(panel.selected(), Some(Field::MonoFont));
        panel.step(100);
        assert_eq!(panel.selected(), Some(Field::Unlink));
        panel.select(Field::Clock);
        assert_eq!(panel.selected(), Some(Field::Clock));
        panel.set_query("size");
        assert_eq!(panel.selected(), Some(Field::FontSize));
    }

    #[test]
    fn themes_step_through_the_list_and_pick_on_enter() {
        let mut config = Config::default();
        assert_eq!(
            Field::Theme.value(&config, None),
            Value::Pick(Some("System".into()))
        );
        assert_eq!(Field::Theme.step(&config, -1), None);
        stepped(&mut config, Field::Theme, 2);
        assert_eq!(config.appearance.theme, ThemeName::CopialeLight);
        assert_eq!(
            Field::Theme.value(&config, None),
            Value::Pick(Some("Copiale Light".into()))
        );
        config.appearance.theme = *theme::listed().last().unwrap();
        assert_eq!(Field::Theme.step(&config, 1), None);
        assert_eq!(
            Field::Theme.activate(&config),
            Activation::Pick(Picking::Theme)
        );
        assert_eq!(Field::Theme.choose(0), None);
        // Custom is set in the file only.
        config.appearance.theme = ThemeName::Custom;
        assert_eq!(Field::Theme.step(&config, 1), None);
        assert_eq!(Field::Theme.step(&config, -1), None);
    }

    #[test]
    fn unusable_custom_theme_is_a_problem() {
        let mut config = Config::default();
        assert_eq!(Field::Theme.problem(&config), None);
        config.appearance.theme = ThemeName::Custom;
        assert_eq!(
            Field::Theme.problem(&config).as_deref(),
            Some("no [theme] table in the config file; using system")
        );
        assert_eq!(
            Field::Theme.value(&config, None),
            Value::Pick(Some("Custom".into()))
        );
        assert_eq!(Field::Density.problem(&config), None);
    }

    #[test]
    fn choices_step_to_the_ends_and_cycle_on_enter() {
        let mut config = Config::default();
        assert_eq!(Field::Density.step(&config, 2), None);
        stepped(&mut config, Field::Density, 1);
        assert_eq!(config.layout.density, Density::Roomy);
        assert_eq!(
            Field::Density.activate(&config),
            Activation::Change(Setting::Density(Density::Compact))
        );
        assert_eq!(
            Field::Density.choose(0),
            Some(Setting::Density(Density::Compact))
        );
        assert_eq!(Field::Density.choose(3), None);
    }

    #[test]
    fn numbers_step_within_their_ranges() {
        let mut config = Config::default();
        stepped(&mut config, Field::FontSize, 1);
        assert_eq!(config.typography.size, 15);
        config.typography.size = 32;
        assert_eq!(Field::FontSize.step(&config, 1), None);

        stepped(&mut config, Field::LineHeight, 1);
        assert_eq!(config.typography.line_height, 1.5);
        assert_eq!(
            Field::LineHeight.value(&config, None),
            Value::Number("1.5".into())
        );
        config.typography.line_height = 2.0;
        assert_eq!(Field::LineHeight.step(&config, 1), None);
        config.typography.line_height = 1.0;
        assert_eq!(Field::LineHeight.step(&config, -1), None);

        assert_eq!(
            Field::Opacity.value(&config, None),
            Value::Number("100%".into())
        );
        assert_eq!(Field::Opacity.step(&config, 1), None);
        stepped(&mut config, Field::Opacity, -1);
        assert_eq!(config.window.opacity, 0.95);
        config.window.opacity = 0.5;
        assert_eq!(Field::Opacity.step(&config, -1), None);
    }

    #[test]
    fn toggles_flip_either_way() {
        let mut config = Config::default();
        stepped(&mut config, Field::Avatars, -1);
        assert!(config.layout.avatars);
        let Activation::Change(setting) = Field::Avatars.activate(&config) else {
            panic!("toggles change");
        };
        config.set(&setting);
        assert!(!config.layout.avatars);
        stepped(&mut config, Field::Blur, 1);
        assert!(config.window.blur);
        assert_eq!(Field::Emoticons.value(&config, None), Value::Toggle(true));
        stepped(&mut config, Field::Emoticons, 1);
        assert!(!config.composer.convert_emoticons);
    }

    #[test]
    fn capture_protection_changes_only_where_supported() {
        let mut config = Config::default();
        let field = Field::CaptureProtection;
        assert_eq!(field.read_only(), !capture::SUPPORTED);
        if capture::SUPPORTED {
            assert_eq!(field.value(&config, None), Value::Toggle(false));
            stepped(&mut config, field, 1);
            assert!(config.window.capture_protection);
        } else {
            config.window.capture_protection = true;
            assert_eq!(field.value(&config, None), Value::Info("n/a".into()));
            assert_eq!(field.step(&config, 1), None);
            assert_eq!(field.activate(&config), Activation::Nothing);
        }
    }

    #[test]
    fn account_rows_mirror_the_phone_and_do_not_change() {
        let config = Config::default();
        let account = Account {
            device_name: Some("Copiale on laptop".into()),
            settings: AccountSettings {
                read_receipts: true,
                typing_indicators: false,
            },
        };
        assert_eq!(
            Field::ReadReceipts.value(&config, Some(&account)),
            Value::Info("on".into())
        );
        assert_eq!(
            Field::TypingIndicators.value(&config, Some(&account)),
            Value::Info("off".into())
        );
        assert_eq!(
            Field::DeviceName.value(&config, Some(&account)),
            Value::Info("Copiale on laptop".into())
        );
        assert_eq!(
            Field::DeviceName.value(&config, None),
            Value::Info("…".into())
        );
        for field in [
            Field::ReadReceipts,
            Field::TypingIndicators,
            Field::DeviceName,
        ] {
            assert!(field.read_only());
            assert_eq!(field.step(&config, 1), None);
            assert_eq!(field.activate(&config), Activation::Nothing);
        }
    }

    #[test]
    fn unlink_asks_first_and_reset_forgets() {
        let config = Config::default();
        assert_eq!(Field::Unlink.activate(&config), Activation::ConfirmUnlink);
        assert_eq!(Field::Unlink.step(&config, 1), None);
        let mut panel = SettingsPanel::default();
        panel.confirm_unlink();
        assert!(panel.confirming_unlink());
        panel.cancel_unlink();
        assert!(!panel.confirming_unlink());
        panel.confirm_unlink();
        panel.reset();
        assert!(!panel.confirming_unlink());
    }

    #[test]
    fn message_styles_step_and_cycle() {
        let mut config = Config::default();
        assert_eq!(
            Field::MessageStyle.value(&config, None),
            Value::Choice {
                options: vec!["log", "blocks", "bubbles"],
                selected: 0,
            }
        );
        stepped(&mut config, Field::MessageStyle, 2);
        assert_eq!(config.layout.message_style, MessageStyle::Bubbles);
        assert_eq!(
            Field::MessageStyle.activate(&config),
            Activation::Change(Setting::MessageStyle(MessageStyle::Log))
        );
        assert_eq!(
            Field::MessageStyle.choose(1),
            Some(Setting::MessageStyle(MessageStyle::Blocks))
        );
    }

    fn labels(entries: Vec<PickerEntry>) -> Vec<String> {
        entries.into_iter().map(|entry| entry.label).collect()
    }

    #[test]
    fn font_picker_lists_default_then_matches_and_restores_on_cancel() {
        let mut config = Config::default();
        config.typography.ui_font = Some("Inter".into());
        let mut panel = SettingsPanel::default();
        assert_eq!(
            Field::UiFont.activate(&config),
            Activation::Pick(Picking::Font(FontRole::Ui))
        );
        panel.pick(Picking::Font(FontRole::Ui), &config);
        let installed: Vec<String> = ["Inter", "Iosevka", "Noto Sans", "Fira Code"]
            .map(String::from)
            .into();

        let picker = panel.picker_mut().unwrap();
        assert_eq!(
            labels(picker.entries(&installed)),
            ["Default", "Inter", "Iosevka", "Noto Sans", "Fira Code"]
        );
        assert_eq!(picker.entries(&installed)[0].setting, Setting::UiFont(None));
        picker.set_query("fira");
        let entries = picker.entries(&installed);
        assert_eq!(labels(entries.clone()), ["Fira Code"]);
        assert_eq!(
            entries[0].setting,
            Setting::UiFont(Some("Fira Code".into()))
        );
        picker.step(5, entries.len());
        assert_eq!(picker.selected(), 0);

        assert_eq!(
            panel.close_picker(false),
            Some(Setting::UiFont(Some("Inter".into())))
        );
        assert!(panel.picker().is_none());
        panel.pick(Picking::Font(FontRole::Mono), &config);
        assert_eq!(panel.close_picker(true), None);
    }

    #[test]
    fn font_picker_shows_at_most_max_fonts() {
        let mut panel = SettingsPanel::default();
        panel.pick(Picking::Font(FontRole::Body), &Config::default());
        let installed: Vec<String> = (0..20).map(|ix| format!("Font {ix}")).collect();
        let entries = panel.picker().unwrap().entries(&installed);
        assert_eq!(entries.len(), MAX_FONTS + 1);
        assert_eq!(entries[0].setting, Setting::BodyFont(None));
    }

    #[test]
    fn theme_picker_lists_every_theme_but_custom() {
        let mut config = Config::default();
        config.appearance.theme = ThemeName::Nord;
        let mut panel = SettingsPanel::default();
        panel.pick(Picking::Theme, &config);
        let picker = panel.picker_mut().unwrap();
        assert_eq!(picker.original, Setting::Theme(ThemeName::Nord));
        let entries = picker.entries(&[]);
        assert_eq!(entries.len(), theme::listed().len());
        assert_eq!(
            labels(entries.clone())[..3],
            ["System", "Copiale Dark", "Copiale Light"]
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.setting == Setting::Theme(ThemeName::Custom))
        );

        picker.set_query("rose");
        assert_eq!(
            labels(picker.entries(&[])),
            ["Rosé Pine", "Rosé Pine Moon", "Rosé Pine Dawn"]
        );
        assert_eq!(
            panel.close_picker(false),
            Some(Setting::Theme(ThemeName::Nord))
        );
    }
}
