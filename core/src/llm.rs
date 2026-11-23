use std::time::{Duration, Instant};

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::config::LlmConfig;
use crate::error::{Result, SilentError};
use crate::prompt::{
    ANSWER_ALL_PROMPT, NO_QUESTION, SYSTEM_PROMPT, build_user_prompt, is_no_question,
};

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub id: u64,
    pub question: String,
    pub transcript: String,
    /// Epoch milliseconds captured when the question ended; informs the UI's
    /// end-to-end latency calculation.
    pub speech_end_epoch_ms: u64,
}

#[derive(Debug, Clone)]
pub enum LlmEvent {
    FirstToken {
        request_id: u64,
        at: Instant,
    },
    Token {
        request_id: u64,
        text: String,
        at: Instant,
    },
    NoQuestion {
        request_id: u64,
    },
    Done {
        request_id: u64,
        full_text: String,
        first_token_at: Option<Instant>,
        incomplete: bool,
        error: Option<String>,
    },
    Error {
        request_id: u64,
        message: String,
    },
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    config: LlmConfig,
    endpoint: String,
    api_key: Option<String>,
    /// Stable per-process id for OpenCode's `x-opencode-session` routing
    /// header. Other providers do not receive it.
    session_id: String,
    send_session_header: bool,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(config.connect_timeout_ms.max(500)))
            .pool_idle_timeout(Duration::from_secs(300))
            .pool_max_idle_per_host(4)
            .tcp_nodelay(true)
            .http2_adaptive_window(true)
            .user_agent(concat!("silent-ai/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| anyhow::Error::from(SilentError::Llm(format!("HTTP client: {e}"))))?;
        let endpoint = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let api_key = config.api_key()?;
        Ok(Self {
            http,
            send_session_header: config.base_url.contains("opencode.ai"),
            session_id: new_session_id(),
            config,
            endpoint,
            api_key,
        })
    }

    /// OpenCode Go rejects requests without a session id. Applied to every
    /// OpenCode request (chat and warmup).
    fn common_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = if self.send_session_header {
            request.header("x-opencode-session", &self.session_id)
        } else {
            request
        };
        if let Some(key) = &self.api_key {
            request.bearer_auth(key)
        } else {
            request
        }
    }

    /// Establish DNS/TLS/HTTP2 before the first real question. A 404 from a
    /// provider without `/models` still warms the connection.
    pub async fn warmup(&self, cancel: &CancellationToken) {
        if !self.config.warmup {
            return;
        }
        let url = format!("{}/models", self.config.base_url.trim_end_matches('/'));
        let request = self.common_headers(self.http.get(&url).timeout(Duration::from_secs(3)));
        tokio::select! {
            _ = cancel.cancelled() => {}
            result = request.send() => {
                match result {
                    Ok(response) => tracing::info!(status = %response.status(), "LLM connection warmed"),
                    Err(err) => tracing::warn!(%err, "LLM warmup request failed (will retry on first question)"),
                }
            }
        }
    }

    pub fn spawn(
        self,
        request: LlmRequest,
        tx: UnboundedSender<LlmEvent>,
        cancel: CancellationToken,
    ) {
        tokio::spawn(async move {
            let id = request.id;
            if let Err(err) = self.stream(request, tx.clone(), cancel).await {
                let _ = tx.send(LlmEvent::Error {
                    request_id: id,
                    message: err.to_string(),
                });
            }
        });
    }

    async fn stream(
        &self,
        request: LlmRequest,
        tx: UnboundedSender<LlmEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let user_prompt = build_user_prompt(
            &request.transcript,
            &request.question,
            self.config.max_history_chars,
        );
        let system_prompt = if self.config.answer_all {
            ANSWER_ALL_PROMPT
        } else {
            SYSTEM_PROMPT
        };
        let mut body = json!({
            "model": self.config.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt }
            ],
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
            "stream": true
        });
        if let (Some(target), Some(extra)) =
            (body.as_object_mut(), self.config.extra_body.as_object())
        {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }

        let attempts = if self.config.retry_once { 2 } else { 1 };
        let mut response = None;
        for attempt in 0..attempts {
            if cancel.is_cancelled() {
                return Err(anyhow::Error::from(SilentError::Cancelled));
            }

            let req = self.common_headers(
                self.http
                    .post(&self.endpoint)
                    .timeout(Duration::from_millis(self.config.timeout_ms))
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .json(&body),
            );

            let result = tokio::select! {
                _ = cancel.cancelled() => return Err(anyhow::Error::from(SilentError::Cancelled)),
                result = req.send() => result,
            };

            match result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        response = Some(resp);
                        break;
                    }

                    let retryable = status.as_u16() == 429 || status.is_server_error();
                    let text = resp.text().await.unwrap_or_default();
                    let short = text.chars().take(400).collect::<String>();
                    if retryable && attempt + 1 < attempts {
                        tracing::warn!(%status, attempt, "retryable LLM status; retrying once");
                        tokio::select! {
                            _ = cancel.cancelled() => return Err(anyhow::Error::from(SilentError::Cancelled)),
                            _ = tokio::time::sleep(Duration::from_millis(self.config.retry_delay_ms)) => {}
                        }
                        continue;
                    }
                    let hint = if status.as_u16() == 402 && self.endpoint.contains("opencode.ai") {
                        " Hint: OpenCode Go subscription requests must use base_url \
                         https://opencode.ai/zen/go/v1; the Zen endpoint (https://opencode.ai/zen/v1) \
                         is pay-as-you-go and needs credits."
                    } else {
                        ""
                    };
                    return Err(anyhow::Error::from(SilentError::Llm(format!(
                        "HTTP {status} from {}: {short}{hint}",
                        self.endpoint
                    ))));
                }
                Err(err) => {
                    return Err(anyhow::Error::from(SilentError::Llm(format!(
                        "request to {} failed: {err}",
                        self.endpoint
                    ))));
                }
            }
        }

        let Some(response) = response else {
            return Err(anyhow::Error::from(SilentError::Llm(
                "no response from LLM".to_owned(),
            )));
        };

        let mut stream = response.bytes_stream().eventsource();
        let mut full_text = String::new();
        let mut sent_any = false;
        let mut first_token_at: Option<Instant> = None;
        let mut held_prefix = String::new();
        let mut no_question = false;
        let mut incomplete = false;
        let mut stream_error: Option<String> = None;

        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => {
                    return Ok(());
                }
                next = stream.next() => next,
            };
            let Some(event) = next else {
                break;
            };
            match event {
                Ok(event) => {
                    let data = event.data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if data == "[DONE]" {
                        break;
                    }
                    let value: Value = match serde_json::from_str(data) {
                        Ok(value) => value,
                        Err(err) => {
                            tracing::debug!(%err, "ignoring malformed SSE data");
                            continue;
                        }
                    };
                    if let Some(error) = value.get("error") {
                        let message = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("provider returned an error event");
                        let _ = tx.send(LlmEvent::Error {
                            request_id: request.id,
                            message: message.to_owned(),
                        });
                        return Ok(());
                    }

                    let Some(fragment) = extract_delta(&value) else {
                        continue;
                    };
                    if fragment.is_empty() {
                        continue;
                    }
                    full_text.push_str(&fragment);

                    if is_no_question(&full_text) {
                        no_question = true;
                        continue;
                    }

                    // Hold the first few characters while they could still be
                    // the NO_QUESTION sentinel; providers may split it into
                    // "NO_" + "QUESTION".
                    if !sent_any && full_text.trim_start().len() < NO_QUESTION.len() {
                        let trimmed = full_text.trim_start();
                        if NO_QUESTION.starts_with(trimmed) {
                            held_prefix.push_str(&fragment);
                            continue;
                        }
                    }

                    let text = if held_prefix.is_empty() {
                        fragment
                    } else {
                        let mut combined = std::mem::take(&mut held_prefix);
                        combined.push_str(&fragment);
                        combined
                    };
                    let at = Instant::now();
                    if first_token_at.is_none() {
                        first_token_at = Some(at);
                        let _ = tx.send(LlmEvent::FirstToken {
                            request_id: request.id,
                            at,
                        });
                    }
                    sent_any = true;
                    let _ = tx.send(LlmEvent::Token {
                        request_id: request.id,
                        text,
                        at,
                    });
                }
                Err(err) => {
                    incomplete = true;
                    stream_error = Some(format!("stream error: {err}"));
                    break;
                }
            }
        }

        if no_question || is_no_question(&full_text) {
            let _ = tx.send(LlmEvent::NoQuestion {
                request_id: request.id,
            });
            return Ok(());
        }

        let _ = tx.send(LlmEvent::Done {
            request_id: request.id,
            full_text,
            first_token_at,
            incomplete,
            error: stream_error,
        });
        Ok(())
    }
}

/// Extract the incremental text from the many OpenAI-compatible shapes in the
/// wild: `choices[0].delta.content`, array content parts, and non-stream
/// `message.content`.
pub fn extract_delta(value: &Value) -> Option<String> {
    let choice = value.get("choices")?.get(0)?;
    if let Some(content) = choice.get("delta").and_then(|d| d.get("content")) {
        return content_to_string(content);
    }
    if let Some(content) = choice.get("message").and_then(|m| m.get("content")) {
        return content_to_string(content);
    }
    if let Some(text) = choice.get("text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    None
}

fn content_to_string(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    if let Some(parts) = content.as_array() {
        let mut combined = String::new();
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                combined.push_str(text);
            }
        }
        if !combined.is_empty() {
            return Some(combined);
        }
    }
    None
}

/// Stable for the lifetime of the process, unique across restarts. OpenCode
/// only needs an opaque id; it does not have to be a UUID.
fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("silent-ai-{}-{nanos:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_nonempty_and_stable_shape() {
        let id = new_session_id();
        assert!(id.starts_with("silent-ai-"));
        assert!(id.len() > "silent-ai-".len() + 4);
    }

    #[test]
    fn extracts_openai_delta() {
        let value = json!({"choices":[{"delta":{"content":"Привіт"}}]});
        assert_eq!(extract_delta(&value).as_deref(), Some("Привіт"));
    }

    #[test]
    fn extracts_content_parts() {
        let value = json!({"choices":[{"delta":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]});
        assert_eq!(extract_delta(&value).as_deref(), Some("ab"));
    }

    #[test]
    fn ignores_role_only_delta() {
        let value = json!({"choices":[{"delta":{"role":"assistant"}}]});
        assert_eq!(extract_delta(&value), None);
    }

    #[test]
    fn no_question_is_detected_from_prefix() {
        assert!(is_no_question("NO_QUESTION"));
        assert!(is_no_question("NO_QUESTION\n"));
        assert!(!is_no_question("NO, the answer is 42"));
    }
}
