//! What a conversation with a model is made of, the same for every provider. The
//! adapters translate it to and from their API.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One turn: what the user said (or the tool results sent back), or what the
/// model answered (text and tool calls).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    pub fn assistant(parts: Vec<Part>) -> Self {
        Self {
            role: Role::Assistant,
            parts,
        }
    }

    /// Its text parts, joined.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.parts.iter().filter_map(|p| match p {
            Part::ToolCall(call) => Some(call),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

/// The model asks for a tool to be called.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Pairs the call with its result (made up for APIs that have none).
    pub id: String,
    pub name: String,
    pub input: Value,
    /// Opaque data the API wants back with the call (Gemini's thought signature).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// What a tool call returned, sent back to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    /// The tool's name (Gemini and Ollama pair results by name).
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// A tool as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema of its input.
    pub input_schema: Value,
}

/// One call to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    /// Longest answer (Anthropic needs one).
    pub max_tokens: u32,
}

/// What arrives while the model answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    /// The model started a call to this tool.
    ToolCall(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StopReason {
    /// Done, or waiting for tool results.
    #[default]
    EndTurn,
    ToolUse,
    /// The answer hit `max_tokens`.
    MaxTokens,
    /// Anything else the API reports (content filter, …).
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The model's whole answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub message: Message,
    pub stop: StopReason,
    pub usage: Usage,
}

impl Default for Reply {
    fn default() -> Self {
        Self {
            message: Message::assistant(Vec::new()),
            stop: StopReason::default(),
            usage: Usage::default(),
        }
    }
}

impl Reply {
    /// Appends streamed text to the last text part (or starts one).
    pub(crate) fn push_text(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        match self.message.parts.last_mut() {
            Some(Part::Text { text }) => text.push_str(delta),
            _ => self.message.parts.push(Part::Text {
                text: delta.to_string(),
            }),
        }
    }
}

/// Why a call to the model gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The provider needs an API key and none is set.
    NoKey,
    /// No model is chosen.
    NoModel,
    /// The API answered with an error status.
    Api { status: u16, message: String },
    /// It could not be reached, or the connection broke.
    Network(String),
    /// The answer wasn't what the API documents.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoKey => write!(f, "no API key is set"),
            Error::NoModel => write!(f, "no model is chosen"),
            Error::Api { status, message } => write!(f, "{status}: {message}"),
            Error::Network(m) => write!(f, "could not reach it: {m}"),
            Error::Invalid(m) => write!(f, "unexpected answer: {m}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_serialize_tagged_and_round_trip() {
        let m = Message::assistant(vec![
            Part::Text { text: "Hi".into() },
            Part::ToolCall(ToolCall {
                id: "1".into(),
                name: "list_connections".into(),
                input: json!({}),
                signature: None,
            }),
        ]);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["parts"][0], json!({"type": "text", "text": "Hi"}));
        assert_eq!(v["parts"][1]["type"], "tool_call");
        assert!(v["parts"][1].get("signature").is_none());
        let back: Message = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.text(), "Hi");
        assert_eq!(back.tool_calls().count(), 1);
    }

    #[test]
    fn text_deltas_join_into_one_part() {
        let mut r = Reply::default();
        r.push_text("a");
        r.push_text("");
        r.push_text("b");
        assert_eq!(r.message.parts, [Part::Text { text: "ab".into() }]);
    }

    #[test]
    fn errors_read_well() {
        assert_eq!(Error::NoKey.to_string(), "no API key is set");
        assert_eq!(Error::NoModel.to_string(), "no model is chosen");
        let api = Error::Api {
            status: 401,
            message: "invalid key".into(),
        };
        assert_eq!(api.to_string(), "401: invalid key");
        assert!(Error::Network("x".into()).to_string().contains("reach"));
        assert!(
            Error::Invalid("x".into())
                .to_string()
                .contains("unexpected")
        );
    }
}
