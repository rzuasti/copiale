//! User settings from `config.toml`, applied live.

use copiale_core::config::{Config, ConfigFile, ConfigWatcher};
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
}

/// Loads the config file and keeps [`Settings`] in step with it.
pub fn load(file: ConfigFile, cx: &mut App) {
    let loaded = file.load();
    for warning in &loaded.warnings {
        tracing::warn!(%warning, "config");
    }
    cx.set_global(Settings(loaded.config));

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
