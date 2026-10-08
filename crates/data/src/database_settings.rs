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
    match serde_json::from_str(&text) {
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
    serde_json::to_value(DatabaseSettings::default()).unwrap_or_default()
}

/// `text` in this version's layout: older layouts moved over, the user's values
/// kept, missing keys at their default, unknown ones kept (the file then says it
/// can't be applied, and AI tools see no rows until it's fixed).
pub fn upgrade(text: &str) -> Result<String, String> {
    let mut user = crate::upgrade::parse(text)?;
    if !user.is_object() {
        return Err("the file must contain a JSON object".into());
    }
    migrate(&mut user);
    Ok(crate::upgrade::pretty(&crate::upgrade::merge(
        &defaults(),
        &user,
    )))
}

/// Older layouts in today's: the rules right under `ai` move to
/// `ai.content_blocking`, a whitelist (`type`, `show`) becomes `hide` with
/// exceptions, and `block_queries` a list of modes.
fn migrate(root: &mut Value) {
    let Some(ai) = root.get_mut("ai").and_then(Value::as_object_mut) else {
        return;
    };
    let moved: Vec<(String, Value)> = ["type", "redact", "hide", "show", "block_queries"]
        .into_iter()
        .filter_map(|key| Some((key.to_string(), ai.remove(key)?)))
        .collect();
    let rules = ai.entry("content_blocking").or_insert_with(|| json!({}));
    let Some(rules) = rules.as_object_mut() else {
        return;
    };
    for (key, value) in moved {
        rules.entry(key).or_insert(value);
    }
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| Some(s.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    match rules.get("type").and_then(Value::as_str) {
        Some("whitelist") => {
            // Hidden but what `show` matches; what `hide` matches hidden again.
            let mut hide = vec!["*".to_string()];
            hide.extend(strings(rules.get("show")).iter().map(|s| format!("!{s}")));
            hide.extend(strings(rules.get("hide")));
            rules.insert("hide".into(), json!(hide));
            rules.remove("type");
            rules.remove("show");
        }
        // A blacklist never used `show`.
        Some("blacklist") => {
            rules.remove("type");
            rules.remove("show");
        }
        _ => {}
    }
    if let Some(Value::String(level)) = rules.get("block_queries") {
        let modes = match level.as_str() {
            "none" => json!([]),
            "redacted" => json!(["redacted"]),
            "redacted_and_hidden" => json!(["redacted", "hidden"]),
            _ => return,
        };
        rules.insert("block_queries".into(), modes);
    }
}

#[cfg(test)]
mod tests {
    use tangul_core::ai_view::Blocked;

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
    fn older_layouts_are_upgraded_keeping_every_rule() {
        let up = |text: &str| -> DatabaseSettings {
            serde_json::from_str(&upgrade(text).unwrap()).unwrap_or_else(|e| panic!("{e}"))
        };
        let s = up(r#"{"ai": {"type": "blacklist", "redact": ["*.company_id"],
                        "hide": ["*.org_id"], "show": [], "block_queries": "none"}}"#);
        let c = &s.ai.content_blocking;
        assert_eq!(
            (c.redact.as_slice(), c.hide.as_slice()),
            (
                &["*.company_id".to_string()][..],
                &["*.org_id".to_string()][..]
            )
        );
        assert!(c.block_queries.is_empty());

        let s = up(
            r#"{"ai": {"content_blocking": {"type": "whitelist", "show": ["orders"],
                        "hide": ["orders.total"], "block_queries": "redacted_and_hidden"}}}"#,
        );
        let c = &s.ai.content_blocking;
        assert_eq!(c.hide, ["*", "!orders", "orders.total"]);
        assert_eq!(c.block_queries, [Blocked::Redacted, Blocked::Hidden]);

        assert_eq!(up("{}"), DatabaseSettings::default());
        assert_eq!(upgrade(""), Ok(crate::upgrade::pretty(&defaults())));
        // What it doesn't know stays, and the file then doesn't apply.
        let kept = upgrade(r#"{"ai": {"content_blocking": {"hdie": ["*"]}}}"#).unwrap();
        assert!(kept.contains("hdie"), "{kept}");
        assert!(serde_json::from_str::<DatabaseSettings>(&kept).is_err());
    }
}
