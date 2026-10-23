//! `config.toml` on disk: loading, writing and live reload.

use std::sync::mpsc;
use std::time::Duration;

use copiale_core::config::{
    Config, ConfigError, ConfigEvent, ConfigFile, ConfigSync, Density, Loaded, MessageStyle,
    NotificationContent, Setting, ThemeName, TimeFormat, TimestampVisibility,
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

    file.write(&[
        Setting::Theme(ThemeName::CopialeDark),
        Setting::FontSize(16),
    ])
    .unwrap();

    let loaded = file.load();
    assert_eq!(loaded.warnings, vec![]);
    assert_eq!(loaded.config.appearance.theme, ThemeName::CopialeDark);
    assert_eq!(loaded.config.typography.size, 16);
}

#[test]
fn set_matches_writing_and_reloading() {
    let (_dir, file) = config_file();
    let settings = [
        Setting::Theme(ThemeName::CopialeLight),
        Setting::UiFont(Some("Inter".into())),
        Setting::MonoFont(Some(String::new())),
        Setting::BodyFont(None),
        Setting::FontSize(18),
        Setting::LineHeight(1.6),
        Setting::Density(Density::Compact),
        Setting::MessageStyle(MessageStyle::Bubbles),
        Setting::Timestamps(TimestampVisibility::Hover),
        Setting::Clock(TimeFormat::H12),
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

/// A user file over two defaults files, `system` under `site`.
fn layered_file(system: &str, site: &str) -> (tempfile::TempDir, ConfigFile) {
    let dir = tempfile::tempdir().unwrap();
    let defaults: Vec<_> = [("system", system), ("site", site)]
        .into_iter()
        .map(|(name, text)| {
            let path = dir.path().join(name).join("config.toml");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        })
        .collect();
    let file = ConfigFile::with_defaults(dir.path().join("user").join("config.toml"), defaults);
    (dir, file)
}

#[test]
fn defaults_files_apply_under_the_user_file_in_order() {
    let (_dir, file) = layered_file(
        "[layout]\ndensity = \"roomy\"\nclock = \"12h\"\navatars = true\n",
        "[layout]\nclock = \"24h\"\ndensity = \"compact\"\n",
    );
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    std::fs::write(file.path(), "[layout]\ndensity = \"cozy\"\n").unwrap();

    let loaded = file.load();
    assert_eq!(loaded.warnings, vec![]);
    let layout = loaded.config.layout;
    assert_eq!(layout.density, Density::Cozy);
    assert_eq!(layout.clock, TimeFormat::H24);
    assert!(layout.avatars);
}

#[test]
fn custom_theme_merges_key_by_key() {
    let (_dir, file) = layered_file(
        "[theme]\nappearance = \"dark\"\n[theme.colors]\ntext = \"#eee\"\nlink = \"#00f\"\nnicks = [\"#111\", \"#222\"]\n",
        "",
    );
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    std::fs::write(
        file.path(),
        "[theme.colors]\nlink = \"#0f0\"\nnicks = [\"#333\"]\n",
    )
    .unwrap();

    let theme = file.load().config.custom_theme.unwrap();
    assert_eq!(
        theme,
        toml::from_str(
            "appearance = \"dark\"\n[colors]\ntext = \"#eee\"\nlink = \"#0f0\"\nnicks = [\"#333\"]\n"
        )
        .unwrap()
    );
}

#[test]
fn bad_defaults_warn_naming_the_file_and_fall_back() {
    let (dir, file) = layered_file("[layout]\ndensity = \"tight\"\nclock = \"12h\"\n", "[");
    let system = dir.path().join("system").join("config.toml");
    let site = dir.path().join("site").join("config.toml");

    let loaded = file.load();
    assert_eq!(loaded.config.layout.density, Density::Cozy);
    assert_eq!(loaded.config.layout.clock, TimeFormat::H12);
    let warnings: Vec<_> = loaded.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].starts_with("layout.density: expected one of"));
    assert!(warnings[0].ends_with(&format!("using cozy (in {})", system.display())));
    assert!(warnings[1].starts_with("syntax error"));
    assert!(warnings[1].ends_with(&format!("(in {})", site.display())));
}

#[test]
fn missing_defaults_files_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let file = ConfigFile::with_defaults(
        dir.path().join("config.toml"),
        vec![dir.path().join("none").join("config.toml")],
    );
    assert_eq!(file.load(), Loaded::default());
}

#[test]
fn writes_go_to_the_user_file_only() {
    let (dir, file) = layered_file("[layout]\navatars = true\n", "");

    file.write(&[Setting::Blur(true)]).unwrap();

    let system = dir.path().join("system").join("config.toml");
    assert_eq!(
        std::fs::read_to_string(system).unwrap(),
        "[layout]\navatars = true\n"
    );
    let config = file.load().config;
    assert!(config.window.blur && config.layout.avatars);
}

#[cfg(unix)]
#[test]
fn read_only_files_are_detected() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, file) = config_file();
    assert!(!file.read_only(), "missing");
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    std::fs::write(file.path(), "").unwrap();
    assert!(!file.read_only());
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(file.read_only());
}

/// A watching [`ConfigSync`] that saw the file as it is now.
fn sync(file: &ConfigFile) -> (ConfigSync, mpsc::Receiver<ConfigEvent>) {
    let (tx, rx) = mpsc::channel();
    let mut sync = file
        .sync(&file.load(), move |event| tx.send(event).unwrap())
        .unwrap();
    sync.watch().unwrap();
    (sync, rx)
}

fn changed(rx: &mpsc::Receiver<ConfigEvent>) -> (Loaded, u64) {
    match rx.recv_timeout(WAIT).expect("reload") {
        ConfigEvent::Changed { loaded, saved } => (loaded, saved),
        event => panic!("expected a reload, got {event:?}"),
    }
}

fn saved(rx: &mpsc::Receiver<ConfigEvent>) -> (u64, Vec<ConfigError>) {
    match rx.recv_timeout(WAIT).expect("save") {
        ConfigEvent::Saved { through, errors } => (through, errors),
        event => panic!("expected a save, got {event:?}"),
    }
}

/// Longer than the watcher's debounce.
const QUIET: Duration = Duration::from_millis(800);

#[test]
fn sync_reports_external_edits() {
    let (_dir, file) = config_file();
    let (_sync, rx) = sync(&file);

    std::fs::write(file.path(), "[appearance]\ntheme = \"light\"\n").unwrap();

    let (loaded, saved) = changed(&rx);
    assert_eq!(loaded.config.appearance.theme, ThemeName::CopialeLight);
    assert_eq!(saved, 0);
}

#[test]
fn sync_reports_warnings_and_skips_unchanged_saves() {
    let (_dir, file) = config_file();
    let (_sync, rx) = sync(&file);

    std::fs::write(file.path(), "[layout]\ndensity = \"tight\"\n").unwrap();
    let (loaded, _) = changed(&rx);
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings.len(), 1);

    // Same settings and warnings: no event.
    std::fs::write(file.path(), "# comment\n[layout]\ndensity = \"tight\"\n").unwrap();
    assert!(rx.recv_timeout(QUIET).is_err());
}

#[test]
fn sync_reports_edits_made_before_watching() {
    let (_dir, file) = config_file();
    let (tx, rx) = mpsc::channel();
    let mut sync = file
        .sync(&file.load(), move |event| tx.send(event).unwrap())
        .unwrap();
    file.write(&[Setting::Avatars(true)]).unwrap();

    sync.watch().unwrap();

    assert!(changed(&rx).0.config.layout.avatars);
}

#[test]
fn sync_merges_quick_writes_without_reloading_them() {
    let (_dir, file) = config_file();
    let (mut sync, rx) = sync(&file);

    let mut last = 0;
    for size in 10..=30 {
        last = sync.write(vec![Setting::FontSize(size)]);
    }

    loop {
        let (through, errors) = saved(&rx);
        assert!(errors.is_empty());
        if through == last {
            break;
        }
    }
    assert_eq!(file.load().config.typography.size, 30);
    // Its own writes: no reload.
    assert!(rx.recv_timeout(QUIET).is_err());
}

#[test]
fn sync_reports_external_edits_before_writing_over_them() {
    let (_dir, file) = config_file();
    let (mut sync, rx) = sync(&file);

    std::fs::write(file.path(), "[layout]\navatars = true\n").unwrap();
    let number = sync.write(vec![Setting::FontSize(20)]);

    let (loaded, saved_through) = changed(&rx);
    assert!(loaded.config.layout.avatars);
    assert_eq!(
        loaded.config.typography.size,
        Config::default().typography.size
    );
    assert_eq!(saved_through, 0);
    let (through, errors) = saved(&rx);
    assert_eq!(through, number);
    assert!(errors.is_empty());
    let config = file.load().config;
    assert!(config.layout.avatars);
    assert_eq!(config.typography.size, 20);
    assert!(rx.recv_timeout(QUIET).is_err());
}

#[test]
fn sync_skips_invalid_settings_only() {
    let (_dir, file) = config_file();
    let (mut sync, rx) = sync(&file);

    let number = sync.write(vec![Setting::FontSize(0), Setting::Avatars(true)]);

    let (through, errors) = saved(&rx);
    assert_eq!(through, number);
    assert!(matches!(errors[..], [ConfigError::Invalid { .. }]));
    assert!(file.load().config.layout.avatars);
}

#[test]
fn sync_reports_defaults_a_write_uncovers() {
    let (_dir, file) = layered_file("[typography]\nui_font = \"Inter\"\n", "");
    std::fs::create_dir_all(file.path().parent().unwrap()).unwrap();
    std::fs::write(file.path(), "[typography]\nui_font = \"Mine\"\n").unwrap();
    let (mut sync, rx) = sync(&file);

    let number = sync.write(vec![Setting::UiFont(None)]);

    let (loaded, saved_through) = changed(&rx);
    assert_eq!(loaded.config.typography.ui_font.as_deref(), Some("Inter"));
    assert_eq!(saved_through, number);
    let (through, errors) = saved(&rx);
    assert_eq!(through, number);
    assert!(errors.is_empty());
    // A write the defaults don't affect: no reload.
    let number = sync.write(vec![Setting::Blur(true)]);
    assert_eq!(saved(&rx).0, number);
    assert!(rx.recv_timeout(QUIET).is_err());
}

#[test]
fn dropping_sync_finishes_writes_and_stops() {
    let (_dir, file) = config_file();
    let (mut sync, rx) = sync(&file);

    sync.write(vec![Setting::Blur(true)]);
    drop(sync);

    assert_eq!(saved(&rx).0, 1);
    assert!(file.load().config.window.blur);
    // The callback (and its sender) is dropped with the thread.
    assert!(matches!(
        rx.recv_timeout(WAIT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}
