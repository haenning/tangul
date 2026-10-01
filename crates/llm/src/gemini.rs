//! Google's Gemini API (`models/{model}:streamGenerateContent`, streamed as
//! server-sent events). Tool calls come whole; Gemini's thought signatures are
//! kept with them and sent back, as the API requires.

use serde_json::{Value, json};

use crate::http::{Framing, HttpRequest};
use crate::types::{Delta, Error, Part, Reply, Request, Role, StopReason, ToolCall};
use crate::{Api, Decoder, parse, result_object};

pub(crate) struct Gemini {
    pub base: String,
    pub key: String,
}

impl Api for Gemini {
    fn chat_request(&self, req: &Request) -> HttpRequest {
        HttpRequest::post(
            format!(
                "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
                self.base, req.model
            ),
            body(req),
        )
        .header("x-goog-api-key", &self.key)
    }

    fn framing(&self) -> Framing {
        Framing::Sse
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::<Stream>::default()
    }

    fn models_request(&self) -> HttpRequest {
        HttpRequest::get(format!("{}/v1beta/models?pageSize=1000", self.base))
            .header("x-goog-api-key", &self.key)
    }

    /// The models that chat (`generateContent`), without the `models/` prefix.
    fn models(&self, body: &Value) -> Vec<String> {
        body["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["supportedGenerationMethods"]
                    .as_array()
                    .is_some_and(|ms| ms.iter().any(|x| x == "generateContent"))
            })
            .filter_map(|m| m["name"].as_str())
            .map(|n| n.trim_start_matches("models/").to_string())
            .collect()
    }
}

fn body(req: &Request) -> Value {
    let contents: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let parts: Vec<Value> = m
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } if text.is_empty() => None,
                    Part::Text { text } => Some(json!({"text": text})),
                    Part::ToolCall(c) => {
                        let mut part = json!({
                            "functionCall": {"id": c.id, "name": c.name, "args": c.input},
                        });
                        if let Some(sig) = &c.signature {
                            part["thoughtSignature"] = json!(sig);
                        }
                        Some(part)
                    }
                    Part::ToolResult(r) => Some(json!({
                        "functionResponse": {
                            "id": r.call_id, "name": r.name, "response": result_object(r),
                        },
                    })),
                })
                .collect();
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "model",
            };
            json!({"role": role, "parts": parts})
        })
        .collect();
    let mut body = json!({
        "contents": contents,
        "generationConfig": {"maxOutputTokens": req.max_tokens},
    });
    if !req.system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": req.system}]});
    }
    if !req.tools.is_empty() {
        // `parametersJsonSchema` takes JSON Schema as it is (`parameters` only a
        // subset of it).
        let declarations: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "parametersJsonSchema": t.input_schema}))
            .collect();
        body["tools"] = json!([{"functionDeclarations": declarations}]);
    }
    body
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
        if let Some(message) = chunk["error"]["message"].as_str() {
            return Err(Error::Api {
                status: chunk["error"]["code"].as_u64().unwrap_or(200) as u16,
                message: message.to_string(),
            });
        }
        if let Some(usage) = chunk.get("usageMetadata") {
            reply.usage.input_tokens = usage["promptTokenCount"].as_u64().unwrap_or(0);
            reply.usage.output_tokens = usage["candidatesTokenCount"].as_u64().unwrap_or(0);
        }
        let Some(candidate) = chunk["candidates"].get(0) else {
            return Ok(());
        };
        for part in candidate["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let signature = part["thoughtSignature"].as_str().map(str::to_string);
            if let Some(call) = part.get("functionCall") {
                self.calls += 1;
                let name = call["name"].as_str().unwrap_or_default().to_string();
                on(Delta::ToolCall(name.clone()));
                reply.message.parts.push(Part::ToolCall(ToolCall {
                    id: call["id"]
                        .as_str()
                        .map_or_else(|| format!("call_{}", self.calls), str::to_string),
                    name,
                    input: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    signature,
                }));
            } else if part["thought"] == true {
                // Thought summaries aren't part of the answer.
            } else if let Some(text) = part["text"].as_str() {
                reply.push_text(text);
                on(Delta::Text(text.to_string()));
            }
        }
        if let Some(reason) = candidate["finishReason"].as_str() {
            reply.stop = match reason {
                "STOP" if reply.message.tool_calls().next().is_some() => StopReason::ToolUse,
                "STOP" => StopReason::EndTurn,
                "MAX_TOKENS" => StopReason::MaxTokens,
                other => StopReason::Other(other.to_string()),
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolDef, ToolResult};

    fn api() -> Gemini {
        Gemini {
            base: "https://generativelanguage.googleapis.com".into(),
            key: "k".into(),
        }
    }

    fn request() -> Request {
        Request {
            model: "gemini-x".into(),
            system: "Be brief.".into(),
            messages: vec![
                Message::user("Tables?"),
                Message::assistant(vec![Part::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "describe_table".into(),
                    input: json!({"table": "users"}),
                    signature: Some("sig".into()),
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
    fn the_request_follows_generate_content() {
        let r = api().chat_request(&request());
        assert_eq!(
            r.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-x:streamGenerateContent?alt=sse"
        );
        assert!(r.headers.contains(&("x-goog-api-key", "k".into())));
        let body = r.body.unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be brief.");
        assert_eq!(body["contents"][1]["role"], "model");
        let call = &body["contents"][1]["parts"][0];
        assert_eq!(call["functionCall"]["args"]["table"], "users");
        assert_eq!(call["thoughtSignature"], "sig");
        let result = &body["contents"][2]["parts"][0]["functionResponse"];
        assert_eq!(result["response"], json!({"content": "id int"}));
        assert_eq!(result["name"], "describe_table");
        let decl = &body["tools"][0]["functionDeclarations"][0];
        assert_eq!(decl["parametersJsonSchema"]["type"], "object");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 100);
        let bare = Request {
            system: String::new(),
            tools: Vec::new(),
            messages: vec![Message::assistant(vec![Part::Text {
                text: String::new(),
            }])],
            ..request()
        };
        let body = api().chat_request(&bare).body.unwrap();
        assert!(body.get("systemInstruction").is_none() && body.get("tools").is_none());
        assert_eq!(body["contents"][0]["parts"], json!([]));
        assert_eq!(api().framing(), Framing::Sse);
    }

    #[test]
    fn models_that_chat() {
        assert!(api().models_request().url.contains("/v1beta/models"));
        let body = json!({"models": [
            {"name": "models/gemini-a", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/embedding", "supportedGenerationMethods": ["embedContent"]},
        ]});
        assert_eq!(api().models(&body), ["gemini-a"]);
    }

    fn decode(chunks: &[Value]) -> Result<(Reply, Vec<Delta>), Error> {
        let mut d = api().decoder();
        let mut reply = Reply::default();
        let mut deltas = Vec::new();
        for c in chunks {
            d.feed(&c.to_string(), &mut reply, &mut |x| deltas.push(x))?;
        }
        d.finish(&mut reply)?;
        Ok((reply, deltas))
    }

    #[test]
    fn a_stream_with_text_and_a_tool_call() {
        let (reply, deltas) = decode(&[
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "Let me "}, {"text": "hidden", "thought": true}]}}]}),
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "look."}]}}]}),
            json!({"candidates": [{"content": {"parts": [
                {"functionCall": {"name": "describe_table", "args": {"table": "users"}}, "thoughtSignature": "s1"},
                {"functionCall": {"id": "x", "name": "list_connections"}}
            ]}, "finishReason": "STOP"}],
             "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 4}}),
        ])
        .unwrap();
        assert_eq!(reply.message.text(), "Let me look.");
        let calls: Vec<_> = reply.message.tool_calls().collect();
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].signature.as_deref(), Some("s1"));
        assert_eq!(calls[0].input, json!({"table": "users"}));
        assert_eq!(
            (calls[1].id.as_str(), calls[1].input.clone()),
            ("x", json!({}))
        );
        assert_eq!(reply.stop, StopReason::ToolUse);
        assert_eq!(
            (reply.usage.input_tokens, reply.usage.output_tokens),
            (3, 4)
        );
        assert_eq!(deltas.len(), 4);
    }

    #[test]
    fn finish_reasons_and_errors() {
        let stop = |r: &str| {
            decode(&[json!({"candidates": [{"content": {"parts": []}, "finishReason": r}]})])
                .unwrap()
                .0
                .stop
        };
        assert_eq!(stop("STOP"), StopReason::EndTurn);
        assert_eq!(stop("MAX_TOKENS"), StopReason::MaxTokens);
        assert_eq!(stop("SAFETY"), StopReason::Other("SAFETY".into()));
        assert_eq!(decode(&[json!({})]).unwrap().0, Reply::default());
        assert_eq!(
            decode(&[json!({"error": {"code": 429, "message": "quota"}})]).unwrap_err(),
            Error::Api {
                status: 429,
                message: "quota".into()
            }
        );
    }
}
