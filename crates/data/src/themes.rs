//! Color themes as JSON files: every color tangul uses, under semantic names, for one
//! appearance (dark or light). Built-in themes ship with the app; more live in the
//! `themes` folder next to `settings.json`, where tangul also saves them. A file
//! that misses a color doesn't load (the optional `grid` keys are the exception:
//! they fall back to colors derived from `selection` and `accent`). `settings.json`
//! picks one dark and one light theme (`theme.dark`, `theme.light`).
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
/// One dark and one light theme, each also with rainbow charts (the colors of the
/// rainbow, light ones on dark, deep ones on light).
const BUILT_IN: [&str; 4] = [
    include_str!("../themes/tangul-dark.json"),
    include_str!("../themes/tangul-light.json"),
    include_str!("../themes/tangul-dark-rainbow.json"),
    include_str!("../themes/tangul-light-rainbow.json"),
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
    /// How the result grid's mark looks. Optional: users' themes in the `themes` folder
    /// aren't migrated, and a missing required color makes a theme fail to load.
    #[serde(default, skip_serializing_if = "GridColors::is_empty")]
    pub grid: GridColors,
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

/// The widest outline of the grid's mark, and its roundest corners, in pixels.
const MAX_MARK_BORDER: f32 = 8.;
const MAX_MARK_RADIUS: f32 = 8.;

/// The result grid's mark (the cells and rows a selection covers). Every key is
/// optional: what is missing comes from `selection` and `accent` (see `resolve`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridColors {
    /// Highlight of the rows the mark covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<Rgb>,
    /// Fill of the marked cells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Rgb>,
    /// Outline around the mark's outer edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_border: Option<Rgb>,
    /// Width of that outline in pixels, 0 to 8 (0: no outline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_border_width: Option<f32>,
    /// Radius of the mark's outer corners in pixels, 0 to 8 (0: square).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_radius: Option<f32>,
    /// Fill of a marked column's header cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_mark: Option<Rgb>,
}

/// The grid's mark with every value known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub row: Rgb,
    pub mark: Rgb,
    pub mark_border: Rgb,
    pub mark_border_width: f32,
    pub mark_radius: f32,
    pub header_mark: Rgb,
}

impl GridColors {
    /// No key set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The values to draw: the set ones, the rest from `colors`. Rows are the
    /// selection at 35% of its alpha, the mark and its header the selection, the
    /// outline the accent, 2 px wide, its corners rounded by 4 px.
    pub fn resolve(&self, colors: &ThemeColors) -> Grid {
        let selection = colors.selection;
        Grid {
            row: self.row.unwrap_or(Rgb {
                a: (f32::from(selection.a) * 0.35).round() as u8,
                ..selection
            }),
            mark: self.mark.unwrap_or(selection),
            mark_border: self.mark_border.unwrap_or(colors.accent),
            mark_border_width: self.mark_border_width.unwrap_or(2.),
            mark_radius: self.mark_radius.unwrap_or(4.),
            header_mark: self.header_mark.unwrap_or(selection),
        }
    }
}

impl Theme {
    /// A theme from its JSON, checked: every color is there (but for the optional
    /// `grid` keys), `charts` isn't empty and the mark's outline and corners are
    /// 0 to 8 px.
    pub fn parse(text: &str) -> Result<Self, String> {
        let theme: Theme = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if theme.name.trim().is_empty() {
            return Err("the theme has no name".into());
        }
        if theme.charts.is_empty() {
            return Err("\"charts\" needs at least one color".into());
        }
        let sizes = [
            (
                "mark_border_width",
                theme.grid.mark_border_width,
                MAX_MARK_BORDER,
            ),
            ("mark_radius", theme.grid.mark_radius, MAX_MARK_RADIUS),
        ];
        for (key, value, max) in sizes {
            if let Some(value) = value
                && !(0. ..=max).contains(&value)
            {
                return Err(format!(
                    "\"grid.{key}\" is {value}, it has to be between 0 and {max}"
                ));
            }
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

/// The themes that ship with the app.
pub fn built_in() -> Vec<Theme> {
    BUILT_IN
        .iter()
        .map(|text| Theme::parse(text).expect("a built-in theme is valid"))
        .collect()
}

/// The themes chosen until the user picks others.
pub const DEFAULT_DARK: &str = "Tangul Dark Rainbow";
pub const DEFAULT_LIGHT: &str = "Tangul Light Rainbow";

/// The default theme for `mode`.
pub fn default_for(mode: ThemeMode) -> Theme {
    let name = match mode {
        ThemeMode::Dark => DEFAULT_DARK,
        ThemeMode::Light => DEFAULT_LIGHT,
    };
    built_in()
        .into_iter()
        .find(|t| t.name == name && t.mode == mode)
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
    fn dark_and_light_ship_monochrome_and_rainbow() {
        assert_eq!(default_for(ThemeMode::Dark).name, "Tangul Dark Rainbow");
        assert_eq!(default_for(ThemeMode::Light).name, "Tangul Light Rainbow");
        let themes = built_in();
        let named = |name: &str| themes.iter().find(|t| t.name == name).unwrap();
        for (name, mono, mode) in [
            ("Tangul Dark Rainbow", "Tangul Dark", ThemeMode::Dark),
            ("Tangul Light Rainbow", "Tangul Light", ThemeMode::Light),
        ] {
            let rainbow = named(name);
            assert_eq!(rainbow.mode, mode);
            assert_eq!(rainbow.charts.len(), 7);
            // The rest as the monochrome one of its mode.
            assert_eq!(rainbow.colors, named(mono).colors);
            assert_eq!(rainbow.syntax, named(mono).syntax);
        }
        assert!(themes.iter().filter(|t| t.charts.len() == 1).count() == 2);
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
    fn a_theme_without_grid_gets_the_default_mark() {
        let theme = Theme::parse(&strip_grid(BUILT_IN[0])).unwrap();
        assert!(theme.grid.is_empty());
        let rgb = |s: &str| Rgb::parse(s).unwrap();
        // The selection is #e5484d40: rows at 35% of its alpha (0x40 * 0.35 = 22).
        assert_eq!(
            theme.grid.resolve(&theme.colors),
            Grid {
                row: rgb("#e5484d16"),
                mark: rgb("#e5484d40"),
                mark_border: rgb("#e5484d"),
                mark_border_width: 2.,
                mark_radius: 4.,
                header_mark: rgb("#e5484d40"),
            }
        );
        assert!(!theme.to_json().contains("grid"));
    }

    #[test]
    fn a_partial_grid_fills_the_rest_from_the_defaults() {
        let text = strip_grid(BUILT_IN[0]).replace(
            "\"syntax\"",
            "\"grid\": { \"mark\": \"#00ff0080\", \"mark_border_width\": 0 }, \"syntax\"",
        );
        let theme = Theme::parse(&text).unwrap();
        let grid = theme.grid.resolve(&theme.colors);
        assert_eq!(grid.mark.to_hex(), "#00ff0080");
        assert_eq!(grid.mark_border_width, 0.);
        assert_eq!(grid.row.to_hex(), "#e5484d16");
        assert_eq!(grid.mark_border, theme.colors.accent);
        assert_eq!(grid.header_mark, theme.colors.selection);
        // Only what was set is written back.
        let json = theme.to_json();
        assert!(json.contains("\"mark\": \"#00ff0080\""), "{json}");
        assert!(!json.contains("\"row\""), "{json}");
        assert_eq!(Theme::parse(&json).unwrap(), theme);
    }

    #[test]
    fn a_bad_grid_doesnt_load() {
        let with = |grid: &str| {
            strip_grid(BUILT_IN[0])
                .replace("\"syntax\"", &format!("\"grid\": {{ {grid} }}, \"syntax\""))
        };
        let e = Theme::parse(&with("\"glow\": \"#fff\"")).unwrap_err();
        assert!(e.contains("glow"), "{e}");
        for width in ["-1", "8.5", "100"] {
            let e = Theme::parse(&with(&format!("\"mark_border_width\": {width}"))).unwrap_err();
            assert!(e.contains("mark_border_width"), "{e}");
        }
        assert!(Theme::parse(&with("\"mark_border_width\": 8")).is_ok());
        for radius in ["-2", "9"] {
            let e = Theme::parse(&with(&format!("\"mark_radius\": {radius}"))).unwrap_err();
            assert!(e.contains("mark_radius"), "{e}");
        }
        assert!(Theme::parse(&with("\"mark_radius\": 0")).is_ok());
        assert!(Theme::parse(&with("\"mark\": \"red\"")).is_err());
    }

    #[test]
    fn every_built_in_theme_sets_the_whole_mark() {
        for theme in built_in() {
            let g = &theme.grid;
            assert!(
                g.row.is_some()
                    && g.mark.is_some()
                    && g.mark_border.is_some()
                    && g.mark_border_width.is_some()
                    && g.mark_radius.is_some()
                    && g.header_mark.is_some(),
                "{}",
                theme.name
            );
            // Rows are fainter than the mark.
            assert!(g.row.unwrap().a < g.mark.unwrap().a, "{}", theme.name);
            assert_eq!(Theme::parse(&theme.to_json()).unwrap(), theme);
        }
    }

    /// A built-in theme's text without its `grid` section (the last one).
    fn strip_grid(text: &str) -> String {
        let start = text.find("  \"grid\"").unwrap();
        let end = start + text[start..].find("  }").unwrap() + "  }".len();
        // The comma before the section goes with it.
        let before = text[..start].trim_end().trim_end_matches(',');
        format!("{before}{}", &text[end..])
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
        assert_eq!(
            names,
            [
                "My Dark",
                "Tangul Dark",
                "Tangul Dark Rainbow",
                "Tangul Light",
                "Tangul Light Rainbow"
            ]
        );
        assert_eq!(themes[0], mine);
        assert_eq!(themes[2].colors.accent.to_hex(), "#ff0000");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("broken.json"));
        assert_eq!(stamp(&dir).len(), 4);
        // No folder: the built-in ones.
        assert_eq!(load(None).0.len(), BUILT_IN.len());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn colors_round_trip_as_hex() {
        let theme = default_for(ThemeMode::Light);
        assert_eq!(Theme::parse(&theme.to_json()).unwrap(), theme);
        assert!(theme.to_json().contains("\"selection\": \"#dc262633\""));
        assert_eq!(theme.file_name(), "tangul-light-rainbow.json");
    }
}
