//! Cell values for the value viewer: what a value is (NULL, JSON, text), JSON shown
//! readably, and differences between two values, as byte ranges to highlight.

use std::ops::Range;
use std::time::Duration;

use serde_json::Value;
use similar::{ChangeTag, TextDiff};

/// Longest a diff may take; past it the result is coarser, not wrong.
const DIFF_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Null,
    /// A JSON object or array (plain scalars stay text).
    Json,
    Text,
}

impl ValueKind {
    pub fn of(value: Option<&str>) -> Self {
        match value {
            None => ValueKind::Null,
            Some(v) if json(v).is_some() => ValueKind::Json,
            Some(_) => ValueKind::Text,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ValueKind::Null => "NULL",
            ValueKind::Json => "JSON",
            ValueKind::Text => "Text",
        }
    }
}

/// `value` parsed, if it is a JSON object or array.
fn json(value: &str) -> Option<Value> {
    let trimmed = value.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    serde_json::from_str(value).ok()
}

/// A JSON object or array indented, keys in their original order; other values
/// as they are.
pub fn pretty(value: &str) -> String {
    json(value)
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| value.to_string())
}

/// Like [`pretty`], with object keys sorted: two JSON values that differ only in
/// key order or formatting come out the same.
pub fn normalized(value: &str) -> String {
    match json(value) {
        Some(mut v) => {
            sort_keys(&mut v);
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| value.to_string())
        }
        None => value.to_string(),
    }
}

fn sort_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.sort_keys();
            map.values_mut().for_each(sort_keys);
        }
        Value::Array(items) => items.iter_mut().for_each(sort_keys),
        _ => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Line,
    Word,
}

/// Differences between two texts, for showing them side by side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// Byte ranges in the old text that are gone in the new one.
    pub removed: Vec<Range<usize>>,
    /// Byte ranges in the new text that the old one doesn't have.
    pub added: Vec<Range<usize>>,
    /// Places that differ (a run of removed and added parts counts once).
    pub changes: usize,
}

/// One text with the differences inline: removed and added parts one after the
/// other, for a single view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    pub text: String,
    pub removed: Vec<Range<usize>>,
    pub added: Vec<Range<usize>>,
    pub changes: usize,
}

fn text_diff<'a>(old: &'a str, new: &'a str, granularity: Granularity) -> TextDiff<'a, 'a, str> {
    let mut config = TextDiff::configure();
    config.timeout(DIFF_TIMEOUT);
    match granularity {
        Granularity::Line => config.diff_lines(old, new),
        Granularity::Word => config.diff_words(old, new),
    }
}

/// What changed from `old` to `new`.
pub fn diff(old: &str, new: &str, granularity: Granularity) -> Diff {
    let mut out = Diff::default();
    let (mut at_old, mut at_new) = (0, 0);
    let mut in_change = false;
    for change in text_diff(old, new, granularity).iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                at_old += len;
                at_new += len;
                in_change = false;
                continue;
            }
            ChangeTag::Delete => {
                push(&mut out.removed, at_old..at_old + len);
                at_old += len;
            }
            ChangeTag::Insert => {
                push(&mut out.added, at_new..at_new + len);
                at_new += len;
            }
        }
        if !in_change {
            out.changes += 1;
            in_change = true;
        }
    }
    out
}

/// `old` and `new` merged: by lines, removed lines start with `- ` and added ones
/// with `+ ` (unchanged ones with two spaces); by words, removed and added words
/// stand next to each other in the running text.
pub fn inline(old: &str, new: &str, granularity: Granularity) -> Inline {
    let mut out = Inline::default();
    let mut in_change = false;
    for change in text_diff(old, new, granularity).iter_all_changes() {
        let value = change.value();
        let (prefix, ranges) = match change.tag() {
            ChangeTag::Equal => ("  ", None),
            ChangeTag::Delete => ("- ", Some(&mut out.removed)),
            ChangeTag::Insert => ("+ ", Some(&mut out.added)),
        };
        let start = out.text.len();
        if granularity == Granularity::Line {
            out.text.push_str(prefix);
            out.text.push_str(value);
            if !value.ends_with('\n') {
                out.text.push('\n');
            }
        } else {
            out.text.push_str(value);
        }
        match ranges {
            Some(ranges) => {
                // A line's range leaves out its line break.
                let end = out.text.trim_end_matches('\n').len().max(start);
                push(ranges, start..end);
                if !in_change {
                    out.changes += 1;
                    in_change = true;
                }
            }
            None => in_change = false,
        }
    }
    out
}

/// Adds `range`, merging it into the last one if they touch.
fn push(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    match ranges.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts<'a>(src: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &src[r.clone()]).collect()
    }

    #[test]
    fn kinds() {
        assert_eq!(ValueKind::of(None), ValueKind::Null);
        assert_eq!(ValueKind::of(Some(r#"{"a": 1}"#)), ValueKind::Json);
        assert_eq!(ValueKind::of(Some(" [1, 2]")), ValueKind::Json);
        // Scalars and broken JSON stay text.
        assert_eq!(ValueKind::of(Some("42")), ValueKind::Text);
        assert_eq!(ValueKind::of(Some(r#""x""#)), ValueKind::Text);
        assert_eq!(ValueKind::of(Some("{not json")), ValueKind::Text);
    }

    #[test]
    fn pretty_keeps_key_order() {
        assert_eq!(
            pretty(r#"{"b":1,"a":[1,2]}"#),
            "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(pretty("plain text"), "plain text");
    }

    #[test]
    fn normalized_ignores_key_order_and_formatting() {
        let a = r#"{"b": 1, "a": {"y": 2, "x": 1}}"#;
        let b = r#"{"a":{"x":1,"y":2},"b":1}"#;
        assert_eq!(normalized(a), normalized(b));
        assert_ne!(pretty(a), pretty(b));
        // Arrays keep their order.
        assert_ne!(normalized("[1, 2]"), normalized("[2, 1]"));
        assert_eq!(normalized("text"), "text");
    }

    #[test]
    fn line_diff() {
        let (old, new) = ("a\nb\nc\n", "a\nB\nc\nd\n");
        let d = diff(old, new, Granularity::Line);
        assert_eq!(texts(old, &d.removed), ["b\n"]);
        assert_eq!(texts(new, &d.added), ["B\n", "d\n"]);
        assert_eq!(d.changes, 2);
        assert_eq!(diff(old, old, Granularity::Line), Diff::default());
    }

    #[test]
    fn word_diff() {
        let (old, new) = ("the quick fox", "the slow fox");
        let d = diff(old, new, Granularity::Word);
        assert_eq!(texts(old, &d.removed), ["quick"]);
        assert_eq!(texts(new, &d.added), ["slow"]);
        assert_eq!(d.changes, 1);
    }

    #[test]
    fn base_decides_what_counts_as_removed() {
        let (a, b) = ("x\n", "y\n");
        let ab = diff(a, b, Granularity::Line);
        let ba = diff(b, a, Granularity::Line);
        assert_eq!(texts(a, &ab.removed), texts(a, &ba.added));
        assert_eq!(texts(b, &ab.added), texts(b, &ba.removed));
    }

    #[test]
    fn inline_by_lines() {
        let i = inline("a\nb\n", "a\nc", Granularity::Line);
        assert_eq!(i.text, "  a\n- b\n+ c\n");
        assert_eq!(texts(&i.text, &i.removed), ["- b"]);
        assert_eq!(texts(&i.text, &i.added), ["+ c"]);
        assert_eq!(i.changes, 1);
    }

    #[test]
    fn inline_by_words() {
        let i = inline("the quick fox", "the slow fox", Granularity::Word);
        assert_eq!(i.text, "the quickslow fox");
        assert_eq!(texts(&i.text, &i.removed), ["quick"]);
        assert_eq!(texts(&i.text, &i.added), ["slow"]);
    }
}
