use deadpool_redis::{Config as RedisCfg, Pool, Runtime};

use crate::error::{Error, Result};

pub fn init_pool(url: &str) -> Result<Pool> {
    let cfg = RedisCfg::from_url(url);
    cfg.create_pool(Some(Runtime::Tokio1))
        .map_err(|e| Error::Redis(format!("create_pool: {e}")))
}

pub async fn healthcheck(pool: &Pool) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let pong: String = deadpool_redis::redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("PING: {e}")))?;
    if pong != "PONG" {
        return Err(Error::Redis(format!("unexpected PING response: {pong}")));
    }
    Ok(())
}

/// Per-user LLM cooldown: at most one reply per `cooldown_sec` window.
/// Implemented via `SET key 1 NX EX cooldown_sec` — the atomic
/// "set-if-not-exists" answer is whether the user was eligible right now.
/// Returns `Ok(true)` if the call should proceed (cooldown freshly set),
/// `Ok(false)` if the user is still on cooldown (silently skip the reply).
/// `cooldown_sec = 0` disables the gate (mirrors `working_window_size = 0`).
pub async fn check_user_cooldown(pool: &Pool, user_id: i64, cooldown_sec: u32) -> Result<bool> {
    if cooldown_sec == 0 {
        return Ok(true);
    }
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("rl:user:{user_id}");
    let resp: Option<String> = deadpool_redis::redis::cmd("SET")
        .arg(&key)
        .arg(1)
        .arg("NX")
        .arg("EX")
        .arg(cooldown_sec as u64)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("SET NX EX: {e}")))?;
    Ok(resp.as_deref() == Some("OK"))
}

/// Per-chat LLM quota: at most `max_per_min` replies per rolling-ish minute.
/// Implemented as a fixed-window counter (`INCR key`, set `EXPIRE 60` on the
/// first hit). Slightly stricter than a true sliding window under bursts —
/// fine for "don't burn OpenRouter credits" semantics.
/// `max_per_min = 0` disables the gate.
pub async fn check_chat_quota(pool: &Pool, chat_id: i64, max_per_min: u32) -> Result<bool> {
    if max_per_min == 0 {
        return Ok(true);
    }
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("rl:chat:{chat_id}");
    let count: i64 = deadpool_redis::redis::cmd("INCR")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("INCR: {e}")))?;
    if count == 1 {
        let _: i64 = deadpool_redis::redis::cmd("EXPIRE")
            .arg(&key)
            .arg(60)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Redis(format!("EXPIRE: {e}")))?;
    }
    Ok(count <= max_per_min as i64)
}

#[cfg(test)]
mod tests {
    //! Live-Redis integration tests, skipped when Redis is unreachable.

    use super::*;
    use deadpool_redis::redis::AsyncCommands;

    fn redis_url() -> String {
        std::env::var("WORKING_MEMORY_TEST_REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".into())
    }

    async fn pool_or_skip() -> Option<Pool> {
        let pool = init_pool(&redis_url()).ok()?;
        if healthcheck(&pool).await.is_err() {
            eprintln!("redis unreachable; skipping test");
            return None;
        }
        Some(pool)
    }

    async fn del(pool: &Pool, key: &str) {
        if let Ok(mut conn) = pool.get().await {
            let _: i64 = conn.del::<_, i64>(key).await.unwrap_or(0);
        }
    }

    #[tokio::test]
    async fn user_cooldown_blocks_second_call_within_window() {
        let Some(pool) = pool_or_skip().await else { return };
        let user_id: i64 = -777_001;
        del(&pool, &format!("rl:user:{user_id}")).await;

        assert!(check_user_cooldown(&pool, user_id, 30).await.expect("first"));
        assert!(!check_user_cooldown(&pool, user_id, 30).await.expect("second"));
        del(&pool, &format!("rl:user:{user_id}")).await;
    }

    #[tokio::test]
    async fn user_cooldown_zero_is_disabled() {
        let Some(pool) = pool_or_skip().await else { return };
        let user_id: i64 = -777_002;
        del(&pool, &format!("rl:user:{user_id}")).await;

        for _ in 0..5 {
            assert!(check_user_cooldown(&pool, user_id, 0).await.expect("call"));
        }
    }

    #[tokio::test]
    async fn chat_quota_blocks_after_max() {
        let Some(pool) = pool_or_skip().await else { return };
        let chat_id: i64 = -777_003;
        del(&pool, &format!("rl:chat:{chat_id}")).await;

        for i in 0..10 {
            assert!(
                check_chat_quota(&pool, chat_id, 10).await.expect("call"),
                "call #{i} must pass"
            );
        }
        assert!(!check_chat_quota(&pool, chat_id, 10).await.expect("11th"));
        assert!(!check_chat_quota(&pool, chat_id, 10).await.expect("12th"));
        del(&pool, &format!("rl:chat:{chat_id}")).await;
    }

    #[tokio::test]
    async fn chat_quota_zero_is_disabled() {
        let Some(pool) = pool_or_skip().await else { return };
        let chat_id: i64 = -777_004;
        del(&pool, &format!("rl:chat:{chat_id}")).await;

        for _ in 0..20 {
            assert!(check_chat_quota(&pool, chat_id, 0).await.expect("call"));
        }
    }

    #[tokio::test]
    async fn chat_quota_sets_expire_on_first_hit() {
        let Some(pool) = pool_or_skip().await else { return };
        let chat_id: i64 = -777_005;
        let key = format!("rl:chat:{chat_id}");
        del(&pool, &key).await;

        assert!(check_chat_quota(&pool, chat_id, 10).await.expect("first"));
        let mut conn = pool.get().await.expect("conn");
        let ttl: i64 = conn.ttl(&key).await.expect("ttl");
        assert!((1..=60).contains(&ttl), "unexpected TTL {ttl}");
        del(&pool, &key).await;
    }
}
