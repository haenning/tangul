//! Applies `settings.json` (see [`tangul_data::settings`]) to the theme, reloads it
//! while the app runs, and makes the settings in effect available to every view.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::Theme;
use gpui_kit::{App, Global, Hsla, Pixels, Rgba, SharedString, px};
pub use tangul_data::settings::*;

/// How often the file is checked for changes.
const POLL: Duration = Duration::from_secs(1);

/// The settings in effect, as a global so any view can read them (and observe
/// them changing).
pub(crate) struct Current(Settings);
impl Global for Current {}

/// The settings in effect.
pub fn current(cx: &App) -> &Settings {
    &cx.global::<Current>().0
}

/// The settings file, created with the theme's editor font if it doesn't exist yet.
pub fn ensure_file(cx: &App) -> Option<PathBuf> {
    let theme = cx.global::<Theme>();
    create_if_missing(theme.mono_font_size.as_f32(), &theme.mono_font_family)
}

/// Puts `user` in or takes it out of `safety.read_only_users` in the file, keeping
/// everything else in it, and applies the change right away.
pub fn set_read_only(user: &str, on: bool, cx: &mut App) -> Result<(), String> {
    let path = ensure_file(cx).ok_or("could not create the settings file")?;
    let settings = write_read_only(&path, user, on)?;
    cx.set_global(Current(settings));
    cx.refresh_windows();
    Ok(())
}

/// Sets the value at `keys` in the file (`null` removes it), keeping everything
/// else in it, and applies the change right away.
pub fn set_value(keys: &[&str], value: serde_json::Value, cx: &mut App) -> Result<(), String> {
    let path = ensure_file(cx).ok_or("could not create the settings file")?;
    let settings = write_value(&path, keys, value)?;
    cx.set_global(Current(settings));
    cx.refresh_windows();
    Ok(())
}

/// Turns the MCP server on or off (`mcp.enabled`).
pub fn set_mcp_enabled(on: bool, cx: &mut App) -> Result<(), String> {
    set_value(&["mcp", "enabled"], on.into(), cx)
}

fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.,
        g: f32::from(c.g) / 255.,
        b: f32::from(c.b) / 255.,
        a: f32::from(c.a) / 255.,
    }
    .into()
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
    theme.chart_1 = settings
        .chart
        .primary_color
        .map_or(defaults.chart_color, hsla);
    cx.refresh_windows();
}

/// Loads and applies the settings, then re-applies them whenever the file changes.
/// The defaults, without reading `settings.json` (tests).
#[cfg(test)]
pub fn init_defaults(cx: &mut App) {
    cx.set_global(Current(Settings::default()));
}

/// Applies `settings` without the file (tests).
#[cfg(test)]
pub fn set_for_test(settings: Settings, cx: &mut App) {
    cx.set_global(Current(settings));
}

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
