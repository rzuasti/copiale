//! Application state, messages, update and view.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fixtures::{ConversationId, ConversationKind, ImageAttachment, MessageId, Sender};
use iced::futures::StreamExt as _;
use iced::keyboard::{self, key};
use iced::theme::Mode;
use iced::widget::text_editor::{self, Binding, KeyPress};
use iced::widget::{
    Space, button, column, container, image, row, scrollable, text, text_input, toggler,
};
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Shrink, Subscription, Task, Theme, system,
    window,
};
use tokio::sync::mpsc;

use crate::feed::FeedHandle;
use crate::images::{self, ImageCache};
use crate::selectable_text::SelectableText;
use crate::store::ChatStore;
use crate::theme::{ChatPalette, ThemeChoice, Themes, color};
use crate::virtual_list::{ListHandle, VirtualList};

/// Messages loaded per page.
pub const PAGE: usize = 200;
/// Load the previous page when this close to the oldest loaded message.
const LOAD_THRESHOLD: usize = 20;
/// Simulated storage latency for loading older messages.
const LOAD_LATENCY: Duration = Duration::from_millis(30);
/// Largest image box, in logical pixels.
const IMAGE_MAX_W: f32 = 288.;
const IMAGE_MAX_H: f32 = 320.;

#[derive(Debug, Clone)]
pub enum Message {
    Open(ConversationId),
    Visible(Range<usize>),
    OlderLoaded(ConversationId),
    Composer(text_editor::Action),
    Send,
    Paste,
    JumpInput(String),
    Jump,
    SelectTheme(ThemeChoice),
    PickThemeFile,
    ThemeFile(Option<PathBuf>),
    SystemMode(Mode),
    Feed(bool),
    Incoming(Vec<fixtures::Message>),
    ImageReady(u64, image::Handle),
    Link(String),
    Frame(Instant),
}

/// Startup options from the command line.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub feed: bool,
    pub jump: Option<u64>,
    pub theme: Option<String>,
    pub metrics: bool,
    pub bench_scroll: Option<f32>,
    /// Off in tests: no feed thread.
    pub live_feed: bool,
    /// Process start, for the first-frame metric.
    pub started: Option<Instant>,
}

struct Bench {
    step: f32,
    started: Instant,
    last: Option<Instant>,
    frames: Vec<Duration>,
}

pub struct App {
    store: ChatStore,
    open: ConversationId,
    /// Index in the conversation's messages of the first loaded message.
    window_start: usize,
    list: ListHandle,
    loading_older: bool,
    highlighted: Option<MessageId>,
    composer: text_editor::Content,
    paste_status: Option<String>,
    jump_input: String,
    status: Option<String>,
    themes: Themes,
    images: ImageCache,
    feed: Option<FeedPort>,
    bench: Option<Bench>,
    started: Instant,
    metrics_pending: bool,
}

impl App {
    pub fn new(dataset: fixtures::Dataset, options: Options) -> (Self, Task<Message>) {
        let started = options.started.unwrap_or_else(Instant::now);
        let store = ChatStore::new(dataset);
        let open = most_recent(&store)[0];
        let mut themes = Themes::load();
        if let Some(name) = &options.theme
            && !themes.select_named(name)
        {
            eprintln!("unknown theme {name}");
        }
        let feed = options.live_feed.then(|| FeedPort::spawn(&store));
        let mut app = Self {
            store,
            open,
            window_start: 0,
            list: ListHandle::default(),
            loading_older: false,
            highlighted: None,
            composer: text_editor::Content::new(),
            paste_status: None,
            jump_input: String::new(),
            status: None,
            themes,
            images: ImageCache::default(),
            feed,
            bench: options.bench_scroll.map(|step| Bench {
                step,
                started,
                last: None,
                frames: Vec::new(),
            }),
            started,
            metrics_pending: options.metrics,
        };
        app.open_conversation(open);
        if let Some(id) = options.jump {
            app.jump_to(MessageId(id));
        }
        if options.feed {
            app.set_feed(true);
        }
        (app, system::theme().map(Message::SystemMode))
    }

    pub fn open_conversation_id(&self) -> ConversationId {
        self.open
    }

    pub fn store(&self) -> &ChatStore {
        &self.store
    }

    pub fn loaded_count(&self) -> usize {
        self.list.count()
    }

    pub fn composer_text(&self) -> String {
        self.composer.text()
    }

    pub fn theme(&self) -> Theme {
        self.themes.iced().clone()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Open(id) => {
                self.open_conversation(id);
                if self.feed_target().is_some() {
                    self.set_feed(true);
                }
            }
            Message::Visible(range) => return self.visible_changed(range),
            Message::OlderLoaded(conversation) => {
                if conversation == self.open {
                    let new_start = self.window_start.saturating_sub(PAGE);
                    // The list keeps its anchor on the same message.
                    self.list.splice(0..0, self.window_start - new_start);
                    self.window_start = new_start;
                }
                self.loading_older = false;
            }
            Message::Composer(action) => self.composer.perform(action),
            Message::Send => self.send(),
            Message::Paste => self.paste(),
            Message::JumpInput(value) => self.jump_input = value,
            Message::Jump => match self.jump_input.trim().trim_start_matches('#').parse() {
                Ok(id) => self.jump_to(MessageId(id)),
                Err(_) => self.status = Some("Enter a numeric message id".into()),
            },
            Message::SelectTheme(choice) => self.themes.select(choice),
            Message::PickThemeFile => {
                return Task::perform(
                    rfd::AsyncFileDialog::new()
                        .set_title("Load theme")
                        .add_filter("Theme", &["toml"])
                        .pick_file(),
                    |file| Message::ThemeFile(file.map(|f| f.path().to_path_buf())),
                );
            }
            Message::ThemeFile(Some(path)) => {
                self.status = self.themes.load_file(&path).err().map(|e| e.to_string());
            }
            Message::ThemeFile(None) => {}
            Message::SystemMode(mode) => self.themes.set_system_mode(mode),
            Message::Feed(on) => self.set_feed(on),
            Message::Incoming(messages) => {
                for (conversation, count) in self.store.append(messages) {
                    if conversation == self.open {
                        let end = self.list.count();
                        self.list.splice(end..end, count);
                    }
                }
            }
            Message::ImageReady(seed, handle) => self.images.insert(seed, handle),
            Message::Link(url) => {
                if let Err(error) = std::process::Command::new("xdg-open").arg(&url).spawn() {
                    self.status = Some(format!("Cannot open {url}: {error}"));
                }
            }
            Message::Frame(now) => return self.frame(now),
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![system::theme_changes().map(Message::SystemMode)];
        if let Some(feed) = &self.feed {
            subscriptions.push(Subscription::run_with(feed.clone(), FeedPort::stream));
        }
        if self.bench.is_some() || self.metrics_pending {
            subscriptions.push(window::frames().map(Message::Frame));
        }
        Subscription::batch(subscriptions)
    }

    fn open_conversation(&mut self, id: ConversationId) {
        let total = self.store.conversation(id).messages.len();
        self.open = id;
        self.window_start = total.saturating_sub(PAGE);
        self.loading_older = false;
        self.highlighted = None;
        self.list.reset(total - self.window_start);
    }

    fn jump_to(&mut self, id: MessageId) {
        let Some((conversation, ix)) = self
            .store
            .dataset()
            .find_message(id)
            .map(|(c, ix)| (c.id, ix))
        else {
            self.status = Some(format!("No message #{}", id.0));
            return;
        };
        if conversation != self.open {
            self.open_conversation(conversation);
        }
        if ix < self.window_start {
            let new_start = ix.saturating_sub(PAGE / 2);
            self.list.splice(0..0, self.window_start - new_start);
            self.window_start = new_start;
        }
        self.list.scroll_to(ix - self.window_start);
        self.highlighted = Some(id);
        self.status = None;
    }

    fn visible_changed(&mut self, range: Range<usize>) -> Task<Message> {
        let mut tasks = Vec::new();
        if range.start < LOAD_THRESHOLD && self.window_start > 0 && !self.loading_older {
            self.loading_older = true;
            let conversation = self.open;
            tasks.push(Task::perform(tokio::time::sleep(LOAD_LATENCY), move |_| {
                Message::OlderLoaded(conversation)
            }));
        }
        let messages = &self.store.conversation(self.open).messages;
        let wanted: Vec<ImageAttachment> = (range.start.saturating_sub(5)..range.end + 5)
            .filter_map(|ix| messages.get(self.window_start + ix)?.image)
            .collect();
        for attachment in wanted {
            if self.images.request(attachment) {
                tasks.push(Task::perform(
                    tokio::task::spawn_blocking(move || images::decode_thumbnail(attachment)),
                    move |handle| {
                        Message::ImageReady(attachment.seed, handle.expect("decode task"))
                    },
                ));
            }
        }
        Task::batch(tasks)
    }

    fn send(&mut self) {
        let body = self.composer.text().trim().to_string();
        if body.is_empty() {
            return;
        }
        self.composer = text_editor::Content::new();
        self.store.send(self.open, body);
        let end = self.list.count();
        self.list.splice(end..end, 1);
        self.list.scroll_to_end();
        self.paste_status = None;
    }

    /// iced's clipboard is text-only, so images are detected via arboard.
    fn paste(&mut self) {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return;
        };
        if let Ok(image) = clipboard.get_image() {
            self.paste_status = Some(format!(
                "Image pasted ({}×{}); attachments are out of scope",
                image.width, image.height
            ));
        } else if let Ok(text) = clipboard.get_text() {
            self.composer
                .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                    Arc::new(text),
                )));
        }
    }

    fn feed_target(&self) -> Option<ConversationId> {
        self.feed.as_ref().and_then(|f| f.handle.target())
    }

    fn set_feed(&mut self, on: bool) {
        if let Some(feed) = &self.feed {
            feed.handle.set_target(on.then_some(self.open));
        }
    }

    fn frame(&mut self, now: Instant) -> Task<Message> {
        if self.metrics_pending {
            self.metrics_pending = false;
            eprintln!("metrics: first frame {:?}", now - self.started);
            spawn_rss_sampler();
        }
        let Some(bench) = &mut self.bench else {
            return Task::none();
        };
        if let Some(last) = bench.last {
            bench.frames.push(now - last);
        }
        bench.last = Some(now);
        if self.window_start == 0 && self.list.is_at_top() {
            report(bench);
            if let Some(kib) = rss_kib() {
                eprintln!("bench: rss after full scroll {} MiB", kib / 1024);
            }
            return iced::exit();
        }
        self.list.scroll_by(-bench.step);
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let palette = *self.themes.palette();
        row![
            self.view_sidebar(&palette),
            column![
                self.view_toolbar(&palette),
                self.view_header(&palette),
                self.view_messages(&palette),
                self.view_composer(&palette),
                self.view_status(&palette),
            ]
            .width(Fill)
            .height(Fill),
        ]
        .height(Fill)
        .into()
    }

    fn view_sidebar(&self, palette: &ChatPalette) -> Element<'_, Message> {
        let palette = *palette;
        let items = most_recent(&self.store).into_iter().map(|id| {
            let conversation = self.store.conversation(id);
            let last = conversation.last_message();
            let preview = last.map_or(String::new(), |m| {
                if m.body.is_empty() {
                    "Photo".into()
                } else {
                    m.body.lines().next().unwrap_or_default().to_string()
                }
            });
            let prefix = match conversation.kind {
                ConversationKind::Direct => "",
                ConversationKind::Group => "Group · ",
            };
            let selected = id == self.open;
            let initials: String = conversation
                .title
                .split_whitespace()
                .filter_map(|w| w.chars().next())
                .take(2)
                .collect();
            let content = row![
                container(text(initials).size(13).color(palette.accent))
                    .center(32)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(palette.surface)),
                        border: Border::default().rounded(16),
                        ..container::Style::default()
                    }),
                column![
                    row![
                        text(conversation.title.clone())
                            .width(Fill)
                            .wrapping(text::Wrapping::None),
                        text(last.map(|m| m.time_label()).unwrap_or_default())
                            .size(12)
                            .color(palette.text_muted),
                    ]
                    .spacing(8),
                    text(format!("{prefix}{preview}"))
                        .size(14)
                        .color(palette.text_muted)
                        .wrapping(text::Wrapping::None)
                        .height(20),
                ]
                .spacing(2)
                .width(Fill)
                .clip(true),
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            button(content)
                .width(Fill)
                .padding([8, 12])
                .on_press(Message::Open(id))
                .style(move |_, status| {
                    let hovered =
                        matches!(status, button::Status::Hovered | button::Status::Pressed);
                    button::Style {
                        background: (selected || hovered)
                            .then_some(Background::Color(palette.surface)),
                        text_color: palette.text,
                        border: Border::default().rounded(8),
                        ..button::Style::default()
                    }
                })
                .into()
        });

        container(
            column![
                container(text("Copiale · iced spike").size(18)).padding([12, 16]),
                scrollable(column(items).spacing(2).padding([0, 8])).height(Fill),
            ]
            .height(Fill),
        )
        .width(320)
        .height(Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(palette.sidebar)),
            border: Border {
                color: palette.border,
                width: 1.,
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
    }

    fn view_toolbar(&self, palette: &ChatPalette) -> Element<'_, Message> {
        let palette = *palette;
        let choice = self.themes.choice();
        let theme_button = move |label: String, value: ThemeChoice| {
            let selected = choice == value;
            button(text(label).size(14))
                .padding([4, 10])
                .on_press(Message::SelectTheme(value))
                .style(move |_, status| button::Style {
                    background: (selected || status == button::Status::Hovered)
                        .then_some(Background::Color(palette.surface)),
                    text_color: palette.text,
                    border: Border::default().rounded(6),
                    ..button::Style::default()
                })
        };
        let mut themes = vec![theme_button("System".into(), ThemeChoice::System).into()];
        for (ix, theme) in self.themes.themes().iter().enumerate() {
            themes.push(theme_button(theme.name.clone(), ThemeChoice::Named(ix)).into());
        }
        themes.push(
            button(text("Load theme…").size(14))
                .padding([4, 10])
                .on_press(Message::PickThemeFile)
                .style(move |_, _| button::Style {
                    background: None,
                    text_color: palette.text,
                    border: Border {
                        color: palette.border,
                        width: 1.,
                        radius: 6.into(),
                    },
                    ..button::Style::default()
                })
                .into(),
        );
        themes.push(Space::new().width(Fill).into());
        themes.push(
            toggler(self.feed_target().is_some())
                .label("Live feed (50 msg/s)")
                .on_toggle(Message::Feed)
                .size(18)
                .width(Shrink)
                .into(),
        );
        themes.push(
            text_input("Jump to message id", &self.jump_input)
                .on_input(Message::JumpInput)
                .on_submit(Message::Jump)
                .size(14)
                .width(200)
                .into(),
        );
        container(row(themes).spacing(8).align_y(Alignment::Center).wrap())
            .padding([8, 16])
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(palette.sidebar)),
                ..container::Style::default()
            })
            .into()
    }

    fn view_header(&self, palette: &ChatPalette) -> Element<'_, Message> {
        let conversation = self.store.conversation(self.open);
        container(
            row![
                text(conversation.title.clone()).size(17),
                text(format!(
                    "{} members · {} of {} messages loaded",
                    conversation.members.len() + 1,
                    self.list.count(),
                    conversation.messages.len()
                ))
                .size(14)
                .color(palette.text_muted)
                .wrapping(text::Wrapping::None),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .clip(true),
        )
        .padding([10, 16])
        .width(Fill)
        .into()
    }

    fn view_messages(&self, palette: &ChatPalette) -> Element<'_, Message> {
        let palette = *palette;
        let conversation = self.store.conversation(self.open);
        let start = self.window_start;
        let list = VirtualList::new(
            &self.list,
            move |ix| conversation.messages.get(start + ix).map_or(0, |m| m.id.0),
            move |ix| self.view_message(start + ix, &palette),
        )
        .on_visible(Message::Visible)
        .scrollbar_color(palette.border);

        let mut content = column![list].height(Fill);
        if self.loading_older {
            content = column![
                container(
                    text("Loading older messages…")
                        .size(12)
                        .color(palette.text_muted)
                )
                .center_x(Fill),
                content
            ];
        }
        container(content)
            .height(Fill)
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(palette.background)),
                ..container::Style::default()
            })
            .into()
    }

    fn view_message(&self, ix: usize, palette: &ChatPalette) -> Element<'_, Message> {
        let palette = *palette;
        let dataset = self.store.dataset();
        let conversation = self.store.conversation(self.open);
        let Some(message) = conversation.messages.get(ix) else {
            return Space::new().into();
        };
        let outgoing = message.sender == Sender::Me;
        let (bubble_bg, bubble_fg) = if outgoing {
            (palette.bubble_outgoing, palette.bubble_outgoing_text)
        } else {
            (palette.bubble_incoming, palette.bubble_incoming_text)
        };

        let mut bubble = column![].spacing(4);
        if let Sender::Contact(id) = message.sender
            && conversation.kind == ConversationKind::Group
        {
            let name_color = dataset.contact(id).map_or(bubble_fg, |c| color(c.color));
            bubble = bubble.push(
                text(dataset.sender_name(message.sender).to_string())
                    .size(14)
                    .color(name_color),
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
            bubble = bubble.push(
                container(
                    column![
                        text(dataset.sender_name(reply.sender).to_string()).size(13),
                        // No line clamp in iced: one clipped line.
                        text(excerpt).size(13).wrapping(text::Wrapping::None),
                    ]
                    .spacing(2),
                )
                .padding([4, 8])
                .width(Fill)
                .clip(true)
                .style(move |_| container::Style {
                    background: Some(Background::Color(Color {
                        a: 0.08,
                        ..bubble_fg
                    })),
                    border: Border {
                        color: palette.accent,
                        width: 0.,
                        radius: 4.into(),
                    },
                    ..container::Style::default()
                }),
            );
        }
        if let Some(attachment) = message.image.filter(|_| !debug_flag("SPIKE_NO_IMAGES")) {
            bubble = bubble.push(self.view_image(attachment));
        }
        if !message.body.is_empty() && debug_flag("SPIKE_PLAIN_TEXT") {
            bubble = bubble.push(text(&message.body).color(bubble_fg));
        } else if !message.body.is_empty() {
            bubble = bubble.push(
                SelectableText::new(&message.body)
                    .color(bubble_fg)
                    .link_color(palette.link)
                    .selection_color(Color {
                        a: 0.5,
                        ..palette.selection
                    })
                    .on_link(Message::Link),
            );
        }
        let reactions: String = message
            .reactions
            .iter()
            .map(|r| format!("{} {}", r.emoji, r.count))
            .collect::<Vec<_>>()
            .join("  ");
        bubble = bubble.push(
            container(
                text(
                    format!("{reactions}  {}", message.time_label())
                        .trim()
                        .to_string(),
                )
                .size(12)
                .color(Color {
                    a: 0.7,
                    ..bubble_fg
                }),
            )
            .align_right(Fill),
        );

        let highlighted = self.highlighted == Some(message.id);
        let bubble = container(bubble.width(Shrink))
            .padding([8, 12])
            .max_width(560)
            .style(move |_| container::Style {
                background: Some(Background::Color(bubble_bg)),
                text_color: Some(bubble_fg),
                // Always bordered so highlighting never changes the height.
                border: Border {
                    color: if highlighted {
                        palette.accent
                    } else {
                        bubble_bg
                    },
                    width: 2.,
                    radius: 12.into(),
                },
                ..container::Style::default()
            });
        let aligned = if outgoing {
            row![Space::new().width(Fill), bubble]
        } else {
            row![bubble, Space::new().width(Fill)]
        };

        let mut item = column![].padding([4, 16]);
        let previous = ix.checked_sub(1).map(|p| &conversation.messages[p]);
        let day = message.date_label();
        if previous.is_none_or(|p| p.date_label() != day) {
            item = item.push(
                container(text(day).size(12).color(palette.text_muted))
                    .center_x(Fill)
                    .padding(8),
            );
        }
        item.push(aligned).into()
    }

    /// Fixed-size box from the attachment's dimensions: no layout shift.
    fn view_image(&self, attachment: ImageAttachment) -> Element<'_, Message> {
        let aspect = attachment.width as f32 / attachment.height as f32;
        let width = IMAGE_MAX_W.min(IMAGE_MAX_H * aspect);
        let height = width / aspect;
        let placeholder = color(attachment.placeholder_color());
        let content: Element<'_, Message> = match self.images.get(attachment.seed) {
            Some(handle) => image(handle.clone())
                .width(Fill)
                .height(Fill)
                .content_fit(iced::ContentFit::Cover)
                .into(),
            None => Space::new().into(),
        };
        container(content)
            .width(width)
            .height(height)
            .clip(true)
            .style(move |_| container::Style {
                background: Some(Background::Color(placeholder)),
                border: Border::default().rounded(8),
                ..container::Style::default()
            })
            .into()
    }

    fn view_composer(&self, palette: &ChatPalette) -> Element<'_, Message> {
        let editor = iced::widget::text_editor(&self.composer)
            .id("composer")
            .placeholder("Message")
            .on_action(Message::Composer)
            .key_binding(composer_binding)
            .height(Shrink)
            .max_height(200)
            .padding(10);
        let mut content = column![].spacing(4);
        if let Some(status) = &self.paste_status {
            content = content.push(text(status).size(12).color(palette.text_muted));
        }
        container(content.push(editor)).padding([12, 16]).into()
    }

    fn view_status(&self, palette: &ChatPalette) -> Element<'_, Message> {
        let mut status = row![text(format!(
            "{} thumbnails cached",
            self.images.ready_count()
        ))]
        .spacing(16);
        if let Some(message) = &self.status {
            status = status.push(text(message));
        }
        container(status)
            .padding([4, 16])
            .style({
                let muted = palette.text_muted;
                move |_| container::Style {
                    text_color: Some(muted),
                    ..container::Style::default()
                }
            })
            .into()
    }
}

/// Enter sends, Shift+Enter inserts a newline, paste goes through arboard.
fn composer_binding(press: KeyPress) -> Option<Binding<Message>> {
    match &press.key {
        keyboard::Key::Named(key::Named::Enter) if !press.modifiers.shift() => {
            Some(Binding::Custom(Message::Send))
        }
        keyboard::Key::Character(c) if press.modifiers.command() && c.as_str() == "v" => {
            Some(Binding::Custom(Message::Paste))
        }
        _ => Binding::from_key_press(press),
    }
}

/// Conversation ids, most recent activity first.
fn most_recent(store: &ChatStore) -> Vec<ConversationId> {
    let mut conversations: Vec<_> = store
        .dataset()
        .conversations
        .iter()
        .map(|c| (c.last_message().map(|m| m.id), c.id))
        .collect();
    conversations.sort_by_key(|(last, _)| std::cmp::Reverse(*last));
    conversations.into_iter().map(|(_, id)| id).collect()
}

/// Bridges the feed thread's channel into an iced subscription.
#[derive(Clone)]
struct FeedPort {
    handle: Arc<FeedHandle>,
    receiver: Arc<Mutex<Option<mpsc::UnboundedReceiver<fixtures::Message>>>>,
}

impl std::hash::Hash for FeedPort {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // One feed per app: a constant identity keeps the subscription alive.
        "feed".hash(state);
    }
}

impl FeedPort {
    fn spawn(store: &ChatStore) -> Self {
        let (handle, receiver) = FeedHandle::spawn(store.dataset(), 7);
        Self {
            handle: Arc::new(handle),
            receiver: Arc::new(Mutex::new(Some(receiver))),
        }
    }

    fn stream(&self) -> impl iced::futures::Stream<Item = Message> + use<> {
        let receiver = self.receiver.lock().expect("feed lock").take();
        iced::futures::stream::unfold(receiver, |receiver| async move {
            let mut receiver = receiver?;
            let message = receiver.recv().await?;
            Some((message, Some(receiver)))
        })
        // Batch bursts into one update.
        .ready_chunks(64)
        .map(Message::Incoming)
    }
}

fn report(bench: &Bench) {
    let mut frames: Vec<f64> = bench
        .frames
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .collect();
    if frames.is_empty() {
        return;
    }
    frames.sort_by(f64::total_cmp);
    let pct = |p: f64| frames[((frames.len() - 1) as f64 * p).round() as usize];
    let slow = frames.iter().filter(|ms| **ms > 25.0).count();
    eprintln!(
        "bench: {} frames in {:.1?}; frame ms p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}; {} frames > 25 ms",
        frames.len(),
        bench.started.elapsed(),
        pct(0.5),
        pct(0.95),
        pct(0.99),
        pct(1.0),
        slow,
    );
}

/// Prints RSS every 10 s (Linux only).
fn spawn_rss_sampler() {
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(10));
            if let Some(kib) = rss_kib() {
                eprintln!("metrics: rss {} MiB", kib / 1024);
            }
        }
    });
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
}

/// Debug toggle for isolating memory and performance costs.
fn debug_flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}
