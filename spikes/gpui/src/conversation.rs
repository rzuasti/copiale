//! The open conversation: virtualized message list plus composer.

use std::time::Duration;

use fixtures::{ConversationId, MessageId, Sender};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardEntry, Context, Entity, FollowMode, IntoElement,
    ListAlignment, ListOffset, ListState, ParentElement as _, Pixels, Render, SharedString,
    Styled as _, Subscription, Window, div, list, px,
};

use crate::images::ImageCache;
use crate::message_row::MessageRow;
use crate::store::{ChatStore, StoreEvent};
use crate::theme::hsla;

/// Messages loaded per page.
pub const PAGE: usize = 200;
/// Load the previous page when this close to the oldest loaded message.
const LOAD_THRESHOLD: usize = 20;
/// Simulated storage latency for loading older messages.
const LOAD_LATENCY: Duration = Duration::from_millis(30);

pub struct ConversationView {
    store: Entity<ChatStore>,
    images: Entity<ImageCache>,
    conversation: ConversationId,
    /// Index in the conversation's messages of the first loaded message.
    window_start: usize,
    list: ListState,
    loading_older: bool,
    highlighted: Option<MessageId>,
    composer: Entity<TextareaState>,
    paste_status: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ConversationView {
    pub fn new(
        store: Entity<ChatStore>,
        images: Entity<ImageCache>,
        conversation: ConversationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder("Message")
        });
        let list = ListState::new(0, ListAlignment::Top, px(1000.));

        let subscriptions = vec![
            cx.subscribe(&store, Self::on_store_event),
            cx.subscribe_in(&composer, window, Self::on_composer_event),
            cx.observe(&images, |_, _, cx| cx.notify()),
        ];

        let mut view = Self {
            store,
            images,
            conversation,
            window_start: 0,
            list,
            loading_older: false,
            highlighted: None,
            composer,
            paste_status: None,
            _subscriptions: subscriptions,
        };
        view.open(conversation, cx);
        view
    }

    pub fn conversation(&self) -> ConversationId {
        self.conversation
    }

    pub fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    pub fn loaded_count(&self) -> usize {
        self.list.item_count()
    }

    /// Whether the oldest message is loaded and scrolled into view.
    pub fn is_at_top(&self) -> bool {
        let top = self.list.logical_scroll_top();
        self.window_start == 0 && top.item_ix == 0 && top.offset_in_item <= px(0.)
    }

    pub fn scroll_by(&mut self, distance: Pixels, cx: &mut Context<Self>) {
        self.list.scroll_by(distance);
        cx.notify();
    }

    /// Shows `conversation`, scrolled to its newest message.
    pub fn open(&mut self, conversation: ConversationId, cx: &mut Context<Self>) {
        let total = self
            .store
            .read(cx)
            .conversation(conversation)
            .messages
            .len();
        self.conversation = conversation;
        self.window_start = total.saturating_sub(PAGE);
        self.loading_older = false;
        self.highlighted = None;
        self.list.reset(total - self.window_start);
        self.list.set_follow_mode(FollowMode::Tail);
        cx.notify();
    }

    /// Opens the message's conversation and scrolls to it.
    pub fn jump_to(&mut self, id: MessageId, cx: &mut Context<Self>) -> bool {
        let Some((conversation, ix)) = self
            .store
            .read(cx)
            .dataset()
            .find_message(id)
            .map(|(c, ix)| (c.id, ix))
        else {
            return false;
        };
        if conversation != self.conversation {
            self.open(conversation, cx);
        }
        if ix < self.window_start {
            // Load everything from one page before the target.
            let new_start = ix.saturating_sub(PAGE / 2);
            self.list.splice(0..0, self.window_start - new_start);
            self.window_start = new_start;
        }
        // Scrolling away from the end pauses tail following.
        self.list.scroll_to(ListOffset {
            item_ix: ix - self.window_start,
            offset_in_item: px(0.),
        });
        self.highlighted = Some(id);
        cx.notify();
        true
    }

    /// Starts loading the previous page when the top is near. Checked on
    /// render because the list's scroll handler only fires for wheel input.
    fn load_older_if_near_top(&mut self, cx: &mut Context<Self>) {
        let near_top = self.list.logical_scroll_top().item_ix < LOAD_THRESHOLD;
        if near_top && self.window_start > 0 && !self.loading_older {
            self.load_older(cx);
        }
    }

    fn load_older(&mut self, cx: &mut Context<Self>) {
        self.loading_older = true;
        let conversation = self.conversation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOAD_LATENCY).await;
            this.update(cx, |view, cx| {
                if view.conversation != conversation {
                    return;
                }
                let new_start = view.window_start.saturating_sub(PAGE);
                // Prepending keeps the scroll anchor on the same message.
                view.list.splice(0..0, view.window_start - new_start);
                view.window_start = new_start;
                view.loading_older = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_store_event(&mut self, _: Entity<ChatStore>, event: &StoreEvent, cx: &mut Context<Self>) {
        let StoreEvent::Appended {
            conversation,
            count,
        } = event;
        if *conversation == self.conversation {
            let end = self.list.item_count();
            self.list.splice(end..end, *count);
            cx.notify();
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
            let body = composer.read(cx).value().trim().to_string();
            if body.is_empty() {
                return;
            }
            composer.update(cx, |state, cx| state.set_value("", window, cx));
            let conversation = self.conversation;
            self.store
                .update(cx, |store, cx| store.send(conversation, body, cx));
            self.paste_status = None;
            self.list.set_follow_mode(FollowMode::Tail);
        }
    }

    fn paste_detected(&mut self, status: SharedString, cx: &mut Context<Self>) {
        self.paste_status = Some(status);
        cx.notify();
    }

    fn render_header(&self, cx: &App) -> impl IntoElement {
        let store = self.store.read(cx);
        let conversation = store.conversation(self.conversation);
        let subtitle = format!(
            "{} members · {} of {} messages loaded",
            conversation.members.len() + 1,
            self.list.item_count(),
            conversation.messages.len(),
        );
        h_flex()
            .px_4()
            .py_2()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().font_semibold().child(conversation.title.clone()))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(subtitle),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.clone();
        let images = self.images.clone();
        let conversation = self.conversation;
        let window_start = self.window_start;
        let highlighted = self.highlighted;

        let messages = list(self.list.clone(), move |ix, _, cx| {
            render_row(
                &store,
                &images,
                conversation,
                window_start + ix,
                highlighted,
                cx,
            )
        })
        .size_full()
        .py_2();

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(messages)
            .vertical_scrollbar(&self.list)
            .when(self.loading_older, |this| {
                this.child(
                    h_flex()
                        .absolute()
                        .top_2()
                        .w_full()
                        .justify_center()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Loading older messages…"),
                )
            })
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let textarea = Textarea::new(&self.composer).on_paste(move |item, _, cx| {
            let image = item.entries().iter().find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image.format),
                _ => None,
            });
            let Some(format) = image else {
                return false;
            };
            let status = format!("Image pasted ({format:?}); attachments are out of scope");
            weak.update(cx, |view, cx| view.paste_detected(status.into(), cx))
                .ok();
            true
        });

        v_flex()
            .gap_1()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .when_some(self.paste_status.clone(), |this, status| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(status),
                )
            })
            .child(textarea)
    }
}

impl Render for ConversationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_older_if_near_top(cx);
        v_flex()
            .size_full()
            .min_w_0()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(self.render_list(cx))
            .child(self.render_composer(cx))
    }
}

fn render_row(
    store: &Entity<ChatStore>,
    images: &Entity<ImageCache>,
    conversation: ConversationId,
    ix: usize,
    highlighted: Option<MessageId>,
    cx: &mut App,
) -> AnyElement {
    let store = store.read(cx);
    let dataset = store.dataset();
    let conversation = store.conversation(conversation);
    let Some(message) = conversation.messages.get(ix) else {
        return div().into_any_element();
    };

    let mut row = MessageRow::new(message.clone(), conversation.kind, images.clone())
        .highlighted(highlighted == Some(message.id));
    if let Sender::Contact(id) = message.sender {
        let contact = dataset.contact(id);
        row = row.sender(
            dataset.sender_name(message.sender).to_string(),
            contact.map(|c| hsla(c.color)),
        );
    }
    if let Some(reply) = message
        .reply_to
        .and_then(|id| conversation.position(id))
        .map(|pos| &conversation.messages[pos])
    {
        let excerpt: String = if reply.body.is_empty() {
            "Photo".into()
        } else {
            reply.body.chars().take(140).collect()
        };
        row = row.reply(dataset.sender_name(reply.sender).to_string(), excerpt);
    }
    let previous = ix.checked_sub(1).map(|p| &conversation.messages[p]);
    let day = message.date_label();
    if previous.is_none_or(|p| p.date_label() != day) {
        row = row.day_label(day);
    }
    row.into_any_element()
}
