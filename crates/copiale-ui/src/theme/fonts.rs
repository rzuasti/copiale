//! Font families and sizes from the settings, where installed.

use std::rc::Rc;

use copiale_core::config::Typography;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Global, SharedString};

use super::system;

/// The fonts the system has, listed once.
#[derive(Clone, Debug)]
pub struct InstalledFonts(pub Rc<[String]>);

impl Global for InstalledFonts {}

impl InstalledFonts {
    pub fn get(cx: &mut App) -> Rc<[String]> {
        if let Some(fonts) = cx.try_global::<Self>() {
            return fonts.0.clone();
        }
        let mut names = cx.text_system().all_font_names();
        // Bundled for emoji only.
        names.retain(|name| name != super::EMOJI_FAMILY);
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup();
        let fonts: Rc<[String]> = names.into();
        cx.set_global(Self(fonts.clone()));
        fonts
    }
}

/// GPUI Kit's default UI font.
const KIT_UI_FONT: &str = ".SystemUIFont";

/// GPUI Kit's default mono font, as it was before any theme applied.
struct KitMono(SharedString);

impl Global for KitMono {}

/// Typography in effect: `cx.global::<Fonts>()`.
#[derive(Clone, Debug, PartialEq)]
pub struct Fonts {
    pub ui: SharedString,
    pub mono: SharedString,
    /// Message text.
    pub body: SharedString,
    /// Base size in pixels.
    pub size: f32,
    /// Multiple of the font size, for message text.
    pub line_height: f32,
}

impl Global for Fonts {}

impl Fonts {
    /// The configured fonts; one that isn't installed falls back to the
    /// default, as GPUI fails on unknown families.
    pub fn resolve(typography: &Typography, cx: &mut App) -> Self {
        let configured = [
            &typography.ui_font,
            &typography.mono_font,
            &typography.body_font,
        ];
        let installed = if configured.iter().any(|font| font.is_some()) {
            InstalledFonts::get(cx)
        } else {
            Rc::from([])
        };
        let pick = |font: &Option<String>, setting: &str| -> Option<SharedString> {
            let font = font.as_ref()?;
            if installed.contains(font) {
                Some(font.clone().into())
            } else {
                tracing::warn!(setting, "font not installed; using the default");
                None
            }
        };
        let kit_mono = match cx.try_global::<KitMono>() {
            Some(mono) => mono.0.clone(),
            None => {
                let mono = cx.theme().mono_font_family.clone();
                cx.set_global(KitMono(mono.clone()));
                mono
            }
        };
        let ui = pick(&typography.ui_font, "typography.ui_font").unwrap_or(KIT_UI_FONT.into());
        Self {
            mono: pick(&typography.mono_font, "typography.mono_font")
                .or_else(|| system::mono_font(cx))
                .unwrap_or(kit_mono),
            body: pick(&typography.body_font, "typography.body_font").unwrap_or(ui.clone()),
            ui,
            size: typography.size as f32,
            line_height: typography.line_height as f32,
        }
    }

    /// With the families GPUI Kit draws: it names the installed font that
    /// `.SystemUIFont` lands on, as looking up a missing family again on
    /// every text run is costly.
    pub(super) fn into_resolved(self, cx: &App) -> Self {
        let kit = cx.theme();
        let body = if self.body == self.ui {
            kit.font_family.clone()
        } else {
            self.body
        };
        Self {
            ui: kit.font_family.clone(),
            mono: kit.mono_font_family.clone(),
            body,
            ..self
        }
    }
}
