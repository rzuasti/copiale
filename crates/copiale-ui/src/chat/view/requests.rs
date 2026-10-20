//! The banner standing in for the composer in a message request or blocked
//! chat, and answering it.

use copiale_core::Command;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use crate::theme::ActivePalette as _;

use super::{AcceptRequest, BlockChat, ChatView, DeleteRequest, REQUEST, UnblockChat};

impl ChatView {
    /// Sends an answer to a request, or a block, and returns to the chat.
    pub(super) fn answer_request(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.core.send(command);
        self.restore_focus(window, cx);
        cx.notify();
    }

    fn accept_request(&mut self, _: &AcceptRequest, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.chat.accept_request() {
            self.answer_request(command, window, cx);
        }
    }

    fn unblock_chat(&mut self, _: &UnblockChat, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.chat.unblock() {
            self.answer_request(command, window, cx);
        }
    }

    fn block_chat(&mut self, _: &BlockChat, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.chat.block() {
            self.answer_request(command, window, cx);
        }
    }

    fn delete_request(&mut self, _: &DeleteRequest, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.chat.delete_request() {
            self.answer_request(command, window, cx);
        }
    }

    /// Why there's no composer, and the answers, with their keys; `None`
    /// once accepted.
    pub(super) fn render_request_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let banner = self.chat.request_banner()?;
        let answers: &[(&str, &str, &str)] = if banner.blocked {
            &[
                ("unblock", "u", "unblock"),
                ("delete-request", "d", "delete"),
            ]
        } else {
            &[
                ("accept-request", "a", "accept"),
                ("delete-request", "d", "delete"),
                ("block-chat", "b", "block"),
            ]
        };
        let buttons = answers.iter().map(|&(id, key, label)| {
            h_flex()
                .id(id)
                .gap_1()
                .cursor_pointer()
                .child(div().text_color(cx.palette().text_muted).child(key))
                .child(div().text_color(cx.palette().action).child(label))
                .on_click(cx.listener(move |view, _, window, cx| match id {
                    "accept-request" => view.accept_request(&AcceptRequest, window, cx),
                    "unblock" => view.unblock_chat(&UnblockChat, window, cx),
                    "block-chat" => view.block_chat(&BlockChat, window, cx),
                    _ => view.delete_request(&DeleteRequest, window, cx),
                }))
                .test_support()
        });
        let element = v_flex()
            .id("request-banner")
            .key_context(REQUEST)
            .track_focus(&self.request_focus)
            .on_action(cx.listener(Self::accept_request))
            .on_action(cx.listener(Self::unblock_chat))
            .on_action(cx.listener(Self::block_chat))
            .on_action(cx.listener(Self::delete_request))
            .gap_1()
            .py_1()
            .font_family(cx.theme().mono_font_family.clone())
            .text_sm()
            .child(
                div()
                    .text_color(if banner.blocked {
                        cx.palette().text_muted
                    } else {
                        cx.palette().danger
                    })
                    .child(SharedString::from(banner.text)),
            )
            .child(h_flex().gap_4().children(buttons))
            .test_support();
        Some(element.into_any_element())
    }
}
