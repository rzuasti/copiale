//! Main window once linked: chat list on the left, the open conversation on
//! the right.

use std::rc::Rc;
use std::time::Duration;

use copiale_core::{
    Command, ConversationId, ConversationSummary, Direction, Event, MessageId, MessageStatus,
};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FollowMode, InteractiveElement as _, IntoElement,
    ListAlignment, ListState, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, list, px, rems,
};

use super::model::{Chat, Connection, Effects, LogChange, Row, title};
use super::text::message_markdown;
use super::time::Clock;
use crate::CoreSink;
use crate::theme::ActivePalette as _;

/// Load the previous page when the top visible row is this close to the
/// oldest loaded one.
const LOAD_OLDER_WITHIN: usize = 20;

pub struct ChatView {
    core: Rc<dyn CoreSink>,
    chat: Chat,
    log: ListState,
    search: Entity<InputState>,
    composer: Entity<TextareaState>,
    /// Redraws the reconnect countdown.
    countdown: Option<Task<()>>,
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
        Self {
            core,
            chat: Chat::new(Clock::system()),
            log: ListState::new(0, ListAlignment::Top, px(1000.)),
            search,
            composer,
            countdown: None,
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

    pub fn on_event(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) {
        let effects = self.chat.apply(event, window.is_window_active());
        self.run(effects, cx);
        if let Event::Disconnected { .. } = event {
            self.start_countdown(cx);
        }
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
        let effects = self.chat.open(id);
        self.run(effects, cx);
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
                    self.log.reset(count);
                    self.log.set_follow_mode(FollowMode::Tail);
                }
                LogChange::Inserted { at, count } => {
                    self.log.splice(at..at, count);
                    let next = at + count;
                    if next < self.log.item_count() {
                        self.log.remeasure_items(next..next + 1);
                    }
                }
                LogChange::Changed(ix) => self.log.remeasure_items(ix..ix + 1),
            }
        }
        cx.notify();
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
            _ => {}
        }
    }

    fn on_composer_event(
        &mut self,
        composer: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { shift: false, .. } = event {
            let Some(command) = self.chat.submit(&composer.read(cx).value()) else {
                return;
            };
            self.core.send(command);
            composer.update(cx, |composer, cx| composer.set_value("", window, cx));
            self.log.set_follow_mode(FollowMode::Tail);
        }
    }

    fn retry(&mut self, id: MessageId) {
        self.send(self.chat.retry(id));
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
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

        v_flex()
            .w(px(260.))
            .h_full()
            .flex_shrink_0()
            .bg(cx.palette().sidebar)
            .border_r_1()
            .border_color(cx.palette().border)
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.palette().border)
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
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
        let age = summary
            .last_activity
            .map(|at| self.chat.clock().age(at))
            .unwrap_or_default();
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
                            .text_color(cx.palette().text_muted)
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
            .child(div().truncate().child(title(summary)));

        if self.log.logical_scroll_top().item_ix < LOAD_OLDER_WITHIN {
            let command = self.chat.load_older();
            self.send(command);
        }
        let view = cx.entity();
        let messages = list(self.log.clone(), move |ix, _, cx| {
            view.update(cx, |view, cx| view.render_row(ix, cx))
        })
        .size_full()
        .py_2();

        v_flex()
            .size_full()
            .min_w_0()
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(messages)
                    .vertical_scrollbar(&self.log),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.palette().border)
                    .child(Textarea::new(&self.composer)),
            )
            .into_any_element()
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.chat.row(ix) else {
            return div().into_any_element();
        };
        render_log_row(row, cx)
    }
}

/// Log style: time, right-aligned sender, rule, text, status.
fn render_log_row(row: Row<'_>, cx: &mut Context<ChatView>) -> AnyElement {
    let palette = *cx.palette();
    let mono = cx.theme().mono_font_family.clone();
    let muted = palette.text_muted;
    let message = row.message;
    let id = message.id;
    let outgoing = message.direction == Direction::Outgoing;
    let (time, sender) = row.header.clone().unzip();
    let sender_color = if outgoing { muted } else { palette.text };
    let failed = message.status == Some(MessageStatus::Failed);
    let status = match message.status {
        Some(MessageStatus::Pending) => Some("…"),
        Some(MessageStatus::Sent) => Some("✓"),
        Some(MessageStatus::Failed) | None => None,
    };
    let error: Option<SharedString> = row.error.map(|error| error.to_string().into());

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
            .child("not sent · retry")
            .when_some(error, |this, error| {
                this.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
            })
            .on_click(cx.listener(move |view, _, _, _| view.retry(id)))
            .test_support()
    });

    v_flex()
        .w_full()
        .children(day)
        .child(
            h_flex()
                .w_full()
                .px_4()
                .items_start()
                .when(row.header.is_some(), |this| this.pt_1())
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_2()
                        .pr_2()
                        .font_family(mono.clone())
                        .text_sm()
                        .child(div().w(rems(2.75)).text_color(muted).children(time))
                        .child(
                            div()
                                .w(rems(7.))
                                .truncate()
                                .text_right()
                                .text_color(sender_color)
                                .children(sender),
                        ),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_start()
                        .gap_2()
                        .pl_2()
                        .border_l_1()
                        .border_color(palette.border)
                        .child(
                            div().flex_1().min_w_0().child(
                                TextView::markdown(
                                    ("body", id.get() as u64),
                                    message_markdown(message.body.as_str()),
                                )
                                .selectable(true),
                            ),
                        )
                        .children(status.map(|status| {
                            div()
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(muted)
                                .child(status)
                        }))
                        .children(failure),
                ),
        )
        .into_any_element()
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .items_stretch()
            .bg(cx.palette().background)
            .text_color(cx.palette().text)
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_conversation_pane(cx)),
            )
    }
}
