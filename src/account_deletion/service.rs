use super::dto::*;
use crate::{
    auth::extract::AuthUser,
    coins::{
        model::{Owner, OwnerType},
        service as coins,
    },
    error::{AppError, ErrorCode},
    otp::model::Purpose,
    state::AppState,
    user::{model::AccountDeletionChannel, repository::UserRepository},
};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

pub const CONSENT_VERSION: &str = "account-deletion-72h-v1";
pub const CONSENT_TEXT: &str = "我确认申请注销账号。申请成功后等待连续72小时，期间可以撤销，钱包全部收支暂停且余额保持不变。到期后账号不可再使用，剩余天生币作废，相关历史流水及必要的签署证据保留，个人数据随后完成清理。有余额时，我主动同意放弃本次确认金额对应的全部天生币。";
fn conflict(code: ErrorCode, message: &str) -> AppError {
    AppError::conflict(code, None, message)
}
fn invalid() -> AppError {
    AppError::unauthorized(ErrorCode::InvalidToken, "invalid token")
}
async fn lock_user(
    tx: &mut Transaction<'_, Postgres>,
    auth: &AuthUser,
) -> Result<(Option<String>, Option<String>), AppError> {
    let contacts=sqlx::query_as::<_,(Option<String>,Option<String>)>("SELECT phone,email FROM users WHERE id=$1 AND status='active' AND security_version=$2 FOR UPDATE")
        .bind(auth.subject).bind(auth.security_version).fetch_optional(&mut **tx).await.map_err(AppError::internal)?.ok_or_else(invalid)?;
    super::ensure_not_effective_in(tx, auth.subject).await?;
    Ok(contacts)
}
async fn read(conn: &mut PgConnection, id: Uuid) -> Result<AccountDeletionRequest, AppError> {
    sqlx::query_as("SELECT id,status,requested_at,effective_at,cancelled_at,completed_at,confirmed_balance::text AS confirmed_balance,waive_balance,consent_version,consent_text FROM account_deletion_requests WHERE id=$1")
    .bind(id)
    .fetch_one(conn)
    .await
    .map_err(AppError::internal)
}
pub async fn state(state: &AppState, auth: &AuthUser) -> Result<AccountDeletionState, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let request=sqlx::query_as("SELECT id,status,requested_at,effective_at,cancelled_at,completed_at,confirmed_balance::text AS confirmed_balance,waive_balance,consent_version,consent_text FROM account_deletion_requests WHERE user_id=$1 ORDER BY requested_at DESC,id DESC LIMIT 1")
        .bind(auth.subject).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    let balance: Option<i64> = sqlx::query_scalar(
        "SELECT balance FROM coin_wallets WHERE owner_type='user' AND owner_id=$1",
    )
    .bind(auth.subject)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let server_time = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(AccountDeletionState {
        request,
        coin_balance: balance.unwrap_or(0).to_string(),
        consent_version: CONSENT_VERSION.into(),
        consent_text: CONSENT_TEXT.into(),
        server_time,
    })
}
pub async fn create(
    state: &AppState,
    auth: &AuthUser,
    input: CreateAccountDeletionRequest,
) -> Result<AccountDeletionRequest, AppError> {
    let balance = input
        .expected_coin_balance
        .parse::<i64>()
        .ok()
        .filter(|n| *n >= 0 && n.to_string() == input.expected_coin_balance)
        .ok_or_else(|| {
            AppError::bad_request(ErrorCode::InvalidRequestBody, "invalid coin balance")
        })?;
    // The OTP is proof, not intent. Never store it or a low-entropy hash of it.
    let hash = Sha256::digest(
        serde_json::to_vec(&(
            input.channel,
            balance,
            input.waive_balance,
            input.confirm_deletion,
            &input.consent_version,
        ))
        .expect("consent intent"),
    )
    .to_vec();
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let (phone, email) = lock_user(&mut tx, auth).await?;
    if let Some((id,old_hash))=sqlx::query_as::<_,(Uuid,Vec<u8>)>("SELECT id,request_hash FROM account_deletion_requests WHERE user_id=$1 AND idempotency_key=$2 FOR UPDATE")
        .bind(auth.subject).bind(input.idempotency_key).fetch_optional(&mut *tx).await.map_err(AppError::internal)? {
        if hash!=old_hash {return Err(conflict(ErrorCode::IdempotencyConflict,"deletion intent changed"));}
        return read(&mut tx,id).await;
    }
    let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM account_deletion_requests WHERE user_id=$1 AND status='pending')")
        .bind(auth.subject).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if pending {
        return Err(conflict(
            ErrorCode::AccountDeletionPending,
            "account deletion is already pending",
        ));
    }
    if input.consent_version != CONSENT_VERSION {
        return Err(conflict(
            ErrorCode::AccountDeletionConsentOutdated,
            "reload the current consent",
        ));
    }
    if !input.confirm_deletion || (balance > 0 && !input.waive_balance) {
        return Err(AppError::bad_request(
            ErrorCode::AccountDeletionConsentRequired,
            "active consent is required",
        ));
    }
    let wallet = coins::pause_in(
        &mut tx,
        Owner {
            owner_type: OwnerType::User,
            owner_id: auth.subject,
        },
    )
    .await
    .map_err(AppError::internal)?;
    if wallet.balance != balance {
        return Err(conflict(
            ErrorCode::AccountDeletionBalanceChanged,
            "reload balance and sign again",
        ));
    }
    let target = match input.channel {
        AccountDeletionChannel::Phone => phone,
        AccountDeletionChannel::Email => email,
    }
    .ok_or_else(|| {
        conflict(
            ErrorCode::AccountDeletionChannelUnavailable,
            "selected channel is unavailable",
        )
    })?;
    state
        .otp_service
        .verify(&target, Purpose::AccountDeletion, &input.code)
        .await
        .map_err(crate::auth::handler::map_account_deletion_otp_error)?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let id = Uuid::now_v7();
    let channel = match input.channel {
        AccountDeletionChannel::Phone => "phone",
        AccountDeletionChannel::Email => "email",
    };
    sqlx::query("INSERT INTO account_deletion_requests (id,user_id,status,requested_at,effective_at,confirmed_balance,waive_balance,confirm_deletion,consent_version,consent_text,signed_at,verification_channel,idempotency_key,request_hash) VALUES ($1,$2,'pending',$3,$3+interval '72 hours',$4,$5,true,$6,$7,$3,$8,$9,$10)")
        .bind(id).bind(auth.subject).bind(now).bind(balance).bind(input.waive_balance).bind(CONSENT_VERSION).bind(CONSENT_TEXT).bind(channel).bind(input.idempotency_key).bind(hash)
        .execute(&mut *tx).await.map_err(AppError::internal)?;
    let result = read(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn cancel(
    state: &AppState,
    auth: &AuthUser,
    id: Uuid,
) -> Result<AccountDeletionRequest, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, auth).await?;
    let request = sqlx::query_as::<_, AccountDeletionRequest>("SELECT id,status,requested_at,effective_at,cancelled_at,completed_at,confirmed_balance::text AS confirmed_balance,waive_balance,consent_version,consent_text FROM account_deletion_requests WHERE id=$1 AND user_id=$2 FOR UPDATE")
    .bind(id)
    .bind(auth.subject)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("deletion request not found"))?;
    if request.status == DeletionStatus::Cancelled {
        return Ok(request);
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    if request.status != DeletionStatus::Pending || now >= request.effective_at {
        return Err(conflict(
            ErrorCode::AccountDeletionExpired,
            "cancellation deadline passed",
        ));
    }
    coins::resume_in(
        &mut tx,
        Owner {
            owner_type: OwnerType::User,
            owner_id: auth.subject,
        },
    )
    .await
    .map_err(AppError::internal)?;
    // Recheck after all locks, including the wallet lock acquired above.
    let updated=sqlx::query("WITH timing AS MATERIALIZED (SELECT clock_timestamp() AS now) UPDATE account_deletion_requests SET status='cancelled',cancelled_at=timing.now FROM timing WHERE id=$1 AND effective_at>timing.now")
        .bind(id).execute(&mut *tx).await.map_err(AppError::internal)?.rows_affected();
    if updated != 1 {
        return Err(conflict(
            ErrorCode::AccountDeletionExpired,
            "cancellation deadline passed",
        ));
    }
    let result = read(&mut tx, id).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(result)
}
pub async fn complete(pool: &sqlx::PgPool, user_id: Uuid, id: Uuid) -> Result<bool, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    // Lock accounts before requests; skip busy users so one transaction cannot stall the sweep.
    let user: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE id=$1 FOR UPDATE SKIP LOCKED")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    if user.is_none() {
        return Ok(false);
    }
    let request = sqlx::query_as::<_, AccountDeletionRequest>("SELECT id,status,requested_at,effective_at,cancelled_at,completed_at,confirmed_balance::text AS confirmed_balance,waive_balance,consent_version,consent_text FROM account_deletion_requests WHERE id=$1 AND user_id=$2 FOR UPDATE")
    .bind(id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let Some(request) = request else {
        return Ok(false);
    };
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    if request.status != DeletionStatus::Pending || now < request.effective_at {
        return Ok(false);
    }
    let balance = request
        .confirmed_balance
        .parse::<i64>()
        .map_err(AppError::internal)?;
    if (balance > 0 && !request.waive_balance) || request.consent_text.is_empty() {
        return Err(AppError::internal(anyhow::anyhow!(
            "invalid persisted consent"
        )));
    }
    coins::close_in(
        &mut tx,
        Owner {
            owner_type: OwnerType::User,
            owner_id: user_id,
        },
        balance,
        id,
    )
    .await
    .map_err(AppError::internal)?;
    crate::avatar::repository::schedule_user_cleanup_in(&mut tx, user_id)
        .await
        .map_err(AppError::internal)?;
    sqlx::query("UPDATE teacher_certification_files SET state='delete_pending' WHERE user_id=$1 AND state<>'uploading'")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    if !UserRepository::delete_account_in(&mut tx, user_id)
        .await
        .map_err(AppError::internal)?
    {
        return Err(invalid());
    }
    sqlx::query("UPDATE account_deletion_requests SET status='completed',completed_at=clock_timestamp() WHERE id=$1").bind(id).execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(true)
}
