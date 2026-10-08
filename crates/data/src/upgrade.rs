//! Bringing a settings file up to date: the migration steps from its `"version"` to
//! the current one ([`crate::migrations`]), then every key the current layout knows,
//! with the user's values kept, and keys it doesn't know kept too (so nothing the
//! user wrote is lost; the file tab says what doesn't apply). The app does this once
//! at start for `settings.json` and each database's file; the file tab offers it,
//! and the defaults, as buttons.

use std::path::Path;

use serde_json::Value;

use semver::Version;

use crate::migrations::Steps;

/// The key holding a file's layout: the tangul version it was written for.
pub const VERSION: &str = "version";

/// `text` (a JSON object) migrated by `steps` from its version to the current one,
/// then merged onto `defaults`. A file written by a newer tangul is refused.
pub fn upgrade(text: &str, defaults: &Value, steps: Steps) -> Result<String, String> {
    let mut user = parse(text)?;
    if !user.is_object() {
        return Err("the file must contain a JSON object".into());
    }
    let version = version(&user)?;
    if let Some(newer) = newer(version.as_ref(), steps) {
        return Err(newer);
    }
    steps.apply(&mut user, version.as_ref())?;
    let mut out = merge(defaults, &user);
    out[VERSION] = steps.current().to_string().into();
    Ok(pretty(&out))
}

/// The layout of `file`; `None` before versions were written (or a dev build's
/// whole number, before they were tangul versions).
pub fn version(file: &Value) -> Result<Option<Version>, String> {
    match file.get(VERSION) {
        None | Some(Value::Number(_)) => Ok(None),
        Some(Value::String(v)) => Version::parse(v)
            .map(Some)
            .map_err(|e| format!("\"{VERSION}\" {v:?}: {e}")),
        Some(_) => Err(format!(
            "\"{VERSION}\" must be a tangul version, e.g. \"0.1.3\""
        )),
    }
}

/// Why a file of layout `version` can't be read here, if a newer tangul wrote it.
pub fn newer(version: Option<&Version>, steps: Steps) -> Option<String> {
    let current = steps.current();
    version
        .filter(|v| **v > current)
        .map(|v| format!("written by a newer tangul ({v}; this one is {current})"))
}

/// A file's upgrade: its text in the current layout.
pub type Upgrade = fn(&str) -> Result<String, String>;

/// `user` in the shape of `defaults`: where `user` has a value it stays, missing
/// keys come from `defaults`, keys only `user` has are kept.
pub fn merge(defaults: &Value, user: &Value) -> Value {
    match (defaults, user) {
        (Value::Object(d), Value::Object(u)) => {
            let mut out = d.clone();
            for (key, value) in u {
                let merged = match d.get(key) {
                    Some(default) => merge(default, value),
                    None => value.clone(),
                };
                out.insert(key.clone(), merged);
            }
            Value::Object(out)
        }
        _ => user.clone(),
    }
}

/// The JSON in `text`; `{}` when it is empty.
pub fn parse(text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))
}

/// `value` as the file's text.
pub fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default() + "\n"
}

/// Upgrades the file at `path` with `upgrade` if that changes what it says, keeping
/// the old text next to it as `<name>.bak`. `Ok(false)`: nothing to do (no file,
/// or already up to date). A file that isn't valid JSON is left as it is.
pub fn upgrade_file(
    path: &Path,
    upgrade: fn(&str) -> Result<String, String>,
) -> Result<bool, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let new = upgrade(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if parse(&new).ok() == parse(&text).ok() {
        return Ok(false);
    }
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    std::fs::write(&backup, &text)
        .and_then(|()| std::fs::write(path, &new))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Upgrades `settings.json` and every database's file in `config_dir`; what
/// couldn't be upgraded, and why.
pub fn upgrade_all(config_dir: &Path) -> Vec<String> {
    let mut files: Vec<(std::path::PathBuf, Upgrade)> = vec![(
        config_dir.join(crate::settings::FILE),
        crate::settings::upgrade,
    )];
    if let Ok(dir) = std::fs::read_dir(crate::database_settings::dir(config_dir)) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                files.push((path, crate::database_settings::upgrade));
            }
        }
    }
    files
        .into_iter()
        .filter_map(|(path, upgrade)| upgrade_file(&path, upgrade).err())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merge_keeps_the_users_values_and_unknown_keys() {
        let defaults = json!({"a": {"x": 1, "y": [1]}, "b": null});
        let user = json!({"a": {"y": []}, "c": "mine"});
        assert_eq!(
            merge(&defaults, &user),
            json!({"a": {"x": 1, "y": []}, "b": null, "c": "mine"})
        );
        assert_eq!(
            merge(&defaults, &json!({"a": 3})),
            json!({"a": 3, "b": null})
        );
    }

    #[test]
    fn files_are_upgraded_once_with_a_backup_and_broken_ones_left_alone() {
        let dir = std::env::temp_dir().join(format!("tangul-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("databases")).unwrap();
        let db = dir.join("databases").join("db1.json");
        let old = r#"{"ai": {"content_blocking": {"redact": ["*.company_id"]}}}"#;
        std::fs::write(&db, old).unwrap();
        std::fs::write(dir.join("databases").join("broken.json"), "{ nope").unwrap();

        let problems = upgrade_all(&dir);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("broken.json"), "{problems:?}");
        let new: Value = serde_json::from_str(&std::fs::read_to_string(&db).unwrap()).unwrap();
        assert_eq!(new["version"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(
            new["ai"]["content_blocking"]["redact"],
            json!(["*.company_id"])
        );
        assert_eq!(new["ai"]["content_blocking"]["hide"], json!([]));
        assert_eq!(
            std::fs::read_to_string(dir.join("databases").join("db1.json.bak")).unwrap(),
            old
        );
        assert!(!dir.join("settings.json").exists(), "no file, none made");

        // Up to date: left as it is.
        let text = std::fs::read_to_string(&db).unwrap();
        std::fs::remove_file(dir.join("databases").join("db1.json.bak")).unwrap();
        assert!(!upgrade_file(&db, crate::database_settings::upgrade).unwrap());
        assert_eq!(std::fs::read_to_string(&db).unwrap(), text);
        assert!(!dir.join("databases").join("db1.json.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
