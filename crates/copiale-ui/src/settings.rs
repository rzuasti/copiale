//! User settings from `config.toml`, applied live.

use std::path::Path;

use copiale_core::config::{Config, ConfigFile, ConfigWatcher, Setting};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui_kit::{App, AppContext as _, Global};

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
        cx.try_global::<SettingsFile>().map(|file| file.0.path())
    }
}

/// The file [`Settings`] come from.
struct SettingsFile(ConfigFile);

impl Global for SettingsFile {}

/// Loads the config file and keeps [`Settings`] in step with it.
pub fn load(file: ConfigFile, cx: &mut App) {
    let loaded = file.load();
    for warning in &loaded.warnings {
        tracing::warn!(%warning, "config");
    }
    cx.set_global(Settings(loaded.config));
    cx.set_global(SettingsFile(file.clone()));

    let (changes, mut changed) = unbounded();
    let watcher = match file.watch(move |loaded| {
        changes.unbounded_send(loaded).ok();
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::warn!(%error, "config changes won't apply until restart");
            return;
        }
    };
    cx.spawn(async move |cx| {
        let _watcher: ConfigWatcher = watcher;
        while let Some(loaded) = changed.next().await {
            for warning in &loaded.warnings {
                tracing::warn!(%warning, "config");
            }
            cx.update(|cx| cx.set_global(Settings(loaded.config)));
        }
    })
    .detach();
}

/// Applies a change now and saves it to the file, if loaded.
pub fn change(setting: Setting, cx: &mut App) {
    let mut config = Settings::get(cx);
    config.set(&setting);
    cx.set_global(Settings(config));
    let Some(SettingsFile(file)) = cx.try_global::<SettingsFile>() else {
        return;
    };
    let file = file.clone();
    cx.background_spawn(async move {
        if let Err(error) = file.write(&[setting]) {
            tracing::warn!(%error, "setting not saved");
        }
    })
    .detach();
}
