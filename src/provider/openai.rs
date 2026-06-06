//! OpenAI-compatible Chat Completions API provider (also used for OpenRouter).

use std::future::Future;
use std::pin::Pin;

use reqwest::Client;
use serde::Serialize;

use crate::Result;
use crate::config::ApiKey;

use super::sse::{check_status, connection_error, stream_sse};
use super::{ApiMessage, LlmProvider, Message};

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    messages: Vec<ApiMessage<'a>>,
}

pub(super) struct OpenAiClient {
    client: Client,
    key: ApiKey,
    model: String,
    base_url: String,
}

impl OpenAiClient {
    const MAX_TOKENS: u32 = 4096;

    pub fn new(client: Client, key: ApiKey, model: String, base_url: &str) -> Self {
        Self {
            client,
            key,
            model,
            base_url: base_url.into(),
        }
    }

    async fn do_stream(
        &self,
        messages: &[Message],
        system: &str,
        on_text: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let mut api_messages: Vec<ApiMessage<'_>> = Vec::with_capacity(messages.len() + 1);

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
        stream_sse(response, super::sse::extract_openai_text, on_text).await
    }
}

impl LlmProvider for OpenAiClient {
    fn provider_name(&self) -> &'static str {
        if self.base_url.contains("openrouter") {
            "openai-compatible/openrouter"
        } else {
            "openai"
        }
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
