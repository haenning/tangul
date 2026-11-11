//! What the editor notes after a statement: when it last ran and what it got
//! (`5 h ago · 105 rows`). Found in the history by the statement's text, as its
//! [`key`]: comments, whitespace and the case of keywords aside, values kept, so
//! only the same SQL matches and an edited statement loses its note.

use crate::guard;
use crate::lexer::{Lexing, TokenKind, tokenize_with};

/// The text a run of `sql` is recognized by: its tokens without comments and
/// whitespace (a trailing `;` aside), joined by single spaces; keywords and
/// unquoted names in lower case (values and quoted names as they are).
pub fn key(sql: &str, lexing: Lexing) -> String {
    let tokens = tokenize_with(sql, lexing);
    let mut parts: Vec<String> = tokens
        .iter()
        .filter(|t| !t.is_trivia())
        .map(|t| match t.kind {
            TokenKind::Word => t.text(sql).to_lowercase(),
            _ => t.text(sql).to_string(),
        })
        .collect();
    while parts.last().is_some_and(|p| p == ";") {
        parts.pop();
    }
    parts.join(" ")
}

/// A run as the history kept it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// Seconds since it started.
    pub ago: i64,
    pub rows: Option<i64>,
    pub failed: bool,
}

/// The note for a run of `sql`: when, and the rows it returned or, for a write,
/// changed; `failed` for an error.
pub fn note(sql: &str, lexing: Lexing, run: Run) -> String {
    let when = ago(run.ago);
    if run.failed {
        return format!("{when} · failed");
    }
    let write = !guard::not_reads_with(sql, lexing).is_empty();
    match run.rows {
        Some(1) if write => format!("{when} · 1 row changed"),
        Some(n) if write => format!("{when} · {n} rows changed"),
        Some(1) => format!("{when} · 1 row"),
        Some(n) => format!("{when} · {n} rows"),
        None => when,
    }
}

/// "5 min ago"-style age of `secs` seconds.
pub fn ago(secs: i64) -> String {
    match secs.max(0) {
        0..60 => "just now".into(),
        s @ 60..3_600 => format!("{} min ago", s / 60),
        s @ 3_600..86_400 => format!("{} h ago", s / 3_600),
        s => format!("{} d ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_ignores_comments_and_spacing_but_not_values() {
        let lexing = Lexing::default();
        let a = key("-- top ten\nSELECT *\n  FROM t   WHERE id = 5;", lexing);
        assert_eq!(a, "select * from t where id = 5");
        // Values keep their case.
        assert_ne!(key("select 'A'", lexing), key("select 'a'", lexing));
        assert_eq!(
            key("select * from t where id = 5", lexing),
            "select * from t where id = 5"
        );
        assert_ne!(a, key("SELECT * FROM t WHERE id = 6", lexing));
        assert_eq!(key("", lexing), "");
    }

    #[test]
    fn notes_say_when_and_what() {
        let run = |ago, rows, failed| Run { ago, rows, failed };
        let note = |sql, run| note(sql, Lexing::default(), run);
        assert_eq!(
            note("select 1", run(5 * 3600, Some(105), false)),
            "5 h ago · 105 rows"
        );
        assert_eq!(
            note("select 1", run(30, Some(1), false)),
            "just now · 1 row"
        );
        assert_eq!(
            note("update t set a = 1", run(120, Some(3), false)),
            "2 min ago · 3 rows changed"
        );
        assert_eq!(
            note("delete from t", run(120, Some(1), false)),
            "2 min ago · 1 row changed"
        );
        assert_eq!(
            note("select 1", run(3 * 86_400, None, true)),
            "3 d ago · failed"
        );
        assert_eq!(note("select 1", run(-5, None, false)), "just now");
    }
}
