//! The built-in hotkey (`window.hotkey`): shows or hides the window from
//! any app, as `copiale --toggle` does, on Windows and macOS.

use copiale_core::config::Hotkey;
use gpui_kit::{App, Global, Keystroke, Modifiers};

use crate::settings::Settings;

/// Whether this platform can register it; elsewhere, desktops bind
/// `copiale --toggle` to a key.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

/// Registers hotkeys with the OS; a fake in tests.
pub trait Registrar {
    /// Error message if it can't be registered, e.g. as another app has it.
    fn register(&mut self, hotkey: Hotkey) -> Result<(), String>;
    fn unregister(&mut self, hotkey: Hotkey);
}

/// The hotkey in effect.
struct Registration {
    registrar: Box<dyn Registrar>,
    /// Last registered or tried.
    tried: Option<Hotkey>,
    registered: bool,
    problem: Option<String>,
    /// Off while recording a new one, so the old one reaches the window.
    paused: bool,
}

impl Global for Registration {}

#[cfg(any(windows, target_os = "macos"))]
pub use platform::install;

/// Registers the hotkey `window.hotkey` names through `registrar` from now
/// on, following the settings.
pub fn install_with(registrar: Box<dyn Registrar>, cx: &mut App) {
    cx.set_global(Registration {
        registrar,
        tried: None,
        registered: false,
        problem: None,
        paused: false,
    });
    sync(cx);
    cx.observe_global::<Settings>(sync).detach();
}

/// Lets go of the hotkey while `paused`, then takes it back.
pub fn pause(paused: bool, cx: &mut App) {
    if cx.has_global::<Registration>() {
        cx.global_mut::<Registration>().paused = paused;
        sync(cx);
    }
}

/// Why the hotkey in the settings isn't registered, if it isn't.
pub fn problem(cx: &App) -> Option<&str> {
    cx.try_global::<Registration>()?.problem.as_deref()
}

/// Registers the hotkey the settings name, once each time it changes.
fn sync(cx: &mut App) {
    let wanted = Settings::get(cx).window.hotkey;
    if !cx.has_global::<Registration>() {
        return;
    }
    let registration = cx.global_mut::<Registration>();
    let wanted = wanted.filter(|_| !registration.paused);
    if wanted == registration.tried {
        return;
    }
    if let Some(old) = registration.tried.take()
        && std::mem::take(&mut registration.registered)
    {
        registration.registrar.unregister(old);
    }
    registration.tried = wanted;
    registration.problem = None;
    let Some(hotkey) = wanted else {
        return;
    };
    match registration.registrar.register(hotkey) {
        Ok(()) => registration.registered = true,
        Err(problem) => {
            tracing::warn!(%hotkey, %problem, "hotkey not registered");
            registration.problem = Some(problem);
        }
    }
}

/// What a keystroke does while recording a hotkey.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    Set(Hotkey),
    /// `Backspace` or `Delete`: no hotkey.
    Clear,
    /// `Esc`: keep the current one.
    Cancel,
    /// A modifier alone: keep waiting.
    Pending,
    /// Can't be a hotkey; why.
    Invalid(String),
}

/// GPUI's names for modifiers pressed and released alone.
const MODIFIER_KEYS: [&str; 5] = ["shift", "control", "alt", "platform", "function"];

/// What `keystroke` does while recording a hotkey.
pub fn record(keystroke: &Keystroke) -> Recorded {
    let (key, modifiers) = (keystroke.key.as_str(), &keystroke.modifiers);
    if MODIFIER_KEYS.contains(&key) {
        return Recorded::Pending;
    }
    if !modifiers.modified() {
        match key {
            "escape" => return Recorded::Cancel,
            "backspace" | "delete" => return Recorded::Clear,
            _ => {}
        }
    }
    if modifiers.function {
        return Recorded::Invalid("Fn can't be part of it".into());
    }
    let Some(key) = Hotkey::key_named(key) else {
        return Recorded::Invalid(format!("{key} can't be used"));
    };
    let hotkey = Hotkey {
        ctrl: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        super_key: modifiers.platform,
        key,
    };
    match hotkey.check() {
        Ok(()) => Recorded::Set(hotkey),
        Err(problem) => Recorded::Invalid(problem),
    }
}

/// `hotkey` as GPUI writes keys, to show it.
pub fn keystroke(hotkey: Hotkey) -> Keystroke {
    Keystroke {
        modifiers: Modifiers {
            control: hotkey.ctrl,
            alt: hotkey.alt,
            shift: hotkey.shift,
            platform: hotkey.super_key,
            function: false,
        },
        key: hotkey.key.to_lowercase(),
        key_char: None,
    }
}

/// `key`'s W3C `code` name, as the OS registers it: by position on a US
/// layout.
#[cfg(any(windows, target_os = "macos", test))]
fn code_name(key: &str) -> String {
    let name = match key {
        "Up" | "Down" | "Left" | "Right" => return format!("Arrow{key}"),
        "`" => "Backquote",
        "-" => "Minus",
        "=" => "Equal",
        "[" => "BracketLeft",
        "]" => "BracketRight",
        "\\" => "Backslash",
        ";" => "Semicolon",
        "'" => "Quote",
        "," => "Comma",
        "." => "Period",
        "/" => "Slash",
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_uppercase() => {
            return format!("Key{key}");
        }
        key if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => {
            return format!("Digit{key}");
        }
        key => key,
    };
    name.into()
}

#[cfg(any(windows, target_os = "macos"))]
mod platform {
    use copiale_core::config::Hotkey;
    use futures::StreamExt as _;
    use futures::channel::mpsc::{UnboundedReceiver, unbounded};
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use gpui_kit::App;

    use super::{Registrar, code_name, install_with};
    use crate::main_window;

    /// Registers the hotkey `window.hotkey` names from now on, following
    /// the settings; presses toggle the main window.
    pub fn install(cx: &mut App) {
        match SystemRegistrar::new() {
            Ok((registrar, mut presses)) => {
                install_with(Box::new(registrar), cx);
                cx.spawn(async move |cx| {
                    while presses.next().await.is_some() {
                        cx.update(|cx| main_window::toggle(None, cx));
                    }
                })
                .detach();
            }
            Err(error) => tracing::warn!(%error, "no global hotkey"),
        }
    }

    struct SystemRegistrar(GlobalHotKeyManager);

    impl SystemRegistrar {
        /// On the main thread, whose event loop receives presses.
        fn new() -> global_hotkey::Result<(Self, UnboundedReceiver<()>)> {
            let manager = GlobalHotKeyManager::new()?;
            let (presses, received) = unbounded();
            GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
                if event.state == HotKeyState::Pressed {
                    presses.unbounded_send(()).ok();
                }
            }));
            Ok((Self(manager), received))
        }
    }

    impl Registrar for SystemRegistrar {
        fn register(&mut self, hotkey: Hotkey) -> Result<(), String> {
            match self.0.register(os_hotkey(hotkey)?) {
                Ok(()) => Ok(()),
                Err(global_hotkey::Error::AlreadyRegistered(_)) => {
                    Err("taken by another app".into())
                }
                Err(error) => Err(error.to_string()),
            }
        }

        fn unregister(&mut self, hotkey: Hotkey) {
            if let Ok(os) = os_hotkey(hotkey)
                && let Err(error) = self.0.unregister(os)
            {
                tracing::warn!(%error, "hotkey not unregistered");
            }
        }
    }

    fn os_hotkey(hotkey: Hotkey) -> Result<HotKey, String> {
        let code: Code = code_name(hotkey.key)
            .parse()
            .map_err(|_| format!("{} can't be used", hotkey.key))?;
        let modifiers = [
            (hotkey.ctrl, Modifiers::CONTROL),
            (hotkey.alt, Modifiers::ALT),
            (hotkey.shift, Modifiers::SHIFT),
            (hotkey.super_key, Modifiers::META),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(Modifiers::empty(), |all, (_, modifier)| all | modifier);
        Ok(HotKey::new(Some(modifiers), code))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use copiale_core::config::Setting;
    use gpui_kit::TestAppContext;

    use super::*;
    use crate::settings;

    fn hotkey(text: &str) -> Hotkey {
        text.parse().unwrap()
    }

    fn keystroke_of(text: &str) -> Keystroke {
        Keystroke::parse(text).unwrap()
    }

    #[test]
    fn records_keys_with_modifiers() {
        assert_eq!(
            record(&keystroke_of("ctrl-alt-space")),
            Recorded::Set(hotkey("Ctrl+Alt+Space"))
        );
        assert_eq!(
            record(&keystroke_of("cmd-shift-pageup")),
            Recorded::Set(hotkey("Shift+Super+PageUp"))
        );
        assert_eq!(record(&keystroke_of("f9")), Recorded::Set(hotkey("F9")));
        assert_eq!(
            record(&keystroke_of("alt-;")),
            Recorded::Set(hotkey("Alt+;"))
        );
    }

    #[test]
    fn plain_escape_and_backspace_cancel_and_clear() {
        assert_eq!(record(&keystroke_of("escape")), Recorded::Cancel);
        assert_eq!(record(&keystroke_of("backspace")), Recorded::Clear);
        assert_eq!(record(&keystroke_of("delete")), Recorded::Clear);
        assert_eq!(
            record(&keystroke_of("ctrl-backspace")),
            Recorded::Set(hotkey("Ctrl+Backspace"))
        );
    }

    #[test]
    fn modifiers_alone_wait_and_unusable_keys_say_why() {
        assert_eq!(record(&keystroke_of("shift")), Recorded::Pending);
        assert!(matches!(record(&keystroke_of("a")), Recorded::Invalid(_)));
        assert!(matches!(
            record(&keystroke_of("shift-a")),
            Recorded::Invalid(_)
        ));
        assert!(matches!(
            record(&keystroke_of("ctrl-f24")),
            Recorded::Invalid(_)
        ));
        assert!(matches!(
            record(&keystroke_of("fn-ctrl-a")),
            Recorded::Invalid(_)
        ));
    }

    #[test]
    fn shown_as_recorded() {
        for key in Hotkey::KEYS {
            let hotkey = Hotkey {
                ctrl: true,
                alt: true,
                shift: false,
                super_key: true,
                key,
            };
            assert_eq!(record(&keystroke(hotkey)), Recorded::Set(hotkey), "{key}");
        }
    }

    #[test]
    fn every_key_has_a_code() {
        for key in Hotkey::KEYS {
            let name = code_name(key);
            assert!(
                name.parse::<keyboard_types::Code>().is_ok(),
                "{key}: {name}"
            );
        }
    }

    /// Registers what it's asked, but refuses `taken`.
    #[derive(Clone, Default)]
    struct FakeRegistrar {
        registered: Rc<RefCell<Vec<Hotkey>>>,
        taken: Option<Hotkey>,
    }

    impl Registrar for FakeRegistrar {
        fn register(&mut self, hotkey: Hotkey) -> Result<(), String> {
            if self.taken == Some(hotkey) {
                return Err("taken by another app".into());
            }
            self.registered.borrow_mut().push(hotkey);
            Ok(())
        }

        fn unregister(&mut self, hotkey: Hotkey) {
            self.registered.borrow_mut().retain(|kept| *kept != hotkey);
        }
    }

    #[gpui_kit::test]
    fn registers_the_hotkey_in_the_settings(cx: &mut TestAppContext) {
        let first = hotkey("Ctrl+Alt+Space");
        let second = hotkey("Super+K");
        let fake = FakeRegistrar {
            taken: Some(second),
            ..Default::default()
        };
        let registered = fake.registered.clone();
        cx.update(|cx| {
            settings::preview(&Setting::Hotkey(Some(first)), cx);
            install_with(Box::new(fake), cx);
        });
        assert_eq!(*registered.borrow(), [first]);

        cx.update(|cx| settings::preview(&Setting::Blur(true), cx));
        assert_eq!(*registered.borrow(), [first], "registered once");

        cx.update(|cx| pause(true, cx));
        assert!(registered.borrow().is_empty());
        cx.update(|cx| pause(false, cx));
        assert_eq!(*registered.borrow(), [first]);

        cx.update(|cx| settings::preview(&Setting::Hotkey(Some(second)), cx));
        assert!(registered.borrow().is_empty());
        cx.update(|cx| assert_eq!(problem(cx), Some("taken by another app")));

        cx.update(|cx| settings::preview(&Setting::Hotkey(None), cx));
        cx.update(|cx| assert_eq!(problem(cx), None));
        assert!(registered.borrow().is_empty());
    }
}
