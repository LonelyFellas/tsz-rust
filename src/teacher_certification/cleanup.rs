use crate::{error::AppError, platform::storage::ObjectKey, state::AppState};
use uuid::Uuid;

pub async fn sweep(state: &AppState) -> Result<u64, AppError> {
    let store = super::files::store(state)?;
    let candidates = sqlx::query_as::<_, (Uuid, String)>(
        "WITH candidates AS (SELECT f.id FROM teacher_certification_files f WHERE ((f.user_id IS NULL AND f.state <> 'uploading') OR f.state = 'delete_pending' OR f.expires_at <= now()) AND NOT EXISTS(SELECT 1 FROM teacher_application_files a WHERE a.file_id = f.id) ORDER BY f.created_at LIMIT 50 FOR UPDATE SKIP LOCKED) UPDATE teacher_certification_files f SET state = 'delete_pending' FROM candidates c WHERE f.id = c.id RETURNING f.id, f.object_key",
    ).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    let mut removed = 0;
    for (id, key) in candidates {
        let key = ObjectKey::parse(key).map_err(AppError::internal)?;
        if store.delete(&key).await.is_err() {
            tracing::warn!(file_id = %id, "certification material cleanup will retry");
            continue;
        }
        removed += sqlx::query("DELETE FROM teacher_certification_files f WHERE id = $1 AND state = 'delete_pending' AND NOT EXISTS(SELECT 1 FROM teacher_application_files a WHERE a.file_id = f.id)")
            .bind(id).execute(&state.pool).await.map_err(AppError::internal)?.rows_affected();
    }
    Ok(removed)
}

pub fn run_worker(state: AppState) {
    if super::files::store(&state).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            if sweep(&state).await.is_err() {
                tracing::warn!("certification cleanup pass failed; will retry");
            }
        }
    });
}
