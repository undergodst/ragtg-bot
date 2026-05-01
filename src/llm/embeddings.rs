//! Embedding client for DeepInfra (BGE-M3, 1024-dim).
//!
//! Uses the OpenAI-compatible `/v1/openai/embeddings` endpoint.
//! Retry logic mirrors `OpenRouterClient`: exponential backoff on 429 / 5xx.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{Error, Result};

#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Debug, Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

#[derive(Clone)]
pub struct EmbeddingClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    max_retries: u32,
}

impl EmbeddingClient {
    pub fn new(base_url: String, api_key: String, model: String, timeout_sec: u64, max_retries: u32) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_sec))
            .build()
            .map_err(|e| Error::OpenRouter(format!("build embedding http client: {e}")))?;
        Ok(Self {
            http,
            base_url,
            api_key,
            model,
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
        Ok(h)
    }

    /// Embed one or more texts, returning a vector of f32 embeddings (1024-dim
    /// for BGE-M3). The order of returned vectors matches the input order.
    pub async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = EmbedRequest {
            model: &self.model,
            input: texts,
        };
        let headers = self.headers()?;

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
                        let parsed: EmbedResponse = resp
                            .json()
                            .await
                            .map_err(|e| Error::OpenRouter(format!("parse embedding body: {e}")))?;
                        return Ok(parsed.data.into_iter().map(|d| d.embedding).collect());
                    }

                    let retryable = status.as_u16() == 429 || status.is_server_error();
                    let body_text = resp.text().await.unwrap_or_default();
                    if retryable && attempt < self.max_retries {
                        let delay_ms = backoff_ms(attempt);
                        warn!(
                            attempt,
                            status = status.as_u16(),
                            delay_ms,
                            "embedding retryable error, backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::OpenRouter(format!(
                        "embedding status {} after {} attempt(s): {}",
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
                            "embedding network error, backing off"
                        );
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::OpenRouter(format!(
                        "embedding send after {} attempt(s): {}",
                        attempt + 1,
                        e
                    )));
                }
            }
        }
    }

    /// Convenience: embed a single text and return its vector.
    pub async fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let vecs = self.embed(&[text]).await?;
        vecs.into_iter()
            .next()
            .ok_or_else(|| Error::OpenRouter("embedding returned empty data array".into()))
    }
}

fn backoff_ms(attempt: u32) -> u64 {
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
