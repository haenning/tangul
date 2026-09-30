//! Talking to language models: one [`Provider`] interface (a streamed answer with
//! tool calls, the models on offer) with an adapter per API. No UI, no Keychain,
//! nothing about databases or tangul's tools: the caller passes the key, the
//! messages and the tools ([`ToolDef`]); `tangul_ai` runs the conversation.
//!
//! Adding a provider: a [`ProviderKind`] variant, an adapter module implementing
//! [`Api`] (the request it sends, how its stream reads), and its arm in
//! [`provider`], with tests. The compiler points to the rest.

mod anthropic;
mod gemini;
mod http;
mod ollama;
mod openai;
mod types;

use std::sync::Arc;

use async_trait::async_trait;
pub use tangul_core::llm::ProviderKind;
pub use types::{
    Delta, Error, Message, Part, Reply, Request, Role, StopReason, ToolCall, ToolDef, ToolResult,
    Usage,
};

use http::{Framing, HttpRequest};

/// Where and how to reach a provider.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub kind: ProviderKind,
    /// `None`: the provider's own ([`ProviderKind::default_base_url`]).
    pub base_url: Option<String>,
    pub key: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("key", &self.key.as_ref().map(|_| "***"))
            .finish()
    }
}

/// A model provider.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Sends `req` and streams the answer: text as it arrives (and the tools the
    /// model starts calling) go to `on`, the whole answer is returned.
    async fn chat(&self, req: &Request, on: &mut (dyn FnMut(Delta) + Send))
    -> Result<Reply, Error>;

    /// The models it offers, by id.
    async fn models(&self) -> Result<Vec<String>, Error>;
}

/// The provider `config` describes.
pub fn provider(config: Config) -> Result<Arc<dyn Provider>, Error> {
    let key = config.key.filter(|k| !k.trim().is_empty());
    if config.kind.needs_key() && key.is_none() {
        return Err(Error::NoKey);
    }
    let base = config
        .base_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| config.kind.default_base_url().to_string());
    let base = base.trim().trim_end_matches('/').to_string();
    let key = key.unwrap_or_default();
    let api: Box<dyn Api> = match config.kind {
        ProviderKind::Anthropic => Box::new(anthropic::Anthropic { base, key }),
        ProviderKind::Openai => Box::new(openai::OpenAi { base, key }),
        ProviderKind::Gemini => Box::new(gemini::Gemini { base, key }),
        ProviderKind::Ollama => Box::new(ollama::Ollama { base }),
    };
    Ok(Arc::new(Client {
        api,
        http: http::client(),
    }))
}

/// What an API does its own way; [`Client`] does the HTTP.
pub(crate) trait Api: Send + Sync {
    fn chat_request(&self, req: &Request) -> HttpRequest;
    fn framing(&self) -> Framing;
    /// Reads one streamed answer.
    fn decoder(&self) -> Box<dyn Decoder>;
    fn models_request(&self) -> HttpRequest;
    fn models(&self, body: &serde_json::Value) -> Vec<String>;
}

/// Turns a stream's events (or lines) into a [`Reply`].
pub(crate) trait Decoder: Send {
    /// One SSE event's data, or one line.
    fn feed(
        &mut self,
        payload: &str,
        reply: &mut Reply,
        on: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<(), Error>;

    /// The stream ended: what is still open goes into `reply`.
    fn finish(&mut self, _reply: &mut Reply) -> Result<(), Error> {
        Ok(())
    }
}

struct Client {
    api: Box<dyn Api>,
    http: reqwest::Client,
}

#[async_trait]
impl Provider for Client {
    async fn chat(
        &self,
        req: &Request,
        on: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<Reply, Error> {
        if req.model.trim().is_empty() {
            return Err(Error::NoModel);
        }
        let mut decoder = self.api.decoder();
        let mut reply = Reply::default();
        http::stream(
            &self.http,
            self.api.chat_request(req),
            self.api.framing(),
            |payload| decoder.feed(payload, &mut reply, on),
        )
        .await?;
        decoder.finish(&mut reply)?;
        Ok(reply)
    }

    async fn models(&self) -> Result<Vec<String>, Error> {
        let body = http::json(&self.http, self.api.models_request()).await?;
        let mut models = self.api.models(&body);
        models.sort();
        models.dedup();
        Ok(models)
    }
}

/// Parses one streamed JSON payload.
pub(crate) fn parse(payload: &str) -> Result<serde_json::Value, Error> {
    serde_json::from_str(payload).map_err(|e| Error::Invalid(format!("{e}: {payload}")))
}

/// A tool result's text as a JSON value for APIs that want an object.
pub(crate) fn result_object(result: &ToolResult) -> serde_json::Value {
    let key = if result.is_error { "error" } else { "content" };
    serde_json::json!({ key: result.content })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_needed_where_the_api_wants_one() {
        let config = |kind, key: Option<&str>| Config {
            kind,
            base_url: None,
            key: key.map(str::to_string),
        };
        assert_eq!(
            provider(config(ProviderKind::Anthropic, None)).err(),
            Some(Error::NoKey)
        );
        assert_eq!(
            provider(config(ProviderKind::Openai, Some("  "))).err(),
            Some(Error::NoKey)
        );
        assert!(provider(config(ProviderKind::Ollama, None)).is_ok());
        assert!(provider(config(ProviderKind::Gemini, Some("k"))).is_ok());
        let shown = format!("{:?}", config(ProviderKind::Gemini, Some("secret")));
        assert!(!shown.contains("secret"));
    }

    #[test]
    fn tool_results_become_objects() {
        let mut r = ToolResult {
            call_id: "1".into(),
            name: "t".into(),
            content: "ok".into(),
            is_error: false,
        };
        assert_eq!(result_object(&r), serde_json::json!({"content": "ok"}));
        r.is_error = true;
        assert_eq!(result_object(&r), serde_json::json!({"error": "ok"}));
        assert!(parse("{").is_err());
    }
}
