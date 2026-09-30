//! The text of a conversation: SQL blocks in answers, a title for it, and what
//! is kept of it (passwords in SQL never are).

use serde_json::Value;
use tangul_assist::redact::redact;
use tangul_llm::{Message, Part};

/// Longest title, in characters.
const TITLE_CHARS: usize = 60;

/// A piece of an answer: prose (Markdown), or a block of SQL the user can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Sql(String),
}

/// Fence languages taken as SQL; a fence without one stays text.
const SQL_LANGS: [&str; 6] = ["sql", "postgresql", "postgres", "pgsql", "mysql", "sqlite"];

/// Splits `text` at its fenced SQL blocks (```sql … ```). A block that is never
/// closed (still streaming) stays text.
pub fn segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut prose = String::new();
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let lang = line.trim().strip_prefix("```").map(str::trim);
        let is_sql = lang.is_some_and(|l| SQL_LANGS.contains(&l.to_ascii_lowercase().as_str()));
        if !is_sql {
            prose.push_str(line);
            continue;
        }
        let mut block = String::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if inner.trim() == "```" {
                closed = true;
                break;
            }
            block.push_str(inner);
        }
        if !closed {
            prose.push_str(line);
            prose.push_str(&block);
            continue;
        }
        if !prose.trim().is_empty() {
            out.push(Segment::Text(std::mem::take(&mut prose)));
        }
        prose.clear();
        out.push(Segment::Sql(block.trim_end().to_string()));
    }
    if !prose.trim().is_empty() {
        out.push(Segment::Text(prose));
    }
    out
}

/// A conversation's title from its first question: one line, shortened.
pub fn title_for(question: &str) -> String {
    let line = question.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TITLE_CHARS {
        return line;
    }
    let cut: String = line.chars().take(TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// `message` as it is kept: passwords in SQL (`… PASSWORD '…'`) replaced, in the
/// text, in tool inputs and in what tools returned.
pub fn for_storage(message: &Message) -> Message {
    let parts = message
        .parts
        .iter()
        .map(|p| match p {
            Part::Text { text } => Part::Text {
                text: redact(text).into_owned(),
            },
            Part::ToolCall(call) => {
                let mut call = call.clone();
                if let Some(Value::String(sql)) = call.input.get_mut("sql") {
                    *sql = redact(sql).into_owned();
                }
                Part::ToolCall(call)
            }
            Part::ToolResult(result) => {
                let mut result = result.clone();
                result.content = redact(&result.content).into_owned();
                Part::ToolResult(result)
            }
        })
        .collect();
    Message {
        role: message.role,
        parts,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tangul_llm::{Role, ToolCall, ToolResult};

    use super::*;

    #[test]
    fn sql_blocks_are_cut_out_of_the_prose() {
        let text = "Try this:\n```sql\nSELECT 1;\n```\nor\n```MySQL\nSELECT 2\n```\n";
        assert_eq!(
            segments(text),
            [
                Segment::Text("Try this:\n".into()),
                Segment::Sql("SELECT 1;".into()),
                Segment::Text("or\n".into()),
                Segment::Sql("SELECT 2".into()),
            ]
        );
        // Other languages and bare fences stay prose, as does a block still open.
        let other = "```json\n{}\n```\n";
        assert_eq!(segments(other), [Segment::Text(other.into())]);
        let open = "Here:\n```sql\nSELECT";
        assert_eq!(segments(open), [Segment::Text(open.into())]);
        assert_eq!(segments(""), []);
        assert_eq!(
            segments("```sql\nSELECT 1\n```"),
            [Segment::Sql("SELECT 1".into())]
        );
    }

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(
            title_for("  which  customers\nordered? "),
            "which customers ordered?"
        );
        let long = "word ".repeat(30);
        let title = title_for(&long);
        assert_eq!(title.chars().count(), TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn passwords_are_not_kept() {
        let secret = "CREATE ROLE ai LOGIN PASSWORD 'hunter2'";
        let message = Message {
            role: Role::Assistant,
            parts: vec![
                Part::Text {
                    text: secret.into(),
                },
                Part::ToolCall(ToolCall {
                    id: "1".into(),
                    name: "propose_sql".into(),
                    input: json!({"sql": secret, "title": "x"}),
                    signature: None,
                }),
                Part::ToolResult(ToolResult {
                    call_id: "1".into(),
                    name: "propose_sql".into(),
                    content: secret.into(),
                    is_error: false,
                }),
            ],
        };
        let kept = serde_json::to_string(&for_storage(&message)).unwrap();
        assert!(!kept.contains("hunter2"), "{kept}");
        assert!(kept.contains("\"title\":\"x\""));
    }
}
