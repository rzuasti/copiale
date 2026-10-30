//! `packaging/windows/AppxManifest.xml` against the app it describes.

use std::path::Path;

const MANIFEST: &str = include_str!("../../../packaging/windows/AppxManifest.xml");

/// The value of the first `attribute="…"` after `element`.
fn value(element: &str, attribute: &str) -> &'static str {
    let (_, rest) = MANIFEST.split_once(element).expect("element present");
    let (_, rest) = rest
        .split_once(&format!("{attribute}=\""))
        .expect("attribute present");
    rest.split_once('"').unwrap().0
}

#[test]
fn identifies_the_app() {
    assert_eq!(value("<Identity", "Name"), copiale_core::APP_ID);
}

#[test]
fn runs_the_binary() {
    assert_eq!(value("<Application ", "Executable"), "copiale.exe");
    assert_eq!(value("<desktop:ExecutionAlias", "Alias"), "copiale.exe");
}

#[test]
fn starts_at_login_only_if_enabled() {
    assert_eq!(value("<desktop:StartupTask", "Enabled"), "false");
}

#[test]
fn has_its_icons() {
    let logo = MANIFEST.split_once("<Logo>").unwrap().1;
    let logo = logo.split_once('<').unwrap().0;
    let tile = value("<uap:VisualElements", "Square150x150Logo");
    let small = value("<uap:VisualElements", "Square44x44Logo");
    let packaging = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows");
    for (icon, qualifier) in [
        (logo, "scale-100"),
        (tile, "scale-100"),
        (small, "scale-100"),
        (small, "targetsize-256_altform-unplated"),
    ] {
        let icon = packaging
            .join(icon.replace('\\', "/"))
            .with_extension(format!("{qualifier}.png"));
        assert!(icon.exists(), "{}", icon.display());
    }
}
