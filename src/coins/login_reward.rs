//! One fixed reward per user/business day, using the ledger's durable source uniqueness.
use super::{model::*, service};
use crate::{
    api::{ApiJson, ApiQuery},
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    learning_tasks::question::business_window,
    state::AppState,
};
use axum::{Json, extract::State};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRewardRequest {}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoginRewardStatus {
    Awarded,
    WalletUnavailable,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRewardDay {
    pub business_day: NaiveDate,
    pub server_time: DateTime<Utc>,
    pub next_day_starts_at: DateTime<Utc>,
    pub status: LoginRewardStatus,
    #[schema(pattern = "^[01]$")]
    pub awarded_amount: String,
    #[schema(required = true)]
    pub operation_id: Option<Uuid>,
}

async fn now(tx: &mut Transaction<'_, Postgres>) -> Result<DateTime<Utc>, AppError> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(AppError::internal)
}

pub async fn settle(pool: &PgPool, auth: &AuthUser) -> Result<LoginRewardDay, AppError> {
    let mut tx = pool.begin().await.map_err(AppError::internal)?;
    // Serialize against account deletion/security changes before any ledger locks.
    let account: Option<(String, i64, Option<String>)> =
        sqlx::query_as("SELECT status,security_version,phone FROM users WHERE id=$1 FOR SHARE")
            .bind(auth.subject)
            .fetch_optional(&mut *tx)
            .await
            .map_err(AppError::internal)?;
    let Some((status, version, phone)) = account else {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "会话已失效",
        ));
    };
    if status != "active" || version != auth.security_version {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "会话已失效",
        ));
    }
    if phone.is_none() {
        return Err(AppError::forbidden(
            ErrorCode::PhoneBindingRequired,
            "请先绑定手机号",
        ));
    }
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    let at = now(&mut tx).await?;
    let (day, _, next_day) = business_window(at);
    let event = format!("{}:{day}", auth.subject);
    let old: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM coin_operations WHERE source_type='daily_login_reward' AND source_id=$1",
    )
    .bind(&event)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let operation = if old.is_some() {
        old
    } else {
        let unavailable: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM account_deletion_requests WHERE user_id=$1 AND status='pending') OR EXISTS(SELECT 1 FROM coin_wallets WHERE owner_type='user' AND owner_id=$1 AND status<>'open')")
            .bind(auth.subject).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
        if unavailable {
            None
        } else {
            Some(
                service::credit_in(
                    &mut tx,
                    Owner {
                        owner_type: OwnerType::User,
                        owner_id: auth.subject,
                    },
                    Amount::new(1).map_err(AppError::internal)?,
                    &Context {
                        actor: Actor::System,
                        idempotency_scope: "daily_login_reward".into(),
                        idempotency_key: event.clone(),
                        source_type: "daily_login_reward".into(),
                        source_id: event,
                        reason: "每日登录奖励".into(),
                        evidence_ref: None,
                    },
                )
                .await
                .map_err(AppError::internal)?
                .operation_id,
            )
        }
    };
    // Wallet/request locks can wait across 04:00 or the deletion deadline.
    crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
    let finished_at = now(&mut tx).await?;
    if business_window(finished_at).0 != day {
        return Err(AppError::conflict(
            ErrorCode::CoinSourceConflict,
            None,
            "业务日已切换，请重试登录奖励",
        ));
    }
    tx.commit().await.map_err(AppError::internal)?;
    Ok(LoginRewardDay {
        business_day: day,
        server_time: finished_at,
        next_day_starts_at: next_day,
        status: if operation.is_some() {
            LoginRewardStatus::Awarded
        } else {
            LoginRewardStatus::WalletUnavailable
        },
        awarded_amount: if operation.is_some() { "1" } else { "0" }.into(),
        operation_id: operation,
    })
}

#[utoipa::path(post,path="/api/v1/me/coins/login-reward",tag="coins",security(("bearer_auth"=[])),
    request_body=LoginRewardRequest,
    responses((status=200,description="当日登录奖励结算；重复访问返回同一流水",body=LoginRewardDay),
    (status=400,description="查询参数无效"),(status=422,description="不接受客户端金额、日期或目标账号"),(status=401,description="会话失效"),
    (status=403,description="需绑定手机号或账户不可用"),(status=409,description="跨日，请重试"),
    (status=500,description="结算失败，不影响登录")))]
pub async fn claim(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(_query): ApiQuery<LoginRewardRequest>,
    ApiJson(_body): ApiJson<LoginRewardRequest>,
) -> Result<Json<LoginRewardDay>, AppError> {
    Ok(Json(settle(&state.pool, &auth).await?))
}
