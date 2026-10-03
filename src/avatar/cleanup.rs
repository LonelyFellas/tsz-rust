use super::{IO_TIMEOUT, service::store};
use crate::{error::AppError, platform::storage::ObjectKey, state::AppState};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

const SLOTS: usize = 4;

#[derive(FromRow)]
pub struct CleanupClaim {
    pub id: Uuid,
    pub object_key: String,
    pub token: Uuid,
    write_completed: bool,
}

async fn claim_group(
    pool: &PgPool,
    token: Uuid,
    recheck: bool,
    limit: usize,
) -> Result<Vec<CleanupClaim>, AppError> {
    let query = if recheck {
        "WITH due AS (SELECT id FROM avatar_cleanup_tasks WHERE status='done' AND kind IN ('source','candidate') AND NOT write_completed AND not_before<=clock_timestamp() AND (lease_until IS NULL OR lease_until<=clock_timestamp()) ORDER BY not_before,id LIMIT $2 FOR UPDATE SKIP LOCKED) UPDATE avatar_cleanup_tasks t SET status='pending',attempts=t.attempts+1,lease_until=clock_timestamp()+interval '120 seconds',lease_token=$1 FROM due WHERE t.id=due.id RETURNING t.id,t.object_key,t.lease_token AS token,(t.write_completed OR t.kind='canonical') AS write_completed"
    } else {
        "WITH due AS (SELECT id FROM avatar_cleanup_tasks WHERE status='pending' AND not_before<=clock_timestamp() AND (lease_until IS NULL OR lease_until<=clock_timestamp()) ORDER BY not_before,id LIMIT $2 FOR UPDATE SKIP LOCKED) UPDATE avatar_cleanup_tasks t SET status='pending',attempts=t.attempts+1,lease_until=clock_timestamp()+interval '120 seconds',lease_token=$1 FROM due WHERE t.id=due.id RETURNING t.id,t.object_key,t.lease_token AS token,(t.write_completed OR t.kind='canonical') AS write_completed"
    };
    sqlx::query_as(query)
        .bind(token)
        .bind(limit as i64)
        .fetch_all(pool)
        .await
        .map_err(AppError::internal)
}

pub async fn claim(pool: &PgPool) -> Result<Vec<CleanupClaim>, AppError> {
    let token = Uuid::now_v7();
    let mut tasks = claim_group(pool, token, false, SLOTS - 1).await?;
    tasks.extend(claim_group(pool, token, true, 1).await?);
    if tasks.len() < SLOTS {
        tasks.extend(claim_group(pool, token, false, SLOTS - tasks.len()).await?);
    }
    if tasks.len() < SLOTS {
        tasks.extend(claim_group(pool, token, true, SLOTS - tasks.len()).await?);
    }
    Ok(tasks)
}

async fn process(state: AppState, task: CleanupClaim) -> Result<u64, AppError> {
    let active = sqlx::query("UPDATE avatar_cleanup_tasks SET lease_until=clock_timestamp()+interval '120 seconds' WHERE id=$1 AND lease_token=$2 AND status='pending' AND lease_until>clock_timestamp()")
        .bind(task.id).bind(task.token).execute(&state.pool).await.map_err(AppError::internal)?.rows_affected();
    if active == 0 {
        return Ok(0);
    }
    let referenced: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM avatar_uploads a JOIN users u ON u.avatar_upload_id=a.id AND u.id=a.user_id WHERE a.canonical_key=$1)")
        .bind(&task.object_key).fetch_one(&state.pool).await.map_err(AppError::internal)?;
    if referenced {
        retry(&state.pool, &task, "currently_referenced").await?;
        return Ok(0);
    }
    let key = ObjectKey::parse(&task.object_key).map_err(AppError::internal)?;
    let store = store(&state)?;
    if tokio::time::timeout(IO_TIMEOUT, store.delete(&key))
        .await
        .is_ok_and(|result| result.is_ok())
    {
        // 领取时未知的写入，不能因删除响应期间收到完成标记而直接终结。
        let changed = sqlx::query("UPDATE avatar_cleanup_tasks SET status=CASE WHEN $3 OR NOT write_completed THEN 'done' ELSE 'pending' END,lease_until=NULL,lease_token=NULL,last_error=NULL,not_before=CASE WHEN $3 THEN not_before WHEN write_completed THEN clock_timestamp() ELSE clock_timestamp()+interval '1 day' END WHERE id=$1 AND lease_token=$2 AND status='pending'")
            .bind(task.id).bind(task.token).bind(task.write_completed).execute(&state.pool).await.map_err(AppError::internal)?.rows_affected();
        Ok(changed)
    } else {
        retry(&state.pool, &task, "storage_unavailable").await?;
        Ok(0)
    }
}

pub async fn sweep(state: &AppState) -> Result<u64, AppError> {
    store(state)?;
    let tasks = claim(&state.pool).await?;
    let results =
        futures_util::future::join_all(tasks.into_iter().map(|task| process(state.clone(), task)))
            .await;
    results
        .into_iter()
        .try_fold(0, |count, result| result.map(|deleted| count + deleted))
}

async fn retry(pool: &PgPool, task: &CleanupClaim, code: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE avatar_cleanup_tasks SET lease_until=NULL,lease_token=NULL,not_before=clock_timestamp()+interval '60 seconds',last_error=$3 WHERE id=$1 AND lease_token=$2 AND status='pending'")
        .bind(task.id).bind(task.token).bind(code).execute(pool).await.map_err(AppError::internal)?;
    Ok(())
}

pub fn run_worker(state: AppState) {
    if store(&state).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            if sweep(&state).await.is_err() {
                tracing::warn!("avatar cleanup pass failed; will retry");
            }
        }
    });
}
