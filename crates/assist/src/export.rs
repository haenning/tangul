//! A result's text as the bytes of a file ("Save as"): the text of a format
//! ([`crate::copy`], [`crate::mark::tsv`], INSERT statements) with the file's line
//! endings and, where asked for, a byte order mark.

/// Where a line break of the text is part of a value rather than the end of a
/// line: inside `quote`s (doubled ones included). `None`: every line break ends a
/// line (values carry theirs escaped, as in JSON or Markdown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quoted(pub Option<char>);

/// CSV and TSV: fields with line breaks are in double quotes.
pub const FIELDS: Quoted = Quoted(Some('"'));
/// SQL: line breaks in string literals.
pub const LITERALS: Quoted = Quoted(Some('\''));
/// JSON, Markdown: no line break inside a value.
pub const ESCAPED: Quoted = Quoted(None);

/// The file's bytes: UTF-8, lines ending in `\r\n` when `crlf` (the line breaks of
/// values kept as they are), a byte order mark first when `bom`.
pub fn file_bytes(text: &str, quoted: Quoted, crlf: bool, bom: bool) -> Vec<u8> {
    let mut out = String::with_capacity(text.len() + 3);
    if bom {
        out.push('\u{feff}');
    }
    if !crlf {
        out.push_str(text);
        return out.into_bytes();
    }
    let mut inside = false;
    for c in text.chars() {
        if Some(c) == quoted.0 {
            inside = !inside;
        }
        if c == '\n' && !inside {
            out.push('\r');
        }
        out.push(c);
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn line_endings_leave_values_alone() {
        let csv = "id,note\n1,\"two\nlines \"\"q\"\"\"\n2,x";
        assert_eq!(text(file_bytes(csv, FIELDS, false, false)), csv);
        assert_eq!(
            text(file_bytes(csv, FIELDS, true, false)),
            "id,note\r\n1,\"two\nlines \"\"q\"\"\"\r\n2,x"
        );
        let sql = "INSERT INTO t (a) VALUES\n    ('it''s\nhere'),\n    ('b');";
        assert_eq!(
            text(file_bytes(sql, LITERALS, true, false)),
            "INSERT INTO t (a) VALUES\r\n    ('it''s\nhere'),\r\n    ('b');"
        );
        assert_eq!(
            text(file_bytes(
                "[\n  {\"a\":\"x\\ny\"}\n]",
                ESCAPED,
                true,
                false
            )),
            "[\r\n  {\"a\":\"x\\ny\"}\r\n]"
        );
    }

    #[test]
    fn a_bom_comes_first() {
        let bytes = file_bytes("a\nü", FIELDS, true, true);
        assert_eq!(&bytes[..3], [0xef, 0xbb, 0xbf]);
        assert_eq!(text(bytes[3..].to_vec()), "a\r\nü");
        assert_eq!(file_bytes("", ESCAPED, false, true), [0xef, 0xbb, 0xbf]);
    }
}
