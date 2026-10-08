//! Bringing a settings file up to date: every key the current version knows, with
//! the user's values kept, and keys this version doesn't know kept too (so nothing
//! the user wrote is lost; the file tab says what doesn't apply). The app does this
//! once at start for `settings.json` and each database's file; the file tab offers
//! it, and the defaults, as buttons.

use std::path::Path;

use serde_json::Value;

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
        let old = r#"{"ai": {"type": "blacklist", "redact": ["*.company_id"]}}"#;
        std::fs::write(&db, old).unwrap();
        std::fs::write(dir.join("databases").join("broken.json"), "{ nope").unwrap();

        let problems = upgrade_all(&dir);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("broken.json"), "{problems:?}");
        let new: Value = serde_json::from_str(&std::fs::read_to_string(&db).unwrap()).unwrap();
        assert_eq!(
            new["ai"]["content_blocking"]["redact"],
            json!(["*.company_id"])
        );
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
