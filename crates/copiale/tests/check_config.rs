//! `copiale --check-config FILE`.

use std::process::{Command, Output};

fn check(contents: &str) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, contents).unwrap();
    Command::new(env!("CARGO_BIN_EXE_copiale"))
        .arg("--check-config")
        .arg(&file)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_good_file_passes() {
    let theme = include_str!("../../copiale-ui/themes/nord.toml").replace("name = \"Nord\"\n", "");
    let theme = theme.replace("[colors]", "[theme.colors]");
    let output = check(&format!(
        "[appearance]\ntheme = \"custom\"\n[layout]\ndensity = \"roomy\"\n\n[theme]\n{theme}"
    ));
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "No problems found.\n"
    );
}

#[test]
fn bad_values_fail_naming_the_setting() {
    let output = check("[layout]\ndensity = \"tight\"\n[window]\nopacity = 2\n");
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(
        stderr.contains("layout.density: expected one of"),
        "{stderr}"
    );
    assert!(
        stderr.contains("window.opacity: must be between"),
        "{stderr}"
    );
}

#[test]
fn an_unusable_custom_theme_fails() {
    let output = check("[theme]\nappearance = \"dark\"\n[theme.colors]\ntext = \"#fff\"\n");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("[theme]: missing field"),
        "{}",
        stderr(&output)
    );
}
