//! Main window once linked: chat list on the left, the open conversation on
//! the right.

use std::borrow::Cow;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use copiale_core::config::{self, MessageStyle, Setting, TimestampVisibility};
use copiale_core::{
    Account, Command, ConversationId, ConversationSummary, Direction, Event, MessageId,
    MessageStatus, Peer, Timestamp,
};
use gpui_kit::base::{Easing, Presence, PresencePhase, Transition};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, App, AppContext as _, AsKeystroke as _, Context, Entity, FocusHandle,
    Focusable as _, FollowMode, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    KeyBinding, KeyContext, ListAlignment, ListState, MouseButton, MouseMoveEvent,
    ParentElement as _, Pixels, Rems, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, StyledText, Subscription, Task,
    TestSupportExt as _, TextRun, WeakEntity, Window, WindowId, actions, canvas, div, font, list,
    point, px, relative, rems,
};

use super::layout::{Layout, Spacing};
use super::model::{
    Chat, Compose, Connection, Effects, LogChange, Overlay, Row, member_list, title,
};
use super::notices::Notices;
use super::palette::PaletteAction;
use super::settings_panel::{Activation, FIELDS, Field, Section, SettingsPanel, Value};
use super::text::message_markdown;
use super::time::Clock;
use crate::drawer::drawer;
use crate::kinetic::{self, Kinetic, kinetic};
use crate::notifier::Notifier;
use crate::settings::{self, Settings};
use crate::theme::{ActivePalette as _, Fonts, InstalledFonts};
use crate::{CoreSink, Quit, main_window, tray};

/// Load the previous page when the top visible row is this close to the
/// oldest loaded one.
const LOAD_OLDER_WITHIN: usize = 20;

const PREFIX_SIZE: Rems = rems(0.875);
/// How long a pause in typing counts as having stopped.
const TYPING_IDLE: Duration = Duration::from_secs(5);

/// Above the first and below the last message.
const LOG_PADDING: Pixels = px(8.);

/// Height of the strip at the sidebar's top whose hover reveals search.
const SEARCH_STRIP: Pixels = px(12.);
/// How long search takes to slide in or out.
const SEARCH_SLIDE: Duration = Duration::from_millis(260);

const SIDEBAR_WIDTH: Pixels = px(260.);
/// How long the chat list takes to slide in or out between layouts.
const SIDEBAR_SLIDE: Duration = Duration::from_millis(220);

const CONTEXT: &str = "ChatView";

actions!(
    chat,
    [
        FocusSearch,
        EditLast,
        SelectLast,
        SelectPrevious,
        SelectNext,
        ReplySelected,
        EditSelected,
        DeleteSelected,
        DeleteForEveryone,
        DeleteForMe,
        Cancel,
        NewChat,
        PickPrevious,
        PickNext,
        TogglePalette,
        OpenSettings,
        SettingPrevious,
        SettingNext,
        SettingDecrease,
        SettingIncrease,
        PreviousChat,
        NextChat,
        SearchPrevious,
        SearchNext,
        HideWindow,
    ]
);

/// Key context around the composer; which one depends on whether a message
/// is selected or being deleted.
const COMPOSING: &str = "Composing";
const SELECTING: &str = "Selecting";
const CONFIRMING: &str = "Confirming";
/// Key context of the new chat picker and the palette.
const PICKING: &str = "Picking";
/// Key context of the search box.
const SEARCHING: &str = "Searching";
/// Key context of the settings list.
const SETTINGS: &str = "Settings";

/// Registers the chat key bindings. Composer bindings act on the input
/// itself, ahead of its own, and pass keys on when they don't apply.
pub fn init(cx: &mut App) {
    let composing = Some("Composing > Input");
    let selecting = Some("Selecting > Input");
    let confirming = Some("Confirming > Input");
    cx.bind_keys([
        KeyBinding::new("secondary-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("secondary-n", NewChat, Some(CONTEXT)),
        KeyBinding::new("secondary-k", TogglePalette, Some(CONTEXT)),
        KeyBinding::new("secondary-,", OpenSettings, Some(CONTEXT)),
        KeyBinding::new("alt-up", PreviousChat, Some(CONTEXT)),
        KeyBinding::new("alt-down", NextChat, Some(CONTEXT)),
        // After everything else Esc does.
        KeyBinding::new("escape", HideWindow, Some(CONTEXT)),
        KeyBinding::new("up", PickPrevious, Some("Picking > Input")),
        KeyBinding::new("down", PickNext, Some("Picking > Input")),
        KeyBinding::new("up", SettingPrevious, Some("Settings > Input")),
        KeyBinding::new("down", SettingNext, Some("Settings > Input")),
        KeyBinding::new("left", SettingDecrease, Some("Settings > Input")),
        KeyBinding::new("right", SettingIncrease, Some("Settings > Input")),
        KeyBinding::new("up", SearchPrevious, Some("Searching > Input")),
        KeyBinding::new("down", SearchNext, Some("Searching > Input")),
        KeyBinding::new("up", EditLast, composing),
        KeyBinding::new("shift-up", SelectLast, composing),
        KeyBinding::new("escape", Cancel, composing),
        KeyBinding::new("up", SelectPrevious, selecting),
        KeyBinding::new("down", SelectNext, selecting),
        KeyBinding::new("r", ReplySelected, selecting),
        KeyBinding::new("e", EditSelected, selecting),
        KeyBinding::new("delete", DeleteSelected, selecting),
        KeyBinding::new("backspace", DeleteSelected, selecting),
        KeyBinding::new("escape", Cancel, selecting),
        KeyBinding::new("e", DeleteForEveryone, confirming),
        KeyBinding::new("m", DeleteForMe, confirming),
        KeyBinding::new("escape", Cancel, confirming),
    ]);
}
const MAX_SENDER_CHARS: usize = 24;

pub struct ChatView {
    core: Rc<dyn CoreSink>,
    chat: Chat,
    notices: Notices,
    notifier: Rc<dyn Notifier>,
    log: ListState,
    kinetic: Kinetic,
    /// Holds focus while no input does, so shortcuts work.
    focus: FocusHandle,
    /// Inputs for the window showing the view, if any.
    inputs: Option<Inputs>,
    /// The window is active.
    active: bool,
    /// The composer's text while no window shows it.
    draft: String,
    /// Clear and focus the composer on the next frame.
    ready_pending: bool,
    /// Focus the composer on the next frame.
    focus_pending: bool,
    /// The pointer reached the sidebar's top and hasn't left the sidebar.
    search_hovered: bool,
    sidebar_top: Rc<Cell<Pixels>>,
    settings_panel: SettingsPanel,
    settings_scroll: ScrollHandle,
    /// Device name and privacy settings, once the core reports them.
    account: Option<Account>,
    layout: Layout,
    /// A frame has been drawn, so layout changes animate.
    drawn: bool,
    /// Redraws the reconnect countdown.
    countdown: Option<Task<()>>,
    /// Redraws when a typing indicator expires.
    typing_expiry: Option<Task<()>>,
    /// Redraws disappearing messages' countdowns.
    timers: Option<Task<()>>,
    /// The log ends with a typing line.
    typing_row: bool,
    /// The row under the pointer.
    hovered: Option<MessageId>,
    /// Rows showing their actions, last frame.
    with_actions: Vec<MessageId>,
    /// Tells the core we stopped typing after a pause.
    typing_idle: Option<Task<()>>,
    log_part: Entity<Part>,
    list_part: Entity<Part>,
    /// Redraws each minute, for chat ages and day labels.
    _minutes: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// A part of the chat view drawn as a view of its own, so GPUI reuses its
/// last frame until the chat view changes: a blink of the composer's cursor
/// then redraws the composer, not the messages and chat list.
struct Part {
    view: WeakEntity<ChatView>,
    render: fn(&mut ChatView, &mut Window, &mut Context<ChatView>) -> AnyElement,
    /// How often it was drawn anew.
    renders: usize,
    /// Redraws when the chat view changes; notifying it alone leaves cached
    /// children as they were.
    observing: Option<Subscription>,
}

impl Render for Part {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        if self.observing.is_none()
            && let Some(view) = self.view.upgrade()
        {
            self.observing = Some(cx.observe(&view, |_, _, cx| cx.notify()));
        }
        let render = self.render;
        self.view
            .update(cx, |view, cx| render(view, window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}

fn full_size() -> StyleRefinement {
    StyleRefinement::default().size_full()
}

/// The inputs and window subscriptions, made for the window showing the
/// view.
struct Inputs {
    window: WindowId,
    search: Entity<InputState>,
    composer: Entity<TextareaState>,
    /// The new chat picker's query.
    new_chat: Entity<InputState>,
    /// The palette's query.
    palette: Entity<InputState>,
    /// The settings list's filter.
    settings: Entity<InputState>,
    /// The font picker's query.
    font: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    /// Asks the core for the chat list; shown once attached to a window.
    pub fn new(core: Rc<dyn CoreSink>, notifier: Rc<dyn Notifier>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe_global::<Settings>(|view, cx| {
            let settings = Settings::get(cx);
            for action in view.notices.set_content(settings.notifications.content) {
                view.notifier.apply(action);
            }
            view.chat
                .set_twelve_hour(settings.layout.clock == config::Clock::H12);
            // Fonts, spacing and timestamps change row heights.
            view.log.remeasure();
            cx.notify();
        })];
        core.send(Command::LoadConversations);
        let focus = cx.focus_handle();
        let mut chat = Chat::new(Clock::system());
        chat.set_twelve_hour(Settings::get(cx).layout.clock == config::Clock::H12);
        let part = |render, cx: &mut Context<Self>| {
            let view = cx.weak_entity();
            cx.new(|_| Part {
                view,
                render,
                renders: 0,
                observing: None,
            })
        };
        let log_part = part(|view, _, cx| view.render_log(cx), cx);
        let list_part = part(|view, window, cx| view.render_sidebar(window, cx), cx);
        let minutes = cx.spawn(async move |view, cx| {
            loop {
                let into_minute = Timestamp::now().as_millis() % 60_000;
                // A little late, so the labels have changed by then.
                let delay = Duration::from_millis(60_000 - into_minute + 10);
                cx.background_executor().timer(delay).await;
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            core,
            chat,
            notices: Notices::new(Settings::get(cx).notifications.content),
            notifier,
            log: ListState::new(0, ListAlignment::Top, px(1000.)),
            kinetic: Kinetic::default(),
            focus,
            inputs: None,
            active: false,
            draft: String::new(),
            ready_pending: false,
            focus_pending: true,
            search_hovered: false,
            sidebar_top: Rc::default(),
            settings_panel: SettingsPanel::default(),
            settings_scroll: ScrollHandle::new(),
            account: None,
            layout: Layout::default(),
            drawn: false,
            countdown: None,
            typing_expiry: None,
            timers: None,
            typing_row: false,
            hovered: None,
            with_actions: Vec::new(),
            typing_idle: None,
            log_part,
            list_part,
            _minutes: minutes,
            _subscriptions: subscriptions,
        }
    }

    pub fn chat(&self) -> &Chat {
        &self.chat
    }

    /// Makes the inputs for `window`, unless it already has them, keeping
    /// the composer's draft.
    pub fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = window.window_handle().window_id();
        if self
            .inputs
            .as_ref()
            .is_some_and(|inputs| inputs.window == id)
        {
            return;
        }
        let draft = match self.inputs.take() {
            Some(old) => old.composer.read(cx).value().to_string(),
            None => std::mem::take(&mut self.draft),
        };
        let placeholder = self
            .chat
            .open_conversation()
            .map_or("Message".into(), |summary| {
                format!("Message {}", title(summary))
            });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let composer = cx.new(|cx| {
            let mut composer = TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(placeholder);
            // Inserted, so the cursor ends up after it.
            composer.insert(draft, window, cx);
            composer
        });
        let new_chat = cx.new(|cx| InputState::new(window, cx).placeholder("New chat with…"));
        let palette = cx.new(|cx| InputState::new(window, cx).placeholder("Chat or command…"));
        let settings = cx.new(|cx| InputState::new(window, cx).placeholder("Filter settings…"));
        let font = cx.new(|cx| InputState::new(window, cx).placeholder("Font…"));
        let subscriptions = vec![
            cx.subscribe_in(&settings, window, Self::on_settings_event),
            cx.subscribe_in(&font, window, Self::on_font_event),
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.subscribe_in(&new_chat, window, Self::on_picker_event),
            cx.subscribe_in(&palette, window, Self::on_picker_event),
            cx.subscribe_in(&composer, window, Self::on_composer_event),
            cx.observe_window_bounds(window, |view, window, cx| view.resized(window, cx)),
            cx.observe_window_activation(window, |view, window, _| {
                view.active = window.is_window_active();
                if view.active
                    && let Some(command) = view.chat.activated()
                {
                    view.core.send(command);
                }
            }),
        ];
        self.inputs = Some(Inputs {
            window: id,
            search,
            composer,
            new_chat,
            palette,
            settings,
            font,
            _subscriptions: subscriptions,
        });
        self.active = window.is_window_active();
        self.layout = layout_for(window);
        self.drawn = false;
        self.chat.close_overlay();
        self.focus_pending = true;
        cx.notify();
    }

    /// The window closed: keeps the draft and drops what the next window
    /// starts without.
    pub fn detach(&mut self, cx: &mut Context<Self>) {
        if let Some(inputs) = self.inputs.take() {
            self.draft = inputs.composer.read(cx).value().to_string();
        }
        self.active = false;
        self.chat.set_search("");
        self.close_font_picker(false, cx);
        self.chat.close_overlay();
        self.search_hovered = false;
        self.hovered = None;
    }

    fn inputs(&self) -> &Inputs {
        self.inputs.as_ref().expect("attached to a window")
    }

    /// Focuses the composer on the next frame.
    pub fn focus_composer(&mut self, cx: &mut Context<Self>) {
        self.focus_pending = true;
        cx.notify();
    }

    /// Opens a chat, ready to type in once shown.
    pub fn open_later(&mut self, id: ConversationId, cx: &mut Context<Self>) {
        self.open_chat(id, cx);
        self.ready_pending = true;
        cx.notify();
    }

    pub fn search(&self) -> &Entity<InputState> {
        &self.inputs().search
    }

    pub fn composer(&self) -> &Entity<TextareaState> {
        &self.inputs().composer
    }

    pub fn log(&self) -> &ListState {
        &self.log
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Switches layout at the breakpoint; going compact drops the hidden
    /// search.
    fn resized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = layout_for(window);
        if layout == self.layout {
            return;
        }
        self.layout = layout;
        if !layout.shows_chat_list() {
            let focused = self
                .inputs()
                .search
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
            self.inputs()
                .search
                .update(cx, |search, cx| search.set_value("", window, cx));
            self.chat.set_search("");
            self.search_hovered = false;
            if focused {
                self.restore_focus(window, cx);
            }
        }
        cx.notify();
    }

    /// Applies a core event, shown or not.
    pub fn on_event(&mut self, event: &Event, cx: &mut Context<Self>) {
        if let Event::Account(account) = event {
            self.account = Some(account.clone());
        }
        let effects = self.chat.apply(event, self.active);
        self.run(effects, cx);
        if let Some(id) = self.chat.take_requested_open() {
            self.open_later(id, cx);
        }
        let focused = self.active.then(|| self.chat.open_id()).flatten();
        for action in self.notices.apply(event, focused) {
            self.notifier.apply(action);
        }
        let unread = self.chat.has_unread();
        tray::update(cx, |tray| tray.unread = unread);
        if let Event::Conversations(_) = event
            && let Some(effects) = self.chat.open_latest(self.active)
        {
            self.run(effects, cx);
            self.ready_pending = true;
        }
        match event {
            Event::Disconnected { .. } => self.start_countdown(cx),
            Event::Typing { .. } => self.expire_typing(cx),
            Event::MessageAdded(_) => {
                self.expire_typing(cx);
                self.tick_timers(cx);
            }
            Event::Messages { .. } | Event::MessageUpdated(_) => self.tick_timers(cx),
            _ => {}
        }
    }

    /// Redraws as countdowns change, while any run in the open conversation.
    fn tick_timers(&mut self, cx: &mut Context<Self>) {
        self.timers = Some(cx.spawn(async move |view, cx| {
            loop {
                let Ok(Some(delay)) = view.read_with(cx, |view, _| view.chat.timers_change_in())
                else {
                    break;
                };
                // A little late, so the label has changed by then.
                cx.background_executor()
                    .timer(delay + Duration::from_millis(10))
                    .await;
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    /// Redraws as typing indicators expire.
    fn expire_typing(&mut self, cx: &mut Context<Self>) {
        let Some(delay) = self.chat.typing_changes_in() else {
            self.typing_expiry = None;
            return;
        };
        self.typing_expiry = Some(cx.spawn(async move |view, cx| {
            // A little late, so it has expired by then.
            cx.background_executor()
                .timer(delay + Duration::from_millis(10))
                .await;
            view.update(cx, |view, cx| {
                view.sync_typing_row(cx);
                cx.notify();
                view.expire_typing(cx);
            })
            .ok();
        }));
    }

    /// Tells the core whether we're typing; stops after [`TYPING_IDLE`]
    /// without changes.
    fn typing(&mut self, text: &str, cx: &mut Context<Self>) {
        let command = self.chat.typing_command(text);
        let typing = matches!(command, Some(Command::Typing { typing: true, .. }));
        self.send(command);
        self.typing_idle = typing.then(|| {
            cx.spawn(async move |view, cx| {
                cx.background_executor().timer(TYPING_IDLE).await;
                view.update(cx, |view, _| view.send(view.chat.typing_command("")))
                    .ok();
            })
        });
    }

    fn start_countdown(&mut self, cx: &mut Context<Self>) {
        self.countdown = Some(cx.spawn(async move |view, cx| {
            loop {
                let Ok(Some(delay)) = view.read_with(cx, |view, _| {
                    let chat = &view.chat;
                    chat.connection_label_changes_in(chat.clock().now())
                }) else {
                    break;
                };
                // A little late, so the label has changed by then.
                cx.background_executor()
                    .timer(delay + Duration::from_millis(10))
                    .await;
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    pub fn open(&mut self, id: ConversationId, window: &mut Window, cx: &mut Context<Self>) {
        self.open_chat(id, cx);
        self.ready_composer(window, cx);
    }

    fn open_chat(&mut self, id: ConversationId, cx: &mut Context<Self>) {
        if self.chat.open_id() != Some(id) && self.typing_idle.take().is_some() {
            self.send(self.chat.typing_command(""));
        }
        let effects = self.chat.open(id);
        self.run(effects, cx);
    }

    /// Clears the composer for the open conversation and focuses it.
    fn ready_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self
            .chat
            .open_conversation()
            .map(|summary| format!("Message {}", title(summary)));
        self.inputs().composer.update(cx, |composer, cx| {
            composer.set_value("", window, cx);
            if let Some(placeholder) = placeholder {
                composer.set_placeholder(placeholder, window, cx);
            }
            composer.focus(window, cx);
        });
    }

    fn run(&mut self, effects: Effects, cx: &mut Context<Self>) {
        for command in effects.commands {
            self.core.send(command);
        }
        for change in effects.log {
            match change {
                LogChange::Reset(count) => {
                    self.kinetic.stop();
                    self.log.reset(count);
                    self.typing_row = false;
                    self.log.set_follow_mode(FollowMode::Tail);
                }
                LogChange::Inserted { at, count } => {
                    let at_end = at + count == self.chat.row_count();
                    let reveal = at_end && self.hold_position(cx);
                    self.log.splice(at..at, count);
                    let next = at + count;
                    if next < self.log.item_count() {
                        self.log.remeasure_items(next..next + 1);
                    }
                    if reveal {
                        self.kinetic.settle_to_end(cx);
                    }
                }
                LogChange::Changed(ix) => self.log.remeasure_items(ix..ix + 1),
                LogChange::Removed(ix) => {
                    if ix == self.chat.row_count() && !self.typing_row {
                        self.ease_removal(ix, cx);
                    }
                    self.log.splice(ix..ix + 1, 0);
                    if ix < self.log.item_count() {
                        self.log.remeasure_items(ix..ix + 1);
                    }
                }
            }
        }
        self.sync_typing_row(cx);
        cx.notify();
    }

    /// Before rows are added at the end while it's in view: keeps the
    /// current position instead of following the end, so the caller can
    /// ease there. `false` if not showing the end, or not animating.
    fn hold_position(&mut self, cx: &App) -> bool {
        if !self.log.is_following_tail() || !kinetic::animates(cx) {
            return false;
        }
        // Pin the position: just short of the end, since setting within a
        // pixel of it follows again, then the rest, which doesn't.
        const SHORT: Pixels = px(2.);
        let max = self.log.max_offset_for_scrollbar().y + LOG_PADDING * 2.;
        self.log
            .set_offset_from_scrollbar(point(px(0.), -(max - SHORT).max(px(0.))));
        self.log.scroll_by(SHORT);
        self.log.pause_following_tail();
        true
    }

    /// Before the last row goes while the end is in view: the rows above
    /// drop into its place, so draw them where they were and ease down.
    fn ease_removal(&mut self, ix: usize, cx: &App) {
        if !self.log.is_following_tail() || !kinetic::animates(cx) {
            return;
        }
        let scrollable = self.log.max_offset_for_scrollbar().y;
        if let Some(bounds) = self.log.bounds_for_item(ix)
            && scrollable > px(0.)
        {
            self.kinetic
                .shift_by(-bounds.size.height.min(scrollable), cx);
        }
    }

    /// Shows or hides the typing line after the last message. A message
    /// from the typist is inserted before it first, so takes its place.
    fn sync_typing_row(&mut self, cx: &App) {
        let wanted = self.chat.typing_label().is_some();
        if wanted == self.typing_row {
            return;
        }
        let at = self.chat.row_count();
        if wanted {
            let reveal = self.hold_position(cx);
            self.log.splice(at..at, 1);
            if reveal {
                self.kinetic.settle_to_end(cx);
            }
        } else {
            self.ease_removal(at, cx);
            self.log.splice(at..at + 1, 0);
        }
        self.typing_row = wanted;
    }

    fn send(&self, command: Option<Command>) {
        if let Some(command) = command {
            self.core.send(command);
        }
    }

    fn on_search_event(
        &mut self,
        search: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.chat.set_search(&search.read(cx).value());
                cx.notify();
            }
            // Opens the highlighted match.
            InputEvent::PressEnter { .. } => {
                if let Some(id) = self.chat.search_selected() {
                    self.open(id, window, cx);
                }
            }
            InputEvent::Focus | InputEvent::Blur => cx.notify(),
        }
    }

    fn on_composer_event(
        &mut self,
        composer: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::Change = event {
            let text = composer.read(cx).value();
            if !text.trim().is_empty() && self.chat.selected().is_some() {
                self.chat.cancel();
            }
            self.typing(&text, cx);
        }
        if let InputEvent::PressEnter { shift: false, .. } = event {
            let Some(command) = self.chat.submit(&composer.read(cx).value()) else {
                return;
            };
            self.typing_idle = None;
            self.core.send(command);
            composer.update(cx, |composer, cx| composer.set_value("", window, cx));
            self.kinetic.stop();
            self.log.set_follow_mode(FollowMode::Tail);
        }
    }

    fn retry(&mut self, id: MessageId) {
        self.send(self.chat.retry(id));
    }

    /// Opens the new chat picker, empty, with contacts refreshed.
    fn start_new_chat(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        self.core.send(self.chat.start_new_chat());
        Self::ready_query(&self.inputs().new_chat, window, cx);
        cx.notify();
    }

    /// Opens the palette, empty.
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chat.open_overlay(Overlay::Palette);
        Self::ready_query(&self.inputs().palette, window, cx);
        cx.notify();
    }

    /// Opens the palette, empty, or closes it.
    fn toggle_palette(&mut self, _: &TogglePalette, window: &mut Window, cx: &mut Context<Self>) {
        self.chat.toggle_palette();
        if self.chat.overlay() == Some(Overlay::Palette) {
            Self::ready_query(&self.inputs().palette, window, cx);
        } else {
            self.restore_focus(window, cx);
        }
        cx.notify();
    }

    /// In compact mode, `Esc` with nothing else to do hides the window.
    fn hide_window(&mut self, _: &HideWindow, _: &mut Window, cx: &mut Context<Self>) {
        if self.layout.shows_chat_list() {
            return cx.propagate();
        }
        cx.defer(main_window::hide);
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.chat.open_overlay(Overlay::Settings);
        self.close_font_picker(false, cx);
        self.settings_panel.reset();
        self.settings_scroll.scroll_to_item(0);
        Self::ready_query(&self.inputs().settings, window, cx);
        cx.notify();
    }

    /// How often the messages were drawn anew rather than reused.
    pub fn log_renders(&self, cx: &App) -> usize {
        self.log_part.read(cx).renders
    }

    pub fn settings_panel(&self) -> &SettingsPanel {
        &self.settings_panel
    }

    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }

    fn on_settings_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.settings_panel.set_query(&input.read(cx).value());
                self.reveal_setting();
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(field) = self.settings_panel.selected() {
                    self.activate_setting(field, window, cx);
                }
            }
            _ => {}
        }
    }

    /// `Enter` or a click on a row's value.
    fn activate_setting(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        match field.activate(&Settings::get(cx)) {
            Activation::Change(setting) => settings::change(setting, cx),
            Activation::PickFont(role) => {
                self.settings_panel.pick_font(role, &Settings::get(cx));
                Self::ready_query(&self.inputs().font, window, cx);
            }
            Activation::Nothing => {}
        }
        cx.notify();
    }

    fn step_setting(&mut self, step: isize, cx: &mut Context<Self>) {
        self.settings_panel.step(step);
        self.reveal_setting();
        cx.notify();
    }

    fn change_setting(&mut self, field: Field, step: i32, cx: &mut Context<Self>) {
        if let Some(setting) = field.step(&Settings::get(cx), step) {
            settings::change(setting, cx);
        }
        cx.notify();
    }

    fn change_selected(&mut self, step: i32, cx: &mut Context<Self>) {
        if let Some(field) = self.settings_panel.selected() {
            self.change_setting(field, step, cx);
        }
    }

    /// Scrolls the highlighted row into view.
    fn reveal_setting(&self) {
        let fields = self.settings_panel.fields();
        if let Some(ix) = self
            .settings_panel
            .selected()
            .and_then(|field| settings_item(&fields, field))
        {
            self.settings_scroll.scroll_to_item(ix);
        }
    }

    fn on_font_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.settings_panel.font_picker_mut() else {
            return;
        };
        match event {
            InputEvent::Change => {
                picker.set_query(&input.read(cx).value());
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.choose_font(None, window, cx),
            _ => {}
        }
    }

    /// Moves the font picker's highlight and previews that font.
    fn step_font(&mut self, step: isize, cx: &mut Context<Self>) {
        let installed = InstalledFonts::get(cx);
        let Some(picker) = self.settings_panel.font_picker_mut() else {
            return;
        };
        let entries = picker.entries(&installed);
        picker.step(step, entries.len());
        if let Some(setting) = picker.setting(&entries, picker.selected()) {
            settings::preview(&setting, cx);
        }
        cx.notify();
    }

    /// Saves font entry `ix`, or the highlighted one, and returns to the
    /// list.
    fn choose_font(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let installed = InstalledFonts::get(cx);
        let Some(picker) = self.settings_panel.font_picker_mut() else {
            return;
        };
        let entries = picker.entries(&installed);
        let setting = picker.setting(&entries, ix.unwrap_or(picker.selected()));
        self.settings_panel.close_font_picker(true);
        if let Some(setting) = setting {
            settings::change(setting, cx);
        }
        Self::focus_input(&self.inputs().settings, window, cx);
        cx.notify();
    }

    /// Closes the font picker, undoing its preview unless `keep`.
    fn close_font_picker(&mut self, keep: bool, cx: &mut Context<Self>) {
        if let Some(setting) = self.settings_panel.close_font_picker(keep) {
            settings::preview(&setting, cx);
        }
    }

    fn cancel_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_font_picker(false, cx);
        Self::focus_input(&self.inputs().settings, window, cx);
        cx.notify();
    }

    fn focus_input(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn ready_query(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
    }

    /// Query changes and Enter in the new chat picker or the palette.
    fn on_picker_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.chat.set_picker_query(&input.read(cx).value());
                cx.notify();
            }
            InputEvent::PressEnter { .. } => match self.chat.overlay() {
                Some(Overlay::NewChat) => self.choose_new_chat(None, window, cx),
                Some(Overlay::Palette) => self.choose_palette(None, window, cx),
                _ => {}
            },
            _ => {}
        }
    }

    fn pick_previous(&mut self, _: &PickPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.chat.picker_step(-1);
        cx.notify();
    }

    fn pick_next(&mut self, _: &PickNext, _: &mut Window, cx: &mut Context<Self>) {
        self.chat.picker_step(1);
        cx.notify();
    }

    /// Opens the chat with candidate `ix`, or the highlighted one; a new
    /// one opens once the core has created it.
    fn choose_new_chat(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.chat.new_chat_open() {
            return;
        }
        let command = self.chat.choose_new_chat(ix);
        self.send(command);
        match self.chat.take_requested_open() {
            Some(id) => self.open(id, window, cx),
            None if !self.chat.new_chat_open() => window.focus(&self.focus, cx),
            None => {}
        }
        cx.notify();
    }

    /// Runs palette entry `ix`, or the highlighted one.
    fn choose_palette(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.chat.choose_palette(ix) else {
            return;
        };
        match action {
            PaletteAction::Open(id) => self.open(id, window, cx),
            PaletteAction::NewChat => self.start_new_chat(&NewChat, window, cx),
            PaletteAction::Settings => self.open_settings(&OpenSettings, window, cx),
            PaletteAction::Theme(mode) => {
                settings::change(Setting::Theme(mode), cx);
                self.restore_focus(window, cx);
            }
            PaletteAction::Quit => window.dispatch_action(Quit.boxed_clone(), cx),
        }
        cx.notify();
    }

    /// Closes any overlay and returns to the composer.
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_font_picker(false, cx);
        self.chat.close_overlay();
        self.restore_focus(window, cx);
        cx.notify();
    }

    /// Focuses the composer, or the view without an open chat.
    fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.open_id().is_some() {
            self.inputs()
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx));
        } else {
            window.focus(&self.focus, cx);
        }
    }

    /// Opens the chat `step` places away in the list.
    fn step_chat(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.overlay().is_some() {
            return;
        }
        if let Some(id) = self.chat.step_chat(step)
            && self.chat.open_id() != Some(id)
        {
            self.open(id, window, cx);
        }
    }

    fn render_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, card) = match self.chat.overlay()? {
            Overlay::NewChat => ("new-chat", self.render_new_chat(cx)),
            Overlay::Palette => ("palette", self.render_palette(window, cx)),
            Overlay::Settings => ("settings", self.render_settings(window, cx)),
        };
        let width = match self.chat.overlay() {
            Some(Overlay::Settings) => px(560.),
            _ => px(420.),
        };
        let palette = cx.palette();
        let card = card
            .id(id)
            // Clicks inside don't reach the backdrop.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w(width)
            .max_w(relative(0.9))
            // Opaque, unlike the window background.
            .bg(cx.theme().popover)
            .border_1()
            .border_color(palette.border)
            .rounded(px(6.))
            .shadow_lg()
            .test_support();
        Some(
            div()
                .id(SharedString::from(format!("{id}-backdrop")))
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(64.))
                .bg(palette.background.opacity(0.6))
                // The window underneath gets no hover, clicks or scrolling.
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _, window, cx| view.close_overlay(window, cx)),
                )
                .child(card)
                .test_support()
                .into_any_element(),
        )
    }

    /// A query box over the rows, or a muted line if there are none.
    fn picker(
        &self,
        input: &Entity<InputState>,
        rows: Vec<AnyElement>,
        empty: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let palette = cx.palette();
        let empty = rows.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(palette.text_muted)
                .child(empty)
        });
        v_flex()
            .key_context(PICKING)
            .on_action(cx.listener(Self::pick_previous))
            .on_action(cx.listener(Self::pick_next))
            .on_action(cx.listener(|view, _: &Escape, window, cx| view.close_overlay(window, cx)))
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(Input::new(input).small()),
            )
            .child(v_flex().p_1().children(rows).children(empty))
    }

    fn render_new_chat(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let selected = self.chat.picker_selected();
        let rows: Vec<_> = self
            .chat
            .new_chat_candidates()
            .into_iter()
            .enumerate()
            .map(|(ix, candidate)| {
                let details = candidate
                    .detail
                    .map(|phone| SharedString::from(phone.to_owned()))
                    .into_iter()
                    .chain(candidate.existing.then(|| "chat".into()));
                picker_row(
                    ("contact", ix as u64),
                    selected == Some(ix),
                    None,
                    StyledText::new(candidate.label)
                        .with_highlights(match_highlights(candidate.highlights, cx)),
                    details,
                    muted,
                    mono.clone(),
                )
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.choose_new_chat(Some(ix), window, cx)
                }))
                .into_any_element()
            })
            .collect();
        let empty = if self.chat.has_contacts() {
            "No matches"
        } else {
            "No contacts yet; they sync from your phone"
        };
        self.picker(&self.inputs().new_chat, rows, empty, cx)
    }

    fn render_palette(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        let palette = cx.palette();
        let muted = palette.text_muted;
        let mono = cx.theme().mono_font_family.clone();
        let selected = self.chat.picker_selected();
        let theme = Settings::get(cx).appearance.theme;
        let rows: Vec<_> = self
            .chat
            .palette_entries()
            .into_iter()
            .enumerate()
            .map(|(ix, entry)| {
                let (glyph, hint) = match entry.action {
                    PaletteAction::Open(_) => ("#", None),
                    PaletteAction::NewChat => (">", key_hint(&NewChat, window)),
                    PaletteAction::Settings => (">", key_hint(&OpenSettings, window)),
                    PaletteAction::Theme(mode) => (">", (mode == theme).then(|| "current".into())),
                    PaletteAction::Quit => (">", key_hint(&Quit, window)),
                };
                picker_row(
                    ("palette-entry", ix as u64),
                    selected == Some(ix),
                    Some(glyph),
                    StyledText::new(entry.label)
                        .with_highlights(match_highlights(entry.highlights, cx)),
                    hint,
                    muted,
                    mono.clone(),
                )
                .on_click(
                    cx.listener(move |view, _, window, cx| {
                        view.choose_palette(Some(ix), window, cx)
                    }),
                )
                .into_any_element()
            })
            .collect();
        self.picker(&self.inputs().palette, rows, "No matches", cx)
    }

    /// Every setting by section, filtered by the query; or the font picker.
    fn render_settings(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::Div {
        if self.settings_panel.font_picker().is_some() {
            return self.render_font_picker(cx);
        }
        let config = Settings::get(cx);
        let mono = cx.theme().mono_font_family.clone();
        let muted = cx.palette().text_muted;
        let border = cx.palette().border;
        let fields = self.settings_panel.fields();
        let selected = self.settings_panel.selected();
        let mut items = Vec::new();
        let mut section = None;
        for field in &fields {
            if section != Some(field.section()) {
                section = Some(field.section());
                items.push(section_header(field.section(), muted, mono.clone()));
            }
            items.push(self.render_setting(*field, &config, selected == Some(*field), cx));
        }
        let empty = fields.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child("No matches")
        });
        let path = Settings::path(cx).map_or_else(
            || "No config directory: not saved".to_owned(),
            |path| path.display().to_string(),
        );
        let max_height = (window.viewport_size().height - px(220.)).max(px(160.));
        v_flex()
            .key_context(SETTINGS)
            .on_action(cx.listener(|view, _: &SettingPrevious, _, cx| view.step_setting(-1, cx)))
            .on_action(cx.listener(|view, _: &SettingNext, _, cx| view.step_setting(1, cx)))
            .on_action(cx.listener(|view, _: &SettingDecrease, _, cx| view.change_selected(-1, cx)))
            .on_action(cx.listener(|view, _: &SettingIncrease, _, cx| view.change_selected(1, cx)))
            .on_action(cx.listener(|view, _: &Escape, window, cx| view.close_overlay(window, cx)))
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(border)
                    .child(Input::new(&self.inputs().settings).small()),
            )
            .child(
                div()
                    .id("settings-list")
                    .max_h(max_height)
                    .overflow_y_scroll()
                    .track_scroll(&self.settings_scroll)
                    .py_1()
                    .children(items)
                    .children(empty)
                    .test_support(),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .border_t_1()
                    .border_color(border)
                    .font_family(mono)
                    .text_xs()
                    .text_color(muted)
                    .child(
                        div()
                            .id("config-path")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(path)
                            .test_support(),
                    )
                    .child("↑↓ ←→ ↵")
                    .child(
                        div()
                            .id("close-settings")
                            .cursor_pointer()
                            .text_color(cx.palette().action)
                            .child("esc close")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.close_overlay(window, cx)),
                            )
                            .test_support(),
                    ),
            )
    }

    /// A row: marker, label and hint, then the value, which clicks change.
    fn render_setting(
        &self,
        field: Field,
        config: &config::Config,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let n = FIELDS.iter().position(|f| *f == field).unwrap_or(0) as u64;
        let palette = cx.palette();
        let (accent, muted) = (palette.accent, palette.text_muted);
        let mono = cx.theme().mono_font_family.clone();
        let read_only = field.read_only();
        let value =
            match field.value(config, self.account.as_ref()) {
                Value::Choice { options, selected } => {
                    let mut parts = Vec::new();
                    for (ix, option) in options.into_iter().enumerate() {
                        if ix > 0 {
                            parts.push(div().child("│").into_any_element());
                        }
                        let current = ix == selected;
                        parts.push(
                            div()
                                .id(("setting-option", n * 16 + ix as u64))
                                .when(current && !read_only, |el| el.text_color(accent))
                                .when(!current && !read_only, |el| {
                                    el.cursor_pointer()
                                        .hover(|style| style.underline())
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            if let Some(setting) = field.choose(ix) {
                                                settings::change(setting, cx);
                                            }
                                        }))
                                })
                                .child(option)
                                .test_support()
                                .into_any_element(),
                        );
                    }
                    h_flex().gap_1().children(parts).into_any_element()
                }
                Value::Number(text) => {
                    let arrow = |id: &'static str, glyph: &'static str, step: i32| {
                        div()
                            .id((id, n))
                            .px_1()
                            .cursor_pointer()
                            .hover(|style| style.text_color(accent))
                            .child(glyph)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.change_setting(field, step, cx)
                            }))
                            .test_support()
                    };
                    h_flex()
                        .child(arrow("setting-less", "‹", -1))
                        .child(div().text_color(palette.text).child(text))
                        .child(arrow("setting-more", "›", 1))
                        .into_any_element()
                }
                Value::Toggle(on) => div()
                    .id(("setting-toggle", n))
                    .cursor_pointer()
                    .when(on, |el| el.text_color(accent))
                    .child(if on { "on" } else { "off" })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.activate_setting(field, window, cx)
                    }))
                    .test_support()
                    .into_any_element(),
                Value::Font(font) => div()
                    .id(("setting-font", n))
                    .cursor_pointer()
                    .max_w(px(200.))
                    .truncate()
                    .when(font.is_some(), |el| el.text_color(palette.text))
                    .child(match (font, selected) {
                        (Some(font), true) => format!("{font} ↵"),
                        (Some(font), false) => font,
                        (None, true) => "default ↵".into(),
                        (None, false) => "default".into(),
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.activate_setting(field, window, cx)
                    }))
                    .test_support()
                    .into_any_element(),
                Value::Info(text) => div()
                    .max_w(px(220.))
                    .truncate()
                    .child(text)
                    .into_any_element(),
            };
        h_flex()
            .id(("setting", n))
            .px_3()
            .py(px(3.))
            .gap_2()
            .items_start()
            .when(selected, |row| row.bg(palette.surface))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.settings_panel.select(field);
                cx.notify();
            }))
            .child(
                div()
                    .flex_none()
                    .w(rems(0.75))
                    .font_family(mono.clone())
                    .text_color(accent)
                    .child(if selected { "▸" } else { "" }),
            )
            .child(
                v_flex().flex_1().min_w_0().child(field.label()).children(
                    field
                        .hint()
                        .map(|hint| div().text_xs().text_color(muted).child(hint)),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(mono)
                    .text_sm()
                    .text_color(muted)
                    .child(value),
            )
            .test_support()
            .into_any_element()
    }

    /// Fonts to pick from, previewed as the highlight moves.
    fn render_font_picker(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let installed = InstalledFonts::get(cx);
        let Some(picker) = self.settings_panel.font_picker() else {
            return div();
        };
        let palette = cx.palette();
        let (muted, border) = (palette.text_muted, palette.border);
        let mono = cx.theme().mono_font_family.clone();
        let rows: Vec<_> = picker
            .entries(&installed)
            .into_iter()
            .enumerate()
            .map(|(ix, entry)| {
                let current = (entry.font == picker.original).then(|| "current".into());
                picker_row(
                    ("font", ix as u64),
                    picker.selected() == ix,
                    None,
                    StyledText::new(entry.label)
                        .with_highlights(match_highlights(entry.highlights, cx)),
                    current,
                    muted,
                    mono.clone(),
                )
                .on_click(
                    cx.listener(move |view, _, window, cx| view.choose_font(Some(ix), window, cx)),
                )
                .into_any_element()
            })
            .collect();
        let empty = rows.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(muted)
                .child("No matches")
        });
        let label = match picker.role {
            super::FontRole::Ui => Field::UiFont,
            super::FontRole::Mono => Field::MonoFont,
            super::FontRole::Body => Field::BodyFont,
        }
        .label();
        v_flex()
            .key_context(PICKING)
            .on_action(cx.listener(|view, _: &PickPrevious, _, cx| view.step_font(-1, cx)))
            .on_action(cx.listener(|view, _: &PickNext, _, cx| view.step_font(1, cx)))
            .on_action(cx.listener(|view, _: &Escape, window, cx| view.cancel_font(window, cx)))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .gap_3()
                    .font_family(mono)
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().child(label))
                    .child("↑↓ preview · ↵ pick · esc back"),
            )
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(border)
                    .child(Input::new(&self.inputs().font).small()),
            )
            .child(v_flex().p_1().children(rows).children(empty))
    }

    /// Focuses search; the palette stands in while the chat list is hidden.
    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        if !self.layout.shows_chat_list() {
            return self.open_palette(window, cx);
        }
        self.inputs()
            .search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    /// Clears the search and returns to the composer.
    fn dismiss_search(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs()
            .search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.chat.set_search("");
        self.restore_focus(window, cx);
        cx.notify();
    }

    fn search_previous(&mut self, _: &SearchPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.chat.search_step(-1);
        cx.notify();
    }

    fn search_next(&mut self, _: &SearchNext, _: &mut Window, cx: &mut Context<Self>) {
        self.chat.search_step(1);
        cx.notify();
    }

    /// Reveals search at the sidebar's top; it hides when the pointer leaves
    /// the sidebar, so rows don't move under the pointer.
    fn on_sidebar_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.position.y - self.sidebar_top.get() < SEARCH_STRIP {
            self.set_search_hovered(true, cx);
        }
    }

    fn set_search_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.search_hovered != hovered {
            self.search_hovered = hovered;
            cx.notify();
        }
    }

    /// Search shows while hovered, focused or in use.
    pub fn search_visible(&self, window: &Window, cx: &App) -> bool {
        let search = self.inputs().search.read(cx);
        self.search_hovered
            || !search.value().is_empty()
            || search.focus_handle(cx).is_focused(window)
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // While searching, the highlight picks; otherwise it marks the open chat.
        let searching = self
            .inputs()
            .search
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let highlighted = if searching {
            self.chat.search_selected()
        } else {
            self.chat.open_id()
        };
        let rows: Vec<_> = self
            .chat
            .conversations()
            .map(|summary| self.render_conversation(summary, highlighted == Some(summary.id), cx))
            .collect();
        let empty = rows.is_empty().then(|| {
            let text = if self.chat.has_conversations() {
                "No matches"
            } else {
                "No chats yet"
            };
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(cx.palette().text_muted)
                .child(text)
        });

        let presence = Presence::new("search-drawer", self.search_visible(window, cx))
            .transition(Transition::new(SEARCH_SLIDE).easing(drawer_easing()))
            .sample(window, cx);
        // A never-shown drawer starts out exiting at 0.
        let rendered = match presence.phase {
            PresencePhase::Absent => false,
            PresencePhase::Exiting => presence.progress > 0.,
            PresencePhase::Entering | PresencePhase::Present => true,
        };
        let search = rendered.then(|| {
            drawer(
                "search-drawer",
                presence.progress,
                div()
                    .id("search")
                    .key_context(SEARCHING)
                    .px_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(cx.palette().border)
                    .on_action(cx.listener(Self::dismiss_search))
                    .on_action(cx.listener(Self::search_previous))
                    .on_action(cx.listener(Self::search_next))
                    .child(Input::new(&self.inputs().search).small().cleanable(true))
                    .test_support(),
            )
        });
        let top = self.sidebar_top.clone();

        v_flex()
            .id("sidebar")
            .relative()
            .w(SIDEBAR_WIDTH)
            .h_full()
            .flex_shrink_0()
            .bg(cx.palette().sidebar)
            .border_r_1()
            .border_color(cx.palette().border)
            .on_mouse_move(cx.listener(Self::on_sidebar_mouse_move))
            .on_hover(cx.listener(|view, hovered: &bool, _, cx| {
                if !hovered {
                    view.set_search_hovered(false, cx);
                }
            }))
            .child(
                canvas(move |bounds, _, _| top.set(bounds.top()), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(div().flex_none().h(SEARCH_STRIP))
            .children(search)
            .child(
                v_flex()
                    .id("conversations")
                    .flex_1()
                    .min_h_0()
                    .p_1()
                    .overflow_y_scrollbar()
                    .children(rows)
                    .children(empty),
            )
            .child(self.render_status(cx))
            .test_support()
            .into_any_element()
    }

    fn render_conversation(
        &self,
        summary: &ConversationSummary,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = summary.id;
        let unread = summary.unread > 0;
        let typing = self.chat.is_typing(id);
        let age = if typing {
            "typing…".into()
        } else {
            summary
                .last_activity
                .map(|at| self.chat.clock().age(at))
                .unwrap_or_default()
        };
        let layout = Settings::get(cx).layout;
        let avatar = layout.avatars.then(|| {
            let palette = cx.palette();
            let color = match summary.peer {
                Peer::Contact(peer) => palette.nick(peer),
                Peer::Group(group) => palette.nick_for(group.as_bytes()),
            };
            avatar(&title(summary), color, px(20.), cx)
        });
        ListItem::new(("conversation", id.get() as u64))
            .selected(selected)
            .py(Spacing::for_density(layout.density).chat)
            .on_click(cx.listener(move |view, _, window, cx| view.open(id, window, cx)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(
                        div()
                            .w(rems(0.75))
                            .text_color(cx.palette().accent)
                            .child(if unread { "●" } else { "" }),
                    )
                    .children(avatar)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(unread, |this| this.font_weight(gpui_kit::FontWeight::BOLD))
                            .child(title(summary)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if typing {
                                cx.palette().accent
                            } else {
                                cx.palette().text_muted
                            })
                            .child(age),
                    ),
            )
            .into_any_element()
    }

    /// The connection's dot and its color.
    fn status_dot(&self, cx: &App) -> (&'static str, Hsla) {
        match self.chat.connection() {
            Connection::Connecting | Connection::Syncing => ("○", cx.palette().text_muted),
            Connection::Online => ("●", cx.palette().success),
            Connection::Offline { .. } => ("●", cx.palette().danger),
        }
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let (dot, color) = self.status_dot(cx);
        let text = self.chat.connection_label(self.chat.clock().now());
        let offline = matches!(self.chat.connection(), Connection::Offline { .. });
        h_flex()
            .id("connection")
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(cx.palette().border)
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .text_color(cx.palette().text_muted)
            .child(div().text_color(color).child(dot))
            .child(text)
            .when(offline, |this| {
                this.cursor_pointer()
                    .on_click(cx.listener(|view, _, _, _| view.core.send(Command::Reconnect)))
            })
            .test_support()
            .into_any_element()
    }

    fn render_conversation_pane(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let compact = !self.layout.shows_chat_list();
        let Some(summary) = self.chat.open_conversation() else {
            let text: SharedString = if !self.chat.has_conversations() {
                "Messages you receive will appear here".into()
            } else if compact {
                match key_hint(&TogglePalette, window) {
                    Some(key) => format!("{key} to pick a chat").into(),
                    None => "Pick a chat from the palette".into(),
                }
            } else {
                "Select a chat".into()
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.palette().text_muted)
                .child(text)
                .into_any_element();
        };
        let header = h_flex()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.palette().border)
            .font_family(cx.theme().mono_font_family.clone())
            .child(div().text_color(cx.palette().text_muted).child("# "))
            .child(
                div()
                    .flex_none()
                    .max_w_1_2()
                    .truncate()
                    .child(title(summary)),
            )
            .children(member_list(summary).map(|members| {
                div()
                    .id("members")
                    .flex_1()
                    .min_w_0()
                    .ml_3()
                    .truncate()
                    .text_sm()
                    .text_color(cx.palette().text_muted)
                    .child(members)
                    .test_support()
            }))
            .when(compact, |header| {
                header
                    .child(div().flex_1())
                    .children(self.render_compact_status(cx))
            });

        if self.log.logical_scroll_top().item_ix < LOAD_OLDER_WITHIN {
            let command = self.chat.load_older();
            self.send(command);
        }

        v_flex()
            .size_full()
            .min_w_0()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.log_part.clone().cached(full_size())),
            )
            .child(
                v_flex()
                    .px_4()
                    .py_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.palette().border)
                    .key_context(self.composer_context())
                    .on_action(cx.listener(Self::edit_last))
                    .on_action(cx.listener(Self::select_last))
                    .on_action(cx.listener(Self::select_previous))
                    .on_action(cx.listener(Self::select_next))
                    .on_action(cx.listener(Self::reply_selected))
                    .on_action(cx.listener(Self::edit_selected))
                    .on_action(cx.listener(Self::delete_selected))
                    .on_action(cx.listener(Self::delete_for_everyone))
                    .on_action(cx.listener(Self::delete_for_me))
                    .on_action(cx.listener(Self::cancel))
                    .children(self.chat.compose_banner().map(|banner| {
                        h_flex()
                            .id("compose")
                            .gap_2()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(PREFIX_SIZE)
                            .text_color(cx.palette().text_muted)
                            .child(div().flex_1().min_w_0().truncate().child(banner))
                            .child(
                                div()
                                    .id("cancel-compose")
                                    .cursor_pointer()
                                    .text_color(cx.palette().action)
                                    .child("esc")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.cancel(&Cancel, window, cx)
                                    }))
                                    .test_support(),
                            )
                            .test_support()
                    }))
                    .child(Textarea::new(&self.inputs().composer)),
            )
            .into_any_element()
    }

    /// The open conversation's messages; drawn as [`Part`].
    fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let messages = list(self.log.clone(), move |ix, window, cx| {
            view.update(cx, |view, cx| view.render_row(ix, window, cx))
        })
        .size_full()
        .py(LOG_PADDING);
        div()
            .id("log")
            .relative()
            .size_full()
            .when_some(cx.try_global::<Fonts>().cloned(), |log, fonts| {
                log.font_family(fonts.body)
                    .line_height(relative(fonts.line_height))
            })
            .child(kinetic(
                "kinetic",
                &self.kinetic,
                &self.log,
                LOG_PADDING * 2.,
                messages,
            ))
            .vertical_scrollbar(&self.log)
            .test_support()
            .into_any_element()
    }

    /// In compact mode, the header stands in for the chat list: unread
    /// chats elsewhere, and the connection unless online.
    fn render_compact_status(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let palette = cx.palette();
        let mut items = Vec::new();
        if !matches!(self.chat.connection(), Connection::Online) {
            let (dot, color) = self.status_dot(cx);
            let offline = matches!(self.chat.connection(), Connection::Offline { .. });
            items.push(
                h_flex()
                    .id("header-connection")
                    .flex_none()
                    .ml_3()
                    .gap_1()
                    .text_xs()
                    .text_color(palette.text_muted)
                    .child(div().text_color(color).child(dot))
                    .child(self.chat.connection_label(self.chat.clock().now()))
                    .when(offline, |this| {
                        this.cursor_pointer().on_click(
                            cx.listener(|view, _, _, _| view.core.send(Command::Reconnect)),
                        )
                    })
                    .test_support()
                    .into_any_element(),
            );
        }
        let unread = self.chat.unread_elsewhere();
        if unread > 0 {
            items.push(
                div()
                    .id("unread-elsewhere")
                    .flex_none()
                    .ml_3()
                    .text_xs()
                    .text_color(palette.accent)
                    .cursor_pointer()
                    .child(format!("● {unread}"))
                    .on_click(cx.listener(|view, _, window, cx| view.open_palette(window, cx)))
                    .test_support()
                    .into_any_element(),
            );
        }
        items
    }

    /// The chat list, sliding out to the left as the window narrows.
    fn render_sidebar_slot(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // The first frame shows the layout as is.
        let slide = if self.drawn {
            SIDEBAR_SLIDE
        } else {
            Duration::ZERO
        };
        self.drawn = true;
        let presence = Presence::new("sidebar-slide", self.layout.shows_chat_list())
            .transition(Transition::new(slide).easing(drawer_easing()))
            .sample(window, cx);
        if !presence.should_render() {
            return None;
        }
        let shown = SIDEBAR_WIDTH * presence.progress;
        Some(
            div()
                .id("sidebar-slot")
                .flex_none()
                .h_full()
                .w(shown)
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(SIDEBAR_WIDTH)
                        .ml(shown - SIDEBAR_WIDTH)
                        .child(self.list_part.clone().cached(full_size())),
                )
                .test_support()
                .into_any_element(),
        )
    }

    fn hover_row(&mut self, id: MessageId, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.hovered = Some(id);
        } else if self.hovered == Some(id) {
            self.hovered = None;
        } else {
            return;
        }
        cx.notify();
    }

    /// Remeasures rows whose actions appeared or went, since they change
    /// how the text wraps.
    fn sync_actions(&mut self) {
        let shown: Vec<_> = [
            self.hovered,
            self.chat.selected(),
            self.chat.confirming_delete(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let changed = shown
            .iter()
            .filter(|id| !self.with_actions.contains(id))
            .chain(self.with_actions.iter().filter(|id| !shown.contains(id)));
        for id in changed {
            if let Some(ix) = self.chat.row_of(*id) {
                self.log.remeasure_items(ix..ix + 1);
            }
        }
        self.with_actions = shown;
    }

    fn composer_context(&self) -> &'static str {
        if self.chat.confirming_delete().is_some() {
            CONFIRMING
        } else if self.chat.selected().is_some() {
            SELECTING
        } else {
            COMPOSING
        }
    }

    fn composer_empty(&self, cx: &App) -> bool {
        self.inputs().composer.read(cx).value().trim().is_empty()
    }

    /// Loads a message's text for editing.
    fn load_edit(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = text else {
            return;
        };
        self.inputs().composer.update(cx, |composer, cx| {
            composer.set_value(text, window, cx);
            composer.focus(window, cx);
        });
        cx.notify();
    }

    fn reveal(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        if let Some(row) = row {
            self.log.scroll_to_reveal_item(row);
        }
        cx.notify();
    }

    fn edit_last(&mut self, _: &EditLast, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer_empty(cx) || self.chat.compose() != Compose::New {
            return cx.propagate();
        }
        match self.chat.edit_last() {
            Some(text) => self.load_edit(Some(text), window, cx),
            None => cx.propagate(),
        }
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        if !self.composer_empty(cx) {
            return cx.propagate();
        }
        let row = self.chat.select_last();
        self.reveal(row, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.chat.select_step(-1) {
            self.reveal(Some(row), cx);
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        match self.chat.select_step(1) {
            Some(row) => self.reveal(Some(row), cx),
            // Past the last message: back to the composer.
            None => {
                self.chat.cancel();
                cx.notify();
            }
        }
    }

    fn reply_selected(&mut self, _: &ReplySelected, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.chat.selected() {
            self.reply_to(id, window, cx);
        }
    }

    fn edit_selected(&mut self, _: &EditSelected, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.chat.selected() {
            self.edit_message(id, window, cx);
        }
    }

    fn delete_selected(&mut self, _: &DeleteSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.chat.selected() {
            self.chat.confirm_delete(id);
            cx.notify();
        }
    }

    fn delete_for_everyone(
        &mut self,
        _: &DeleteForEveryone,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = self.chat.delete(true);
        self.send(command);
        cx.notify();
    }

    fn delete_for_me(&mut self, _: &DeleteForMe, _: &mut Window, cx: &mut Context<Self>) {
        let command = self.chat.delete(false);
        self.send(command);
        cx.notify();
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        let editing = matches!(self.chat.compose(), Compose::Edit(_));
        if !self.chat.cancel() {
            return cx.propagate();
        }
        if editing && self.chat.compose() == Compose::New {
            self.inputs()
                .composer
                .update(cx, |composer, cx| composer.set_value("", window, cx));
        }
        cx.notify();
    }

    fn reply_to(&mut self, id: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.chat.open_id() else {
            return;
        };
        if self.chat.start_reply(open, id) {
            self.inputs()
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx));
            cx.notify();
        }
    }

    fn edit_message(&mut self, id: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.chat.open_id() else {
            return;
        };
        let text = self.chat.start_edit(open, id);
        self.load_edit(text, window, cx);
    }

    fn render_row(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(row) = self.chat.row(ix) {
            let hovered = self.hovered == Some(row.message.id);
            let layout = Settings::get(cx).layout;
            let style = LogStyle {
                message_style: layout.message_style,
                spacing: Spacing::for_density(layout.density),
                timestamps: layout.timestamps,
                time: self.chat.clock().time(row.message.sent_at),
            };
            return render_log_row(row, hovered, &style, window, cx);
        }
        match self.chat.typing_label() {
            Some(label) if self.typing_row => div()
                .id("typing")
                .px_4()
                .pt_1()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(PREFIX_SIZE)
                .line_height(window.line_height())
                .text_color(cx.palette().text_muted)
                .child(label)
                .test_support()
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }
}

/// How message rows look, from the settings.
struct LogStyle {
    message_style: MessageStyle,
    spacing: Spacing,
    timestamps: TimestampVisibility,
    /// The message's own time, for showing on hover.
    time: String,
}

/// A message in the configured style, after its day label; system lines
/// look the same in every style.
fn render_log_row(
    row: Row<'_>,
    hovered: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let palette = cx.palette();
    let (muted, border, surface) = (palette.text_muted, palette.border, palette.surface);
    let mono = cx.theme().mono_font_family.clone();
    let day = row.day.clone().map(|day| {
        h_flex()
            .px_4()
            .pt_3()
            .pb_1()
            .gap_2()
            .font_family(mono.clone())
            .text_xs()
            .text_color(muted)
            .child(div().flex_1().h(px(1.)).bg(border))
            .child(day)
            .child(div().flex_1().h(px(1.)).bg(border))
    });
    if !row.system.is_empty() {
        let time = &row.prefix.time;
        return v_flex()
            .w_full()
            .children(day)
            .child(
                v_flex()
                    .px_4()
                    .pt_1()
                    .font_family(mono)
                    .text_size(PREFIX_SIZE)
                    .line_height(window.line_height())
                    .text_color(muted)
                    .children(
                        row.system
                            .iter()
                            .map(|line| div().child(format!("-- {line} · {time}"))),
                    ),
            )
            .into_any_element();
    }

    let shown = hovered || row.selected || row.confirming_delete;
    let bubbles = style.message_style == MessageStyle::Bubbles;
    let message = match style.message_style {
        MessageStyle::Log => log_row(&row, shown, style, window, cx),
        MessageStyle::Blocks => block_row(&row, shown, style, window, cx),
        MessageStyle::Bubbles => bubble_row(&row, shown, style, window, cx),
    };
    let id = row.message.id;
    v_flex()
        .w_full()
        .children(day)
        .child(
            message
                .id(("row", id.get() as u64))
                .on_hover(
                    cx.listener(move |view, hovered: &bool, _, cx| {
                        view.hover_row(id, *hovered, cx)
                    }),
                )
                .w_full()
                .px_4()
                // Bubbles mark the selected bubble instead.
                .when(shown && !bubbles, |this| this.bg(surface))
                .test_support(),
        )
        .into_any_element()
}

/// Log style: `sender @ time > text`; follow-ups in a group indent to the
/// first message's text.
fn log_row(
    row: &Row<'_>,
    shown: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let palette = cx.palette();
    let (muted, text) = (palette.text_muted, palette.text);
    let mono = cx.theme().mono_font_family.clone();
    let sender = truncate(&row.prefix.sender, MAX_SENDER_CHARS);
    let prefix: SharedString = match style.timestamps {
        TimestampVisibility::Always => format!("{sender} @ {} >", row.prefix.time),
        TimestampVisibility::Hover | TimestampVisibility::Never => format!("{sender} >"),
    }
    .into();
    let font_size = PREFIX_SIZE.to_pixels(window.rem_size());
    let run = TextRun {
        len: prefix.len(),
        font: font(mono.clone()),
        color: muted,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let width = window
        .text_system()
        .shape_line(prefix.clone(), font_size, &[run], None)
        .width;
    let color = sender_color(row, cx);
    let prefix = row.leads.then(|| {
        let sender_end = sender.len();
        div()
            .flex_none()
            .w(width)
            // The measured width may round below the drawn one.
            .whitespace_nowrap()
            .font_family(mono.clone())
            .text_size(PREFIX_SIZE)
            .line_height(window.line_height())
            .text_color(muted)
            .child(StyledText::new(prefix).with_highlights([(
                0..sender_end,
                HighlightStyle {
                    color: Some(color),
                    ..Default::default()
                },
            )]))
    });
    let indent = (!row.leads).then(|| div().flex_none().w(width));
    let hover_time = style.timestamps == TimestampVisibility::Hover && shown;
    h_flex()
        .items_start()
        .gap_2()
        .pt(group_gap(row, style.spacing, 1.))
        .children(prefix)
        .children(indent)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .text_color(text)
                .children(message_text(row, window, cx))
                .children(shown.then(|| row_actions(row, muted, mono.clone(), cx))),
        )
        .children(message_meta(row, hover_time, &style.time, cx))
        .children(message_failures(row, cx))
}

/// Blocks style: the sender and time head each group, messages below.
fn block_row(
    row: &Row<'_>,
    shown: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let palette = cx.palette();
    let (muted, text) = (palette.text_muted, palette.text);
    let mono = cx.theme().mono_font_family.clone();
    let color = sender_color(row, cx);
    let header = row.leads.then(|| {
        h_flex()
            .gap_2()
            .items_baseline()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(truncate(&row.prefix.sender, MAX_SENDER_CHARS).into_owned()),
            )
            .when(style.timestamps == TimestampVisibility::Always, |header| {
                header.child(
                    div()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(muted)
                        .child(row.prefix.time.clone()),
                )
            })
    });
    let hover_time = style.timestamps == TimestampVisibility::Hover && shown;
    h_flex()
        .items_start()
        .gap_2()
        .pt(group_gap(row, style.spacing, 2.))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .text_color(text)
                .children(header)
                .children(message_text(row, window, cx))
                .children(shown.then(|| row_actions(row, muted, mono.clone(), cx))),
        )
        .children(message_meta(row, hover_time, &style.time, cx))
        .children(message_failures(row, cx))
}

/// Bubbles style: incoming on the left, ours on the right, with the time
/// and status inside; the sender's name heads each group.
fn bubble_row(
    row: &Row<'_>,
    shown: bool,
    style: &LogStyle,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let palette = cx.palette();
    let (muted, text, accent) = (palette.text_muted, palette.text, palette.accent);
    let surface = palette.surface;
    let mono = cx.theme().mono_font_family.clone();
    let outgoing = row.message.direction == Direction::Outgoing;
    let color = sender_color(row, cx);
    let name = row.leads.then(|| {
        div()
            .id(("bubble-name", row.message.id.get() as u64))
            .px_3()
            .text_xs()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(color)
            .child(truncate(&row.prefix.sender, MAX_SENDER_CHARS).into_owned())
            .test_support()
    });
    let time = match style.timestamps {
        TimestampVisibility::Always => true,
        TimestampVisibility::Hover => shown,
        TimestampVisibility::Never => false,
    };
    let meta = message_meta(row, time, &style.time, cx);
    let footer = (!meta.is_empty()).then(|| h_flex().justify_end().gap_1().children(meta));
    let marked = row.selected || row.confirming_delete;
    let bubble = v_flex()
        .id(("bubble", row.message.id.get() as u64))
        .min_w_0()
        .px_3()
        .py(px(6.))
        .rounded(px(12.))
        .bg(if outgoing {
            accent.opacity(0.2)
        } else {
            surface
        })
        .border_1()
        .border_color(if marked {
            accent
        } else {
            gpui_kit::transparent_black()
        })
        .text_color(text)
        .children(message_text(row, window, cx))
        .children(footer)
        .test_support();
    h_flex()
        .items_start()
        .pt(group_gap(row, style.spacing, 2.))
        .when(outgoing, |row| row.justify_end())
        .child(
            v_flex()
                .max_w(relative(0.75))
                .min_w_0()
                .gap_1()
                .when(outgoing, |column| column.items_end())
                .when(!outgoing, |column| column.items_start())
                .children(name)
                .child(bubble)
                .children(message_failures(row, cx))
                .children(shown.then(|| row_actions(row, muted, mono.clone(), cx))),
        )
}

/// Space above a message: wider before a new group, by `scale`.
fn group_gap(row: &Row<'_>, spacing: Spacing, scale: f32) -> Pixels {
    if row.leads {
        spacing.group * scale
    } else {
        spacing.message
    }
}

fn sender_color(row: &Row<'_>, cx: &App) -> Hsla {
    let palette = cx.palette();
    row.prefix
        .peer
        .map_or(palette.text_secondary, |peer| palette.nick(peer))
}

/// The quote and placeholder lines, then the text.
fn message_text(row: &Row<'_>, window: &Window, cx: &App) -> Vec<AnyElement> {
    let mono = cx.theme().mono_font_family.clone();
    let muted = cx.palette().text_muted;
    let line_height = window.line_height();
    let id = row.message.id;
    row.quote
        .iter()
        .chain(&row.placeholders)
        .map(|line| {
            div()
                .font_family(mono.clone())
                .text_size(PREFIX_SIZE)
                .line_height(line_height)
                .text_color(muted)
                .truncate()
                .child(line.clone())
                .into_any_element()
        })
        .chain(row.message.content.body().map(|body| {
            TextView::markdown(("body", id.get() as u64), message_markdown(body.as_str()))
                .selectable(true)
                .into_any_element()
        }))
        .collect()
}

/// Small muted glyphs: the time if `time`, edited, timer and status.
fn message_meta(row: &Row<'_>, time: bool, at: &str, cx: &App) -> Vec<AnyElement> {
    let palette = cx.palette();
    let muted = palette.text_muted;
    let mono = cx.theme().mono_font_family.clone();
    let status = match row.message.status {
        Some(MessageStatus::Pending) => Some(("…", muted)),
        Some(MessageStatus::Sent) => Some(("✓", muted)),
        Some(MessageStatus::Delivered) => Some(("✓✓", muted)),
        Some(MessageStatus::Read) => Some(("✓✓", palette.accent)),
        Some(MessageStatus::Failed) | None => None,
    };
    let glyph = |text: SharedString, color: Hsla| {
        div()
            .flex_none()
            .font_family(mono.clone())
            .text_xs()
            .text_color(color)
            .child(text)
            .into_any_element()
    };
    [
        time.then(|| glyph(at.to_owned().into(), muted)),
        row.message.edited.then(|| glyph("edited".into(), muted)),
        row.timer.clone().map(|timer| glyph(timer.into(), muted)),
        status.map(|(status, color)| glyph(status.into(), color)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Sending it, or an edit or delete of it, failed.
fn message_failures(row: &Row<'_>, cx: &mut Context<ChatView>) -> Vec<AnyElement> {
    let palette = cx.palette();
    let (danger, action) = (palette.danger, palette.action);
    let mono = cx.theme().mono_font_family.clone();
    let id = row.message.id;
    let error: Option<SharedString> = row.error.map(|error| error.to_string().into());
    let failed = row.message.status == Some(MessageStatus::Failed);
    let failure = failed.then(|| {
        h_flex()
            .id(("retry", id.get() as u64))
            .flex_none()
            .gap_1()
            .font_family(mono.clone())
            .text_xs()
            .text_color(danger)
            .cursor_pointer()
            .child("not sent ·")
            .child(div().text_color(action).child("retry"))
            .when_some(error, |this, error| {
                this.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
            })
            .on_click(cx.listener(move |view, _, _, _| view.retry(id)))
            .test_support()
            .into_any_element()
    });
    let change_failed = row.change_failed.then(|| {
        div()
            .flex_none()
            .font_family(mono)
            .text_xs()
            .text_color(danger)
            .child("change not sent")
            .into_any_element()
    });
    failure.into_iter().chain(change_failed).collect()
}

/// Reply, edit and delete, or how to delete, with key hints when selected:
/// a line under the text of hovered and selected rows only.
fn row_actions(
    row: &Row<'_>,
    muted: Hsla,
    mono: SharedString,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let id = row.message.id;
    let n = id.get() as u64;
    let color = cx.palette().action;
    let action = |name: &'static str, label: &'static str| {
        div()
            .id((name, n))
            .text_color(color)
            .cursor_pointer()
            .hover(|style| style.underline())
            .child(label)
            .test_support()
    };
    let keys = row.selected;
    let mut items: Vec<AnyElement> = Vec::new();
    if row.confirming_delete {
        if row.actions.delete_for_everyone {
            items.push(
                action(
                    "for-everyone",
                    if keys {
                        "e for everyone"
                    } else {
                        "for everyone"
                    },
                )
                .on_click(cx.listener(|view, _, window, cx| {
                    view.delete_for_everyone(&DeleteForEveryone, window, cx)
                }))
                .into_any_element(),
            );
        }
        items.push(
            action("for-me", if keys { "m for me" } else { "for me" })
                .on_click(
                    cx.listener(|view, _, window, cx| view.delete_for_me(&DeleteForMe, window, cx)),
                )
                .into_any_element(),
        );
        items.push(
            action("cancel-delete", if keys { "esc cancel" } else { "cancel" })
                .on_click(cx.listener(|view, _, window, cx| view.cancel(&Cancel, window, cx)))
                .into_any_element(),
        );
    } else {
        if row.actions.reply {
            items.push(
                action("reply", if keys { "r reply" } else { "reply" })
                    .on_click(cx.listener(move |view, _, window, cx| view.reply_to(id, window, cx)))
                    .into_any_element(),
            );
        }
        if row.actions.edit {
            items.push(
                action("edit", if keys { "e edit" } else { "edit" })
                    .on_click(
                        cx.listener(move |view, _, window, cx| view.edit_message(id, window, cx)),
                    )
                    .into_any_element(),
            );
        }
        items.push(
            action("delete", if keys { "del delete" } else { "delete" })
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.chat.confirm_delete(id);
                    cx.notify();
                }))
                .into_any_element(),
        );
    }
    let mut separated = Vec::new();
    for (ix, item) in items.into_iter().enumerate() {
        if ix > 0 {
            separated.push(div().child("·").into_any_element());
        }
        separated.push(item);
    }
    h_flex()
        .id(("actions", n))
        .flex_none()
        .gap_1()
        .font_family(mono)
        .text_xs()
        .text_color(muted)
        .children(separated)
        .test_support()
        .into_any_element()
}

/// A picker entry: an optional glyph, the label, then muted details.
fn picker_row(
    id: impl Into<gpui_kit::ElementId>,
    selected: bool,
    glyph: Option<&'static str>,
    label: StyledText,
    details: impl IntoIterator<Item = SharedString>,
    muted: Hsla,
    mono: SharedString,
) -> ListItem {
    let detail = |text: SharedString| {
        div()
            .flex_none()
            .font_family(mono.clone())
            .text_xs()
            .text_color(muted)
            .child(text)
    };
    ListItem::new(id).selected(selected).child(
        h_flex()
            .w_full()
            .gap_2()
            .children(glyph.map(|glyph| {
                div()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_color(muted)
                    .child(glyph)
            }))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .children(details.into_iter().map(detail)),
    )
}

/// Matched characters in the accent color.
fn match_highlights(ranges: Vec<Range<usize>>, cx: &App) -> Vec<(Range<usize>, HighlightStyle)> {
    let highlight = HighlightStyle {
        color: Some(cx.palette().accent),
        ..Default::default()
    };
    ranges.into_iter().map(|range| (range, highlight)).collect()
}

/// The key bound to `action` in the chat view, e.g. `Ctrl+N`.
fn key_hint(action: &dyn Action, window: &Window) -> Option<SharedString> {
    let context = KeyContext::parse(CONTEXT).ok()?;
    let binding = window.highest_precedence_binding_for_action_in_context(action, context)?;
    let key = binding.keystrokes().first()?;
    Some(Kbd::format(key.as_keystroke()).into())
}

/// A round badge with a name's initials.
fn avatar(name: &str, color: Hsla, size: Pixels, cx: &App) -> gpui_kit::Div {
    div()
        .flex_none()
        .size(size)
        .rounded_full()
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .text_size(size * 0.45)
        .font_weight(gpui_kit::FontWeight::BOLD)
        .text_color(cx.palette().accent_text)
        .child(initials(name))
}

/// The first letter or digit of up to two words, upper case.
fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// A muted section title in the settings list.
fn section_header(section: Section, muted: Hsla, mono: SharedString) -> AnyElement {
    div()
        .px_3()
        .pt_2()
        .pb_1()
        .font_family(mono)
        .text_xs()
        .text_color(muted)
        .child(section.label().to_uppercase())
        .into_any_element()
}

/// Where `field` is among the settings list's children, counting section
/// headers.
fn settings_item(fields: &[Field], field: Field) -> Option<usize> {
    let mut ix = 0;
    let mut section = None;
    for listed in fields {
        if section != Some(listed.section()) {
            section = Some(listed.section());
            ix += 1;
        }
        if *listed == field {
            return Some(ix);
        }
        ix += 1;
    }
    None
}

fn layout_for(window: &Window) -> Layout {
    Layout::for_width(f32::from(window.viewport_size().width))
}

/// Fast start, long gentle settle, like iOS sheets.
fn drawer_easing() -> Easing {
    Easing::cubic_bezier(0.32, 0.72, 0., 1.).expect("valid curve")
}

/// `text`, cut to `max` characters with an ellipsis.
fn truncate(text: &str, max: usize) -> Cow<'_, str> {
    match text.char_indices().nth(max) {
        Some(_) => {
            let end = text.char_indices().nth(max - 1).map_or(0, |(ix, _)| ix);
            format!("{}…", &text[..end]).into()
        }
        None => text.into(),
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach(window, cx);
        if std::mem::take(&mut self.ready_pending) {
            self.focus_pending = false;
            self.ready_composer(window, cx);
        } else if std::mem::take(&mut self.focus_pending) {
            self.restore_focus(window, cx);
        }
        self.sync_actions();
        let overlay = self.render_overlay(window, cx);
        let sidebar = self.render_sidebar_slot(window, cx);
        h_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::start_new_chat))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::hide_window))
            .on_action(
                cx.listener(|view, _: &PreviousChat, window, cx| view.step_chat(-1, window, cx)),
            )
            .on_action(cx.listener(|view, _: &NextChat, window, cx| view.step_chat(1, window, cx)))
            .relative()
            .size_full()
            .items_stretch()
            .bg(cx.palette().background)
            .text_color(cx.palette().text)
            .children(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_conversation_pane(window, cx)),
            )
            .children(overlay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_the_first_letter_of_two_words() {
        assert_eq!(initials("alice"), "A");
        assert_eq!(initials("Bob van Dijk"), "BV");
        assert_eq!(initials("+1 555 0100"), "15");
        assert_eq!(initials("(Émile) Zola"), "ÉZ");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn long_names_are_truncated() {
        assert_eq!(truncate("Alice", 5), "Alice");
        assert_eq!(truncate("Alexandra", 5), "Alex…");
        assert_eq!(truncate("Ångström", 4), "Ång…");
    }
}
