use std::future::Future;
use std::pin::Pin;

use reqwest::Client;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::Result;
use crate::config::ApiKey;

use super::sse::{check_status, connection_error, stream_sse};
use super::{LlmProvider, Message};

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const MAX_TOKENS: u32 = 4096;

// ── Request types ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GeminiRequest<'a> {
    contents: Vec<GeminiContent<'a>>,
    #[serde(rename = "systemInstruction")]
    system_instruction: GeminiSystemInstruction<'a>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct GeminiSystemInstruction<'a> {
    parts: [GeminiPart<'a>; 1],
}

#[derive(Serialize)]
struct GeminiContent<'a> {
    role: &'a str,
    parts: [GeminiPart<'a>; 1],
}

#[derive(Serialize)]
struct GeminiPart<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct GenerationConfig {
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
}

// ── Client ─────────────────────────────────────────────────────────────────

pub(super) struct GeminiClient {
    client: Client,
    key: ApiKey,
    model: String,
}

impl GeminiClient {
    pub fn new(client: Client, key: ApiKey, model: String) -> Self {
        Self { client, key, model }
    }

    async fn do_stream(
        &self,
        messages: &[Message],
        system: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let contents: Vec<GeminiContent<'_>> = messages
            .iter()
            .map(|m| GeminiContent {
                role: gemini_role(m.role),
                parts: [GeminiPart { text: &m.content }],
            })
            .collect();

        let body = GeminiRequest {
            contents,
            system_instruction: GeminiSystemInstruction {
                parts: [GeminiPart { text: system }],
            },
            generation_config: GenerationConfig {
                max_output_tokens: MAX_TOKENS,
            },
        };

        // Key in URL query parameter — Gemini's auth model.
        // Wrapped in Zeroizing so the URL is zeroed from memory after use.
        let url = Zeroizing::new(format!(
            "{BASE_URL}/models/{}:streamGenerateContent?alt=sse&key={}",
            self.model,
            self.key.expose(),
        ));

        let response = self
            .client
            .post(url.as_str())
            .json(&body)
            .send()
            .await
            .map_err(connection_error)?;

        let response = check_status(response).await?;
        stream_sse(response, super::sse::extract_gemini_text, on_text).await
    }
}

impl LlmProvider for GeminiClient {
    fn provider_name(&self) -> &'static str {
        "gemini"
    }

    fn stream_completion<'a>(
        &'a self,
        messages: &'a [Message],
        system: &'a str,
        on_text: &'a mut dyn FnMut(&str),
    ) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
        Box::pin(self.do_stream(messages, system, on_text))
    }
}

fn gemini_role(role: super::Role) -> &'static str {
    match role {
        super::Role::User => "user",
        super::Role::Assistant => "model",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_role_user() {
        assert_eq!(gemini_role(super::super::Role::User), "user");
    }

    #[test]
    fn gemini_role_assistant() {
        assert_eq!(gemini_role(super::super::Role::Assistant), "model");
    }
}
