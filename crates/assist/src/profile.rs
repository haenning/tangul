//! SQL for value profiles: a sample of a column's values (how many are NULL, how
//! many distinct, the range, the most frequent ones). Only what the dialect says
//! about quoting and `LIMIT`; no database named here.

use crate::dialect::{Dialect, quote_ident};

/// Rows a profile looks at: the first ones the server returns, so it stays cheap on
/// big tables (a sample, not the whole table).
pub const SAMPLE_ROWS: usize = 10_000;
/// Most frequent values kept.
pub const TOP_VALUES: usize = 10;
/// Longest value kept; longer ones are cut.
pub const VALUE_CHARS: usize = 64;

/// Columns never read for a profile, whatever the opt-in: they look like secrets.
pub fn is_secret(column: &str) -> bool {
    const SECRETS: &[&str] = &[
        "password",
        "passwd",
        "pwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "hash",
        "salt",
        "private_key",
        "otp",
    ];
    let c = column.to_lowercase();
    SECRETS.iter().any(|s| c.contains(s))
}

fn sample(d: &dyn Dialect, table: &str, column: &str) -> String {
    format!(
        "(SELECT {c} AS v FROM {table} {}) AS tangul_sample",
        d.limit(SAMPLE_ROWS, 0),
        c = quote_ident(d, column)
    )
}

/// Rows, NULLs and distinct values in the sample; with `range`, also the smallest
/// and largest value. `table` is the table as written in SQL.
pub fn counts_sql(d: &dyn Dialect, table: &str, column: &str, range: bool) -> String {
    let range = if range { ", min(v), max(v)" } else { "" };
    format!(
        "SELECT count(*), count(v), count(DISTINCT v){range} FROM {}",
        sample(d, table, column)
    )
}

/// The most frequent non-NULL values in the sample, most frequent first.
pub fn top_values_sql(d: &dyn Dialect, table: &str, column: &str) -> String {
    format!(
        "SELECT v, count(*) FROM {} WHERE v IS NOT NULL GROUP BY v ORDER BY 2 DESC, 1 {}",
        sample(d, table, column),
        d.limit(TOP_VALUES, 0)
    )
}

/// `value` cut to [`VALUE_CHARS`].
pub fn clip(value: &str) -> String {
    match value.char_indices().nth(VALUE_CHARS) {
        Some((i, _)) => format!("{}…", &value[..i]),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use tangul_core::DriverKind;

    use super::*;
    use crate::dialect::dialect_for;

    #[test]
    fn sampled_queries() {
        let pg = dialect_for(DriverKind::Postgres);
        assert_eq!(
            counts_sql(pg, "app.orders", "Status", true),
            "SELECT count(*), count(v), count(DISTINCT v), min(v), max(v) FROM \
             (SELECT \"Status\" AS v FROM app.orders LIMIT 10000) AS tangul_sample"
        );
        let my = dialect_for(DriverKind::Mysql);
        assert_eq!(
            top_values_sql(my, "`order`", "status"),
            "SELECT v, count(*) FROM (SELECT status AS v FROM `order` LIMIT 10000) AS \
             tangul_sample WHERE v IS NOT NULL GROUP BY v ORDER BY 2 DESC, 1 LIMIT 10"
        );
    }

    #[test]
    fn secrets_and_long_values() {
        assert!(is_secret("password_hash") && is_secret("API_KEY") && is_secret("reset_token"));
        assert!(!is_secret("status") && !is_secret("email"));
        assert_eq!(clip("short"), "short");
        assert_eq!(clip(&"x".repeat(70)), format!("{}…", "x".repeat(64)));
    }
}
