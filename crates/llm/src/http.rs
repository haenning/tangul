//! HTTP for every adapter: sending a request, reading a streamed answer as
//! server-sent events or JSON lines, and turning error answers into [`Error`]s.

use std::time::Duration;

use futures::StreamExt as _;
use serde_json::Value;

use crate::types::Error;

/// A stream that stays silent this long is given up.
const READ_TIMEOUT: Duration = Duration::from_secs(180);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Most characters of an error body kept when it has no message of its own.
const ERROR_BODY_CHARS: usize = 300;

pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// How a streamed answer is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    /// Server-sent events: `data:` lines, events split by blank lines.
    Sse,
    /// One JSON document per line (Ollama).
    Lines,
}

/// A request as an adapter builds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HttpRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    /// `None`: a GET.
    pub body: Option<Value>,
}

impl HttpRequest {
    pub fn get(url: String) -> Self {
        Self {
            url,
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn post(url: String, body: Value) -> Self {
        Self {
            url,
            headers: Vec::new(),
            body: Some(body),
        }
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

async fn send(client: &reqwest::Client, req: HttpRequest) -> Result<reqwest::Response, Error> {
    let mut builder = match &req.body {
        Some(body) => client
            .post(&req.url)
            .header("content-type", "application/json")
            .body(body.to_string()),
        None => client.get(&req.url),
    };
    for (name, value) in req.headers {
        builder = builder.header(name, value);
    }
    let response = builder.send().await.map_err(network)?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    Err(Error::Api {
        status: status.as_u16(),
        message: error_message(&body),
    })
}

/// The whole answer as JSON.
pub(crate) async fn json(client: &reqwest::Client, req: HttpRequest) -> Result<Value, Error> {
    let text = send(client, req).await?.text().await.map_err(network)?;
    serde_json::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))
}

/// Hands each event (SSE `data`) or line of a streamed answer to `each`.
pub(crate) async fn stream(
    client: &reqwest::Client,
    req: HttpRequest,
    framing: Framing,
    mut each: impl FnMut(&str) -> Result<(), Error>,
) -> Result<(), Error> {
    let response = send(client, req).await?;
    let mut chunks = response.bytes_stream();
    let mut lines = Lines::default();
    let mut events = Sse::default();
    let mut handle = |line: &str| -> Result<(), Error> {
        match framing {
            Framing::Sse => match events.line(line) {
                Some(data) => each(&data),
                None => Ok(()),
            },
            Framing::Lines if line.trim().is_empty() => Ok(()),
            Framing::Lines => each(line),
        }
    };
    while let Some(chunk) = chunks.next().await {
        for line in lines.push(&chunk.map_err(network)?) {
            handle(&line)?;
        }
    }
    if let Some(line) = lines.rest() {
        handle(&line)?;
    }
    // A last event without the blank line after it.
    handle("")
}

fn network(e: reqwest::Error) -> Error {
    // The URL's own text is left out: it can carry a key (never in ours, but a
    // base URL typed by the user might).
    Error::Network(e.without_url().to_string())
}

/// What an API says went wrong: `error.message`, `error` or `message` of a JSON
/// body, else the start of the body.
pub(crate) fn error_message(body: &str) -> String {
    let json: Option<Value> = serde_json::from_str(body).ok();
    let from_json = json.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error").filter(|e| e.is_string()))
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    from_json.unwrap_or_else(|| {
        let body = body.trim();
        if body.is_empty() {
            "no details".into()
        } else {
            body.chars().take(ERROR_BODY_CHARS).collect()
        }
    })
}

/// Splits bytes into lines across chunk boundaries.
#[derive(Default)]
pub(crate) struct Lines {
    pending: Vec<u8>,
}

impl Lines {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            lines.push(text.trim_end_matches('\r').to_string());
        }
        lines
    }

    /// What is left after the last newline.
    pub fn rest(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.pending);
        (!rest.is_empty()).then(|| String::from_utf8_lossy(&rest).into_owned())
    }
}

/// Server-sent events: collects `data:` lines until a blank line ends the event.
#[derive(Default)]
pub(crate) struct Sse {
    data: Option<String>,
}

impl Sse {
    /// The event's data once `line` ends one.
    pub fn line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            return self.data.take();
        }
        if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            }
        }
        // `event:`, `id:`, `retry:` and `:` comments: the data says it all.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_across_chunks() {
        let mut l = Lines::default();
        assert_eq!(l.push(b"ab"), Vec::<String>::new());
        assert_eq!(l.push(b"c\r\nde\n\nf"), ["abc", "de", ""]);
        assert_eq!(l.rest().as_deref(), Some("f"));
        assert_eq!(l.rest(), None);
    }

    #[test]
    fn sse_joins_data_lines_until_a_blank_line() {
        let mut s = Sse::default();
        assert_eq!(s.line("event: message"), None);
        assert_eq!(s.line(": comment"), None);
        assert_eq!(s.line("data: {\"a\":"), None);
        assert_eq!(s.line("data:1}"), None);
        assert_eq!(s.line("").as_deref(), Some("{\"a\":\n1}"));
        assert_eq!(s.line(""), None);
    }

    #[test]
    fn error_messages_come_from_the_body() {
        assert_eq!(
            error_message(r#"{"type":"error","error":{"type":"x","message":"bad key"}}"#),
            "bad key"
        );
        assert_eq!(
            error_message(r#"{"error":"model not found"}"#),
            "model not found"
        );
        assert_eq!(error_message(r#"{"message":"slow down"}"#), "slow down");
        assert_eq!(error_message("  Bad Gateway \n"), "Bad Gateway");
        assert_eq!(error_message(""), "no details");
        assert_eq!(error_message(&"x".repeat(1000)).len(), ERROR_BODY_CHARS);
    }

    #[test]
    fn requests_collect_headers() {
        let r = HttpRequest::get("http://x".into()).header("a", "b");
        assert_eq!(r.headers, [("a", "b".to_string())]);
        assert!(r.body.is_none());
        assert!(
            HttpRequest::post("http://x".into(), Value::Null)
                .body
                .is_some()
        );
    }
}
