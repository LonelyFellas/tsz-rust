use deadpool_redis::{Config, Hook, Pool, Runtime};
use std::time::Duration;

pub async fn connect(redis_url: &str) -> anyhow::Result<Pool> {
    let cfg = Config::from_url(redis_url);
    let budget = Duration::from_secs(3);
    Ok(cfg
        .builder()?
        .runtime(Runtime::Tokio1)
        .wait_timeout(Some(budget))
        .create_timeout(Some(budget))
        .recycle_timeout(Some(budget))
        .post_create(Hook::sync_fn(move |connection, _| {
            connection.set_response_timeout(budget);
            Ok(())
        }))
        .build()?)
}
