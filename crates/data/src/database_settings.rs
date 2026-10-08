//! Each database's own settings (`tangul_core::ai_view::DatabaseSettings`), one JSON
//! file per database in the `databases` folder next to `settings.json`, named by
//! its id. Read when needed, so a saved change applies right away.

use std::path::{Path, PathBuf};

use tangul_core::ai_view::{AiColumns, DatabaseAi, DatabaseSettings, ListKind};

const DIR: &str = "databases";

/// Where database `id`'s settings live in `config_dir`.
pub fn path(config_dir: &Path, id: &str) -> PathBuf {
    config_dir.join(DIR).join(format!("{id}.json"))
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
            content_blocking: AiColumns {
                kind: ListKind::Whitelist,
                ..AiColumns::default()
            },
        },
    }
}

/// Writes database `id`'s file with the defaults if there is none, so there is
/// something to edit. Returns its path.
pub fn create_if_missing(config_dir: &Path, id: &str) -> std::io::Result<PathBuf> {
    let file = path(config_dir, id);
    if !file.exists() {
        std::fs::create_dir_all(config_dir.join(DIR))?;
        let text = serde_json::to_string_pretty(&DatabaseSettings::default())
            .map_err(std::io::Error::other)?;
        std::fs::write(&file, text + "\n")?;
    }
    Ok(file)
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
        assert_eq!(settings.ai.content_blocking.kind, ListKind::Whitelist);
        assert!(settings.ai.content_blocking.show.is_empty());
        assert!(problem.unwrap().contains("db1.json"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
