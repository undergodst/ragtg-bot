use std::time::Duration;
use futures_util::StreamExt;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::metrics;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningOptions>,
}

/// OpenRouter reasoning controls. `exclude=true` strips the model's chain-of-
/// thought from the response — model still reasons internally (we want it to
/// think hard), we just don't want the CoT leaking into the chat.
#[derive(Debug, Serialize, Clone, Copy)]
struct ReasoningOptions {
    exclude: bool,
}

impl ReasoningOptions {
    fn hidden() -> Self {
        Self { exclude: true }
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChunk {
    pub choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
pub struct StreamDelta {
    pub content: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
    reasoning: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

/// Outcome of a successful chat completion: the text plus telemetry the
/// caller wants to log (latency, token usage, model that actually answered).
#[derive(Debug, Clone)]
pub struct ChatCompletion {
    pub content: String,
    pub reasoning: Option<String>,
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

    /// capped at `max_retries`). Returns the assistant text + token usage.
    /// `disable_thinking=true` adds OpenRouter's reasoning-suppression flag
    /// so thinking-models go straight to the answer instead of streaming a
    /// chain-of-thought we'd have to discard anyway.
    pub async fn chat_completion_stream(
        &self,
        model: &str,
        messages: &[Message],
        max_tokens: u32,
        disable_thinking: bool,
    ) -> Result<impl futures_util::Stream<Item = Result<StreamChunk>>> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = ChatRequest {
            model,
            messages,
            max_tokens,
            stream: Some(true),
            reasoning: disable_thinking.then(ReasoningOptions::hidden),
        };
        let headers = self.headers()?;

        let resp = self
            .http
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::OpenRouter(format!("send stream request: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::OpenRouter(format!("stream error {status}: {text}")));
        }

        let stream = resp.bytes_stream().map(|res| {
            res.map_err(|e| Error::OpenRouter(format!("stream read error: {e}")))
        });

        Ok(Box::pin(async_stream::try_stream! {
            let mut byte_buffer = Vec::new();
            let mut stream = stream;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                byte_buffer.extend_from_slice(&chunk);

                while let Some(pos) = byte_buffer.iter().position(|&b| b == b'\n') {
                    let line_bytes: Vec<u8> = byte_buffer.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line_bytes);
                    let line = line.trim();
                    
                    if line.is_empty() { continue; }
                    if line == "data: [DONE]" { break; }
                    
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(parsed) = serde_json::from_str::<StreamChunk>(data) {
                            yield parsed;
                        }
                    }
                }
            }
        }))
    }

    /// capped at `max_retries`). Returns the assistant text + token usage.
    pub async fn chat_completion(
        &self,
        purpose: &str,
        model: &str,
        messages: &[Message],
        max_tokens: u32,
    ) -> Result<ChatCompletion> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = ChatRequest {
            model,
            messages,
            max_tokens,
            stream: None,
            reasoning: None,
        };
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
                    let body_text = resp.text().await.unwrap_or_default();
                    let trimmed = body_text.trim();
                    
                    if status.is_success() && trimmed.is_empty() {
                        tracing::error!(status = %status, "OpenRouter 200 OK but empty body");
                        return Err(Error::OpenRouter("Empty response from provider".into()));
                    }

                    let mut retryable = status.as_u16() == 429 || status.is_server_error();
                    let mut success_content = None;
                    let mut parsed_usage = None;

                    if status.is_success() {
                        let trimmed = body_text.trim();
                        match serde_json::from_str::<ChatResponse>(trimmed) {
                            Ok(parsed) => {
                                if let Some(choice) = parsed.choices.into_iter().next() {
                                    let content = choice.message.content.filter(|s| !s.trim().is_empty());
                                    let reasoning = choice.message.reasoning.filter(|s| !s.trim().is_empty());

                                    // If we have content or reasoning, we consider it a success.
                                    if content.is_some() || reasoning.is_some() {
                                        success_content = Some((content.unwrap_or_default(), reasoning));
                                        parsed_usage = parsed.usage;
                                    } else {
                                        tracing::warn!(body = %truncate(trimmed, 200), "200 OK but both content and reasoning are empty");
                                        retryable = true;
                                    }
                                } else {
                                    tracing::warn!(body = %truncate(trimmed, 200), "200 OK but no choices");
                                    retryable = true;
                                }
                            }
                            Err(e) => {
                                tracing::error!(error = %e, body_len = body_text.len(), "failed to parse ChatResponse");
                                retryable = true;
                            }
                        }
                    }

                    if let Some((content, reasoning)) = success_content {
                        let usage = parsed_usage.unwrap_or(Usage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                            total_tokens: 0,
                        });
                        
                        metrics::LLM_CALLS.with_label_values(&[purpose, "ok"]).inc();
                        metrics::LLM_LATENCY
                            .with_label_values(&[purpose])
                            .observe(started.elapsed().as_secs_f64());
                            
                        return Ok(ChatCompletion {
                            content,
                            reasoning,
                            model: model.to_string(),
                            latency_ms: started.elapsed().as_millis(),
                            prompt_tokens: usage.prompt_tokens,
                            completion_tokens: usage.completion_tokens,
                            total_tokens: usage.total_tokens,
                        });
                    }

                    if retryable && attempt < self.max_retries {
                        let delay_ms = backoff_ms(attempt);
                        tracing::warn!(
                            attempt,
                            status = status.as_u16(),
                            delay_ms,
                            body = %truncate(&body_text, 200),
                            "openrouter retryable error (or empty response), backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempt += 1;
                        continue;
                    }
                    
                    metrics::LLM_CALLS.with_label_values(&[purpose, "error"]).inc();
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
                        tracing::warn!(
                            attempt,
                            error = %e,
                            delay_ms,
                            "openrouter network error, backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempt += 1;
                        continue;
                    }
                    
                    metrics::LLM_CALLS.with_label_values(&[purpose, "error"]).inc();
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
    let clean = s.replace('\n', " ").replace('\r', "");
    if clean.chars().count() <= n {
        clean
    } else {
        clean.chars().take(n).collect::<String>() + "…"
    }
}
