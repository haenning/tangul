//! A forgiving SQL tokenizer. It never fails: unterminated strings or comments run
//! to the end of the input, which is what half-typed queries in an editor look like.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Unquoted identifier or keyword.
    Word,
    /// `"ident"` or `` `ident` ``.
    QuotedIdent,
    /// `'text'` or `$tag$text$tag$`.
    String,
    Number,
    /// `?`, `$1`.
    Param,
    /// Any other single character (`.`, `,`, `(`, `=`, …).
    Punct,
    Comment,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    /// Byte range in the source.
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }

    pub fn is_trivia(&self) -> bool {
        matches!(self.kind, TokenKind::Space | TokenKind::Comment)
    }

    pub fn is_ident(&self) -> bool {
        matches!(self.kind, TokenKind::Word | TokenKind::QuotedIdent)
    }

    pub fn is_punct(&self, src: &str, c: char) -> bool {
        self.kind == TokenKind::Punct && src[self.start..].starts_with(c)
    }

    /// Case-insensitive keyword check (only for unquoted words).
    pub fn is_word(&self, src: &str, word: &str) -> bool {
        self.kind == TokenKind::Word && self.text(src).eq_ignore_ascii_case(word)
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric()
}

/// Lexical rules that differ between databases. The default is what [`tokenize`]
/// uses: no `#` comments, flat `/* */` comments and `$tag$` dollar quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lexing {
    /// `# …` comments to the end of the line (MySQL).
    pub hash_comments: bool,
    /// `/* /* … */ */` nests (Postgres).
    pub nested_comments: bool,
    /// `$tag$ … $tag$` string quoting (Postgres).
    pub dollar_quotes: bool,
    /// The client command `DELIMITER x` changes the statement terminator (MySQL).
    pub delimiter_command: bool,
}

impl Default for Lexing {
    fn default() -> Self {
        Self {
            hash_comments: false,
            nested_comments: false,
            dollar_quotes: true,
            delimiter_command: false,
        }
    }
}

pub fn tokenize(src: &str) -> Vec<Token> {
    tokenize_with(src, Lexing::default())
}

/// [`tokenize`] with a database's lexical rules.
pub fn tokenize_with(src: &str, lexing: Lexing) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        let rest = &src[start..];
        let (kind, len) = if c.is_whitespace() {
            let len = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            (TokenKind::Space, len)
        } else if rest.starts_with("--") || (lexing.hash_comments && c == '#') {
            (TokenKind::Comment, rest.find('\n').unwrap_or(rest.len()))
        } else if rest.starts_with("/*") {
            (
                TokenKind::Comment,
                block_comment_len(rest, lexing.nested_comments),
            )
        } else if c == '\'' {
            (TokenKind::String, quoted_len(rest, '\''))
        } else if c == '"' || c == '`' {
            (TokenKind::QuotedIdent, quoted_len(rest, c))
        } else if c == '$' {
            dollar(rest, lexing.dollar_quotes)
        } else if c == '?' {
            (TokenKind::Param, 1)
        } else if c.is_ascii_digit()
            || (c == '.' && rest[1..].starts_with(|c: char| c.is_ascii_digit()))
        {
            (TokenKind::Number, number_len(rest))
        } else if is_ident_start(c) {
            let len = rest
                .char_indices()
                .find(|&(_, c)| !is_ident_continue(c))
                .map_or(rest.len(), |(i, _)| i);
            (TokenKind::Word, len)
        } else {
            (TokenKind::Punct, c.len_utf8())
        };
        tokens.push(Token {
            kind,
            start,
            end: start + len,
        });
        while chars.peek().is_some_and(|&(i, _)| i < start + len) {
            chars.next();
        }
    }
    tokens
}

/// Length of a quoted token starting at `rest[0] == quote`; a doubled quote escapes it.
fn quoted_len(rest: &str, quote: char) -> usize {
    let mut iter = rest.char_indices().skip(1).peekable();
    while let Some((i, c)) = iter.next() {
        if c == '\\' && quote == '\'' {
            iter.next();
        } else if c == quote {
            if iter.peek().is_some_and(|&(_, n)| n == quote) {
                iter.next();
            } else {
                return i + 1;
            }
        }
    }
    rest.len()
}

/// Length of a `/* … */` comment starting at `rest`; nested ones count when `nested`.
fn block_comment_len(rest: &str, nested: bool) -> usize {
    if !nested {
        return rest.find("*/").map_or(rest.len(), |i| i + 2);
    }
    let mut depth = 0usize;
    let mut i = 0;
    while i < rest.len() {
        if rest[i..].starts_with("/*") {
            depth += 1;
            i += 2;
        } else if rest[i..].starts_with("*/") {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += rest[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    rest.len()
}

/// `$1` parameters and, with `quotes`, `$tag$ … $tag$` dollar quoting (Postgres).
fn dollar(rest: &str, quotes: bool) -> (TokenKind, usize) {
    let digits = rest[1..]
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len() - 1);
    if digits > 0 {
        return (TokenKind::Param, 1 + digits);
    }
    if !quotes {
        return (TokenKind::Punct, 1);
    }
    let tag_len = rest[1..]
        .find(|c: char| !is_ident_continue(c) || c == '$')
        .map(|i| i + 1);
    match tag_len {
        Some(i) if rest[i..].starts_with('$') => {
            let tag = &rest[..=i];
            let body = &rest[tag.len()..];
            let len = body
                .find(tag)
                .map_or(rest.len(), |j| tag.len() + j + tag.len());
            (TokenKind::String, len)
        }
        _ => (TokenKind::Punct, 1),
    }
}

fn number_len(rest: &str) -> usize {
    let mut len = 0;
    let bytes = rest.as_bytes();
    while len < bytes.len() {
        let b = bytes[len];
        let exp_sign = (b == b'+' || b == b'-') && len > 0 && matches!(bytes[len - 1], b'e' | b'E');
        if b.is_ascii_digit() || b == b'.' || b == b'e' || b == b'E' || exp_sign {
            len += 1;
        } else {
            break;
        }
    }
    len
}

/// Identifier text without quotes (doubled quotes undone).
pub fn unquote(text: &str) -> String {
    match text.chars().next() {
        Some(q @ ('"' | '`')) => {
            let inner = text[1..].strip_suffix(q).unwrap_or(&text[1..]);
            inner.replace(&format!("{q}{q}"), &q.to_string())
        }
        _ => text.to_string(),
    }
}

/// Canonical form of a query: literals become `?`, keywords and unquoted names
/// lowercase, whitespace and comments normalized, and `IN (?, ?, …)` / multi-row
/// `VALUES` lists collapsed. Queries that differ only in their values share it.
pub fn fingerprint(sql: &str) -> String {
    let tokens = tokenize(sql);
    let mut parts: Vec<String> = Vec::new();
    for t in tokens.iter().filter(|t| !t.is_trivia()) {
        let text = t.text(sql);
        parts.push(match t.kind {
            TokenKind::String | TokenKind::Number | TokenKind::Param => "?".into(),
            TokenKind::Word => text.to_lowercase(),
            _ => text.into(),
        });
    }
    while parts.last().is_some_and(|p| p == ";") {
        parts.pop();
    }
    let parts = collapse_lists(parts);
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| parts[j].as_str());
        let glue =
            matches!(p.as_str(), "." | "," | ")" | ";") || matches!(prev, Some("." | "(") | None);
        if !glue {
            out.push(' ');
        }
        out.push_str(p);
    }
    out
}

/// `( ? , ? , ? )` → `( ? )` and `(?), (?), (?)` → `(?)`.
fn collapse_lists(parts: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    let mut i = 0;
    while i < parts.len() {
        if parts[i] == "(" {
            let mut j = i + 1;
            let mut ok = false;
            while j < parts.len() && parts[j] == "?" {
                ok = true;
                if parts.get(j + 1).is_some_and(|p| p == ",")
                    && parts.get(j + 2).is_some_and(|p| p == "?")
                {
                    j += 2;
                } else {
                    j += 1;
                    break;
                }
            }
            if ok && parts.get(j).is_some_and(|p| p == ")") {
                // Drop a repeated `(?)` group right after an identical one.
                let n = out.len();
                let repeat = n >= 4
                    && out[n - 1] == ","
                    && out[n - 2] == ")"
                    && out[n - 3] == "?"
                    && out[n - 4] == "(";
                if repeat {
                    out.pop();
                } else {
                    out.extend(["(".into(), "?".into(), ")".into()]);
                }
                i = j + 1;
                continue;
            }
        }
        out.push(parts[i].clone());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<(TokenKind, &str)> {
        tokenize(sql)
            .into_iter()
            .filter(|t| !t.is_trivia())
            .map(|t| (t.kind, t.text(sql)))
            .collect()
    }

    #[test]
    fn tokens() {
        use TokenKind::*;
        assert_eq!(
            kinds(r#"SELECT "a""b", 'it''s', 1.5e-3, $1 FROM t -- c"#),
            [
                (Word, "SELECT"),
                (QuotedIdent, r#""a""b""#),
                (Punct, ","),
                (String, "'it''s'"),
                (Punct, ","),
                (Number, "1.5e-3"),
                (Punct, ","),
                (Param, "$1"),
                (Word, "FROM"),
                (Word, "t"),
            ]
        );
        assert_eq!(
            kinds("$fn$ body ' $fn$ x"),
            [(String, "$fn$ body ' $fn$"), (Word, "x")]
        );
        assert_eq!(kinds("'open"), [(String, "'open")]);
        assert_eq!(unquote(r#""a""b""#), r#"a"b"#);
        assert_eq!(unquote("`t`"), "t");
    }

    #[test]
    fn dialect_rules() {
        use TokenKind::*;
        let kinds_with = |sql: &'static str, lexing| -> Vec<(TokenKind, &str)> {
            tokenize_with(sql, lexing)
                .into_iter()
                .filter(|t| t.kind != Space)
                .map(|t| (t.kind, t.text(sql)))
                .collect()
        };
        let mysql = Lexing {
            hash_comments: true,
            dollar_quotes: false,
            delimiter_command: true,
            ..Lexing::default()
        };
        let postgres = Lexing {
            nested_comments: true,
            ..Lexing::default()
        };
        assert_eq!(
            kinds_with("a # b;\nc", mysql),
            [(Word, "a"), (Comment, "# b;"), (Word, "c")]
        );
        assert_eq!(kinds("a # b"), [(Word, "a"), (Punct, "#"), (Word, "b")]);
        assert_eq!(
            kinds_with("/* a /* b */ ; */ x", postgres),
            [(Comment, "/* a /* b */ ; */"), (Word, "x")]
        );
        assert_eq!(
            kinds_with("/* unclosed /* */", postgres),
            [(Comment, "/* unclosed /* */")]
        );
        assert_eq!(
            kinds_with("$$;", mysql),
            [(Punct, "$"), (Punct, "$"), (Punct, ";")]
        );
        assert_eq!(kinds_with("$1", mysql), [(Param, "$1")]);
    }

    #[test]
    fn fingerprints() {
        assert_eq!(
            fingerprint("SELECT *  FROM users WHERE id = 42 -- x\n;"),
            "select * from users where id = ?"
        );
        assert_eq!(
            fingerprint("select * from users where id in (1, 2, 3)"),
            fingerprint("SELECT * FROM users WHERE id IN (7)")
        );
        assert_eq!(
            fingerprint("insert into t (a, b) values (1, 'x'), (2, 'y')"),
            "insert into t (a, b) values (?)"
        );
        assert_eq!(
            fingerprint("select u.name from app.users u"),
            "select u.name from app.users u"
        );
    }
}
