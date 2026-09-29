//! Formatting SQL before it runs: line breaks and indentation only. The text is
//! reformatted by `sqlformat`, then checked with the database's own lexical rules:
//! if anything but whitespace changed (a keyword's case, a comment, a quote the
//! formatter doesn't know), the original is kept, so what runs is always what the
//! user wrote.

use sqlformat::{FormatOptions, Indent, QueryParams};

use crate::lexer::{Lexing, Token, TokenKind, tokenize_with};

/// `sql` formatted, or `None` when it is formatted already or can't be formatted
/// without changing more than whitespace. Leading and trailing whitespace stay.
pub fn format_sql(sql: &str, lexing: Lexing) -> Option<String> {
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
        dialect: if lexing.nested_comments {
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

    fn postgres() -> Lexing {
        Lexing {
            hash_comments: false,
            nested_comments: true,
            dollar_quotes: true,
            delimiter_command: false,
        }
    }

    fn mysql() -> Lexing {
        Lexing {
            hash_comments: true,
            nested_comments: false,
            dollar_quotes: false,
            delimiter_command: true,
        }
    }

    #[test]
    fn breaks_lines_and_keeps_the_rest() {
        let sql = "select id, name from app.users where id = 1 and name like 'a%'";
        let f = format_sql(sql, postgres()).unwrap();
        assert!(f.lines().count() > 1, "{f}");
        // Only whitespace changed: the case as written, the literal untouched.
        assert!(f.contains("select") && f.contains("'a%'"));
        assert!(same_tokens(sql, &f, postgres()));
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
        for (sql, lexing) in [
            ("select $$ a  b $$::text as x, y from t", postgres()),
            ("select `a b`, c from t # note\nwhere d = 1", mysql()),
            ("select a /* x /* y */ z */, b from t", postgres()),
        ] {
            if let Some(f) = format_sql(sql, lexing) {
                assert!(same_tokens(sql, &f, lexing), "{sql} → {f}");
            }
        }
    }

    #[test]
    fn a_change_beyond_whitespace_is_refused() {
        assert!(!same_tokens("select 1", "SELECT 1", postgres()));
        assert!(!same_tokens("select 'a b'", "select 'a  b'", postgres()));
        assert!(same_tokens("select  a,b", "select\n  a,\n  b", postgres()));
    }
}
