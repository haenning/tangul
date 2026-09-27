//! Statements as the editor sees them: where each one is, what runs when it is run
//! on its own, and which one the cursor is in. Follows the database's lexical rules
//! (`#` comments, nested comments, `DELIMITER`); the guard keeps its own split
//! ([`crate::analyze::statements`]), which is stricter.

use std::ops::Range;

use crate::dialect::Dialect;
use crate::lexer::{Token, TokenKind, tokenize_with};

/// One statement in the editor. Byte ranges into the text it was split from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// What runs: from its first token (comments on the lines right above included)
    /// to its last one, without the terminator.
    pub body: Range<usize>,
    /// The body and its terminator, if it has one.
    pub range: Range<usize>,
}

impl Statement {
    pub fn text<'a>(&self, sql: &'a str) -> &'a str {
        &sql[self.body.clone()]
    }

    fn terminated(&self) -> bool {
        self.range.end > self.body.end
    }
}

/// The statements of `sql`, split at top-level terminators: `;`, or what a MySQL
/// `DELIMITER` line set. `DELIMITER` lines belong to no statement; stretches with
/// only whitespace and comments are none.
pub fn split(sql: &str, dialect: &dyn Dialect) -> Vec<Statement> {
    let lexing = dialect.lexing();
    let tokens = tokenize_with(sql, lexing);
    let mut out = Vec::new();
    // `None`: the usual `;`.
    let mut delimiter: Option<String> = None;
    let mut chunk = 0;
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if lexing.delimiter_command
            && t.is_word(sql, "DELIMITER")
            && tokens[chunk..i].iter().all(Token::is_trivia)
        {
            let line_end = sql[t.start..].find('\n').map_or(sql.len(), |j| t.start + j);
            if let Some(arg) = sql[t.end..line_end].split_whitespace().next() {
                delimiter = (arg != ";").then(|| arg.to_string());
            }
            i = skip_to(&tokens, i, line_end);
            chunk = i;
            continue;
        }
        let terminator = match (&delimiter, t.kind) {
            (_, TokenKind::Space | TokenKind::Comment | TokenKind::String) => None,
            (_, TokenKind::QuotedIdent) => None,
            (None, _) => t.is_punct(sql, ';').then_some(1),
            (Some(d), _) => sql[t.start..].starts_with(d.as_str()).then_some(d.len()),
        };
        let Some(len) = terminator else {
            i += 1;
            continue;
        };
        let end = t.start + len;
        out.extend(statement(sql, &tokens[chunk..i], Some(end)));
        i = skip_to(&tokens, i, end);
        chunk = i;
    }
    out.extend(statement(sql, &tokens[chunk..], None));
    out
}

/// The statement a run without a selection takes, for a cursor at byte `cursor`:
/// the one the cursor is in or on the line of; after a terminator, the one it ends.
/// With a single statement that one, wherever the cursor is. `None` between
/// statements (blank lines, comments, `DELIMITER` lines): which one is meant is
/// unclear.
pub fn statement_at(sql: &str, cursor: usize, dialect: &dyn Dialect) -> Option<Statement> {
    let statements = split(sql, dialect);
    if let [only] = statements.as_slice() {
        return Some(only.clone());
    }
    let lexing = dialect.lexing();
    let last = statements.len().checked_sub(1)?;
    statements.into_iter().enumerate().find_map(|(n, s)| {
        let line_start = sql[..s.range.start].rfind('\n').map_or(0, |i| i + 1);
        let from = if sql[line_start..s.range.start].trim().is_empty() {
            line_start
        } else {
            s.range.start
        };
        let to = if n == last && !s.terminated() {
            sql.len()
        } else {
            let line_end = sql[s.range.end..]
                .find('\n')
                .map_or(sql.len(), |i| s.range.end + i);
            let rest_is_trivia = tokenize_with(&sql[s.range.end..line_end], lexing)
                .iter()
                .all(Token::is_trivia);
            if rest_is_trivia {
                line_end
            } else {
                s.range.end
            }
        };
        (from..=to).contains(&cursor).then_some(s)
    })
}

/// Index of the first token at or after byte `end`.
fn skip_to(tokens: &[Token], mut i: usize, end: usize) -> usize {
    while tokens.get(i).is_some_and(|t| t.start < end) {
        i += 1;
    }
    i
}

/// The statement made of `chunk` (the tokens between two terminators), ending in a
/// terminator at `terminator_end`. `None` if it has only whitespace and comments.
fn statement(sql: &str, chunk: &[Token], terminator_end: Option<usize>) -> Option<Statement> {
    let first = chunk.iter().position(|t| !t.is_trivia())?;
    let last = chunk.iter().rposition(|t| !t.is_trivia())?;
    let mut start = chunk[first].start;
    for t in chunk[..first].iter().rev() {
        match t.kind {
            // A blank line ends the comments that belong to the statement.
            TokenKind::Space if t.text(sql).matches('\n').count() <= 1 => {}
            TokenKind::Comment if own_line(sql, t.start) => start = t.start,
            _ => break,
        }
    }
    let body = start..chunk[last].end;
    Some(Statement {
        range: body.start..terminator_end.unwrap_or(body.end),
        body,
    })
}

/// Whether only whitespace comes before byte `at` on its line.
fn own_line(sql: &str, at: usize) -> bool {
    let line_start = sql[..at].rfind('\n').map_or(0, |i| i + 1);
    sql[line_start..at].trim().is_empty()
}

#[cfg(test)]
mod tests {
    use tangul_core::DriverKind;

    use super::*;
    use crate::dialect::dialect_for;

    fn pg() -> &'static dyn Dialect {
        dialect_for(DriverKind::Postgres)
    }

    fn my() -> &'static dyn Dialect {
        dialect_for(DriverKind::Mysql)
    }

    fn bodies<'a>(sql: &'a str, d: &dyn Dialect) -> Vec<&'a str> {
        split(sql, d).iter().map(|s| s.text(sql)).collect()
    }

    /// The statement at the `|` in `marked` (the marker is removed first).
    fn at(marked: &str, d: &dyn Dialect) -> Option<String> {
        let cursor = marked.find('|').expect("cursor marker");
        let sql = marked.replacen('|', "", 1);
        statement_at(&sql, cursor, d).map(|s| s.text(&sql).to_string())
    }

    #[test]
    fn splits_at_top_level_terminators() {
        assert_eq!(
            bodies("select 1; select 2;", pg()),
            ["select 1", "select 2"]
        );
        assert_eq!(
            bodies("select 1;\n\nselect 2", pg()),
            ["select 1", "select 2"]
        );
        assert_eq!(bodies(" ; ;\n", pg()), Vec::<&str>::new());
        // Blank lines don't separate.
        assert_eq!(bodies("select 1\n\nfrom t", pg()), ["select 1\n\nfrom t"]);
        let s = &split("select 1 ;  ", pg())[0];
        assert_eq!((s.body.clone(), s.range.clone()), (0..8, 0..10));
    }

    #[test]
    fn terminators_inside_literals_and_comments_dont_split() {
        for sql in [
            "select ';', 'it''s;'",
            r"select E'\';'",
            r#"select ";" from t"#,
            "select 1 -- ;\n + 2",
            "select 1 /* ; */",
            "select $$ ; $$",
            "select $body$ ; $$ ; $body$",
            "create function f() returns int as $$ begin return 1; end; $$ language plpgsql",
            "select 1 /* a /* ; */ ; */",
        ] {
            assert_eq!(split(sql, pg()).len(), 1, "{sql}");
        }
        for sql in [
            "select `a;b` from t",
            "select 1 # ;\n + 2",
            r#"select "a;b""#,
        ] {
            assert_eq!(split(sql, my()).len(), 1, "{sql}");
        }
        // `#` is an operator in Postgres, nesting isn't MySQL's.
        assert_eq!(
            bodies("select 1 # 2; select 3", pg()),
            ["select 1 # 2", "select 3"]
        );
        assert_eq!(
            bodies("/* a /* b */ select 1; */", my()),
            ["/* a /* b */ select 1", "*/"]
        );
    }

    #[test]
    fn unterminated_tokens_run_to_the_end() {
        assert_eq!(
            bodies("select 1; select 'a; b", pg()),
            ["select 1", "select 'a; b"]
        );
        assert_eq!(bodies("select 1; /* x; y", pg()), ["select 1"]);
        assert_eq!(bodies("select $$ a; b", pg()), ["select $$ a; b"]);
    }

    #[test]
    fn leading_comments_belong_to_the_statement() {
        let sql = "select 1; -- about 1\n-- about 2\n/* too */\nselect 2;\n-- far\n\nselect 3";
        assert_eq!(
            bodies(sql, pg()),
            ["select 1", "-- about 2\n/* too */\nselect 2", "select 3"]
        );
        // Trailing comments stay out of the body.
        assert_eq!(bodies("select 1 -- done\n;", pg()), ["select 1"]);
    }

    #[test]
    fn mysql_delimiter() {
        let sql = "DELIMITER //\n\
                   CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END //\n\
                   delimiter ;\n\
                   CALL p();";
        assert_eq!(
            bodies(sql, my()),
            [
                "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                "CALL p()"
            ]
        );
        // `$$` as delimiter (no dollar quotes in MySQL).
        assert_eq!(
            bodies("DELIMITER $$\nSELECT 1; SELECT 2$$\nSELECT 3$$", my()),
            ["SELECT 1; SELECT 2", "SELECT 3"]
        );
        // Only at the start of a statement, and only MySQL's.
        assert_eq!(
            bodies("select delimiter from t;", my()),
            ["select delimiter from t"]
        );
        assert_eq!(
            bodies("DELIMITER //\nselect 1;", pg()),
            ["DELIMITER //\nselect 1"]
        );
    }

    #[test]
    fn cursor_inside_a_statement() {
        let two = |m: &str| at(m, pg());
        assert_eq!(two("|select 1;\nselect 2;").as_deref(), Some("select 1"));
        assert_eq!(two("sel|ect 1;\nselect 2;").as_deref(), Some("select 1"));
        assert_eq!(two("select 1;\nselect| 2;").as_deref(), Some("select 2"));
        assert_eq!(
            two("select 1;\n  |  select 2;").as_deref(),
            Some("select 2")
        );
        // Multi-line statement, cursor on a blank line inside it.
        assert_eq!(
            two("select 1;\nselect *\n|\nfrom t;").as_deref(),
            Some("select *\n\nfrom t")
        );
        // In a leading comment.
        assert_eq!(
            two("select 1;\n-- n|ote\nselect 2;").as_deref(),
            Some("-- note\nselect 2")
        );
    }

    #[test]
    fn cursor_after_a_terminator() {
        let two = |m: &str| at(m, pg());
        assert_eq!(two("select 1;|\nselect 2;").as_deref(), Some("select 1"));
        assert_eq!(two("select 1;   |\nselect 2;").as_deref(), Some("select 1"));
        assert_eq!(
            two("select 1; -- c|\nselect 2;").as_deref(),
            Some("select 1")
        );
        // Two on one line: right after the `;` is the first.
        assert_eq!(two("select 1;|select 2;").as_deref(), Some("select 1"));
        assert_eq!(two("select 1; |select 2;").as_deref(), Some("select 2"));
        assert_eq!(two("select 1;\nselect 2;|").as_deref(), Some("select 2"));
    }

    #[test]
    fn cursor_between_statements() {
        let two = |m: &str| at(m, pg());
        assert_eq!(two("select 1;\n|\nselect 2;"), None);
        assert_eq!(two("select 1;\n\n-- loose|\n\nselect 2;"), None);
        assert_eq!(two("|\nselect 1;\nselect 2;"), None);
        assert_eq!(two("select 1;\nselect 2;\n\n|"), None);
        assert_eq!(
            at("DELIMITER //\nselect 1//\nDELI|MITER ;\nselect 2;", my()),
            None
        );
    }

    #[test]
    fn single_statement_wherever_the_cursor_is() {
        assert_eq!(at("|\n\nselect 1;\n\n", pg()).as_deref(), Some("select 1"));
        assert_eq!(at("select 1;\n\n|", pg()).as_deref(), Some("select 1"));
        assert_eq!(at("-- only a comment|", pg()), None);
        assert_eq!(at("|", pg()), None);
    }

    #[test]
    fn last_statement_without_terminator() {
        assert_eq!(
            at("select 1;\nselect 2\n\n|", pg()).as_deref(),
            Some("select 2")
        );
        assert_eq!(
            at("select 1;\nsel|ect 2", pg()).as_deref(),
            Some("select 2")
        );
    }
}
