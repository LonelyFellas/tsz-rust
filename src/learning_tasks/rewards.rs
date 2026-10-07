use super::{dto::*, question::business_window, repository::*};
use crate::{
    auth::extract::AuthUser,
    coins::{
        model::{Actor, Amount, Context, Owner, OwnerType},
        service::credit_in,
    },
    error::AppError,
};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct Policy {
    id: Uuid,
    rule_version: String,
    enabled: bool,
    daily_amount: Option<i64>,
    minimum_units: Option<i32>,
}
async fn policy_lock(tx: &mut Tx<'_>) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared(72120400)")
        .execute(&mut **tx)
        .await
        .map_err(AppError::internal)?;
    Ok(())
}
async fn policy(tx: &mut Tx<'_>, day: NaiveDate) -> Result<Option<Policy>, AppError> {
    sqlx::query_as("SELECT id,rule_version,enabled,daily_amount,minimum_units FROM learning_reward_policies WHERE effective_business_day <= $1 ORDER BY effective_business_day DESC LIMIT 1")
        .bind(day).fetch_optional(&mut **tx).await.map_err(AppError::internal)
}
async fn units(tx: &mut Tx<'_>, user: Uuid, day: NaiveDate, limit: i32) -> Result<i32, AppError> {
    sqlx::query_scalar("SELECT count(*)::integer FROM (SELECT DISTINCT q.unit_key FROM learning_completions c JOIN learning_questions q ON q.run_id=c.run_id WHERE c.user_id=$1 AND c.business_day=$2 AND c.task_type='daily' LIMIT $3) units")
        .bind(user).bind(day).bind(limit).fetch_one(&mut **tx).await.map_err(AppError::internal)
}
async fn receipt(
    tx: &mut Tx<'_>,
    user: Uuid,
    day: NaiveDate,
    at: DateTime<Utc>,
) -> Result<Option<LearningRewardDay>, AppError> {
    sqlx::query_as("SELECT business_day,$3::timestamptz AS server_time,rule_version,minimum_units,daily_amount::text,qualifying_units,status,awarded_amount::text,operation_id,settled_at FROM learning_reward_settlements WHERE user_id=$1 AND business_day=$2")
        .bind(user).bind(day).bind(at).fetch_optional(&mut **tx).await.map_err(AppError::internal)
}

/// Called only for a newly inserted completion, under the caller's participant/user locks.
/// The caller MUST retain its final source/account/deadline checks after this function.
pub(super) async fn settle_completion_in(
    tx: &mut Tx<'_>,
    completion: Uuid,
) -> Result<(), AppError> {
    let (user, day, completed_at): (Uuid, Option<NaiveDate>, DateTime<Utc>) = sqlx::query_as(
        "SELECT user_id,business_day,completed_at FROM learning_completions WHERE id=$1",
    )
    .bind(completion)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    let Some(day) = day else {
        return Ok(());
    };
    if receipt(tx, user, day, completed_at).await?.is_some() {
        return Ok(());
    }
    policy_lock(tx).await?;
    let p = policy(tx, day).await?;
    let mut count = 0;
    let mut amount = 0;
    let mut operation = None;
    let mut status = LearningRewardStatus::RewardDisabled;
    if let Some(p) = p.as_ref().filter(|p| p.enabled) {
        let minimum = p
            .minimum_units
            .ok_or_else(|| AppError::internal(anyhow::anyhow!("reward policy missing minimum")))?;
        count = units(tx, user, day, minimum).await?;
        if count < minimum {
            return Ok(());
        }
        // Account locks serialize all supported wallet lifecycle changes; don't take a
        // wallet lock before credit_in's request/source locks.
        let eligible: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM account_deletion_requests WHERE user_id=$1 AND status='pending') AND NOT EXISTS(SELECT 1 FROM coin_wallets WHERE owner_type='user' AND owner_id=$1 AND status<>'open')")
            .bind(user).fetch_one(&mut **tx).await.map_err(AppError::internal)?;
        if eligible {
            amount = p.daily_amount.ok_or_else(|| {
                AppError::internal(anyhow::anyhow!("reward policy missing amount"))
            })?;
            let event = format!("{user}:{day}");
            let awarded = credit_in(
                tx,
                Owner {
                    owner_type: OwnerType::User,
                    owner_id: user,
                },
                Amount::new(amount).map_err(AppError::internal)?,
                &Context {
                    actor: Actor::System,
                    idempotency_scope: "learning_reward".into(),
                    idempotency_key: event.clone(),
                    source_type: "learning_reward".into(),
                    source_id: event,
                    reason: "每日学习奖励".into(),
                    evidence_ref: None,
                },
            )
            .await
            .map_err(AppError::internal)?;
            operation = Some(awarded.operation_id);
            status = LearningRewardStatus::Awarded;
        } else {
            status = LearningRewardStatus::WalletUnavailable;
        }
    }
    sqlx::query("INSERT INTO learning_reward_settlements(id,user_id,business_day,policy_id,rule_version,daily_amount,minimum_units,trigger_completion_id,completed_at,qualifying_units,status,awarded_amount,operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
        .bind(Uuid::now_v7()).bind(user).bind(day).bind(p.as_ref().map(|p|p.id))
        .bind(p.as_ref().map(|p|p.rule_version.as_str())).bind(p.as_ref().and_then(|p|p.daily_amount))
        .bind(p.as_ref().and_then(|p|p.minimum_units)).bind(completion).bind(completed_at)
        .bind(count).bind(status).bind(amount).bind(operation)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}

pub(super) async fn day(
    pool: &PgPool,
    auth: &AuthUser,
    query: LearningRewardQuery,
) -> Result<LearningRewardDay, AppError> {
    let mut tx = begin(pool, auth, &[]).await?;
    policy_lock(&mut tx).await?;
    let at = now(&mut tx).await?;
    let today = business_window(at).0;
    let day = query.business_day.unwrap_or(today);
    if day > today {
        return Err(invalid("不能查询未来奖励日期"));
    }
    let result = if let Some(r) = receipt(&mut tx, auth.subject, day, at).await? {
        r
    } else {
        let p = policy(&mut tx, day).await?;
        let enabled = p.as_ref().is_some_and(|p| p.enabled);
        let count = if let Some(p) = p.as_ref().filter(|p| p.enabled) {
            units(
                &mut tx,
                auth.subject,
                day,
                p.minimum_units.ok_or_else(|| {
                    AppError::internal(anyhow::anyhow!("reward policy missing minimum"))
                })?,
            )
            .await?
        } else {
            0
        };
        LearningRewardDay {
            business_day: day,
            server_time: at,
            rule_version: p.as_ref().map(|p| p.rule_version.clone()),
            minimum_units: p.as_ref().and_then(|p| p.minimum_units),
            daily_amount: p
                .as_ref()
                .and_then(|p| p.daily_amount)
                .map(|n| n.to_string()),
            qualifying_units: count,
            status: if !enabled {
                LearningRewardStatus::RewardDisabled
            } else if day < today {
                LearningRewardStatus::ThresholdNotMet
            } else {
                LearningRewardStatus::InProgress
            },
            awarded_amount: "0".into(),
            operation_id: None,
            settled_at: None,
        }
    };
    learner(&mut tx, auth).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
