//! A global hotkey, written as `Ctrl+Alt+Space`.

use std::fmt;
use std::str::FromStr;

/// Other names read, never written.
const KEY_ALIASES: &[(&str, &str)] = &[
    ("Return", "Enter"),
    ("Esc", "Escape"),
    ("Del", "Delete"),
    ("Ins", "Insert"),
    ("PgUp", "PageUp"),
    ("PgDn", "PageDown"),
];

/// A key with modifiers, pressed anywhere in the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Cmd on macOS, the Windows key on Windows.
    pub super_key: bool,
    /// One of [`Self::KEYS`].
    pub key: &'static str,
}

impl Hotkey {
    /// Keys a hotkey can use, by their written name.
    pub const KEYS: &[&str] = &[
        "A",
        "B",
        "C",
        "D",
        "E",
        "F",
        "G",
        "H",
        "I",
        "J",
        "K",
        "L",
        "M",
        "N",
        "O",
        "P",
        "Q",
        "R",
        "S",
        "T",
        "U",
        "V",
        "W",
        "X",
        "Y",
        "Z",
        "0",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
        "F13",
        "F14",
        "F15",
        "F16",
        "F17",
        "F18",
        "F19",
        "F20",
        "Space",
        "Enter",
        "Tab",
        "Backspace",
        "Escape",
        "Insert",
        "Delete",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Up",
        "Down",
        "Left",
        "Right",
        "`",
        "-",
        "=",
        "[",
        "]",
        "\\",
        ";",
        "'",
        ",",
        ".",
        "/",
    ];

    /// `name` as one of [`Self::KEYS`], ignoring case.
    pub fn key_named(name: &str) -> Option<&'static str> {
        Self::KEYS
            .iter()
            .copied()
            .find(|key| key.eq_ignore_ascii_case(name))
            .or_else(|| {
                KEY_ALIASES
                    .iter()
                    .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
                    .map(|(_, key)| *key)
            })
    }

    /// Why it can't be used: without Ctrl, Alt or Super, it would take a
    /// key from typing everywhere. Function keys are exempt.
    pub fn check(&self) -> Result<(), String> {
        let function_key = self.key.len() > 1 && self.key.starts_with('F');
        if self.ctrl || self.alt || self.super_key || function_key {
            Ok(())
        } else {
            Err("needs Ctrl, Alt or Super, except on function keys".into())
        }
    }
}

impl FromStr for Hotkey {
    type Err = ();

    /// Modifiers then the key, joined by `+`, ignoring case and spaces.
    fn from_str(text: &str) -> Result<Self, ()> {
        let mut hotkey = Hotkey {
            ctrl: false,
            alt: false,
            shift: false,
            super_key: false,
            key: "",
        };
        let mut parts: Vec<_> = text.split('+').map(str::trim).collect();
        hotkey.key = Self::key_named(parts.pop().ok_or(())?).ok_or(())?;
        for part in parts {
            let modifier = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut hotkey.ctrl,
                "alt" | "option" | "opt" => &mut hotkey.alt,
                "shift" => &mut hotkey.shift,
                "super" | "cmd" | "command" | "win" | "meta" => &mut hotkey.super_key,
                _ => return Err(()),
            };
            if std::mem::replace(modifier, true) {
                return Err(());
            }
        }
        Ok(hotkey)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let modifiers = [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.super_key, "Super"),
        ];
        for (_, name) in modifiers.iter().filter(|(on, _)| *on) {
            write!(f, "{name}+")?;
        }
        f.write_str(self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Option<Hotkey> {
        text.parse().ok()
    }

    #[test]
    fn parses_modifiers_in_any_order_and_case() {
        let hotkey = parse("alt + CTRL+space").unwrap();
        assert!(hotkey.ctrl && hotkey.alt && !hotkey.shift && !hotkey.super_key);
        assert_eq!(hotkey.key, "Space");
        assert_eq!(hotkey.to_string(), "Ctrl+Alt+Space");

        let hotkey = parse("Cmd+Option+Shift+pgup").unwrap();
        assert_eq!(hotkey.to_string(), "Alt+Shift+Super+PageUp");
        assert_eq!(parse("Win+,").unwrap().to_string(), "Super+,");
        assert_eq!(parse("F5").unwrap().key, "F5");
    }

    #[test]
    fn rejects_unknown_keys_and_modifiers() {
        for text in [
            "",
            "Ctrl+",
            "Ctrl+Foo",
            "Hyper+A",
            "Ctrl+Ctrl+A",
            "Ctrl+A+B",
            "Ctrl+F21",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn written_form_reads_back() {
        for key in Hotkey::KEYS {
            let hotkey = Hotkey {
                ctrl: true,
                alt: false,
                shift: true,
                super_key: false,
                key,
            };
            assert_eq!(parse(&hotkey.to_string()), Some(hotkey));
        }
    }

    #[test]
    fn needs_a_modifier_but_shift_unless_a_function_key() {
        assert!(parse("Ctrl+A").unwrap().check().is_ok());
        assert!(parse("Super+1").unwrap().check().is_ok());
        assert!(parse("F12").unwrap().check().is_ok());
        assert!(parse("A").unwrap().check().is_err());
        assert!(parse("Shift+Space").unwrap().check().is_err());
        assert!(parse("Shift+F").unwrap().check().is_err());
    }
}
