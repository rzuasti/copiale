//! System appearance and fonts.

use copiale_core::config::ThemeMode;
use gpui_kit::{App, Global, SharedString, Window, WindowAppearance};

use super::{Theme, apply};
use crate::settings::Settings;

/// The system's color scheme preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
    NoPreference,
}

impl From<WindowAppearance> for Scheme {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
        }
    }
}

impl Theme {
    /// The built-in theme for a system preference; dark without one.
    pub fn for_scheme(scheme: Scheme) -> Self {
        match scheme {
            Scheme::Dark | Scheme::NoPreference => Self::dark(),
            Scheme::Light => Self::light(),
        }
    }

    /// The built-in theme for the `appearance.theme` setting.
    pub fn for_mode(mode: ThemeMode, scheme: Scheme) -> Self {
        match mode {
            ThemeMode::System => Self::for_scheme(scheme),
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }
}

/// The system's scheme as last reported, and what was applied for it.
struct Followed {
    scheme: Scheme,
    applied: Option<(ThemeMode, Scheme)>,
}

impl Global for Followed {}

/// Notes the system's scheme and applies the theme for it, unless the
/// settings override it.
pub fn follow(scheme: Scheme, cx: &mut App) {
    cx.set_global(Followed {
        scheme,
        applied: None,
    });
    refresh(cx);
}

/// Applies the theme for the settings and the system's scheme, if changed.
fn refresh(cx: &mut App) {
    let Some(followed) = cx.try_global::<Followed>() else {
        return;
    };
    let wanted = (Settings::get(cx).appearance.theme, followed.scheme);
    if followed.applied == Some(wanted) {
        return;
    }
    apply(&Theme::for_mode(wanted.0, wanted.1), cx);
    cx.global_mut::<Followed>().applied = Some(wanted);
}

/// Reapplies the theme when the settings change it.
pub(crate) fn watch_settings(cx: &mut App) {
    cx.observe_global::<Settings>(refresh).detach();
}

/// Follows the system color scheme. On Linux, reads it from the desktop
/// portal, as GPUI reports no preference as light.
pub fn init(window: &mut Window, cx: &mut App) {
    #[cfg(target_os = "linux")]
    if portal::listen(cx) {
        return;
    }
    init_from_window(window, cx);
}

/// Follows the system appearance as GPUI reports it.
pub fn init_from_window(window: &mut Window, cx: &mut App) {
    follow(window.appearance().into(), cx);
    window
        .observe_window_appearance(|window, cx| follow(window.appearance().into(), cx))
        .detach();
}

/// The system's monospace font, if GPUI can load it.
pub(super) fn mono_font(cx: &App) -> Option<SharedString> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::OnceLock;
        static FONT: OnceLock<Option<SharedString>> = OnceLock::new();
        FONT.get_or_init(|| {
            // GPUI panics on a family it can't find.
            let family = fontconfig_monospace()?;
            let installed = cx.text_system().all_font_names();
            installed
                .contains(&family)
                .then(|| family.into())
                .or_else(|| {
                    tracing::warn!("system monospace font is not loadable; using the default");
                    None
                })
        })
        .clone()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cx;
        None
    }
}

/// The family fontconfig resolves `monospace` to.
#[cfg(target_os = "linux")]
fn fontconfig_monospace() -> Option<String> {
    let fc = fontconfig::Fontconfig::new()?;
    let mut pattern = fontconfig::Pattern::new(&fc);
    pattern.add_string(c"family", c"monospace");
    let matched = pattern.font_match();
    matched.get_string(c"family").map(str::to_owned)
}

#[cfg(target_os = "linux")]
mod portal {
    use std::time::Duration;

    use ashpd::desktop::settings::{ColorScheme, Settings};
    use futures::StreamExt as _;
    use gpui_kit::{App, AsyncApp};

    use super::Scheme;

    /// How long startup waits for the portal before falling back.
    const TIMEOUT: Duration = Duration::from_millis(500);

    impl From<ColorScheme> for Scheme {
        fn from(scheme: ColorScheme) -> Self {
            match scheme {
                ColorScheme::PreferDark => Self::Dark,
                ColorScheme::PreferLight => Self::Light,
                ColorScheme::NoPreference => Self::NoPreference,
            }
        }
    }

    /// Applies the portal's color scheme and follows its changes; false if
    /// the portal can't be read.
    pub fn listen(cx: &mut App) -> bool {
        let read = async {
            let settings = Settings::new().await?;
            let scheme = settings.color_scheme().await?;
            ashpd::Result::Ok((settings, scheme))
        };
        let (settings, scheme) = match cx.foreground_executor().block_with_timeout(TIMEOUT, read) {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => {
                tracing::warn!(%error, "can't read the color scheme from the portal");
                return false;
            }
            Err(_) => {
                tracing::warn!("color scheme portal timed out");
                return false;
            }
        };
        super::follow(scheme.into(), cx);
        cx.spawn(async move |cx: &mut AsyncApp| {
            let mut changes = match settings.receive_color_scheme_changed().await {
                Ok(changes) => changes,
                Err(error) => {
                    tracing::warn!(%error, "can't follow color scheme changes");
                    return;
                }
            };
            while let Some(scheme) = changes.next().await {
                cx.update(|cx| super::follow(scheme.into(), cx));
            }
        })
        .detach();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Appearance;

    #[test]
    fn no_preference_is_dark() {
        for (scheme, expected) in [
            (Scheme::Dark, Appearance::Dark),
            (Scheme::Light, Appearance::Light),
            (Scheme::NoPreference, Appearance::Dark),
        ] {
            assert_eq!(Theme::for_scheme(scheme).appearance, expected);
        }
    }

    #[test]
    fn window_appearances_map_to_schemes() {
        for (appearance, expected) in [
            (WindowAppearance::Dark, Scheme::Dark),
            (WindowAppearance::VibrantDark, Scheme::Dark),
            (WindowAppearance::Light, Scheme::Light),
            (WindowAppearance::VibrantLight, Scheme::Light),
        ] {
            assert_eq!(Scheme::from(appearance), expected);
        }
    }
}
