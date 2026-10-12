//! Headless UI test: open a conversation, type in the composer, send.

use fixtures::{Config, ConversationId, Dataset, Sender};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Bounds, Entity, TestAppContext, WindowBounds, WindowOptions, px, size,
};
use spike_gpui::app::ChatApp;
use spike_gpui::store::ChatStore;
use spike_gpui::theme;

fn small_dataset() -> Dataset {
    Dataset::generate(&Config {
        messages: 300,
        conversations: 4,
        ..Config::default()
    })
}

#[gpui_kit::test]
fn sends_a_message_from_the_composer(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let store = cx.new(|_| ChatStore::without_feed(small_dataset()));
    let (handle, app): (_, Entity<ChatApp>) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(1000.), px(700.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                theme::init(window, cx);
                cx.new(|cx| ChatApp::new(store.clone(), window, cx))
            },
        )
        .expect("open test window")
    });

    // Open another conversation through the sidebar.
    let target = cx.update(|cx| store.read(cx).dataset().conversations[2].id);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(("conversation", target.0), cx);
    })
    .unwrap();
    let view = cx.update(|cx| app.read(cx).conversation_view().clone());
    assert_eq!(cx.update(|cx| view.read(cx).conversation()), target);

    let before = message_count(&store, target, cx);
    let loaded_before = cx.update(|cx| view.read(cx).loaded_count());
    let composer = cx.update(|cx| view.read(cx).composer().clone());

    cx.update_window(handle, |_, window, cx| {
        composer.update(cx, |state, cx| state.focus(window, cx));
        window.render_frame(cx);

        // Enter on an empty composer sends nothing.
        window.press("enter", cx);
        window.input("hello 中文", cx);
        window.press("shift-enter", cx);
        window.input("second line", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(message_count(&store, target, cx), before + 1);
    cx.update(|cx| {
        let sent = store.read(cx).conversation(target).last_message().unwrap();
        assert_eq!(sent.sender, Sender::Me);
        assert_eq!(sent.body, "hello 中文\nsecond line");
        assert_eq!(view.read(cx).loaded_count(), loaded_before + 1);
        assert!(composer.read(cx).value().is_empty());
    });
}

fn message_count(store: &Entity<ChatStore>, id: ConversationId, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| store.read(cx).conversation(id).messages.len())
}
