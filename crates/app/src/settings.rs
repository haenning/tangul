//! `settings.json` in the config directory: user settings, applied to the theme and
//! reloaded while the app runs.
//!
//! ```json
//! { "editor": { "font_size": 14, "font_family": "Menlo" } }
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::Theme;
use gpui_kit::{App, Pixels, SharedString, px};
use serde::Deserialize;

use crate::store::config_dir;

const FILE: &str = "settings.json";
/// How often the file is checked for changes.
const POLL: Duration = Duration::from_secs(1);
/// A typo can't make the editor unusable.
const FONT_SIZE_RANGE: (f32, f32) = (8., 32.);

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub editor: EditorSettings,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// In pixels; the theme's size when unset.
    pub font_size: Option<f32>,
    /// The theme's monospace font when unset.
    pub font_family: Option<String>,
}

impl Settings {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        // An empty file is as good as none.
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(text)
    }

    /// `None` when the size is unset or not a usable number.
    fn font_size(&self) -> Option<f32> {
        let size = self.editor.font_size.filter(|s| s.is_finite())?;
        Some(size.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1))
    }

    fn font_family(&self) -> Option<&str> {
        self.editor
            .font_family
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
    }
}

pub fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE))
}

/// Writes the file with the current values if it doesn't exist yet, so there is
/// something to edit. Returns its path.
pub fn ensure_file(cx: &App) -> Option<PathBuf> {
    let path = path()?;
    if !path.exists() {
        let theme = cx.global::<Theme>();
        let defaults = serde_json::json!({
            "editor": {
                "font_size": theme.mono_font_size.as_f32(),
                "font_family": theme.mono_font_family.to_string(),
            }
        });
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
            std::fs::write(&path, serde_json::to_string_pretty(&defaults)? + "\n")
        };
        if let Err(e) = write() {
            eprintln!("could not create {}: {e}", path.display());
            return None;
        }
    }
    Some(path)
}

/// What the theme had before any setting was applied, so that removing a key
/// brings the default back.
struct ThemeDefaults {
    font_size: Pixels,
    font_family: SharedString,
}

fn apply(settings: &Settings, defaults: &ThemeDefaults, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.mono_font_size = settings.font_size().map_or(defaults.font_size, px);
    theme.mono_font_family = settings
        .font_family()
        .map_or_else(|| defaults.font_family.clone(), |f| f.to_string().into());
    cx.refresh_windows();
}

/// Loads and applies the settings, then re-applies them whenever the file changes.
pub fn init(cx: &mut App) {
    let defaults = {
        let theme = cx.global::<Theme>();
        ThemeDefaults {
            font_size: theme.mono_font_size,
            font_family: theme.mono_font_family.clone(),
        }
    };
    let Some(path) = path() else { return };
    let mut seen = None;
    reload(&path, &mut seen, &defaults, cx);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            cx.update(|cx| reload(&path, &mut seen, &defaults, cx));
        }
    })
    .detach();
}

/// Applies the file if it changed since `seen` (its modification time). A file that
/// can't be parsed keeps the settings that were applied before.
fn reload(
    path: &Path,
    seen: &mut Option<Option<SystemTime>>,
    defaults: &ThemeDefaults,
    cx: &mut App,
) {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if *seen == Some(modified) {
        return;
    }
    *seen = Some(modified);
    // No file: defaults.
    let text = std::fs::read_to_string(path).unwrap_or_default();
    match Settings::parse(&text) {
        Ok(settings) => apply(&settings, defaults, cx),
        Err(e) => eprintln!("ignoring invalid {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_missing_keys_use_defaults() {
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
        assert_eq!(Settings::parse("{}").unwrap(), Settings::default());
        assert_eq!(
            Settings::parse(r#"{"editor": {}}"#).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn reads_the_editor_section() {
        let s =
            Settings::parse(r#"{"editor": {"font_size": 15, "font_family": " Menlo "}}"#).unwrap();
        assert_eq!(s.font_size(), Some(15.));
        assert_eq!(s.font_family(), Some("Menlo"));
    }

    #[test]
    fn font_size_is_limited() {
        let size = |json: &str| Settings::parse(json).unwrap().font_size();
        assert_eq!(size(r#"{"editor": {"font_size": 2}}"#), Some(8.));
        assert_eq!(size(r#"{"editor": {"font_size": 400}}"#), Some(32.));
    }

    #[test]
    fn blank_family_is_unset() {
        let s = Settings::parse(r#"{"editor": {"font_family": "  "}}"#).unwrap();
        assert_eq!(s.font_family(), None);
    }

    #[test]
    fn unknown_keys_and_broken_json() {
        assert!(Settings::parse(r#"{"other": 1}"#).is_ok());
        assert!(Settings::parse(r#"{"editor": "#).is_err());
        assert!(Settings::parse(r#"{"editor": {"font_size": "big"}}"#).is_err());
    }
}
