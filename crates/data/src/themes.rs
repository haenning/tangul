//! Color themes as JSON files: every color tangul uses, under semantic names, for one
//! appearance (dark or light). Built-in themes ship with the app; more live in the
//! `themes` folder next to `settings.json`, where tangul also saves them. A file
//! that misses a color doesn't load. `settings.json` picks one dark and one light
//! theme (`theme.dark`, `theme.light`).
//!
//! ```json
//! {
//!   "name": "Tangul Dark",
//!   "mode": "dark",
//!   "colors": { "background": "#0a0a0a", "foreground": "#fafafa", "accent": "#e5484d", … },
//!   "charts": ["#e5484d"],
//!   "syntax": { "keyword": "#87b1f6", … }
//! }
//! ```
//!
//! `charts` is one color (monochrome: shades of it) or several (one per series, in
//! turn).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::settings::Rgb;

const DIR: &str = "themes";
const BUILT_IN: [&str; 2] = [
    include_str!("../themes/tangul-dark.json"),
    include_str!("../themes/tangul-light.json"),
];

/// Which appearance a theme is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    pub mode: ThemeMode,
    pub colors: ThemeColors,
    /// One color: every series in shades of it. Several: one per series, in turn.
    pub charts: Vec<Rgb>,
    pub syntax: SyntaxColors,
}

/// Every color of the app. All are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeColors {
    /// The window, the editor, the grid.
    pub background: Rgb,
    /// Sidebars, title and status bar, the tab bar, table heads.
    pub surface: Rgb,
    /// Popovers, menus, dialogs.
    pub raised: Rgb,
    /// Under the pointer (list rows, menu items, ghost buttons).
    pub hover: Rgb,
    /// Pressed or chosen (list rows, segments).
    pub active: Rgb,
    pub border: Rgb,
    /// Inputs and other controls.
    pub input_border: Rgb,
    /// Text.
    pub foreground: Rgb,
    /// Secondary text: labels, hints, placeholders.
    pub muted: Rgb,
    /// Least important text: line numbers, scrollbars.
    pub subtle: Rgb,
    /// The brand color: primary buttons, focus, links, the first chart color.
    pub accent: Rgb,
    /// Text on the accent.
    pub accent_foreground: Rgb,
    /// Selected text and rows (usually the accent, translucent).
    pub selection: Rgb,
    /// Errors, write mode, destructive actions.
    pub danger: Rgb,
    /// Warnings, read-only mode.
    pub warning: Rgb,
    pub success: Rgb,
    pub info: Rgb,
}

/// SQL highlighting in the editor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxColors {
    pub keyword: Rgb,
    pub string: Rgb,
    /// Numbers, booleans and other constants.
    pub number: Rgb,
    pub comment: Rgb,
    pub function: Rgb,
    pub r#type: Rgb,
    /// Columns, attributes, variables.
    pub property: Rgb,
    pub operator: Rgb,
    pub punctuation: Rgb,
}

impl Theme {
    /// A theme from its JSON, checked: every color is there and `charts` isn't
    /// empty.
    pub fn parse(text: &str) -> Result<Self, String> {
        let theme: Theme = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if theme.name.trim().is_empty() {
            return Err("the theme has no name".into());
        }
        if theme.charts.is_empty() {
            return Err("\"charts\" needs at least one color".into());
        }
        Ok(theme)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default() + "\n"
    }

    /// The file name it is saved under: its name in lower case, words joined by
    /// `-`.
    pub fn file_name(&self) -> String {
        let slug: Vec<String> = self
            .name
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        format!("{}.json", slug.join("-"))
    }
}

/// The themes that ship with the app: one dark, one light.
pub fn built_in() -> Vec<Theme> {
    BUILT_IN
        .iter()
        .map(|text| Theme::parse(text).expect("a built-in theme is valid"))
        .collect()
}

/// The built-in theme for `mode`.
pub fn default_for(mode: ThemeMode) -> Theme {
    built_in()
        .into_iter()
        .find(|t| t.mode == mode)
        .expect("a built-in theme per mode")
}

/// The folder of the user's themes, next to `settings.json`.
pub fn dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join(DIR))
}

/// The themes on offer: the built-in ones and those in `dir` (a file overrides a
/// built-in theme of the same name), sorted by name; and why files didn't load.
pub fn load(dir: Option<&Path>) -> (Vec<Theme>, Vec<String>) {
    let mut themes = built_in();
    let mut problems = Vec::new();
    let mut files: Vec<PathBuf> = dir
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    for path in files {
        let loaded = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| Theme::parse(&text));
        match loaded {
            Ok(theme) => {
                themes.retain(|t| t.name != theme.name);
                themes.push(theme);
            }
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    (themes, problems)
}

/// Saves `theme` into `dir` (created if needed); the file's path.
pub fn save(dir: &Path, theme: &Theme) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(theme.file_name());
    std::fs::write(&path, theme.to_json()).map_err(|e| e.to_string())?;
    Ok(path)
}

/// What changed in `dir` since the last look: its files and when they were
/// written. The app reloads the themes when it differs.
pub fn stamp(dir: &Path) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    let mut stamp: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| (e.path(), e.metadata().and_then(|m| m.modified()).ok()))
        .collect();
    stamp.sort();
    stamp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG AA for normal text.
    const TEXT: f64 = 4.5;
    /// WCAG AA for large text and graphics (chart lines, icons).
    const GRAPHICS: f64 = 3.;

    #[test]
    fn built_in_themes_are_readable() {
        for theme in built_in() {
            let c = &theme.colors;
            for (name, surface) in [
                ("background", c.background),
                ("surface", c.surface),
                ("raised", c.raised),
                ("hover", c.hover),
            ] {
                for (text, color, min) in [
                    ("foreground", c.foreground, 7.),
                    ("muted", c.muted, TEXT),
                    ("subtle", c.subtle, GRAPHICS),
                ] {
                    let ratio = color.contrast(surface);
                    assert!(
                        ratio >= min,
                        "{}: {text} on {name} is {ratio:.2}:1",
                        theme.name
                    );
                }
            }
            for (name, color) in [
                ("danger", c.danger),
                ("warning", c.warning),
                ("success", c.success),
                ("info", c.info),
                ("accent", c.accent),
            ] {
                let ratio = color.contrast(c.background);
                assert!(ratio >= GRAPHICS, "{}: {name} is {ratio:.2}:1", theme.name);
            }
            assert!(c.accent_foreground.contrast(c.accent) >= GRAPHICS);
            for color in &theme.charts {
                assert!(color.contrast(c.background) >= GRAPHICS, "{}", theme.name);
            }
            for (name, color) in [
                ("keyword", theme.syntax.keyword),
                ("string", theme.syntax.string),
                ("comment", theme.syntax.comment),
            ] {
                let ratio = color.contrast(c.background);
                assert!(ratio >= TEXT, "{}: {name} is {ratio:.2}:1", theme.name);
            }
        }
    }

    #[test]
    fn one_dark_and_one_light_ship() {
        assert_eq!(default_for(ThemeMode::Dark).name, "Tangul Dark");
        assert_eq!(default_for(ThemeMode::Light).name, "Tangul Light");
    }

    #[test]
    fn a_theme_missing_a_color_doesnt_load() {
        let text = BUILT_IN[0].replace("\"success\": \"#4ade80\",", "");
        let e = Theme::parse(&text).unwrap_err();
        assert!(e.contains("success"), "{e}");
        let bad = BUILT_IN[0].replace("\"#4ade80\"", "\"green\"");
        assert!(Theme::parse(&bad).unwrap_err().contains("invalid color"));
        let none = BUILT_IN[0].replace("\"charts\": [\"#e5484d\"]", "\"charts\": []");
        assert!(Theme::parse(&none).unwrap_err().contains("charts"));
        let unknown = BUILT_IN[0].replace("\"colors\": {", "\"colors\": { \"glow\": \"#fff\",");
        assert!(Theme::parse(&unknown).unwrap_err().contains("glow"));
        let nameless = BUILT_IN[0].replace("\"Tangul Dark\"", "\" \"");
        assert!(Theme::parse(&nameless).is_err());
    }

    #[test]
    fn themes_are_saved_and_loaded_from_the_folder() {
        let dir = std::env::temp_dir().join(format!("tangul-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut mine = default_for(ThemeMode::Dark);
        mine.name = "My Dark".into();
        mine.charts = vec![mine.colors.accent, mine.colors.info, mine.colors.success];
        let path = save(&dir, &mine).unwrap();
        assert!(path.ends_with("my-dark.json"));
        // A changed built-in theme overrides it.
        let mut dark = default_for(ThemeMode::Dark);
        dark.colors.accent = Rgb::parse("#ff0000").unwrap();
        save(&dir, &dark).unwrap();
        std::fs::write(dir.join("broken.json"), "{").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a theme").unwrap();
        let (themes, problems) = load(Some(&dir));
        let names: Vec<_> = themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["My Dark", "Tangul Dark", "Tangul Light"]);
        assert_eq!(themes[0], mine);
        assert_eq!(themes[1].colors.accent.to_hex(), "#ff0000");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("broken.json"));
        assert_eq!(stamp(&dir).len(), 4);
        // No folder: the built-in ones.
        assert_eq!(load(None).0.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn colors_round_trip_as_hex() {
        let theme = default_for(ThemeMode::Light);
        assert_eq!(Theme::parse(&theme.to_json()).unwrap(), theme);
        assert!(theme.to_json().contains("\"selection\": \"#dc262633\""));
        assert_eq!(theme.file_name(), "tangul-light.json");
    }
}
