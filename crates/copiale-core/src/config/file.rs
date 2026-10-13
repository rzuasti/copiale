//! Reading, writing and watching `config.toml` on disk.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::{ConfigError, ConfigWarning, Loaded, Setting, edit, parse};

/// Editors save in several steps; wait for quiet before reloading.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// The user's `config.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFile {
    path: PathBuf,
}

impl ConfigFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Platform config dir, e.g. `~/.config/copiale/config.toml` on Linux.
    pub fn platform() -> Option<Self> {
        Some(Self::new(
            crate::dirs::project_dirs()?
                .config_dir()
                .join("config.toml"),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the file; a missing file means defaults. Never fails.
    pub fn load(&self) -> Loaded {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => parse::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Loaded::default(),
            Err(error) => Loaded {
                warnings: vec![ConfigWarning {
                    setting: None,
                    message: format!(
                        "can't read {}: {error}; using all defaults",
                        self.path.display()
                    ),
                }],
                ..Loaded::default()
            },
        }
    }

    /// Applies `settings`, keeping comments and unrelated content.
    ///
    /// Replaces the file atomically. A symlinked file is updated at its
    /// target, keeping the link (dotfile managers).
    pub fn write(&self, settings: &[Setting]) -> Result<(), ConfigError> {
        let target = self.target();
        let text = match std::fs::read_to_string(&target) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        let updated = edit::apply(&text, settings)?;
        write_atomically(&target, updated.as_bytes())?;
        Ok(())
    }

    /// Calls `on_change` from a background thread whenever the file's
    /// settings or warnings change. Stops when the watcher is dropped.
    pub fn watch(
        &self,
        mut on_change: impl FnMut(Loaded) + Send + 'static,
    ) -> Result<ConfigWatcher, ConfigError> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx)?;

        // Watch directories, not the file: editors replace it on save. A
        // symlinked file also changes at its target.
        let mut paths = vec![self.path.clone()];
        let target = self.target();
        if target != self.path {
            paths.push(target);
        }
        let mut dirs: Vec<&Path> = Vec::new();
        let mut names = Vec::new();
        for path in &paths {
            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
                continue;
            };
            if !dirs.contains(&dir) {
                std::fs::create_dir_all(dir)?;
                watcher.watch(dir, RecursiveMode::NonRecursive)?;
                dirs.push(dir);
            }
            names.push(name.to_os_string());
        }

        let file = self.clone();
        let mut last = file.load();
        std::thread::Builder::new()
            .name("copiale-config".into())
            .spawn(move || {
                while let Ok(event) = rx.recv() {
                    if !concerns(&event, &names) {
                        continue;
                    }
                    while rx.recv_timeout(DEBOUNCE).is_ok() {}
                    let loaded = file.load();
                    if loaded != last {
                        last = loaded.clone();
                        on_change(loaded);
                    }
                }
            })?;
        Ok(ConfigWatcher { _watcher: watcher })
    }

    /// The file itself, or its target if it is a symlink.
    fn target(&self) -> PathBuf {
        std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone())
    }
}

/// Keeps a [`ConfigFile::watch`] running.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

fn concerns(event: &notify::Result<notify::Event>, names: &[OsString]) -> bool {
    let Ok(event) = event else {
        return false;
    };
    event.paths.iter().any(|path| {
        path.file_name()
            .is_some_and(|name| names.iter().any(|n| n == name))
    })
}

/// Writes a sibling temp file, then renames it over `path`.
fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".tmp");
    let temp = dir.join(name);

    let mut file = std::fs::File::create(&temp)?;
    if let Ok(metadata) = std::fs::metadata(path) {
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(contents)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}
