//! Applies `settings.json` (see [`tangul_data::settings`]): the fonts here, the
//! themes in [`crate::theme`]; reloads it while the app runs, and makes the settings
//! in effect available to every view.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::component::Theme;
use gpui_kit::{App, Global, Pixels, SharedString, px};
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
    apply(&settings, cx);
    Ok(())
}

/// Turns the MCP server on or off (`mcp.enabled`).
pub fn set_mcp_enabled(on: bool, cx: &mut App) -> Result<(), String> {
    set_value(&["mcp", "enabled"], on.into(), cx)
}

/// The editor font before any setting was applied, so that removing a key brings
/// the default back.
struct FontDefaults {
    size: Pixels,
    family: SharedString,
}

impl Global for FontDefaults {}

/// Puts the font settings onto the theme (after a theme change too, which resets
/// them).
pub fn apply_fonts(cx: &mut App) {
    let Some(defaults) = cx.try_global::<FontDefaults>() else {
        return;
    };
    let (size, family) = (defaults.size, defaults.family.clone());
    let settings = current(cx).clone();
    let theme = Theme::global_mut(cx);
    theme.mono_font_size = settings.font_size().map_or(size, px);
    theme.mono_font_family = settings
        .font_family()
        .map_or(family, |f| f.to_string().into());
}

fn apply(settings: &Settings, cx: &mut App) {
    cx.set_global(Current(settings.clone()));
    crate::theme::apply(None, cx);
}

/// Loads and applies the settings, then re-applies them whenever the file changes.
/// The defaults, without reading `settings.json` (tests).
#[cfg(test)]
pub fn init_defaults(cx: &mut App) {
    cx.set_global(Current(Settings::default()));
    let theme = cx.global::<Theme>();
    let defaults = FontDefaults {
        size: theme.mono_font_size,
        family: theme.mono_font_family.clone(),
    };
    cx.set_global(defaults);
    crate::theme::init_built_in(cx);
    crate::theme::apply(None, cx);
}

/// Applies `settings` without the file (tests).
#[cfg(test)]
pub fn set_for_test(settings: Settings, cx: &mut App) {
    apply(&settings, cx);
}

pub fn init(cx: &mut App) {
    cx.set_global(Current(Settings::default()));
    let defaults = {
        let theme = cx.global::<Theme>();
        FontDefaults {
            size: theme.mono_font_size,
            family: theme.mono_font_family.clone(),
        }
    };
    cx.set_global(defaults);
    crate::theme::init(cx);
    let Some(path) = path() else {
        crate::theme::apply(None, cx);
        return;
    };
    let mut seen = None;
    reload(&path, &mut seen, cx);
    // The settings when no file was read: still the themes.
    if seen == Some(None) {
        crate::theme::apply(None, cx);
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            cx.update(|cx| {
                reload(&path, &mut seen, cx);
                crate::theme::reload_if_changed(cx);
            });
        }
    })
    .detach();
}

/// Applies the file if it changed since `seen` (its modification time). A file that
/// can't be parsed keeps the settings that were applied before.
fn reload(path: &Path, seen: &mut Option<Option<SystemTime>>, cx: &mut App) {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if *seen == Some(modified) {
        return;
    }
    *seen = Some(modified);
    // No file: defaults.
    let text = std::fs::read_to_string(path).unwrap_or_default();
    match Settings::parse(&text) {
        Ok(settings) => apply(&settings, cx),
        Err(e) => eprintln!("ignoring invalid {}: {e}", path.display()),
    }
}
