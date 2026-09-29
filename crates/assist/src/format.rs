//! Formatting SQL before it runs: line breaks and indentation only. The text is
//! reformatted by `sqlformat`, then checked with the database's own lexical rules:
//! if anything but whitespace changed (a keyword's case, a comment, a quote the
//! formatter doesn't know), the original is kept, so what runs is always what the
//! user wrote.

use sqlformat::{FormatOptions, Indent, QueryParams};

use crate::dialect::Dialect;
use crate::lexer::{Lexing, Token, TokenKind, tokenize_with};

/// How to format: the database's lexical rules and syntax (see [`Dialect`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FormatRules {
    pub lexing: Lexing,
    pub array_syntax: bool,
}

impl FormatRules {
    pub fn of(dialect: &dyn Dialect) -> Self {
        Self {
            lexing: dialect.lexing(),
            array_syntax: dialect.array_syntax(),
        }
    }
}

/// `sql` formatted, or `None` when it is formatted already or can't be formatted
/// without changing more than whitespace. Leading and trailing whitespace stay.
pub fn format_sql(sql: &str, rules: FormatRules) -> Option<String> {
    let lexing = rules.lexing;
    // `DELIMITER x` is a client command that ends at the line break: line breaks
    // are syntax there, which the token check below can't see.
    if lexing.delimiter_command
        && sql.lines().any(|l| {
            l.trim_start()
                .get(..9)
                .is_some_and(|w| w.eq_ignore_ascii_case("delimiter"))
        })
    {
        return None;
    }
    let start = sql.len() - sql.trim_start().len();
    let end = sql.trim_end().len();
    if start >= end {
        return None;
    }
    let body = &sql[start..end];
    let options = FormatOptions {
        indent: Indent::Spaces(2),
        // Keep the case as written.
        uppercase: None,
        lines_between_queries: 1,
        dialect: if rules.array_syntax {
            sqlformat::Dialect::PostgreSql
        } else {
            sqlformat::Dialect::Generic
        },
        ..FormatOptions::default()
    };
    let formatted = sqlformat::format(body, &QueryParams::None, &options);
    if formatted == body || !same_tokens(body, &formatted, lexing) {
        return None;
    }
    Some(format!("{}{formatted}{}", &sql[..start], &sql[end..]))
}

/// Where `offset` in `old` is in `new`, the same text with other whitespace: after
/// as many non-whitespace characters. Keeps the cursor on its character.
pub fn map_offset(old: &str, new: &str, offset: usize) -> usize {
    let before = old
        .get(..offset)
        .map_or(0, |s| s.chars().filter(|c| !c.is_whitespace()).count());
    if before == 0 {
        return offset.min(new.len());
    }
    new.char_indices()
        .filter(|(_, c)| !c.is_whitespace())
        .nth(before - 1)
        .map_or(new.len(), |(i, c)| i + c.len_utf8())
}

/// Whether `a` and `b` are the same tokens, whitespace aside.
fn same_tokens(a: &str, b: &str, lexing: Lexing) -> bool {
    let tokens = |src: &str| -> Vec<(TokenKind, String)> {
        tokenize_with(src, lexing)
            .into_iter()
            .filter(|t: &Token| t.kind != TokenKind::Space)
            .map(|t| (t.kind, t.text(src).to_string()))
            .collect()
    };
    tokens(a) == tokens(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::dialect_for;
    use tangul_core::DriverKind;

    fn rules(kind: DriverKind) -> FormatRules {
        FormatRules::of(dialect_for(kind))
    }

    fn postgres() -> FormatRules {
        rules(DriverKind::Postgres)
    }

    fn mysql() -> FormatRules {
        rules(DriverKind::Mysql)
    }

    #[test]
    fn breaks_lines_and_keeps_the_rest() {
        let sql = "select id, name from app.users where id = 1 and name like 'a%'";
        let f = format_sql(sql, postgres()).unwrap();
        assert!(f.lines().count() > 1, "{f}");
        // Only whitespace changed: the case as written, the literal untouched.
        assert!(f.contains("select") && f.contains("'a%'"));
        assert!(same_tokens(sql, &f, postgres().lexing));
        // Formatting again changes nothing: a second run leaves the text alone.
        assert_eq!(format_sql(&f, postgres()), None);
    }

    #[test]
    fn keeps_what_surrounds_the_statement() {
        let f = format_sql("\n  select a, b from t  \n", postgres()).unwrap();
        assert!(f.starts_with("\n  select") && f.ends_with("  \n"), "{f:?}");
        assert_eq!(format_sql("   ", postgres()), None);
    }

    #[test]
    fn keeps_database_specific_quoting() {
        // Dollar quotes and casts (Postgres), backticks and # comments (MySQL):
        // formatted only where the tokens stay the same, else left as written.
        for (sql, rules) in [
            ("select $$ a  b $$::text as x, y from t", postgres()),
            ("select `a b`, c from t # note\nwhere d = 1", mysql()),
            ("select a /* x /* y */ z */, b from t", postgres()),
        ] {
            if let Some(f) = format_sql(sql, rules) {
                assert!(same_tokens(sql, &f, rules.lexing), "{sql} → {f}");
            }
        }
    }

    #[test]
    fn a_change_beyond_whitespace_is_refused() {
        let p = postgres().lexing;
        assert!(!same_tokens("select 1", "SELECT 1", p));
        assert!(!same_tokens("select 'a b'", "select 'a  b'", p));
        assert!(same_tokens("select  a,b", "select\n  a,\n  b", p));
    }

    #[test]
    fn every_database() {
        let sql = "select a, count(*) from t where b = 1 group by a";
        let expected = "select\n  a,\n  count(*)\nfrom\n  t\nwhere\n  b = 1\ngroup by\n  a";
        for kind in DriverKind::ALL {
            assert_eq!(
                format_sql(sql, rules(kind)).as_deref(),
                Some(expected),
                "{kind:?}"
            );
        }
        // Postgres: arrays and casts stay whole.
        let f = format_sql("select a[1], b::int, c @> array[1] from t", postgres()).unwrap();
        assert!(
            f.contains("a[1]") && f.contains("b::int") && f.contains("@>"),
            "{f}"
        );
        // MySQL: backticks and # comments.
        let f = format_sql("select `a b`, c from t # note\nwhere d = 1", mysql()).unwrap();
        assert!(f.contains("`a b`") && f.contains("# note\n"), "{f}");
    }

    #[test]
    fn the_cursor_keeps_its_character() {
        let old = "select a,  b from t";
        let new = "select\n  a,\n  b\nfrom\n  t";
        // After "b" …
        assert_eq!(&new[..map_offset(old, new, 12)], "select\n  a,\n  b");
        // … at the start, and at the end.
        assert_eq!(map_offset(old, new, 0), 0);
        assert_eq!(map_offset(old, new, old.len()), new.len());
    }

    #[test]
    fn a_mysql_delimiter_script_stays_as_written() {
        let script = "DELIMITER //\ncreate procedure p() begin select 1; end //\nDELIMITER ;";
        assert_eq!(format_sql(script, mysql()), None);
        // Elsewhere `delimiter` is just a word.
        assert!(format_sql("select delimiter, b from t", postgres()).is_some());
    }
}
