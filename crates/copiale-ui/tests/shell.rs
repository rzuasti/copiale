//! Headless tests for the main window shell, with a recording core.

use std::cell::RefCell;
use std::rc::Rc;

use copiale_core::{BackendError, Command, Event, StorageFailure};
use copiale_ui::onboarding::Screen;
use copiale_ui::{APP_ID, CoreSink, Shell, main_window_options};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

const URL: &str = "sgnl://linkdevice?uuid=AbCdEfGhIjKlMnOp&pub_key=BQ%2B1YW3pPq0xg9Hk3sR8r5D7";

#[derive(Default)]
struct Recorder(RefCell<Vec<Command>>);

impl CoreSink for Recorder {
    fn send(&self, command: Command) {
        self.0.borrow_mut().push(command);
    }
}

impl Recorder {
    fn take(&self) -> Vec<Command> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

struct Ui {
    window: AnyWindowHandle,
    shell: Entity<Shell>,
    core: Rc<Recorder>,
}

impl Ui {
    fn open(cx: &mut TestAppContext) -> Self {
        cx.update(gpui_kit::init);
        let core = Rc::new(Recorder::default());
        let sink: Rc<dyn CoreSink> = core.clone();
        let (window, shell) = cx
            .update(|cx| {
                gpui_kit::open_window(main_window_options(cx), cx, |window, cx| {
                    cx.new(|cx| Shell::new(sink, window, cx))
                })
            })
            .expect("open main window");
        let ui = Self {
            window,
            shell,
            core,
        };
        ui.frame(cx);
        ui
    }

    fn event(&self, event: Event, cx: &mut TestAppContext) {
        let shell = self.shell.clone();
        cx.update_window(self.window, |_, window, cx| {
            shell.update(cx, |shell, cx| shell.on_event(&event, window, cx));
            window.render_frame(cx);
        })
        .unwrap();
    }

    fn frame(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    fn click(&self, id: &'static str, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.click(id, cx))
            .unwrap();
        self.frame(cx);
    }

    fn type_text(&self, text: &str, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.input(text, cx))
            .unwrap();
    }

    fn press(&self, key: &str, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.press(key, cx))
            .unwrap();
        self.frame(cx);
    }

    fn screen(&self, cx: &mut TestAppContext) -> Screen {
        cx.update(|cx| self.shell.read(cx).screen().clone())
    }
}

#[gpui_kit::test]
fn main_window_options_set_app_id_and_title(cx: &mut TestAppContext) {
    let options = cx.update(|cx| main_window_options(cx));
    assert_eq!(options.app_id.as_deref(), Some(APP_ID));
    let title = options.titlebar.and_then(|titlebar| titlebar.title);
    assert_eq!(title.as_deref(), Some("Copiale"));
}

#[gpui_kit::test]
fn renders_every_screen(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    assert_eq!(ui.screen(cx), Screen::Starting);

    for event in [
        Event::Locked {
            new_passphrase: true,
        },
        Event::Locked {
            new_passphrase: false,
        },
        Event::WrongPassphrase,
        Event::StorageFailed(StorageFailure::KeyMissing),
        Event::StorageFailed(StorageFailure::KeychainUnavailable("locked".into())),
        Event::StorageFailed(StorageFailure::WrongKey),
        Event::StorageFailed(StorageFailure::Other("disk full".into())),
        Event::Unlinked {
            device_name: "Copiale on host".into(),
        },
        Event::LinkUrl(URL.into()),
        Event::LinkFailed(BackendError::new("timed out")),
        Event::Unlinked {
            device_name: "Copiale on host".into(),
        },
        Event::Ready,
        Event::Stopped,
    ] {
        ui.event(event, cx);
    }

    assert_eq!(ui.screen(cx), Screen::Stopped);
}

#[gpui_kit::test]
fn new_passphrase_is_typed_and_submitted(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(
        Event::Locked {
            new_passphrase: true,
        },
        cx,
    );

    // The passphrase field has focus; Tab moves to the confirmation.
    ui.type_text("correct horse", cx);
    ui.press("tab", cx);
    ui.type_text("correct horse", cx);
    ui.press("enter", cx);

    let commands = ui.core.take();
    assert!(
        matches!(commands.as_slice(), [Command::SetPassphrase(_)]),
        "{commands:?}"
    );
    assert!(matches!(
        ui.screen(cx),
        Screen::Passphrase { busy: true, .. }
    ));
}

#[gpui_kit::test]
fn mismatched_passphrases_are_not_sent(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(
        Event::Locked {
            new_passphrase: true,
        },
        cx,
    );

    ui.type_text("correct horse", cx);
    ui.click("submit-passphrase", cx);

    assert!(ui.core.take().is_empty());
    assert!(matches!(
        ui.screen(cx),
        Screen::Passphrase { error: Some(_), .. }
    ));
}

#[gpui_kit::test]
fn link_sends_the_suggested_device_name(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(
        Event::Unlinked {
            device_name: "Copiale on host".into(),
        },
        cx,
    );

    ui.click("link", cx);

    let commands = ui.core.take();
    assert!(
        matches!(commands.as_slice(), [Command::Link { device_name }] if device_name == "Copiale on host"),
        "{commands:?}"
    );
}

#[gpui_kit::test]
fn linking_shows_qr_and_can_cancel(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(Event::LinkUrl(URL.into()), cx);
    assert_eq!(ui.screen(cx), Screen::Linking { url: URL.into() });

    ui.click("cancel-link", cx);

    assert!(matches!(ui.core.take().as_slice(), [Command::CancelLink]));
}

#[gpui_kit::test]
fn start_over_asks_for_confirmation(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(Event::StorageFailed(StorageFailure::KeyMissing), cx);

    ui.click("start-over", cx);
    assert!(ui.core.take().is_empty());
    ui.click("confirm-start-over", cx);

    assert!(matches!(ui.core.take().as_slice(), [Command::StartOver]));
}

#[gpui_kit::test]
fn retry_after_storage_failure(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(
        Event::StorageFailed(StorageFailure::KeychainUnavailable("locked".into())),
        cx,
    );

    ui.click("retry", cx);

    assert!(matches!(ui.core.take().as_slice(), [Command::Retry]));
}
