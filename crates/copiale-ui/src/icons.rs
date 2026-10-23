//! Icons: gpui-kit's default Lucide bundle plus the few extra the app uses.

use std::borrow::Cow;

use gpui_kit::assets::icon_assets;
use gpui_kit::{AssetSource, Result, SharedString};

pub use gpui_kit::assets::IconName;

icon_assets!(Extra, [FaceSlightlySmiling, SquarePen]);

/// The app's asset source.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // The default bundle errors on paths it lacks.
        match Extra.load(path)? {
            Some(data) => Ok(Some(data)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Extra.list(path)?;
        paths.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_used_icon_loads() {
        for icon in [
            IconName::Settings,
            IconName::FaceSlightlySmiling,
            IconName::SquarePen,
        ] {
            let data = Assets.load(&icon.path()).unwrap();
            assert!(data.is_some_and(|svg| svg.starts_with(b"<svg")), "{icon:?}");
        }
    }
}
