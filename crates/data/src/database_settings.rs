//! Each database's own settings (`tangul_core::ai_view::DatabaseSettings`), one JSON
//! file per database in the `databases` folder next to `settings.json`, named by
//! its id. Read when needed, so a saved change applies right away.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tangul_core::ai_view::{AiColumns, DatabaseAi, DatabaseSettings};

const DIR: &str = "databases";

/// The folder of every database's settings in `config_dir`.
pub fn dir(config_dir: &Path) -> PathBuf {
    config_dir.join(DIR)
}

/// Where database `id`'s settings live in `config_dir`.
pub fn path(config_dir: &Path, id: &str) -> PathBuf {
    dir(config_dir).join(format!("{id}.json"))
}

/// Database `id`'s settings: the defaults without a file (or folder). A file that
/// can't be read gives what is safe instead, and why: AI tools see no rows.
pub fn load(config_dir: Option<&Path>, id: &str) -> (DatabaseSettings, Option<String>) {
    let Some(dir) = config_dir else {
        return (DatabaseSettings::default(), None);
    };
    let file = path(dir, id);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (DatabaseSettings::default(), None);
        }
        Err(e) => return (closed(), Some(format!("{}: {e}", file.display()))),
    };
    match parse(&text) {
        Ok(settings) => (settings, None),
        Err(e) => (closed(), Some(format!("{}: {e}", file.display()))),
    }
}

/// Every column hidden: what AI tools get while the file is broken.
fn closed() -> DatabaseSettings {
    DatabaseSettings {
        ai: DatabaseAi {
            content_blocking: AiColumns::closed(),
        },
    }
}

/// Writes database `id`'s file with the defaults if there is none, so there is
/// something to edit. Returns its path.
pub fn create_if_missing(config_dir: &Path, id: &str) -> std::io::Result<PathBuf> {
    let file = path(config_dir, id);
    if !file.exists() {
        std::fs::create_dir_all(config_dir.join(DIR))?;
        std::fs::write(&file, crate::upgrade::pretty(&defaults()))?;
    }
    Ok(file)
}

/// Every key the file can have, at its default.
pub fn defaults() -> Value {
    let settings = serde_json::to_value(DatabaseSettings::default()).unwrap_or_default();
    let version =
        json!({ crate::upgrade::VERSION: crate::migrations::current(crate::migrations::DATABASE) });
    crate::upgrade::merge(&version, &settings)
}

/// `text` in this version's layout: the user's values kept, missing keys at their
/// default, unknown ones kept (the file then says it can't be applied, and AI tools
/// see no rows until it's fixed).
pub fn upgrade(text: &str) -> Result<String, String> {
    crate::upgrade::upgrade(text, &defaults(), crate::migrations::DATABASE)
}

/// The settings in `text`; an error for anything this version can't read exactly
/// (unknown keys, a newer layout).
pub fn parse(text: &str) -> Result<DatabaseSettings, String> {
    let value = crate::upgrade::parse(text)?;
    let version = crate::upgrade::version(&value)?;
    if let Some(newer) = crate::upgrade::newer(version, crate::migrations::DATABASE) {
        return Err(newer);
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tangul-dbsettings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_is_open_written_is_read_and_broken_is_closed() {
        let d = dir("load");
        assert_eq!(load(Some(&d), "db1"), (DatabaseSettings::default(), None));
        assert_eq!(load(None, "db1"), (DatabaseSettings::default(), None));

        let file = create_if_missing(&d, "db1").unwrap();
        assert_eq!(file, d.join("databases").join("db1.json"));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"redact\": []"), "{text}");
        assert_eq!(load(Some(&d), "db1"), (DatabaseSettings::default(), None));

        std::fs::write(
            &file,
            r#"{"ai": {"content_blocking": {"hide": ["*.ssn"]}}}"#,
        )
        .unwrap();
        create_if_missing(&d, "db1").unwrap();
        assert_eq!(
            load(Some(&d), "db1").0.ai.content_blocking.hide,
            ["*.ssn"],
            "kept"
        );

        std::fs::write(&file, "{ broken").unwrap();
        let (settings, problem) = load(Some(&d), "db1");
        assert_eq!(settings.ai.content_blocking, AiColumns::closed());
        assert!(problem.unwrap().contains("db1.json"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn versions_unknown_keys_and_newer_layouts() {
        assert_eq!(
            parse(&upgrade("").unwrap()),
            Ok(DatabaseSettings::default())
        );
        assert!(upgrade("").unwrap().starts_with("{\n  \"version\": 1,"));
        // What it doesn't know stays, and the file then doesn't apply.
        let kept = upgrade(r#"{"ai": {"content_blocking": {"hdie": ["*"]}}}"#).unwrap();
        assert!(kept.contains("hdie"), "{kept}");
        assert!(parse(&kept).is_err());
        // A dev build's layout isn't migrated: it fails, so AI tools see no rows.
        assert!(parse(r#"{"ai": {"type": "blacklist", "redact": ["*.x"]}}"#).is_err());
        let newer = r#"{"version": 2, "ai": {}}"#;
        assert!(parse(newer).unwrap_err().contains("newer tangul"));
        assert!(upgrade(newer).is_err());
    }
}
