//! Colour emoji: a bundled font on Linux (`fonts/README.md`); macOS and
//! Windows use the system's.

use gpui_kit::{App, FontFallbacks};

/// Family of the bundled emoji font.
pub const EMOJI_FAMILY: &str = "Copiale Emoji";

#[cfg(target_os = "linux")]
const EMOJI_FONT: &[u8] = include_bytes!("../../fonts/CopialeEmoji.ttf");

/// Loads the emoji font and makes it every window's fallback, so emoji
/// the configured fonts lack come from it, not a monochrome symbol font.
pub(crate) fn init(cx: &mut App) {
    #[cfg(target_os = "linux")]
    {
        use std::borrow::Cow;

        if let Err(error) = cx.text_system().add_fonts(vec![Cow::Borrowed(EMOJI_FONT)]) {
            tracing::warn!(%error, "emoji font not loadable");
            return;
        }
        gpui_kit::base::Root::register_plugin(cx, |_, _| EmojiFallback);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cx;
}

/// Fallbacks for fonts set outside the window's text style.
pub fn fallbacks() -> Option<FontFallbacks> {
    cfg!(target_os = "linux").then(|| FontFallbacks::from_fonts(vec![EMOJI_FAMILY.to_owned()]))
}

/// Sets [`fallbacks`] on the root of every window.
#[cfg(target_os = "linux")]
struct EmojiFallback;

#[cfg(target_os = "linux")]
impl gpui_kit::Render for EmojiFallback {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        gpui_kit::Empty
    }
}

#[cfg(target_os = "linux")]
impl gpui_kit::base::RootPlugin for EmojiFallback {
    fn style(
        &self,
        surface: &mut gpui_kit::Stateful<gpui_kit::Div>,
        _: &mut gpui_kit::Window,
        _: &mut App,
    ) {
        use gpui_kit::Styled as _;

        surface.text_style().font_fallbacks = fallbacks();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn bundled_font_loads_as_a_colour_fallback() {
        let face = ttf_parser::Face::parse(EMOJI_FONT, 0).unwrap();
        let name = |id| {
            face.names()
                .into_iter()
                .find(|name| name.name_id == id && name.is_unicode())
                .and_then(|name| name.to_string())
        };
        assert_eq!(
            name(ttf_parser::name_id::FAMILY).as_deref(),
            Some(EMOJI_FAMILY)
        );
        // GPUI draws only this name in colour.
        assert_eq!(
            name(ttf_parser::name_id::POST_SCRIPT_NAME).as_deref(),
            Some("NotoColorEmoji")
        );
        // GPUI loads only families that map 'm'.
        assert!(face.glyph_index('m').is_some());
        assert!(face.glyph_index('❤').is_some());
    }

    #[test]
    fn fallbacks_name_the_bundled_font_on_linux() {
        let fallbacks = fallbacks().map(|fallbacks| fallbacks.fallback_list().to_vec());
        if cfg!(target_os = "linux") {
            assert_eq!(fallbacks, Some(vec![EMOJI_FAMILY.to_owned()]));
        } else {
            assert_eq!(fallbacks, None);
        }
    }
}
