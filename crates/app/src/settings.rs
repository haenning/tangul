//! `settings.json` in the config directory: user settings, applied to the theme and
//! reloaded while the app runs.
//!
//! ```json
//! {
//!   "editor": { "font_size": 14, "font_family": "Menlo" },
//!   "table": { "font_size": 12, "font_family": "Menlo" },
//!   "chart": { "primary_color": "#2563eb", "font_size": 10, "font_family": null },
//!   "safety": {
//!     "read_only_users": ["reporting"],
//!     "confirm_unfiltered_writes": true,
//!     "relock_after": "5m"
//!   },
//!   "monitoring": {
//!     "interval": "1d",
//!     "schedule_new_bookmarks": false,
//!     "max_query_time": "60s",
//!     "max_points": 10000,
//!     "max_age": "365d",
//!     "max_mb": 50
//!   }
//! }
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::Theme;
use gpui_kit::{App, Global, Hsla, Pixels, Rgba, SharedString, px};
use serde::{Deserialize, Deserializer};

use crate::store::config_dir;

const FILE: &str = "settings.json";
/// How often the file is checked for changes.
const POLL: Duration = Duration::from_secs(1);
/// A typo can't make the editor unusable.
const FONT_SIZE_RANGE: (f32, f32) = (8., 32.);
/// Grid rows have a fixed height; larger text would be cut off.
const TABLE_FONT_SIZE_RANGE: (f32, f32) = (8., 18.);
/// The chart leaves fixed room for its labels.
const CHART_FONT_SIZE_RANGE: (f32, f32) = (6., 16.);
/// Bounds for how often scheduled bookmarks run.
const MONITOR_INTERVAL_RANGE: (Duration, Duration) =
    (Duration::from_secs(5 * 60), Duration::from_secs(24 * 3600));

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub editor: FontSettings,
    /// The result grid.
    pub table: FontSettings,
    /// Result charts.
    pub chart: ChartSettings,
    pub safety: SafetySettings,
    pub monitoring: MonitoringSettings,
}

/// Scheduled bookmark runs and the metrics kept of bookmark runs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MonitoringSettings {
    /// About how often a scheduled bookmark runs; kept between 5m and 1d.
    #[serde(deserialize_with = "required_duration")]
    pub interval: Duration,
    /// Whether a new bookmark starts out scheduled (if it only reads).
    pub schedule_new_bookmarks: bool,
    /// Longest a scheduled run may take, enforced by the server too; `null` for no
    /// limit.
    #[serde(deserialize_with = "duration")]
    pub max_query_time: Option<Duration>,
    /// Most runs kept per bookmark; `null` for no limit.
    pub max_points: Option<u64>,
    /// Runs older than this are dropped; `null` keeps them.
    #[serde(deserialize_with = "duration")]
    pub max_age: Option<Duration>,
    /// Most space all kept runs may take, in MB; `null` for no limit.
    pub max_mb: Option<f64>,
}

impl Default for MonitoringSettings {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(24 * 3600),
            schedule_new_bookmarks: false,
            max_query_time: Some(Duration::from_secs(60)),
            max_points: Some(10_000),
            max_age: Some(Duration::from_secs(365 * 24 * 3600)),
            max_mb: Some(50.),
        }
    }
}

impl MonitoringSettings {
    /// [`Self::interval`] within its bounds.
    pub fn interval(&self) -> Duration {
        self.interval
            .clamp(MONITOR_INTERVAL_RANGE.0, MONITOR_INTERVAL_RANGE.1)
    }
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
    /// After read-only mode was turned off for a user, turn it back on this long
    /// after the last statement that changed data (or after turning it off, if
    /// none ran). Written like `"30s"`, `"5m"` or `"1h"`; `null` never does.
    #[serde(deserialize_with = "duration")]
    pub relock_after: Option<Duration>,
}

impl Default for SafetySettings {
    fn default() -> Self {
        Self {
            read_only_users: Vec::new(),
            confirm_unfiltered_writes: true,
            relock_after: None,
        }
    }
}

fn required_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    parse_duration(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    Option::<String>::deserialize(d)?
        .map(|text| parse_duration(&text).map_err(serde::de::Error::custom))
        .transpose()
}

/// `"30s"`, `"5m"`, `"1h"`, `"7d"`: a whole number and a unit.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let invalid = || format!("invalid duration {text:?}, expected e.g. \"30s\", \"5m\" or \"1h\"");
    let unit = text.chars().last().ok_or_else(invalid)?;
    let n: u64 = text[..text.len() - unit.len_utf8()]
        .trim()
        .parse()
        .map_err(|_| invalid())?;
    let seconds = match unit {
        's' => n,
        'm' => n * 60,
        'h' => n * 3600,
        'd' => n * 24 * 3600,
        _ => return Err(invalid()),
    };
    Ok(Duration::from_secs(seconds))
}

impl Settings {
    pub fn is_read_only(&self, user: &str) -> bool {
        self.safety.read_only_users.iter().any(|u| u == user)
    }
}

/// Font of one part of the app; unset values keep the default.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct FontSettings {
    /// In pixels.
    pub font_size: Option<f32>,
    pub font_family: Option<String>,
}

impl FontSettings {
    /// `None` when the size is unset or not a usable number.
    pub fn size(&self, (min, max): (f32, f32)) -> Option<f32> {
        let size = self.font_size.filter(|s| s.is_finite())?;
        Some(size.clamp(min, max))
    }

    pub fn family(&self) -> Option<&str> {
        self.font_family
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ChartSettings {
    /// Hex color (`"#2563eb"`) of the first series; the theme's when unset.
    #[serde(deserialize_with = "color")]
    pub primary_color: Option<Hsla>,
    #[serde(flatten)]
    pub font: FontSettings,
}

fn color<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Hsla>, D::Error> {
    Option::<String>::deserialize(d)?
        .map(|text| {
            let text = text.trim();
            Rgba::try_from(text).map(Hsla::from).map_err(|_| {
                serde::de::Error::custom(format!(
                    "invalid color {text:?}, expected e.g. \"#2563eb\""
                ))
            })
        })
        .transpose()
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

    /// The editor's font size; `None` when unset or not a usable number.
    fn font_size(&self) -> Option<f32> {
        self.editor.size(FONT_SIZE_RANGE)
    }

    fn font_family(&self) -> Option<&str> {
        self.editor.family()
    }

    /// The result grid's font size, if set.
    pub fn table_font_size(&self) -> Option<f32> {
        self.table.size(TABLE_FONT_SIZE_RANGE)
    }

    /// The chart labels' font size, if set.
    pub fn chart_font_size(&self) -> Option<f32> {
        self.chart.font.size(CHART_FONT_SIZE_RANGE)
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
            "table": {
                "font_size": null,
                "font_family": null,
            },
            "chart": {
                "primary_color": null,
                "font_size": null,
                "font_family": null,
            },
            "safety": {
                "read_only_users": [],
                "confirm_unfiltered_writes": true,
                "relock_after": null,
            },
            "monitoring": {
                "interval": "1d",
                "schedule_new_bookmarks": false,
                "max_query_time": "60s",
                "max_points": 10000,
                "max_age": "365d",
                "max_mb": 50,
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
    chart_color: Hsla,
}

fn apply(settings: &Settings, defaults: &ThemeDefaults, cx: &mut App) {
    cx.set_global(Current(settings.clone()));
    let theme = Theme::global_mut(cx);
    theme.mono_font_size = settings.font_size().map_or(defaults.font_size, px);
    theme.mono_font_family = settings
        .font_family()
        .map_or_else(|| defaults.font_family.clone(), |f| f.to_string().into());
    theme.chart_1 = settings.chart.primary_color.unwrap_or(defaults.chart_color);
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
            chart_color: theme.chart_1,
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
    fn table_and_chart_sections() {
        let s = Settings::parse(
            r##"{"table": {"font_size": 40, "font_family": "Iosevka"},
                "chart": {"primary_color": "#ff0000", "font_size": 11, "font_family": " "}}"##,
        )
        .unwrap();
        assert_eq!(s.table_font_size(), Some(18.));
        assert_eq!(s.table.family(), Some("Iosevka"));
        assert_eq!(s.chart_font_size(), Some(11.));
        assert_eq!(s.chart.font.family(), None);
        let red = s.chart.primary_color.unwrap();
        assert_eq!((red.h, red.s, red.l), (0., 1., 0.5));
        let s = Settings::parse(r#"{"chart": {"primary_color": null}}"#).unwrap();
        assert_eq!(s.chart.primary_color, None);
        assert!(Settings::parse(r#"{"chart": {"primary_color": "red"}}"#).is_err());
    }

    #[test]
    fn relock_after() {
        let after = |json: &str| Settings::parse(json).map(|s| s.safety.relock_after);
        assert_eq!(after("{}").unwrap(), None);
        assert_eq!(
            after(r#"{"safety": {"relock_after": null}}"#).unwrap(),
            None
        );
        let secs = |text: &str| parse_duration(text).map(|d| d.as_secs());
        assert_eq!(secs("2s"), Ok(2));
        assert_eq!(secs(" 5m "), Ok(300));
        assert_eq!(secs("1h"), Ok(3600));
        assert_eq!(
            after(r#"{"safety": {"relock_after": "5m"}}"#).unwrap(),
            Some(Duration::from_secs(300))
        );
        assert_eq!(secs("2d"), Ok(2 * 24 * 3600));
        for bad in ["", "5", "m", "5w", "-1s", "1.5m", "5ü"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
        assert!(after(r#"{"safety": {"relock_after": "soon"}}"#).is_err());
    }

    #[test]
    fn monitoring_section() {
        let m = Settings::parse("{}").unwrap().monitoring;
        assert_eq!(m, MonitoringSettings::default());
        assert_eq!(m.max_query_time, Some(Duration::from_secs(60)));
        let m = Settings::parse(
            r#"{"monitoring": {"interval": "1m", "schedule_new_bookmarks": true,
                "max_query_time": null, "max_points": null, "max_age": "30d", "max_mb": 1.5}}"#,
        )
        .unwrap()
        .monitoring;
        // Kept between 5 minutes and a day.
        assert_eq!(m.interval(), Duration::from_secs(300));
        assert!(m.schedule_new_bookmarks);
        assert_eq!((m.max_query_time, m.max_points), (None, None));
        assert_eq!(m.max_age, Some(Duration::from_secs(30 * 24 * 3600)));
        assert_eq!(m.max_mb, Some(1.5));
        let week = r#"{"monitoring": {"interval": "7d"}}"#;
        assert_eq!(
            Settings::parse(week).unwrap().monitoring.interval(),
            Duration::from_secs(24 * 3600)
        );
        assert!(Settings::parse(r#"{"monitoring": {"interval": null}}"#).is_err());
    }

    #[test]
    fn unknown_keys_and_broken_json() {
        assert!(Settings::parse(r#"{"other": 1}"#).is_ok());
        assert!(Settings::parse(r#"{"editor": "#).is_err());
        assert!(Settings::parse(r#"{"editor": {"font_size": "big"}}"#).is_err());
    }
}
