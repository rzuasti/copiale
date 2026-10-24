//! Headless tests for the main window shell, with a recording core.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use copiale_core::config::{
    Config, ConfigFile, Density, MessageStyle, NotificationContent, Setting, ThemeName, TimeFormat,
};
use copiale_core::{
    Account, AccountSettings, Attachment, BackendError, ChatState, Command, Contact, Content,
    ConversationId, ConversationSummary, Direction, Event, Expiry, GroupMasterKey, GroupUpdate,
    Identity, Member, Message, MessageBody, MessageId, MessageStatus, Peer, Quote, Quoted,
    RecentEmoji, SafetyNumber, ServiceId, StorageFailure, Timestamp, Unsupported, Verification,
};
use copiale_ui::chat::{
    COMPACT_BELOW, ChatView, Compose, Field, Layout, Notice, NoticeAction, NoticeKey, Overlay,
};
use copiale_ui::notifier::Notifier;
use copiale_ui::onboarding::Screen;
use copiale_ui::settings::{self, Settings};
use copiale_ui::theme::{
    self, ActivePalette as _, Appearance, Fonts, InstalledFonts, Scheme, Theme,
};
use copiale_ui::tray::{self, Tray, TrayAction, TrayState};
use copiale_ui::{
    APP_ID, CoreSink, Quit, Request, Shell, main_window, main_window_options, titlebar,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Focusable as _, Hsla,
    InputEvent as _, IntoElement, MouseMoveEvent, Pixels, Point, Render, Rgba, ScrollDelta,
    TestAppContext, VisualTestContext, Window, WindowDecorations, WindowOptions, point, px, size,
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

#[derive(Default)]
struct Notifications(RefCell<Vec<NoticeAction>>);

impl Notifier for Notifications {
    fn apply(&self, action: NoticeAction) {
        self.0.borrow_mut().push(action);
    }
}

impl Notifications {
    fn take(&self) -> Vec<NoticeAction> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

#[derive(Default)]
struct TrayRecorder(Cell<TrayState>);

impl Tray for TrayRecorder {
    fn set(&self, state: TrayState) {
        self.0.set(state);
    }
}

struct Ui {
    /// The main window, as of the last [`Ui::sync_window`].
    window: Cell<Option<AnyWindowHandle>>,
    shell: Entity<Shell>,
    core: Rc<Recorder>,
    notifications: Rc<Notifications>,
    tray: Rc<TrayRecorder>,
}

impl Ui {
    fn open(cx: &mut TestAppContext) -> Self {
        Self::open_with(
            main_window::Options {
                switch_displays: false,
                follow_window_appearance: true,
                raise_by_reopening: false,
            },
            cx,
        )
    }

    fn open_with(options: main_window::Options, cx: &mut TestAppContext) -> Self {
        let ui = Self::start(options, false, cx);
        ui.frame(cx);
        ui
    }

    /// Starts the UI with the main window open, or hidden if `hidden`.
    fn start(options: main_window::Options, hidden: bool, cx: &mut TestAppContext) -> Self {
        cx.update(copiale_ui::init);
        let core = Rc::new(Recorder::default());
        let notifications = Rc::new(Notifications::default());
        let sink: Rc<dyn CoreSink> = core.clone();
        let notifier: Rc<dyn Notifier> = notifications.clone();
        let tray = Rc::new(TrayRecorder::default());
        let shell = cx.update(|cx| {
            tray::install(tray.clone(), cx);
            let shell = cx.new(|_| Shell::new(sink, notifier));
            main_window::install(shell.clone(), options, cx);
            if hidden {
                main_window::start_hidden(cx);
            } else {
                main_window::show(None, cx);
            }
            shell
        });
        let ui = Self {
            window: Cell::new(None),
            shell,
            core,
            notifications,
            tray,
        };
        assert_eq!(ui.sync_window(cx), !hidden);
        ui
    }

    fn window(&self) -> AnyWindowHandle {
        self.window.get().expect("main window open")
    }

    /// Follows the main window as it closes and opens; whether it's open.
    fn sync_window(&self, cx: &mut TestAppContext) -> bool {
        cx.run_until_parked();
        let handle = cx.update(|cx| main_window::handle(cx));
        self.window.set(handle);
        handle.is_some()
    }

    fn event(&self, event: Event, cx: &mut TestAppContext) {
        let shell = self.shell.clone();
        cx.update(|cx| shell.update(cx, |shell, cx| shell.on_event(&event, cx)));
        if self.window.get().is_some() {
            self.frame(cx);
        }
    }

    fn frame(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window(), |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    fn click(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) {
        let id = id.into();
        cx.update_window(self.window(), |_, window, cx| window.click(id.clone(), cx))
            .unwrap();
        self.frame(cx);
    }

    fn type_text(&self, text: &str, cx: &mut TestAppContext) {
        cx.update_window(self.window(), |_, window, cx| window.input(text, cx))
            .unwrap();
    }

    /// Presses `key`, then draws if the window is still open.
    fn press(&self, key: &str, cx: &mut TestAppContext) {
        cx.update_window(self.window(), |_, window, cx| window.press(key, cx))
            .unwrap();
        if cx.update(|cx| main_window::handle(cx)).is_some() {
            self.frame(cx);
        }
    }

    /// Moves the pointer to `offset` from the top-left of element `id`.
    fn point_at(&self, id: &'static str, offset: Point<Pixels>, cx: &mut TestAppContext) {
        cx.update_window(self.window(), |_, window, cx| {
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
        cx.update_window(self.window(), |_, window, _| window.find(id).bounds().top())
            .unwrap()
    }

    fn shows(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) -> bool {
        let id = id.into();
        cx.update_window(self.window(), |_, window, _| window.try_find(id).is_some())
            .unwrap()
    }

    fn hover(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) {
        let id = id.into();
        cx.update_window(self.window(), |_, window, cx| window.hover(id, cx))
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
            [Command::LoadConversations, Command::LoadRecentEmoji]
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
        state: ChatState::Accepted,
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
        edits: 0,
        read: true,
        status,
        expiry: None,
    }
}

#[gpui_kit::test]
fn main_window_options_set_app_id_and_title(cx: &mut TestAppContext) {
    let options = cx.update(|cx| main_window_options(cx));
    assert_eq!(options.app_id.as_deref(), Some(APP_ID));
    assert!(options.app_owns_titlebar_drag);
    assert_eq!(options.window_decorations, Some(WindowDecorations::Server));
    let titlebar = options.titlebar.unwrap();
    assert_eq!(titlebar.title.as_deref(), Some("Copiale"));
    assert!(titlebar.appears_transparent);
}

// Only Linux draws its own titlebar.
#[cfg(target_os = "linux")]
#[gpui_kit::test]
fn server_decorations_leave_out_the_titlebar(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    assert!(!ui.shows("title-bar", cx));
}

struct Bar;

impl Render for Bar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        titlebar::titlebar(cx)
    }
}

#[gpui_kit::test]
fn titlebar_renders(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        copiale_ui::init(cx);
        theme::apply(&Theme::dark(), cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Bar))
            .unwrap()
            .0
    });
    cx.run_until_parked();
    let found = cx
        .update_window(window, |_, window, _| {
            window.try_find("title-bar").is_some()
        })
        .unwrap();
    assert!(found);
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
        Event::LinkRevoked,
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
    ui.type_text("violet tram oyster lantern", cx);
    ui.press("tab", cx);
    ui.type_text("violet tram oyster lantern", cx);
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

    ui.type_text("violet tram oyster lantern", cx);
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

/// Types `text` one key at a time.
fn type_keys(ui: &Ui, text: &str, cx: &mut TestAppContext) {
    for ch in text.chars() {
        ui.type_text(&ch.to_string(), cx);
    }
    ui.frame(cx);
}

fn composer_text(ui: &Ui, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        let view = ui.shell.read(cx).chat().unwrap().read(cx);
        view.composer().read(cx).value().to_string()
    })
}

/// Bodies sent since last asked.
fn sent(ui: &Ui) -> Vec<String> {
    ui.core
        .take()
        .into_iter()
        .filter_map(|command| match command {
            Command::SendText { body, .. } => Some(body.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn composer_converts_emoticons_and_shortcodes(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    type_keys(&ui, "hello :) it's :fire: ok :D", cx);
    assert_eq!(composer_text(&ui, cx), "hello 🙂 it's 🔥 ok :D");
    // Enter ends the last word too.
    ui.press("enter", cx);
    assert_eq!(sent(&ui), ["hello 🙂 it's 🔥 ok 😄"]);

    // Backspace right after converting puts the emoticon back.
    type_keys(&ui, "hm :/ ", cx);
    assert_eq!(composer_text(&ui, cx), "hm 😕 ");
    ui.press("backspace", cx);
    assert_eq!(composer_text(&ui, cx), "hm :/");
    type_keys(&ui, " sure", cx);
    assert_eq!(composer_text(&ui, cx), "hm :/ sure");
}

#[gpui_kit::test]
fn emoticons_stay_when_turned_off(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    cx.update(|cx| settings::preview(&Setting::ConvertEmoticons(false), cx));

    type_keys(&ui, "hi :) :heart:", cx);
    ui.press("enter", cx);
    assert_eq!(sent(&ui), ["hi :) ❤️"]);
}

#[gpui_kit::test]
fn colon_and_two_letters_suggest_emoji(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    type_keys(&ui, "so :", cx);
    assert!(!ui.shows("emoji-picker", cx));
    type_keys(&ui, "thumb", cx);
    assert!(ui.shows("emoji-picker", cx));
    // ↓ and Enter pick instead of sending.
    ui.press("down", cx);
    ui.press("enter", cx);
    assert!(!ui.shows("emoji-picker", cx));
    assert_eq!(composer_text(&ui, cx), "so 👎");
    assert_eq!(sent(&ui), Vec::<String>::new());

    // Esc closes the list and leaves the text.
    type_keys(&ui, " :fir", cx);
    assert!(ui.shows("emoji-picker", cx));
    ui.press("escape", cx);
    assert!(!ui.shows("emoji-picker", cx));
    ui.press("enter", cx);
    assert_eq!(sent(&ui), ["so 👎 :fir"]);

    // Tab picks too; a click picks that entry.
    type_keys(&ui, ":joy", cx);
    ui.press("tab", cx);
    assert_eq!(composer_text(&ui, cx), "😂");
    type_keys(&ui, " :thumb", cx);
    ui.click(("emoji", 1u64), cx);
    assert_eq!(composer_text(&ui, cx), "😂 👎");
}

/// Recent emoji saved since last asked.
fn recent_saved(ui: &Ui) -> Vec<Vec<String>> {
    ui.core
        .take()
        .into_iter()
        .filter_map(|command| match command {
            Command::SetRecentEmoji(recent) => Some(recent.0),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn emoji_picker_inserts_at_the_cursor(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.event(Event::RecentEmoji(RecentEmoji(vec!["❤️".into()])), cx);
    ui.open_alice(cx);
    type_keys(&ui, "ab", cx);
    ui.press("left", cx);

    // Search, then Enter.
    ui.press("secondary-e", cx);
    assert!(ui.shows("emoji-grid", cx));
    type_keys(&ui, "thumb", cx);
    ui.press("enter", cx);
    assert!(!ui.shows("emoji-grid", cx));
    assert_eq!(composer_text(&ui, cx), "a👍b");
    assert_eq!(recent_saved(&ui), [["👍", "❤️"]]);

    // The button; recent emoji come first, arrows move.
    ui.click("emoji-button", cx);
    assert!(ui.shows("emoji-grid", cx));
    ui.press("right", cx);
    ui.press("enter", cx);
    assert_eq!(composer_text(&ui, cx), "a👍❤️b");

    // A click; Esc closes, back to the composer.
    ui.press("secondary-e", cx);
    ui.click(("emoji-cell", 0u64), cx);
    assert_eq!(composer_text(&ui, cx), "a👍❤️❤️b");
    ui.press("secondary-e", cx);
    ui.press("escape", cx);
    assert!(!ui.shows("emoji-grid", cx));
    type_keys(&ui, "c", cx);
    assert_eq!(composer_text(&ui, cx), "a👍❤️❤️cb");
    assert_eq!(sent(&ui), Vec::<String>::new());
}

#[gpui_kit::test]
fn a_few_emoji_alone_draw_large(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.click(("conversation", 1u64), cx);
    let bodies = ["hi", "😀😀", "😀😀😀😀😀😀", "hi 😀"];
    ui.event(
        Event::Messages {
            conversation: ConversationId::new(1),
            before: None,
            messages: (1..=4)
                .zip(bodies)
                .map(|(id, body)| Message {
                    content: Content::text(body),
                    ..message(id, Direction::Incoming, None)
                })
                .collect(),
        },
        cx,
    );
    ui.settle(cx);
    let heights = cx.update(|cx| {
        let log = ui.shell.read(cx).chat().unwrap().read(cx).log().clone();
        (1..4)
            .map(|ix| log.bounds_for_item(ix).unwrap().size.height)
            .collect::<Vec<_>>()
    });
    assert!(heights[0] > heights[1] + px(10.), "{heights:?}");
    assert_eq!(heights[1], heights[2], "{heights:?}");
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
    cx.update_window(ui.window(), |_, window, cx| {
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

#[gpui_kit::test]
fn storage_errors_show_until_dismissed(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    assert!(!ui.shows("storage-error", cx));

    ui.event(
        Event::DatabaseError("database error: disk I/O error".into()),
        cx,
    );
    assert!(ui.shows("storage-error", cx));
    resize(&ui, 500., cx);
    assert!(ui.shows("header-storage-error", cx), "compact");
    ui.click("header-storage-error", cx);
    assert!(!ui.shows("header-storage-error", cx));

    resize(&ui, 1100., cx);
    ui.event(
        Event::DatabaseError("database error: disk I/O error".into()),
        cx,
    );
    ui.click("storage-error", cx);
    assert!(!ui.shows("storage-error", cx));
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
        cx.update_window(ui.window(), |_, window, cx| {
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
        .update_window(ui.window(), |_, window, _| {
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
    ui.press("secondary-f", cx);
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
fn find_searches_and_escape_dismisses(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("secondary-f", cx);
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
        .update_window(ui.window(), |_, window, cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.composer().focus_handle(cx).is_focused(window)
        })
        .unwrap();
    assert!(composer_focused);
}

// macOS scrolls with its own momentum (`kinetic`).
#[cfg(not(target_os = "macos"))]
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
    cx.update_window(ui.window(), |_, window, cx| {
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
        state: ChatState::Accepted,
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
                original: Some(Quoted {
                    text: "ship it?".into(),
                    attachment: None,
                }),
            }),
        },
    ];
    let messages = (1..)
        .zip(contents)
        .map(|(id, content)| Message {
            content,
            edits: u32::from(id % 2 == 0),
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
    let quote = Some("↳ \u{2068}you\u{2069}: ship it?".to_string());
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
fn edits_of_messages_that_go_keep_the_text(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    chat_with_recent(&ui, cx);
    ui.press("up", cx);
    ui.type_text("!", cx);
    let text = chat_state(&ui, cx).0;

    ui.event(
        Event::MessageRemoved {
            conversation: ConversationId::new(1),
            id: MessageId::new(11),
        },
        cx,
    );
    assert_eq!(chat_state(&ui, cx), (text.clone(), Compose::New, None));
    assert!(ui.shows("compose", cx), "says why");
    ui.press("escape", cx);
    assert!(!ui.shows("compose", cx));
    assert_eq!(chat_state(&ui, cx).0, text);

    ui.core.take();
    ui.press("enter", cx);
    assert!(
        ui.core.take().iter().any(|command| matches!(
            command,
            Command::SendText { reply_to: None, body, .. } if body.as_str() == text
        )),
        "sent as a new message"
    );
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
        .update_window(ui.window(), |_, window, cx| {
            let view = ui.shell.read(cx).chat().unwrap().read(cx);
            let focused = view.composer().focus_handle(cx).is_focused(window);
            (view.chat().open_id(), focused)
        })
        .unwrap();

    assert_eq!(open, Some(ConversationId::new(1)));
    assert!(focused);
}

fn contact(id: i64, name: &str) -> Contact {
    Contact {
        id: peer(id),
        name: Some(name.into()),
        phone: None,
    }
}

fn open_id(ui: &Ui, cx: &mut TestAppContext) -> Option<ConversationId> {
    cx.update(|cx| ui.shell.read(cx).chat().unwrap().read(cx).chat().open_id())
}

#[gpui_kit::test]
fn new_chat_picks_a_contact_from_the_keyboard(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.event(
        Event::Contacts(vec![contact(1, "Alice"), contact(3, "Carol Danvers")]),
        cx,
    );

    ui.press("secondary-n", cx);
    assert!(ui.shows("new-chat", cx));
    assert!(matches!(ui.core.take().as_slice(), [Command::LoadContacts]));
    assert!(ui.shows(("contact", 1u64), cx), "both listed");
    ui.press("down", cx);
    let selected = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx).chat();
        chat.picker_selected()
    });
    assert_eq!(selected, Some(1));

    ui.type_text("cd", cx);
    ui.frame(cx);
    assert!(!ui.shows(("contact", 1u64), cx), "only Carol matches");
    ui.press("enter", cx);
    assert!(!ui.shows("new-chat", cx));
    assert!(matches!(
        ui.core.take().as_slice(),
        [Command::OpenConversation(id)] if *id == peer(3)
    ));

    // The core creates the chat; it opens, ready to type in.
    ui.event(
        Event::ConversationUpdated(ConversationSummary {
            last_activity: None,
            ..summary(3, "Carol Danvers", 0)
        }),
        cx,
    );
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(3)));
    let composer_focused = cx
        .update_window(ui.window(), |_, window, cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.composer().focus_handle(cx).is_focused(window)
        })
        .unwrap();
    assert!(composer_focused);
}

#[gpui_kit::test]
fn new_chat_reuses_existing_chats_and_closes_on_escape(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.event(Event::Contacts(vec![contact(2, "Bob")]), cx);

    ui.press("secondary-n", cx);
    ui.core.take();
    ui.press("escape", cx);
    assert!(!ui.shows("new-chat", cx));

    ui.press("secondary-n", cx);
    ui.core.take();
    ui.click(("contact", 0u64), cx);
    assert!(!ui.shows("new-chat", cx));
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)));
    assert!(matches!(
        ui.core.take().as_slice(),
        [Command::LoadMessages { conversation, .. }] if *conversation == ConversationId::new(2)
    ));
}

fn bob_writes(ui: &Ui, id: i64, cx: &mut TestAppContext) {
    let message = Message {
        conversation: ConversationId::new(2),
        sender: Some(peer(2)),
        read: false,
        ..message(id, Direction::Incoming, None)
    };
    ui.event(Event::MessageAdded(message), cx);
    ui.event(Event::ConversationUpdated(summary(2, "Bob", 1)), cx);
}

#[gpui_kit::test]
fn messages_in_unfocused_chats_notify_and_clicks_open_them(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);

    bob_writes(&ui, 20, cx);
    assert_eq!(
        ui.notifications.take(),
        [NoticeAction::Show(Notice {
            key: NoticeKey::Chat(ConversationId::new(2)),
            title: "Bob".into(),
            body: "New message".into(),
            quiet: false,
        })]
    );

    cx.update(|cx| {
        ui.shell.update(cx, |shell, cx| {
            shell.notification_clicked(NoticeKey::Chat(ConversationId::new(2)), cx)
        })
    });
    ui.sync_window(cx);
    ui.frame(cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)));
}

#[gpui_kit::test]
fn notifications_follow_the_settings(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    bob_writes(&ui, 20, cx);
    ui.notifications.take();

    let mut config = Config::default();
    config.notifications.content = NotificationContent::Off;
    cx.update(|cx| cx.set_global(Settings(config)));
    assert_eq!(
        ui.notifications.take(),
        [
            NoticeAction::Close(NoticeKey::Chat(ConversationId::new(2))),
            NoticeAction::Close(NoticeKey::Backlog),
        ]
    );
    bob_writes(&ui, 21, cx);
    assert!(ui.notifications.take().is_empty());
}

fn composer_focused(ui: &Ui, cx: &mut TestAppContext) -> bool {
    cx.update_window(ui.window(), |_, window, cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.composer().focus_handle(cx).is_focused(window)
    })
    .unwrap()
}

fn overlay(ui: &Ui, cx: &mut TestAppContext) -> Option<Overlay> {
    cx.update(|cx| ui.shell.read(cx).chat().unwrap().read(cx).chat().overlay())
}

#[gpui_kit::test]
fn palette_switches_to_other_chats(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("secondary-k", cx);
    assert!(ui.shows("palette", cx));
    assert!(ui.shows(("palette-entry", 0u64), cx));
    // Bob is first: the open chat isn't listed.
    ui.press("enter", cx);
    assert!(!ui.shows("palette", cx));
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)));
    assert!(composer_focused(&ui, cx));

    ui.press("secondary-k", cx);
    ui.type_text("ali", cx);
    ui.frame(cx);
    assert!(!ui.shows(("palette-entry", 1u64), cx), "only Alice matches");
    ui.click(("palette-entry", 0u64), cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(1)));
}

#[gpui_kit::test]
fn palette_toggles_and_closes_on_escape(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("secondary-k", cx);
    ui.press("secondary-k", cx);
    assert!(!ui.shows("palette", cx));
    assert!(composer_focused(&ui, cx));

    ui.press("secondary-k", cx);
    ui.type_text("zzz", cx);
    ui.frame(cx);
    assert!(!ui.shows(("palette-entry", 0u64), cx));
    ui.press("escape", cx);
    assert!(!ui.shows("palette", cx));
    assert!(composer_focused(&ui, cx));

    // Reopens empty.
    ui.press("secondary-k", cx);
    assert!(ui.shows(("palette-entry", 0u64), cx));
}

#[gpui_kit::test]
fn palette_runs_commands(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("secondary-k", cx);
    ui.type_text("new chat", cx);
    ui.frame(cx);
    ui.press("enter", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::NewChat));
    assert!(ui.shows("new-chat", cx));
    assert!(matches!(ui.core.take().as_slice(), [Command::LoadContacts]));

    // One overlay at a time.
    ui.press("secondary-k", cx);
    assert!(!ui.shows("new-chat", cx));
    ui.type_text("settings", cx);
    ui.frame(cx);
    ui.press("down", cx);
    ui.press("up", cx);
    ui.press("enter", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::Settings));
}

#[gpui_kit::test]
fn sidebar_buttons_open_new_chat_and_settings(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.core.take();

    ui.click("new-chat-button", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::NewChat));
    assert!(matches!(ui.core.take().as_slice(), [Command::LoadContacts]));

    ui.press("escape", cx);
    ui.click("settings-button", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::Settings));
    // Online: the status beside it does nothing.
    assert!(ui.core.take().is_empty());
}

fn alice_identity(verification: Verification) -> Event {
    Event::Identity(Identity {
        peer: peer(1),
        safety_number: Some(SafetyNumber::new("0123456789".repeat(6))),
        verification,
    })
}

fn alice_in(state: ChatState) -> Event {
    Event::ConversationUpdated(ConversationSummary {
        state,
        ..summary(1, "Alice", 0)
    })
}

#[gpui_kit::test]
fn requests_are_answered_from_the_banner(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.event(alice_in(ChatState::Request), cx);
    ui.open_alice(cx);
    assert!(ui.shows("request-banner", cx));
    assert!(!composer_focused(&ui, cx));

    ui.press("a", cx);
    assert!(matches!(
        ui.core.take().as_slice(),
        [Command::Accept(id)] if id.get() == 1
    ));
    ui.event(alice_in(ChatState::Accepted), cx);
    ui.frame(cx);

    assert!(!ui.shows("request-banner", cx));
    assert!(composer_focused(&ui, cx));
}

#[gpui_kit::test]
fn blocked_chats_can_be_unblocked_or_deleted(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.event(alice_in(ChatState::Blocked), cx);
    ui.open_alice(cx);
    assert!(ui.shows("request-banner", cx));
    assert!(!ui.shows("accept-request", cx));

    ui.press("b", cx);
    ui.press("a", cx);
    assert!(ui.core.take().is_empty(), "already blocked, not a request");
    ui.click("unblock", cx);
    assert!(matches!(
        ui.core.take().as_slice(),
        [Command::Accept(id)] if id.get() == 1
    ));
    ui.press("d", cx);
    assert!(matches!(
        ui.core.take().as_slice(),
        [Command::DeleteRequest(id)] if id.get() == 1
    ));
}

#[gpui_kit::test]
fn changed_safety_number_is_reviewed_and_confirmed(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    assert!(!ui.shows("identity-banner", cx));

    ui.event(Event::UnconfirmedIdentities(vec![peer(1)]), cx);
    assert!(ui.shows("identity-banner", cx));
    ui.click("review-safety-number", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::SafetyNumber(peer(1))));
    assert!(matches!(
        ui.core.take().as_slice(),
        [Command::LoadIdentity(id)] if *id == peer(1)
    ));
    ui.event(alice_identity(Verification::Changed), cx);
    assert!(ui.shows("safety-number-digits", cx));
    assert!(ui.shows("confirm-safety-number", cx));

    ui.press("enter", cx);
    assert!(matches!(
        ui.core.take().as_slice(),
        [Command::ConfirmIdentity(id)] if *id == peer(1)
    ));
    assert_eq!(overlay(&ui, cx), None);
    assert!(composer_focused(&ui, cx));
    ui.event(Event::UnconfirmedIdentities(Vec::new()), cx);
    assert!(!ui.shows("identity-banner", cx));
}

#[gpui_kit::test]
fn palette_shows_the_safety_number_until_escape(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("secondary-k", cx);
    ui.type_text("safety", cx);
    ui.frame(cx);
    ui.press("enter", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::SafetyNumber(peer(1))));
    ui.core.take();
    ui.event(alice_identity(Verification::Verified), cx);
    assert!(ui.shows("safety-number-digits", cx));
    assert!(!ui.shows("confirm-safety-number", cx));

    ui.press("enter", cx);
    assert!(ui.core.take().is_empty(), "nothing to confirm");
    ui.press("escape", cx);
    assert_eq!(overlay(&ui, cx), None);
    assert!(composer_focused(&ui, cx));
}

#[gpui_kit::test]
fn palette_theme_overrides_the_system(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    let pick = |query: &str, cx: &mut TestAppContext| {
        ui.press("secondary-k", cx);
        ui.type_text(query, cx);
        ui.frame(cx);
        ui.press("enter", cx);
    };

    let choose = |query: &str, cx: &mut TestAppContext| {
        pick("theme", cx);
        assert!(ui.shows("picker-list", cx));
        ui.type_text(query, cx);
        ui.frame(cx);
        ui.press("enter", cx);
        ui.press("escape", cx);
    };

    choose("copiale dark", cx);
    let theme = cx.update(|cx| Settings::get(cx).appearance.theme);
    assert_eq!(theme, ThemeName::CopialeDark);
    cx.update(|cx| assert_theme(&Theme::dark(), cx));
    assert!(composer_focused(&ui, cx));

    // The system no longer decides.
    cx.update(|cx| theme::follow(Scheme::Light, cx));
    cx.update(|cx| assert_theme(&Theme::dark(), cx));

    choose("system", cx);
    cx.update(|cx| assert_theme(&Theme::light(), cx));

    choose("gruvbox light", cx);
    let gruvbox = Theme::built_in(ThemeName::GruvboxLight).unwrap();
    cx.update(|cx| assert_theme(&gruvbox, cx));
}

#[gpui_kit::test]
fn theme_picker_previews_and_saves_or_restores(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    cx.update(|cx| theme::follow(Scheme::Dark, cx));
    open_settings(&ui, "theme", cx);
    ui.press("enter", cx);
    assert!(ui.shows(("picked", 0u64), cx));
    ui.press("down", cx);
    ui.press("down", cx);
    assert_eq!(config(cx).appearance.theme, ThemeName::CopialeLight);
    cx.update(|cx| assert_theme(&Theme::light(), cx));
    ui.press("escape", cx);
    assert_eq!(config(cx).appearance.theme, ThemeName::System);
    cx.update(|cx| assert_theme(&Theme::dark(), cx));
    assert!(ui.shows("settings-list", cx));

    ui.press("enter", cx);
    ui.type_text("nord", cx);
    ui.frame(cx);
    ui.press("enter", cx);
    assert_eq!(config(cx).appearance.theme, ThemeName::Nord);
    let nord = Theme::built_in(ThemeName::Nord).unwrap();
    cx.update(|cx| assert_theme(&nord, cx));
}

#[gpui_kit::test]
fn custom_theme_applies_or_falls_back_to_the_system(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    cx.update(|cx| theme::follow(Scheme::Light, cx));
    let mut custom = config(cx);
    custom.appearance.theme = ThemeName::Custom;
    cx.update(|cx| cx.set_global(Settings(custom.clone())));
    cx.update(|cx| assert_theme(&Theme::light(), cx));
    open_settings(&ui, "theme", cx);
    assert!(ui.shows(("setting-problem", 0u64), cx));
    ui.press("escape", cx);

    let table = include_str!("../themes/nord.toml").replace("name = \"Nord\"\n", "");
    custom.custom_theme = Some(toml::from_str(&table).unwrap());
    cx.update(|cx| cx.set_global(Settings(custom)));
    let nord = Theme::built_in(ThemeName::Nord).unwrap();
    cx.update(|cx| assert_theme(&nord, cx));
    open_settings(&ui, "theme", cx);
    assert!(!ui.shows(("setting-problem", 0u64), cx));
}

#[gpui_kit::test]
fn settings_open_from_the_keyboard_and_close(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("secondary-,", cx);
    assert!(ui.shows("settings", cx));
    assert!(ui.shows("config-path", cx));
    assert!(!ui.shows("config-read-only", cx));
    ui.press("escape", cx);
    assert!(!ui.shows("settings", cx));
    assert!(composer_focused(&ui, cx));

    ui.press("secondary-,", cx);
    ui.click("close-settings", cx);
    assert!(!ui.shows("settings", cx));
}

fn config(cx: &mut TestAppContext) -> Config {
    cx.update(|cx| Settings::get(cx).clone())
}

fn chat_view<T>(ui: &Ui, cx: &mut TestAppContext, f: impl FnOnce(&ChatView) -> T) -> T {
    cx.update(|cx| f(ui.shell.read(cx).chat().unwrap().read(cx)))
}

/// Opens settings filtered to `query`.
fn open_settings(ui: &Ui, query: &str, cx: &mut TestAppContext) {
    ui.press("secondary-,", cx);
    ui.type_text(query, cx);
    ui.frame(cx);
}

#[gpui_kit::test]
fn settings_change_live_from_the_keyboard(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.press("secondary-,", cx);
    assert_eq!(
        chat_view(&ui, cx, |view| view.settings_panel().selected()),
        Some(Field::Theme)
    );

    ui.press("right", cx);
    ui.press("right", cx);
    assert_eq!(config(cx).appearance.theme, ThemeName::CopialeLight);
    cx.update(|cx| assert_theme(&Theme::light(), cx));

    ui.press("down", cx);
    ui.press("down", cx);
    assert_eq!(
        chat_view(&ui, cx, |view| view.settings_panel().selected()),
        Some(Field::MonoFont)
    );
    // Fonts pick from a list; arrows don't change them.
    ui.press("right", cx);
    assert_eq!(config(cx).typography.mono_font, None);
}

#[gpui_kit::test]
fn settings_filter_then_step(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    open_settings(&ui, "density", cx);
    ui.press("right", cx);
    assert_eq!(config(cx).layout.density, Density::Roomy);
    ui.press("left", cx);
    ui.press("left", cx);
    ui.press("left", cx);
    assert_eq!(config(cx).layout.density, Density::Compact);
    ui.click(("setting-option", 6 * 16 + 1u64), cx);
    assert_eq!(config(cx).layout.density, Density::Cozy);
}

#[gpui_kit::test]
fn font_size_and_opacity_apply_to_the_theme(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    open_settings(&ui, "size", cx);
    ui.press("right", cx);
    ui.press("right", cx);
    assert_eq!(config(cx).typography.size, 16);
    assert_eq!(cx.update(|cx| cx.theme().font_size), px(16.));
    assert_eq!(cx.update(|cx| cx.global::<Fonts>().size), 16.);
    // The families GPUI Kit resolved, not virtual names.
    cx.update(|cx| {
        let fonts = cx.global::<Fonts>();
        assert_eq!(fonts.ui, cx.theme().font_family);
        assert_eq!(fonts.body, fonts.ui);
        assert_eq!(fonts.mono, cx.theme().mono_font_family);
    });

    ui.press("escape", cx);
    open_settings(&ui, "opacity", cx);
    ui.press("left", cx);
    ui.press("left", cx);
    assert_eq!(config(cx).window.opacity, 0.9);
    let background = cx.update(|cx| cx.palette().background);
    assert!((background.a - 0.9).abs() < 1e-6, "{background:?}");
    // Popovers stay opaque; scrollbar tracks are clear.
    assert_eq!(cx.update(|cx| cx.theme().popover.a), 1.);
    assert_eq!(cx.update(|cx| cx.theme().scrollbar.a), 0.);
}

#[gpui_kit::test]
fn font_picker_previews_and_saves_or_restores(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    let fonts: Rc<[String]> = ["Alpha Sans", "Beta Mono"].map(String::from).into();
    cx.update(|cx| cx.set_global(InstalledFonts(fonts)));
    let ui_font = |cx: &mut TestAppContext| config(cx).typography.ui_font;

    open_settings(&ui, "ui font", cx);
    ui.press("enter", cx);
    assert!(ui.shows(("picked", 0u64), cx));
    ui.press("down", cx);
    assert_eq!(ui_font(cx).as_deref(), Some("Alpha Sans"));
    assert_eq!(
        cx.update(|cx| cx.global::<Fonts>().ui.clone()),
        "Alpha Sans"
    );
    ui.press("escape", cx);
    assert_eq!(ui_font(cx), None);
    assert!(ui.shows("settings-list", cx));

    ui.press("enter", cx);
    ui.type_text("beta", cx);
    ui.frame(cx);
    ui.press("enter", cx);
    assert_eq!(ui_font(cx).as_deref(), Some("Beta Mono"));
    assert!(ui.shows("settings-list", cx));

    // Closing settings mid-preview restores the font.
    ui.press("enter", cx);
    ui.press("down", cx);
    ui.press("down", cx);
    ui.click("settings-backdrop", cx);
    assert_eq!(overlay(&ui, cx), None);
    assert_eq!(ui_font(cx).as_deref(), Some("Beta Mono"));
}

#[gpui_kit::test]
fn settings_mirror_the_account(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    let account = Account {
        device_name: Some("Copiale on laptop".into()),
        settings: AccountSettings {
            read_receipts: true,
            typing_indicators: false,
        },
    };
    ui.event(Event::Account(account.clone()), cx);
    assert_eq!(
        chat_view(&ui, cx, |view| view.account().cloned()),
        Some(account)
    );
    open_settings(&ui, "account", cx);
    assert!(ui.shows(("setting", 18u64), cx));
    ui.press("right", cx);
    ui.press("enter", cx);
    assert_eq!(config(cx), Config::default());
}

#[gpui_kit::test]
fn layout_settings_change_the_log(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.event(
        Event::MessageAdded(message(11, Direction::Outgoing, Some(MessageStatus::Sent))),
        cx,
    );
    ui.frame(cx);
    let gap = |cx: &mut TestAppContext| ui.top(("row", 11u64), cx) - ui.top(("row", 10u64), cx);
    let cozy = gap(cx);
    cx.update(|cx| settings::change(Setting::Density(Density::Roomy), cx));
    ui.frame(cx);
    assert!(gap(cx) > cozy);

    let time = |cx: &mut TestAppContext| {
        chat_view(&ui, cx, |view| view.chat().row(0).unwrap().prefix.time)
    };
    assert!(!time(cx).ends_with('M'));
    cx.update(|cx| settings::change(Setting::Clock(TimeFormat::H12), cx));
    assert!(time(cx).ends_with("AM") || time(cx).ends_with("PM"));
}

#[gpui_kit::test]
fn quick_setting_changes_end_saved_and_applied(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = ConfigFile::new(dir.path().join("config.toml"));
    let (tx, events) = std::sync::mpsc::channel();
    cx.update(|cx| settings::load_with(file.clone(), move |event| tx.send(event).unwrap(), cx));

    std::fs::write(file.path(), "[layout]\navatars = true\n").unwrap();
    for size in 10..=30 {
        cx.update(|cx| settings::change(Setting::FontSize(size), cx));
    }

    // Until past the watcher's debounce, reloads never undo a change.
    let until = Instant::now() + Duration::from_secs(1);
    while let Some(wait) = until.checked_duration_since(Instant::now()) {
        let Ok(event) = events.recv_timeout(wait) else {
            break;
        };
        cx.update(|cx| settings::apply(event, cx));
        assert_eq!(config(cx).typography.size, 30);
    }
    assert!(config(cx).layout.avatars);
    let saved = file.load().config;
    assert_eq!(saved.typography.size, 30);
    assert!(saved.layout.avatars);
}

#[cfg(unix)]
#[gpui_kit::test]
fn read_only_config_keeps_changes_for_the_session(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    let dir = tempfile::tempdir().unwrap();
    let file = ConfigFile::new(dir.path().join("config.toml"));
    std::fs::write(file.path(), "[layout]\navatars = true\n").unwrap();
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
    let (tx, events) = std::sync::mpsc::channel();
    cx.update(|cx| settings::load_with(file.clone(), move |event| tx.send(event).unwrap(), cx));
    assert!(cx.update(|cx| Settings::read_only(cx)));

    cx.update(|cx| settings::change(Setting::FontSize(20), cx));
    cx.update(|cx| settings::change(Setting::FontSize(22), cx));
    // A reload, as when Nix relinks the file, keeps them.
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(file.path(), "[layout]\navatars = false\n").unwrap();
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
    let event = events.recv_timeout(Duration::from_secs(5)).unwrap();
    cx.update(|cx| settings::apply(event, cx));

    assert!(!config(cx).layout.avatars);
    assert_eq!(config(cx).typography.size, 22);
    assert_eq!(
        std::fs::read_to_string(file.path()).unwrap(),
        "[layout]\navatars = false\n"
    );
    open_settings(&ui, "size", cx);
    assert!(ui.shows("config-read-only", cx));
}

#[gpui_kit::test]
fn cursor_blinks_reuse_the_drawn_log(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.frame(cx);
    // Unlike `Ui::frame`, draws without refreshing, so caches apply.
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(ui.window(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
    };
    let renders = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.log_renders(cx)
        })
    };
    draw(cx);
    let before = renders(cx);
    // As a blink of the cursor does.
    cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.composer().clone().update(cx, |_, cx| cx.notify());
    });
    draw(cx);
    assert_eq!(renders(cx), before);

    ui.event(
        Event::MessageAdded(message(11, Direction::Incoming, None)),
        cx,
    );
    draw(cx);
    assert!(renders(cx) > before);

    // Changes to the chat view redraw it without a refresh.
    let before = renders(cx);
    cx.update(|cx| settings::change(Setting::Density(Density::Roomy), cx));
    draw(cx);
    assert!(renders(cx) > before);
    let before = renders(cx);
    cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().clone();
        chat.update(cx, |_, cx| cx.notify());
    });
    draw(cx);
    assert!(renders(cx) > before);
}

/// Clicks the link opening Alice's message `id`, shown as a bubble.
fn click_link(ui: &Ui, id: u64, cx: &mut TestAppContext) {
    cx.update_window(ui.window(), |_, window, cx| {
        window.click_at(("bubble", id), point(px(16.), px(12.)), cx)
    })
    .unwrap();
    ui.frame(cx);
}

#[gpui_kit::test]
fn links_open_once_confirmed(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    set_style(&ui, MessageStyle::Bubbles, cx);
    ui.event(
        Event::MessageAdded(Message {
            content: Content::text("WWW.\u{430}pple.com/a"),
            ..message(11, Direction::Incoming, None)
        }),
        cx,
    );

    click_link(&ui, 11, cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::Link));
    assert_eq!(cx.opened_url(), None);
    assert!(ui.shows("link", cx));
    ui.press("escape", cx);
    assert_eq!(overlay(&ui, cx), None);
    assert!(composer_focused(&ui, cx));
    assert_eq!(cx.opened_url(), None);

    click_link(&ui, 11, cx);
    ui.press("enter", cx);
    assert_eq!(overlay(&ui, cx), None);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://www.xn--pple-43d.com/a")
    );
    assert!(composer_focused(&ui, cx));
}

fn set_style(ui: &Ui, style: MessageStyle, cx: &mut TestAppContext) {
    cx.update(|cx| settings::change(Setting::MessageStyle(style), cx));
    ui.frame(cx);
}

fn bounds(ui: &Ui, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) -> Bounds<Pixels> {
    let id = id.into();
    cx.update_window(ui.window(), |_, window, _| window.find(id).bounds())
        .unwrap()
}

#[gpui_kit::test]
fn every_message_style_shows_failures_and_actions(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.event(
        Event::MessageAdded(message(
            11,
            Direction::Outgoing,
            Some(MessageStatus::Failed),
        )),
        cx,
    );
    for style in [
        MessageStyle::Log,
        MessageStyle::Blocks,
        MessageStyle::Bubbles,
    ] {
        set_style(&ui, style, cx);
        assert!(ui.shows(("row", 10u64), cx), "{style:?}");
        assert!(ui.shows(("retry", 11u64), cx), "{style:?}");
        assert_eq!(
            ui.shows(("bubble", 10u64), cx),
            style == MessageStyle::Bubbles,
            "{style:?}"
        );
        ui.hover(("row", 10u64), cx);
        ui.frame(cx);
        assert!(ui.shows(("reply", 10u64), cx), "{style:?}");
        ui.hover(("row", 11u64), cx);
        ui.frame(cx);
    }
}

#[gpui_kit::test]
fn blocks_head_groups_and_bubbles_put_ours_on_the_right(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.event(
        Event::MessageAdded(message(11, Direction::Outgoing, Some(MessageStatus::Sent))),
        cx,
    );
    ui.frame(cx);
    let log_height = bounds(&ui, ("row", 10u64), cx).size.height;
    set_style(&ui, MessageStyle::Blocks, cx);
    // The header takes a line of its own.
    assert!(bounds(&ui, ("row", 10u64), cx).size.height > log_height);

    set_style(&ui, MessageStyle::Bubbles, cx);
    assert!(ui.shows(("bubble-name", 10u64), cx));
    assert!(ui.shows(("bubble-name", 11u64), cx));
    let theirs = bounds(&ui, ("bubble", 10u64), cx);
    let ours = bounds(&ui, ("bubble", 11u64), cx);
    let row = bounds(&ui, ("row", 11u64), cx);
    assert!(ours.left() > theirs.left());
    assert!(ours.right() > theirs.right());
    assert!(row.right() - ours.right() < px(32.));
}

#[gpui_kit::test]
fn switching_style_keeps_the_scroll_position(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.click(("conversation", 1u64), cx);
    long_messages(&ui, cx);
    cx.update_window(ui.window(), |_, window, cx| {
        window.scroll("log", ScrollDelta::Lines(point(0., 40.)), cx);
    })
    .unwrap();
    ui.settle(cx);
    let top = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.log().logical_scroll_top().item_ix
        })
    };
    let before = top(cx);
    assert!(before > 0);
    for style in [
        MessageStyle::Bubbles,
        MessageStyle::Blocks,
        MessageStyle::Log,
    ] {
        set_style(&ui, style, cx);
        ui.frame(cx);
        assert_eq!(top(cx), before, "{style:?}");
    }
}

#[gpui_kit::test]
fn overlays_keep_the_mouse_from_the_window_underneath(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.click(("conversation", 1u64), cx);
    long_messages(&ui, cx);
    ui.press("secondary-,", cx);

    ui.hover(("row", 60u64), cx);
    ui.frame(cx);
    assert!(!ui.shows(("reply", 60u64), cx));
    cx.update_window(ui.window(), |_, window, cx| {
        window.scroll("log", ScrollDelta::Lines(point(0., 40.)), cx);
    })
    .unwrap();
    ui.settle(cx);
    let following = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.log().is_following_tail()
    });
    assert!(following);
    assert_eq!(overlay(&ui, cx), Some(Overlay::Settings));
}

#[gpui_kit::test]
fn unlink_asks_then_wipes_and_drops_the_chats(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.core.take();
    open_settings(&ui, "unlink", cx);
    ui.press("enter", cx);
    assert!(ui.shows("confirm-unlink", cx));

    // `Enter` alone doesn't wipe; `Esc` goes back to the list.
    ui.press("enter", cx);
    assert!(ui.shows("confirm-unlink", cx));
    ui.press("escape", cx);
    assert!(ui.shows("settings-list", cx));
    // The first button is Cancel.
    ui.press("enter", cx);
    ui.press("tab", cx);
    ui.press("enter", cx);
    assert!(ui.shows("settings-list", cx));
    assert!(ui.core.take().is_empty());

    ui.press("enter", cx);
    ui.click("confirm-unlink", cx);
    assert!(matches!(ui.core.take().as_slice(), [Command::Unlink]));
    assert_eq!(overlay(&ui, cx), None);

    // The chats go while the device is removed and its data wiped.
    ui.event(Event::Unlinking, cx);
    assert_eq!(ui.screen(cx), Screen::Unlinking);
    assert!(cx.update(|cx| ui.shell.read(cx).chat().is_none()));
    ui.event(
        Event::Unlinked {
            device_name: "Copiale on host".into(),
        },
        cx,
    );
    assert!(matches!(ui.screen(cx), Screen::Unlinked { .. }));
    assert!(cx.update(|cx| ui.shell.read(cx).chat().is_none()));
}

#[gpui_kit::test]
fn an_unlinked_device_asks_before_wiping(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.core.take();
    ui.event(Event::LinkRevoked, cx);
    assert_eq!(ui.screen(cx), Screen::Revoked { busy: false });
    assert!(ui.shows("wipe", cx));

    ui.click("reconnect", cx);
    assert!(matches!(ui.core.take().as_slice(), [Command::Reconnect]));
    ui.event(Event::Connected, cx);
    assert_eq!(ui.screen(cx), Screen::Ready);
    assert!(ui.shows(("row", 10u64), cx));

    ui.event(Event::LinkRevoked, cx);
    ui.click("wipe", cx);
    assert!(matches!(ui.core.take().as_slice(), [Command::Unlink]));
    assert_eq!(ui.screen(cx), Screen::Revoked { busy: true });
    ui.event(Event::Unlinking, cx);
    assert_eq!(ui.screen(cx), Screen::Unlinking);
    ui.event(
        Event::Unlinked {
            device_name: "Copiale on host".into(),
        },
        cx,
    );
    assert!(cx.update(|cx| ui.shell.read(cx).chat().is_none()));
}

#[gpui_kit::test]
fn quit_from_the_keyboard_and_the_palette(cx: &mut TestAppContext) {
    let quits = Rc::new(Cell::new(0));
    let ui = Ui::open(cx);
    cx.update(|cx| {
        let quits = quits.clone();
        cx.on_action(move |_: &Quit, _| quits.set(quits.get() + 1));
    });

    // Anywhere, even before the chats show.
    ui.press("secondary-q", cx);
    assert_eq!(quits.get(), 1);

    ui.event(Event::Ready, cx);
    ui.event(Event::Conversations(vec![summary(1, "Alice", 0)]), cx);
    ui.press("secondary-k", cx);
    ui.type_text("quit", cx);
    ui.frame(cx);
    ui.press("enter", cx);
    assert_eq!(quits.get(), 2);
}

#[gpui_kit::test]
fn alt_arrows_switch_chats(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.type_text("draft", cx);

    ui.press("alt-up", cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(1)), "first");
    let draft = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.composer().read(cx).value()
    });
    assert_eq!(draft, "draft", "staying keeps the draft");

    ui.press("alt-down", cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)));
    assert!(composer_focused(&ui, cx));
    ui.press("alt-down", cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)), "last");
    ui.press("alt-up", cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(1)));
}

#[gpui_kit::test]
fn search_arrows_pick_a_match(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);

    ui.press("secondary-f", cx);
    ui.press("down", cx);
    let highlighted = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.chat().search_selected()
    });
    assert_eq!(highlighted, Some(ConversationId::new(2)));
    ui.press("enter", cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)));
    assert!(composer_focused(&ui, cx));
}

#[gpui_kit::test]
fn tab_moves_focus_from_the_input_to_the_button(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(
        Event::Unlinked {
            device_name: "Copiale on host".into(),
        },
        cx,
    );
    ui.press("tab", cx);
    // Typed text no longer reaches the input; Enter presses the button.
    ui.type_text("x", cx);
    ui.press("enter", cx);
    let commands = ui.core.take();
    assert!(
        matches!(commands.as_slice(), [Command::Link { device_name }] if device_name == "Copiale on host"),
        "{commands:?}"
    );
}

fn resize(ui: &Ui, width: f32, cx: &mut TestAppContext) {
    cx.simulate_window_resize(ui.window(), size(px(width), px(720.)));
    ui.frame(cx);
}

fn layout(ui: &Ui, cx: &mut TestAppContext) -> Layout {
    cx.update(|cx| ui.shell.read(cx).chat().unwrap().read(cx).layout())
}

#[gpui_kit::test]
fn narrow_windows_hide_the_chat_list(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    assert_eq!(layout(&ui, cx), Layout::Full);

    resize(&ui, COMPACT_BELOW - 1., cx);
    assert_eq!(layout(&ui, cx), Layout::Compact);
    ui.settle(cx);
    assert!(!ui.shows("sidebar", cx));
    assert!(ui.shows("log", cx));

    resize(&ui, COMPACT_BELOW, cx);
    assert_eq!(layout(&ui, cx), Layout::Full);
    ui.settle(cx);
    assert!(ui.shows("sidebar", cx));
}

#[gpui_kit::test]
fn the_chat_list_slides_out(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    let width = |cx: &mut TestAppContext| {
        cx.update_window(ui.window(), |_, window, _| {
            window
                .try_find("sidebar-slot")
                .map_or(px(0.), |slot| slot.bounds().size.width)
        })
        .unwrap()
    };
    assert_eq!(width(cx), px(260.), "no slide at start");

    resize(&ui, 500., cx);
    let mut widths = Vec::new();
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(16));
        ui.frame(cx);
        widths.push(width(cx));
    }
    assert!(widths[0] > px(0.) && widths[0] < px(260.), "{widths:?}");
    assert_eq!(*widths.last().unwrap(), px(0.), "{widths:?}");
    assert!(
        widths.windows(2).all(|pair| pair[0] >= pair[1]),
        "{widths:?}"
    );
}

fn long_messages(ui: &Ui, cx: &mut TestAppContext) {
    ui.event(
        Event::Messages {
            conversation: ConversationId::new(1),
            before: None,
            messages: (1..=60)
                .map(|id| Message {
                    content: Content::text(format!("{id} {}", "words that wrap ".repeat(12))),
                    ..message(id, Direction::Incoming, None)
                })
                .collect(),
        },
        cx,
    );
    for _ in 0..5 {
        ui.frame(cx);
    }
}

#[gpui_kit::test]
fn the_log_keeps_its_place_across_layouts(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.click(("conversation", 1u64), cx);
    long_messages(&ui, cx);
    let log_bottom = |cx: &mut TestAppContext| {
        cx.update_window(ui.window(), |_, window, _| {
            window.find("log").bounds().bottom()
        })
        .unwrap()
    };
    let newest_bottom = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.log().bounds_for_item(59).map(|bounds| bounds.bottom())
        })
    };

    // At the end, the newest message stays at the bottom throughout.
    let mut gaps = vec![log_bottom(cx) - newest_bottom(cx).unwrap()];
    resize(&ui, 500., cx);
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(16));
        ui.frame(cx);
        gaps.push(log_bottom(cx) - newest_bottom(cx).expect("newest in view"));
    }
    assert!(
        gaps.iter().all(|gap| *gap == gaps[0]),
        "newest message moved: {gaps:?}"
    );

    // Scrolled well up, the top message stays the top one. Notch by notch:
    // GPUI's own wheel handling (macOS) stops at the last measured message.
    for _ in 0..15 {
        cx.update_window(ui.window(), |_, window, cx| {
            window.scroll("log", ScrollDelta::Lines(point(0., 10.)), cx);
        })
        .unwrap();
        ui.frame(cx);
    }
    // Lets the wheel's momentum die out.
    for _ in 0..3 {
        ui.settle(cx);
    }
    let following = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.log().is_following_tail()
    });
    assert!(!following);
    let top = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.log().logical_scroll_top().item_ix
        })
    };
    let before = top(cx);
    resize(&ui, 1000., cx);
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(16));
        ui.frame(cx);
        assert_eq!(top(cx), before);
    }
}

#[gpui_kit::test]
fn compact_mode_switches_chats_through_the_palette(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    resize(&ui, 500., cx);

    ui.press("secondary-f", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::Palette));
    ui.press("enter", cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)));
    assert!(composer_focused(&ui, cx));

    ui.press("alt-up", cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(1)));
}

#[gpui_kit::test]
fn compact_header_shows_unread_chats_and_the_connection(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    assert!(!ui.shows("header-connection", cx), "full: in the sidebar");
    resize(&ui, 500., cx);

    assert!(ui.shows("header-connection", cx), "connecting");
    ui.event(Event::Connected, cx);
    ui.event(Event::Synced, cx);
    assert!(!ui.shows("header-connection", cx), "online");

    assert!(!ui.shows("unread-elsewhere", cx));
    ui.event(Event::ConversationUpdated(summary(2, "Bob", 1)), cx);
    assert!(ui.shows("unread-elsewhere", cx));
    ui.click("unread-elsewhere", cx);
    assert_eq!(overlay(&ui, cx), Some(Overlay::Palette));
}

#[gpui_kit::test]
fn going_compact_drops_the_search(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.press("secondary-f", cx);
    ui.type_text("bo", cx);
    ui.frame(cx);

    resize(&ui, 500., cx);
    let count = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.chat().conversations().count()
    });
    assert_eq!(count, 2);
    assert!(composer_focused(&ui, cx));
}

fn request(ui: &Ui, request: Request, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| main_window::handle_request(request, cx));
    let open = ui.sync_window(cx);
    if open {
        ui.frame(cx);
    }
    open
}

fn show() -> Request {
    Request::Show {
        activation_token: None,
    }
}

fn toggle() -> Request {
    Request::Toggle {
        activation_token: None,
    }
}

fn focused(cx: &mut TestAppContext) -> bool {
    cx.update(main_window::is_focused)
}

#[gpui_kit::test]
fn hidden_windows_keep_chats_running(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    ui.type_text("half a thought", cx);

    cx.update(main_window::hide);
    assert!(!ui.sync_window(cx), "closed");

    // Events still apply, and notify since nothing is focused.
    bob_writes(&ui, 20, cx);
    assert!(!ui.notifications.take().is_empty());

    assert!(request(&ui, show(), cx), "reopened");
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(1)));
    let draft = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.composer().read(cx).value()
    });
    assert_eq!(draft, "half a thought");
    assert!(composer_focused(&ui, cx));
    // Typing still reaches the composer.
    ui.type_text("!", cx);
    let draft = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.composer().read(cx).value()
    });
    assert_eq!(draft, "half a thought!");
}

#[gpui_kit::test]
fn hidden_starts_open_no_window_until_asked(cx: &mut TestAppContext) {
    let options = main_window::Options {
        switch_displays: false,
        follow_window_appearance: true,
        raise_by_reopening: false,
    };
    let ui = Ui::start(options, true, cx);
    assert!(!ui.tray.0.get().shown);

    ui.event(Event::Ready, cx);
    ui.event(Event::Conversations(vec![summary(2, "Bob", 0)]), cx);
    bob_writes(&ui, 20, cx);
    assert!(
        !ui.notifications.take().is_empty(),
        "runs in the background"
    );

    assert!(request(&ui, toggle(), cx), "shown");
    assert!(ui.tray.0.get().shown);
}

#[gpui_kit::test]
fn toggle_hides_focused_windows_and_shows_others(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    cx.update_window(ui.window(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    assert!(focused(cx));

    assert!(!request(&ui, toggle(), cx), "focused: hidden");
    assert!(request(&ui, toggle(), cx), "hidden: shown");
    assert!(composer_focused(&ui, cx));

    // Open but unfocused: raised, not hidden.
    VisualTestContext::from_window(ui.window(), cx).deactivate_window();
    assert!(!focused(cx));
    assert!(request(&ui, toggle(), cx));
    assert!(focused(cx));
}

#[gpui_kit::test]
fn escape_hides_compact_windows_last(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    chat_with_recent(&ui, cx);
    ui.press("escape", cx);
    assert!(ui.sync_window(cx), "full: stays");

    resize(&ui, 500., cx);
    // A reply is cancelled first.
    ui.hover(("row", 10u64), cx);
    ui.click(("reply", 10u64), cx);
    let compose = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let chat = ui.shell.read(cx).chat().unwrap().read(cx);
            chat.chat().compose()
        })
    };
    assert_eq!(compose(cx), Compose::Reply(MessageId::new(10)));
    ui.press("escape", cx);
    assert!(ui.sync_window(cx), "cancelled the reply");
    assert_eq!(compose(cx), Compose::New);

    ui.press("escape", cx);
    assert!(!ui.sync_window(cx), "hidden");
}

#[gpui_kit::test]
fn notification_clicks_show_hidden_windows(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    ui.open_alice(cx);
    cx.update(main_window::hide);
    ui.sync_window(cx);

    cx.update(|cx| {
        ui.shell.update(cx, |shell, cx| {
            shell.notification_clicked(NoticeKey::Chat(ConversationId::new(2)), cx)
        })
    });
    assert!(ui.sync_window(cx));
    ui.frame(cx);
    assert_eq!(open_id(&ui, cx), Some(ConversationId::new(2)));
    assert!(composer_focused(&ui, cx));
}

#[gpui_kit::test]
fn onboarding_survives_hiding(cx: &mut TestAppContext) {
    let ui = Ui::open(cx);
    ui.event(
        Event::Unlinked {
            device_name: "Copiale on host".into(),
        },
        cx,
    );
    cx.update(main_window::hide);
    ui.sync_window(cx);
    ui.event(Event::LinkUrl(URL.into()), cx);

    assert!(request(&ui, show(), cx));
    assert!(matches!(ui.screen(cx), Screen::Linking { .. }));
    assert!(ui.shows("cancel-link", cx));
}

#[gpui_kit::test]
fn the_tray_follows_the_window_and_unread_chats(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    assert_eq!(
        ui.tray.0.get(),
        TrayState {
            shown: true,
            unread: true
        }
    );

    ui.event(Event::ConversationUpdated(summary(1, "Alice", 0)), cx);
    assert!(!ui.tray.0.get().unread);

    cx.update(main_window::hide);
    ui.sync_window(cx);
    assert!(!ui.tray.0.get().shown);
    bob_writes(&ui, 20, cx);
    assert!(ui.tray.0.get().unread, "while hidden too");
}

#[gpui_kit::test]
fn tray_clicks_hide_open_windows_and_show_hidden_ones(cx: &mut TestAppContext) {
    let ui = Ui::ready(cx);
    let click = |cx: &mut TestAppContext| {
        cx.update(|cx| main_window::handle_tray(TrayAction::Activate, cx));
        ui.sync_window(cx)
    };
    // Unfocused, as clicking the tray takes focus: hidden all the same.
    assert!(!focused(cx));
    assert!(!click(cx));
    assert!(click(cx));
    assert!(ui.tray.0.get().shown);
}

#[gpui_kit::test]
fn unfocused_windows_raise_by_reopening_where_activation_is_ignored(cx: &mut TestAppContext) {
    let ui = Ui::open_with(
        main_window::Options {
            switch_displays: false,
            follow_window_appearance: true,
            raise_by_reopening: true,
        },
        cx,
    );
    ui.event(Event::Ready, cx);
    ui.event(Event::Conversations(vec![summary(1, "Alice", 0)]), cx);
    ui.type_text("draft", cx);
    let before = ui.window().window_id();

    assert!(!focused(cx));
    assert!(request(&ui, show(), cx));
    assert_ne!(ui.window().window_id(), before, "a new window");
    assert!(composer_focused(&ui, cx));
    let draft = cx.update(|cx| {
        let chat = ui.shell.read(cx).chat().unwrap().read(cx);
        chat.composer().read(cx).value()
    });
    assert_eq!(draft, "draft");

    // Focused: toggling hides, as before.
    cx.update_window(ui.window(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    assert!(!request(&ui, toggle(), cx));
}
