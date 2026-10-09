//! Editing a database's content-blocking rules (`ai.content_blocking` in its
//! settings file, see [`tangul_core::ai_view`]): what to underline beyond what
//! can't be read, what hovering says, and the hints and names while a pattern is
//! typed. With the database's schema where tangul knows it.

use std::collections::BTreeMap;
use std::ops::Range;

use tangul_core::ai_view::{AiColumns, Pattern, glob};

use crate::ai_view::{Mode, Rules};
use crate::dialect::Namespace;
use crate::json::{self, Member, Node, Problem};
use crate::schema::SchemaIndex;

/// The lists of patterns, by key.
const LISTS: [&str; 2] = ["redact", "hide"];
/// Most tables or columns a hover lists.
const MAX_LISTED: usize = 12;
/// Most names offered while typing.
const MAX_NAMES: usize = 50;

/// What the level above tables is called.
pub fn namespace_word(namespace: Namespace) -> &'static str {
    match namespace {
        Namespace::Schema => "schema",
        Namespace::Database => "database",
    }
}

fn lists(root: &Node) -> impl Iterator<Item = (&'static str, &Member)> {
    let rules = root.get("ai").and_then(|ai| ai.get("content_blocking"));
    LISTS
        .into_iter()
        .filter_map(move |list| Some((list, rules?.member(list)?)))
}

/// Every column of `schema`: (schema, table, column).
fn columns(schema: &SchemaIndex) -> impl Iterator<Item = (&str, &str, &str)> {
    schema.tables().iter().enumerate().flat_map(move |(ix, t)| {
        schema
            .columns(ix)
            .iter()
            .map(move |c| (t.schema.as_str(), t.name.as_str(), c.name.as_str()))
    })
}

/// The columns `pattern` matches, as `schema.table.column`.
fn matched(pattern: &Pattern, schema: &SchemaIndex) -> Vec<String> {
    columns(schema)
        .filter(|(s, t, c)| pattern.matches(s, t, c))
        .map(|(s, t, c)| format!("{s}.{t}.{c}"))
        .collect()
}

/// What looks wrong in patterns that can be read (the file check marks the
/// others): an exception that lifts everything before it, and with `schema`, a
/// pattern that matches nothing.
pub fn problems(root: &Node, schema: Option<&SchemaIndex>) -> Vec<Problem> {
    let mut out = Vec::new();
    for (list, member) in lists(root) {
        for item in member.value.items() {
            let Some(pattern) = item.as_str().and_then(|t| Pattern::parse(t).ok()) else {
                continue;
            };
            if pattern.except && pattern.is_everything() {
                out.push(Problem::warning(
                    item.range.clone(),
                    format!(
                        "lifts every pattern before it: nothing above is {} any more",
                        done(list)
                    ),
                ));
            } else if let Some(schema) = schema
                && matched(&pattern, schema).is_empty()
            {
                out.push(Problem::warning(
                    item.range.clone(),
                    "matches no column of this database (as tangul last read it)",
                ));
            }
        }
    }
    out
}

fn done(list: &str) -> &'static str {
    match list {
        "hide" => "hidden",
        _ => "redacted",
    }
}

/// What hovering at `offset` shows (Markdown), and over which range: on `redact`
/// or `hide`, every column the file leaves that way; on a pattern, what it means
/// and what it matches.
pub fn hover(
    root: &Node,
    offset: usize,
    schema: Option<&SchemaIndex>,
    namespace: Namespace,
) -> Option<(Range<usize>, String)> {
    for (list, member) in lists(root) {
        if member.key_range.contains(&offset) {
            return Some((member.key_range.clone(), summary(root, list, schema)));
        }
        if let Some(item) = member
            .value
            .items()
            .iter()
            .find(|i| i.range.contains(&offset))
        {
            let text = explain(list, item.as_str()?, schema, namespace);
            return Some((item.range.clone(), text));
        }
    }
    None
}

/// Every column the file leaves as `list` says (hidden beats redacted), by table.
fn summary(root: &Node, list: &str, schema: Option<&SchemaIndex>) -> String {
    let done = done(list);
    let rules = root
        .get("ai")
        .and_then(|ai| ai.get("content_blocking"))
        .map_or(Ok(AiColumns::default()), |n| {
            serde_json::from_value(n.value())
        });
    let Ok(rules) = rules else {
        return "The rules can't be read: AI tools see no rows until they are fixed.".into();
    };
    let Some(schema) = schema else {
        return format!("Connect to this database to see which columns are {done}.");
    };
    let rules = Rules::new(&rules);
    let mode = if list == "hide" {
        Mode::Hidden
    } else {
        Mode::Redacted
    };
    let mut tables: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (s, t, c) in columns(schema) {
        if rules.mode(s, t, c) == mode {
            tables.entry(format!("{s}.{t}")).or_default().push(c);
        }
    }
    if tables.is_empty() {
        return format!("Nothing in this database is {done} for AI tools.");
    }
    let count: usize = tables.values().map(Vec::len).sum();
    let mut text = format!(
        "**{} for AI tools:** {}, {}\n",
        capitalized(done),
        plural(count, "column"),
        plural(tables.len(), "table")
    );
    for (table, cols) in tables.iter().take(MAX_LISTED) {
        text.push_str(&format!("\n- `{table}`: {}", cols.join(", ")));
    }
    if tables.len() > MAX_LISTED {
        let more = tables.len() - MAX_LISTED;
        text.push_str(&format!("\n- … and {}", plural(more, "more table")));
    }
    text
}

/// What the pattern `text` in `list` means, and what it matches.
fn explain(list: &str, text: &str, schema: Option<&SchemaIndex>, namespace: Namespace) -> String {
    let pattern = match Pattern::parse(text) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let word = namespace_word(namespace);
    let name = |part: &str, one: &str, many: &str, every: &str| match part {
        "*" => every.to_string(),
        p if p.contains('*') => format!("the {many} `{p}`"),
        p => format!("the {one} `{p}`"),
    };
    let cols = match pattern.column.as_deref() {
        None => "all columns".to_string(),
        Some(c) => name(c, "column", "columns", "every column"),
    };
    let tables = name(&pattern.table, "table", "tables", "every table");
    let schemas = name(
        &pattern.schema,
        word,
        &format!("{word}s"),
        &format!("every {word}"),
    );
    let what = format!("{cols} of {tables} in {schemas}");
    let done = done(list);
    let mut out = if pattern.except {
        format!(
            "**Exception:** {what} aren't {done} by the patterns before it (a later one can \
             again)."
        )
    } else {
        format!(
            "**{}** {what}.",
            if list == "hide" { "Hides" } else { "Redacts" }
        )
    };
    match schema {
        None => out.push_str("\n\nConnect to this database to see the columns it matches."),
        Some(schema) => {
            let hits = matched(&pattern, schema);
            if hits.is_empty() {
                out.push_str("\n\nMatches nothing in this database (as tangul last read it).");
            } else {
                let shown: Vec<String> = hits
                    .iter()
                    .take(MAX_LISTED)
                    .map(|c| format!("`{c}`"))
                    .collect();
                out.push_str(&format!(
                    "\n\nMatches {}: {}",
                    plural(hits.len(), "column"),
                    shown.join(", ")
                ));
                if hits.len() > MAX_LISTED {
                    out.push_str(&format!(" and {} more", hits.len() - MAX_LISTED));
                }
            }
        }
    }
    out
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A pattern being typed in `redact` or `hide`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typing {
    /// The parts before the cursor's (`schema`, `table`), as typed.
    pub parts: Vec<String>,
    /// The cursor's part as typed up to it, and where that starts.
    pub prefix: String,
    pub prefix_start: usize,
    /// Whether nothing follows the cursor in the pattern.
    pub at_end: bool,
}

impl Typing {
    /// 0: schema, 1: table, 2: column.
    pub fn part(&self) -> usize {
        self.parts.len()
    }
}

/// The pattern the cursor at `offset` is typing, if it is in one.
pub fn typing(text: &str, offset: usize) -> Option<Typing> {
    let at = json::string_at(text, offset)?;
    let [ai, rules, list] = at.path.as_slice() else {
        return None;
    };
    if ai != "ai" || rules != "content_blocking" || !LISTS.contains(&list.as_str()) {
        return None;
    }
    let typed = text[at.start..offset].trim_start();
    let typed = typed.strip_prefix('!').unwrap_or(typed).trim_start();
    let mut parts: Vec<String> = typed.split('.').map(str::to_string).collect();
    let prefix = parts.pop().unwrap_or_default();
    Some(Typing {
        parts,
        prefix_start: offset - prefix.len(),
        prefix,
        at_end: at.after.is_empty(),
    })
}

/// The parts still to type, as a hint after the cursor: `schema.table.column` in
/// an empty pattern, `.table.column` once the schema has a letter, and so on.
pub fn placeholder(typing: &Typing, namespace: Namespace) -> Option<String> {
    let words = [namespace_word(namespace), "table", "column"];
    let part = typing.part();
    if !typing.at_end || part >= words.len() {
        return None;
    }
    if typing.prefix.is_empty() {
        Some(words[part..].join("."))
    } else if part + 1 < words.len() {
        Some(format!(".{}", words[part + 1..].join(".")))
    } else {
        None
    }
}

/// Names of `schema` for the cursor's part that start with what is typed (once
/// something is), within the parts before it: schemas, tables or columns.
pub fn names(typing: &Typing, schema: &SchemaIndex) -> Vec<String> {
    let prefix = typing.prefix.trim().to_lowercase();
    if prefix.is_empty() || prefix.contains('*') {
        return Vec::new();
    }
    let part = |i: usize| match typing.parts.get(i).map(|p| p.trim()) {
        None | Some("") => "*".to_string(),
        Some(p) => p.to_lowercase(),
    };
    let (s, t) = (part(0), part(1));
    let found: Vec<&str> = match typing.part() {
        0 => schema.tables().iter().map(|t| t.schema.as_str()).collect(),
        1 => schema
            .tables()
            .iter()
            .filter(|table| glob(&s, &table.schema))
            .map(|t| t.name.as_str())
            .collect(),
        2 => columns(schema)
            .filter(|(cs, ct, _)| glob(&s, cs) && glob(&t, ct))
            .map(|(_, _, c)| c)
            .collect(),
        _ => Vec::new(),
    };
    let mut names: Vec<String> = found
        .into_iter()
        .filter(|n| n.to_lowercase().starts_with(&prefix))
        .map(str::to_string)
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by_key(|n| n.to_lowercase());
    names.truncate(MAX_NAMES);
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Severity;

    fn schema() -> SchemaIndex {
        crate::schema::tests::sample()
    }

    fn file(redact: &str, hide: &str) -> String {
        format!(r#"{{"ai": {{"content_blocking": {{"redact": [{redact}], "hide": [{hide}]}}}}}}"#)
    }

    fn at(text: &str, needle: &str) -> usize {
        text.find(needle).unwrap() + 1
    }

    #[test]
    fn sample_has_what_the_tests_use() {
        let s = schema();
        let all: Vec<_> = columns(&s).collect();
        assert!(all.contains(&("app", "users", "email")), "{all:?}");
        assert!(all.contains(&("app", "orders", "user_id")), "{all:?}");
    }

    #[test]
    fn patterns_that_match_nothing_or_lift_everything_are_warned() {
        let text = file(r#""..email", ".nope", "!..""#, r#""..ssn_x""#);
        let root = json::parse(&text).unwrap();
        let problems = problems(&root, Some(&schema()));
        let marked: Vec<&str> = problems.iter().map(|p| &text[p.range.clone()]).collect();
        assert_eq!(marked, [r#"".nope""#, r#""!..""#, r#""..ssn_x""#]);
        assert!(problems.iter().all(|p| p.severity == Severity::Warning));
        assert!(problems[1].message.contains("nothing above is redacted"));
        // Without the schema, only what the text says.
        assert_eq!(problems_without(&text), 1);
    }

    fn problems_without(text: &str) -> usize {
        problems(&json::parse(text).unwrap(), None).len()
    }

    #[test]
    fn hovering_a_list_sums_it_up_and_a_pattern_explains_itself() {
        let text = file(r#""..email", "app.orders.""#, r#""!..id", ".orders""#);
        let root = json::parse(&text).unwrap();
        let s = schema();
        let hover = |needle: &str| {
            hover(&root, at(&text, needle), Some(&s), Namespace::Schema).map(|(_, t)| t)
        };
        let redact = hover("\"redact\"").unwrap();
        assert!(redact.starts_with("**Redacted for AI tools:**"), "{redact}");
        assert!(redact.contains("`app.users`: email"), "{redact}");
        assert!(
            !redact.contains("app.orders"),
            "hidden beats redacted: {redact}"
        );
        let hide = hover("\"hide\"").unwrap();
        assert!(hide.contains("`app.orders`: id"), "{hide}");
        let email = hover("\"..email\"").unwrap();
        assert!(
            email.starts_with(
                "**Redacts** the column `email` of every table in every schema.\n\nMatches"
            ),
            "{email}"
        );
        let id = hover("\"!..id\"").unwrap();
        assert!(id.starts_with("**Exception:** the column `id`"), "{id}");
        assert!(id.contains("aren't hidden"), "{id}");
        let orders = hover("\"app.orders.\"").unwrap();
        assert!(
            orders
                .starts_with("**Redacts** every column of the table `orders` in the schema `app`."),
            "{orders}"
        );
        let (range, mysql) =
            super::hover(&root, at(&text, "\".orders\""), None, Namespace::Database).unwrap();
        assert_eq!(&text[range], "\".orders\"");
        assert!(
            mysql.contains("all columns of the table `orders` in every database"),
            "{mysql}"
        );
        assert!(mysql.contains("Connect to this database"), "{mysql}");
        assert!(super::hover(&root, 0, Some(&s), Namespace::Schema).is_none());
    }

    #[test]
    fn a_list_that_cant_be_read_says_so() {
        let text = file(r#""users""#, "");
        let root = json::parse(&text).unwrap();
        let (_, summary) = hover(&root, at(&text, "\"redact\""), None, Namespace::Schema).unwrap();
        assert!(summary.contains("can't be read"), "{summary}");
        let (_, item) = hover(&root, at(&text, "\"users\""), None, Namespace::Schema).unwrap();
        assert!(item.contains("has no dot"), "{item}");
    }

    #[test]
    fn typing_a_pattern_hints_the_parts_left_and_offers_names() {
        let typed = |pattern: &str| {
            let text = format!(r#"{{"ai": {{"content_blocking": {{"hide": ["a.b", "{pattern}"#);
            typing(&text, text.len())
        };
        let hint = |pattern: &str| placeholder(&typed(pattern).unwrap(), Namespace::Schema);
        assert_eq!(hint("").as_deref(), Some("schema.table.column"));
        assert_eq!(hint("!").as_deref(), Some("schema.table.column"));
        assert_eq!(hint("ap").as_deref(), Some(".table.column"));
        assert_eq!(hint("app.").as_deref(), Some("table.column"));
        assert_eq!(hint("app.us").as_deref(), Some(".column"));
        assert_eq!(hint("app.users.").as_deref(), Some("column"));
        assert_eq!(hint("app.users.em"), None);
        let mysql = placeholder(&typed("").unwrap(), Namespace::Database);
        assert_eq!(mysql.as_deref(), Some("database.table.column"));

        let s = schema();
        assert_eq!(names(&typed("ap").unwrap(), &s), ["app"]);
        assert!(names(&typed("").unwrap(), &s).is_empty(), "only once typed");
        let tables = names(&typed("app.or").unwrap(), &s);
        assert!(tables.contains(&"orders".to_string()), "{tables:?}");
        let cols = names(&typed("..ema").unwrap(), &s);
        assert_eq!(cols, ["email"]);
        let t = typed("..ema").unwrap();
        assert_eq!(t.prefix, "ema");

        // Not in a pattern list.
        let other = r#"{"ai": {"content_blocking": {"block_queries": [""#;
        assert!(typing(other, other.len()).is_none());
        // Text after the cursor: no hint.
        let text = r#"{"ai": {"content_blocking": {"hide": ["pub"]}}}"#;
        let t = typing(text, text.find("ub").unwrap()).unwrap();
        assert_eq!(placeholder(&t, Namespace::Schema), None);
    }
}
