use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::prompts::summary::DNA_PROMPT;
use crate::storage::redis as redis_store;

const DNA_TTL_SEC: u64 = 86_400; // 24h
const DNA_LOOKBACK_SUMMARIES: i64 = 20;

/// Get chat DNA from Redis, or synthesize it if missing.
pub async fn get_or_synthesize_dna(deps: &Deps, chat_id: i64) -> String {
    match redis_store::get_chat_dna(&deps.redis, chat_id).await {
        Ok(Some(dna)) => dna,
        Ok(None) => {
            tracing::info!(chat_id, "chat DNA miss; synthesizing...");
            match synthesize_dna(deps, chat_id).await {
                Ok(dna) => {
                    let _ = redis_store::set_chat_dna(&deps.redis, chat_id, &dna, DNA_TTL_SEC).await;
                    dna
                }
                Err(e) => {
                    tracing::warn!(error = %e, chat_id, "failed to synthesize chat DNA");
                    "Активный чат, где обсуждают разное.".to_string()
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, chat_id, "redis error while getting DNA");
            "Активный чат, где обсуждают разное.".to_string()
        }
    }
}

async fn synthesize_dna(deps: &Deps, chat_id: i64) -> anyhow::Result<String> {
    // Fetch last 20 summaries
    let summaries = sqlx::query_scalar!(
        r#"SELECT text FROM episodic_summaries
           WHERE chat_id = ?
           ORDER BY created_at DESC
           LIMIT ?"#,
        chat_id,
        DNA_LOOKBACK_SUMMARIES
    )
    .fetch_all(&deps.sqlite)
    .await?;

    if summaries.is_empty() {
        return Ok("Новый чат без истории.".to_string());
    }

    let formatted = summaries.into_iter().map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n");
    let messages = vec![
        LlmMessage::system(DNA_PROMPT),
        LlmMessage::user(formatted),
    ];

    let model = deps.config.openrouter.model_main.clone();
    let completion = deps
        .openrouter
        .chat_completion("chat_dna", &model, &messages, 200)
        .await?;

    Ok(completion.content.trim().to_string())
}
