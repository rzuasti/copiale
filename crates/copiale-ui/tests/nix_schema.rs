//! `nix/config-schema.json`: every setting and theme token, from which the
//! Nix modules build their options. Rewrite it with
//! `UPDATE_NIX_SCHEMA=1 cargo test -p copiale-ui --test nix_schema`.

use copiale_core::config::{KindInfo, Setting};
use copiale_ui::theme::{self, Appearance};
use serde_json::{Value, json};

const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../nix/config-schema.json");

fn schema() -> Value {
    let settings: Vec<Value> = Setting::info()
        .into_iter()
        .map(|setting| {
            let mut entry = json!({
                "table": setting.table,
                "key": setting.key,
                "description": setting.description,
            });
            let (kind, choices) = match setting.kind {
                KindInfo::Choice(choices) => ("choice", Some(choices)),
                KindInfo::Bool => ("bool", None),
                KindInfo::Integer => ("integer", None),
                KindInfo::Float => ("float", None),
                KindInfo::Font => ("font", None),
            };
            entry["type"] = kind.into();
            if let Some(choices) = choices {
                entry["choices"] = choices.into();
            }
            if let Some(default) = setting.default {
                entry["default"] = default;
            }
            if let Some((min, max)) = setting.range {
                entry["min"] = min;
                entry["max"] = max;
            }
            entry
        })
        .collect();
    let appearances = ["dark", "light"];
    for name in appearances {
        toml::Value::from(name)
            .try_into::<Appearance>()
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    json!({
        "settings": settings,
        "theme": {
            "appearances": appearances,
            "colors": theme::TOKENS
                .iter()
                .map(|(name, description)| json!({"name": name, "description": description}))
                .collect::<Vec<_>>(),
            "nicks": theme::NICKS,
        },
    })
}

#[test]
fn nix_schema_is_up_to_date() {
    let expected = serde_json::to_string_pretty(&schema()).unwrap() + "\n";
    if std::env::var_os("UPDATE_NIX_SCHEMA").is_some() {
        std::fs::write(PATH, &expected).unwrap();
        return;
    }
    let written = std::fs::read_to_string(PATH).unwrap_or_default();
    assert!(
        written == expected,
        "nix/config-schema.json is out of date; rewrite it with \
         UPDATE_NIX_SCHEMA=1 cargo test -p copiale-ui --test nix_schema"
    );
}
