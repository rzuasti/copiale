//! User settings from `config.toml`, applied live.

use std::path::Path;
use std::sync::LazyLock;

use copiale_core::config::{
    Config, ConfigEvent, ConfigFile, ConfigSync, Loaded, Setting, ThemeName,
};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui_kit::{App, Global};

/// The current settings; defaults until loaded.
#[derive(Clone, Debug, Default)]
pub struct Settings(pub Config);

impl Global for Settings {}

impl Settings {
    /// The settings in effect.
    pub fn get(cx: &App) -> &Config {
        static DEFAULT: LazyLock<Config> = LazyLock::new(Config::default);
        cx.try_global::<Self>()
            .map_or(&DEFAULT, |settings| &settings.0)
    }

    /// Where settings are saved, once loaded.
    pub fn path(cx: &App) -> Option<&Path> {
        cx.try_global::<SettingsFile>()
            .map(|file| file.sync.file().path())
    }

    /// Whether the file can't be written, e.g. as Nix manages it; changes
    /// then last until restart.
    pub fn read_only(cx: &App) -> bool {
        cx.try_global::<SettingsFile>()
            .is_some_and(|file| file.read_only)
    }
}

/// Saves [`Settings`] to the file they come from.
struct SettingsFile {
    sync: ConfigSync,
    /// Changes not saved yet, by write number; kept over reloads.
    unsaved: Vec<(u64, Setting)>,
    /// As of loading or the last change.
    read_only: bool,
    /// Changes not saved as the file is read-only; kept until restart.
    session: Vec<Setting>,
}

impl Global for SettingsFile {}

/// Loads the config file and keeps [`Settings`] in step with it.
pub fn load(file: ConfigFile, cx: &mut App) {
    let (events, mut received) = unbounded();
    let started = load_with(
        file,
        move |event| {
            events.unbounded_send(event).ok();
        },
        cx,
    );
    if started {
        cx.spawn(async move |cx| {
            while let Some(event) = received.next().await {
                cx.update(|cx| apply(event, cx));
            }
        })
        .detach();
    }
}

/// Like [`load`], but hands file events to `on_event`, from another
/// thread, to pass to [`apply`]. Returns whether saving works.
pub fn load_with(
    file: ConfigFile,
    on_event: impl FnMut(ConfigEvent) + Send + 'static,
    cx: &mut App,
) -> bool {
    let loaded = file.load();
    for warning in &loaded.warnings {
        tracing::warn!(%warning, "config");
    }
    cx.set_global(Settings(loaded.config.clone()));

    let mut sync = match file.sync(&loaded, on_event) {
        Ok(sync) => sync,
        Err(error) => {
            tracing::warn!(%error, "settings won't be saved");
            return false;
        }
    };
    if let Err(error) = sync.watch() {
        tracing::warn!(%error, "config changes won't apply until restart");
    }
    let read_only = file.read_only();
    if read_only {
        tracing::info!("config file is read-only: changes last until restart");
    }
    cx.set_global(SettingsFile {
        sync,
        unsaved: Vec::new(),
        read_only,
        session: Vec::new(),
    });
    true
}

/// Applies a file event from [`load_with`]: reloads keep unsaved changes.
pub fn apply(event: ConfigEvent, cx: &mut App) {
    match event {
        ConfigEvent::Changed { loaded, saved } => {
            for warning in &loaded.warnings {
                tracing::warn!(%warning, "config");
            }
            let file = cx.global_mut::<SettingsFile>();
            file.unsaved.retain(|(number, _)| *number > saved);
            let mut config = loaded.config;
            for setting in file.unsaved.iter().map(|(_, setting)| setting) {
                config.set(setting);
            }
            for setting in &file.session {
                config.set(setting);
            }
            cx.set_global(Settings(config));
        }
        ConfigEvent::Saved { through, errors } => {
            for error in &errors {
                tracing::warn!(%error, "setting not saved");
            }
            let file = cx.global_mut::<SettingsFile>();
            file.unsaved.retain(|(number, _)| *number > through);
        }
    }
}

/// Applies a change without saving it, e.g. to preview it.
pub fn preview(setting: &Setting, cx: &mut App) {
    let mut config = Settings::get(cx).clone();
    config.set(setting);
    cx.set_global(Settings(config));
}

/// Applies a change now and saves it to the file, if loaded.
pub fn change(setting: Setting, cx: &mut App) {
    preview(&setting, cx);
    if !cx.has_global::<SettingsFile>() {
        return;
    }
    let file = cx.global_mut::<SettingsFile>();
    file.read_only = file.sync.file().read_only();
    if file.read_only {
        file.session.retain(|kept| kept.key() != setting.key());
        file.session.push(setting);
        return;
    }
    let number = file.sync.write(vec![setting.clone()]);
    file.unsaved.push((number, setting));
}

/// Everything wrong with `loaded`: its warnings, then an unusable custom
/// theme.
pub fn problems(loaded: &Loaded) -> Vec<String> {
    let config = &loaded.config;
    let mut problems: Vec<_> = loaded.warnings.iter().map(ToString::to_string).collect();
    if config.custom_theme.is_some() || config.appearance.theme == ThemeName::Custom {
        problems.extend(crate::theme::Theme::custom(config).err());
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use copiale_core::config::ConfigWarning;

    #[test]
    fn problems_list_warnings_then_the_custom_theme() {
        let mut loaded = Loaded::default();
        assert!(problems(&loaded).is_empty());

        loaded.warnings.push(ConfigWarning {
            setting: Some("layout.density".into()),
            message: "bad".into(),
        });
        loaded.config.appearance.theme = ThemeName::Custom;
        assert_eq!(
            problems(&loaded),
            ["layout.density: bad", "no [theme] table in the config file"]
        );

        // Checked when set, even if not in use.
        loaded.config.appearance.theme = ThemeName::Nord;
        loaded.config.custom_theme = Some(toml::Table::new());
        let found = problems(&loaded);
        assert_eq!(found.len(), 2);
        assert!(
            found[1].starts_with("[theme]: missing field"),
            "{}",
            found[1]
        );
    }
}
