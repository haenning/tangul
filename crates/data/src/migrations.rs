//! The one migrator of the settings files: each file's `"version"` is the tangul
//! version whose layout it has; the steps between layouts are data, one JSON file
//! per version in `crates/data/migrations/<file>/` (see `CLAUDE.md` there), applied
//! in version order to files older than them.
//!
//! A step is a list of operations on dotted paths (`"ai.content_blocking.hide"`);
//! each does nothing where its key is missing, so it runs on any older file:
//!
//! ```json
//! [
//!   { "move": "ai.redact", "to": "ai.content_blocking.redact" },
//!   { "remove": "ai.show" },
//!   { "map": "ai.block_queries", "values": { "none": [], "redacted": ["redacted"] } },
//!   { "merge": "mcp.access", "into": "ai.access", "strictest": ["data", "schema"] }
//! ]
//! ```

use semver::Version;
use serde::Deserialize;
use serde_json::{Map, Value};

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// One settings file's steps, by the version whose layout they lead to.
#[derive(Debug, Clone, Copy)]
pub struct Steps(&'static [(&'static str, &'static str)]);

/// `settings.json`.
pub const SETTINGS: Steps = Steps(SETTINGS_STEPS);

/// `databases/<id>.json`.
pub const DATABASE: Steps = Steps(DATABASE_STEPS);

/// One operation of a step.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
enum Op {
    Move(MoveOp),
    Remove(RemoveOp),
    Map(MapOp),
    Merge(MergeOp),
}

/// The value at `move` goes to `to`, unless `to` is set already (then it is
/// dropped).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveOp {
    #[serde(rename = "move")]
    from: String,
    to: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveOp {
    remove: String,
}

/// A text value at `map` that is a key of `values` becomes that key's value.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct MapOp {
    map: String,
    values: Map<String, Value>,
}

/// The value at `merge` goes away; `into` keeps the stricter of the two, the later
/// in `strictest` (a missing `into` counts as the first, the loosest).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct MergeOp {
    merge: String,
    into: String,
    strictest: Vec<String>,
}

impl Steps {
    /// Each step's version and operations, oldest first.
    fn parsed(self) -> Result<Vec<(Version, Vec<Op>)>, String> {
        let mut steps = self
            .0
            .iter()
            .map(|(version, text)| {
                let v = Version::parse(version).map_err(|e| format!("step {version}: {e}"))?;
                let ops = serde_json::from_str(text).map_err(|e| format!("step {version}: {e}"))?;
                Ok((v, ops))
            })
            .collect::<Result<Vec<_>, String>>()?;
        steps.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(steps)
    }

    /// The layout files are written in: this tangul's version, or a later step's
    /// (one made while that version is developed).
    pub fn current(self) -> Version {
        let app = Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate's version");
        self.parsed()
            .ok()
            .and_then(|s| s.last().map(|(v, _)| v.clone()))
            .map_or(app.clone(), |step| step.max(app))
    }

    /// Runs on `file` (layout `from`; `None`: before versions) the steps of every
    /// later layout.
    pub fn apply(self, file: &mut Value, from: Option<&Version>) -> Result<(), String> {
        for (version, ops) in self.parsed()? {
            if from.is_none_or(|from| version > *from) {
                for op in &ops {
                    op.apply(file);
                }
            }
        }
        Ok(())
    }
}

impl Op {
    fn apply(&self, file: &mut Value) {
        match self {
            Op::Move(MoveOp { from, to }) => {
                if let Some(value) = take(file, from)
                    && get(file, to).is_none()
                {
                    set(file, to, value);
                }
            }
            Op::Remove(RemoveOp { remove }) => {
                take(file, remove);
            }
            Op::Merge(MergeOp {
                merge,
                into,
                strictest,
            }) => {
                let rank = |v: Option<&Value>| {
                    let text = v.and_then(Value::as_str)?;
                    strictest.iter().position(|s| s == text)
                };
                let Some(value) = take(file, merge) else {
                    return;
                };
                let (from, to) = (rank(Some(&value)), rank(get(file, into)).or(Some(0)));
                if from > to {
                    set(file, into, value);
                }
            }
            Op::Map(MapOp { map, values }) => {
                let new = get(file, map)
                    .and_then(Value::as_str)
                    .and_then(|old| values.get(old))
                    .cloned();
                if let Some(new) = new {
                    set(file, map, new);
                }
            }
        }
    }
}

fn get<'a>(file: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(file, |v, key| v.get(key))
}

/// Removes the value at `path` and returns it.
fn take(file: &mut Value, path: &str) -> Option<Value> {
    let (parent, last) = match path.rsplit_once('.') {
        Some((parent, last)) => (parent, last),
        None => ("", path),
    };
    let mut object = file;
    if !parent.is_empty() {
        for key in parent.split('.') {
            object = object.get_mut(key)?;
        }
    }
    object.as_object_mut()?.remove(last)
}

/// Sets `path` to `value`, making objects on the way; not where a value on the
/// way isn't an object.
fn set(file: &mut Value, path: &str, value: Value) {
    let mut keys: Vec<&str> = path.split('.').collect();
    let Some(last) = keys.pop() else {
        return;
    };
    let mut object = file;
    for key in keys {
        let Some(map) = object.as_object_mut() else {
            return;
        };
        object = map.entry(key).or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(map) = object.as_object_mut() {
        map.insert(last.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(ops: Value, mut file: Value) -> Value {
        let ops: Vec<Op> = serde_json::from_value(ops).unwrap();
        for op in &ops {
            op.apply(&mut file);
        }
        file
    }

    #[test]
    fn operations_move_remove_and_map_and_skip_what_is_missing() {
        let ops = json!([
            { "move": "ai.redact", "to": "ai.content_blocking.redact" },
            { "move": "ai.hide", "to": "ai.content_blocking.hide" },
            { "remove": "ai.show" },
            { "map": "ai.block_queries", "values": { "none": [], "redacted": ["redacted"] } },
        ]);
        let old = json!({"ai": {"redact": ["*.x"], "show": [], "block_queries": "redacted",
                                "content_blocking": {"hide": ["kept"]}, "hide": ["dropped"]}});
        assert_eq!(
            run(ops.clone(), old),
            json!({"ai": {"block_queries": ["redacted"],
                          "content_blocking": {"hide": ["kept"], "redact": ["*.x"]}}})
        );
        // Nothing to do on a file that has none of it.
        assert_eq!(run(ops, json!({"other": 1})), json!({"other": 1}));
        // Not through a value that isn't an object.
        assert_eq!(
            run(
                json!([{ "move": "a", "to": "b.c" }]),
                json!({"a": 1, "b": 2})
            ),
            json!({"b": 2})
        );
    }

    #[test]
    fn a_merge_keeps_the_stricter_value() {
        let op = json!([{ "merge": "mcp.access", "into": "ai.access",
                          "strictest": ["data", "schema"] }]);
        let merged = |file: Value| run(op.clone(), file);
        assert_eq!(
            merged(json!({"ai": {"access": "data"}, "mcp": {"access": "schema", "port": 1}})),
            json!({"ai": {"access": "schema"}, "mcp": {"port": 1}})
        );
        assert_eq!(
            merged(json!({"ai": {"access": "schema"}, "mcp": {"access": "data"}})),
            json!({"ai": {"access": "schema"}, "mcp": {}})
        );
        assert_eq!(
            merged(json!({"mcp": {"access": "schema"}})),
            json!({"mcp": {}, "ai": {"access": "schema"}}),
            "a missing ai.access is the loosest"
        );
        assert_eq!(
            merged(json!({"ai": {"access": "data"}, "mcp": {"access": "weird"}})),
            json!({"ai": {"access": "data"}, "mcp": {}})
        );
        // Twice is once.
        assert_eq!(
            merged(json!({"ai": {"access": "schema"}})),
            json!({"ai": {"access": "schema"}})
        );
    }

    #[test]
    fn unknown_operations_are_refused() {
        for op in [
            json!({"rename": "a", "to": "b"}),
            json!({"move": "a"}),
            json!({"remove": "a", "x": 1}),
        ] {
            assert!(serde_json::from_value::<Op>(op.clone()).is_err(), "{op}");
        }
    }

    #[test]
    fn steps_run_in_version_order_on_older_files_only() {
        let steps = Steps(&[
            ("0.2.0", r#"[{ "move": "b", "to": "c" }]"#),
            ("0.1.3-dev.2", r#"[{ "move": "a", "to": "b" }]"#),
            (
                "0.1.3-dev.10",
                r#"[{ "map": "b", "values": { "x": "y" } }]"#,
            ),
        ]);
        let run = |from: Option<&str>| {
            let mut file = json!({"a": "x"});
            steps
                .apply(&mut file, from.map(|v| Version::parse(v).unwrap()).as_ref())
                .unwrap();
            file
        };
        assert_eq!(run(None), json!({"c": "y"}), "a, then b, then the map");
        assert_eq!(run(Some("0.1.2")), json!({"c": "y"}));
        assert_eq!(
            run(Some("0.1.3-dev.2")),
            json!({"a": "x"}),
            "only the later two"
        );
        assert_eq!(run(Some("0.2.0")), json!({"a": "x"}));
        assert_eq!(steps.current(), Version::parse("0.2.0").unwrap());
        assert!(Steps(&[("1.0", "[]")]).apply(&mut json!({}), None).is_err());
    }

    /// Every step file is valid, and every released layout, as its release wrote
    /// it, reaches the current one and loads there.
    #[test]
    fn every_step_parses_and_every_released_layout_upgrades_and_loads() {
        SETTINGS.parsed().unwrap();
        DATABASE.parsed().unwrap();
        assert!(!SETTINGS_FIXTURES.is_empty());
        for (version, text) in SETTINGS_FIXTURES {
            let up = crate::settings::upgrade(text).unwrap_or_else(|e| panic!("{version}: {e}"));
            crate::settings::Settings::parse(&up).unwrap_or_else(|e| panic!("{version}: {e}"));
        }
        for (version, text) in DATABASE_FIXTURES {
            let up = crate::database_settings::upgrade(text)
                .unwrap_or_else(|e| panic!("{version}: {e}"));
            crate::database_settings::parse(&up).unwrap_or_else(|e| panic!("{version}: {e}"));
        }
    }
}
