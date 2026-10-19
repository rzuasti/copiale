//! The chat list, its search and the connection status.

use std::time::Duration;

use copiale_core::{Command, ConversationSummary, Peer};
use gpui_kit::base::{Easing, Presence, PresencePhase, Transition};
use gpui_kit::component::input::{Escape, Input};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Focusable as _, Hsla, InteractiveElement as _, IntoElement,
    MouseMoveEvent, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, canvas, div, px, rems,
};

use crate::chat::layout::Spacing;
use crate::chat::model::{Connection, title};
use crate::drawer::drawer;
use crate::settings::Settings;
use crate::theme::ActivePalette as _;

use super::{ChatView, FocusSearch, SEARCHING, SearchNext, SearchPrevious, full_size};

/// Height of the strip at the sidebar's top whose hover reveals search.
const SEARCH_STRIP: Pixels = px(12.);
/// How long search takes to slide in or out.
const SEARCH_SLIDE: Duration = Duration::from_millis(260);

const SIDEBAR_WIDTH: Pixels = px(260.);
/// How long the chat list takes to slide in or out between layouts.
const SIDEBAR_SLIDE: Duration = Duration::from_millis(220);

impl ChatView {
    /// Focuses search; the palette stands in while the chat list is hidden.
    pub(super) fn focus_search(
        &mut self,
        _: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        let layout = &Settings::get(cx).layout;
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

    /// The last storage failure, if any, above the connection.
    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let error = self.chat.storage_error().map(|error| {
            h_flex()
                .id("storage-error")
                .gap_2()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(cx.palette().border)
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .text_color(cx.palette().danger)
                .cursor_pointer()
                .child(div().flex_none().child("⚠"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("local data: {error}")),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(cx.palette().action)
                        .child("dismiss"),
                )
                .tooltip(storage_error_tooltip(error))
                .on_click(cx.listener(|view, _, _, cx| view.dismiss_storage_error(cx)))
                .test_support()
        });
        v_flex()
            .children(error)
            .child(self.render_connection(cx))
            .into_any_element()
    }

    fn dismiss_storage_error(&mut self, cx: &mut Context<Self>) {
        self.chat.dismiss_storage_error();
        cx.notify();
    }

    fn render_connection(&self, cx: &mut Context<Self>) -> AnyElement {
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

    /// In compact mode, the header stands in for the chat list: unread
    /// chats elsewhere, and the connection unless online.
    pub(super) fn render_compact_status(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
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
        if let Some(error) = self.chat.storage_error() {
            items.push(
                div()
                    .id("header-storage-error")
                    .flex_none()
                    .ml_3()
                    .text_xs()
                    .text_color(palette.danger)
                    .cursor_pointer()
                    .child("⚠")
                    .tooltip(storage_error_tooltip(error))
                    .on_click(cx.listener(|view, _, _, cx| view.dismiss_storage_error(cx)))
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
    pub(super) fn render_sidebar_slot(
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
}

/// What a storage failure means, for its tooltip.
fn storage_error_tooltip(
    error: &str,
) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static {
    let text = SharedString::from(format!(
        "Copiale couldn't read or save its local data ({error}). Something may not have been \
         saved. Click to dismiss."
    ));
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
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

/// Fast start, long gentle settle, like iOS sheets.
fn drawer_easing() -> Easing {
    Easing::cubic_bezier(0.32, 0.72, 0., 1.).expect("valid curve")
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
}
