//! A local Ollama server (`/api/chat`, streamed as one JSON object per line). No
//! key; tool calls come whole, their arguments as objects.

use serde_json::{Value, json};

use crate::http::{Framing, HttpRequest};
use crate::openai::{messages, tools};
use crate::types::{Delta, Error, Part, Reply, Request, StopReason, ToolCall};
use crate::{Api, Decoder, parse};

pub(crate) struct Ollama {
    pub base: String,
}

impl Api for Ollama {
    fn chat_request(&self, req: &Request) -> HttpRequest {
        let mut body = json!({
            "model": req.model,
            "messages": messages(req, Value::clone, true),
            "stream": true,
            "options": {"num_predict": req.max_tokens},
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(tools(req));
        }
        HttpRequest::post(format!("{}/api/chat", self.base), body)
    }

    fn framing(&self) -> Framing {
        Framing::Lines
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::<Stream>::default()
    }

    fn models_request(&self) -> HttpRequest {
        HttpRequest::get(format!("{}/api/tags", self.base))
    }

    fn models(&self, body: &Value) -> Vec<String> {
        body["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["name"].as_str().map(str::to_string))
            .collect()
    }
}

#[derive(Default)]
struct Stream {
    calls: usize,
}

impl Decoder for Stream {
    fn feed(
        &mut self,
        payload: &str,
        reply: &mut Reply,
        on: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<(), Error> {
        let chunk = parse(payload)?;
        if let Some(message) = chunk["error"].as_str() {
            return Err(Error::Api {
                status: 200,
                message: message.to_string(),
            });
        }
        let message = &chunk["message"];
        if let Some(text) = message["content"].as_str().filter(|t| !t.is_empty()) {
            reply.push_text(text);
            on(Delta::Text(text.to_string()));
        }
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            self.calls += 1;
            let function = &call["function"];
            let name = function["name"].as_str().unwrap_or_default().to_string();
            on(Delta::ToolCall(name.clone()));
            reply.message.parts.push(Part::ToolCall(ToolCall {
                id: call["id"]
                    .as_str()
                    .map_or_else(|| format!("call_{}", self.calls), str::to_string),
                name,
                input: function
                    .get("arguments")
                    .cloned()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({})),
                signature: None,
            }));
        }
        if chunk["done"] == true {
            reply.usage.input_tokens = chunk["prompt_eval_count"].as_u64().unwrap_or(0);
            reply.usage.output_tokens = chunk["eval_count"].as_u64().unwrap_or(0);
            reply.stop = match chunk["done_reason"].as_str() {
                _ if reply.message.tool_calls().next().is_some() => StopReason::ToolUse,
                Some("length") => StopReason::MaxTokens,
                Some("stop") | None => StopReason::EndTurn,
                Some(other) => StopReason::Other(other.to_string()),
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, Role, ToolDef, ToolResult};

    fn api() -> Ollama {
        Ollama {
            base: "http://localhost:11434".into(),
        }
    }

    #[test]
    fn the_request_follows_api_chat() {
        let req = Request {
            model: "llama".into(),
            system: String::new(),
            messages: vec![
                Message::assistant(vec![Part::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "t".into(),
                    input: json!({"a": 1}),
                    signature: None,
                })]),
                Message {
                    role: Role::User,
                    parts: vec![Part::ToolResult(ToolResult {
                        call_id: "c1".into(),
                        name: "t".into(),
                        content: "ok".into(),
                        is_error: false,
                    })],
                },
            ],
            tools: vec![ToolDef {
                name: "t".into(),
                description: "d".into(),
                input_schema: json!({}),
            }],
            max_tokens: 50,
        };
        let r = api().chat_request(&req);
        assert_eq!(r.url, "http://localhost:11434/api/chat");
        assert!(r.headers.is_empty());
        let body = r.body.unwrap();
        // Arguments as an object, results named by their tool.
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["arguments"]["a"],
            1
        );
        assert_eq!(body["messages"][1]["tool_name"], "t");
        assert_eq!(body["options"]["num_predict"], 50);
        assert_eq!(body["tools"][0]["function"]["name"], "t");
        let body = api()
            .chat_request(&Request {
                tools: Vec::new(),
                ..req
            })
            .body
            .unwrap();
        assert!(body.get("tools").is_none());
        assert_eq!(api().framing(), Framing::Lines);
        assert_eq!(
            api().models_request().url,
            "http://localhost:11434/api/tags"
        );
        assert_eq!(
            api().models(&json!({"models": [{"name": "llama3.2:latest"}]})),
            ["llama3.2:latest"]
        );
    }

    fn decode(lines: &[Value]) -> Result<Reply, Error> {
        let mut d = api().decoder();
        let mut reply = Reply::default();
        for l in lines {
            d.feed(&l.to_string(), &mut reply, &mut |_| {})?;
        }
        Ok(reply)
    }

    #[test]
    fn a_stream_with_text_and_a_tool_call() {
        let reply = decode(&[
            json!({"message": {"role": "assistant", "content": "Hi"}, "done": false}),
            json!({"message": {"role": "assistant", "content": "", "tool_calls": [
                {"function": {"name": "describe_table", "arguments": {"table": "users"}}},
                {"function": {"name": "list_connections", "arguments": "oops"}}
            ]}, "done": false}),
            json!({"message": {"content": ""}, "done": true, "done_reason": "stop", "prompt_eval_count": 9, "eval_count": 2}),
        ])
        .unwrap();
        assert_eq!(reply.message.text(), "Hi");
        let calls: Vec<_> = reply.message.tool_calls().collect();
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].input, json!({"table": "users"}));
        assert_eq!(calls[1].input, json!({}));
        assert_eq!(reply.stop, StopReason::ToolUse);
        assert_eq!(
            (reply.usage.input_tokens, reply.usage.output_tokens),
            (9, 2)
        );
    }

    #[test]
    fn done_reasons_and_errors() {
        let stop = |r: Value| {
            decode(&[json!({"message": {}, "done": true, "done_reason": r})])
                .unwrap()
                .stop
        };
        assert_eq!(stop(json!("stop")), StopReason::EndTurn);
        assert_eq!(stop(json!("length")), StopReason::MaxTokens);
        assert_eq!(stop(json!("unload")), StopReason::Other("unload".into()));
        assert_eq!(
            decode(&[json!({"error": "model \"x\" not found"})]).unwrap_err(),
            Error::Api {
                status: 200,
                message: "model \"x\" not found".into()
            }
        );
    }
}
