mod anthropic;
mod gemini;
mod openai;
pub(crate) mod sse;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::{Provider, ProviderConfig};
use crate::{Error, Result};

// ── Message types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

// ── Shared request types ────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct ApiMessage<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

// ── LlmProvider trait ───────────────────────────────────────────────────────

pub trait LlmProvider {
    /// Canonical provider name for diagnostics.
    fn provider_name(&self) -> &'static str;

    fn stream_completion<'a>(
        &'a self,
        messages: &'a [Message],
        system: &'a str,
        on_text: &'a mut dyn FnMut(&str),
    ) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>>;
}

pub fn create_provider(config: ProviderConfig) -> Result<Box<dyn LlmProvider>> {
    let ProviderConfig {
        provider,
        key,
        model,
        base_url,
    } = config;

    let client = Client::builder()
        .user_agent(concat!("trurlic/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::ProviderConfig(format!("failed to create HTTP client: {e}")))?;

    Ok(match provider {
        Provider::Anthropic => Box::new(anthropic::AnthropicClient::new(client, key, model)),
        Provider::OpenAi => Box::new(openai::OpenAiClient::new(
            client,
            key,
            model,
            "https://api.openai.com/v1",
            openai::ApiVariant::Standard,
        )),
        Provider::OpenRouter => Box::new(openai::OpenAiClient::new(
            client,
            key,
            model,
            "https://openrouter.ai/api/v1",
            openai::ApiVariant::OpenRouter,
        )),
        Provider::Custom | Provider::Ollama => {
            let variant = match provider {
                Provider::Custom => openai::ApiVariant::Custom,
                Provider::Ollama => openai::ApiVariant::Ollama,
                _ => unreachable!(),
            };
            let url = base_url.ok_or_else(|| {
                Error::ProviderConfig(format!("{} provider requires a base URL", provider.name(),))
            })?;
            Box::new(openai::OpenAiClient::new(client, key, model, &url, variant))
        }
        Provider::Gemini => Box::new(gemini::GeminiClient::new(client, key, model)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiKey;

    #[test]
    fn create_provider_anthropic() {
        let config = ProviderConfig {
            provider: Provider::Anthropic,
            key: ApiKey::new("sk-test".into()),
            model: "claude-sonnet-4-20250514".into(),
            base_url: None,
        };
        let client = create_provider(config).unwrap();
        assert_eq!(client.provider_name(), "anthropic");
    }

    #[test]
    fn create_provider_openai() {
        let config = ProviderConfig {
            provider: Provider::OpenAi,
            key: ApiKey::new("sk-test".into()),
            model: "gpt-4o".into(),
            base_url: None,
        };
        let client = create_provider(config).unwrap();
        assert_eq!(client.provider_name(), "openai");
    }

    #[test]
    fn create_provider_openrouter_uses_openai_compatible() {
        let config = ProviderConfig {
            provider: Provider::OpenRouter,
            key: ApiKey::new("sk-test".into()),
            model: "anthropic/claude-sonnet-4-20250514".into(),
            base_url: None,
        };
        let client = create_provider(config).unwrap();
        assert!(client.provider_name().starts_with("openai-compatible"));
    }

    #[test]
    fn create_provider_custom() {
        let config = ProviderConfig {
            provider: Provider::Custom,
            key: ApiKey::new("sk-test".into()),
            model: "llama-3.3-70b".into(),
            base_url: Some("http://localhost:8080/v1".into()),
        };
        let client = create_provider(config).unwrap();
        assert_eq!(client.provider_name(), "openai-compatible/custom");
    }

    #[test]
    fn create_provider_custom_missing_base_url() {
        let config = ProviderConfig {
            provider: Provider::Custom,
            key: ApiKey::new("sk-test".into()),
            model: "some-model".into(),
            base_url: None,
        };
        match create_provider(config) {
            Err(Error::ProviderConfig(msg)) => {
                assert!(msg.contains("base URL"), "{msg}");
            }
            Err(other) => panic!("expected ProviderConfig, got: {other}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[test]
    fn create_provider_ollama() {
        let config = ProviderConfig {
            provider: Provider::Ollama,
            key: ApiKey::new(String::new()),
            model: "llama3.1".into(),
            base_url: Some("http://localhost:11434/v1".into()),
        };
        let client = create_provider(config).unwrap();
        assert_eq!(client.provider_name(), "ollama");
    }

    #[test]
    fn create_provider_ollama_custom_base_url() {
        let config = ProviderConfig {
            provider: Provider::Ollama,
            key: ApiKey::new(String::new()),
            model: "llama3.1".into(),
            base_url: Some("http://myhost:9999/v1".into()),
        };
        let client = create_provider(config).unwrap();
        assert_eq!(client.provider_name(), "ollama");
    }

    #[test]
    fn create_provider_gemini() {
        let config = ProviderConfig {
            provider: Provider::Gemini,
            key: ApiKey::new("test-key".into()),
            model: "gemini-2.5-flash".into(),
            base_url: None,
        };
        let client = create_provider(config).unwrap();
        assert_eq!(client.provider_name(), "gemini");
    }

    #[test]
    fn role_serializes_correctly() {
        assert_eq!(Role::User.as_str(), "user");
        assert_eq!(Role::Assistant.as_str(), "assistant");
    }
}
