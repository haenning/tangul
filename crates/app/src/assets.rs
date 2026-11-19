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
        Archive,
        ArrowRightLeft,
        ArrowRightToLine,
        Bookmark,
        Braces,
        ChartArea,
        ChartColumn,
        ChartColumnStacked,
        ChartLine,
        CircleArrowDown,
        CircleDashed,
        Clock,
        CornerDownRight,
        Database,
        Equal,
        EqualNot,
        ExternalLink,
        FingerprintPattern,
        Funnel,
        GripVertical,
        Key,
        KeyRound,
        LayoutDashboard,
        Lightbulb,
        Link,
        Lock,
        LockOpen,
        List,
        ListOrdered,
        ListTree,
        Maximize2,
        Minimize2,
        Pencil,
        PencilOff,
        Pin,
        Plug,
        RotateCcwClock,
        Rows3,
        Sigma,
        SlidersHorizontal,
        Star,
        StarFill,
        Table,
        Type,
        Zap
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

/// The icon of a result shown as the table (`None`) or a chart style.
pub fn style_icon(style: Option<tangul_core::chart::ChartStyle>) -> Lucide {
    use tangul_core::chart::ChartStyle;
    match style {
        None => Lucide::Table,
        Some(ChartStyle::Line) => Lucide::ChartLine,
        Some(ChartStyle::Area | ChartStyle::StackedArea) => Lucide::ChartArea,
        Some(ChartStyle::Bars) => Lucide::ChartColumn,
        Some(ChartStyle::StackedBars) => Lucide::ChartColumnStacked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `Lucide::…` the app's code names, from its sources.
    fn named_in_sources() -> Vec<String> {
        fn walk(dir: &std::path::Path, names: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, names);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for part in text.split("Lucide::").skip(1) {
                        let name: String = part
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric())
                            .collect();
                        // An icon (`Pencil`), not a constant (`ALL`).
                        if name.starts_with(|c: char| c.is_ascii_uppercase())
                            && name.chars().any(|c| c.is_ascii_lowercase())
                        {
                            names.push(name);
                        }
                    }
                }
            }
        }
        let mut names = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut names,
        );
        names.sort();
        names.dedup();
        names
    }

    #[test]
    fn every_icon_the_app_names_is_embedded() {
        let names = named_in_sources();
        assert!(names.iter().any(|n| n == "SlidersHorizontal"), "{names:?}");
        let missing: Vec<&String> = names
            .iter()
            .filter(|name| {
                let icon = Lucide::ALL
                    .iter()
                    .find(|i| format!("{i:?}") == **name)
                    .unwrap_or_else(|| panic!("no such icon: {name}"));
                AppAssets.load(&icon.path()).unwrap().is_none()
            })
            .collect();
        assert!(
            missing.is_empty(),
            "not embedded (add to ExtraIcons): {missing:?}"
        );
    }
}
