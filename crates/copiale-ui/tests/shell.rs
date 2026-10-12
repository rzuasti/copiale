//! Headless tests for the main window shell.

use copiale_ui::{APP_ID, Shell, main_window_options};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

#[gpui_kit::test]
fn main_window_options_set_app_id_and_title(cx: &mut TestAppContext) {
    let options = cx.update(|cx| main_window_options(cx));
    assert_eq!(options.app_id.as_deref(), Some(APP_ID));
    let title = options.titlebar.and_then(|titlebar| titlebar.title);
    assert_eq!(title.as_deref(), Some("Copiale"));
}

#[gpui_kit::test]
fn shell_renders_in_main_window(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (handle, _shell) = cx
        .update(|cx| gpui_kit::open_window(main_window_options(cx), cx, |_, cx| cx.new(|_| Shell)))
        .expect("open main window");

    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("render shell");
}
