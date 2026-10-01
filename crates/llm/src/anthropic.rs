//! Anthropic's Messages API (`/v1/messages`, streamed as server-sent events).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::http::{Framing, HttpRequest};
use crate::types::{Delta, Error, Part, Reply, Request, Role, StopReason, ToolCall};
use crate::{Api, Decoder, parse};

const VERSION: &str = "2023-06-01";

pub(crate) struct Anthropic {
    pub base: String,
    pub key: String,
}

impl Api for Anthropic {
    fn chat_request(&self, req: &Request) -> HttpRequest {
        HttpRequest::post(format!("{}/v1/messages", self.base), body(req))
            .header("x-api-key", &self.key)
            .header("anthropic-version", VERSION)
    }

    fn framing(&self) -> Framing {
        Framing::Sse
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::<Stream>::default()
    }

    fn models_request(&self) -> HttpRequest {
        HttpRequest::get(format!("{}/v1/models?limit=1000", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", VERSION)
    }

    fn models(&self, body: &Value) -> Vec<String> {
        ids(body)
    }
}

/// `data[].id`, as OpenAI lists them too.
pub(crate) fn ids(body: &Value) -> Vec<String> {
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect()
}

fn body(req: &Request) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let content: Vec<Value> = m
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } if text.is_empty() => None,
                    Part::Text { text } => Some(json!({"type": "text", "text": text})),
                    Part::ToolCall(c) => Some(json!({
                        "type": "tool_use", "id": c.id, "name": c.name, "input": c.input,
                    })),
                    Part::ToolResult(r) => Some(json!({
                        "type": "tool_result",
                        "tool_use_id": r.call_id,
                        "content": r.content,
                        "is_error": r.is_error,
                    })),
                })
                .collect();
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            json!({"role": role, "content": content})
        })
        .collect();
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "stream": true,
    });
    if !req.system.is_empty() {
        body["system"] = json!(req.system);
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
            .collect();
    }
    body
}

/// Content blocks by index while they stream: text goes out as it comes, a tool
/// call's input arrives as pieces of JSON.
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
        let event = parse(payload)?;
        let index = event["index"].as_u64().unwrap_or(0);
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let usage = &event["message"]["usage"];
                reply.usage.input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let block = &event["content_block"];
                if block["type"] == "tool_use" {
                    let name = block["name"].as_str().unwrap_or_default().to_string();
                    on(Delta::ToolCall(name.clone()));
                    let call = ToolCall {
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        name,
                        input: json!({}),
                        signature: None,
                    };
                    self.calls.insert(index, (call, String::new()));
                }
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        reply.push_text(text);
                        on(Delta::Text(text.to_string()));
                    }
                    "input_json_delta" => {
                        if let Some((_, json)) = self.calls.get_mut(&index) {
                            json.push_str(delta["partial_json"].as_str().unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some((mut call, json)) = self.calls.remove(&index) {
                    if !json.trim().is_empty() {
                        call.input = parse(&json)?;
                    }
                    reply.message.parts.push(Part::ToolCall(call));
                }
            }
            "message_delta" => {
                reply.stop = match event["delta"]["stop_reason"].as_str() {
                    Some("tool_use") => StopReason::ToolUse,
                    Some("max_tokens") => StopReason::MaxTokens,
                    Some("end_turn") | Some("stop_sequence") | None => StopReason::EndTurn,
                    Some(other) => StopReason::Other(other.to_string()),
                };
                if let Some(n) = event["usage"]["output_tokens"].as_u64() {
                    reply.usage.output_tokens = n;
                }
            }
            "error" => {
                return Err(Error::Api {
                    status: 200,
                    message: event["error"]["message"]
                        .as_str()
                        .unwrap_or("error in the stream")
                        .to_string(),
                });
            }
            // ping, message_stop, thinking blocks
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolDef, ToolResult};

    fn api() -> Anthropic {
        Anthropic {
            base: "https://api.anthropic.com".into(),
            key: "k".into(),
        }
    }

    fn request() -> Request {
        Request {
            model: "claude".into(),
            system: "Be brief.".into(),
            messages: vec![
                Message::user("Tables?"),
                Message::assistant(vec![
                    Part::Text {
                        text: String::new(),
                    },
                    Part::ToolCall(ToolCall {
                        id: "tu_1".into(),
                        name: "list_connections".into(),
                        input: json!({}),
                        signature: None,
                    }),
                ]),
                Message {
                    role: Role::User,
                    parts: vec![Part::ToolResult(ToolResult {
                        call_id: "tu_1".into(),
                        name: "list_connections".into(),
                        content: "app".into(),
                        is_error: false,
                    })],
                },
            ],
            tools: vec![ToolDef {
                name: "list_connections".into(),
                description: "Lists".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: 100,
        }
    }

    #[test]
    fn the_request_follows_the_messages_api() {
        let r = api().chat_request(&request());
        assert_eq!(r.url, "https://api.anthropic.com/v1/messages");
        assert!(r.headers.contains(&("x-api-key", "k".into())));
        let body = r.body.unwrap();
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["content"][0]["text"], "Tables?");
        // The empty text block is left out.
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "tu_1");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        let bare = Request {
            system: String::new(),
            tools: Vec::new(),
            ..request()
        };
        let body = api().chat_request(&bare).body.unwrap();
        assert!(body.get("system").is_none() && body.get("tools").is_none());
        assert!(
            api()
                .models_request()
                .url
                .ends_with("/v1/models?limit=1000")
        );
        assert_eq!(
            api().models(&json!({"data": [{"id": "a"}, {"x": 1}]})),
            ["a"]
        );
        assert_eq!(api().framing(), Framing::Sse);
    }

    fn decode(events: &[Value]) -> (Result<Reply, Error>, Vec<Delta>) {
        let mut d = api().decoder();
        let mut reply = Reply::default();
        let mut deltas = Vec::new();
        for e in events {
            if let Err(e) = d.feed(&e.to_string(), &mut reply, &mut |x| deltas.push(x)) {
                return (Err(e), deltas);
            }
        }
        d.finish(&mut reply).unwrap();
        (Ok(reply), deltas)
    }

    #[test]
    fn a_stream_with_text_and_a_tool_call() {
        let (reply, deltas) = decode(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 12}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Let me "}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "look."}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "tu_1", "name": "describe_table", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"table\":"}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "\"users\"}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}}),
            json!({"type": "ping"}),
            json!({"type": "message_stop"}),
        ]);
        let reply = reply.unwrap();
        assert_eq!(reply.message.text(), "Let me look.");
        let call = reply.message.tool_calls().next().unwrap();
        assert_eq!(
            (call.id.as_str(), call.name.as_str()),
            ("tu_1", "describe_table")
        );
        assert_eq!(call.input, json!({"table": "users"}));
        assert_eq!(reply.stop, StopReason::ToolUse);
        assert_eq!(
            (reply.usage.input_tokens, reply.usage.output_tokens),
            (12, 30)
        );
        assert_eq!(
            deltas,
            [
                Delta::Text("Let me ".into()),
                Delta::Text("look.".into()),
                Delta::ToolCall("describe_table".into())
            ]
        );
    }

    #[test]
    fn a_tool_call_without_input_and_stop_reasons() {
        let stop = |reason: Value| {
            decode(&[
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "a", "name": "t"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "message_delta", "delta": {"stop_reason": reason}}),
            ])
            .0
            .unwrap()
        };
        let reply = stop(json!("max_tokens"));
        assert_eq!(reply.message.tool_calls().next().unwrap().input, json!({}));
        assert_eq!(reply.stop, StopReason::MaxTokens);
        assert_eq!(stop(json!("end_turn")).stop, StopReason::EndTurn);
        assert_eq!(
            stop(json!("refusal")).stop,
            StopReason::Other("refusal".into())
        );
    }

    #[test]
    fn an_error_event_fails_the_answer() {
        let (reply, _) = decode(&[
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ]);
        assert_eq!(
            reply.unwrap_err(),
            Error::Api {
                status: 200,
                message: "Overloaded".into()
            }
        );
        let (reply, _) = decode(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "a", "name": "t"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{"}}),
            json!({"type": "content_block_stop", "index": 0}),
        ]);
        assert!(matches!(reply, Err(Error::Invalid(_))));
    }
}
