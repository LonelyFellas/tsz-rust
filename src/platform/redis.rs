use deadpool_redis::{Manager, Pool, Runtime, redis::AsyncConnectionConfig};
use std::time::Duration;

pub async fn connect(redis_url: &str) -> anyhow::Result<Pool> {
    let budget = Duration::from_secs(3);
    // 建连前设置客户端预算；post_create 无法覆盖内部默认的 1 秒握手超时。
    let connection = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(budget))
        .set_response_timeout(Some(budget));
    Ok(
        Pool::builder(Manager::new_with_config(redis_url, connection)?)
            .runtime(Runtime::Tokio1)
            .wait_timeout(Some(budget))
            .create_timeout(Some(budget))
            .recycle_timeout(Some(budget))
            .build()?,
    )
}
