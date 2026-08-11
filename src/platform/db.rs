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
/// 判断 sqlx 错误是否为「撞了指定唯一约束」的 23505 冲突。
/// 只做判断不做映射——映射成哪个领域错误归各域自己。
pub fn is_unique_violation(e: &sqlx::Error, constraint: &str) -> bool {
    if let sqlx::Error::Database(db) = e {
        return db.code().as_deref() == Some("23505") && db.constraint() == Some(constraint);
    }
    false
}

/// 判断 sqlx 错误是否为指定外键约束的 23503；业务映射仍由各领域负责。
pub fn is_foreign_key_violation(e: &sqlx::Error, constraint: &str) -> bool {
    if let sqlx::Error::Database(db) = e {
        return db.code().as_deref() == Some("23503") && db.constraint() == Some(constraint);
    }
    false
}
