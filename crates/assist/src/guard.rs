//! Finds statements worth a second look before they run: ones that change data or
//! schema, and `UPDATE` / `DELETE` without a `WHERE`. Works on tokens, not a full
//! parse, so it is a guardrail and not a guarantee (a function called from a
//! `SELECT` can still write).

use crate::analyze::{significant, statements};
use crate::lexer::{Token, TokenKind};

/// A statement the guard has something to say about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The statement's text, trimmed.
    pub statement: String,
    /// What it does, upper case: `UPDATE`, `DROP`, …
    pub keyword: String,
    /// Changes data or schema.
    pub writes: bool,
    /// An `UPDATE` or `DELETE` without a `WHERE`.
    pub unfiltered: bool,
}

/// Statements that change data, schema, privileges or server state.
const WRITES: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "UPSERT", "TRUNCATE", "DROP", "ALTER",
    "CREATE", "RENAME", "GRANT", "REVOKE", "COMMENT", "COPY", "CALL", "DO", "VACUUM", "REINDEX",
    "CLUSTER", "REFRESH", "LOAD", "IMPORT", "LOCK",
];
/// What a `WITH` or `EXPLAIN ANALYZE` statement can be about.
const DML: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE"];

/// The statements of `sql` that write or are unfiltered; harmless ones are left out.
pub fn check(sql: &str) -> Vec<Check> {
    statements(sql)
        .into_iter()
        .filter_map(|(start, end)| check_statement(&sql[start..end]))
        .collect()
}

fn check_statement(sql: &str) -> Option<Check> {
    let tokens = significant(sql);
    // Words outside any parentheses, with their position among the tokens.
    let mut depth = 0usize;
    let mut top: Vec<(usize, String)> = Vec::new();
    let mut modifying_cte = None;
    let mut analyze = false;
    for (i, t) in tokens.iter().enumerate() {
        if t.is_punct(sql, '(') {
            depth += 1;
            // `WITH x AS (DELETE …)` writes, whatever the main query is.
            if let Some(next) = tokens.get(i + 1).filter(|n| n.kind == TokenKind::Word) {
                let word = next.text(sql).to_ascii_uppercase();
                if matches!(word.as_str(), "INSERT" | "UPDATE" | "DELETE") {
                    modifying_cte.get_or_insert(word);
                }
            }
        } else if t.is_punct(sql, ')') {
            depth = depth.saturating_sub(1);
        } else if t.kind == TokenKind::Word {
            let word = t.text(sql).to_ascii_uppercase();
            // Also inside the option list of `EXPLAIN (ANALYZE, …)`.
            analyze |= word == "ANALYZE" || word == "ANALYSE";
            if depth == 0 {
                top.push((i, word));
            }
        }
    }
    let lead = top.first()?.1.as_str();
    let has = |word: &str| top.iter().any(|(_, w)| w == word);

    // The keyword that decides what the statement does.
    let main: &str = match lead {
        "WITH" => top
            .iter()
            .skip(1)
            .map(|(_, w)| w.as_str())
            .find(|w| DML.contains(w))
            .unwrap_or("WITH"),
        // Only `ANALYZE` actually runs the statement.
        "EXPLAIN" if analyze => top
            .iter()
            .skip(1)
            .map(|(_, w)| w.as_str())
            .find(|w| DML.contains(w))
            .unwrap_or("EXPLAIN"),
        _ => lead,
    };
    // `SELECT … INTO` creates a table (Postgres) or writes a file (MySQL).
    let select_into = main == "SELECT" && has("INTO");
    let writes = WRITES.contains(&main) || select_into || modifying_cte.is_some();
    let unfiltered = matches!(main, "UPDATE" | "DELETE") && !has("WHERE");
    if !writes && !unfiltered {
        return None;
    }
    let keyword = match (&modifying_cte, WRITES.contains(&main) || select_into) {
        (Some(cte), false) => cte.clone(),
        _ if select_into => "SELECT INTO".to_string(),
        _ => main.to_string(),
    };
    Some(Check {
        statement: text_of(sql, &tokens),
        keyword,
        writes,
        unfiltered,
    })
}

fn text_of(sql: &str, tokens: &[Token]) -> String {
    match (tokens.first(), tokens.last()) {
        (Some(first), Some(last)) => sql[first.start..last.end].to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(sql: &str) -> Option<Check> {
        let mut found = check(sql);
        assert!(found.len() <= 1, "{found:?}");
        found.pop()
    }

    #[test]
    fn reads_are_fine() {
        for sql in [
            "SELECT * FROM users",
            "select 1",
            "WITH a AS (SELECT 1) SELECT * FROM a",
            "SHOW TABLES",
            "SET search_path = public",
            "BEGIN",
            "EXPLAIN SELECT * FROM users",
            "EXPLAIN DELETE FROM users",
            "SELECT * FROM users FOR UPDATE",
            "  ",
            "-- just a comment",
        ] {
            assert_eq!(check(sql), vec![], "{sql}");
        }
    }

    #[test]
    fn writes_are_found() {
        for (sql, keyword) in [
            ("INSERT INTO t VALUES (1)", "INSERT"),
            ("update t set a = 1 where id = 2", "UPDATE"),
            ("DELETE FROM t WHERE id = 1", "DELETE"),
            ("DROP TABLE t", "DROP"),
            ("ALTER TABLE t ADD COLUMN c int", "ALTER"),
            ("CREATE INDEX i ON t (a)", "CREATE"),
            ("TRUNCATE t", "TRUNCATE"),
            ("GRANT SELECT ON t TO u", "GRANT"),
            ("SELECT * INTO copy FROM t", "SELECT INTO"),
            ("EXPLAIN ANALYZE DELETE FROM t WHERE id = 1", "DELETE"),
            (
                "EXPLAIN (ANALYZE, VERBOSE) UPDATE t SET a = 1 WHERE b",
                "UPDATE",
            ),
            (
                "WITH gone AS (DELETE FROM t WHERE a RETURNING *) SELECT * FROM gone",
                "DELETE",
            ),
            (
                "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x",
                "INSERT",
            ),
        ] {
            let found = one(sql).unwrap_or_else(|| panic!("not found: {sql}"));
            assert!(found.writes, "{sql}");
            assert_eq!(found.keyword, keyword, "{sql}");
            assert!(!found.unfiltered, "{sql}");
        }
    }

    #[test]
    fn missing_where_is_found() {
        for (sql, keyword) in [
            ("UPDATE users SET active = false", "UPDATE"),
            ("delete from users", "DELETE"),
            ("DELETE FROM users;", "DELETE"),
            ("WITH x AS (SELECT 1) DELETE FROM users", "DELETE"),
            // A WHERE inside a subquery belongs to the subquery.
            (
                "UPDATE t SET a = (SELECT b FROM u WHERE u.id = 1)",
                "UPDATE",
            ),
            ("DELETE FROM t ORDER BY id LIMIT 10", "DELETE"),
        ] {
            let found = one(sql).unwrap_or_else(|| panic!("not found: {sql}"));
            assert!(found.unfiltered, "{sql}");
            assert!(found.writes, "{sql}");
            assert_eq!(found.keyword, keyword, "{sql}");
        }
    }

    #[test]
    fn where_makes_it_filtered() {
        for sql in [
            "UPDATE users SET active = false WHERE id = 1",
            "delete from users where id in (select id from old)",
            "UPDATE t SET a = 1\nWHERE b",
            "DELETE FROM t USING u WHERE t.id = u.id",
        ] {
            assert!(!one(sql).unwrap().unfiltered, "{sql}");
        }
    }

    #[test]
    fn words_in_strings_and_comments_do_not_count() {
        assert_eq!(check("SELECT 'DROP TABLE t; DELETE FROM t'"), vec![]);
        assert_eq!(check("SELECT 1 -- DELETE FROM t"), vec![]);
        let found = one("UPDATE t SET a = 'where' -- WHERE x").unwrap();
        assert!(found.unfiltered);
    }

    #[test]
    fn every_statement_of_a_script_is_checked() {
        let found =
            check("SELECT 1;\nUPDATE a SET x = 1;\nDELETE FROM b WHERE id = 3;\nDELETE FROM c");
        let keywords: Vec<_> = found
            .iter()
            .map(|c| (c.keyword.as_str(), c.unfiltered))
            .collect();
        assert_eq!(
            keywords,
            [("UPDATE", true), ("DELETE", false), ("DELETE", true)]
        );
        assert_eq!(found[0].statement, "UPDATE a SET x = 1");
    }
}
