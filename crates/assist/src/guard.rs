//! Finds statements worth a second look before they run: ones that change data or
//! schema, and `UPDATE` / `DELETE` without a `WHERE`. Works on tokens, not a full
//! parse, so it is a guardrail and not a guarantee (a function called from a
//! `SELECT` can still write).

use crate::analyze::{significant_with, statements_with};
use crate::dialect;
use crate::lexer::{Lexing, Token, TokenKind, tokenize_with};

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
/// Statements that only read, when nothing else in them writes. The allow-list for
/// callers that may only read (the assistant, schedules): anything else needs a
/// person's go-ahead or is refused.
const READS: &[&str] = &[
    "SELECT", "WITH", "EXPLAIN", "SHOW", "DESCRIBE", "DESC", "TABLE", "VALUES",
];
/// A statement that isn't plainly a read: (keyword, statement text).
pub type NotRead = (String, String);

/// The statements of `sql` that are not plainly reads: not on the read allow-list,
/// or writing, locking rows or calling a function with side effects. Empty means
/// every statement only reads (as far as tokens can tell; the server's read-only
/// transaction is the guarantee).
pub fn not_reads(sql: &str) -> Vec<NotRead> {
    not_reads_with(sql, Lexing::default())
}

/// [`not_reads`] with a database's lexical rules: the ones the gate uses, so the
/// statements are split and read as the server will.
pub fn not_reads_with(sql: &str, lexing: Lexing) -> Vec<NotRead> {
    statements_with(sql, lexing)
        .into_iter()
        .filter_map(|(start, end)| not_read(&sql[start..end], lexing))
        .collect()
}

/// The text a caller that may only read sends instead of `sql`: comments blanked out
/// by the database's rules (same length, so error positions stay) and trailing `;`
/// dropped. What the server runs is then what was checked: nothing hides in a
/// comment the server reads differently (MySQL runs `/*! … */`).
pub fn read_text(sql: &str, lexing: Lexing) -> String {
    let mut text = String::with_capacity(sql.len());
    for t in tokenize_with(sql, lexing) {
        match t.kind {
            TokenKind::Comment => text.extend(std::iter::repeat_n(' ', t.end - t.start)),
            _ => text.push_str(t.text(sql)),
        }
    }
    let end = text
        .trim_end_matches(|c: char| c == ';' || c.is_whitespace())
        .len();
    text.truncate(end);
    text
}

/// Why `text` (from [`read_text`]) can't be sent as one read, if it can't:
/// - a `;` anywhere, even in a string: databases don't all agree where a string ends
///   (backslashes, server settings), and a second statement could end the read-only
///   transaction;
/// - a `U&` escape, which can spell any name (`U&"d\0062link"` is `dblink`) past the
///   check for functions with side effects.
pub fn not_one_read(text: &str) -> Option<&'static str> {
    if text.contains(';') {
        return Some(
            "only one statement at a time, and no ';' in it (not even in a string; \
             use chr(59) or CHAR(59))",
        );
    }
    if text.to_ascii_lowercase().contains("u&") {
        return Some("U& escapes are not allowed here; write names and strings plainly");
    }
    None
}

fn not_read(sql: &str, lexing: Lexing) -> Option<NotRead> {
    let tokens = significant_with(sql, lexing);
    let first = tokens.first()?;
    let words: Vec<String> = tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(sql).to_ascii_uppercase())
        .collect();
    let lead = if first.kind == TokenKind::Word {
        first.text(sql).to_ascii_uppercase()
    } else {
        first.text(sql).to_string()
    };
    let statement = text_of(sql, &tokens);
    if let Some(check) = check_statement(sql, lexing).filter(|c| c.writes) {
        return Some((check.keyword, statement));
    }
    if !READS.contains(&lead.as_str()) {
        return Some((lead, statement));
    }
    // `SELECT … FOR UPDATE / SHARE` locks rows.
    if words
        .windows(2)
        .any(|w| w[0] == "FOR" && matches!(w[1].as_str(), "UPDATE" | "SHARE" | "NO" | "KEY"))
    {
        return Some(("FOR UPDATE".to_string(), statement));
    }
    let lower = sql.to_ascii_lowercase();
    // Every dialect's, whichever database this goes to: stricter, and the guard
    // needs no connection.
    let side_effect = dialect::all()
        .flat_map(|d| d.side_effect_functions())
        .find(|name| lower.contains(*name));
    if let Some(name) = side_effect {
        return Some((name.to_uppercase(), statement));
    }
    None
}

/// What a `WITH` or `EXPLAIN ANALYZE` statement can be about.
const DML: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE"];

/// The statements of `sql` that write or are unfiltered; harmless ones are left out.
pub fn check(sql: &str) -> Vec<Check> {
    let lexing = Lexing::default();
    statements_with(sql, lexing)
        .into_iter()
        .filter_map(|(start, end)| check_statement(&sql[start..end], lexing))
        .collect()
}

fn check_statement(sql: &str, lexing: Lexing) -> Option<Check> {
    let tokens = significant_with(sql, lexing);
    // Words outside any parentheses, with their position among the tokens.
    let mut depth = 0usize;
    let mut top: Vec<(usize, String)> = Vec::new();
    let mut modifying_cte = None;
    let mut analyze = false;
    let mut all: Vec<String> = Vec::new();
    let mut mode_var = false;
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
        } else if t.kind == TokenKind::String {
            // `set_config('default_transaction_read_only', 'off', false)`
            mode_var |= names_read_only_variable(t.text(sql));
        } else if t.kind == TokenKind::Word {
            let word = t.text(sql).to_ascii_uppercase();
            mode_var |= names_read_only_variable(&word);
            all.push(word.clone());
            // Also inside the option list of `EXPLAIN (ANALYZE, …)`.
            analyze |= word == "ANALYZE" || word == "ANALYSE";
            if depth == 0 {
                top.push((i, word));
            }
        }
    }
    let lead = top.first()?.1.as_str();
    let has = |word: &str| top.iter().any(|(_, w)| w == word);

    // Statements that would switch the session out of read-only mode count as writes.
    let read_write = all.windows(2).any(|w| w[0] == "READ" && w[1] == "WRITE");
    let escapes = mode_var
        || (matches!(lead, "SET" | "BEGIN" | "START") && read_write)
        || lead == "DISCARD"
        || (lead == "RESET" && has("ALL"));
    if escapes {
        return Some(Check {
            statement: text_of(sql, &tokens),
            keyword: lead.to_string(),
            writes: true,
            unfiltered: false,
        });
    }

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

/// Names one of the session settings that make transactions read-only.
fn names_read_only_variable(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("transaction_read_only") || text.contains("tx_read_only")
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

    #[test]
    fn read_text_blanks_comments_by_the_rules() {
        let mysql = dialect::dialect_for(tangul_core::DriverKind::Mysql).lexing();
        // Same length: error positions stay.
        assert_eq!(
            read_text("select 1 /* x */ # y\n+ 2 ;; \n", mysql),
            format!("select 1{}\n+ 2", " ".repeat(12))
        );
        // Without MySQL's rules `#` is no comment.
        assert_eq!(read_text("select 1 # y", Lexing::default()), "select 1 # y");
        assert_eq!(read_text("select /*! 1 */ 2", mysql), "select          2");
    }

    #[test]
    fn one_read_at_a_time() {
        assert_eq!(not_one_read("select * from t where a = 'x'"), None);
        for (text, why) in [
            ("select 1; select 2", "only one statement"),
            ("select ';'", "only one statement"),
            ("select U&\"d\\0062link\"('x')", "U& escapes"),
        ] {
            assert!(not_one_read(text).unwrap().starts_with(why), "{text}");
        }
    }

    #[test]
    fn reads_on_the_allow_list() {
        let keyword = |sql: &str| not_reads(sql).first().map(|(k, _)| k.clone());
        for read in [
            "select * from t",
            "  SELECT 1; select 2",
            "with a as (select 1) select * from a",
            "explain select * from t",
            "show tables",
            "describe t",
            "table t",
            "values (1), (2)",
            "select updated_at, lock_timeout from t where id = 1",
        ] {
            assert_eq!(keyword(read), None, "{read}");
        }
        for (sql, why) in [
            ("delete from t where id = 1", "DELETE"),
            ("select 1; update t set a = 1 where id = 1", "UPDATE"),
            (
                "with d as (delete from t returning *) select * from d",
                "DELETE",
            ),
            ("explain analyze delete from t", "DELETE"),
            ("select * into copy from t", "SELECT INTO"),
            ("set search_path = x", "SET"),
            ("call refresh()", "CALL"),
            ("begin", "BEGIN"),
            ("select * from t for update", "FOR UPDATE"),
            ("select pg_terminate_backend(123)", "PG_TERMINATE_BACKEND"),
            ("select nextval('s')", "NEXTVAL"),
            ("SELECT LOAD_FILE('/etc/passwd')", "LOAD_FILE"),
        ] {
            assert_eq!(keyword(sql).as_deref(), Some(why), "{sql}");
        }
    }

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
    fn switching_off_read_only_counts_as_a_write() {
        for sql in [
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            "SET SESSION TRANSACTION READ WRITE",
            "BEGIN READ WRITE",
            "START TRANSACTION READ WRITE",
            "SET default_transaction_read_only = off",
            "SET SESSION transaction_read_only = 0",
            "RESET default_transaction_read_only",
            "RESET ALL",
            "DISCARD ALL",
            "SELECT set_config('default_transaction_read_only', 'off', false)",
        ] {
            let found = one(sql).unwrap_or_else(|| panic!("not found: {sql}"));
            assert!(found.writes, "{sql}");
        }
        for sql in [
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            "BEGIN READ ONLY",
            "SET search_path = public",
            "SELECT * FROM readers WHERE mode = 'write'",
        ] {
            assert_eq!(check(sql), vec![], "{sql}");
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
