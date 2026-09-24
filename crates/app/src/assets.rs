//! Asset source: gpui-kit's default component icons plus the few extra Lucide icons
//! the app uses (only these get embedded, not the whole catalog).

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// The full Lucide catalog names; only those listed in [`ExtraIcons`] (plus the
/// component defaults) are embedded.
pub use gpui_kit::assets::IconName as Lucide;

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Bookmark,
        Database,
        Lightbulb,
        ListTree,
        Pencil,
        PencilOff,
        Pin,
        RotateCcwClock,
        Sparkles,
        Table
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
