//! Perception sub-agent: hands raw media bytes to a vision/audio LLM and
//! gets back a short Russian description that the main bot inlines into
//! working memory.
//!
//! Models per CLAUDE.md:
//! - primary: `nvidia/nemotron-3-nano-omni:free` (multimodal — image + audio)
//! - image fallbacks (on primary 429/5xx/timeout): qwen3-vl :free → gemini 2.0 flash
//! - audio: no fallbacks (image-only models can't transcribe).
//!
//! Concurrency cap (`vision_concurrent`) is enforced by the caller via
//! `storage::redis::acquire_vision_slot` — keeping the gate at the call
//! site keeps this module pure (no Redis types in the signature).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::error::{Error, Result};
use crate::llm::client::{ContentBlock, ImageUrl, InputAudio, Message, OpenRouterClient};
use crate::llm::prompts::vision::{IMAGE_PROMPT, VOICE_PROMPT};

/// Hard cap on how long a description can be before we trim it. Three
/// lines of voice transcript or two paragraphs of meme description fit
/// comfortably; anything longer just bloats every future prompt.
const DESC_MAX_CHARS: usize = 800;
const VISION_MAX_TOKENS: u32 = 300;

/// Describe a still image (photo / static sticker / image document).
/// Tries `primary` first, then walks `fallbacks` on any error. Returns
/// the trimmed description text.
pub async fn describe_image(
    client: &OpenRouterClient,
    bytes: &[u8],
    mime: &str,
    primary: &str,
    fallbacks: &[String],
) -> Result<String> {
    let data_url = format!("data:{};base64,{}", mime, BASE64.encode(bytes));
    let messages = vec![Message::user_multipart(vec![
        ContentBlock::Text {
            text: IMAGE_PROMPT.into(),
        },
        ContentBlock::ImageUrl {
            image_url: ImageUrl { url: data_url },
        },
    ])];

    let mut models: Vec<&str> = Vec::with_capacity(1 + fallbacks.len());
    models.push(primary);
    models.extend(fallbacks.iter().map(String::as_str));

    let mut last_err: Option<Error> = None;
    for model in models {
        match client
            .chat_completion(model, &messages, VISION_MAX_TOKENS)
            .await
        {
            Ok(c) => {
                let trimmed = trim_desc(&c.content);
                if trimmed.is_empty() {
                    tracing::warn!(model, "vision returned empty content; trying next");
                    last_err = Some(Error::OpenRouter("empty content".into()));
                    continue;
                }
                tracing::info!(
                    model,
                    latency_ms = c.latency_ms,
                    total_tokens = c.total_tokens,
                    "vision describe ok"
                );
                return Ok(trimmed);
            }
            Err(e) => {
                tracing::warn!(model, error = %e, "vision attempt failed, trying next");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| Error::OpenRouter("no vision models configured".into())))
}

/// Transcribe a Telegram voice message (OGG/Opus) via the omni model.
/// No fallback: the listed image-only models (qwen-vl, gemini-flash via
/// OpenRouter) don't accept audio blocks, so a fallback would fail loud.
pub async fn transcribe_voice(
    client: &OpenRouterClient,
    bytes: &[u8],
    voice_model: &str,
) -> Result<String> {
    let messages = vec![Message::user_multipart(vec![
        ContentBlock::Text {
            text: VOICE_PROMPT.into(),
        },
        ContentBlock::InputAudio {
            input_audio: InputAudio {
                data: BASE64.encode(bytes),
                format: "ogg".into(),
            },
        },
    ])];
    let c = client
        .chat_completion(voice_model, &messages, VISION_MAX_TOKENS)
        .await?;
    let trimmed = trim_desc(&c.content);
    if trimmed.is_empty() {
        return Err(Error::OpenRouter("voice transcription empty".into()));
    }
    tracing::info!(
        model = voice_model,
        latency_ms = c.latency_ms,
        total_tokens = c.total_tokens,
        "voice transcribe ok"
    );
    Ok(trimmed)
}

fn trim_desc(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= DESC_MAX_CHARS {
        t.to_string()
    } else {
        t.chars().take(DESC_MAX_CHARS).collect::<String>() + "…"
    }
}
