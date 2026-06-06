//! LLM provider implementations with streaming SSE support.
//!
//! [`LlmClient`] dispatches to provider-specific implementations via enum,
//! avoiding async trait object overhead. All providers stream responses
//! through a shared SSE parser, calling a text callback for each chunk.
//!
//! # Providers
//!
//! - **Anthropic** — Messages API (`/v1/messages`)
//! - **OpenAI** — Chat Completions API (`/v1/chat/completions`)
//! - **OpenRouter** — OpenAI-compatible format with different base URL

use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::Value;

use crate::config::{ApiKey, Provider, ProviderConfig};
use crate::{Error, Result};

// ── Message types ────────────────────────────────────────────────────────────

/// A message in the conversation history.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// Message author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

// ── LlmClient ────────────────────────────────────────────────────────────────

/// LLM provider with streaming completion support.
///
/// Uses enum dispatch (closed set of providers) — no vtable, no `Box`,
/// no async trait object-safety issues.
pub enum LlmClient {
    Anthropic(AnthropicClient),
    OpenAi(OpenAiClient),
}

impl LlmClient {
    /// Create a provider client from resolved configuration.
    ///
    /// Consumes the [`ProviderConfig`] to take ownership of the API key.
    pub fn from_config(config: ProviderConfig) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::ProviderConfig(format!("failed to create HTTP client: {e}")))?;

        Ok(match config.provider {
            Provider::Anthropic => Self::Anthropic(AnthropicClient {
                client,
                key: config.key,
                model: config.model,
            }),
            Provider::OpenAi => Self::OpenAi(OpenAiClient {
                client,
                key: config.key,
                model: config.model,
                base_url: "https://api.openai.com/v1".into(),
            }),
            Provider::OpenRouter => Self::OpenAi(OpenAiClient {
                client,
                key: config.key,
                model: config.model,
                base_url: "https://openrouter.ai/api/v1".into(),
            }),
        })
    }

    /// Stream a completion, calling `on_text` for each text chunk.
    ///
    /// Returns the full accumulated response. Each chunk is delivered as
    /// soon as it arrives from the provider — the caller controls display
    /// (e.g. printing to stdout, accumulating in a buffer).
    pub async fn stream_completion(
        &self,
        messages: &[Message],
        system: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        match self {
            Self::Anthropic(c) => c.stream_completion(messages, system, on_text).await,
            Self::OpenAi(c) => c.stream_completion(messages, system, on_text).await,
        }
    }
}

// ── Request types (zero-overhead serialization, no intermediate Value tree) ──

/// A message in an API request body.
#[derive(Serialize)]
struct ApiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

/// Anthropic Messages API request body.
#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    stream: bool,
    messages: Vec<ApiMessage<'a>>,
}

/// OpenAI-compatible Chat Completions request body.
#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    messages: Vec<ApiMessage<'a>>,
}

// ── AnthropicClient ──────────────────────────────────────────────────────────

/// Anthropic Messages API client.
pub struct AnthropicClient {
    client: Client,
    key: ApiKey,
    model: String,
}

impl AnthropicClient {
    const API_URL: &str = "https://api.anthropic.com/v1/messages";
    const API_VERSION: &str = "2023-06-01";
    const MAX_TOKENS: u32 = 4096;

    async fn stream_completion(
        &self,
        messages: &[Message],
        system: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let api_messages: Vec<ApiMessage<'_>> = messages
            .iter()
            .map(|m| ApiMessage {
                role: m.role.as_str(),
                content: &m.content,
            })
            .collect();

        let body = AnthropicRequest {
            model: &self.model,
            max_tokens: Self::MAX_TOKENS,
            system,
            stream: true,
            messages: api_messages,
        };

        let response = self
            .client
            .post(Self::API_URL)
            .header("x-api-key", self.key.expose())
            .header("anthropic-version", Self::API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| connection_error(&e))?;

        let response = check_status(response).await?;
        stream_sse(response, extract_anthropic_text, on_text).await
    }
}

// ── OpenAiClient ─────────────────────────────────────────────────────────────

/// OpenAI-compatible client (also used for OpenRouter).
pub struct OpenAiClient {
    client: Client,
    key: ApiKey,
    model: String,
    base_url: String,
}

impl OpenAiClient {
    const MAX_TOKENS: u32 = 4096;

    async fn stream_completion(
        &self,
        messages: &[Message],
        system: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let mut api_messages: Vec<ApiMessage<'_>> = Vec::with_capacity(messages.len() + 1);

        // OpenAI encodes the system prompt as a system-role message
        api_messages.push(ApiMessage {
            role: "system",
            content: system,
        });

        for m in messages {
            api_messages.push(ApiMessage {
                role: m.role.as_str(),
                content: &m.content,
            });
        }

        let body = OpenAiRequest {
            model: &self.model,
            max_tokens: Self::MAX_TOKENS,
            stream: true,
            messages: api_messages,
        };

        let url = format!("{}/chat/completions", self.base_url);

        let mut req = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.key.expose()))
            .header("content-type", "application/json");

        // OpenRouter requires attribution headers
        if self.base_url.contains("openrouter") {
            req = req
                .header("http-referer", "https://github.com/trurl-labs/trurl")
                .header("x-title", "trurl");
        }

        let response = req
            .json(&body)
            .send()
            .await
            .map_err(|e| connection_error(&e))?;

        let response = check_status(response).await?;
        stream_sse(response, extract_openai_text, on_text).await
    }
}

// ── Shared SSE streaming ─────────────────────────────────────────────────────

/// Maximum time to wait for the next chunk before treating the stream as stalled.
const STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Stream SSE events from a response, extracting text with the given function.
///
/// Times out if no data arrives within [`STREAM_STALL_TIMEOUT`], preventing
/// a stalled connection from hanging the terminal indefinitely.
async fn stream_sse(
    mut response: reqwest::Response,
    extract: fn(&str) -> Option<String>,
    on_text: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut full = String::new();
    let mut buffer = String::new();

    loop {
        let chunk = match tokio::time::timeout(STREAM_STALL_TIMEOUT, response.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                return Err(Error::Api {
                    status: 0,
                    detail: format!("stream interrupted: {e}"),
                });
            }
            Err(_) => {
                return Err(Error::Api {
                    status: 0,
                    detail: format!(
                        "stream stalled: no data received for {}s",
                        STREAM_STALL_TIMEOUT.as_secs()
                    ),
                });
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        for text in drain_sse_text(&mut buffer, extract) {
            on_text(&text);
            full.push_str(&text);
        }
    }

    // Flush any remaining complete lines
    for text in drain_sse_text(&mut buffer, extract) {
        on_text(&text);
        full.push_str(&text);
    }

    Ok(full)
}

/// Extract all complete SSE text chunks from a buffer.
///
/// Scans for lines terminated by `\n`, collecting text from `data:` fields.
/// Drains consumed bytes in a single operation at the end, leaving any
/// trailing incomplete line in the buffer for the next call.
fn drain_sse_text(buffer: &mut String, extract: fn(&str) -> Option<String>) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut consumed = 0;

    while let Some(pos) = buffer[consumed..].find('\n') {
        let line = buffer[consumed..consumed + pos].trim();
        consumed += pos + 1;

        // Skip empty lines (SSE event boundary) and comments
        if line.is_empty() || line.starts_with(':') {
            continue;
        }

        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                continue;
            }
            if let Some(text) = extract(data) {
                chunks.push(text);
            }
        }
    }

    if consumed > 0 {
        buffer.drain(..consumed);
    }
    chunks
}

// ── Provider-specific text extractors ────────────────────────────────────────

/// Extract text from an Anthropic SSE `content_block_delta` event.
///
/// Format: `{"type":"content_block_delta","delta":{"type":"text_delta","text":"..."}}`
fn extract_anthropic_text(data: &str) -> Option<String> {
    let json: Value = serde_json::from_str(data).ok()?;
    let event_type = json.get("type")?.as_str()?;
    if event_type != "content_block_delta" {
        return None;
    }
    json.pointer("/delta/text")?.as_str().map(String::from)
}

/// Extract text from an OpenAI SSE `chat.completion.chunk` event.
///
/// Format: `{"choices":[{"delta":{"content":"..."},"index":0}]}`
fn extract_openai_text(data: &str) -> Option<String> {
    let json: Value = serde_json::from_str(data).ok()?;
    json.pointer("/choices/0/delta/content")?
        .as_str()
        .map(String::from)
}

// ── Error helpers ────────────────────────────────────────────────────────────

/// Check HTTP status, consuming the body for error detail on failure.
///
/// Returns the response on success for subsequent streaming.
async fn check_status(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();

    // Try to extract a structured error message from the JSON body
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message").or_else(|| e.get("type")))
                .and_then(|m| m.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| truncate(&body, 200));

    Err(match status {
        401 => Error::Api {
            status,
            detail: format!("authentication failed — check your API key ({detail})"),
        },
        403 => Error::Api {
            status,
            detail: format!("access denied — API key may lack permissions ({detail})"),
        },
        429 => Error::Api {
            status,
            detail: format!("rate limited — wait a moment and try again ({detail})"),
        },
        500..=599 => Error::Api {
            status,
            detail: format!("provider server error: {detail}"),
        },
        _ => Error::Api { status, detail },
    })
}

/// Truncate a string to at most `max` chars.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max)
            .last()
            .unwrap_or(0);
        format!("{}…", &s[..end])
    }
}

/// Build a user-friendly error from a connection/request failure.
fn connection_error(err: &reqwest::Error) -> Error {
    if err.is_connect() {
        Error::Api {
            status: 0,
            detail: format!("could not connect to API — check your internet connection ({err})"),
        }
    } else if err.is_timeout() {
        Error::Api {
            status: 0,
            detail: "connection timed out — the API may be overloaded".into(),
        }
    } else {
        Error::Api {
            status: 0,
            detail: format!("API request failed: {err}"),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── SSE text extraction ─────────────────────────────────────────────

    #[test]
    fn anthropic_extracts_text_delta() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        assert_eq!(extract_anthropic_text(data), Some("Hello".into()));
    }

    #[test]
    fn anthropic_extracts_multiline_text() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"line1\nline2"}}"#;
        assert_eq!(extract_anthropic_text(data), Some("line1\nline2".into()));
    }

    #[test]
    fn anthropic_ignores_message_start() {
        let data = r#"{"type":"message_start","message":{"id":"msg_01"}}"#;
        assert_eq!(extract_anthropic_text(data), None);
    }

    #[test]
    fn anthropic_ignores_content_block_start() {
        let data =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        assert_eq!(extract_anthropic_text(data), None);
    }

    #[test]
    fn anthropic_ignores_message_delta() {
        let data = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#;
        assert_eq!(extract_anthropic_text(data), None);
    }

    #[test]
    fn anthropic_ignores_invalid_json() {
        assert_eq!(extract_anthropic_text("not json"), None);
    }

    #[test]
    fn openai_extracts_content_delta() {
        let data = r#"{"choices":[{"delta":{"content":"world"},"index":0}]}"#;
        assert_eq!(extract_openai_text(data), Some("world".into()));
    }

    #[test]
    fn openai_ignores_empty_delta() {
        let data = r#"{"choices":[{"delta":{},"index":0}]}"#;
        assert_eq!(extract_openai_text(data), None);
    }

    #[test]
    fn openai_ignores_role_only_delta() {
        let data = r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#;
        assert_eq!(extract_openai_text(data), None);
    }

    #[test]
    fn openai_ignores_invalid_json() {
        assert_eq!(extract_openai_text("{malformed"), None);
    }

    // ── SSE buffer processing ───────────────────────────────────────────

    fn extract_test_text(data: &str) -> Option<String> {
        let json: Value = serde_json::from_str(data).ok()?;
        json.get("t")?.as_str().map(String::from)
    }

    #[test]
    fn drain_processes_complete_lines() {
        let mut buf = "data: {\"t\":\"a\"}\ndata: {\"t\":\"b\"}\n".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["a", "b"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn drain_preserves_incomplete_line() {
        let mut buf = "data: {\"t\":\"a\"}\ndata: incompl".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["a"]);
        assert_eq!(buf, "data: incompl");
    }

    #[test]
    fn drain_skips_done_marker() {
        let mut buf = "data: {\"t\":\"x\"}\ndata: [DONE]\n".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["x"]);
    }

    #[test]
    fn drain_skips_sse_comments() {
        let mut buf = ": keep-alive\ndata: {\"t\":\"y\"}\n".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["y"]);
    }

    #[test]
    fn drain_skips_empty_lines() {
        let mut buf = "\ndata: {\"t\":\"z\"}\n\n".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["z"]);
    }

    #[test]
    fn drain_handles_crlf() {
        let mut buf = "data: {\"t\":\"cr\"}\r\n".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["cr"]);
    }

    #[test]
    fn drain_skips_unparseable_data() {
        let mut buf = "data: not-json\ndata: {\"t\":\"ok\"}\n".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["ok"]);
    }

    #[test]
    fn drain_ignores_non_data_fields() {
        let mut buf = "event: delta\ndata: {\"t\":\"v\"}\n\n".to_string();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert_eq!(chunks, vec!["v"]);
    }

    #[test]
    fn drain_empty_buffer_is_noop() {
        let mut buf = String::new();
        let chunks = drain_sse_text(&mut buf, extract_test_text);
        assert!(chunks.is_empty());
    }

    // ── Client construction ─────────────────────────────────────────────

    #[test]
    fn from_config_anthropic() {
        let config = ProviderConfig {
            provider: Provider::Anthropic,
            key: ApiKey::new("sk-test".into()),
            model: "claude-sonnet-4-20250514".into(),
        };
        let client = LlmClient::from_config(config).unwrap();
        assert!(matches!(client, LlmClient::Anthropic(_)));
    }

    #[test]
    fn from_config_openai() {
        let config = ProviderConfig {
            provider: Provider::OpenAi,
            key: ApiKey::new("sk-test".into()),
            model: "gpt-4o".into(),
        };
        let client = LlmClient::from_config(config).unwrap();
        assert!(matches!(client, LlmClient::OpenAi(_)));
    }

    #[test]
    fn from_config_openrouter_uses_openai_client() {
        let config = ProviderConfig {
            provider: Provider::OpenRouter,
            key: ApiKey::new("sk-test".into()),
            model: "anthropic/claude-sonnet-4-20250514".into(),
        };
        let client = LlmClient::from_config(config).unwrap();
        assert!(matches!(client, LlmClient::OpenAi(_)));
    }

    #[test]
    fn role_serializes_correctly() {
        assert_eq!(Role::User.as_str(), "user");
        assert_eq!(Role::Assistant.as_str(), "assistant");
    }
}
