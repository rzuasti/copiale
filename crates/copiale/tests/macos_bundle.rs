//! `packaging/macos/Info.plist` against the app it describes.

use std::path::Path;

const INFO_PLIST: &str = include_str!("../../../packaging/macos/Info.plist");

/// The string after `<key>key</key>`.
fn value(key: &str) -> &'static str {
    let key = format!("<key>{key}</key>");
    let (_, rest) = INFO_PLIST.split_once(&key).expect("key present");
    let rest = rest
        .trim_start()
        .strip_prefix("<string>")
        .expect("a string");
    rest.split_once("</string>").unwrap().0
}

#[test]
fn identifies_the_app() {
    assert_eq!(value("CFBundleIdentifier"), copiale_core::APP_ID);
}

#[test]
fn runs_the_binary() {
    let binary = Path::new(env!("CARGO_BIN_EXE_copiale"));
    assert_eq!(value("CFBundleExecutable"), binary.file_stem().unwrap());
}

#[test]
fn has_its_icon() {
    let icon = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/macos")
        .join(value("CFBundleIconFile"))
        .with_extension("svg");
    assert!(icon.exists(), "{}", icon.display());
}

#[test]
fn takes_the_version_from_the_workspace() {
    assert_eq!(value("CFBundleShortVersionString"), "@VERSION@");
    assert_eq!(value("CFBundleVersion"), "@VERSION@");
}

#[test]
fn needs_macos_14() {
    assert_eq!(value("LSMinimumSystemVersion"), "14.0");
}
