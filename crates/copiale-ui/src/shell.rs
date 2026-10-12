use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{Context, IntoElement, ParentElement as _, Render, Styled as _, Window, div};

/// Root view of the main window.
pub struct Shell;

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .text_color(cx.theme().muted_foreground)
            .child("Copiale")
    }
}
