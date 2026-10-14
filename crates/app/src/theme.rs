//! Every color of the app, in one place. A theme ([`tangul_data::themes::Theme`]:
//! semantic colors as a JSON file) becomes gpui-kit's theme here, which the views
//! read (`cx.theme()`); what gpui-kit has no field for (the chart colors, the
//! engraved wordmark) comes from this module too. No other file names a color.
//!
//! `settings.json` picks a dark and a light theme and which shows (`theme`); "auto"
//! follows the system and switches with it.

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, Global, Hsla, Rgba, Window, WindowAppearance, black, white};
use serde_json::{Value, json};
use tangul_data::settings::{Appearance, Rgb};
use tangul_data::themes::{self, Theme as ThemeFile, ThemeMode as FileMode};

use crate::settings;

/// The themes on offer and what tangul draws itself.
pub struct Themes {
    /// Built-in and from the themes folder, by name.
    pub list: Vec<ThemeFile>,
    /// Why theme files didn't load, or a chosen theme isn't used.
    pub problems: Vec<String>,
    /// The theme showing.
    pub current: ThemeFile,
    /// The themes folder as last read (reloaded when it changes).
    stamp: Vec<(std::path::PathBuf, Option<std::time::SystemTime>)>,
}

impl Global for Themes {}

/// Loads the themes (built-in, and the user's folder).
pub fn init(cx: &mut App) {
    let dir = themes::dir();
    let (list, problems) = themes::load(dir.as_deref());
    let stamp = dir.as_deref().map(themes::stamp).unwrap_or_default();
    cx.set_global(Themes {
        list,
        problems,
        current: themes::default_for(FileMode::Dark),
        stamp,
    });
}

/// Only the built-in themes, nothing from disk (tests).
#[cfg(test)]
pub fn init_built_in(cx: &mut App) {
    cx.set_global(Themes {
        list: themes::built_in(),
        problems: Vec::new(),
        current: themes::default_for(FileMode::Dark),
        stamp: Vec::new(),
    });
}

/// Reloads the themes when a file in the folder changed, and applies them.
pub fn reload_if_changed(cx: &mut App) {
    let Some(dir) = themes::dir() else {
        return;
    };
    let stamp = themes::stamp(&dir);
    if cx.global::<Themes>().stamp == stamp {
        return;
    }
    let (list, problems) = themes::load(Some(&dir));
    let themes = cx.global_mut::<Themes>();
    (themes.list, themes.problems, themes.stamp) = (list, problems, stamp);
    apply(None, cx);
}

/// The themes on offer.
pub fn list(cx: &App) -> &[ThemeFile] {
    &cx.global::<Themes>().list
}

/// The theme showing.
pub fn current(cx: &App) -> &ThemeFile {
    &cx.global::<Themes>().current
}

/// The theme the settings choose for `mode`: by name, of that mode; else the
/// built-in one (and the reason is kept in [`Themes::problems`]).
fn chosen(mode: FileMode, cx: &App) -> (ThemeFile, Option<String>) {
    let set = &settings::current(cx).theme;
    let name = match mode {
        FileMode::Dark => &set.dark,
        FileMode::Light => &set.light,
    };
    match list(cx).iter().find(|t| t.name == *name) {
        Some(t) if t.mode == mode => (t.clone(), None),
        Some(_) => (
            themes::default_for(mode),
            Some(format!("{name} is not a {} theme", mode_key(mode))),
        ),
        None => (
            themes::default_for(mode),
            Some(format!("there is no theme named {name}")),
        ),
    }
}

fn mode_key(mode: FileMode) -> &'static str {
    match mode {
        FileMode::Dark => "dark",
        FileMode::Light => "light",
    }
}

/// Which appearance shows: the setting, or the system's (the window's when there
/// is one).
fn mode(window: Option<&Window>, cx: &App) -> FileMode {
    match settings::current(cx).theme.appearance {
        Appearance::Dark => FileMode::Dark,
        Appearance::Light => FileMode::Light,
        Appearance::Auto => {
            let appearance = window.map_or_else(|| cx.window_appearance(), Window::appearance);
            match appearance {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => FileMode::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => FileMode::Light,
            }
        }
    }
}

/// Applies the chosen dark and light themes and shows the one the settings (or the
/// system) want; then the font settings, which a theme change resets.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    let (dark, dark_problem) = chosen(FileMode::Dark, cx);
    let (light, light_problem) = chosen(FileMode::Light, cx);
    let shown = mode(window.as_deref(), cx);
    {
        let theme = Theme::global_mut(cx);
        theme.dark_theme = Rc::new(config(&dark));
        theme.light_theme = Rc::new(config(&light));
    }
    let themes = cx.global_mut::<Themes>();
    themes.problems.retain(|p| !p.starts_with("theme: "));
    themes.problems.extend(
        [dark_problem, light_problem]
            .into_iter()
            .flatten()
            .map(|p| format!("theme: {p}")),
    );
    themes.current = if shown == FileMode::Dark { dark } else { light };
    let gpui_mode = match shown {
        FileMode::Dark => ThemeMode::Dark,
        FileMode::Light => ThemeMode::Light,
    };
    Theme::change(gpui_mode, window, cx);
    settings::apply_fonts(cx);
    cx.refresh_windows();
}

/// An sRGB color as gpui's.
pub fn color(c: Rgb) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.,
        g: f32::from(c.g) / 255.,
        b: f32::from(c.b) / 255.,
        a: f32::from(c.a) / 255.,
    }
    .into()
}

/// `n` colors for chart series: the theme's chart colors in turn, or, when it has
/// one, shades of it from full down to faint (over the background, so stacked
/// areas stay opaque). `chart.primary_color` in the settings replaces the first.
pub fn chart_colors(n: usize, cx: &App) -> Vec<Hsla> {
    let theme = current(cx);
    let mut colors = theme.charts.clone();
    if let Some(primary) = settings::current(cx).chart.primary_color {
        colors[0] = primary;
    }
    let n = n.max(1);
    if colors.len() > 1 {
        return (0..n).map(|i| color(colors[i % colors.len()])).collect();
    }
    let (base, background) = (colors[0], theme.colors.background);
    (0..n)
        .map(|i| {
            let amount = if n == 1 {
                1.
            } else {
                1. - 0.65 * i as f32 / (n - 1) as f32
            };
            color(mix(background, base, amount))
        })
        .collect()
}

/// The wordmark on the empty content area, engraved: its fill, the edge above (a
/// shadow) and the edge below (a highlight).
pub struct Engraving {
    pub fill: Hsla,
    pub shade: Hsla,
    pub light: Hsla,
}

pub fn engraving(cx: &App) -> Engraving {
    if current(cx).mode == FileMode::Dark {
        Engraving {
            fill: white().opacity(0.05),
            shade: black().opacity(0.9),
            light: white().opacity(0.09),
        }
    } else {
        Engraving {
            fill: black().opacity(0.08),
            shade: black().opacity(0.12),
            light: white().opacity(0.95),
        }
    }
}

/// `color` mixed into `background`: an opaque tint, so stacked layers painted over
/// each other keep their own color.
pub fn tint(color: Hsla, background: Hsla, amount: f32) -> Hsla {
    let (c, b) = (color.to_rgb(), background.to_rgb());
    let mix = |c: f32, b: f32| b + (c - b) * amount;
    Rgba {
        r: mix(c.r, b.r),
        g: mix(c.g, b.g),
        b: mix(c.b, b.b),
        a: 1.,
    }
    .into()
}

/// `b` mixed into `a` by `amount` (0: `a`, 1: `b`), opaque.
fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    let channel =
        |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    Rgb {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: 255,
    }
}

/// `c` with alpha `a` (0-255).
fn alpha(c: Rgb, a: u8) -> Rgb {
    Rgb { a, ..c }
}

/// The theme as gpui-kit's: every field it has, from the theme's colors.
pub fn config(theme: &ThemeFile) -> ThemeConfig {
    let c = &theme.colors;
    let hex = |c: Rgb| Value::String(c.to_hex());
    // Hover and pressed states of a filled color: toward the text, toward the
    // background.
    let hover = |x: Rgb| hex(mix(x, c.foreground, 0.12));
    let pressed = |x: Rgb| hex(mix(x, c.background, 0.2));
    let chart = |i: usize| hex(theme.charts[i % theme.charts.len()]);
    let mut colors: serde_json::Map<String, Value> = [
        ("background", hex(c.background)),
        ("foreground", hex(c.foreground)),
        ("border", hex(c.border)),
        ("caret", hex(c.foreground)),
        ("ring", hex(c.accent)),
        ("overlay", hex(alpha(c.background, 0x99))),
        ("window.border", hex(c.border)),
        ("accent.background", hex(c.hover)),
        ("accent.foreground", hex(c.foreground)),
        ("accordion.background", hex(c.background)),
        ("muted.background", hex(c.raised)),
        ("muted.foreground", hex(c.muted)),
        ("popover.background", hex(c.raised)),
        ("popover.foreground", hex(c.foreground)),
        ("group_box.background", hex(c.surface)),
        ("group_box.foreground", hex(c.foreground)),
        ("group_box.title.foreground", hex(c.muted)),
        ("description_list.label.background", hex(c.surface)),
        ("description_list.label.foreground", hex(c.foreground)),
        ("skeleton.background", hex(c.surface)),
        ("primary.background", hex(c.accent)),
        ("primary.hover.background", hover(c.accent)),
        ("primary.active.background", pressed(c.accent)),
        ("primary.foreground", hex(c.accent_foreground)),
        ("secondary.background", hex(c.raised)),
        ("secondary.hover.background", hex(c.hover)),
        ("secondary.active.background", hex(c.active)),
        ("secondary.foreground", hex(c.foreground)),
        ("link", hex(c.accent)),
        ("link.hover", hover(c.accent)),
        ("link.active", pressed(c.accent)),
        ("selection.background", hex(c.selection)),
        ("drag.border", hex(c.accent)),
        ("drop_target.background", hex(alpha(c.accent, 0x26))),
        ("progress.bar.background", hex(c.accent)),
        ("slider.background", hex(c.accent)),
        ("slider.thumb.background", hex(c.background)),
        ("switch.background", hex(c.active)),
        ("switch.thumb.background", hex(c.foreground)),
        ("input.border", hex(c.input_border)),
        ("scrollbar.background", hex(alpha(c.background, 0))),
        ("scrollbar.thumb.background", hex(alpha(c.subtle, 0xb3))),
        ("scrollbar.thumb.hover.background", hex(c.subtle)),
        ("sidebar.background", hex(c.surface)),
        ("sidebar.foreground", hex(c.foreground)),
        ("sidebar.border", hex(c.border)),
        ("sidebar.accent.background", hex(c.hover)),
        ("sidebar.accent.foreground", hex(c.foreground)),
        ("sidebar.primary.background", hex(c.accent)),
        ("sidebar.primary.foreground", hex(c.accent_foreground)),
        ("title_bar.background", hex(c.surface)),
        ("title_bar.border", hex(c.border)),
        ("status_bar.background", hex(c.surface)),
        ("status_bar.border", hex(c.border)),
        ("tab.background", hex(alpha(c.surface, 0))),
        ("tab.foreground", hex(c.muted)),
        ("tab.active.background", hex(c.background)),
        ("tab.active.foreground", hex(c.foreground)),
        ("tab_bar.background", hex(c.surface)),
        ("tab_bar.segmented.background", hex(c.surface)),
        ("list.background", hex(c.background)),
        (
            "list.even.background",
            hex(mix(c.background, c.surface, 0.5)),
        ),
        ("list.head.background", hex(c.surface)),
        ("list.hover.background", hex(c.hover)),
        ("list.active.background", hex(c.selection)),
        ("list.active.border", hex(c.accent)),
        ("table.background", hex(c.background)),
        (
            "table.even.background",
            hex(mix(c.background, c.surface, 0.5)),
        ),
        ("table.head.background", hex(c.surface)),
        ("table.head.foreground", hex(c.muted)),
        ("table.foot.background", hex(c.surface)),
        ("table.foot.foreground", hex(c.muted)),
        ("table.hover.background", hex(c.hover)),
        ("table.active.background", hex(c.selection)),
        ("table.active.border", hex(c.accent)),
        ("table.row.border", hex(c.border)),
        ("chart.1", chart(0)),
        ("chart.2", chart(1)),
        ("chart.3", chart(2)),
        ("chart.4", chart(3)),
        ("chart.5", chart(4)),
        ("chart.bullish", hex(c.success)),
        ("chart.bearish", hex(c.danger)),
        ("base.red", hex(c.danger)),
        ("base.red.light", hover(c.danger)),
        ("base.green", hex(c.success)),
        ("base.green.light", hover(c.success)),
        ("base.yellow", hex(c.warning)),
        ("base.yellow.light", hover(c.warning)),
        ("base.blue", hex(c.info)),
        ("base.blue.light", hover(c.info)),
        ("base.cyan", hex(c.info)),
        ("base.cyan.light", hover(c.info)),
        ("base.magenta", hex(c.accent)),
        ("base.magenta.light", hover(c.accent)),
        ("button.background", hex(c.raised)),
        ("button.hover.background", hex(c.hover)),
        ("button.active.background", hex(c.active)),
        ("button.foreground", hex(c.foreground)),
        ("button.primary.background", hex(c.accent)),
        ("button.primary.hover.background", hover(c.accent)),
        ("button.primary.active.background", pressed(c.accent)),
        ("button.primary.foreground", hex(c.accent_foreground)),
        ("button.secondary.background", hex(c.raised)),
        ("button.secondary.hover.background", hex(c.hover)),
        ("button.secondary.active.background", hex(c.active)),
        ("button.secondary.foreground", hex(c.foreground)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    // Status colors: filled (alerts, buttons) with the background's color as text.
    for (name, x) in [
        ("danger", c.danger),
        ("warning", c.warning),
        ("success", c.success),
        ("info", c.info),
    ] {
        for (key, value) in [
            (format!("{name}.background"), hex(x)),
            (format!("{name}.hover.background"), hover(x)),
            (format!("{name}.active.background"), pressed(x)),
            (format!("{name}.foreground"), hex(c.background)),
            (format!("button.{name}.background"), hex(x)),
            (format!("button.{name}.hover.background"), hover(x)),
            (format!("button.{name}.active.background"), pressed(x)),
            (format!("button.{name}.foreground"), hex(c.background)),
        ] {
            colors.insert(key, value);
        }
    }
    let s = &theme.syntax;
    let token = |x: Rgb| json!({ "color": x.to_hex() });
    let syntax = json!({
        "keyword": token(s.keyword),
        "string": token(s.string),
        "string.escape": token(s.string),
        "string.special": token(s.string),
        "text.literal": token(s.string),
        "number": token(s.number),
        "boolean": token(s.number),
        "constant": token(s.number),
        "comment": token(s.comment),
        "comment.doc": token(s.comment),
        "function": token(s.function),
        "type": token(s.r#type),
        "constructor": token(s.r#type),
        "property": token(s.property),
        "attribute": token(s.property),
        "variable": token(s.property),
        "variable.special": token(s.property),
        "operator": token(s.operator),
        "punctuation": token(s.punctuation),
        "punctuation.bracket": token(s.punctuation),
        "punctuation.delimiter": token(s.punctuation),
        "embedded": token(c.foreground),
    });
    let highlight = json!({
        "editor.background": hex(c.background),
        "editor.foreground": hex(c.foreground),
        "editor.active_line.background": hex(c.surface),
        "editor.line_number": hex(c.subtle),
        "editor.active_line_number": hex(c.foreground),
        "editor.invisible": hex(alpha(c.subtle, 0x66)),
        // Opaque: these are also the backgrounds of the editor's problem popups.
        "error.border": hex(c.danger),
        "error.background": hex(mix(c.background, c.danger, 0.15)),
        "warning.border": hex(c.warning),
        "warning.background": hex(mix(c.background, c.warning, 0.15)),
        "info.border": hex(c.info),
        "info.background": hex(mix(c.background, c.info, 0.15)),
        "success.background": hex(mix(c.background, c.success, 0.15)),
        "syntax": syntax,
    });
    let config = json!({
        "name": theme.name,
        "mode": mode_key(theme.mode),
        "colors": colors,
        "highlight": highlight,
    });
    serde_json::from_value(config).expect("the theme maps onto gpui-kit's")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_maps_onto_gpui_kit() {
        for theme in themes::built_in() {
            let config = config(&theme);
            assert_eq!(config.name.as_ref(), theme.name);
            assert_eq!(config.mode.is_dark(), theme.mode == FileMode::Dark);
            let colors = serde_json::to_value(&config.colors).unwrap();
            assert_eq!(colors["background"], theme.colors.background.to_hex());
            assert_eq!(colors["primary.background"], theme.colors.accent.to_hex());
            assert_eq!(colors["danger.background"], theme.colors.danger.to_hex());
            // Every field gpui-kit has is set: nothing falls back to its colors.
            let unset: Vec<_> = colors
                .as_object()
                .unwrap()
                .iter()
                .filter(|(_, v)| v.is_null())
                .map(|(k, _)| k.clone())
                .collect();
            assert_eq!(unset, Vec::<String>::new(), "{}", theme.name);
            assert!(config.highlight.is_some());
        }
    }

    #[test]
    fn mixing_and_alpha() {
        let (black, white) = (Rgb::parse("#000").unwrap(), Rgb::parse("#fff").unwrap());
        assert_eq!(mix(black, white, 0.5).to_hex(), "#808080");
        assert_eq!(mix(black, white, 0.).to_hex(), "#000000");
        assert_eq!(alpha(white, 0x80).to_hex(), "#ffffff80");
        assert_eq!(color(white), gpui_kit::white());
    }

    /// Whether `code` makes a color from numbers or white/black. `to_rgb(`,
    /// `transparent_black()` and signatures (`-> Hsla {`) are no colors.
    fn has_color_literal(code: &str) -> bool {
        let built = ["Rgba {", "Hsla {"].iter().any(|p| code.contains(p))
            && !code.contains("->")
            && !code.contains("impl");
        built
            || ["rgb(", "rgba(", "hsla(", "white()", "black()"]
                .iter()
                .any(|p| {
                    code.match_indices(p).any(|(at, _)| {
                        !code[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    })
                })
    }

    #[test]
    fn color_literals_are_recognized() {
        for code in [
            "let c = rgb(0x112233);",
            ".bg(white().opacity(0.1))",
            "black()",
            "let c = hsla(0., 0., 0.5, 1.);",
            "let c = Rgba { r: 1., g: 0., b: 0., a: 1. };",
        ] {
            assert!(has_color_literal(code), "{code}");
        }
        for code in [
            "let (c, b) = (color.to_rgb(), background.to_rgb());",
            ".stroke(transparent_black())",
            "fn color(&self, series: usize) -> Hsla {",
            "theme.background",
        ] {
            assert!(!has_color_literal(code), "{code}");
        }
    }

    /// No color literal outside this module: every color comes from a theme.
    #[test]
    fn colors_are_named_only_here() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.ends_with("theme.rs") || path.ends_with("ui_tests.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (i, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or_default();
                    // A color made from numbers or white/black; `to_rgb(` or
                    // `transparent_black()` are no colors and don't match.
                    let literal = has_color_literal(code);
                    if literal {
                        found.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "colors outside theme.rs:\n{}",
            found.join("\n")
        );
    }
}
