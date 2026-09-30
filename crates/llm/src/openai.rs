//! OpenAI's Chat Completions API (`/chat/completions`, streamed as server-sent
//! events), which many services speak too: OpenRouter, Azure OpenAI, Groq,
//! DeepSeek, LM Studio, … by their base URL.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::anthropic::ids;
use crate::http::{Framing, HttpRequest};
use crate::types::{Delta, Error, Part, Reply, Request, Role, StopReason, ToolCall};
use crate::{Api, Decoder, parse};

pub(crate) struct OpenAi {
    pub base: String,
    pub key: String,
}

impl Api for OpenAi {
    fn chat_request(&self, req: &Request) -> HttpRequest {
        HttpRequest::post(format!("{}/chat/completions", self.base), body(req))
            .header("authorization", format!("Bearer {}", self.key))
    }

    fn framing(&self) -> Framing {
        Framing::Sse
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::<Stream>::default()
    }

    fn models_request(&self) -> HttpRequest {
        HttpRequest::get(format!("{}/models", self.base))
            .header("authorization", format!("Bearer {}", self.key))
    }

    fn models(&self, body: &Value) -> Vec<String> {
        ids(body)
    }
}

/// The messages in the Chat Completions shape; Ollama's is the same but for how
/// tool calls carry their arguments and that results name their tool
/// (`tool_name`, which OpenAI doesn't accept).
pub(crate) fn messages(
    req: &Request,
    arguments: impl Fn(&Value) -> Value,
    name_results: bool,
) -> Vec<Value> {
    let mut out = Vec::new();
    if !req.system.is_empty() {
        out.push(json!({"role": "system", "content": req.system}));
    }
    for m in &req.messages {
        let text: String = m.text();
        match m.role {
            Role::Assistant => {
                let calls: Vec<Value> = m
                    .tool_calls()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": {"name": c.name, "arguments": arguments(&c.input)},
                        })
                    })
                    .collect();
                let mut msg = json!({"role": "assistant", "content": text});
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                out.push(msg);
            }
            Role::User => {
                // Results go first: they must follow the assistant's calls.
                for p in &m.parts {
                    if let Part::ToolResult(r) = p {
                        let mut msg = json!({
                            "role": "tool",
                            "tool_call_id": r.call_id,
                            "content": r.content,
                        });
                        if name_results {
                            msg["tool_name"] = json!(r.name);
                        }
                        out.push(msg);
                    }
                }
                if !text.is_empty() {
                    out.push(json!({"role": "user", "content": text}));
                }
            }
        }
    }
    out
}

/// The tools in the Chat Completions shape (Ollama's too).
pub(crate) fn tools(req: &Request) -> Vec<Value> {
    req.tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {"name": t.name, "description": t.description, "parameters": t.input_schema},
            })
        })
        .collect()
}

fn body(req: &Request) -> Value {
    // Arguments go as a JSON string.
    let messages = messages(req, |input| json!(input.to_string()), false);
    let mut body = json!({"model": req.model, "messages": messages, "stream": true});
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(tools(req));
    }
    body
}

/// Tool calls by index while their arguments stream in as text.
#[derive(Default)]
struct Stream {
    calls: BTreeMap<u64, (ToolCall, String)>,
}

impl Decoder for Stream {
    fn feed(
        &mut self,
        payload: &str,
        reply: &mut Reply,
        on: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<(), Error> {
        if payload.trim() == "[DONE]" {
            return Ok(());
        }
        let chunk = parse(payload)?;
        if let Some(message) = chunk["error"]["message"].as_str() {
            return Err(Error::Api {
                status: 200,
                message: message.to_string(),
            });
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            reply.usage.input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
            reply.usage.output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0);
        }
        let Some(choice) = chunk["choices"].get(0) else {
            return Ok(());
        };
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            reply.push_text(text);
            on(Delta::Text(text.to_string()));
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = call["index"].as_u64().unwrap_or(0);
            let function = &call["function"];
            let (entry, arguments) = self.calls.entry(index).or_insert_with(|| {
                (
                    ToolCall {
                        id: String::new(),
                        name: String::new(),
                        input: json!({}),
                        signature: None,
                    },
                    String::new(),
                )
            });
            if let Some(id) = call["id"].as_str() {
                entry.id = id.to_string();
            }
            if let Some(name) = function["name"].as_str().filter(|n| !n.is_empty()) {
                entry.name = name.to_string();
                on(Delta::ToolCall(entry.name.clone()));
            }
            arguments.push_str(function["arguments"].as_str().unwrap_or_default());
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            reply.stop = match reason {
                "stop" => StopReason::EndTurn,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                other => StopReason::Other(other.to_string()),
            };
        }
        Ok(())
    }

    fn finish(&mut self, reply: &mut Reply) -> Result<(), Error> {
        for (index, (mut call, arguments)) in std::mem::take(&mut self.calls) {
            if !arguments.trim().is_empty() {
                call.input = parse(&arguments)?;
            }
            if call.id.is_empty() {
                call.id = format!("call_{index}");
            }
            reply.message.parts.push(Part::ToolCall(call));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolDef, ToolResult};

    fn api() -> OpenAi {
        OpenAi {
            base: "https://api.openai.com/v1".into(),
            key: "k".into(),
        }
    }

    fn request() -> Request {
        Request {
            model: "m".into(),
            system: "Be brief.".into(),
            messages: vec![
                Message::user("Tables?"),
                Message::assistant(vec![Part::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "describe_table".into(),
                    input: json!({"table": "users"}),
                    signature: None,
                })]),
                Message {
                    role: Role::User,
                    parts: vec![Part::ToolResult(ToolResult {
                        call_id: "c1".into(),
                        name: "describe_table".into(),
                        content: "id int".into(),
                        is_error: false,
                    })],
                },
            ],
            tools: vec![ToolDef {
                name: "describe_table".into(),
                description: "Describes".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: 100,
        }
    }

    #[test]
    fn the_request_follows_chat_completions() {
        let r = api().chat_request(&request());
        assert_eq!(r.url, "https://api.openai.com/v1/chat/completions");
        assert!(r.headers.contains(&("authorization", "Bearer k".into())));
        let body = r.body.unwrap();
        let m = &body["messages"];
        assert_eq!(m[0], json!({"role": "system", "content": "Be brief."}));
        assert_eq!(m[1], json!({"role": "user", "content": "Tables?"}));
        assert_eq!(
            m[2]["tool_calls"][0]["function"]["arguments"],
            "{\"table\":\"users\"}"
        );
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[3]["tool_call_id"], "c1");
        assert!(m[3].get("tool_name").is_none());
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
        let bare = Request {
            system: String::new(),
            tools: Vec::new(),
            ..request()
        };
        let body = api().chat_request(&bare).body.unwrap();
        assert!(body.get("tools").is_none());
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(
            api().models_request().url,
            "https://api.openai.com/v1/models"
        );
        assert_eq!(api().models(&json!({"data": [{"id": "gpt"}]})), ["gpt"]);
        assert_eq!(api().framing(), Framing::Sse);
    }

    fn decode(chunks: &[&str]) -> Result<(Reply, Vec<Delta>), Error> {
        let mut d = api().decoder();
        let mut reply = Reply::default();
        let mut deltas = Vec::new();
        for c in chunks {
            d.feed(c, &mut reply, &mut |x| deltas.push(x))?;
        }
        d.finish(&mut reply)?;
        Ok((reply, deltas))
    }

    #[test]
    fn a_stream_with_text_and_tool_calls() {
        let (reply, deltas) = decode(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":"Hi "}}]}"#,
            r#"{"choices":[{"delta":{"content":"there"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"describe_table","arguments":"{\"ta"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ble\":\"users\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"list_connections","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":7}}"#,
            "[DONE]",
        ])
        .unwrap();
        assert_eq!(reply.message.text(), "Hi there");
        let calls: Vec<_> = reply.message.tool_calls().collect();
        assert_eq!(calls[0].input, json!({"table": "users"}));
        assert_eq!(calls[0].id, "c1");
        assert_eq!(
            (calls[1].id.as_str(), calls[1].input.clone()),
            ("call_1", json!({}))
        );
        assert_eq!(reply.stop, StopReason::ToolUse);
        assert_eq!(
            (reply.usage.input_tokens, reply.usage.output_tokens),
            (5, 7)
        );
        assert_eq!(deltas.len(), 4);
    }

    #[test]
    fn finish_reasons_and_errors() {
        let stop = |r: &str| {
            let chunk = format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{r}"}}]}}"#);
            decode(&[&chunk]).unwrap().0.stop
        };
        assert_eq!(stop("stop"), StopReason::EndTurn);
        assert_eq!(stop("length"), StopReason::MaxTokens);
        assert_eq!(
            stop("content_filter"),
            StopReason::Other("content_filter".into())
        );
        assert_eq!(decode(&[r#"{"choices":[]}"#]).unwrap().0, Reply::default());
        assert_eq!(
            decode(&[r#"{"error":{"message":"rate limited"}}"#]).unwrap_err(),
            Error::Api {
                status: 200,
                message: "rate limited".into()
            }
        );
        let broken = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"t","arguments":"{"}}]}}]}"#;
        assert!(matches!(decode(&[broken]), Err(Error::Invalid(_))));
    }
}
