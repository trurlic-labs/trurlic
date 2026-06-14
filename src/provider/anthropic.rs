use std::future::Future;
use std::pin::Pin;

use reqwest::Client;
use serde::Serialize;

use crate::Result;
use crate::config::ApiKey;

use super::sse::{check_status, connection_error, stream_sse};
use super::{ApiMessage, LlmProvider, Message};

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    stream: bool,
    messages: Vec<ApiMessage<'a>>,
}

pub(super) struct AnthropicClient {
    client: Client,
    key: ApiKey,
    model: String,
}

impl AnthropicClient {
    const API_URL: &str = "https://api.anthropic.com/v1/messages";
    const API_VERSION: &str = "2023-06-01";
    const MAX_TOKENS: u32 = 4096;

    pub fn new(client: Client, key: ApiKey, model: String) -> Self {
        Self { client, key, model }
    }

    async fn do_stream(
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
            .json(&body)
            .send()
            .await
            .map_err(|e| connection_error(&e))?;

        let response = check_status(response).await?;
        stream_sse(response, super::sse::extract_anthropic_text, on_text).await
    }
}

impl LlmProvider for AnthropicClient {
    fn provider_name(&self) -> &'static str {
        "anthropic"
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
