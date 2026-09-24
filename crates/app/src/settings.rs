//! `settings.json` in the config directory: user settings, applied to the theme and
//! reloaded while the app runs.
//!
//! ```json
//! {
//!   "editor": { "font_size": 14, "font_family": "Menlo" },
//!   "safety": { "read_only_users": ["reporting"], "confirm_unfiltered_writes": true }
//! }
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::Theme;
use gpui_kit::{App, Global, Pixels, SharedString, px};
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
    pub safety: SafetySettings,
}

/// The settings in effect, as a global so any view can read them.
struct Current(Settings);
impl Global for Current {}

/// The settings in effect.
pub fn current(cx: &App) -> &Settings {
    &cx.global::<Current>().0
}

/// Guardrails against running the wrong statement.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct SafetySettings {
    /// Database user names in read-only mode: statements that change data ask for
    /// confirmation first. Matched against the user name of the connection.
    pub read_only_users: Vec<String>,
    /// Ask before running an `UPDATE` or `DELETE` without a `WHERE`.
    pub confirm_unfiltered_writes: bool,
}

impl Default for SafetySettings {
    fn default() -> Self {
        Self {
            read_only_users: Vec::new(),
            confirm_unfiltered_writes: true,
        }
    }
}

impl Settings {
    pub fn is_read_only(&self, user: &str) -> bool {
        self.safety.read_only_users.iter().any(|u| u == user)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// In pixels; the theme's size when unset.
    pub font_size: Option<f32>,
    /// The theme's monospace font when unset.
    pub font_family: Option<String>,
}

/// Why `text` can't be applied as settings, if it can't.
pub fn problem(text: &str) -> Option<String> {
    Settings::parse(text).err().map(|e| e.to_string())
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
            },
            "safety": {
                "read_only_users": [],
                "confirm_unfiltered_writes": true,
            },
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

/// Puts `user` in or takes it out of `safety.read_only_users` in the file, keeping
/// everything else in it, and applies the change right away.
pub fn set_read_only(user: &str, on: bool, cx: &mut App) -> Result<(), String> {
    let path = ensure_file(cx).ok_or("could not create the settings file")?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let updated = with_read_only(&text, user, on)?;
    std::fs::write(&path, updated).map_err(|e| e.to_string())?;
    let settings = Settings::parse(&std::fs::read_to_string(&path).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    cx.set_global(Current(settings));
    cx.refresh_windows();
    Ok(())
}

/// `text` (the settings file) with `user` added to or removed from the read-only list.
fn with_read_only(text: &str, user: &str, on: bool) -> Result<String, String> {
    let mut root: serde_json::Value = if text.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(text).map_err(|e| format!("settings.json is not valid JSON: {e}"))?
    };
    let root_object = root
        .as_object_mut()
        .ok_or("settings.json must contain a JSON object")?;
    let safety = root_object
        .entry("safety")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("\"safety\" in settings.json must be an object")?;
    let users = safety
        .entry("read_only_users")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or("\"safety.read_only_users\" in settings.json must be an array")?;
    users.retain(|u| u.as_str() != Some(user));
    if on {
        users.push(user.into());
    }
    Ok(serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n")
}

/// What the theme had before any setting was applied, so that removing a key
/// brings the default back.
struct ThemeDefaults {
    font_size: Pixels,
    font_family: SharedString,
}

fn apply(settings: &Settings, defaults: &ThemeDefaults, cx: &mut App) {
    cx.set_global(Current(settings.clone()));
    let theme = Theme::global_mut(cx);
    theme.mono_font_size = settings.font_size().map_or(defaults.font_size, px);
    theme.mono_font_family = settings
        .font_family()
        .map_or_else(|| defaults.font_family.clone(), |f| f.to_string().into());
    cx.refresh_windows();
}

/// Loads and applies the settings, then re-applies them whenever the file changes.
pub fn init(cx: &mut App) {
    cx.set_global(Current(Settings::default()));
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
    fn safety_defaults_and_read_only_users() {
        let s = Settings::parse("{}").unwrap();
        assert!(s.safety.confirm_unfiltered_writes);
        assert!(!s.is_read_only("reporting"));
        let s = Settings::parse(
            r#"{"safety": {"read_only_users": ["reporting"], "confirm_unfiltered_writes": false}}"#,
        )
        .unwrap();
        assert!(s.is_read_only("reporting"));
        assert!(!s.is_read_only("admin"));
        assert!(!s.safety.confirm_unfiltered_writes);
        // Only the list given; the flag keeps its default.
        let s = Settings::parse(r#"{"safety": {"read_only_users": ["a"]}}"#).unwrap();
        assert!(s.safety.confirm_unfiltered_writes);
    }

    #[test]
    fn toggling_read_only_keeps_the_rest_of_the_file() {
        let text =
            r#"{"editor": {"font_size": 15}, "safety": {"confirm_unfiltered_writes": false}}"#;
        let on = with_read_only(text, "reporting", true).unwrap();
        let s = Settings::parse(&on).unwrap();
        assert!(s.is_read_only("reporting"));
        assert_eq!(s.editor.font_size, Some(15.));
        assert!(!s.safety.confirm_unfiltered_writes);
        // Twice is still once.
        let again = with_read_only(&on, "reporting", true).unwrap();
        assert_eq!(
            Settings::parse(&again)
                .unwrap()
                .safety
                .read_only_users
                .len(),
            1
        );
        let off = with_read_only(&on, "reporting", false).unwrap();
        assert!(!Settings::parse(&off).unwrap().is_read_only("reporting"));
        // From nothing.
        assert!(
            Settings::parse(&with_read_only("", "u", true).unwrap())
                .unwrap()
                .is_read_only("u")
        );
        assert!(with_read_only("{broken", "u", true).is_err());
        assert!(with_read_only("[]", "u", true).is_err());
    }

    #[test]
    fn unknown_keys_and_broken_json() {
        assert!(Settings::parse(r#"{"other": 1}"#).is_ok());
        assert!(Settings::parse(r#"{"editor": "#).is_err());
        assert!(Settings::parse(r#"{"editor": {"font_size": "big"}}"#).is_err());
    }
}
