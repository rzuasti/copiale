//! User settings from `config.toml`, applied live.

use std::path::Path;

use copiale_core::config::{Config, ConfigEvent, ConfigFile, ConfigSync, Setting};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui_kit::{App, Global};

/// The current settings; defaults until loaded.
#[derive(Clone, Debug, Default)]
pub struct Settings(pub Config);

impl Global for Settings {}

impl Settings {
    /// The settings in effect.
    pub fn get(cx: &App) -> Config {
        cx.try_global::<Self>()
            .map(|settings| settings.0.clone())
            .unwrap_or_default()
    }

    /// Where settings are saved, once loaded.
    pub fn path(cx: &App) -> Option<&Path> {
        cx.try_global::<SettingsFile>()
            .map(|file| file.sync.file().path())
    }
}

/// Saves [`Settings`] to the file they come from.
struct SettingsFile {
    sync: ConfigSync,
    /// Changes not saved yet, by write number; kept over reloads.
    unsaved: Vec<(u64, Setting)>,
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
    cx.set_global(SettingsFile {
        sync,
        unsaved: Vec::new(),
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
            for (_, setting) in &file.unsaved {
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
    let mut config = Settings::get(cx);
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
    let number = file.sync.write(vec![setting.clone()]);
    file.unsaved.push((number, setting));
}
