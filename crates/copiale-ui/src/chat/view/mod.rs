//! Main window once linked: chat list on the left, the open conversation on
//! the right.

mod emoji;
mod log;
mod overlays;
mod requests;
mod settings;
mod sidebar;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use copiale_core::config;
use copiale_core::{Account, Command, ConversationId, Event, MessageId, Timestamp};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::{
    Action, AnyElement, App, AppContext as _, AsKeystroke as _, Context, Div, Entity, FocusHandle,
    Focusable as _, FollowMode, InteractiveElement as _, IntoElement, KeyBinding, KeyContext,
    ListAlignment, ListState, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    Stateful, StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Task,
    WeakEntity, Window, WindowId, actions, div, point, px,
};

use super::emoji::EmojiTyping;
use super::layout::Layout;
use super::model::{Chat, Compose, Effects, LogChange, Overlay, title};
use super::notices::Notices;
use super::settings_panel::SettingsPanel;
use super::text::isolate;
use super::time::LocalClock;
use crate::icons::IconName;
use crate::kinetic::{self, Kinetic};
use crate::notifier::Notifier;
use crate::settings::Settings;
use crate::theme::ActivePalette as _;
use crate::{CoreSink, main_window, tray};
use emoji::GridState;

/// How long a pause in typing counts as having stopped.
const TYPING_IDLE: Duration = Duration::from_secs(5);

/// Above the first and below the last message.
const LOG_PADDING: Pixels = px(8.);

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
        ConfirmSafetyNumber,
        OpenLink,
        AcceptRequest,
        DeleteRequest,
        BlockChat,
        UnblockChat,
        PickEmoji,
        EmojiPrevious,
        EmojiNext,
        CloseEmojiPicker,
        ToggleEmojiGrid,
        CloseEmojiGrid,
        GridLeft,
        GridRight,
        GridUp,
        GridDown,
    ]
);

/// Key context around the composer; which one depends on whether a message
/// is selected or being deleted.
const COMPOSING: &str = "Composing";
const SELECTING: &str = "Selecting";
const CONFIRMING: &str = "Confirming";
/// Key context around the composer while emoji are suggested.
const EMOJI_PICKING: &str = "EmojiPicking";
/// Key context of the emoji picker.
const EMOJI_GRID: &str = "EmojiGrid";
/// Key context of the new chat picker and the palette.
const PICKING: &str = "Picking";
/// Key context of the search box.
const SEARCHING: &str = "Searching";
/// Key context of the settings list.
const SETTINGS: &str = "Settings";
/// Key context of the unlink confirmation.
const CONFIRM_UNLINK: &str = "ConfirmUnlink";
/// Key context of the safety number card.
const SAFETY_NUMBER: &str = "SafetyNumber";
/// Key context of the question before opening a link.
const LINK: &str = "Link";
/// Key context of the banner standing in for the composer in a message
/// request or blocked chat.
const REQUEST: &str = "Request";

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
        KeyBinding::new("secondary-e", ToggleEmojiGrid, Some(CONTEXT)),
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
        KeyBinding::new("escape", Cancel, Some(CONFIRM_UNLINK)),
        KeyBinding::new("escape", Cancel, Some(SAFETY_NUMBER)),
        KeyBinding::new("enter", ConfirmSafetyNumber, Some(SAFETY_NUMBER)),
        KeyBinding::new("escape", Cancel, Some(LINK)),
        KeyBinding::new("enter", OpenLink, Some(LINK)),
        KeyBinding::new("a", AcceptRequest, Some(REQUEST)),
        KeyBinding::new("d", DeleteRequest, Some(REQUEST)),
        KeyBinding::new("b", BlockChat, Some(REQUEST)),
        KeyBinding::new("u", UnblockChat, Some(REQUEST)),
        KeyBinding::new("e", DeleteForEveryone, confirming),
        KeyBinding::new("m", DeleteForMe, confirming),
        KeyBinding::new("escape", Cancel, confirming),
        KeyBinding::new("enter", PickEmoji, Some("EmojiPicking > Input")),
        KeyBinding::new("tab", PickEmoji, Some("EmojiPicking > Input")),
        KeyBinding::new("up", EmojiPrevious, Some("EmojiPicking > Input")),
        KeyBinding::new("down", EmojiNext, Some("EmojiPicking > Input")),
        KeyBinding::new("escape", CloseEmojiPicker, Some("EmojiPicking > Input")),
        KeyBinding::new("left", GridLeft, Some("EmojiGrid > Input")),
        KeyBinding::new("right", GridRight, Some("EmojiGrid > Input")),
        KeyBinding::new("up", GridUp, Some("EmojiGrid > Input")),
        KeyBinding::new("down", GridDown, Some("EmojiGrid > Input")),
        KeyBinding::new("escape", CloseEmojiGrid, Some("EmojiGrid > Input")),
    ]);
}

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
    /// Emoji conversions and suggestions in the composer.
    emoji: EmojiTyping,
    /// The emoji picker, while open.
    emoji_grid: Option<GridState>,
    /// Emoji picked lately, newest first.
    recent_emoji: Vec<String>,
    /// Clear and focus the composer on the next frame.
    ready_pending: bool,
    /// Focus the composer on the next frame.
    focus_pending: bool,
    /// The pointer reached the sidebar's top and hasn't left the sidebar.
    search_hovered: bool,
    sidebar_top: Rc<Cell<Pixels>>,
    settings_panel: SettingsPanel,
    settings_scroll: ScrollHandle,
    /// Held while asking whether to unlink.
    confirm_focus: FocusHandle,
    /// Held while a safety number shows.
    identity_focus: FocusHandle,
    /// Held while asking whether to open a link.
    link_focus: FocusHandle,
    /// Held in a message request or blocked chat, which has no composer.
    request_focus: FocusHandle,
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
    /// The emoji picker's query.
    emoji_search: Entity<InputState>,
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
                .set_twelve_hour(settings.layout.clock == config::TimeFormat::H12);
            // Fonts, spacing and timestamps change row heights.
            view.log.remeasure();
            cx.notify();
        })];
        core.send(Command::LoadConversations);
        core.send(Command::LoadRecentEmoji);
        let focus = cx.focus_handle();
        let mut chat = Chat::new(LocalClock::system());
        chat.set_twelve_hour(Settings::get(cx).layout.clock == config::TimeFormat::H12);
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
        let minutes = Self::redraw_after(
            |_| {
                Some(Duration::from_millis(
                    60_000 - Timestamp::now().as_millis() % 60_000,
                ))
            },
            |_, cx| cx.notify(),
            cx,
        );
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
            emoji: EmojiTyping::default(),
            emoji_grid: None,
            recent_emoji: Vec::new(),
            ready_pending: false,
            focus_pending: true,
            search_hovered: false,
            sidebar_top: Rc::default(),
            settings_panel: SettingsPanel::default(),
            settings_scroll: ScrollHandle::new(),
            confirm_focus: cx.focus_handle(),
            identity_focus: cx.focus_handle(),
            link_focus: cx.focus_handle(),
            request_focus: cx.focus_handle(),
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
                format!("Message {}", isolate(&title(summary)))
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
        let emoji_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji…"));
        let subscriptions = vec![
            cx.subscribe_in(&emoji_search, window, Self::on_emoji_search_event),
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
        self.emoji.reset();
        self.emoji_grid = None;
        self.inputs = Some(Inputs {
            window: id,
            search,
            composer,
            new_chat,
            palette,
            settings,
            font,
            emoji_search,
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
        self.emoji_grid = None;
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
        match event {
            Event::Account(account) => self.account = Some(account.clone()),
            Event::RecentEmoji(recent) => self.recent_emoji.clone_from(&recent.0),
            _ => {}
        }
        let shown = self.open_state();
        let effects = self.chat.apply(event, self.active);
        self.run(effects, cx);
        // Accepting brings the composer back; deleting closes the chat.
        if self.open_state() != shown && self.chat.overlay().is_none() {
            self.focus_pending = true;
        }
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

    /// The open chat and whether it has a composer.
    fn open_state(&self) -> Option<(ConversationId, bool)> {
        let id = self.chat.open_id()?;
        Some((id, self.chat.request_banner().is_none()))
    }

    /// Redraws as countdowns change, while any run in the open conversation.
    fn tick_timers(&mut self, cx: &mut Context<Self>) {
        self.timers = Some(Self::redraw_after(
            |view| view.chat.timers_change_in(),
            |_, cx| cx.notify(),
            cx,
        ));
    }

    /// Redraws as typing indicators expire.
    fn expire_typing(&mut self, cx: &mut Context<Self>) {
        self.typing_expiry = Some(Self::redraw_after(
            |view| view.chat.typing_changes_in(),
            |view, cx| {
                view.sync_typing_row(cx);
                cx.notify();
            },
            cx,
        ));
    }

    /// Waits each delay `next` gives, then runs `redraw`; stops when it
    /// gives `None` or the view is gone.
    fn redraw_after(
        next: impl Fn(&Self) -> Option<Duration> + 'static,
        redraw: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |view, cx| {
            loop {
                let Ok(Some(delay)) = view.read_with(cx, |view, _| next(view)) else {
                    break;
                };
                // A little late, so labels have changed by then.
                cx.background_executor()
                    .timer(delay + Duration::from_millis(10))
                    .await;
                if view.update(cx, |view, cx| redraw(view, cx)).is_err() {
                    break;
                }
            }
        })
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
        self.countdown = Some(Self::redraw_after(
            |view| {
                let chat = &view.chat;
                chat.connection_label_changes_in(chat.clock().now())
            },
            |_, cx| cx.notify(),
            cx,
        ));
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
        self.emoji_grid = None;
        let placeholder = self
            .chat
            .open_conversation()
            .map(|summary| format!("Message {}", isolate(&title(summary))));
        self.inputs().composer.update(cx, |composer, cx| {
            composer.set_value("", window, cx);
            if let Some(placeholder) = placeholder {
                composer.set_placeholder(placeholder, window, cx);
            }
            composer.focus(window, cx);
        });
        if self.chat.request_banner().is_some() {
            window.focus(&self.request_focus, cx);
        }
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
            self.composer_changed(composer, window, cx);
        }
        if let InputEvent::PressEnter { shift: false, .. } = event {
            let emoticons = Settings::get(cx).composer.convert_emoticons;
            let text = super::emoji::on_send(&composer.read(cx).value(), emoticons);
            let Some(command) = self.chat.submit(&text) else {
                return;
            };
            self.emoji.reset();
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

    fn focus_input(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn ready_query(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
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
        if self.chat.request_banner().is_some() {
            window.focus(&self.request_focus, cx);
        } else if self.chat.open_id().is_some() {
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

    /// Closes every notification shown, as the account's data goes.
    pub fn close_notices(&mut self, cx: &mut Context<Self>) {
        for action in self.notices.close_all() {
            self.notifier.apply(action);
        }
        tray::update(cx, |tray| tray.unread = false);
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
}

/// The key bound to `action` in the chat view, e.g. `Ctrl+N`.
fn key_hint(action: &dyn Action, window: &Window) -> Option<SharedString> {
    let context = KeyContext::parse(CONTEXT).ok()?;
    let binding = window.highest_precedence_binding_for_action_in_context(action, context)?;
    let key = binding.keystrokes().first()?;
    Some(Kbd::format(key.as_keystroke()).into())
}

/// An icon running `action`, its name and key in the tooltip; `active`
/// while what it opens is shown.
fn icon_button(
    id: &'static str,
    icon: IconName,
    name: &'static str,
    action: &dyn Action,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let palette = cx.palette();
    let hover = palette.action;
    let action = action.boxed_clone();
    div()
        .id(id)
        .flex_none()
        .p_1()
        .cursor_pointer()
        .text_color(if active { hover } else { palette.text_muted })
        .hover(move |button| button.text_color(hover))
        .child(Icon::new(icon).small())
        .tooltip(move |window, cx| {
            Tooltip::new(name)
                .action(action.as_ref(), Some(CONTEXT))
                .build(window, cx)
        })
}

fn layout_for(window: &Window) -> Layout {
    Layout::for_width(f32::from(window.viewport_size().width))
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
            .on_action(cx.listener(Self::toggle_emoji_grid))
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
