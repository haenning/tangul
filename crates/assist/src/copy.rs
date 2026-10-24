//! Rows of a result as text in other formats than TSV ([`crate::mark::tsv`]): CSV,
//! JSON, a Markdown table and, for one column, a list for `IN (…)`. INSERT
//! statements are [`crate::edit::RowEdit::insert`]. Like `tsv`, each takes rows and
//! columns in grid order, a cell reader and every column's name by column number.
//!
//! Values arrive as text (text protocol), so JSON writes them as strings.

use crate::dialect::{Dialect, literal};

/// CSV (RFC 4180), with a header line when `names` are given: fields with commas,
/// quotes or line breaks quoted (quotes doubled), NULL empty. Lines end in `\n`.
pub fn csv<'a>(
    rows: &[usize],
    cols: &[usize],
    cell: impl Fn(usize, usize) -> Option<&'a str>,
    names: Option<&[String]>,
) -> String {
    let field = |v: Option<&str>| match v {
        Some(v) if v.contains([',', '"', '\n', '\r']) => format!("\"{}\"", v.replace('"', "\"\"")),
        Some(v) => v.to_string(),
        None => String::new(),
    };
    let mut lines: Vec<String> = names
        .map(|names| {
            cols.iter()
                .map(|&c| field(names.get(c).map(String::as_str)))
                .collect::<Vec<_>>()
                .join(",")
        })
        .into_iter()
        .collect();
    for &r in rows {
        lines.push(
            cols.iter()
                .map(|&c| field(cell(r, c)))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    lines.join("\n")
}

/// An array of objects, one per row on its own line, keyed by column name (a
/// repeated name gets `_2`, `_3`, …); values as strings, NULL as `null`.
pub fn json<'a>(
    rows: &[usize],
    cols: &[usize],
    cell: impl Fn(usize, usize) -> Option<&'a str>,
    names: &[String],
) -> String {
    let keys = unique(
        cols.iter()
            .map(|&c| names.get(c).map_or("", String::as_str)),
    );
    let objects: Vec<String> = rows
        .iter()
        .map(|&r| {
            let object: serde_json::Map<String, serde_json::Value> = keys
                .iter()
                .zip(cols)
                .map(|(key, &c)| {
                    (
                        key.clone(),
                        cell(r, c).map_or(serde_json::Value::Null, Into::into),
                    )
                })
                .collect();
            format!("  {}", serde_json::Value::Object(object))
        })
        .collect();
    if objects.is_empty() {
        return "[]".into();
    }
    format!("[\n{}\n]", objects.join(",\n"))
}

/// A Markdown table: `|` escaped, line breaks as `<br>`, NULL as `NULL`.
pub fn markdown<'a>(
    rows: &[usize],
    cols: &[usize],
    cell: impl Fn(usize, usize) -> Option<&'a str>,
    names: &[String],
) -> String {
    let field = |v: &str| {
        v.replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace("\r\n", "<br>")
            .replace(['\n', '\r'], "<br>")
    };
    let line = |fields: Vec<String>| format!("| {} |", fields.join(" | "));
    let mut lines = vec![
        line(
            cols.iter()
                .map(|&c| field(names.get(c).map_or("", String::as_str)))
                .collect(),
        ),
        line(cols.iter().map(|_| "---".to_string()).collect()),
    ];
    for &r in rows {
        lines.push(line(
            cols.iter()
                .map(|&c| cell(r, c).map_or_else(|| "NULL".into(), field))
                .collect(),
        ));
    }
    lines.join("\n")
}

/// The values of one column as literals for `IN (…)`, without the parentheses:
/// each value once, NULLs left out (`IN (NULL)` never matches). Numbers stay bare
/// for a `numeric` column.
pub fn in_list<'a>(
    values: impl IntoIterator<Item = Option<&'a str>>,
    dialect: &dyn Dialect,
    numeric: bool,
) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for v in values.into_iter().flatten() {
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.iter()
        .map(|v| literal(dialect, v, numeric))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The names in order, a repeated one with `_2`, `_3`, … (never clashing with
/// another name).
fn unique<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let names: Vec<&str> = names.collect();
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for name in &names {
        let mut key = name.to_string();
        let mut n = 1;
        while out.contains(&key) {
            n += 1;
            key = format!("{name}_{n}");
        }
        out.push(key);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::dialect_for;
    use tangul_core::DriverKind;

    const ROWS: [[Option<&str>; 3]; 3] = [
        [Some("1"), Some("plain"), None],
        [Some("2"), Some("a,b \"q\""), Some("line\nbreak")],
        [Some("3"), Some("pipe | ü"), Some("")],
    ];

    fn cell(r: usize, c: usize) -> Option<&'static str> {
        ROWS[r][c]
    }

    fn names() -> Vec<String> {
        ["id", "note", "extra"].map(String::from).to_vec()
    }

    #[test]
    fn csv_quotes_what_needs_it() {
        assert_eq!(
            csv(&[0, 1, 2], &[0, 1, 2], cell, Some(&names())),
            "id,note,extra\n\
             1,plain,\n\
             2,\"a,b \"\"q\"\"\",\"line\nbreak\"\n\
             3,pipe | ü,"
        );
        // Columns in the order given; no rows: the header alone.
        assert_eq!(csv(&[0], &[1, 0], cell, Some(&names())), "note,id\nplain,1");
        assert_eq!(csv(&[], &[0], cell, Some(&names())), "id");
        // Without names: the values alone.
        assert_eq!(
            csv(&[0, 1], &[0, 1], cell, None),
            "1,plain\n2,\"a,b \"\"q\"\"\""
        );
        assert_eq!(csv(&[], &[0], cell, None), "");
    }

    #[test]
    fn json_is_an_array_of_objects() {
        assert_eq!(
            json(&[0, 1], &[0, 1, 2], cell, &names()),
            "[\n  {\"id\":\"1\",\"note\":\"plain\",\"extra\":null},\n  \
             {\"id\":\"2\",\"note\":\"a,b \\\"q\\\"\",\"extra\":\"line\\nbreak\"}\n]"
        );
        assert_eq!(json(&[], &[0], cell, &names()), "[]");
        // Repeated names stay apart.
        let names = ["a", "a", "a_2"].map(String::from).to_vec();
        assert_eq!(
            json(&[0], &[0, 1, 2], cell, &names),
            "[\n  {\"a\":\"1\",\"a_2\":\"plain\",\"a_2_2\":null}\n]"
        );
    }

    #[test]
    fn markdown_escapes_pipes_and_breaks() {
        assert_eq!(
            markdown(&[0, 1, 2], &[0, 1, 2], cell, &names()),
            "| id | note | extra |\n\
             | --- | --- | --- |\n\
             | 1 | plain | NULL |\n\
             | 2 | a,b \"q\" | line<br>break |\n\
             | 3 | pipe \\| ü |  |"
        );
    }

    #[test]
    fn in_list_per_dialect() {
        let values = [Some("7"), Some("o'k"), None, Some("7")];
        let pg = dialect_for(DriverKind::Postgres);
        assert_eq!(in_list(values, pg, false), "'7', 'o''k'");
        assert_eq!(in_list([Some("7"), Some("8")], pg, true), "7, 8");
        assert_eq!(in_list([None], pg, true), "");
        let my = dialect_for(DriverKind::Mysql);
        assert_eq!(in_list([Some("a\\b")], my, false), "'a\\\\b'");
    }
}
