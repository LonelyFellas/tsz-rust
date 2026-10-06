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

/// Registration only: the caller just inserted invitee and holds inviter's user lock.
/// Technical errors propagate so user, roles, attribution, ledger and session all roll back.
pub async fn record_registration_in(
    tx: &mut Transaction<'_, Postgres>,
    inviter: Uuid,
    invitee: Uuid,
    eligible: bool,
    reward: Option<i64>,
) -> Result<(), AppError> {
    let (status, amount, operation) = if !eligible {
        ("inviter_unavailable", 0, None)
    } else if let Some(amount) = reward {
        let event = invitee.to_string();
        let receipt = coins::credit_in(
            tx,
            Owner {
                owner_type: OwnerType::User,
                owner_id: inviter,
            },
            Amount::new(amount).map_err(AppError::internal)?,
            &Context {
                actor: Actor::System,
                idempotency_scope: "invitation_registration".into(),
                idempotency_key: event.clone(),
                source_type: "invitation_reward".into(),
                source_id: event,
                reason: "新用户注册邀请奖励".into(),
                evidence_ref: None,
            },
        )
        .await
        .map_err(AppError::internal)?;
        ("awarded", amount, Some(receipt.operation_id))
    } else {
        ("reward_disabled", 0, None)
    };
    sqlx::query("INSERT INTO invitation_registrations(invitee_user_id,inviter_user_id,reward_status,reward_amount,operation_id) VALUES($1,$2,$3,$4,$5)")
        .bind(invitee).bind(inviter).bind(status).bind(amount).bind(operation)
        .execute(&mut **tx).await.map_err(AppError::internal)?;
    Ok(())
}
