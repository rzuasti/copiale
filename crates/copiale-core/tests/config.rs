//! `config.toml` on disk: loading, writing and live reload.

use std::sync::mpsc;
use std::time::Duration;

use copiale_core::config::{
    Clock, Config, ConfigFile, Density, Loaded, MessageStyle, NotificationContent, Setting,
    ThemeMode, TimestampVisibility,
};

/// Generous: file watching on a loaded machine can be slow.
const WAIT: Duration = Duration::from_secs(5);

fn config_file() -> (tempfile::TempDir, ConfigFile) {
    let dir = tempfile::tempdir().unwrap();
    let file = ConfigFile::new(dir.path().join("copiale").join("config.toml"));
    (dir, file)
}

#[test]
fn missing_file_means_defaults() {
    let (_dir, file) = config_file();
    assert_eq!(file.load(), Loaded::default());
}

#[test]
fn write_creates_file_and_reads_back() {
    let (_dir, file) = config_file();

    file.write(&[Setting::Theme(ThemeMode::Dark), Setting::FontSize(16)])
        .unwrap();

    let loaded = file.load();
    assert_eq!(loaded.warnings, vec![]);
    assert_eq!(loaded.config.appearance.theme, ThemeMode::Dark);
    assert_eq!(loaded.config.typography.size, 16);
}

#[test]
fn set_matches_writing_and_reloading() {
    let (_dir, file) = config_file();
    let settings = [
        Setting::Theme(ThemeMode::Light),
        Setting::UiFont(Some("Inter".into())),
        Setting::MonoFont(Some(String::new())),
        Setting::BodyFont(None),
        Setting::FontSize(18),
        Setting::LineHeight(1.6),
        Setting::Density(Density::Compact),
        Setting::MessageStyle(MessageStyle::Bubbles),
        Setting::Timestamps(TimestampVisibility::Hover),
        Setting::Clock(Clock::H12),
        Setting::Avatars(true),
        Setting::Opacity(0.8),
        Setting::Blur(true),
        Setting::NotificationContent(NotificationContent::Message),
    ];

    file.write(&settings).unwrap();

    let mut expected = Config::default();
    for setting in &settings {
        expected.set(setting);
    }
    assert_eq!(file.load().config, expected);
    assert_ne!(expected, Config::default());
}

#[test]
fn write_keeps_user_comments() {
    let (_dir, file) = config_file();
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    std::fs::write(file.path(), "# mine\n[layout]\ndensity = \"cozy\" # note\n").unwrap();

    file.write(&[Setting::Density(Density::Roomy)]).unwrap();

    assert_eq!(
        std::fs::read_to_string(file.path()).unwrap(),
        "# mine\n[layout]\ndensity = \"roomy\" # note\n"
    );
}

#[test]
fn write_to_broken_file_fails_and_leaves_it_alone() {
    let (_dir, file) = config_file();
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    std::fs::write(file.path(), "[layout\n").unwrap();

    assert!(file.write(&[Setting::Blur(true)]).is_err());
    assert_eq!(std::fs::read_to_string(file.path()).unwrap(), "[layout\n");
}

#[cfg(unix)]
#[test]
fn write_through_symlink_keeps_the_link() {
    let (dir, file) = config_file();
    let dotfiles = dir.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    let target = dotfiles.join("copiale.toml");
    std::fs::write(&target, "# tracked in git\n").unwrap();
    std::os::unix::fs::symlink(&target, file.path()).unwrap();

    file.write(&[Setting::Avatars(true)]).unwrap();

    assert!(file.path().symlink_metadata().unwrap().is_symlink());
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# tracked in git\n\n[layout]\navatars = true\n"
    );
}

#[cfg(unix)]
#[test]
fn write_keeps_file_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, file) = config_file();
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    std::fs::write(file.path(), "").unwrap();
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    file.write(&[Setting::Blur(true)]).unwrap();

    let mode = std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn watcher_reports_external_edits() {
    let (_dir, file) = config_file();
    let (tx, rx) = mpsc::channel();
    let _watcher = file.watch(move |loaded| tx.send(loaded).unwrap()).unwrap();

    std::fs::write(file.path(), "[appearance]\ntheme = \"light\"\n").unwrap();

    let loaded = rx.recv_timeout(WAIT).expect("reload after edit");
    assert_eq!(loaded.config.appearance.theme, ThemeMode::Light);
}

#[test]
fn watcher_reports_warnings_and_skips_unchanged_saves() {
    let (_dir, file) = config_file();
    let (tx, rx) = mpsc::channel();
    let _watcher = file.watch(move |loaded| tx.send(loaded).unwrap()).unwrap();

    std::fs::write(file.path(), "[layout]\ndensity = \"tight\"\n").unwrap();
    let loaded = rx.recv_timeout(WAIT).expect("reload after edit");
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings.len(), 1);

    // Same settings and warnings: no callback.
    std::fs::write(file.path(), "# comment\n[layout]\ndensity = \"tight\"\n").unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(800)).is_err());
}

#[test]
fn watcher_sees_settings_ui_writes() {
    let (_dir, file) = config_file();
    let (tx, rx) = mpsc::channel();
    let _watcher = file.watch(move |loaded| tx.send(loaded).unwrap()).unwrap();

    file.write(&[Setting::Density(Density::Compact)]).unwrap();

    let loaded = rx.recv_timeout(WAIT).expect("reload after write");
    assert_eq!(loaded.config.layout.density, Density::Compact);
}

#[test]
fn dropping_watcher_stops_it() {
    let (_dir, file) = config_file();
    let (tx, rx) = mpsc::channel();
    let watcher = file.watch(move |loaded| tx.send(loaded).unwrap()).unwrap();

    drop(watcher);

    // The callback (and its sender) is dropped with the watcher thread.
    assert_eq!(
        rx.recv_timeout(WAIT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
}
