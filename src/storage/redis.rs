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

/// Rate limit specifically for the /ask command.
/// Returns 0 if allowed (and sets the cooldown), or the remaining TTL in seconds if not.
pub async fn check_ask_cooldown(pool: &Pool, user_id: i64, cooldown_sec: u64) -> Result<u64> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("rl:ask:{user_id}");
    
    // Check TTL
    let ttl: i64 = deadpool_redis::redis::cmd("TTL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("TTL: {e}")))?;

    if ttl > 0 {
        return Ok(ttl as u64);
    }

    // Set cooldown
    let _: () = deadpool_redis::redis::cmd("SET")
        .arg(&key)
        .arg(1)
        .arg("EX")
        .arg(cooldown_sec)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("SET EX: {e}")))?;

    Ok(0)
}

/// SHA256-keyed cache of media descriptions (image/voice/circle output from
/// the perception sub-agent). 30-day TTL by default — same media files
/// (memes, recurring forwards) often repeat in chats and re-running vision
/// LLM on every appearance is the single biggest waste avoidable here.
/// Returns `Ok(None)` on miss.
pub async fn get_media_desc(pool: &Pool, sha256_hex: &str) -> Result<Option<String>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("media:{sha256_hex}");
    let v: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("GET {key}: {e}")))?;
    Ok(v)
}

pub async fn put_media_desc(
    pool: &Pool,
    sha256_hex: &str,
    desc: &str,
    ttl_days: u32,
) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("media:{sha256_hex}");
    let ttl_seconds = (ttl_days as u64) * 86_400;
    deadpool_redis::redis::cmd("SET")
        .arg(&key)
        .arg(desc)
        .arg("EX")
        .arg(ttl_seconds)
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("SET {key}: {e}")))?;
    Ok(())
}

const VISION_SLOT_KEY: &str = "vision:slots";
/// Guard TTL on the slot counter. If a worker panics between acquire and
/// release the counter would otherwise stay inflated forever; with a TTL
/// it self-heals after this many seconds. Should comfortably exceed the
/// longest vision call (OpenRouter timeout × retries).
const VISION_SLOT_TTL_SEC: u64 = 300;

/// Try to take one vision-pipeline slot. Returns `Ok(true)` if acquired
/// (caller MUST call `release_vision_slot` on every exit path), `Ok(false)`
/// if `max` slots are already busy.
///
/// Implementation: `INCR`, then if the post-increment count exceeds `max`,
/// roll back via `DECR`. Brief over-counting under bursty contention is
/// acceptable (a few short-lived false-busy responses, never the opposite).
/// `EXPIRE` is set on the first hit so a crashed process can't pin the
/// counter at the cap forever — see `VISION_SLOT_TTL_SEC`.
pub async fn acquire_vision_slot(pool: &Pool, max: u32) -> Result<bool> {
    if max == 0 {
        return Ok(true);
    }
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;

    let count: i64 = deadpool_redis::redis::cmd("INCR")
        .arg(VISION_SLOT_KEY)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("INCR vision: {e}")))?;
    if count == 1 {
        let _: i64 = deadpool_redis::redis::cmd("EXPIRE")
            .arg(VISION_SLOT_KEY)
            .arg(VISION_SLOT_TTL_SEC)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Redis(format!("EXPIRE vision: {e}")))?;
    }
    if count > max as i64 {
        let _: i64 = deadpool_redis::redis::cmd("DECR")
            .arg(VISION_SLOT_KEY)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Redis(format!("DECR vision rollback: {e}")))?;
        return Ok(false);
    }
    Ok(true)
}

pub async fn release_vision_slot(pool: &Pool) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let count: i64 = deadpool_redis::redis::cmd("DECR")
        .arg(VISION_SLOT_KEY)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("DECR vision: {e}")))?;
    // A negative counter means a release happened without a paired acquire
    // (or the TTL expired and reset the counter mid-flight). Force back to
    // zero so future acquires see a sane baseline.
    if count < 0 {
        let _: i64 = deadpool_redis::redis::cmd("SET")
            .arg(VISION_SLOT_KEY)
            .arg(0)
            .arg("EX")
            .arg(VISION_SLOT_TTL_SEC)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Redis(format!("SET vision reset: {e}")))?;
    }
    Ok(())
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

/// Increment the per-chat episodic message counter. Returns the new count.
/// The summarization task checks `count >= episodic_summary_every_n` to
/// decide whether to run. No TTL — the counter lives until explicitly
/// reset after a successful summarization.
pub async fn incr_episodic_counter(pool: &Pool, chat_id: i64) -> Result<i64> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("episodic:counter:{chat_id}");
    let count: i64 = deadpool_redis::redis::cmd("INCR")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("INCR episodic counter: {e}")))?;
    Ok(count)
}

/// Reset the per-chat episodic message counter (after a successful summarization).
pub async fn reset_episodic_counter(pool: &Pool, chat_id: i64) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("episodic:counter:{chat_id}");
    deadpool_redis::redis::cmd("DEL")
        .arg(&key)
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("DEL episodic counter: {e}")))?;
    Ok(())
}

/// Increment the per-user per-chat facts extraction counter. Returns the new count.
pub async fn incr_facts_counter(pool: &Pool, chat_id: i64, user_id: i64) -> Result<i64> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("facts:counter:{chat_id}:{user_id}");
    let count: i64 = deadpool_redis::redis::cmd("INCR")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("INCR facts counter: {e}")))?;
    Ok(count)
}

/// Reset the per-user per-chat facts extraction counter.
pub async fn reset_facts_counter(pool: &Pool, chat_id: i64, user_id: i64) -> Result<()> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = format!("facts:counter:{chat_id}:{user_id}");
    deadpool_redis::redis::cmd("DEL")
        .arg(&key)
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("DEL facts counter: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// chat_events candidate buffer + dedup-set
// ---------------------------------------------------------------------------

const EVENT_CANDIDATES_TTL_SEC: i64 = 86_400; // 1 day
const EVENT_DEDUP_SET_TTL_SEC: i64 = 3_600; // 1 hour

fn event_candidates_key(chat_id: i64) -> String {
    format!("chat:{chat_id}:event_candidates")
}

fn event_dedup_set_key(chat_id: i64) -> String {
    format!("chat:{chat_id}:event_dedup")
}

/// Append a JSON-serialised candidate to the chat's event-candidate list.
/// Pipeline: RPUSH + EXPIRE in one round-trip. Returns the new list length.
pub async fn push_event_candidate(pool: &Pool, chat_id: i64, payload: &str) -> Result<i64> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = event_candidates_key(chat_id);
    let (len, _): (i64, ()) = deadpool_redis::redis::pipe()
        .atomic()
        .rpush(&key, payload)
        .expire(&key, EVENT_CANDIDATES_TTL_SEC)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("RPUSH event candidate: {e}")))?;
    Ok(len)
}

/// Atomically read all candidates and clear the buffer (LRANGE 0 -1 + DEL,
/// pipelined). Returns oldest-first.
pub async fn pop_event_candidates(pool: &Pool, chat_id: i64) -> Result<Vec<String>> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = event_candidates_key(chat_id);
    let (raw, _): (Vec<String>, i64) = deadpool_redis::redis::pipe()
        .atomic()
        .lrange(&key, 0, -1)
        .del(&key)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("pop event candidates: {e}")))?;
    Ok(raw)
}

/// Re-push candidates back onto the buffer (e.g. after a scoring failure).
/// Uses LPUSH so they land oldest-first relative to whatever else has come
/// in since pop_event_candidates ran.
pub async fn requeue_event_candidates(
    pool: &Pool,
    chat_id: i64,
    payloads: &[String],
) -> Result<()> {
    if payloads.is_empty() {
        return Ok(());
    }
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = event_candidates_key(chat_id);
    let mut pipe = deadpool_redis::redis::pipe();
    pipe.atomic();
    // Reverse order: LPUSHing in original order would invert; LPUSH each
    // from end-to-start preserves chronological order.
    for p in payloads.iter().rev() {
        pipe.lpush(&key, p);
    }
    pipe.expire(&key, EVENT_CANDIDATES_TTL_SEC);
    pipe.query_async::<()>(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("requeue event candidates: {e}")))?;
    Ok(())
}

/// Current candidate buffer length (read-only LLEN).
pub async fn len_event_candidates(pool: &Pool, chat_id: i64) -> Result<i64> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = event_candidates_key(chat_id);
    let len: i64 = deadpool_redis::redis::cmd("LLEN")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("LLEN event candidates: {e}")))?;
    Ok(len)
}

/// Returns `true` if `hash` was newly added to the chat's recent-text set,
/// `false` if it was already there. Uses SADD; refreshes TTL on every call
/// so the rolling 1h window stays alive in active chats.
pub async fn record_unique_event_hash(pool: &Pool, chat_id: i64, hash: &str) -> Result<bool> {
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Error::Redis(format!("get conn: {e}")))?;
    let key = event_dedup_set_key(chat_id);
    let (added, _): (i64, ()) = deadpool_redis::redis::pipe()
        .atomic()
        .sadd(&key, hash)
        .expire(&key, EVENT_DEDUP_SET_TTL_SEC)
        .query_async(&mut conn)
        .await
        .map_err(|e| Error::Redis(format!("record unique event hash: {e}")))?;
    Ok(added == 1)
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
    async fn media_desc_round_trip() {
        let Some(pool) = pool_or_skip().await else { return };
        let sha = "deadbeefcafebabe1111";
        del(&pool, &format!("media:{sha}")).await;

        assert!(get_media_desc(&pool, sha).await.expect("get miss").is_none());
        put_media_desc(&pool, sha, "котик с подписью «соси»", 30)
            .await
            .expect("put");
        let got = get_media_desc(&pool, sha).await.expect("get hit");
        assert_eq!(got.as_deref(), Some("котик с подписью «соси»"));
        del(&pool, &format!("media:{sha}")).await;
    }

    #[tokio::test]
    async fn vision_slot_acquire_release_cycle() {
        let Some(pool) = pool_or_skip().await else { return };
        del(&pool, VISION_SLOT_KEY).await;

        // Take 5 of 5.
        for i in 0..5 {
            assert!(
                acquire_vision_slot(&pool, 5).await.expect("acq"),
                "slot {i} must be free"
            );
        }
        // 6th must be denied.
        assert!(!acquire_vision_slot(&pool, 5).await.expect("acq full"));
        // After releasing one, a new acquire succeeds.
        release_vision_slot(&pool).await.expect("rel");
        assert!(acquire_vision_slot(&pool, 5).await.expect("acq after rel"));
        // Cleanup remaining 5.
        for _ in 0..5 {
            release_vision_slot(&pool).await.expect("rel");
        }
        del(&pool, VISION_SLOT_KEY).await;
    }

    #[tokio::test]
    async fn vision_slot_zero_max_disables() {
        let Some(pool) = pool_or_skip().await else { return };
        del(&pool, VISION_SLOT_KEY).await;
        for _ in 0..20 {
            assert!(acquire_vision_slot(&pool, 0).await.expect("acq disabled"));
        }
        // No DECRs needed — `max=0` short-circuits before INCR.
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
