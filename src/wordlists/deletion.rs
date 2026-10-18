use super::service::Tx;
use crate::error::AppError;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use uuid::Uuid;

/// Deleted wordlists keep already-started learning runs usable for this long.
/// The deadline is persisted per deletion event, so changing this constant only
/// affects future deletions.
pub const GRACE_PERIOD_DAYS: i64 = 7;

#[derive(Clone, Copy)]
pub enum DeletionActor {
    User(Uuid),
    Admin(Uuid),
}

/// Persist the deletion fact and notify owners of active tasks that reference it.
/// This must run inside the same transaction that marks the wordlist deleted.
pub async fn record(
    tx: &mut Tx<'_>,
    wordlist_id: Uuid,
    deleted_at: DateTime<Utc>,
    actor: DeletionActor,
) -> Result<(), AppError> {
    let (name, prior_state): (String, String) =
        sqlx::query_as("SELECT name,state FROM wordlists WHERE id=$1")
            .bind(wordlist_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    let grace_until = if prior_state == "published" {
        deleted_at + Duration::days(GRACE_PERIOD_DAYS)
    } else {
        deleted_at + Duration::microseconds(1)
    };
    let (deleted_by_user_id, deleted_by_admin_id) = match actor {
        DeletionActor::User(id) => (Some(id), None),
        DeletionActor::Admin(id) => (None, Some(id)),
    };
    sqlx::query(
        "INSERT INTO wordlist_deletion_events(id,wordlist_id,deleted_at,grace_until,prior_state,deleted_by_user_id,deleted_by_admin_id) VALUES($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(wordlist_id)
    .bind(deleted_at)
    .bind(grace_until)
    .bind(&prior_state)
    .bind(deleted_by_user_id)
    .bind(deleted_by_admin_id)
    .execute(&mut **tx)
    .await
    .map_err(AppError::internal)?;

    let deadline = grace_until
        .with_timezone(&FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8"))
        .format("%Y-%m-%d %H:%M");
    let actor_user_id = match actor {
        DeletionActor::User(id) => Some(id),
        DeletionActor::Admin(_) => None,
    };
    // Only a wordlist that was still public at deletion can grant grace. A
    // withdrawn or already-private source has already invalidated its runs.
    let notified = if prior_state == "published" {
        let reason = format!(
            "词表「{name}」已被删除。已开始的学习轮次请在本轮正常截止前完成：日任务仍受当天窗口限制，长期任务最晚可在 {deadline}（北京时间）前完成。完成后任务将结束；未开始的轮次无法再开始。"
        );
        sqlx::query(
            "INSERT INTO user_notifications(id,user_id,kind,wordlist_id,actor_user_id,reason) SELECT gen_random_uuid(),u.user_id,'wordlist_deleted',$1,$2,$3 FROM (SELECT DISTINCT user_id FROM learning_tasks WHERE state='active' AND $1=ANY(wordlist_ids)) u WHERE $4::uuid IS NULL OR u.user_id<>$4",
        )
        .bind(wordlist_id)
        .bind(actor_user_id)
        .bind(&reason)
        .bind(actor_user_id)
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?
        .rows_affected()
    } else {
        0
    };
    tracing::info!(
        wordlist_id = %wordlist_id,
        prior_state,
        grace_until = %grace_until,
        notified,
        "recorded wordlist deletion"
    );
    Ok(())
}
