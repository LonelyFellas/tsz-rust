use crate::{
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
};
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

const UPLOAD_LIMITS_SQL: &str = "SELECT (SELECT count(*) FROM (SELECT 1 FROM avatar_uploads WHERE user_id=$1 AND state='pending' AND expires_at>statement_timestamp() LIMIT 3) pending),(SELECT count(*) FROM (SELECT 1 FROM avatar_uploads WHERE user_id=$1 AND created_at>statement_timestamp()-interval '1 minute' LIMIT 3) recent)";

#[derive(FromRow)]
pub struct Upload {
    pub id: Uuid,
    pub source_key: String,
    pub declared_type: String,
    pub size_bytes: i64,
    pub state: String,
    baseline_frozen: bool,
    baseline_avatar_upload_id: Option<Uuid>,
}

pub fn invalid_key() -> AppError {
    AppError::validation(ErrorCode::InvalidAvatarKey, "key", "invalid avatar key")
}

pub async fn lock_user(connection: &mut PgConnection, auth: &AuthUser) -> Result<(), AppError> {
    let found = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE id=$1 AND status='active' AND security_version=$2 FOR UPDATE",
    )
    .bind(auth.subject)
    .bind(auth.security_version)
    .fetch_optional(connection)
    .await
    .map_err(AppError::internal)?;
    if found.is_none() {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    Ok(())
}

pub async fn create(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    key: &str,
    request: &super::dto::AvatarUploadRequest,
    ttl: i64,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let (pending, recent): (i64, i64) = sqlx::query_as(UPLOAD_LIMITS_SQL)
        .bind(auth.subject)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    if pending >= 3 || recent >= 3 {
        return Err(AppError::rate_limited(
            ErrorCode::AvatarUploadRateLimited,
            "avatar upload rate limited",
        ));
    }
    sqlx::query("INSERT INTO avatar_uploads (id,user_id,source_key,declared_type,size_bytes,expires_at) VALUES ($1,$2,$3,$4,$5,clock_timestamp()+make_interval(secs=>$6))")
        .bind(id).bind(auth.subject).bind(key).bind(&request.content_type).bind(request.size as i64).bind(ttl as f64).execute(&mut *tx).await.map_err(AppError::internal)?;
    sqlx::query("INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before) VALUES ($1,$2,$3,'source',clock_timestamp()+make_interval(secs=>$4)+interval '150 seconds')")
        .bind(Uuid::now_v7()).bind(id).bind(key).bind(ttl as f64).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)
}

pub async fn signed(
    pool: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    expires_at: DateTime<Utc>,
) -> Result<u64, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let valid = sqlx::query_scalar::<_,Uuid>("SELECT id FROM avatar_uploads WHERE id=$1 AND user_id=$2 AND state='pending' AND expires_at>clock_timestamp() FOR UPDATE")
        .bind(id).bind(auth.subject).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if valid.is_none() {
        return Err(invalid_key());
    }
    let safe = sqlx::query_scalar::<_,Uuid>("SELECT id FROM avatar_cleanup_tasks WHERE upload_id=$1 AND kind='source' AND status='pending' AND attempts=0 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if safe.is_none() {
        return Err(invalid_key());
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT floor(EXTRACT(EPOCH FROM $1::timestamptz-clock_timestamp()))::bigint",
    )
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    if remaining < 1 {
        return Err(invalid_key());
    }
    sqlx::query("UPDATE avatar_uploads SET expires_at=$2 WHERE id=$1")
        .bind(id)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=GREATEST(not_before,$2::timestamptz+interval '120 seconds') WHERE upload_id=$1 AND kind='source'")
        .bind(id).bind(expires_at).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(remaining as u64)
}

pub async fn find(pool: &PgPool, auth: &AuthUser, key: &str) -> Result<Upload, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let mut upload = sqlx::query_as::<_,Upload>("SELECT id,source_key,declared_type,size_bytes,state,baseline_frozen,baseline_avatar_upload_id FROM avatar_uploads WHERE source_key=$1 AND user_id=$2 AND (state='confirmed' OR (state='pending' AND expires_at>clock_timestamp())) FOR UPDATE")
        .bind(key).bind(auth.subject).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(invalid_key)?;
    if upload.state == "pending" {
        let current: Option<Uuid> =
            sqlx::query_scalar("SELECT avatar_upload_id FROM users WHERE id=$1")
                .bind(auth.subject)
                .fetch_one(&mut *tx)
                .await
                .map_err(AppError::internal)?;
        if !upload.baseline_frozen {
            sqlx::query("UPDATE avatar_uploads SET baseline_frozen=true,baseline_avatar_upload_id=$2 WHERE id=$1")
                .bind(upload.id).bind(current).execute(&mut *tx).await.map_err(AppError::internal)?;
            upload.baseline_frozen = true;
            upload.baseline_avatar_upload_id = current;
        } else if upload.baseline_avatar_upload_id != current {
            sqlx::query("UPDATE avatar_uploads SET state='invalid' WHERE id=$1")
                .bind(upload.id)
                .execute(&mut *tx)
                .await
                .map_err(AppError::internal)?;
            tx.commit().await.map_err(AppError::internal)?;
            return Err(invalid_key());
        }
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(upload)
}

pub async fn register_candidate(
    pool: &PgPool,
    auth: &AuthUser,
    upload: Uuid,
    key: &str,
    task: Uuid,
) -> Result<bool, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let state=sqlx::query_scalar::<_,String>("SELECT state FROM avatar_uploads WHERE id=$1 AND user_id=$2 AND (state='confirmed' OR (state='pending' AND expires_at>clock_timestamp())) FOR UPDATE")
        .bind(upload).bind(auth.subject).fetch_optional(&mut *tx).await.map_err(AppError::internal)?.ok_or_else(invalid_key)?;
    if state == "confirmed" {
        tx.commit().await.map_err(AppError::internal)?;
        return Ok(false);
    }
    sqlx::query("INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before) VALUES ($1,$2,$3,'candidate',clock_timestamp()+interval '150 seconds')")
        .bind(task).bind(upload).bind(key).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(true)
}

pub async fn commit_avatar_in(
    connection: &mut PgConnection,
    auth: &AuthUser,
    upload: Uuid,
    task: Uuid,
    key: &str,
    url: &str,
) -> Result<bool, AppError> {
    lock_user(connection, auth).await?;
    let (state, valid, frozen, baseline): (String, bool, bool, Option<Uuid>) = sqlx::query_as(
        "SELECT state,expires_at>clock_timestamp(),baseline_frozen,baseline_avatar_upload_id FROM avatar_uploads WHERE id=$1 AND user_id=$2 FOR UPDATE",
    )
    .bind(upload)
    .bind(auth.subject)
    .fetch_optional(&mut *connection)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(invalid_key)?;
    if state == "confirmed" {
        return Ok(true);
    }
    if state != "pending" || !valid || !frozen {
        return Err(invalid_key());
    }
    let current: Option<Uuid> =
        sqlx::query_scalar("SELECT avatar_upload_id FROM users WHERE id=$1")
            .bind(auth.subject)
            .fetch_one(&mut *connection)
            .await
            .map_err(AppError::internal)?;
    if current != baseline {
        sqlx::query("UPDATE avatar_uploads SET state='invalid' WHERE id=$1")
            .bind(upload)
            .execute(connection)
            .await
            .map_err(AppError::internal)?;
        return Ok(false);
    }
    let candidate=sqlx::query_as::<_,(String,i32)>("SELECT status,attempts FROM avatar_cleanup_tasks WHERE id=$1 AND upload_id=$2 AND object_key=$3 AND kind='candidate' FOR UPDATE")
        .bind(task).bind(upload).bind(key).fetch_optional(&mut *connection).await.map_err(AppError::internal)?;
    if candidate != Some(("pending".into(), 0)) {
        return Err(AppError::conflict(
            ErrorCode::AvatarUploadNotCompleted,
            Some("key"),
            "avatar upload not completed",
        ));
    }
    sqlx::query("INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before,write_completed) SELECT $1,a.id,a.canonical_key,'canonical',now(),true FROM avatar_uploads a JOIN users u ON u.avatar_upload_id=a.id WHERE u.id=$2 AND a.canonical_key IS NOT NULL ON CONFLICT (object_key,kind) DO NOTHING")
        .bind(Uuid::now_v7()).bind(auth.subject).execute(&mut *connection).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE avatar_uploads SET state='confirmed',canonical_key=$2,confirmed_at=now() WHERE id=$1")
        .bind(upload).bind(key).execute(&mut *connection).await.map_err(AppError::internal)?;
    sqlx::query("UPDATE users SET avatar_upload_id=$2,avatar_url=$3 WHERE id=$1")
        .bind(auth.subject)
        .bind(upload)
        .bind(url)
        .execute(&mut *connection)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("UPDATE avatar_cleanup_tasks SET status='cancelled' WHERE id=$1")
        .bind(task)
        .execute(connection)
        .await
        .map_err(AppError::internal)?;
    Ok(true)
}

pub async fn mark_candidate_complete(pool: &PgPool, task: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE avatar_cleanup_tasks SET write_completed=true,status=CASE WHEN status='done' THEN 'pending' ELSE status END,not_before=CASE WHEN status='done' THEN clock_timestamp() ELSE not_before END WHERE id=$1 AND kind='candidate' AND status<>'cancelled' AND NOT write_completed")
        .bind(task).execute(pool).await.map_err(AppError::internal)?;
    Ok(())
}

pub async fn schedule_user_cleanup_in(
    connection: &mut PgConnection,
    user: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT avatar_schedule_user_cleanup($1)")
        .bind(user)
        .execute(connection)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned_rows(node: &serde_json::Value) -> u64 {
        let here = if node["Node Type"]
            .as_str()
            .is_some_and(|kind| kind.contains("Scan"))
        {
            node["Actual Rows"]
                .as_f64()
                .expect("scan must report actual rows")
                .ceil() as u64
                + node["Rows Removed by Filter"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .ceil() as u64
        } else {
            0
        };
        here + node["Plans"]
            .as_array()
            .map_or(0, |children| children.iter().map(scanned_rows).sum())
    }

    #[sqlx::test]
    async fn quota_check_does_not_scan_old_upload_history(pool: PgPool) {
        let user = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO users (id,email,password_hash,display_name) VALUES ($1,$2,'hash','Quota')",
        )
        .bind(user)
        .bind(format!("{user}@example.test"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO avatar_uploads (id,user_id,source_key,declared_type,size_bytes,expires_at,state,created_at) SELECT gen_random_uuid(),$1,'uploads/history/'||i||'.png','image/png',1,now()-interval '1 day','invalid',now()-interval '1 day' FROM generate_series(1,10000) i")
            .bind(user).execute(&pool).await.unwrap();
        sqlx::raw_sql("ANALYZE avatar_uploads")
            .execute(&pool)
            .await
            .unwrap();
        let counts: (i64, i64) = sqlx::query_as(UPLOAD_LIMITS_SQL)
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(counts, (0, 0));
        let mut explain =
            sqlx::QueryBuilder::<sqlx::Postgres>::new("EXPLAIN (ANALYZE, FORMAT JSON) ");
        explain.push(UPLOAD_LIMITS_SQL);
        let plan: serde_json::Value = explain
            .build_query_scalar()
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
        let root = plan.as_array().expect("EXPLAIN must return an array")[0]
            .get("Plan")
            .expect("EXPLAIN must include Plan");
        let work = scanned_rows(root);
        assert!(work < 100, "quota query scanned {work} old rows");
    }
}
