use std::time::Duration;

use sqlx::PgPool;

use super::repository::PreviewRepository;

const INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 周期性删除过期的试听缓存行。只删 row，不删 OSS 对象：
/// 对象由 bucket 生命周期规则按年龄回收，避免应用与规则重复承担同一个删除职责。
pub fn run_worker(pool: PgPool) {
    crate::safe_log::spawn_worker("speech/preview/cleanup", async move {
        let repository = PreviewRepository::new(pool);
        loop {
            match repository.delete_expired().await {
                Ok(0) => {}
                Ok(deleted) => tracing::info!(deleted, "speech preview cache rows expired"),
                Err(error) => tracing::error!(
                    error_kind = crate::safe_log::error_kind(&error),
                    operation = "speech_preview_cleanup",
                    "speech preview cache cleanup failed"
                ),
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}
