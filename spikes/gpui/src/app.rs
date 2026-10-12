//! Window root: conversation sidebar, toolbar and the open conversation.

use fixtures::{ConversationId, ConversationKind, MessageId};
use gpui_kit::component::avatar::Avatar;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window, div,
};

use crate::conversation::ConversationView;
use crate::images::ImageCache;
use crate::store::ChatStore;
use crate::theme::{self, ThemeChoice, Themes};

pub struct ChatApp {
    store: Entity<ChatStore>,
    images: Entity<ImageCache>,
    conversation: Entity<ConversationView>,
    jump_input: Entity<InputState>,
    status: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ChatApp {
    pub fn new(store: Entity<ChatStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let images = cx.new(|_| ImageCache::default());
        let first = most_recent(&store, cx)[0];
        let conversation =
            cx.new(|cx| ConversationView::new(store.clone(), images.clone(), first, window, cx));
        let jump_input = cx.new(|cx| InputState::new(window, cx).placeholder("Jump to message id"));

        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&conversation, |_, _, cx| cx.notify()),
            cx.subscribe_in(&jump_input, window, Self::on_jump_event),
            cx.observe_window_appearance(window, |_, window, cx| {
                theme::appearance_changed(window, cx);
            }),
        ];

        Self {
            store,
            images,
            conversation,
            jump_input,
            status: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn conversation_view(&self) -> &Entity<ConversationView> {
        &self.conversation
    }

    pub fn jump_to(&mut self, id: MessageId, cx: &mut Context<Self>) {
        let found = self
            .conversation
            .update(cx, |view, cx| view.jump_to(id, cx));
        self.status = (!found).then(|| format!("No message #{}", id.0).into());
        cx.notify();
    }

    fn open(&mut self, id: ConversationId, cx: &mut Context<Self>) {
        self.conversation.update(cx, |view, cx| view.open(id, cx));
        let store = self.store.clone();
        if store.read(cx).feed_target().is_some() {
            store.update(cx, |store, cx| store.set_feed_target(Some(id), cx));
        }
    }

    fn toggle_feed(&mut self, on: bool, cx: &mut Context<Self>) {
        let target = on.then(|| self.conversation.read(cx).conversation());
        self.store
            .update(cx, |store, cx| store.set_feed_target(target, cx));
    }

    fn on_jump_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { .. } = event {
            let text = input.read(cx).value();
            match text.trim().trim_start_matches('#').parse::<u64>() {
                Ok(id) => self.jump_to(MessageId(id), cx),
                Err(_) => {
                    self.status = Some("Enter a numeric message id".into());
                    cx.notify();
                }
            }
        }
    }

    fn load_theme_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Load theme".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |app, window, cx| {
                app.status = theme::load_file(&path, window, cx)
                    .err()
                    .map(|error| error.to_string().into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.conversation.read(cx).conversation();
        let store = self.store.read(cx);
        let items: Vec<_> = most_recent(&self.store, cx)
            .into_iter()
            .map(|id| {
                let conversation = store.conversation(id);
                let preview: SharedString = conversation
                    .last_message()
                    .map(|m| {
                        if m.body.is_empty() {
                            "Photo".to_string()
                        } else {
                            m.body.lines().next().unwrap_or_default().to_string()
                        }
                    })
                    .unwrap_or_default()
                    .into();
                let time = conversation
                    .last_message()
                    .map(|m| m.time_label())
                    .unwrap_or_default();
                let icon = match conversation.kind {
                    ConversationKind::Direct => "",
                    ConversationKind::Group => "Group · ",
                };
                ListItem::new(("conversation", id.0))
                    .selected(id == open)
                    .on_click(cx.listener(move |app, _, _, cx| app.open(id, cx)))
                    .child(
                        h_flex()
                            .gap_3()
                            .min_w_0()
                            .child(Avatar::new().name(conversation.title.clone()).small())
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_medium()
                                                    .child(conversation.title.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(time),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{icon}{preview}")),
                                    ),
                            ),
                    )
            })
            .collect();

        v_flex()
            .id("sidebar")
            .w_80()
            .h_full()
            .flex_shrink_0()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                div()
                    .px_4()
                    .py_3()
                    .font_semibold()
                    .child("Copiale · GPUI spike"),
            )
            .child(
                v_flex()
                    .id("conversations")
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .gap_1()
                    .overflow_y_scrollbar()
                    .children(items),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let themes = Themes::global(cx);
        let choice = themes.choice();
        let mut theme_buttons = vec![
            Button::new("theme-system")
                .small()
                .ghost()
                .label("System")
                .selected(choice == ThemeChoice::System)
                .on_click(
                    cx.listener(|_, _, window, cx| theme::select(ThemeChoice::System, window, cx)),
                ),
        ];
        for (ix, theme) in themes.themes().iter().enumerate() {
            theme_buttons.push(
                Button::new(("theme", ix))
                    .small()
                    .ghost()
                    .label(theme.name.clone())
                    .selected(choice == ThemeChoice::Named(ix))
                    .on_click(cx.listener(move |_, _, window, cx| {
                        theme::select(ThemeChoice::Named(ix), window, cx)
                    })),
            );
        }
        let feed_on = self.store.read(cx).feed_target().is_some();

        h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .flex_wrap()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .children(theme_buttons)
            .child(
                Button::new("theme-load")
                    .small()
                    .outline()
                    .label("Load theme…")
                    .on_click(cx.listener(|app, _, window, cx| app.load_theme_file(window, cx))),
            )
            .child(div().flex_1())
            .child(
                Switch::new("feed")
                    .checked(feed_on)
                    .label("Live feed (50 msg/s)")
                    .on_click(cx.listener(|app, on: &bool, _, cx| app.toggle_feed(*on, cx))),
            )
            .child(div().w_56().child(Input::new(&self.jump_input).small()))
    }
}

impl Render for ChatApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let images = self.images.read(cx).ready_count();
        h_flex()
            .size_full()
            .items_stretch()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_toolbar(cx))
                    .child(self.conversation.clone())
                    .child(
                        h_flex()
                            .px_4()
                            .py_1()
                            .gap_4()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(format!("{images} thumbnails cached"))
                            .children(self.status.clone()),
                    ),
            )
    }
}

/// Conversation ids, most recent activity first.
fn most_recent(store: &Entity<ChatStore>, cx: &gpui_kit::App) -> Vec<ConversationId> {
    let mut conversations: Vec<_> = store
        .read(cx)
        .dataset()
        .conversations
        .iter()
        .map(|c| (c.last_message().map(|m| m.id), c.id))
        .collect();
    conversations.sort_by_key(|(last, _)| std::cmp::Reverse(*last));
    conversations.into_iter().map(|(_, id)| id).collect()
}
