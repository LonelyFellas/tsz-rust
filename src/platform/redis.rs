use deadpool_redis::{Config, Pool, Runtime};

pub async fn connect(redis_url: &str) -> anyhow::Result<Pool> {
    let cfg = Config::from_url(redis_url);
    let pool = cfg.create_pool(Some(Runtime::Tokio1))?;
    Ok(pool)
}
