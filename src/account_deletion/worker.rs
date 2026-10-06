use crate::error::AppError;
use sqlx::PgPool;
use uuid::Uuid;
/// The database is the durable queue; restarting or running another instance is safe.
pub async fn sweep(pool: &PgPool) -> Result<u64, AppError> {
    let candidates:Vec<(Uuid,Uuid)>=sqlx::query_as("SELECT user_id,id FROM account_deletion_requests WHERE status='pending' AND effective_at<=clock_timestamp() AND next_attempt_at<=clock_timestamp() ORDER BY next_attempt_at,effective_at,id LIMIT 100")
        .fetch_all(pool).await.map_err(AppError::internal)?;
    let mut completed = 0;
    for (user, id) in candidates {
        let result = super::service::complete(pool, user, id).await;
        match result {
            Ok(true) => {
                completed += 1;
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::error!(request_id=%id,error=%error,"account deletion completion failed; will retry")
            }
        }
        // Persist backoff so repeatedly busy/broken requests yield to later accounts,
        // including across restarts and multiple worker instances.
        sqlx::query("UPDATE account_deletion_requests SET next_attempt_at=clock_timestamp()+interval '60 seconds' WHERE id=$1 AND status='pending'")
            .bind(id).execute(pool).await.map_err(AppError::internal)?;
    }
    Ok(completed)
}
pub fn run_worker(pool: PgPool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = sweep(&pool).await {
                tracing::error!(error=%error,"account deletion sweep failed");
            }
        }
    });
}
