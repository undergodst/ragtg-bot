use crate::deps::Deps;
use crate::llm::client::Message as LlmMessage;
use crate::llm::prompts::persona::CORE_PERSONA;
use crate::memory::{episodic, events, semantic, working::WorkingMessage};
use crate::tasks::{chat_dna, shots_seeder};

/// Assemble a 7-layer dynamic prompt for the bot's reply.
pub async fn assemble(
    deps: &Deps,
    chat_id: i64,
    query_vector: &Vec<f32>,
    window: &[WorkingMessage],
) -> Vec<LlmMessage> {
    let mut messages = Vec::new();

    // Layer 1: CORE_PERSONA (Static, cached at provider if possible)
    messages.push(LlmMessage::system(CORE_PERSONA));

    // Layer 2: CHAT_DNA (Synthesized vibe, Redis-cached for 24h)
    let dna = chat_dna::get_or_synthesize_dna(deps, chat_id).await;
    messages.push(LlmMessage::system(format!("[ДНК этого чата]:\n{dna}")));

    // Layer 3: CHAT_EVENTS_RAG (Significant past moments)
    let chat_events = events::retrieve_relevant(deps, chat_id, query_vector).await;
    if !chat_events.is_empty() {
        let mut ctx = String::from("[Релевантные моменты из прошлого]:\n");
        for e in chat_events {
            ctx.push_str(&format!("- {e}\n"));
        }
        messages.push(LlmMessage::system(ctx));
    }

    // Layer 4: EPISODIC_RAG (Last N summaries)
    let episodic_summaries = episodic::retrieve_relevant_summaries(deps, chat_id, query_vector).await;
    if !episodic_summaries.is_empty() {
        let mut ctx = String::from("[Контекст из истории чата]:\n");
        for s in episodic_summaries {
            ctx.push_str(&format!("- {s}\n"));
        }
        messages.push(LlmMessage::system(ctx));
    }

    // Layer 5: PEOPLE_IN_ROOM (Known facts about users in the current window)
    let user_facts = semantic::retrieve_facts_for_window_users(deps, chat_id, window, query_vector).await;
    if !user_facts.is_empty() {
        let mut ctx = String::from("[Факты об участниках]:\n");
        for (username, facts) in user_facts {
            ctx.push_str(&format!("@{username}:\n"));
            for f in facts {
                ctx.push_str(&format!("  - {f}\n"));
            }
        }
        messages.push(LlmMessage::system(ctx));
    }

    // Layer 6: STYLE_EXAMPLES (Few-shots by semantic similarity)
    let shots = shots_seeder::retrieve_relevant_shots(deps, query_vector.clone(), 3).await;
    if !shots.is_empty() {
        let mut ctx = String::from("[Примеры твоего стиля общения]:\n");
        for (context, reply) in shots {
            ctx.push_str(&format!("Контекст: {}\nТвой ответ: {}\n\n", context, reply));
        }
        messages.push(LlmMessage::system(ctx));
    }

    // Layer 7: ACTIVE_THREAD (Working window + current message is added by caller)
    for w in window {
        messages.push(LlmMessage::user(format_window_msg(w)));
    }

    messages
}

fn format_window_msg(w: &WorkingMessage) -> String {
    let username = w.username.as_deref().unwrap_or("unknown");
    let text = &w.text;
    if let Some(desc) = &w.media_desc {
        format!("@{username}: {text} [медиа: {desc}]")
    } else {
        format!("@{username}: {text}")
    }
}
