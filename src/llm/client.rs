use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{Error, Result};

const HTTP_REFERER: &str = "https://github.com/undergodst/ragtg-bot";
const X_TITLE: &str = "ragtg-bot";

/// OpenAI-compatible chat message. `content` is either a plain string (the
/// historical text-only form, kept for the simple system/user/assistant
/// constructors) or an array of typed blocks (text + image_url + input_audio)
/// used by the vision/audio pipeline. `#[serde(untagged)]` lets serde pick
/// the right wire shape automatically.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Multipart(Vec<ContentBlock>),
}

/// One block inside a multimodal user message. The `type` field is the
/// OpenAI/OpenRouter discriminator (`text` / `image_url` / `input_audio`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
    InputAudio { input_audio: InputAudio },
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageUrl {
    /// Either an `https://...` URL or a `data:<mime>;base64,<...>` data URL.
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputAudio {
    /// Raw base64 (no `data:` prefix) — that's what OpenRouter / NVIDIA
    /// omni models expect for `input_audio` blocks.
    pub data: String,
    /// Format hint, e.g. `"ogg"`, `"mp3"`, `"wav"`.
    pub format: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: MessageContent::Text(content.into()),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: MessageContent::Text(content.into()),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: MessageContent::Text(content.into()),
        }
    }
    /// Build a `user`-role message with mixed text + media blocks. The
    /// caller decides ordering — typical: `[Text, ImageUrl]` or
    /// `[Text, InputAudio]`.
    pub fn user_multipart(blocks: Vec<ContentBlock>) -> Self {
        Self {
            role: "user".into(),
            content: MessageContent::Multipart(blocks),
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    max_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

/// Outcome of a successful chat completion: the text plus telemetry the
/// caller wants to log (latency, token usage, model that actually answered).
#[derive(Debug, Clone)]
pub struct ChatCompletion {
    pub content: String,
    pub model: String,
    pub latency_ms: u128,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Clone)]
pub struct OpenRouterClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    max_retries: u32,
}

impl OpenRouterClient {
    pub fn new(base_url: String, api_key: String, timeout_sec: u64, max_retries: u32) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .build()
            .map_err(|e| Error::OpenRouter(format!("build http client: {e}")))?;
        Ok(Self {
            http,
            base_url,
            api_key,
            max_retries,
        })
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        let auth = format!("Bearer {}", self.api_key);
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth)
                .map_err(|e| Error::OpenRouter(format!("auth header: {e}")))?,
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("HTTP-Referer", HeaderValue::from_static(HTTP_REFERER));
        h.insert("X-Title", HeaderValue::from_static(X_TITLE));
        Ok(h)
    }

    /// POST /chat/completions with retry on 429 / 5xx (exponential backoff,
    /// capped at `max_retries`). Returns the assistant text + token usage.
    pub async fn chat_completion(
        &self,
        model: &str,
        messages: &[Message],
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = ChatRequest { model, messages, max_tokens };
        let headers = self.headers()?;

        let started = std::time::Instant::now();
        let mut attempt: u32 = 0;
        loop {
            let res = self
                .http
                .post(&url)
                .headers(headers.clone())
                .json(&body)
                .send()
                .await;

            match res {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let parsed: ChatResponse = resp
                            .json()
                            .await
                            .map_err(|e| Error::OpenRouter(format!("parse body: {e}")))?;
                        let content = parsed
                            .choices
                            .into_iter()
                            .next()
                            .and_then(|c| c.message.content)
                            .ok_or_else(|| Error::OpenRouter("no choices in response".into()))?;
                        let usage = parsed.usage.unwrap_or(Usage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                            total_tokens: 0,
                        });
                        return Ok(ChatCompletion {
                            content,
                            model: model.to_string(),
                            latency_ms: started.elapsed().as_millis(),
                            prompt_tokens: usage.prompt_tokens,
                            completion_tokens: usage.completion_tokens,
                            total_tokens: usage.total_tokens,
                        });
                    }

                    let retryable = status.as_u16() == 429 || status.is_server_error();
                    let body_text = resp.text().await.unwrap_or_default();
                    if retryable && attempt < self.max_retries {
                        let delay_ms = backoff_ms(attempt);
                        warn!(
                            attempt,
                            status = status.as_u16(),
                            delay_ms,
                            body = %truncate(&body_text, 200),
                            "openrouter retryable error, backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::OpenRouter(format!(
                        "status {} after {} attempt(s): {}",
                        status,
                        attempt + 1,
                        truncate(&body_text, 500)
                    )));
                }
                Err(e) => {
                    if (e.is_timeout() || e.is_connect()) && attempt < self.max_retries {
                        let delay_ms = backoff_ms(attempt);
                        warn!(
                            attempt,
                            error = %e,
                            delay_ms,
                            "openrouter network error, backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::OpenRouter(format!(
                        "send after {} attempt(s): {}",
                        attempt + 1,
                        e
                    )));
                }
            }
        }
    }
}

fn backoff_ms(attempt: u32) -> u64 {
    // 500ms, 1000ms, 2000ms, ... capped at 8s.
    let base = 500u64.saturating_mul(1u64 << attempt.min(4));
    base.min(8_000)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
