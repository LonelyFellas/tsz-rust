use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// 从连接串建连接池。参数（连接数/超时）先给保守默认值。
pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await?;
    Ok(pool)
}
