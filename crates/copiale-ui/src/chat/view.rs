//! Main window once linked: chat list on the left, the open conversation on
//! the right.

use std::borrow::Cow;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use copiale_core::{Command, ConversationId, ConversationSummary, Event, MessageId, MessageStatus};
use gpui_kit::base::{Easing, Presence, PresencePhase, Transition};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, FollowMode,
    HighlightStyle, Hsla, InteractiveElement as _, IntoElement, KeyBinding, ListAlignment,
    ListState, MouseMoveEvent, ParentElement as _, Pixels, Rems, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task,
    TestSupportExt as _, TextRun, Window, actions, canvas, div, font, list, point, px, rems,
};

use super::model::{Chat, Compose, Connection, Effects, LogChange, Row, member_list, title};
use super::text::message_markdown;
use super::time::Clock;
use crate::CoreSink;
use crate::drawer::drawer;
use crate::kinetic::{self, Kinetic, kinetic};
use crate::theme::ActivePalette as _;

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
    ]
);

/// Key context around the composer; which one depends on whether a message
/// is selected or being deleted.
const COMPOSING: &str = "Composing";
const SELECTING: &str = "Selecting";
const CONFIRMING: &str = "Confirming";

/// Registers the chat key bindings. Composer bindings act on the input
/// itself, ahead of its own, and pass keys on when they don't apply.
pub fn init(cx: &mut App) {
    let composing = Some("Composing > Input");
    let selecting = Some("Selecting > Input");
    let confirming = Some("Confirming > Input");
    cx.bind_keys([
        KeyBinding::new("secondary-f", FocusSearch, Some(CONTEXT)),
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
    log: ListState,
    kinetic: Kinetic,
    /// Holds focus while no input does, so shortcuts work.
    focus: FocusHandle,
    search: Entity<InputState>,
    /// The pointer reached the sidebar's top and hasn't left the sidebar.
    search_hovered: bool,
    sidebar_top: Rc<Cell<Pixels>>,
    composer: Entity<TextareaState>,
    /// Redraws the reconnect countdown.
    countdown: Option<Task<()>>,
    /// Redraws when a typing indicator expires.
    typing_expiry: Option<Task<()>>,
    /// The log ends with a typing line.
    typing_row: bool,
    /// The row under the pointer.
    hovered: Option<MessageId>,
    /// Rows showing their actions, last frame.
    with_actions: Vec<MessageId>,
    /// Tells the core we stopped typing after a pause.
    typing_idle: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    /// Asks the core for the chat list.
    pub fn new(core: Rc<dyn CoreSink>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder("Message")
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, Self::on_search_event),
            cx.subscribe_in(&composer, window, Self::on_composer_event),
            cx.observe_window_activation(window, |view, window, _| {
                if window.is_window_active()
                    && let Some(command) = view.chat.activated()
                {
                    view.core.send(command);
                }
            }),
        ];
        core.send(Command::LoadConversations);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            core,
            chat: Chat::new(Clock::system()),
            log: ListState::new(0, ListAlignment::Top, px(1000.)),
            kinetic: Kinetic::default(),
            focus,
            search,
            search_hovered: false,
            sidebar_top: Rc::default(),
            composer,
            countdown: None,
            typing_expiry: None,
            typing_row: false,
            hovered: None,
            with_actions: Vec::new(),
            typing_idle: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn chat(&self) -> &Chat {
        &self.chat
    }

    pub fn search(&self) -> &Entity<InputState> {
        &self.search
    }

    pub fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    pub fn log(&self) -> &ListState {
        &self.log
    }

    pub fn on_event(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) {
        let effects = self.chat.apply(event, window.is_window_active());
        self.run(effects, cx);
        if let Event::Conversations(_) = event
            && let Some(effects) = self.chat.open_latest(window.is_window_active())
        {
            self.run(effects, cx);
            self.ready_composer(window, cx);
        }
        match event {
            Event::Disconnected { .. } => self.start_countdown(cx),
            Event::Typing { .. } | Event::MessageAdded(_) => self.expire_typing(cx),
            _ => {}
        }
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
        if self.chat.open_id() != Some(id) && self.typing_idle.take().is_some() {
            self.send(self.chat.typing_command(""));
        }
        let effects = self.chat.open(id);
        self.run(effects, cx);
        self.ready_composer(window, cx);
    }

    /// Clears the composer for the open conversation and focuses it.
    fn ready_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self
            .chat
            .open_conversation()
            .map(|summary| format!("Message {}", title(summary)));
        self.composer.update(cx, |composer, cx| {
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
            // Opens the best match.
            InputEvent::PressEnter { .. } => {
                let best = self.chat.conversations().next().map(|c| c.id);
                if let Some(id) = best {
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

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    /// Clears the search and returns to the composer.
    fn dismiss_search(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.chat.set_search("");
        if self.chat.open_id().is_some() {
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
        } else {
            window.focus(&self.focus, cx);
        }
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
        let search = self.search.read(cx);
        self.search_hovered
            || !search.value().is_empty()
            || search.focus_handle(cx).is_focused(window)
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let open = self.chat.open_id();
        let rows: Vec<_> = self
            .chat
            .conversations()
            .map(|summary| self.render_conversation(summary, open == Some(summary.id), cx))
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
                    .px_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(cx.palette().border)
                    .on_action(cx.listener(Self::dismiss_search))
                    .child(Input::new(&self.search).small().cleanable(true))
                    .test_support(),
            )
        });
        let top = self.sidebar_top.clone();

        v_flex()
            .id("sidebar")
            .relative()
            .w(px(260.))
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
        ListItem::new(("conversation", id.get() as u64))
            .selected(selected)
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

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let (dot, color) = match self.chat.connection() {
            Connection::Connecting | Connection::Syncing => ("○", cx.palette().text_muted),
            Connection::Online => ("●", cx.palette().success),
            Connection::Offline { .. } => ("●", cx.palette().danger),
        };
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

    fn render_conversation_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(summary) = self.chat.open_conversation() else {
            let text = if self.chat.has_conversations() {
                "Select a chat"
            } else {
                "Messages you receive will appear here"
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
            }));

        if self.log.logical_scroll_top().item_ix < LOAD_OLDER_WITHIN {
            let command = self.chat.load_older();
            self.send(command);
        }
        let view = cx.entity();
        let messages = list(self.log.clone(), move |ix, window, cx| {
            view.update(cx, |view, cx| view.render_row(ix, window, cx))
        })
        .size_full()
        .py(LOG_PADDING);

        v_flex()
            .size_full()
            .min_w_0()
            .child(header)
            .child(
                div()
                    .id("log")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(kinetic(
                        "kinetic",
                        &self.kinetic,
                        &self.log,
                        LOG_PADDING * 2.,
                        messages,
                    ))
                    .vertical_scrollbar(&self.log)
                    .test_support(),
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
                    .child(Textarea::new(&self.composer)),
            )
            .into_any_element()
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
        self.composer.read(cx).value().trim().is_empty()
    }

    /// Loads a message's text for editing.
    fn load_edit(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = text else {
            return;
        };
        self.composer.update(cx, |composer, cx| {
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
            self.composer
                .update(cx, |composer, cx| composer.set_value("", window, cx));
        }
        cx.notify();
    }

    fn reply_to(&mut self, id: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.chat.open_id() else {
            return;
        };
        if self.chat.start_reply(open, id) {
            self.composer
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
            return render_log_row(row, hovered, window, cx);
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

/// Log style: `sender @ time > text`; follow-ups in a group indent to the
/// first message's text.
fn render_log_row(
    row: Row<'_>,
    hovered: bool,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let palette = cx.palette();
    let mono = cx.theme().mono_font_family.clone();
    let muted = palette.text_muted;
    let message = row.message;
    let id = message.id;
    let failed = message.status == Some(MessageStatus::Failed);
    let status = match message.status {
        Some(MessageStatus::Pending) => Some(("…", muted)),
        Some(MessageStatus::Sent) => Some(("✓", muted)),
        Some(MessageStatus::Delivered) => Some(("✓✓", muted)),
        Some(MessageStatus::Read) => Some(("✓✓", palette.accent)),
        Some(MessageStatus::Failed) | None => None,
    };
    let error: Option<SharedString> = row.error.map(|error| error.to_string().into());

    let sender = truncate(&row.prefix.sender, MAX_SENDER_CHARS);
    let prefix: SharedString = format!("{sender} @ {} >", row.prefix.time).into();
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
    let sender_color = row
        .prefix
        .peer
        .map_or(palette.text_secondary, |peer| palette.nick(peer));
    let prefix = row.leads.then(|| {
        let sender_end = sender.len();
        div()
            .flex_none()
            .w(width)
            .font_family(mono.clone())
            .text_size(PREFIX_SIZE)
            .line_height(window.line_height())
            .text_color(muted)
            .child(StyledText::new(prefix).with_highlights([(
                0..sender_end,
                HighlightStyle {
                    color: Some(sender_color),
                    ..Default::default()
                },
            )]))
    });
    let indent = (!row.leads).then(|| div().flex_none().w(width));

    let day = row.day.clone().map(|day| {
        h_flex()
            .px_4()
            .pt_3()
            .pb_1()
            .gap_2()
            .font_family(mono.clone())
            .text_xs()
            .text_color(muted)
            .child(div().flex_1().h(px(1.)).bg(palette.border))
            .child(day)
            .child(div().flex_1().h(px(1.)).bg(palette.border))
    });
    let failure = failed.then(|| {
        h_flex()
            .id(("retry", id.get() as u64))
            .gap_1()
            .font_family(mono.clone())
            .text_xs()
            .text_color(palette.danger)
            .cursor_pointer()
            .child("not sent ·")
            .child(div().text_color(palette.action).child("retry"))
            .when_some(error, |this, error| {
                this.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
            })
            .on_click(cx.listener(move |view, _, _, _| view.retry(id)))
            .test_support()
    });

    let line_height = window.line_height();
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
                    .line_height(line_height)
                    .text_color(muted)
                    .children(
                        row.system
                            .iter()
                            .map(|line| div().child(format!("-- {line} · {time}"))),
                    ),
            )
            .into_any_element();
    }
    let placeholders: Vec<_> = row
        .quote
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
        })
        .collect();

    let change_failed = row.change_failed.then(|| {
        div()
            .font_family(mono.clone())
            .text_xs()
            .text_color(palette.danger)
            .child("change not sent")
    });
    let actions = (hovered || row.selected || row.confirming_delete)
        .then(|| row_actions(&row, muted, mono.clone(), cx));
    let palette = cx.palette();

    v_flex()
        .w_full()
        .children(day)
        .child(
            h_flex()
                .id(("row", id.get() as u64))
                .on_hover(
                    cx.listener(move |view, hovered: &bool, _, cx| {
                        view.hover_row(id, *hovered, cx)
                    }),
                )
                .w_full()
                .px_4()
                .items_start()
                .gap_2()
                .when(row.leads, |this| this.pt_1())
                .when(hovered || row.selected || row.confirming_delete, |this| {
                    this.bg(palette.surface)
                })
                .children(prefix)
                .children(indent)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .text_color(palette.text)
                        .children(placeholders)
                        .children(message.content.body().map(|body| {
                            TextView::markdown(
                                ("body", id.get() as u64),
                                message_markdown(body.as_str()),
                            )
                            .selectable(true)
                        }))
                        .children(actions),
                )
                .children(message.edited.then(|| {
                    div()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(muted)
                        .child("edited")
                }))
                .children(status.map(|(status, color)| {
                    div()
                        .font_family(mono.clone())
                        .text_xs()
                        .text_color(color)
                        .child(status)
                }))
                .children(failure)
                .children(change_failed)
                .test_support(),
        )
        .into_any_element()
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
        self.sync_actions();
        h_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::focus_search))
            .size_full()
            .items_stretch()
            .bg(cx.palette().background)
            .text_color(cx.palette().text)
            .child(self.render_sidebar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_conversation_pane(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_are_truncated() {
        assert_eq!(truncate("Alice", 5), "Alice");
        assert_eq!(truncate("Alexandra", 5), "Alex…");
        assert_eq!(truncate("Ångström", 4), "Ång…");
    }
}
