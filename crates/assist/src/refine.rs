//! Filtering, sorting and paging a result without touching the user's SQL: the
//! query runs again as a subquery,
//!
//! ```sql
//! SELECT * FROM (
//! <query>
//! ) AS tangul_q WHERE … ORDER BY … LIMIT … OFFSET …
//! ```
//!
//! which works for any single SELECT, however complex (joins, CTEs, GROUP BY,
//! window functions). The subquery's own ORDER BY holds when the outer query adds
//! none (Postgres keeps the order of a subquery scan; MySQL passes a derived table's
//! ORDER BY on to an outer query without one), so pages follow the original order.

use tangul_core::DriverKind;

use crate::analyze::statements;
use crate::lexer::{TokenKind, tokenize};
use crate::related::{literal, numeric_type};
use crate::schema::quote_ident;

const ALIAS: &str = "tangul_q";

/// What a column holds, which decides the filters offered for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Number,
    Bool,
    Date,
    /// Date with time (`timestamp`, `datetime`).
    DateTime,
    Text,
}

impl ColumnKind {
    /// From a type as the server spells it.
    pub fn from_type(data_type: &str) -> Self {
        let t = data_type.to_ascii_lowercase();
        if t == "boolean" || t == "bool" || t.starts_with("tinyint(1)") {
            Self::Bool
        } else if t.starts_with("timestamp") || t.starts_with("datetime") {
            Self::DateTime
        } else if t == "date" {
            Self::Date
        } else if numeric_type(&t) {
            Self::Number
        } else {
            Self::Text
        }
    }

    /// Guessed from the values (for expressions, whose type the schema doesn't
    /// know): the narrowest kind all non-NULL values fit.
    pub fn from_values<'a>(values: impl Iterator<Item = Option<&'a str>>) -> Self {
        let (mut bool, mut date, mut datetime, mut number, mut any) =
            (true, true, true, true, false);
        for v in values.flatten() {
            any = true;
            bool &= matches!(v, "t" | "f" | "true" | "false");
            date &= is_date(v) && v.len() == 10;
            datetime &= is_date(v)
                && v.len() > 10
                && matches!(v.as_bytes()[10], b' ' | b'T')
                && v.as_bytes().get(11).is_some_and(u8::is_ascii_digit);
            number &= v.trim().parse::<f64>().is_ok();
            if !(bool || date || datetime || number) {
                return Self::Text;
            }
        }
        match (any, bool, date, datetime, number) {
            (false, ..) => Self::Text,
            (_, true, ..) => Self::Bool,
            (_, _, true, ..) => Self::Date,
            (_, _, _, true, _) => Self::DateTime,
            (_, _, _, _, true) => Self::Number,
            _ => Self::Text,
        }
    }

    pub fn is_date(self) -> bool {
        matches!(self, Self::Date | Self::DateTime)
    }

    /// The filter operators that make sense for the kind.
    pub fn ops(self) -> &'static [&'static [FilterOp]] {
        use FilterOp::*;
        match self {
            Self::Number => &[&[Eq, Ne, Lt, Le, Gt, Ge, Between], &[IsNull, NotNull]],
            Self::Bool => &[&[IsTrue, IsFalse], &[IsNull, NotNull]],
            Self::Date | Self::DateTime => &[&[Between], &[IsNull, NotNull]],
            Self::Text => &[
                &[Contains, NotContains, StartsWith, EndsWith],
                &[Eq, Ne],
                &[IsNull, NotNull],
            ],
        }
    }

    pub fn default_op(self) -> FilterOp {
        self.ops()[0][0]
    }
}

/// Starts with `YYYY-MM-DD`.
fn is_date(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    IsNull,
    NotNull,
    IsTrue,
    IsFalse,
    /// From `value` to `to`, both included; either may be empty (open end).
    Between,
}

impl FilterOp {
    pub fn label(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "≠",
            Self::Lt => "<",
            Self::Le => "≤",
            Self::Gt => ">",
            Self::Ge => "≥",
            Self::Contains => "contains",
            Self::NotContains => "doesn't contain",
            Self::StartsWith => "starts with",
            Self::EndsWith => "ends with",
            Self::IsNull => "is NULL",
            Self::NotNull => "is not NULL",
            Self::IsTrue => "is true",
            Self::IsFalse => "is false",
            Self::Between => "from … to",
        }
    }

    /// Whether the operator compares with a value.
    pub fn takes_value(self) -> bool {
        !matches!(
            self,
            Self::IsNull | Self::NotNull | Self::IsTrue | Self::IsFalse
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnFilter {
    /// Result column index.
    pub column: usize,
    pub op: FilterOp,
    pub value: String,
    /// Upper end of [`FilterOp::Between`].
    pub to: String,
}

impl ColumnFilter {
    /// Short description for the filter chip, e.g. `name contains ann`.
    pub fn describe(&self, name: &str) -> String {
        match self.op {
            FilterOp::Between => match (self.value.is_empty(), self.to.is_empty()) {
                (false, false) => format!("{name} {} – {}", self.value, self.to),
                (false, true) => format!("{name} ≥ {}", self.value),
                (true, false) => format!("{name} ≤ {}", self.to),
                (true, true) => format!("{name} (any)"),
            },
            op if op.takes_value() => format!("{name} {} {}", op.label(), self.value),
            op => format!("{name} {}", op.label()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub descending: bool,
}

/// Filters (combined with AND) and sort order applied to a result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Refine {
    pub filters: Vec<ColumnFilter>,
    pub sort: Option<Sort>,
}

impl Refine {
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty() && self.sort.is_none()
    }

    pub fn filter(&self, column: usize) -> Option<&ColumnFilter> {
        self.filters.iter().find(|f| f.column == column)
    }

    /// Sets the one filter of `filter.column`.
    pub fn set_filter(&mut self, filter: ColumnFilter) {
        match self.filters.iter_mut().find(|f| f.column == filter.column) {
            Some(f) => *f = filter,
            None => self.filters.push(filter),
        }
    }

    pub fn remove_filter(&mut self, column: usize) {
        self.filters.retain(|f| f.column != column);
    }
}

/// The statement to wrap, if `sql` is a single query that can run again safely:
/// one statement, starting with SELECT / WITH / TABLE / VALUES, without anything
/// that writes (`WITH … DELETE`, `SELECT … INTO`, `FOR UPDATE` locks).
pub fn refinable(sql: &str) -> Option<String> {
    let mut parts = statements(sql)
        .into_iter()
        .map(|(s, e)| sql[s..e].trim())
        .filter(|s| !s.is_empty());
    let stmt = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let words: Vec<&str> = tokenize(stmt)
        .into_iter()
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(stmt))
        .collect();
    let first = words.first()?;
    let is = |w: &str, k: &str| w.eq_ignore_ascii_case(k);
    if !["select", "with", "table", "values"]
        .iter()
        .any(|k| is(first, k))
    {
        return None;
    }
    const WRITES: [&str; 8] = [
        "insert", "update", "delete", "merge", "into", "call", "lock", "share",
    ];
    if words.iter().any(|w| WRITES.iter().any(|k| is(w, k))) {
        return None;
    }
    Some(stmt.to_string())
}

/// `base` (from [`refinable`]) filtered, sorted and cut to `limit` rows from
/// `offset`. `columns` are the result's column names, `kinds` what they hold.
pub fn refined_sql(
    base: &str,
    columns: &[String],
    kinds: &[ColumnKind],
    refine: &Refine,
    driver: DriverKind,
    offset: usize,
    limit: usize,
) -> String {
    // Duplicate names (`a.id, b.id`) can't be told apart outside the subquery, and
    // MySQL refuses them in a derived table: rename them there.
    let names = unique_names(columns);
    let renamed = names.iter().zip(columns).any(|(n, c)| n != c);
    let col = |i: usize| {
        format!(
            "{ALIAS}.{}",
            quote_ident(driver, names.get(i).map_or("", String::as_str))
        )
    };
    let mut sql = format!("SELECT * FROM (\n{base}\n) AS {ALIAS}");
    if renamed {
        let list: Vec<String> = names.iter().map(|n| quote_ident(driver, n)).collect();
        sql.push_str(&format!(" ({})", list.join(", ")));
    }
    let conditions: Vec<String> = refine
        .filters
        .iter()
        .filter(|f| f.column < columns.len())
        .map(|f| {
            condition(
                &col(f.column),
                f,
                kinds.get(f.column).copied().unwrap_or(ColumnKind::Text),
                driver,
            )
        })
        .collect();
    if !conditions.is_empty() {
        sql.push_str(&format!("\nWHERE {}", conditions.join("\n  AND ")));
    }
    if let Some(sort) = refine.sort.filter(|s| s.column < columns.len()) {
        let dir = if sort.descending { "DESC" } else { "ASC" };
        sql.push_str(&format!("\nORDER BY {} {dir}", col(sort.column)));
    }
    sql.push_str(&format!("\nLIMIT {limit}"));
    if offset > 0 {
        sql.push_str(&format!(" OFFSET {offset}"));
    }
    sql
}

fn condition(col: &str, f: &ColumnFilter, kind: ColumnKind, driver: DriverKind) -> String {
    let numeric = kind == ColumnKind::Number;
    let compare = |op: &str| format!("{col} {op} {}", literal(&f.value, numeric, driver));
    // LIKE on the text of any type, case-insensitive (MySQL's usual collations
    // already are). `!` escapes wildcards: no backslash rules to get wrong.
    let like = |not: bool, prefix: &str, suffix: &str| {
        let pattern = format!(
            "{prefix}{}{suffix}",
            f.value
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        let (text, op) = match driver {
            DriverKind::Postgres => (format!("CAST({col} AS text)"), "ILIKE"),
            DriverKind::Mysql => (format!("CAST({col} AS CHAR)"), "LIKE"),
            // SQLite's LIKE ignores ASCII case already.
            DriverKind::Sqlite => (format!("CAST({col} AS TEXT)"), "LIKE"),
        };
        let not = if not { "NOT " } else { "" };
        format!(
            "{text} {not}{op} {} ESCAPE '!'",
            literal(&pattern, false, driver)
        )
    };
    match f.op {
        FilterOp::Eq => compare("="),
        FilterOp::Ne => compare("<>"),
        FilterOp::Lt => compare("<"),
        FilterOp::Le => compare("<="),
        FilterOp::Gt => compare(">"),
        FilterOp::Ge => compare(">="),
        FilterOp::Contains => like(false, "%", "%"),
        FilterOp::NotContains => like(true, "%", "%"),
        FilterOp::StartsWith => like(false, "", "%"),
        FilterOp::EndsWith => like(false, "%", ""),
        FilterOp::IsNull => format!("{col} IS NULL"),
        FilterOp::NotNull => format!("{col} IS NOT NULL"),
        FilterOp::IsTrue => format!("{col} IS TRUE"),
        FilterOp::IsFalse => format!("{col} IS FALSE"),
        FilterOp::Between => {
            let mut parts = Vec::new();
            let from = f.value.trim();
            let to = f.to.trim();
            if !from.is_empty() {
                parts.push(format!("{col} >= {}", literal(from, numeric, driver)));
            }
            if !to.is_empty() {
                let lit = literal(to, numeric, driver);
                // A day as the end of a timestamp range means the whole day.
                parts.push(if kind == ColumnKind::DateTime && to.len() == 10 {
                    match driver {
                        DriverKind::Postgres => format!("{col} < CAST({lit} AS date) + 1"),
                        DriverKind::Mysql => format!("{col} < {lit} + INTERVAL 1 DAY"),
                        DriverKind::Sqlite => format!("{col} < date({lit}, '+1 day')"),
                    }
                } else {
                    format!("{col} <= {lit}")
                });
            }
            if parts.is_empty() {
                "TRUE".into()
            } else {
                parts.join(" AND ")
            }
        }
    }
}

/// `id, id` → `id, id_2` (skipping names already taken).
fn unique_names(columns: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(columns.len());
    for name in columns {
        let taken = |n: &str, out: &[String]| out.iter().any(|o| o.eq_ignore_ascii_case(n));
        let mut candidate = name.clone();
        let mut n = 2;
        while taken(&candidate, &out) || (candidate != *name && columns.contains(&candidate)) {
            candidate = format!("{name}_{n}");
            n += 1;
        }
        out.push(candidate);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_single_read_queries() {
        assert_eq!(
            refinable("  select * from t;  ").as_deref(),
            Some("select * from t")
        );
        assert!(refinable("with x as (select 1) select * from x").is_some());
        assert!(refinable("select 1; select 2").is_none());
        assert!(refinable("with d as (delete from t returning *) select * from d").is_none());
        assert!(refinable("select * into t2 from t").is_none());
        assert!(refinable("select * from t for update").is_none());
        assert!(refinable("show tables").is_none());
        assert!(refinable("update t set a = 1").is_none());
        // Words in strings and comments don't count.
        assert!(refinable("select 'delete' as x -- update\nfrom t").is_some());
    }

    #[test]
    fn wraps_with_filters_sort_and_page() {
        let refine = Refine {
            filters: vec![
                ColumnFilter {
                    column: 0,
                    op: FilterOp::Ge,
                    value: "10".into(),
                    to: String::new(),
                },
                ColumnFilter {
                    column: 1,
                    op: FilterOp::Contains,
                    value: "50%_o'k".into(),
                    to: String::new(),
                },
                ColumnFilter {
                    column: 2,
                    op: FilterOp::IsNull,
                    value: String::new(),
                    to: String::new(),
                },
            ],
            sort: Some(Sort {
                column: 1,
                descending: true,
            }),
        };
        let sql = refined_sql(
            "select id, \"Name\", note from t -- trailing comment",
            &cols(&["id", "Name", "note"]),
            &[ColumnKind::Number, ColumnKind::Text, ColumnKind::Text],
            &refine,
            DriverKind::Postgres,
            10_000,
            10_001,
        );
        assert_eq!(
            sql,
            "SELECT * FROM (\nselect id, \"Name\", note from t -- trailing comment\n) AS tangul_q\n\
             WHERE tangul_q.id >= 10\n  \
             AND CAST(tangul_q.\"Name\" AS text) ILIKE '%50!%!_o''k%' ESCAPE '!'\n  \
             AND tangul_q.note IS NULL\n\
             ORDER BY tangul_q.\"Name\" DESC\nLIMIT 10001 OFFSET 10000"
        );
    }

    #[test]
    fn values_and_duplicate_names() {
        let refine = Refine {
            filters: vec![ColumnFilter {
                column: 1,
                op: FilterOp::Eq,
                value: "a\\b".into(),
                to: String::new(),
            }],
            sort: None,
        };
        let sql = refined_sql(
            "select a.id, b.id from a, b",
            &cols(&["id", "id"]),
            &[ColumnKind::Number, ColumnKind::Text],
            &refine,
            DriverKind::Mysql,
            0,
            100,
        );
        assert_eq!(
            sql,
            "SELECT * FROM (\nselect a.id, b.id from a, b\n) AS tangul_q (id, id_2)\n\
             WHERE tangul_q.id_2 = 'a\\\\b'\nLIMIT 100"
        );
        // A non-number for a numeric column stays a quoted string.
        let f = ColumnFilter {
            column: 0,
            op: FilterOp::Lt,
            value: "1; drop".into(),
            to: String::new(),
        };
        assert_eq!(
            condition("c", &f, ColumnKind::Number, DriverKind::Postgres),
            "c < '1; drop'"
        );
        assert_eq!(unique_names(&cols(&["a", "a", "a_2"])), ["a", "a_3", "a_2"]);
    }

    #[test]
    fn kinds_and_ranges() {
        assert_eq!(
            ColumnKind::from_type("timestamp with time zone"),
            ColumnKind::DateTime
        );
        assert_eq!(ColumnKind::from_type("tinyint(1)"), ColumnKind::Bool);
        assert_eq!(ColumnKind::from_type("date"), ColumnKind::Date);
        assert_eq!(ColumnKind::from_type("numeric(10,2)"), ColumnKind::Number);
        assert_eq!(ColumnKind::from_type("jsonb"), ColumnKind::Text);
        let kind = |v: &[Option<&str>]| ColumnKind::from_values(v.iter().copied());
        assert_eq!(kind(&[Some("t"), None, Some("f")]), ColumnKind::Bool);
        assert_eq!(kind(&[Some("2024-01-31")]), ColumnKind::Date);
        assert_eq!(
            kind(&[Some("2024-01-31 10:00:00+01")]),
            ColumnKind::DateTime
        );
        assert_eq!(kind(&[Some("1"), Some("2.5")]), ColumnKind::Number);
        assert_eq!(kind(&[Some("1"), Some("x")]), ColumnKind::Text);
        assert_eq!(kind(&[None]), ColumnKind::Text);

        let f = |value: &str, to: &str| ColumnFilter {
            column: 0,
            op: FilterOp::Between,
            value: value.into(),
            to: to.into(),
        };
        assert_eq!(
            condition(
                "c",
                &f("2024-01-01", "2024-01-31"),
                ColumnKind::DateTime,
                DriverKind::Postgres
            ),
            "c >= '2024-01-01' AND c < CAST('2024-01-31' AS date) + 1"
        );
        assert_eq!(
            condition(
                "c",
                &f("", "2024-01-31"),
                ColumnKind::DateTime,
                DriverKind::Mysql
            ),
            "c < '2024-01-31' + INTERVAL 1 DAY"
        );
        assert_eq!(
            condition(
                "c",
                &f("2024-01-01", "2024-01-31"),
                ColumnKind::Date,
                DriverKind::Postgres
            ),
            "c >= '2024-01-01' AND c <= '2024-01-31'"
        );
        assert_eq!(
            condition("c", &f("5", ""), ColumnKind::Number, DriverKind::Mysql),
            "c >= 5"
        );
        assert_eq!(f("", "").describe("d"), "d (any)");
    }
}
