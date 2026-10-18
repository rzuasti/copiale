//! Reading, writing and watching `config.toml` on disk.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
        match read(&self.path) {
            Ok(text) => parse::parse(&text),
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
    /// target, keeping the link (dotfile managers). Concurrent writes can
    /// lose each other's changes; [`ConfigSync`] serializes them.
    pub fn write(&self, settings: &[Setting]) -> Result<(), ConfigError> {
        let target = self.target();
        let text = read(&target)?;
        let updated = edit::apply(&text, settings)?;
        write_atomically(&target, updated.as_bytes())?;
        Ok(())
    }

    /// Starts a [`ConfigSync`] for the file, last seen as `loaded`; calls
    /// `on_event` from its thread.
    pub fn sync(
        &self,
        loaded: &Loaded,
        on_event: impl FnMut(ConfigEvent) + Send + 'static,
    ) -> std::io::Result<ConfigSync> {
        let (messages, received) = mpsc::channel();
        let file = self.clone();
        let last = loaded.clone();
        std::thread::Builder::new()
            .name("copiale-config".into())
            .spawn(move || file.run(last, received, on_event))?;
        Ok(ConfigSync {
            file: self.clone(),
            messages,
            written: 0,
            _watcher: None,
        })
    }

    /// The [`ConfigSync`] thread: one write or reload at a time.
    fn run(
        self,
        mut last: Loaded,
        received: mpsc::Receiver<Message>,
        mut on_event: impl FnMut(ConfigEvent),
    ) {
        let names: Vec<OsString> = self
            .watched()
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_os_string())
            .collect();
        let mut saved = 0;
        let mut dirty = false;
        loop {
            let message = if dirty {
                match received.recv_timeout(DEBOUNCE) {
                    Ok(message) => message,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        dirty = false;
                        let loaded = self.load();
                        if loaded != last {
                            last = loaded.clone();
                            on_event(ConfigEvent::Changed { loaded, saved });
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match received.recv() {
                    Ok(message) => message,
                    Err(mpsc::RecvError) => break,
                }
            };
            let Message::Write(mut through, mut settings) = message else {
                dirty |= message.concerns(&names);
                continue;
            };
            // Merge queued writes into one.
            while let Ok(message) = received.try_recv() {
                match message {
                    Message::Write(number, more) => {
                        through = number;
                        settings.extend(more);
                    }
                    message => dirty |= message.concerns(&names),
                }
            }
            let errors = self.save(&settings, &mut last, |loaded| {
                on_event(ConfigEvent::Changed { loaded, saved })
            });
            saved = through;
            on_event(ConfigEvent::Saved { through, errors });
        }
    }

    /// Applies each of `settings`, skipping invalid ones. Reports external
    /// changes to `last` found first through `on_change`.
    fn save(
        &self,
        settings: &[Setting],
        last: &mut Loaded,
        on_change: impl FnOnce(Loaded),
    ) -> Vec<ConfigError> {
        let target = self.target();
        let text = match read(&target) {
            Ok(text) => text,
            Err(error) => return vec![error.into()],
        };
        let found = parse::parse(&text);
        if found != *last {
            *last = found.clone();
            on_change(found);
        }

        let mut errors = Vec::new();
        let mut updated = text.clone();
        for setting in settings {
            match edit::apply(&updated, std::slice::from_ref(setting)) {
                Ok(text) => updated = text,
                Err(error) => errors.push(error),
            }
        }
        if updated != text {
            match write_atomically(&target, updated.as_bytes()) {
                Ok(()) => *last = parse::parse(&updated),
                Err(error) => errors.push(error.into()),
            }
        }
        errors
    }

    /// The file, and its target if it is a symlink.
    fn watched(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.path.clone()];
        let target = self.target();
        if target != self.path {
            paths.push(target);
        }
        paths
    }

    /// The file itself, or its target if it is a symlink.
    fn target(&self) -> PathBuf {
        std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone())
    }
}

/// What a [`ConfigSync`] reports.
#[derive(Debug)]
pub enum ConfigEvent {
    /// The file's settings or warnings changed other than through
    /// [`ConfigSync::write`]. `loaded` includes writes numbered up to `saved`.
    Changed { loaded: Loaded, saved: u64 },
    /// Writes numbered up to `through` are done; `errors` lists failures.
    Saved {
        through: u64,
        errors: Vec<ConfigError>,
    },
}

/// Writes a [`ConfigFile`] and watches it for external edits, from a
/// single thread so writes and reloads never overlap. Writes queued while
/// one runs are merged. Dropping it finishes queued writes, then stops.
pub struct ConfigSync {
    file: ConfigFile,
    messages: mpsc::Sender<Message>,
    written: u64,
    _watcher: Option<RecommendedWatcher>,
}

impl ConfigSync {
    pub fn file(&self) -> &ConfigFile {
        &self.file
    }

    /// Queues `settings` to be saved; returns the write's number, from 1.
    pub fn write(&mut self, settings: Vec<Setting>) -> u64 {
        self.written += 1;
        self.messages
            .send(Message::Write(self.written, settings))
            .ok();
        self.written
    }

    /// Also reports external edits, and changes since the `loaded` given to
    /// [`ConfigFile::sync`].
    pub fn watch(&mut self) -> Result<(), ConfigError> {
        let messages = self.messages.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            messages.send(Message::Fs(event)).ok();
        })?;
        // Watch directories, not the file: editors replace it on save. A
        // symlinked file also changes at its target.
        let mut dirs: Vec<PathBuf> = Vec::new();
        for path in self.file.watched() {
            let Some(dir) = path.parent() else {
                continue;
            };
            if !dirs.iter().any(|d| d == dir) {
                std::fs::create_dir_all(dir)?;
                watcher.watch(dir, RecursiveMode::NonRecursive)?;
                dirs.push(dir.to_path_buf());
            }
        }
        self._watcher = Some(watcher);
        self.messages.send(Message::Check).ok();
        Ok(())
    }
}

enum Message {
    Write(u64, Vec<Setting>),
    Fs(notify::Result<notify::Event>),
    /// Reload, in case the file changed before watching began.
    Check,
}

impl Message {
    /// Whether the file may have changed.
    fn concerns(&self, names: &[OsString]) -> bool {
        match self {
            Message::Write(..) => false,
            Message::Check => true,
            Message::Fs(Ok(event)) => event.paths.iter().any(|path| {
                path.file_name()
                    .is_some_and(|name| names.iter().any(|n| n == name))
            }),
            Message::Fs(Err(_)) => false,
        }
    }
}

/// The file's text; empty if missing.
fn read(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

/// Writes a sibling temp file, unique to this write, then renames it over
/// `path`.
fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = dir.join(name);

    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        std::fs::remove_file(&temp).ok();
    }
    written
}
