//! Passwords out of SQL before it is kept (history, bookmarks, the audit log):
//! `CREATE ROLE … PASSWORD '…'`, `CREATE USER … IDENTIFIED BY '…'`,
//! `SET PASSWORD … = '…'` keep their shape with `'***'` in place of the secret.

use std::borrow::Cow;

use crate::lexer::{Token, TokenKind, tokenize};

/// What a password becomes.
pub const MASK: &str = "'***'";

/// `sql` with the string literals that are passwords replaced by [`MASK`].
pub fn redact(sql: &str) -> Cow<'_, str> {
    let tokens: Vec<Token> = tokenize(sql)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect();
    let mut secrets = Vec::new();
    // Per statement: whether it has `IDENTIFIED`, and whether it is `SET PASSWORD`.
    let (mut identified, mut set_password, mut first) = (false, false, true);
    for (i, t) in tokens.iter().enumerate() {
        if t.is_punct(sql, ';') {
            (identified, set_password, first) = (false, false, true);
            continue;
        }
        if t.is_word(sql, "IDENTIFIED") {
            identified = true;
        }
        if first && t.is_word(sql, "SET") {
            set_password = tokens
                .get(i + 1)
                .is_some_and(|n| n.is_word(sql, "PASSWORD"));
        }
        first = false;
        if t.kind != TokenKind::String || i == 0 {
            continue;
        }
        let before = &tokens[i - 1];
        let secret = before.is_word(sql, "PASSWORD")
            || (identified && before.is_word(sql, "BY"))
            || (set_password && before.is_punct(sql, '='));
        if secret {
            secrets.push(t);
        }
    }
    if secrets.is_empty() {
        return Cow::Borrowed(sql);
    }
    let mut out = String::with_capacity(sql.len());
    let mut at = 0;
    for t in secrets {
        out.push_str(&sql[at..t.start]);
        out.push_str(MASK);
        at = t.end;
    }
    out.push_str(&sql[at..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_passwords() {
        let cases = [
            (
                "CREATE ROLE ai LOGIN PASSWORD 's3cr3t'",
                "CREATE ROLE ai LOGIN PASSWORD '***'",
            ),
            (
                "alter role ai with encrypted password 'x''y';",
                "alter role ai with encrypted password '***';",
            ),
            (
                "CREATE USER 'ai'@'%' IDENTIFIED BY 'pw'",
                "CREATE USER 'ai'@'%' IDENTIFIED BY '***'",
            ),
            (
                "ALTER USER 'ai'@'%' IDENTIFIED WITH caching_sha2_password BY 'pw'",
                "ALTER USER 'ai'@'%' IDENTIFIED WITH caching_sha2_password BY '***'",
            ),
            (
                "SET PASSWORD FOR 'ai'@'%' = 'pw'",
                "SET PASSWORD FOR 'ai'@'%' = '***'",
            ),
            (
                "CREATE ROLE a PASSWORD 'p1'; CREATE ROLE b /* c */ PASSWORD\n'p2'",
                "CREATE ROLE a PASSWORD '***'; CREATE ROLE b /* c */ PASSWORD\n'***'",
            ),
        ];
        for (sql, want) in cases {
            assert_eq!(redact(sql), want, "{sql}");
        }
    }

    #[test]
    fn leaves_everything_else() {
        for sql in [
            "SELECT 'password' FROM users WHERE password = 'x'",
            "SELECT * FROM t ORDER BY 'a'",
            "UPDATE users SET password = 'hash'",
            "CREATE USER 'ai'@'%' IDENTIFIED BY RANDOM PASSWORD",
            "SELECT 1; SELECT 'x' FROM t GROUP BY 'y'",
        ] {
            assert!(matches!(redact(sql), Cow::Borrowed(_)), "{sql}");
        }
    }
}
