use deadpool_redis::Pool;
use deadpool_redis::redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One entry in the chat's working-memory window. Goes into Redis as JSON,
/// gets read back into prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingMessage {
    pub user_id: i64,
    pub username: Option<String>,
    pub text: String,
    pub media_desc: Option<String>,
    pub ts: i64,
}

fn key(chat_id: i64) -> String {
    format!("chat:{chat_id}:window")
}

/// Push a message onto the chat's working window: LPUSH new entry, LTRIM to
/// `window_size`, EXPIRE the key. All three commands are pipelined into one
/// round-trip.
pub async fn push(
    pool: &Pool,
    chat_id: i64,
    msg: &WorkingMessage,
    window_size: u32,
    ttl_days: u32,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;

    let payload = serde_json::to_string(msg)?;
    let key = key(chat_id);
    let trim_to = window_size.saturating_sub(1) as isize;
    let ttl_seconds = (ttl_days as i64) * 86_400;

    deadpool_redis::redis::pipe()
        .atomic()
        .lpush(&key, payload)
        .ignore()
        .ltrim(&key, 0, trim_to)
        .ignore()
        .expire(&key, ttl_seconds)
        .ignore()
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("pipeline: {e}")))?;

    Ok(())
}

/// Read the last `n` messages from the chat window, **chronological** order
/// (oldest first), so they can be fed straight into a prompt.
pub async fn get_window(pool: &Pool, chat_id: i64, n: u32) -> Result<Vec<WorkingMessage>> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;

    let raw: Vec<String> = conn
        .lrange(&key(chat_id), 0, (n as isize) - 1)
        .await
        .map_err(|e| Error::Redis(format!("LRANGE: {e}")))?;

    // LRANGE returns newest-first (we LPUSH each new message); reverse to
    // chronological for prompt consumption.
    let mut out: Vec<WorkingMessage> = raw
        .into_iter()
        .map(|s| serde_json::from_str(&s))
        .collect::<std::result::Result<_, _>>()?;
    out.reverse();
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! Live-Redis integration tests. Skipped when Redis is unreachable
    //! (so unit-test runs on a fresh dev box don't fail spuriously).

    use super::*;
    use deadpool_redis::redis::AsyncCommands;

    fn redis_url() -> String {
        std::env::var("WORKING_MEMORY_TEST_REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".into())
    }

    async fn pool_or_skip() -> Option<deadpool_redis::Pool> {
        let url = redis_url();
        let pool = crate::storage::redis::init_pool(&url).ok()?;
        if crate::storage::redis::healthcheck(&pool).await.is_err() {
            eprintln!("redis at {url} unreachable; skipping test");
            return None;
        }
        Some(pool)
    }

    fn mk(user_id: i64, text: &str, ts: i64) -> WorkingMessage {
        WorkingMessage {
            user_id,
            username: Some(format!("user{user_id}")),
            text: text.to_string(),
            media_desc: None,
            ts,
        }
    }

    async fn cleanup(pool: &deadpool_redis::Pool, chat_id: i64) {
        if let Ok(mut conn) = pool.get().await {
            let _: i64 = conn.del::<_, i64>(key(chat_id)).await.unwrap_or(0);
        }
    }

    #[tokio::test]
    async fn push_then_get_returns_chronological_order() {
        let Some(pool) = pool_or_skip().await else {
            return;
        };
        let chat_id: i64 = -100_000_000_001;
        cleanup(&pool, chat_id).await;

        for i in 0..5_i64 {
            push(&pool, chat_id, &mk(i, &format!("hello {i}"), 1000 + i), 30, 7)
                .await
                .expect("push");
        }

        let window = get_window(&pool, chat_id, 30).await.expect("get");
        assert_eq!(window.len(), 5);
        for (i, m) in window.iter().enumerate() {
            assert_eq!(m.text, format!("hello {i}"));
            assert_eq!(m.user_id, i as i64);
            assert_eq!(m.ts, 1000 + i as i64);
        }
        cleanup(&pool, chat_id).await;
    }

    #[tokio::test]
    async fn push_trims_to_window_size() {
        let Some(pool) = pool_or_skip().await else {
            return;
        };
        let chat_id: i64 = -100_000_000_002;
        cleanup(&pool, chat_id).await;

        for i in 0..35_i64 {
            push(&pool, chat_id, &mk(i, &format!("m{i}"), i), 30, 7)
                .await
                .expect("push");
        }

        let window = get_window(&pool, chat_id, 100).await.expect("get");
        assert_eq!(window.len(), 30, "should trim to window_size = 30");
        assert_eq!(window.first().unwrap().text, "m5");
        assert_eq!(window.last().unwrap().text, "m34");
        cleanup(&pool, chat_id).await;
    }

    #[tokio::test]
    async fn get_window_on_empty_returns_empty() {
        let Some(pool) = pool_or_skip().await else {
            return;
        };
        let chat_id: i64 = -100_000_000_003;
        cleanup(&pool, chat_id).await;
        let window = get_window(&pool, chat_id, 30).await.expect("get");
        assert!(window.is_empty());
    }

    #[tokio::test]
    async fn push_sets_expire() {
        let Some(pool) = pool_or_skip().await else {
            return;
        };
        let chat_id: i64 = -100_000_000_004;
        cleanup(&pool, chat_id).await;

        push(&pool, chat_id, &mk(1, "x", 1), 30, 7)
            .await
            .expect("push");

        let mut conn = pool.get().await.expect("conn");
        let ttl: i64 = conn.ttl(key(chat_id)).await.expect("ttl");
        // 7 days = 604_800 sec; allow some drift.
        assert!(
            (604_000..=604_800).contains(&ttl),
            "unexpected TTL {ttl}"
        );
        cleanup(&pool, chat_id).await;
    }
}
