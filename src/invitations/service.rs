use crate::{
    coins::{
        model::{Actor, Amount, Context, Owner, OwnerType},
        service as coins,
    },
    error::{AppError, ErrorCode},
};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Validate before consuming the OTP. Codes and their owners are immutable and retained.
pub async fn resolve_code(pool: &PgPool, code: Option<&str>) -> Result<Option<Uuid>, AppError> {
    let Some(code) = code.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let invalid = || {
        AppError::validation(
            ErrorCode::InvalidInvitationCode,
            "invite_code",
            "invalid invitation code",
        )
    };
    if code.len() != 16 || !code.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    sqlx::query_scalar("SELECT user_id FROM invitation_codes WHERE code=$1")
        .bind(code.to_ascii_uppercase())
        .fetch_optional(pool)
        .await
        .map_err(AppError::internal)?
        .map(Some)
        .ok_or_else(invalid)
}

/// The sole existing participant is locked before inserting the new, uncommitted user.
/// A missing/inactive owner still has a valid historical code, but cannot receive rewards.
pub async fn lock_inviter_in(
    tx: &mut Transaction<'_, Postgres>,
    inviter: Uuid,
) -> Result<bool, AppError> {
    let active: Option<bool> =
        sqlx::query_scalar("SELECT status='active' FROM users WHERE id=$1 FOR SHARE")
            .bind(inviter)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    if active != Some(true) {
        return Ok(false);
    }
    eligible_in(tx, inviter).await
}

pub async fn eligible_in(tx: &mut Transaction<'_, Postgres>, user: Uuid) -> Result<bool, AppError> {
    // Run AFTER the user lock. All pending requests exclude rewards, including expired ones.
    sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM account_deletion_requests WHERE user_id=$1 AND status='pending') AND NOT EXISTS(SELECT 1 FROM coin_wallets WHERE owner_type='user' AND owner_id=$1 AND status<>'open')")
        .bind(user).fetch_one(&mut **tx).await.map_err(AppError::internal)
}

/// Registration records attribution only. No wallet is credited here.
pub async fn record_registration_in(
    tx: &mut Transaction<'_, Postgres>,
    inviter: Uuid,
    invitee: Uuid,
    eligible: bool,
    reward: Option<i64>,
) -> Result<(), AppError> {
    if let Some(amount) = reward.filter(|_| eligible) {
        sqlx::query("INSERT INTO invitation_reward_candidates(invitee_user_id,inviter_user_id,reward_amount) VALUES($1,$2,$3)")
            .bind(invitee).bind(inviter).bind(amount).execute(&mut **tx).await.map_err(AppError::internal)?;
    } else {
        let status = if eligible {
            "reward_disabled"
        } else {
            "inviter_unavailable"
        };
        sqlx::query("INSERT INTO invitation_registrations(invitee_user_id,inviter_user_id,reward_status,reward_amount) VALUES($1,$2,$3,0)")
            .bind(invitee).bind(inviter).bind(status).execute(&mut **tx).await.map_err(AppError::internal)?;
    }
    Ok(())
}

/// Called inside the completion transaction, after all participants are locked in UUID
/// order. A successful daily reward proves a bound, active invitee reached the threshold.
/// The terminal registration and ledger commit together, once for the invitee's lifetime.
pub async fn settle_qualified_in(
    tx: &mut Transaction<'_, Postgres>,
    completion: Uuid,
    invitee: Uuid,
    enabled: bool,
) -> Result<(), AppError> {
    let candidate: Option<(Uuid, Uuid, i64)> = sqlx::query_as(
        "SELECT c.invitee_user_id,c.inviter_user_id,c.reward_amount FROM invitation_reward_candidates c JOIN learning_reward_settlements s ON s.user_id=c.invitee_user_id WHERE c.invitee_user_id=$2 AND s.trigger_completion_id=$1 AND s.status='awarded' AND NOT EXISTS(SELECT 1 FROM invitation_registrations r WHERE r.invitee_user_id=c.invitee_user_id)"
    ).bind(completion).bind(invitee).fetch_optional(&mut **tx).await.map_err(AppError::internal)?;
    let Some((invitee, inviter, promised_amount)) = candidate else {
        return Ok(());
    };
    // The account lock was already taken by learning_tasks::repository::begin.
    let active: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND status='active')")
            .bind(inviter)
            .fetch_one(&mut **tx)
            .await
            .map_err(AppError::internal)?;
    let eligible = active && eligible_in(tx, inviter).await?;
    let (status, amount, operation) = if !eligible {
        ("inviter_unavailable", 0, None)
    } else if enabled {
        let event = invitee.to_string();
        let receipt = coins::credit_in(
            tx,
            Owner {
                owner_type: OwnerType::User,
                owner_id: inviter,
            },
            Amount::new(promised_amount).map_err(AppError::internal)?,
            &Context {
                actor: Actor::System,
                idempotency_scope: "invitation_registration".into(),
                idempotency_key: event.clone(),
                source_type: "invitation_reward".into(),
                source_id: event,
                reason: "受邀用户绑定手机号并首次学习达标奖励".into(),
                evidence_ref: None,
            },
        )
        .await
        .map_err(AppError::internal)?;
        ("awarded", promised_amount, Some(receipt.operation_id))
    } else {
        ("reward_disabled", 0, None)
    };
    sqlx::query("INSERT INTO invitation_registrations(invitee_user_id,inviter_user_id,reward_status,reward_amount,operation_id) VALUES($1,$2,$3,$4,$5)")
        .bind(invitee).bind(inviter).bind(status).bind(amount).bind(operation)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
