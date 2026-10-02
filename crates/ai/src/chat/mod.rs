//! The assistant's conversation: the model (`tangul_llm`) with the tools of the
//! [`Registry`], called in-process through the same [`Ctx`] as MCP clients use,
//! so the same rules hold (exposed databases only, reads through the gate as the
//! assistant, writes only proposed). No UI: the app shows the [`ChatEvent`]s and
//! keeps the messages.

mod prompt;
mod text;

use serde_json::Value;
use tangul_llm::{Delta, Error, Message, Part, Provider, Request, Role, ToolDef, ToolResult};

use crate::ctx::Ctx;
use crate::tool::Registry;

pub use prompt::system_prompt;
pub use text::{Segment, for_storage, segments, title_for};

/// Tool rounds per question when the settings don't say.
pub const DEFAULT_ROUNDS: usize = 12;
/// Longest answer asked for.
pub const MAX_TOKENS: u32 = 8192;

/// How a conversation runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSettings {
    pub model: String,
    /// Most model answers with tool calls per question; then it stops and says so.
    pub max_rounds: usize,
    /// The connection the user works on (an exposed connection's name), the
    /// default for questions that don't name one.
    pub focus: Option<String>,
}

/// What happens while the assistant answers, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    /// Text of the answer as it streams in.
    Text(String),
    /// The model started calling a tool.
    ToolStarted(String),
    /// A tool ran: its name, input and whether it worked, with the first line of
    /// what it returned.
    ToolRan {
        name: String,
        input: Value,
        ok: bool,
        summary: String,
    },
    /// A finished message for the conversation (the model's answer, or the tool
    /// results sent back to it).
    Message(Message),
}

/// Why the assistant stopped without a final answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatError {
    /// The provider failed (no key, network, the API's error).
    Provider(Error),
    /// It kept calling tools past [`ChatSettings::max_rounds`].
    TooManyRounds(usize),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Provider(e) => write!(f, "{e}"),
            ChatError::TooManyRounds(n) => write!(
                f,
                "stopped after {n} rounds of tool calls; ask again to go on"
            ),
        }
    }
}

impl std::error::Error for ChatError {}

/// The registry's tools `ctx` may use as the model sees them, each mapped the same
/// way.
pub fn tool_defs(registry: &Registry, ctx: &Ctx) -> Vec<ToolDef> {
    registry
        .specs_for(ctx)
        .into_iter()
        .map(|s| ToolDef {
            name: s.name.to_string(),
            description: s.description.to_string(),
            input_schema: s.input_schema,
        })
        .collect()
}

/// Answers the last message of `history` (the user's): asks the model, runs the
/// tools it calls, and asks again with their results until it answers without
/// tools. Every message it adds goes to `on` as [`ChatEvent::Message`].
pub async fn answer(
    provider: &dyn Provider,
    registry: &Registry,
    ctx: &Ctx,
    settings: &ChatSettings,
    history: &[Message],
    on: &mut (dyn FnMut(ChatEvent) + Send),
) -> Result<(), ChatError> {
    let mut messages = history.to_vec();
    let tools = tool_defs(registry, ctx);
    let system = system_prompt(ctx, settings.focus.as_deref());
    for _ in 0..settings.max_rounds.max(1) {
        let request = Request {
            model: settings.model.clone(),
            system: system.clone(),
            messages: normalized(&messages),
            tools: tools.clone(),
            max_tokens: MAX_TOKENS,
        };
        let reply = provider
            .chat(&request, &mut |delta| match delta {
                Delta::Text(t) => on(ChatEvent::Text(t)),
                Delta::ToolCall(name) => on(ChatEvent::ToolStarted(name)),
            })
            .await
            .map_err(ChatError::Provider)?;
        let calls: Vec<_> = reply.message.tool_calls().cloned().collect();
        on(ChatEvent::Message(reply.message.clone()));
        messages.push(reply.message);
        if calls.is_empty() {
            return Ok(());
        }
        let mut results = Vec::new();
        for call in calls {
            let outcome = registry.call(ctx, &call.name, call.input.clone()).await;
            let (content, ok) = match outcome {
                Ok(out) => (out.text, true),
                Err(e) => (e.to_string(), false),
            };
            on(ChatEvent::ToolRan {
                name: call.name.clone(),
                input: call.input,
                ok,
                summary: content.lines().next().unwrap_or_default().to_string(),
            });
            results.push(Part::ToolResult(ToolResult {
                call_id: call.id,
                name: call.name,
                content,
                is_error: !ok,
            }));
        }
        let results = Message {
            role: Role::User,
            parts: results,
        };
        on(ChatEvent::Message(results.clone()));
        messages.push(results);
    }
    Err(ChatError::TooManyRounds(settings.max_rounds.max(1)))
}

/// `messages` as every API takes them: turns alternate (consecutive messages of
/// one role are merged, tool results first), empty ones are left out.
fn normalized(messages: &[Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for m in messages.iter().filter(|m| !m.parts.is_empty()) {
        match out.last_mut() {
            Some(last) if last.role == m.role => last.parts.extend(m.parts.iter().cloned()),
            _ => out.push(m.clone()),
        }
    }
    for m in out.iter_mut().filter(|m| m.role == Role::User) {
        // A stable sort: results keep their order, before the text.
        m.parts.sort_by_key(|p| !matches!(p, Part::ToolResult(_)));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;
    use tangul_llm::{Reply, ToolCall};
    use tangul_service::knowledge::Knowledge;

    use super::*;

    /// Answers with the replies it was given, in turn, and keeps the requests.
    struct Script {
        replies: Mutex<Vec<Result<Message, Error>>>,
        seen: Mutex<Vec<Request>>,
    }

    impl Script {
        fn new(replies: Vec<Result<Message, Error>>) -> Self {
            Self {
                replies: Mutex::new(replies),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Provider for Script {
        async fn chat(
            &self,
            req: &Request,
            on: &mut (dyn FnMut(Delta) + Send),
        ) -> Result<Reply, Error> {
            self.seen.lock().unwrap().push(req.clone());
            let message = self.replies.lock().unwrap().remove(0)?;
            for call in message.tool_calls() {
                on(Delta::ToolCall(call.name.clone()));
            }
            on(Delta::Text(message.text()));
            Ok(Reply {
                message,
                ..Reply::default()
            })
        }

        async fn models(&self) -> Result<Vec<String>, Error> {
            Ok(Vec::new())
        }
    }

    fn call(id: &str, name: &str, input: Value) -> Part {
        Part::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            signature: None,
        })
    }

    fn settings(max_rounds: usize) -> ChatSettings {
        ChatSettings {
            model: "m".into(),
            max_rounds,
            focus: None,
        }
    }

    fn ctx() -> Ctx {
        Ctx::new(Knowledge::default(), None, Vec::new())
    }

    #[tokio::test]
    async fn runs_the_tools_the_model_calls_until_it_answers() {
        let script = Script::new(vec![
            Ok(Message::assistant(vec![
                Part::Text {
                    text: "Checking.".into(),
                },
                call("1", "list_connections", json!({})),
                call("2", "no_such_tool", json!({})),
            ])),
            Ok(Message::assistant(vec![Part::Text {
                text: "None are enabled.".into(),
            }])),
        ]);
        let mut events = Vec::new();
        answer(
            &script,
            &Registry::standard(),
            &ctx(),
            &settings(4),
            &[Message::user("Which databases?")],
            &mut |e| events.push(e),
        )
        .await
        .unwrap();
        let ran: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::ToolRan {
                    name, ok, summary, ..
                } => Some((name.as_str(), *ok, summary.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            ran,
            [
                (
                    "list_connections",
                    true,
                    "No connection is enabled for the assistant."
                ),
                (
                    "no_such_tool",
                    false,
                    "not found: no tool named no_such_tool"
                ),
            ]
        );
        assert!(events.contains(&ChatEvent::ToolStarted("list_connections".into())));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ChatEvent::Message(_)))
                .count(),
            3
        );
        // The second request carries the answer with its calls and their results.
        let seen = script.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].tools.len(),
            Registry::standard().specs_for(&ctx()).len()
        );
        assert!(seen[0].system.contains("tangul"));
        let results = &seen[1].messages[2];
        assert_eq!(results.role, Role::User);
        assert!(matches!(
            &results.parts[1],
            Part::ToolResult(ToolResult { is_error: true, .. })
        ));
    }

    #[tokio::test]
    async fn stops_after_the_rounds_allowed() {
        let looping = || {
            Ok(Message::assistant(vec![call(
                "1",
                "list_connections",
                json!({}),
            )]))
        };
        let script = Script::new(vec![looping(), looping()]);
        let err = answer(
            &script,
            &Registry::standard(),
            &ctx(),
            &settings(2),
            &[Message::user("?")],
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(err, ChatError::TooManyRounds(2));
        assert!(err.to_string().contains("2 rounds"));
    }

    #[tokio::test]
    async fn provider_errors_end_the_answer() {
        let script = Script::new(vec![Err(Error::NoKey)]);
        let err = answer(
            &script,
            &Registry::standard(),
            &ctx(),
            &settings(0),
            &[Message::user("?")],
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(err, ChatError::Provider(Error::NoKey));
        assert_eq!(err.to_string(), "no API key is set");
    }

    #[test]
    fn turns_alternate_with_results_first() {
        let result = Part::ToolResult(ToolResult {
            call_id: "1".into(),
            name: "t".into(),
            content: "x".into(),
            is_error: false,
        });
        let messages = [
            Message::assistant(vec![call("1", "t", json!({}))]),
            Message {
                role: Role::User,
                parts: vec![result.clone()],
            },
            Message::assistant(Vec::new()),
            Message::user("and now?"),
        ];
        let out = normalized(&messages);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[1].parts[0], Part::ToolResult(_)));
        assert_eq!(
            out[1].parts[1],
            Part::Text {
                text: "and now?".into()
            }
        );
        // Out of order: the results still go first.
        let out = normalized(&[Message::user("hi"), messages[1].clone()]);
        assert_eq!(out[0].parts[0], result);
    }

    #[test]
    fn tools_that_read_rows_need_a_database_that_allows_it() {
        // No engine, nothing to read: only what tangul knows is offered.
        let defs = tool_defs(&Registry::standard(), &ctx());
        assert!(defs.iter().any(|d| d.name == "describe_table"));
        assert!(defs.iter().all(|d| d.name != "run_readonly"));
        assert!(defs.iter().all(|d| d.input_schema.is_object()));
    }
}
