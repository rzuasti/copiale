//! Headless tests for the main window shell, with a recording core.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use copiale_core::{
    BackendError, Command, ConversationId, ConversationSummary, Direction, Event, Message,
    MessageBody, MessageId, MessageStatus, Peer, ServiceId, StorageFailure, Timestamp,
};
use copiale_ui::onboarding::Screen;
use copiale_ui::theme::{self, ActivePalette as _, Appearance, Scheme, Theme};
use copiale_ui::{APP_ID, CoreSink, Shell, main_window_options};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, Focusable as _, Hsla, InputEvent as _,
    MouseMoveEvent, Pixels, Point, Rgba, ScrollDelta, TestAppContext, point, px,
};
use uuid::Uuid;

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
        cx.update(copiale_ui::init);
        let core = Rc::new(Recorder::default());
        let sink: Rc<dyn CoreSink> = core.clone();
        let (window, shell) = cx
            .update(|cx| {
                gpui_kit::open_window(main_window_options(cx), cx, |window, cx| {
                    theme::init_from_window(window, cx);
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

    fn click(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) {
        let id = id.into();
        cx.update_window(self.window, |_, window, cx| window.click(id.clone(), cx))
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

    /// Moves the pointer to `offset` from the top-left of element `id`.
    fn point_at(&self, id: &'static str, offset: Point<Pixels>, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| {
            let position = window.find(id).bounds().origin + offset;
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
    }

    /// Lets animations finish.
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..30 {
            cx.executor().advance_clock(Duration::from_millis(16));
            self.frame(cx);
        }
    }

    fn top(&self, id: &'static str, cx: &mut TestAppContext) -> Pixels {
        cx.update_window(self.window, |_, window, _| window.find(id).bounds().top())
            .unwrap()
    }

    fn shows(&self, id: &'static str, cx: &mut TestAppContext) -> bool {
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some())
            .unwrap()
    }

    fn screen(&self, cx: &mut TestAppContext) -> Screen {
        cx.update(|cx| self.shell.read(cx).screen().clone())
    }

    /// Ready, with Alice (unread) and Bob in the chat list.
    fn ready(cx: &mut TestAppContext) -> Self {
        let ui = Self::open(cx);
        ui.event(Event::Ready, cx);
        assert!(matches!(
            ui.core.take().as_slice(),
            [Command::LoadConversations]
        ));
        ui.event(
            Event::Conversations(vec![summary(1, "Alice", 1), summary(2, "Bob", 0)]),
            cx,
        );
        ui
    }

    fn open_alice(&self, cx: &mut TestAppContext) {
        self.click(("conversation", 1u64), cx);
        self.core.take();
        self.event(
            Event::Messages {
                conversation: ConversationId::new(1),
                before: None,
                messages: vec![message(10, Direction::Incoming, None)],
            },
            cx,
        );
    }
}

fn summary(id: i64, name: &str, unread: u32) -> ConversationSummary {
    ConversationSummary {
        id: ConversationId::new(id),
        peer: Peer::Contact(peer(id)),
        title: Some(name.into()),
        last_activity: Some(Timestamp::from_millis(1_000 - id as u64)),
        unread,
        last_message: None,
    }
}

fn peer(id: i64) -> ServiceId {
    ServiceId::new(Uuid::from_u128(id as u128))
}

fn message(id: i64, direction: Direction, status: Option<MessageStatus>) -> Message {
    Message {
        id: MessageId::new(id),
        conversation: ConversationId::new(1),
        direction,
        sent_at: Timestamp::from_millis(id as u64 * 1000),
        received_at: Timestamp::from_millis(id as u64 * 1000),
        body: MessageBody::new(format!("*message* {id}")),
        read: true,
        status,
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

// Chats

#[gpui_kit::test]
fn opening_a_chat_loads_it_and_marks_it_read(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);

    ui.click(("conversation", 1u64), cx);

    let commands = ui.core.take();
    assert!(
        matches!(
            commands.as_slice(),
            [
                Command::LoadMessages { conversation, before: None, .. },
                Command::MarkRead(read),
            ] if *conversation == ConversationId::new(1) && *read == ConversationId::new(1)
        ),
        "{commands:?}"
    );
}

#[gpui_kit::test]
fn composer_sends_to_the_open_chat(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    // Opening focuses the composer; Enter on nothing sends nothing.
    ui.press("enter", cx);
    ui.type_text("hello", cx);
    ui.press("shift-enter", cx);
    ui.type_text("there", cx);
    ui.press("enter", cx);

    let commands = ui.core.take();
    assert!(
        matches!(
            commands.as_slice(),
            [Command::SendText { recipient, body }]
                if *recipient == peer(1) && body.as_str() == "hello\nthere"
        ),
        "{commands:?}"
    );
    let composer = cx.update(|cx| {
        ui.shell
            .read(cx)
            .chat()
            .unwrap()
            .read(cx)
            .composer()
            .clone()
    });
    assert!(cx.update(|cx| composer.read(cx).value().is_empty()));
}

#[gpui_kit::test]
fn failed_message_can_be_retried(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    let pending = message(11, Direction::Outgoing, Some(MessageStatus::Pending));
    ui.event(Event::MessageAdded(pending.clone()), cx);

    ui.event(
        Event::SendFailed {
            message: Message {
                status: Some(MessageStatus::Failed),
                ..pending
            },
            error: BackendError::new("offline"),
        },
        cx,
    );
    ui.click(("retry", 11u64), cx);

    let commands = ui.core.take();
    assert!(
        matches!(commands.as_slice(), [Command::RetrySend(id)] if *id == MessageId::new(11)),
        "{commands:?}"
    );
}

#[gpui_kit::test]
fn search_filters_and_enter_opens_the_match(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    let search = cx.update(|cx| ui.shell.read(cx).chat().unwrap().read(cx).search().clone());
    cx.update_window(ui.window, |_, window, cx| {
        search.update(cx, |search, cx| search.focus(window, cx));
    })
    .unwrap();

    ui.type_text("bo", cx);
    ui.press("enter", cx);

    let open = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx).chat();
        assert_eq!(chat.conversations().count(), 1);
        chat.open_id()
    });
    assert_eq!(open, Some(ConversationId::new(2)));
}

#[gpui_kit::test]
fn offline_status_reconnects_on_click(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.event(
        Event::Disconnected {
            error: None,
            retry_in: std::time::Duration::from_secs(8),
        },
        cx,
    );

    ui.click("connection", cx);

    assert!(matches!(ui.core.take().as_slice(), [Command::Reconnect]));
}

/// Bytes, as GPUI Kit and the palette parse colors slightly differently.
fn rgba(color: Hsla) -> [u8; 4] {
    let Rgba { r, g, b, a } = color.into();
    [r, g, b, a].map(|v| (v * 255.).round() as u8)
}

fn assert_theme(expected: &Theme, cx: &App) {
    let palette = cx.palette();
    assert_eq!(palette, &expected.colors);
    let components = cx.theme();
    assert_eq!(
        components.is_dark(),
        expected.appearance == Appearance::Dark
    );
    for (role, token) in [
        (components.background, palette.background),
        (components.foreground, palette.text),
        (components.muted_foreground, palette.text_muted),
        (components.primary, palette.accent),
        (components.danger, palette.danger),
        (components.selection, palette.selection),
    ] {
        assert_eq!(rgba(role), rgba(token));
    }
}

#[gpui_kit::test]
fn theme_follows_the_system_appearance(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    // Test windows start light.
    cx.update(|cx| assert_theme(&Theme::light(), cx));

    for scheme in [
        Scheme::Dark,
        Scheme::Light,
        Scheme::NoPreference,
        Scheme::Light,
    ] {
        cx.update_window(ui.window, |_, window, cx| {
            theme::follow(scheme, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update(|cx| assert_theme(&Theme::for_scheme(scheme), cx));
    }
}

#[gpui_kit::test]
fn search_shows_from_the_sidebar_top_until_the_pointer_leaves(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    assert!(!ui.shows("search", cx));

    ui.point_at("sidebar", point(px(40.), px(30.)), cx);
    assert!(!ui.shows("search", cx));
    ui.point_at("sidebar", point(px(40.), px(4.)), cx);
    assert!(ui.shows("search", cx));
    ui.settle(cx);
    // Stays while moving down the list.
    ui.point_at("sidebar", point(px(40.), px(120.)), cx);
    assert!(ui.shows("search", cx));

    let width = cx
        .update_window(ui.window, |_, window, _| {
            window.find("sidebar").bounds().size.width
        })
        .unwrap();
    ui.point_at("sidebar", point(width + px(40.), px(120.)), cx);
    ui.settle(cx);
    assert!(!ui.shows("search", cx));
}

#[gpui_kit::test]
fn search_slides_in(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.press("ctrl-f", cx);
    // The box moves down from above the sidebar.
    let mut tops = Vec::new();
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(16));
        ui.frame(cx);
        tops.push(ui.top("search", cx) - ui.top("sidebar", cx));
    }
    assert!(tops[0] < px(0.), "{tops:?}");
    assert!(*tops.last().unwrap() > px(0.), "{tops:?}");
    assert!(tops.windows(2).all(|pair| pair[0] <= pair[1]), "{tops:?}");
}

#[gpui_kit::test]
fn ctrl_f_searches_and_escape_dismisses(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("ctrl-f", cx);
    assert!(ui.shows("search", cx));
    ui.type_text("bo", cx);
    ui.frame(cx);
    let count = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.chat().conversations().count()
        })
    };
    assert_eq!(count(cx), 1);

    ui.press("escape", cx);
    ui.settle(cx);
    assert!(!ui.shows("search", cx));
    assert_eq!(count(cx), 2);
    let composer_focused = cx
        .update_window(ui.window, |_, window, cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.composer().focus_handle(cx).is_focused(window)
        })
        .unwrap();
    assert!(composer_focused);
}

#[gpui_kit::test]
fn wheel_scrolls_the_log_smoothly(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.click(("conversation", 1u64), cx);
    ui.event(
        Event::Messages {
            conversation: ConversationId::new(1),
            before: None,
            messages: (1..=80)
                .map(|id| message(id, Direction::Incoming, None))
                .collect(),
        },
        cx,
    );
    for _ in 0..5 {
        ui.frame(cx);
    }
    // Where the newest message sits; scrolling up moves it down.
    let position = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.log().bounds_for_item(79).unwrap().top()
        })
    };
    let start = position(cx);

    // One notch up.
    cx.update_window(ui.window, |_, window, cx| {
        window.scroll("log", ScrollDelta::Lines(point(0., 3.)), cx);
    })
    .unwrap();
    let mut path = Vec::new();
    for _ in 0..40 {
        cx.executor().advance_clock(Duration::from_millis(16));
        ui.frame(cx);
        path.push(position(cx) - start);
    }

    let travelled = *path.last().unwrap();
    assert!(
        (travelled - px(60.)).abs() < px(6.),
        "one notch scrolls about 60 px, not {travelled:?}"
    );
    assert!(path[0] < px(30.), "eases in instead of jumping: {path:?}");
    assert!(path.windows(2).all(|pair| pair[0] <= pair[1]), "{path:?}");
}
