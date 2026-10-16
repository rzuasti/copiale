//! Headless tests for the main window shell, with a recording core.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use copiale_core::{
    Attachment, BackendError, Command, Content, ConversationId, ConversationSummary, Direction,
    Event, Expiry, GroupMasterKey, GroupUpdate, Member, Message, MessageBody, MessageId,
    MessageStatus, Peer, Quote, ServiceId, StorageFailure, Timestamp, Unsupported,
};
use copiale_ui::chat::Compose;
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

    fn top(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) -> Pixels {
        let id = id.into();
        cx.update_window(self.window, |_, window, _| window.find(id).bounds().top())
            .unwrap()
    }

    fn shows(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) -> bool {
        let id = id.into();
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some())
            .unwrap()
    }

    fn hover(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) {
        let id = id.into();
        cx.update_window(self.window, |_, window, cx| window.hover(id, cx))
            .unwrap();
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
        // The latest chat opens; read once the window is active.
        assert!(matches!(
            ui.core.take().as_slice(),
            [Command::LoadMessages { conversation, .. }] if *conversation == ConversationId::new(1)
        ));
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
        members: Vec::new(),
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
        sender: (direction == Direction::Incoming).then(|| peer(1)),
        sent_at: Timestamp::from_millis(id as u64 * 1000),
        received_at: Timestamp::from_millis(id as u64 * 1000),
        content: Content::text(format!("*message* {id}")),
        edited: false,
        read: true,
        status,
        expiry: None,
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
    ui.click(("conversation", 2u64), cx);
    ui.core.take();

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

    let commands: Vec<_> = ui
        .core
        .take()
        .into_iter()
        .filter(|command| !matches!(command, Command::Typing { .. }))
        .collect();
    assert!(
        matches!(
            commands.as_slice(),
            [Command::SendText { conversation, body, reply_to: None }]
                if *conversation == ConversationId::new(1) && body.as_str() == "hello\nthere"
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

#[gpui_kit::test]
fn group_header_lists_members(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    let group = ConversationSummary {
        peer: Peer::Group(GroupMasterKey::new([3; 32]).id()),
        members: vec![
            Member {
                id: peer(1),
                name: Some("Alice".into()),
            },
            Member {
                id: peer(4),
                name: None,
            },
        ],
        ..summary(3, "Dev", 0)
    };
    ui.event(Event::ConversationUpdated(group), cx);
    assert!(!ui.shows("members", cx));

    ui.click(("conversation", 3u64), cx);
    ui.event(
        Event::Messages {
            conversation: ConversationId::new(3),
            before: None,
            messages: vec![Message {
                conversation: ConversationId::new(3),
                sender: Some(peer(4)),
                ..message(10, Direction::Incoming, None)
            }],
        },
        cx,
    );

    assert!(ui.shows("members", cx));
    let row = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx).chat();
        chat.row(0).unwrap().prefix.sender.clone()
    });
    assert_eq!(row, "Unknown (0004)");
}

#[gpui_kit::test]
fn renders_every_kind_of_content(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.click(("conversation", 1u64), cx);
    let contents = [
        Content::Text {
            body: MessageBody::new("caption"),
            attachments: vec![Attachment::Image, Attachment::File { name: None }],
            quote: None,
        },
        Content::Text {
            body: MessageBody::new(""),
            attachments: vec![Attachment::Voice],
            quote: None,
        },
        Content::Sticker { emoji: None },
        Content::Reaction {
            emoji: "👍".into()
        },
        Content::Call { video: true },
        Content::Unsupported(Unsupported::Payment),
        Content::GroupChanged(vec![GroupUpdate::Title("Dev".into()), GroupUpdate::Left]),
        Content::TimerChanged(Some(60)),
        Content::Deleted,
        Content::Text {
            body: MessageBody::new("agreed"),
            attachments: Vec::new(),
            quote: Some(Quote {
                author: None,
                sent_at: Timestamp::from_millis(1),
                text: "ship it?".into(),
                attachment: None,
            }),
        },
    ];
    let messages = (1..)
        .zip(contents)
        .map(|(id, content)| Message {
            content,
            edited: id % 2 == 0,
            expiry: (id % 3 == 0).then_some(Expiry {
                timer: 3600,
                at: None,
            }),
            ..message(id, Direction::Incoming, None)
        })
        .collect();

    ui.event(
        Event::Messages {
            conversation: ConversationId::new(1),
            before: None,
            messages,
        },
        cx,
    );

    let rows = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx).chat();
        (0..chat.row_count())
            .map(|ix| {
                let row = chat.row(ix).unwrap();
                let shape = (row.placeholders.len(), row.system.len(), row.quote.clone());
                (shape, row.timer)
            })
            .collect::<Vec<_>>()
    });
    let (shapes, timers): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
    let quote = Some("↳ you: ship it?".to_string());
    assert_eq!(
        shapes,
        [
            (2, 0, None),
            (1, 0, None),
            (1, 0, None),
            (1, 0, None),
            (1, 0, None),
            (1, 0, None),
            (0, 2, None),
            (0, 1, None),
            (1, 0, None),
            (0, 0, quote),
        ]
    );
    let hour = Some("◷ 1h".to_string());
    assert_eq!(
        timers,
        [
            None,
            None,
            hour.clone(),
            None,
            None,
            hour.clone(),
            None,
            None,
            hour,
            None
        ]
    );
}

#[gpui_kit::test]
fn typing_is_shown_and_sent(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    assert!(!ui.shows("typing", cx));

    ui.event(
        Event::Typing {
            conversation: ConversationId::new(1),
            member: peer(1),
            typing: true,
        },
        cx,
    );
    assert!(ui.shows("typing", cx));
    // A line after the last message, not over it.
    let items = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            (chat.log().item_count(), chat.chat().row_count())
        })
    };
    assert_eq!(items(cx), (2, 1));
    let last_item = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.log().bounds_for_item(1).unwrap().top()
        })
    };
    let typing_top = last_item(cx);

    // Their message takes the typing line's place.
    ui.event(
        Event::MessageAdded(message(11, Direction::Incoming, None)),
        cx,
    );
    assert!(!ui.shows("typing", cx));
    assert_eq!(items(cx), (2, 2));
    let bounds = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.log().bounds_for_item(1).unwrap()
    });
    assert!(
        (bounds.top() - typing_top).abs() < px(8.),
        "{bounds:?} vs {typing_top:?}"
    );

    // Stopping without a message removes the line.
    ui.event(
        Event::Typing {
            conversation: ConversationId::new(1),
            member: peer(1),
            typing: true,
        },
        cx,
    );
    assert_eq!(items(cx), (3, 2));
    ui.event(
        Event::Typing {
            conversation: ConversationId::new(1),
            member: peer(1),
            typing: false,
        },
        cx,
    );
    assert!(!ui.shows("typing", cx));
    assert_eq!(items(cx), (2, 2));

    ui.type_text("h", cx);
    let typing = |commands: Vec<Command>| {
        commands
            .into_iter()
            .filter_map(|command| match command {
                Command::Typing {
                    conversation,
                    typing,
                } => Some((conversation.get(), typing)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(typing(ui.core.take()), [(1, true)]);
    cx.executor().advance_clock(Duration::from_secs(6));
    ui.frame(cx);
    assert_eq!(typing(ui.core.take()), [(1, false)], "stops after a pause");
}

#[gpui_kit::test]
fn rows_at_the_end_ease_in_and_out(cx: &mut TestAppContext) {
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
    ui.settle(cx);
    let log = |cx: &mut TestAppContext| {
        cx.update(|cx| ui.shell.read(cx).chat().unwrap().read(cx).log().clone())
    };
    let bottom =
        |ix: usize, cx: &mut TestAppContext| log(cx).bounds_for_item(ix).map(|b| b.bottom());
    let frames = |ix: usize, cx: &mut TestAppContext| {
        (0..30)
            .map(|_| {
                cx.executor().advance_clock(Duration::from_millis(16));
                ui.frame(cx);
                bottom(ix, cx).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let end = bottom(79, cx).unwrap();

    // A new message rises into view.
    ui.event(
        Event::MessageAdded(message(81, Direction::Incoming, None)),
        cx,
    );
    let rising = frames(80, cx);
    assert!(rising[0] > end + px(4.), "starts lower: {rising:?}");
    assert!(
        (rising[29] - end).abs() < px(1.),
        "ends at the bottom: {rising:?}"
    );
    assert!(
        rising.windows(2).all(|pair| pair[1] <= pair[0]),
        "{rising:?}"
    );
    assert!(log(cx).is_following_tail());

    // The typing line comes and goes; the last message drops back.
    for typing in [true, false] {
        ui.event(
            Event::Typing {
                conversation: ConversationId::new(1),
                member: peer(1),
                typing,
            },
            cx,
        );
        if typing {
            frames(81, cx);
        }
    }
    let dropping = frames(80, cx);
    assert!(dropping[0] < end - px(4.), "starts higher: {dropping:?}");
    assert!(
        (dropping[29] - end).abs() < px(1.),
        "ends at the bottom: {dropping:?}"
    );
    assert!(
        dropping.windows(2).all(|pair| pair[1] >= pair[0]),
        "{dropping:?}"
    );
}

#[gpui_kit::test]
fn deleting_the_last_message_eases_the_rest_down(cx: &mut TestAppContext) {
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
    ui.settle(cx);
    let bottom = |ix: usize, cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.log().bounds_for_item(ix).unwrap().bottom()
        })
    };
    let end = bottom(79, cx);

    ui.event(
        Event::MessageRemoved {
            conversation: ConversationId::new(1),
            id: MessageId::new(80),
        },
        cx,
    );

    let path: Vec<_> = (0..30)
        .map(|_| {
            cx.executor().advance_clock(Duration::from_millis(16));
            ui.frame(cx);
            bottom(78, cx)
        })
        .collect();
    assert!(path[0] < end - px(4.), "starts where it was: {path:?}");
    assert!(
        (path[29] - end).abs() < px(1.),
        "ends at the bottom: {path:?}"
    );
    assert!(path.windows(2).all(|pair| pair[1] >= pair[0]), "{path:?}");
    let rows = cx.update(|cx| {
        ui.shell
            .read(cx)
            .chat()
            .unwrap()
            .read(cx)
            .chat()
            .row_count()
    });
    assert_eq!(rows, 79);
}

/// Alice's chat with a recent message from her and one of ours.
fn chat_with_recent(ui: &Ui, cx: &mut TestAppContext) {
    ui.click(("conversation", 1u64), cx);
    ui.core.take();
    let now = Timestamp::now().as_millis();
    let recent = |id: i64, direction, ago: u64| Message {
        sent_at: Timestamp::from_millis(now - ago),
        received_at: Timestamp::from_millis(now - ago),
        sender: (direction == Direction::Incoming).then(|| peer(1)),
        ..message(
            id,
            direction,
            (direction == Direction::Outgoing).then_some(MessageStatus::Sent),
        )
    };
    ui.event(
        Event::Messages {
            conversation: ConversationId::new(1),
            before: None,
            messages: vec![
                recent(10, Direction::Incoming, 60_000),
                recent(11, Direction::Outgoing, 30_000),
            ],
        },
        cx,
    );
}

fn chat_state(ui: &Ui, cx: &mut TestAppContext) -> (String, Compose, Option<MessageId>) {
    cx.update(|cx| {
        let view = ui.shell.read(cx).chat().unwrap().read(cx);
        let text = view.composer().read(cx).value().to_string();
        (text, view.chat().compose(), view.chat().selected())
    })
}

#[gpui_kit::test]
fn keyboard_edits_replies_and_deletes(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    chat_with_recent(&ui, cx);

    // ↑ in an empty composer edits our last message; Esc lets go.
    ui.press("up", cx);
    assert_eq!(
        chat_state(&ui, cx),
        (
            "*message* 11".into(),
            Compose::Edit(MessageId::new(11)),
            None
        )
    );
    ui.press("escape", cx);
    assert_eq!(chat_state(&ui, cx), (String::new(), Compose::New, None));

    // Shift+↑ selects, ↑ moves, r replies without typing an "r".
    ui.press("shift-up", cx);
    ui.press("up", cx);
    assert_eq!(chat_state(&ui, cx).2, Some(MessageId::new(10)));
    assert!(ui.shows(("actions", 10u64), cx));
    ui.press("r", cx);
    assert_eq!(
        chat_state(&ui, cx),
        (String::new(), Compose::Reply(MessageId::new(10)), None)
    );
    assert!(ui.shows("compose", cx));
    ui.type_text("yes", cx);
    ui.press("enter", cx);
    let reply = ui
        .core
        .take()
        .into_iter()
        .find(|command| matches!(command, Command::SendText { .. }));
    assert!(
        matches!(
            reply,
            Some(Command::SendText { reply_to: Some(id), ref body, .. })
                if id == MessageId::new(10) && body.as_str() == "yes"
        ),
        "{reply:?}"
    );

    // Deleting asks first: e for everyone.
    ui.press("shift-up", cx);
    ui.press("backspace", cx);
    assert!(ui.shows(("for-everyone", 11u64), cx));
    ui.press("e", cx);
    let deletes: Vec<_> = ui
        .core
        .take()
        .into_iter()
        .filter(|command| matches!(command, Command::Delete { .. }))
        .collect();
    assert!(
        matches!(
            deletes.as_slice(),
            [Command::Delete { id, everyone: true }] if *id == MessageId::new(11)
        ),
        "{deletes:?}"
    );
    assert_eq!(chat_state(&ui, cx), (String::new(), Compose::New, None));
}

#[gpui_kit::test]
fn row_actions_work_with_the_mouse(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    chat_with_recent(&ui, cx);
    // No room is kept for actions until a row is hovered.
    assert!(!ui.shows(("actions", 10u64), cx));
    assert!(!ui.shows(("actions", 11u64), cx));

    ui.hover(("row", 10u64), cx);
    assert!(ui.shows(("actions", 10u64), cx));
    assert!(!ui.shows(("actions", 11u64), cx));
    // On a line of their own, under the text.
    assert!(ui.top(("actions", 10u64), cx) > ui.top(("row", 10u64), cx) + px(10.));
    ui.hover(("row", 11u64), cx);
    assert!(!ui.shows(("actions", 10u64), cx), "gone with the pointer");
    ui.hover(("row", 10u64), cx);
    ui.click(("reply", 10u64), cx);
    assert_eq!(chat_state(&ui, cx).1, Compose::Reply(MessageId::new(10)));
    ui.click("cancel-compose", cx);
    assert_eq!(chat_state(&ui, cx).1, Compose::New);

    ui.hover(("row", 11u64), cx);
    ui.click(("edit", 11u64), cx);
    assert_eq!(chat_state(&ui, cx).0, "*message* 11");
    ui.press("escape", cx);

    ui.hover(("row", 10u64), cx);
    ui.click(("delete", 10u64), cx);
    assert!(!ui.shows(("for-everyone", 10u64), cx), "not ours");
    ui.click(("for-me", 10u64), cx);
    assert!(matches!(
        ui.core.take().as_slice(),
        [.., Command::Delete { id, everyone: false }] if *id == MessageId::new(10)
    ));
}

#[gpui_kit::test]
fn the_latest_chat_opens_at_start(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);

    let (open, focused) = cx
        .update_window(ui.window, |_, window, cx| {
            let view = ui.shell.read(cx).chat().unwrap().read(cx);
            let focused = view.composer().focus_handle(cx).is_focused(window);
            (view.chat().open_id(), focused)
        })
        .unwrap();

    assert_eq!(open, Some(ConversationId::new(1)));
    assert!(focused);
}
