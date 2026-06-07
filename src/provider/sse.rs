use std::time::Duration;

use serde_json::Value;

use crate::{Error, Result};

/// Maximum time to wait for the next chunk before treating the stream as stalled.
const STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum accumulated response size (2 MB). Design conversations are typically
/// 10-50 KB; this guards against runaway streams exhausting memory.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Maximum size for the SSE line buffer before a newline is found (1 MB).
/// A legitimate SSE line is rarely over a few KB; a buffer this large without
/// a newline indicates a corrupt or malicious stream.
const MAX_SSE_BUFFER_BYTES: usize = 1024 * 1024;

/// Timeout for reading a non-streaming error body. Prevents hanging on a
/// server that sends headers but stalls the error payload.
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn stream_sse(
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

        if buffer.len() > MAX_SSE_BUFFER_BYTES {
            return Err(Error::Api {
                status: 0,
                detail: format!(
                    "SSE buffer exceeded {MAX_SSE_BUFFER_BYTES} bytes without a newline — stream may be corrupt"
                ),
            });
        }

        for text in drain_sse_text(&mut buffer, extract) {
            on_text(&text);
            full.push_str(&text);
        }

        if full.len() > MAX_RESPONSE_BYTES {
            return Err(Error::Api {
                status: 0,
                detail: format!("response exceeded {MAX_RESPONSE_BYTES} byte limit"),
            });
        }
    }

    // Flush any remaining complete lines
    for text in drain_sse_text(&mut buffer, extract) {
        on_text(&text);
        full.push_str(&text);
    }

    Ok(full)
}

fn drain_sse_text(buffer: &mut String, extract: fn(&str) -> Option<String>) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut consumed = 0;

    while let Some(pos) = buffer[consumed..].find('\n') {
        let line = buffer[consumed..consumed + pos].trim();
        consumed += pos + 1;

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

pub(crate) fn extract_anthropic_text(data: &str) -> Option<String> {
    let json: Value = serde_json::from_str(data).ok()?;
    let event_type = json.get("type")?.as_str()?;
    if event_type != "content_block_delta" {
        return None;
    }
    json.pointer("/delta/text")?.as_str().map(String::from)
}

pub(crate) fn extract_openai_text(data: &str) -> Option<String> {
    let json: Value = serde_json::from_str(data).ok()?;
    json.pointer("/choices/0/delta/content")?
        .as_str()
        .map(String::from)
}

// ── Error helpers ────────────────────────────────────────────────────────────

pub(crate) async fn check_status(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status().as_u16();

    // Bounded read: prevent hanging on a server that stalls the error body.
    let body = match tokio::time::timeout(ERROR_BODY_TIMEOUT, response.text()).await {
        Ok(Ok(text)) => text,
        Ok(Err(_)) => String::new(),
        Err(_) => String::new(),
    };

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

pub(crate) fn connection_error(err: &reqwest::Error) -> Error {
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── anthropic extraction ─────────────────────────────────────────────

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

    // ── openai extraction ────────────────────────────────────────────────

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
}
