//! Each provider end to end over HTTP, against a local server that answers with a
//! recorded stream.

use serde_json::{Value, json};
use tangul_llm::{
    Config, Delta, Error, Message, ProviderKind, Request, StopReason, ToolDef, provider,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// What the server was sent.
struct Seen {
    head: String,
    body: Value,
}

/// Serves one request: answers `status` with `body`, in chunks of 7 bytes so that
/// lines and events arrive split.
async fn serve(status: u16, body: &str) -> (String, JoinHandle<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let body = body.to_string();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        let (head, rest) = loop {
            let n = socket.read(&mut buf).await.unwrap();
            request.extend_from_slice(&buf[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&request[..end]).to_string();
                break (head, request[end + 4..].to_vec());
            }
        };
        let length = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let mut sent = rest;
        while sent.len() < length {
            let n = socket.read(&mut buf).await.unwrap();
            sent.extend_from_slice(&buf[..n]);
        }
        let reply = format!("HTTP/1.1 {status} X\r\nconnection: close\r\n\r\n");
        socket.write_all(reply.as_bytes()).await.unwrap();
        for chunk in body.as_bytes().chunks(7) {
            socket.write_all(chunk).await.unwrap();
            socket.flush().await.unwrap();
        }
        Seen {
            head,
            body: serde_json::from_slice(&sent).unwrap_or(Value::Null),
        }
    });
    (url, task)
}

fn request(model: &str) -> Request {
    Request {
        model: model.into(),
        system: "Be brief.".into(),
        messages: vec![Message::user("Which tables?")],
        tools: vec![ToolDef {
            name: "list_connections".into(),
            description: "Lists the connections".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }],
        max_tokens: 256,
    }
}

fn config(kind: ProviderKind, url: &str) -> Config {
    Config {
        kind,
        base_url: Some(format!("{url}/")),
        key: Some("secret".into()),
    }
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect::<String>()
}

#[tokio::test]
async fn anthropic_streams_text_and_a_tool_call() {
    let body = sse(&[
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 3}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Looking…"}}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "t1", "name": "list_connections"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}}),
    ]);
    let (url, seen) = serve(200, &body).await;
    let p = provider(config(ProviderKind::Anthropic, &url)).unwrap();
    let mut deltas = Vec::new();
    let reply = p
        .chat(&request("claude"), &mut |d| deltas.push(d))
        .await
        .unwrap();
    assert_eq!(reply.message.text(), "Looking…");
    assert_eq!(
        reply.message.tool_calls().next().unwrap().name,
        "list_connections"
    );
    assert_eq!(reply.stop, StopReason::ToolUse);
    assert_eq!(deltas[0], Delta::Text("Looking…".into()));
    let seen = seen.await.unwrap();
    assert!(seen.head.starts_with("POST /v1/messages "));
    assert!(seen.head.to_lowercase().contains("x-api-key: secret"));
    assert_eq!(seen.body["model"], "claude");
}

#[tokio::test]
async fn openai_streams_until_done() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                data: [DONE]\n\n";
    let (url, seen) = serve(200, body).await;
    let p = provider(config(ProviderKind::Openai, &url)).unwrap();
    let reply = p.chat(&request("gpt"), &mut |_| {}).await.unwrap();
    assert_eq!(reply.message.text(), "Hi");
    assert_eq!(reply.stop, StopReason::EndTurn);
    let seen = seen.await.unwrap();
    assert!(seen.head.starts_with("POST /chat/completions "));
    assert!(
        seen.head
            .to_lowercase()
            .contains("authorization: bearer secret")
    );
}

#[tokio::test]
async fn gemini_streams_a_last_event_without_a_blank_line() {
    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]},\"finishReason\":\"STOP\"}]}";
    let (url, seen) = serve(200, body).await;
    let p = provider(config(ProviderKind::Gemini, &url)).unwrap();
    let reply = p.chat(&request("gemini-x"), &mut |_| {}).await.unwrap();
    assert_eq!(reply.message.text(), "Hello");
    let seen = seen.await.unwrap();
    assert!(
        seen.head
            .starts_with("POST /v1beta/models/gemini-x:streamGenerateContent?alt=sse ")
    );
    assert!(seen.head.to_lowercase().contains("x-goog-api-key: secret"));
}

#[tokio::test]
async fn ollama_streams_lines_without_a_key() {
    let body = "{\"message\":{\"content\":\"Hi\"},\"done\":false}\n\
                {\"message\":{\"content\":\" there\"},\"done\":false}\n\
                {\"message\":{\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\"}";
    let (url, seen) = serve(200, body).await;
    let p = provider(Config {
        key: None,
        ..config(ProviderKind::Ollama, &url)
    })
    .unwrap();
    let reply = p.chat(&request("llama"), &mut |_| {}).await.unwrap();
    assert_eq!(reply.message.text(), "Hi there");
    let seen = seen.await.unwrap();
    assert!(seen.head.starts_with("POST /api/chat "));
    assert_eq!(seen.body["messages"][0]["role"], "system");
}

#[tokio::test]
async fn models_are_listed_sorted() {
    let (url, seen) = serve(200, r#"{"data":[{"id":"b"},{"id":"a"},{"id":"a"}]}"#).await;
    let p = provider(config(ProviderKind::Openai, &url)).unwrap();
    assert_eq!(p.models().await.unwrap(), ["a", "b"]);
    assert!(seen.await.unwrap().head.starts_with("GET /models "));
}

#[tokio::test]
async fn errors_say_what_the_api_said() {
    let (url, _seen) = serve(401, r#"{"error":{"message":"invalid x-api-key"}}"#).await;
    let p = provider(config(ProviderKind::Anthropic, &url)).unwrap();
    assert_eq!(
        p.chat(&request("claude"), &mut |_| {}).await.unwrap_err(),
        Error::Api {
            status: 401,
            message: "invalid x-api-key".into()
        }
    );
    let (url, _seen) = serve(500, "not json").await;
    let p = provider(config(ProviderKind::Ollama, &url)).unwrap();
    assert!(matches!(
        p.models().await,
        Err(Error::Api { status: 500, .. })
    ));
    // A 200 that isn't JSON.
    let (url, _seen) = serve(200, "oops").await;
    let p = provider(config(ProviderKind::Ollama, &url)).unwrap();
    assert!(matches!(p.models().await, Err(Error::Invalid(_))));
    // No model chosen: nothing is sent.
    let p = provider(config(ProviderKind::Openai, "http://127.0.0.1:9")).unwrap();
    assert_eq!(
        p.chat(&request(" "), &mut |_| {}).await.unwrap_err(),
        Error::NoModel
    );
    // Nobody listens.
    assert!(matches!(
        p.chat(&request("m"), &mut |_| {}).await,
        Err(Error::Network(_))
    ));
}
