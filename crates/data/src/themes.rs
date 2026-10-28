//! Color themes as JSON files: every color tangul uses, under semantic names, for one
//! appearance (dark or light). Built-in themes ship with the app; more live in the
//! `themes` folder next to `settings.json`, where tangul also saves them.
//! `settings.json` picks one dark and one light theme (`theme.dark`, `theme.light`).
//!
//! The user's files are read leniently ([`Theme::read`]): what is missing or can't be
//! used comes from the rainbow theme of its mode (Tangul Dark Rainbow or Tangul
//! Light Rainbow), each with a [`Note`]; a file without a mode takes the mode it is
//! used for. Only a file that isn't a JSON object doesn't load. The built-in themes
//! are complete ([`Theme::parse`]).
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

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    /// How the result grid's mark looks. Optional: what is missing is derived from
    /// `selection` and `accent`.
    #[serde(default, skip_serializing_if = "GridColors::is_empty")]
    pub grid: GridColors,
    /// The file's text when it names no mode: read again for the mode it is used
    /// for ([`Theme::for_mode`]).
    #[serde(skip)]
    modeless: Option<String>,
}

/// Something in a theme file that can't be used, and what is used instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The keys down to what it is about; empty for the file as a whole.
    pub path: Vec<String>,
    /// About the key itself (one that is unknown), not its value.
    pub at_key: bool,
    pub message: String,
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "\"{}\": {}", self.path.join("."), self.message)
        }
    }
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
    /// Fill of the inspected cell (A), and of the A label in the value viewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspected: Option<Rgb>,
    /// Text on that fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspected_foreground: Option<Rgb>,
    /// Fill of the compared cell (B), and of the B label in the value viewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compared: Option<Rgb>,
    /// Text on that fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compared_foreground: Option<Rgb>,
    /// The `NULL` shown in cells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub null: Option<Rgb>,
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
    pub inspected: Rgb,
    pub inspected_foreground: Rgb,
    pub compared: Rgb,
    pub compared_foreground: Rgb,
    pub null: Rgb,
}

impl GridColors {
    /// No key set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The values to draw: the set ones, the rest from `colors`. Rows are the
    /// selection at 35% of its alpha, the mark and its header the selection, the
    /// outline the accent, 2 px wide, its corners rounded by 4 px. A and B are a
    /// faint `info` and `warning` (12%) under the usual text, `NULL` is `muted`.
    pub fn resolve(&self, colors: &ThemeColors) -> Grid {
        let selection = colors.selection;
        let faint = |c: Rgb| Rgb { a: 31, ..c };
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
            inspected: self.inspected.unwrap_or(faint(colors.info)),
            inspected_foreground: self.inspected_foreground.unwrap_or(colors.foreground),
            compared: self.compared.unwrap_or(faint(colors.warning)),
            compared_foreground: self.compared_foreground.unwrap_or(colors.foreground),
            null: self.null.unwrap_or(colors.muted),
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

    /// A user's theme from its JSON, as much of it as can be used: every color that is
    /// missing or isn't one, an empty `charts`, a `grid` value out of range come from
    /// the rainbow theme of its mode, unknown keys are left out, each with a note.
    /// Without a valid `mode` it takes `mode` (the one it is used for; dark when not
    /// known yet), without a name `name`. Only a text that isn't a JSON object fails.
    pub fn read(
        text: &str,
        mode: Option<ThemeMode>,
        name: &str,
    ) -> Result<(Theme, Vec<Note>), String> {
        let root: Value = if text.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(text).map_err(|e| e.to_string())?
        };
        let Value::Object(file) = root else {
            return Err("the theme must be a JSON object ({ … })".into());
        };
        let mut notes = Notes::default();
        let given = match file.get("mode") {
            None => {
                notes.value(
                    &[],
                    "no \"mode\" (\"dark\" or \"light\"): it takes the mode it is used for",
                );
                None
            }
            Some(v) => {
                let mode = serde_json::from_value::<ThemeMode>(v.clone()).ok();
                if mode.is_none() {
                    notes.value(
                        &["mode"],
                        "is \"dark\" or \"light\"; the mode it is used for is taken",
                    );
                }
                mode
            }
        };
        let base = default_for(given.or(mode).unwrap_or(ThemeMode::Dark));
        let from = format!("{}'s is used", base.name);
        let base_value = serde_json::to_value(&base).map_err(|e| e.to_string())?;
        let mut out = Map::new();

        let named = file
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty());
        if named.is_none() {
            notes.value(&["name"], "missing; the file's name is used");
        }
        out.insert("name".into(), named.unwrap_or(name).into());
        out.insert(
            "mode".into(),
            serde_json::to_value(base.mode).unwrap_or_default(),
        );

        for section in ["colors", "syntax"] {
            let colors = base_value[section].as_object().cloned().unwrap_or_default();
            let set = file.get(section).and_then(Value::as_object);
            if set.is_none() {
                notes.value(
                    &[section],
                    &format!("missing; every color is {}", from.replace(" is used", "")),
                );
            }
            let mut merged = Map::new();
            let mut missing = Vec::new();
            for (key, fallback) in colors {
                let value = set.and_then(|s| s.get(&key));
                let color = value.and_then(Value::as_str).and_then(Rgb::parse);
                match (value, color) {
                    (_, Some(c)) => {
                        merged.insert(key, c.to_hex().into());
                    }
                    (Some(_), None) => {
                        notes.value(
                            &[section, &key],
                            &format!("isn't a color (\"#rrggbb\" or \"#rrggbbaa\"); {from}"),
                        );
                        merged.insert(key, fallback);
                    }
                    (None, None) => {
                        missing.push(format!("\"{key}\""));
                        merged.insert(key, fallback);
                    }
                }
            }
            // One note for what the section misses, at its key.
            if set.is_some() {
                match &missing[..] {
                    [] => {}
                    [key] => notes.key(&[section], &format!("{key} is missing; {from}")),
                    keys => notes.key(
                        &[section],
                        &format!("{} are missing; {}'s are used", keys.join(", "), base.name),
                    ),
                }
            }
            for key in set.into_iter().flat_map(Map::keys) {
                if !merged.contains_key(key) {
                    notes.unknown(&[section, key]);
                }
            }
            out.insert(section.into(), Value::Object(merged));
        }

        let charts: Vec<Value> = match file.get("charts") {
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let color = item.as_str().and_then(Rgb::parse);
                    if color.is_none() {
                        notes.value(&["charts", &i.to_string()], "isn't a color; left out");
                    }
                    color.map(|c| c.to_hex().into())
                })
                .collect(),
            Some(_) => {
                notes.value(&["charts"], "is a list of colors");
                Vec::new()
            }
            None => Vec::new(),
        };
        if charts.is_empty() {
            notes.value(&["charts"], &format!("has no color; {from}"));
            out.insert("charts".into(), base_value["charts"].clone());
        } else {
            out.insert("charts".into(), Value::Array(charts));
        }

        let mut grid = Map::new();
        match file.get("grid") {
            None => {}
            Some(Value::Object(set)) => {
                for (key, value) in set {
                    let size = match key.as_str() {
                        "mark_border_width" => Some(MAX_MARK_BORDER),
                        "mark_radius" => Some(MAX_MARK_RADIUS),
                        "row"
                        | "mark"
                        | "mark_border"
                        | "header_mark"
                        | "inspected"
                        | "inspected_foreground"
                        | "compared"
                        | "compared_foreground"
                        | "null" => None,
                        _ => {
                            notes.unknown(&["grid", key]);
                            continue;
                        }
                    };
                    let usable = match size {
                        Some(max) => value
                            .as_f64()
                            .filter(|v| (0. ..=f64::from(max)).contains(v))
                            .map(Value::from),
                        None => value
                            .as_str()
                            .and_then(Rgb::parse)
                            .map(|c| c.to_hex().into()),
                    };
                    match (usable, size) {
                        (Some(v), _) => {
                            grid.insert(key.clone(), v);
                        }
                        (None, Some(max)) => notes.value(
                            &["grid", key],
                            &format!("is a number from 0 to {max}; the default is used"),
                        ),
                        (None, None) => notes.value(
                            &["grid", key],
                            "isn't a color; the default (from \"selection\" and \"accent\") is used",
                        ),
                    }
                }
            }
            Some(_) => notes.value(&["grid"], "is an object; the default mark is used"),
        }
        out.insert("grid".into(), Value::Object(grid));

        for key in file.keys() {
            if !["name", "mode", "colors", "charts", "syntax", "grid"].contains(&key.as_str()) {
                notes.unknown(&[key]);
            }
        }
        let mut theme: Theme =
            serde_json::from_value(Value::Object(out)).map_err(|e| e.to_string())?;
        if given.is_none() {
            theme.modeless = Some(text.to_string());
        }
        Ok((theme, notes.0))
    }

    /// This theme for `mode`: itself when it is of that mode, read again with that
    /// mode's defaults when its file names no mode, `None` when it is of the other.
    pub fn for_mode(&self, mode: ThemeMode) -> Option<Theme> {
        if self.mode == mode {
            return Some(self.clone());
        }
        let text = self.modeless.as_deref()?;
        Theme::read(text, Some(mode), &self.name)
            .ok()
            .map(|(t, _)| t)
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

/// What [`Theme::read`] notes.
#[derive(Default)]
struct Notes(Vec<Note>);

impl Notes {
    fn value(&mut self, path: &[&str], message: &str) {
        self.0.push(Note {
            path: path.iter().map(|k| (*k).to_string()).collect(),
            at_key: false,
            message: message.to_string(),
        });
    }

    /// About the key at `path` (what its object misses).
    fn key(&mut self, path: &[&str], message: &str) {
        self.0.push(Note {
            path: path.iter().map(|k| (*k).to_string()).collect(),
            at_key: true,
            message: message.to_string(),
        });
    }

    fn unknown(&mut self, path: &[&str]) {
        self.0.push(Note {
            path: path.iter().map(|k| (*k).to_string()).collect(),
            at_key: true,
            message: "unknown key, not used".into(),
        });
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
/// built-in theme of the same name), sorted by name; and why files didn't load or
/// what in them is replaced by defaults.
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
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let loaded = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| Theme::read(&text, None, &stem));
        match loaded {
            Ok((theme, notes)) => {
                problems.extend(notes.iter().map(|n| format!("{}: {n}", path.display())));
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

    fn read(text: &str) -> (Theme, Vec<String>) {
        let (theme, notes) = Theme::read(text, None, "file").unwrap();
        (theme, notes.iter().map(Note::to_string).collect())
    }

    #[test]
    fn a_complete_theme_reads_without_notes() {
        for (text, theme) in BUILT_IN.iter().zip(built_in()) {
            assert_eq!(read(text), (theme, Vec::new()));
        }
    }

    #[test]
    fn what_cant_be_used_comes_from_the_rainbow_theme_of_its_mode() {
        let dark_rainbow = default_for(ThemeMode::Dark);
        // Tangul Dark without "success", with a color that isn't one, an unknown key.
        let text = BUILT_IN[0]
            .replace("\"success\": \"#4ade80\",", "")
            .replace("\"info\": \"#22d3ee\"", "\"info\": \"blue\"")
            .replace("\"colors\": {", "\"colors\": { \"glow\": \"#fff\",");
        let (theme, notes) = read(&text);
        assert_eq!(theme.name, "Tangul Dark");
        assert_eq!(theme.colors.success, dark_rainbow.colors.success);
        assert_eq!(theme.colors.info, dark_rainbow.colors.info);
        // What is there stays.
        assert_eq!(
            theme.colors.accent,
            default_for_name("Tangul Dark").colors.accent
        );
        assert_eq!(
            notes,
            [
                "\"colors.info\": isn't a color (\"#rrggbb\" or \"#rrggbbaa\"); Tangul Dark Rainbow's is used",
                "\"colors\": \"success\" is missing; Tangul Dark Rainbow's is used",
                "\"colors.glow\": unknown key, not used",
            ]
        );
        // A light theme: from Tangul Light Rainbow.
        let light = BUILT_IN[1].replace("\"success\": \"#15803d\",", "");
        assert!(
            light.len() < BUILT_IN[1].len(),
            "the color is in the light theme"
        );
        let (theme, _) = read(&light);
        assert_eq!(
            theme.colors.success,
            default_for(ThemeMode::Light).colors.success
        );
    }

    #[test]
    fn charts_grid_name_and_extra_keys_fall_back_too() {
        let text = BUILT_IN[0]
            .replace("\"charts\": [\"#e5484d\"]", "\"charts\": []")
            .replace("\"name\": \"Tangul Dark\",", "\"look\": 1,");
        let (theme, notes) = read(&text);
        assert_eq!(theme.charts, default_for(ThemeMode::Dark).charts);
        assert_eq!(theme.name, "file");
        assert_eq!(
            notes,
            [
                "\"name\": missing; the file's name is used",
                "\"charts\": has no color; Tangul Dark Rainbow's is used",
                "\"look\": unknown key, not used",
            ]
        );
        // Chart colors that aren't one are left out.
        let text = BUILT_IN[0].replace(
            "\"charts\": [\"#e5484d\"]",
            "\"charts\": [\"red\", \"#00ff00\"]",
        );
        let (theme, notes) = read(&text);
        assert_eq!(theme.charts, [Rgb::parse("#00ff00").unwrap()]);
        assert_eq!(notes, ["\"charts.0\": isn't a color; left out"]);
        // The grid: what can't be used is derived like a missing value.
        let text = strip_grid(BUILT_IN[0]).replace(
            "\"syntax\"",
            "\"grid\": { \"mark\": \"#00ff0080\", \"mark_radius\": 30, \"row\": 3, \"x\": 1 }, \"syntax\"",
        );
        let (theme, notes) = read(&text);
        assert_eq!(theme.grid.mark.unwrap().to_hex(), "#00ff0080");
        assert_eq!((theme.grid.mark_radius, theme.grid.row), (None, None));
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("grid.mark_radius")));
    }

    #[test]
    fn a_theme_without_a_mode_takes_the_one_it_is_used_for() {
        // Only an accent, nothing else: everything else from the rainbow themes.
        let text = r##"{ "name": "Accent", "colors": { "accent": "#123456" } }"##;
        let (dark, notes) = read(text);
        assert_eq!(dark.mode, ThemeMode::Dark);
        assert_eq!(dark.colors.accent.to_hex(), "#123456");
        assert_eq!(
            dark.colors.background,
            default_for(ThemeMode::Dark).colors.background
        );
        assert!(notes[0].starts_with("no \"mode\""), "{notes:?}");
        let light = dark.for_mode(ThemeMode::Light).unwrap();
        assert_eq!(light.mode, ThemeMode::Light);
        assert_eq!(light.colors.accent.to_hex(), "#123456");
        assert_eq!(
            light.colors.background,
            default_for(ThemeMode::Light).colors.background
        );
        assert_eq!(dark.for_mode(ThemeMode::Dark).unwrap(), dark);
        // A theme with a mode stays of that mode only.
        assert_eq!(
            default_for(ThemeMode::Dark).for_mode(ThemeMode::Light),
            None
        );
        // An invalid mode is like none.
        let (theme, notes) = read(r#"{ "mode": "dim" }"#);
        assert!(theme.for_mode(ThemeMode::Light).is_some());
        assert!(
            notes[0].starts_with("\"mode\": is \"dark\" or \"light\""),
            "{notes:?}"
        );
        // Even an empty file is a theme; only one that isn't a JSON object fails.
        assert_eq!(read("").0.colors, default_for(ThemeMode::Dark).colors);
        assert!(Theme::read("[1]", None, "x").is_err());
        assert!(Theme::read("{", None, "x").is_err());
    }

    #[test]
    fn a_partial_theme_in_the_folder_loads_with_notes() {
        let dir = std::env::temp_dir().join(format!("tangul-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("paper.json"),
            r##"{ "mode": "light", "colors": { "background": "#f4f1ea" } }"##,
        )
        .unwrap();
        let (themes, problems) = load(Some(&dir));
        let paper = themes.iter().find(|t| t.name == "paper").unwrap();
        assert_eq!(paper.colors.background.to_hex(), "#f4f1ea");
        assert_eq!(
            paper.colors.accent,
            default_for(ThemeMode::Light).colors.accent
        );
        assert!(
            problems.iter().all(|p| p.contains("paper.json")),
            "{problems:?}"
        );
        // What a section misses in one note.
        let missing: Vec<_> = problems
            .iter()
            .filter(|p| p.contains("are missing"))
            .collect();
        assert_eq!(missing.len(), 1, "{problems:?}");
        assert!(missing[0].contains("\"accent\""), "{problems:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn default_for_name(name: &str) -> Theme {
        built_in().into_iter().find(|t| t.name == name).unwrap()
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
                // Today's A and B: a faint info and warning under the usual text.
                inspected: rgb("#22d3ee1f"),
                inspected_foreground: theme.colors.foreground,
                compared: rgb("#facc151f"),
                compared_foreground: theme.colors.foreground,
                null: theme.colors.muted,
            }
        );
        assert!(!theme.to_json().contains("grid"));
    }

    /// WCAG's contrast ratio of `fg` on `bg` (opaque).
    fn contrast(fg: Rgb, bg: Rgb) -> f32 {
        let lum = |c: Rgb| {
            let ch = |v: u8| {
                let v = f32::from(v) / 255.;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
        };
        let (a, b) = (lum(fg), lum(bg));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn built_in_themes_fill_a_and_b_with_readable_text() {
        for theme in built_in() {
            let g = &theme.grid;
            let (a, af) = (g.inspected.unwrap(), g.inspected_foreground.unwrap());
            let (b, bf) = (g.compared.unwrap(), g.compared_foreground.unwrap());
            let null = g.null.unwrap();
            // Solid fills, so A and B stand out from the mark and the stripes.
            assert_eq!((a.a, b.a), (255, 255), "{}", theme.name);
            assert!(
                contrast(af, a) >= 4.5,
                "{}: A {}",
                theme.name,
                contrast(af, a)
            );
            assert!(
                contrast(bf, b) >= 4.5,
                "{}: B {}",
                theme.name,
                contrast(bf, b)
            );
            let bg = theme.colors.background;
            assert!(contrast(null, bg) >= 4.5, "{}: NULL", theme.name);
            // Each fill stands out from the background, and A from B.
            assert!(contrast(a, bg) >= 3., "{}: A on the background", theme.name);
            assert!(contrast(b, bg) >= 3., "{}: B on the background", theme.name);
            assert_ne!(a, b, "{}", theme.name);
        }
    }

    #[test]
    fn a_partial_set_of_a_b_and_null_fills_the_rest() {
        let text = strip_grid(BUILT_IN[0]).replace(
            "\"syntax\"",
            "\"grid\": { \"inspected\": \"#ffff00\", \"null\": \"#ff00ff\" }, \"syntax\"",
        );
        let theme = Theme::parse(&text).unwrap();
        let g = theme.grid.resolve(&theme.colors);
        assert_eq!(g.inspected.to_hex(), "#ffff00");
        assert_eq!(g.null.to_hex(), "#ff00ff");
        assert_eq!(g.inspected_foreground, theme.colors.foreground);
        assert_eq!(g.compared.to_hex(), "#facc151f");
        // A user's theme: the lenient reader takes the new keys too.
        let (read, notes) = Theme::read(&text, None, "x").unwrap();
        assert_eq!(read.grid.inspected, theme.grid.inspected);
        assert!(notes.is_empty(), "{notes:?}");
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
