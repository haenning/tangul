//! Settings files (JSON) as written: every value with where it is in the text, to
//! underline what can't be applied, and what the cursor is in while typing.

use std::ops::Range;

use serde_json::{Map, Value};

/// A value and its byte range in the text.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub range: Range<usize>,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// `null`, a boolean, a number or a string.
    Scalar(Value),
    Array(Vec<Node>),
    Object(Vec<Member>),
}

/// A key of an object and its value.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub key: String,
    pub key_range: Range<usize>,
    pub value: Node,
}

impl Node {
    /// The value at `key`, if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.member(key).map(|m| &m.value)
    }

    pub fn member(&self, key: &str) -> Option<&Member> {
        match &self.kind {
            Kind::Object(members) => members.iter().find(|m| m.key == key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::Scalar(Value::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn items(&self) -> &[Node] {
        match &self.kind {
            Kind::Array(items) => items,
            _ => &[],
        }
    }

    pub fn value(&self) -> Value {
        match &self.kind {
            Kind::Scalar(v) => v.clone(),
            Kind::Array(items) => Value::Array(items.iter().map(Node::value).collect()),
            Kind::Object(members) => Value::Object(
                members
                    .iter()
                    .map(|m| (m.key.clone(), m.value.value()))
                    .collect(),
            ),
        }
    }
}

/// How bad a [`Problem`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The file can't be applied.
    Error,
    /// It can, but something in it has no effect or looks wrong.
    Warning,
}

/// Something to underline: a byte range of the text and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
}

impl Problem {
    pub fn error(range: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            range,
            severity: Severity::Error,
            message: message.into(),
        }
    }

    pub fn warning(range: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            range,
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

/// `text` read with positions; an empty text is an empty object. Not JSON: where
/// and why.
pub fn parse(text: &str) -> Result<Node, Problem> {
    if text.trim().is_empty() {
        return Ok(Node {
            range: 0..0,
            kind: Kind::Object(Vec::new()),
        });
    }
    // serde_json says what is wrong and where; the reader below then can't fail.
    if let Err(e) = serde_json::from_str::<Value>(text) {
        let at = offset_of(text, e.line(), e.column());
        let end = text[at..].chars().next().map_or(at, |c| at + c.len_utf8());
        let message = e.to_string();
        let message = message
            .rfind(" at line ")
            .map_or(message.as_str(), |i| &message[..i]);
        return Err(Problem::error(
            at..end,
            format!("not valid JSON: {message}"),
        ));
    }
    let mut reader = Reader { text, at: 0 };
    reader
        .value()
        .ok_or_else(|| Problem::error(0..0, "not valid JSON"))
}

/// The byte offset of serde_json's `line` and `column` (both from 1).
fn offset_of(text: &str, line: usize, column: usize) -> usize {
    let start: usize = text
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum();
    let mut at = (start + column.saturating_sub(1)).min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn skip_space(&mut self) {
        let rest = &self.text[self.at..];
        self.at += rest.len() - rest.trim_start().len();
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn value(&mut self) -> Option<Node> {
        self.skip_space();
        let start = self.at;
        let kind = match self.peek()? {
            b'{' => {
                self.at += 1;
                let mut members = Vec::new();
                loop {
                    self.skip_space();
                    match self.peek()? {
                        b'}' => break,
                        b',' => self.at += 1,
                        _ => {
                            let key_start = self.at;
                            let key = self.string()?;
                            let key_range = key_start..self.at;
                            self.skip_space();
                            self.at += 1; // ':'
                            let value = self.value()?;
                            members.push(Member {
                                key,
                                key_range,
                                value,
                            });
                        }
                    }
                }
                self.at += 1;
                Kind::Object(members)
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.peek()? {
                        b']' => break,
                        b',' => self.at += 1,
                        _ => items.push(self.value()?),
                    }
                }
                self.at += 1;
                Kind::Array(items)
            }
            b'"' => Kind::Scalar(Value::String(self.string()?)),
            _ => {
                let rest = &self.text[self.at..];
                let len = rest
                    .find(|c: char| c == ',' || c == ']' || c == '}' || c.is_whitespace())
                    .unwrap_or(rest.len());
                self.at += len;
                Kind::Scalar(serde_json::from_str(&rest[..len]).ok()?)
            }
        };
        Some(Node {
            range: start..self.at,
            kind,
        })
    }

    /// A string starting at the cursor, its quotes included.
    fn string(&mut self) -> Option<String> {
        let start = self.at;
        let bytes = self.text.as_bytes();
        let mut i = start + 1;
        while *bytes.get(i)? != b'"' {
            i += if bytes[i] == b'\\' { 2 } else { 1 };
        }
        self.at = i + 1;
        serde_json::from_str(&self.text[start..self.at]).ok()
    }
}

/// What can't be applied in `text`, a settings file whose every key is in
/// `defaults`, and what is unknown. `parse` reads a whole file's value; each value
/// that keeps it from being read is underlined with its reason, on its own (in the
/// defaults), so one mistake doesn't mark the rest. Unknown keys that are ignored
/// are warnings.
pub fn check(
    text: &str,
    defaults: &Value,
    parse: &dyn Fn(&Value) -> Result<(), String>,
) -> Vec<Problem> {
    let root = match self::parse(text) {
        Ok(root) => root,
        Err(problem) => return vec![problem],
    };
    if !matches!(root.kind, Kind::Object(_)) {
        return vec![Problem::error(
            root.range,
            "the file must contain a JSON object ({ … })",
        )];
    }
    let failing = parse(&root.value()).err();
    let mut checker = Checker {
        defaults,
        parse,
        failing: failing.is_some(),
        path: Vec::new(),
        out: Vec::new(),
    };
    checker.walk(&root, defaults);
    let mut out = checker.out;
    if let Some(e) = failing
        && !out.iter().any(|p| p.severity == Severity::Error)
    {
        let start = root.range.start;
        out.push(Problem::error(start..start + 1, e));
    }
    out
}

struct Checker<'a> {
    defaults: &'a Value,
    parse: &'a dyn Fn(&Value) -> Result<(), String>,
    /// Whether the whole file fails: only then are values tried on their own.
    failing: bool,
    path: Vec<String>,
    out: Vec<Problem>,
}

impl Checker<'_> {
    /// `node` at the current path, where the defaults have `default`.
    fn walk(&mut self, node: &Node, default: &Value) {
        match (&node.kind, default) {
            (Kind::Object(members), Value::Object(known)) if !known.is_empty() => {
                for m in members {
                    self.path.push(m.key.clone());
                    match known.get(&m.key) {
                        Some(d) => self.walk(&m.value, d),
                        None => self.unknown(m, known),
                    }
                    self.path.pop();
                }
            }
            _ if self.failing => self.leaf(node),
            _ => {}
        }
    }

    /// A key the defaults don't have: an error where the file can't have it,
    /// else a warning that it does nothing.
    fn unknown(&mut self, m: &Member, known: &Map<String, Value>) {
        match self.try_value(m.value.value()) {
            Err(e) => self.out.push(Problem::error(m.key_range.clone(), e)),
            Ok(()) => {
                let keys: Vec<String> = known.keys().map(|k| format!("\"{k}\"")).collect();
                self.out.push(Problem::warning(
                    m.key_range.clone(),
                    format!("unknown key, not used; here: {}", keys.join(", ")),
                ));
            }
        }
    }

    /// A value the defaults have no keys below: tried on its own, and a list's
    /// items each on their own.
    fn leaf(&mut self, node: &Node) {
        let Err(e) = self.try_value(node.value()) else {
            return;
        };
        let before = self.out.len();
        if let Kind::Array(items) = &node.kind {
            for item in items {
                if let Err(e) = self.try_value(Value::Array(vec![item.value()])) {
                    self.out.push(Problem::error(item.range.clone(), e));
                }
            }
        }
        if self.out.len() == before {
            self.out.push(Problem::error(node.range.clone(), e));
        }
    }

    /// Whether the defaults with `value` at the current path can be read.
    fn try_value(&self, value: Value) -> Result<(), String> {
        let mut file = self.defaults.clone();
        let mut at = &mut file;
        for key in &self.path {
            if !at.is_object() {
                *at = Value::Object(Map::new());
            }
            at = at
                .as_object_mut()
                .expect("an object")
                .entry(key.clone())
                .or_insert(Value::Null);
        }
        *at = value;
        (self.parse)(&file)
    }
}

/// Where the cursor is while typing: in a string that is an item of a list, with
/// the keys down to that list. `None` anywhere else. Reads only the text before
/// the cursor, so it works while the file isn't valid JSON yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InString {
    /// The keys from the top of the file down to the list.
    pub path: Vec<String>,
    /// Where the string's text starts (after its quote).
    pub start: usize,
    /// Its text after the cursor, up to its closing quote (or the line's end).
    pub after: String,
}

pub fn string_at(text: &str, offset: usize) -> Option<InString> {
    struct Frame {
        array: bool,
        /// The key this container is the value of.
        key: Option<String>,
        /// In an object: the last string, a key until a `:` follows.
        last: Option<String>,
        /// In an object: the key whose value comes now.
        current: Option<String>,
    }
    let before = text.get(..offset)?;
    let mut frames: Vec<Frame> = Vec::new();
    let mut string: Option<(usize, String)> = None;
    let mut chars = before.char_indices();
    while let Some((i, c)) = chars.next() {
        if let Some((_, s)) = &mut string {
            match c {
                '"' => {
                    let (_, s) = string.take()?;
                    if let Some(top) = frames.last_mut().filter(|f| !f.array) {
                        top.last = Some(s);
                    }
                }
                '\\' => {
                    if let Some((_, escaped)) = chars.next() {
                        s.push(escaped);
                    }
                }
                _ => s.push(c),
            }
            continue;
        }
        match c {
            '"' => string = Some((i + 1, String::new())),
            '{' | '[' => {
                let key = frames
                    .last()
                    .filter(|f| !f.array)
                    .and_then(|f| f.current.clone());
                frames.push(Frame {
                    array: c == '[',
                    key,
                    last: None,
                    current: None,
                });
            }
            '}' | ']' => {
                frames.pop();
            }
            ':' => {
                if let Some(top) = frames.last_mut() {
                    top.current = top.last.take();
                }
            }
            ',' => {
                if let Some(top) = frames.last_mut() {
                    top.current = None;
                    top.last = None;
                }
            }
            _ => {}
        }
    }
    let (start, _) = string?;
    if !frames.last()?.array {
        return None;
    }
    let path = frames
        .iter()
        .skip(1)
        .map(|f| f.key.clone())
        .collect::<Option<Vec<_>>>()?;
    let rest = &text[offset..];
    let end = rest.find(['"', '\n']).unwrap_or(rest.len());
    Some(InString {
        path,
        start,
        after: rest[..end].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node_at<'a>(root: &'a Node, path: &[&str]) -> &'a Node {
        path.iter().fold(root, |n, k| n.get(k).unwrap())
    }

    #[test]
    fn values_keep_where_they_are() {
        let text = r#"{ "a": {"b": [1, "x\"y", null]}, "c": true }"#;
        let root = parse(text).unwrap();
        let b = node_at(&root, &["a", "b"]);
        assert_eq!(&text[b.range.clone()], r#"[1, "x\"y", null]"#);
        assert_eq!(b.items()[1].as_str(), Some("x\"y"));
        assert_eq!(&text[b.items()[1].range.clone()], r#""x\"y""#);
        assert_eq!(&text[root.member("c").unwrap().key_range.clone()], "\"c\"");
        assert_eq!(root.value(), serde_json::from_str::<Value>(text).unwrap());
        assert_eq!(parse("  ").unwrap().value(), json!({}));
    }

    #[test]
    fn invalid_json_says_where() {
        let text = "{\n  \"a\": 1,\n  \"b\" 2\n}";
        let e = parse(text).unwrap_err();
        assert_eq!(&text[e.range.clone()], "2");
        assert!(
            e.message.starts_with("not valid JSON: expected `:`"),
            "{}",
            e.message
        );
        assert!(!e.message.contains("line"), "{}", e.message);
    }

    /// A file of `{"n": u8, "mode": "a" | "b", "list": [u8], "strict": {"x": bool}}`,
    /// unknown keys refused only under `strict`.
    fn read(v: &Value) -> Result<(), String> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        #[allow(dead_code)]
        struct File {
            n: u8,
            mode: Mode,
            list: Vec<u8>,
            strict: Strict,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "lowercase")]
        enum Mode {
            #[default]
            A,
            B,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default, deny_unknown_fields)]
        #[allow(dead_code)]
        struct Strict {
            x: bool,
        }
        serde_json::from_value::<File>(v.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn defaults() -> Value {
        json!({"n": 1, "mode": "a", "list": [], "strict": {"x": false}})
    }

    fn marked(text: &str) -> Vec<(String, Severity, String)> {
        check(text, &defaults(), &read)
            .into_iter()
            .map(|p| (text[p.range].to_string(), p.severity, p.message))
            .collect()
    }

    #[test]
    fn each_wrong_value_is_marked_with_what_would_do() {
        let text = r#"{"n": 300, "mode": "c", "list": [1, "two", 3], "strict": {"x": true}}"#;
        let m = marked(text);
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!(m[0].0, "300");
        assert!(m[0].2.contains("expected u8"), "{m:?}");
        assert_eq!(m[1].0, "\"c\"");
        assert!(m[1].2.contains("expected `a` or `b`"), "{m:?}");
        assert_eq!(m[2].0, "\"two\"", "the item, not the list");
        assert!(m.iter().all(|p| p.1 == Severity::Error));
    }

    #[test]
    fn unknown_keys_are_errors_where_refused_and_warnings_where_ignored() {
        let m = marked(r#"{"later": 1, "strict": {"y": true}}"#);
        assert_eq!(m.len(), 2, "{m:?}");
        assert_eq!(m[0].0, "\"later\"");
        assert_eq!(m[0].1, Severity::Warning);
        assert!(m[0].2.contains(r#""n", "mode""#), "{m:?}");
        assert_eq!(m[1].0, "\"y\"");
        assert_eq!(m[1].1, Severity::Error);
        assert!(m[1].2.contains("unknown field `y`"), "{m:?}");
        assert!(marked(r#"{"n": 2}"#).is_empty());
        assert!(marked("").is_empty());
        let m = marked("[1]");
        assert!(m[0].2.contains("must contain a JSON object"), "{m:?}");
        assert!(marked("{").len() == 1);
    }

    #[test]
    fn a_failure_no_single_value_explains_goes_on_the_file() {
        let never = |_: &Value| Err("no".to_string());
        let problems = check(r#"{"n": 2}"#, &defaults(), &never);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].message, "no");
    }

    #[test]
    fn the_string_being_typed_and_its_list() {
        let text = r#"{"ai": {"x": ["a", "b"], "list": ["one", "tw"#;
        let at = string_at(text, text.len()).unwrap();
        assert_eq!(at.path, ["ai", "list"]);
        assert_eq!(&text[at.start..], "tw");
        assert_eq!(at.after, "");
        let text = r#"{"ai": {"list": ["o\"ne", "two"]}}"#;
        let cursor = text.find("wo").unwrap();
        let at = string_at(text, cursor).unwrap();
        assert_eq!(at.after, "wo");
        assert_eq!(at.path, ["ai", "list"]);
        assert!(string_at(text, text.find("ai").unwrap()).is_none(), "a key");
        assert!(string_at(r#"{"a": "b"#, 8).is_none(), "not in a list");
        assert!(string_at(r#"{"a": [1, "#, 10).is_none(), "outside a string");
    }
}
