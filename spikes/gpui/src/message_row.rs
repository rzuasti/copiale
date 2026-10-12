//! One row of the message list: optional day separator plus a bubble.

use fixtures::{ConversationKind, ImageAttachment, Message, Sender};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, Hsla, IntoElement, ObjectFit, ParentElement as _, RenderOnce, SharedString,
    Styled as _, StyledImage as _, Window, div, img, relative, rems,
};

use crate::images::ImageCache;
use crate::text::message_markdown;
use crate::theme::{ChatPalette, hsla};

/// Largest image box, in rems.
const IMAGE_MAX_W: f32 = 18.;
const IMAGE_MAX_H: f32 = 20.;

/// Everything a row needs, snapshotted from the store.
#[derive(IntoElement)]
pub struct MessageRow {
    message: Message,
    kind: ConversationKind,
    sender_name: SharedString,
    sender_color: Option<Hsla>,
    /// `(sender, excerpt)` of the replied-to message.
    reply: Option<(SharedString, SharedString)>,
    day_label: Option<SharedString>,
    highlighted: bool,
    images: Entity<ImageCache>,
}

impl MessageRow {
    pub fn new(message: Message, kind: ConversationKind, images: Entity<ImageCache>) -> Self {
        Self {
            message,
            kind,
            sender_name: SharedString::default(),
            sender_color: None,
            reply: None,
            day_label: None,
            highlighted: false,
            images,
        }
    }

    pub fn sender(mut self, name: impl Into<SharedString>, color: Option<Hsla>) -> Self {
        self.sender_name = name.into();
        self.sender_color = color;
        self
    }

    pub fn reply(
        mut self,
        sender: impl Into<SharedString>,
        excerpt: impl Into<SharedString>,
    ) -> Self {
        self.reply = Some((sender.into(), excerpt.into()));
        self
    }

    pub fn day_label(mut self, label: impl Into<SharedString>) -> Self {
        self.day_label = Some(label.into());
        self
    }

    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }
}

impl RenderOnce for MessageRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = *cx.global::<ChatPalette>();
        let outgoing = self.message.sender == Sender::Me;
        let (bubble_bg, bubble_fg) = if outgoing {
            (palette.bubble_outgoing, palette.bubble_outgoing_text)
        } else {
            (palette.bubble_incoming, palette.bubble_incoming_text)
        };
        let id = self.message.id.0;
        let show_sender = !outgoing && self.kind == ConversationKind::Group;
        let image = self
            .message
            .image
            .filter(|_| !debug_flag("SPIKE_NO_IMAGES"))
            .map(|attachment| render_image(attachment, &self.images, cx));
        let reactions: Option<SharedString> = (!self.message.reactions.is_empty()).then(|| {
            self.message
                .reactions
                .iter()
                .map(|r| format!("{} {}", r.emoji, r.count))
                .collect::<Vec<_>>()
                .join("  ")
                .into()
        });

        let bubble = v_flex()
            .min_w_0()
            .max_w(relative(0.8))
            .gap_1()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius_lg)
            .bg(bubble_bg)
            .text_color(bubble_fg)
            // Always bordered so highlighting never changes the row height.
            .border_2()
            .border_color(if self.highlighted {
                palette.accent
            } else {
                bubble_bg
            })
            .when(show_sender, |this| {
                this.child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .when_some(self.sender_color, |this, color| this.text_color(color))
                        .child(self.sender_name.clone()),
                )
            })
            .when_some(self.reply, |this, (sender, excerpt)| {
                this.child(
                    v_flex()
                        .px_2()
                        .py_1()
                        .border_l_2()
                        .border_color(palette.accent)
                        .rounded(cx.theme().radius)
                        .bg(bubble_fg.opacity(0.08))
                        .text_sm()
                        .child(div().font_semibold().child(sender))
                        .child(div().line_clamp(2).child(excerpt)),
                )
            })
            .children(image)
            .when(!self.message.body.is_empty(), |this| {
                if debug_flag("SPIKE_PLAIN_TEXT") {
                    this.child(div().child(self.message.body.clone()))
                } else {
                    this.child(
                        TextView::markdown(("body", id), message_markdown(&self.message.body))
                            .selectable(true),
                    )
                }
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .text_xs()
                    .opacity(0.7)
                    .when_some(reactions, |this, reactions| this.child(reactions))
                    .child(self.message.time_label()),
            );

        // GPUI's list ignores its own horizontal padding, so rows carry it.
        v_flex()
            .w_full()
            .px_4()
            .py_1()
            .when_some(self.day_label, |this, label| {
                this.child(
                    h_flex()
                        .justify_center()
                        .py_2()
                        .text_xs()
                        .text_color(palette.text_muted)
                        .child(label),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .when(outgoing, |this| this.justify_end())
                    .child(bubble),
            )
    }
}

/// Fixed-size box from the attachment's dimensions, so loading never
/// changes the row height.
fn render_image(
    attachment: ImageAttachment,
    images: &Entity<ImageCache>,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let aspect = attachment.width as f32 / attachment.height as f32;
    let width = IMAGE_MAX_W.min(IMAGE_MAX_H * aspect);
    let height = width / aspect;
    let placeholder = hsla(attachment.placeholder_color());
    let loaded = images.update(cx, |cache, cx| cache.get(attachment, cx));

    div()
        .w(rems(width))
        .h(rems(height))
        .rounded(cx.theme().radius)
        .overflow_hidden()
        .bg(placeholder)
        .when_some(loaded, |this, image| {
            this.child(img(image).size_full().object_fit(ObjectFit::Cover))
        })
}

/// Debug toggle for isolating memory and performance costs.
fn debug_flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}
