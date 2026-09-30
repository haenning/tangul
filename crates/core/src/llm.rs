//! The model providers the assistant can talk to. Only this list, the adapters in
//! `tangul_llm` and the AI settings page name a concrete provider; everything else
//! goes through `tangul_llm::Provider`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Anthropic's Messages API (Claude).
    Anthropic,
    /// OpenAI's Chat Completions API, and every endpoint that speaks it
    /// (OpenRouter, Azure OpenAI, Groq, DeepSeek, LM Studio, …) by its base URL.
    Openai,
    /// Google's Gemini API.
    Gemini,
    /// A local Ollama server: no key, nothing leaves the machine.
    Ollama,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 4] = [
        ProviderKind::Anthropic,
        ProviderKind::Openai,
        ProviderKind::Gemini,
        ProviderKind::Ollama,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::Openai => "OpenAI-compatible",
            ProviderKind::Gemini => "Google Gemini",
            ProviderKind::Ollama => "Ollama (local)",
        }
    }

    /// The name in `settings.json` (`ai.provider`).
    pub fn key(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }

    /// Where its API is when no base URL is set.
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::Openai => "https://api.openai.com/v1",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com",
            ProviderKind::Ollama => "http://localhost:11434",
        }
    }

    /// The model used until one is chosen; `None`: the user picks one from the list
    /// the provider offers.
    pub fn default_model(self) -> Option<&'static str> {
        match self {
            ProviderKind::Anthropic => Some("claude-sonnet-5-5"),
            ProviderKind::Openai | ProviderKind::Gemini | ProviderKind::Ollama => None,
        }
    }

    /// Whether it needs an API key (stored in the Keychain).
    pub fn needs_key(self) -> bool {
        self != ProviderKind::Ollama
    }

    /// Whether its base URL is worth changing: other OpenAI-compatible services, or
    /// Ollama on another machine or port.
    pub fn custom_base_url(self) -> bool {
        matches!(self, ProviderKind::Openai | ProviderKind::Ollama)
    }

    /// Keychain account of its API key.
    pub fn key_account(self) -> String {
        format!("ai-key:{}", self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderKind;

    #[test]
    fn keys_round_trip() {
        for p in ProviderKind::ALL {
            assert_eq!(ProviderKind::from_key(p.key()), Some(p));
            assert!(!p.label().is_empty());
            assert!(p.default_base_url().starts_with("http"));
            assert!(p.key_account().ends_with(p.key()));
        }
        assert_eq!(ProviderKind::from_key("nope"), None);
        assert!(!ProviderKind::Ollama.needs_key() && ProviderKind::Gemini.needs_key());
        assert!(ProviderKind::Anthropic.default_model().is_some());
        assert_eq!(ProviderKind::Ollama.default_model(), None);
        assert!(
            ProviderKind::Openai.custom_base_url() && !ProviderKind::Anthropic.custom_base_url()
        );
    }
}
