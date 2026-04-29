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
