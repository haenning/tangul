//! The config directory: where `profiles.toml`, `settings.json`, `local.db` and
//! `metrics.db` live.

use std::path::{Path, PathBuf};

/// The app's name before it became tangul. Its config directory and Keychain
/// entries are taken over the first time they are needed.
pub(crate) const LEGACY_NAME: &str = "rusdb";

/// Where `profiles.toml` and `settings.json` live; `TANGUL_CONFIG_DIR` overrides it.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TANGUL_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    let base = dirs::config_dir()?;
    let dir = base.join("tangul");
    adopt_legacy_dir(&base.join(LEGACY_NAME), &dir);
    Some(dir)
}

/// Where a save dialog starts: the Downloads folder, else the home folder.
pub fn save_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_default()
}

/// Moves the config directory of the old name to `dir`, if there is one and `dir`
/// doesn't exist yet.
fn adopt_legacy_dir(legacy: &Path, dir: &Path) {
    if dir.exists() || !legacy.is_dir() {
        return;
    }
    match std::fs::rename(legacy, dir) {
        Ok(()) => eprintln!("moved {} to {}", legacy.display(), dir.display()),
        Err(e) => eprintln!("could not move {}: {e}", legacy.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_over_the_legacy_config_dir() {
        let base = std::env::temp_dir().join(format!("tangul-dirs-{}", std::process::id()));
        let (legacy, dir) = (base.join("rusdb"), base.join("tangul"));
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("profiles.toml"), "x").unwrap();
        adopt_legacy_dir(&legacy, &dir);
        assert!(!legacy.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("profiles.toml")).unwrap(),
            "x"
        );
        // An existing new directory is never replaced.
        std::fs::create_dir_all(&legacy).unwrap();
        adopt_legacy_dir(&legacy, &dir);
        assert!(legacy.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
