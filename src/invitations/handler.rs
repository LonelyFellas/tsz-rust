use super::{dto::*, service};
use crate::{
    api::{ApiQuery, PaginationMeta},
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    state::AppState,
};
use axum::{Json, extract::State};
use sqlx::{Postgres, Transaction};

async fn lock_user(tx: &mut Transaction<'_, Postgres>, auth: &AuthUser) -> Result<(), AppError> {
    let exists: Option<bool> = sqlx::query_scalar(
        "SELECT true FROM users WHERE id=$1 AND status='active' AND security_version=$2 FOR SHARE",
    )
    .bind(auth.subject)
    .bind(auth.security_version)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::internal)?;
    if exists != Some(true) {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    crate::account_deletion::ensure_not_effective_in(tx, auth.subject).await
}

#[utoipa::path(get,path="/api/v1/me/invitations",tag="invitations",security(("bearer_auth"=[])),
    responses((status=200,description="本人邀请码及当前有效奖励配置；未启用时金额为空",body=InvitationOverview),
    (status=401,description="会话无效"),(status=500,description="查询失败")))]
pub async fn overview(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<InvitationOverview>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, &auth).await?;
    let invite_code = sqlx::query_scalar("SELECT code FROM invitation_codes WHERE user_id=$1")
        .bind(auth.subject)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    let can_receive_reward = service::eligible_in(&mut tx, auth.subject).await?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(InvitationOverview {
        invite_code,
        reward_amount: state.invitation_reward_amount.map(|v| v.to_string()),
        can_receive_reward,
    }))
}

#[utoipa::path(post,path="/api/v1/me/invitations/code",tag="invitations",security(("bearer_auth"=[])),
    responses((status=200,description="幂等创建或返回本人固定邀请码",body=InvitationCode),
    (status=401,description="会话无效"),(status=500,description="创建失败")))]
pub async fn create_code(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<InvitationCode>, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, &auth).await?;
    // ON CONFLICT also handles a concurrent request for the same user. No UPDATE of immutable rows.
    loop {
        let code = format!("{:016X}", rand::random::<u64>());
        sqlx::query(
            "INSERT INTO invitation_codes(user_id,code) VALUES($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(auth.subject)
        .bind(code)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
        if let Some(invite_code) =
            sqlx::query_scalar("SELECT code FROM invitation_codes WHERE user_id=$1")
                .bind(auth.subject)
                .fetch_optional(&mut *tx)
                .await
                .map_err(AppError::internal)?
        {
            crate::account_deletion::ensure_not_effective_in(&mut tx, auth.subject).await?;
            tx.commit().await.map_err(AppError::internal)?;
            return Ok(Json(InvitationCode { invite_code }));
        }
        // The random code belonged to someone else; generate another without changing their row.
    }
}

#[utoipa::path(get,path="/api/v1/me/invitations/records",tag="invitations",security(("bearer_auth"=[])),params(InvitationRecordsQuery),
    responses((status=200,description="本人邀请记录；不含联系方式或内部凭据",body=InvitationRecordPage),
    (status=400,description="分页无效"),(status=401,description="会话无效"),(status=500,description="查询失败")))]
pub async fn records(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<InvitationRecordsQuery>,
) -> Result<Json<InvitationRecordPage>, AppError> {
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    if page == 0 || !(1..=100).contains(&page_size) {
        return Err(AppError::bad_request(
            ErrorCode::InvalidQuery,
            "invalid invitation pagination",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    lock_user(&mut tx, &auth).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM invitation_registrations WHERE inviter_user_id=$1",
    )
    .bind(auth.subject)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::internal)?;
    let items = sqlx::query_as("SELECT r.invitee_user_id,u.display_name AS invitee_name,r.reward_status,r.reward_amount::text AS reward_amount,r.created_at FROM invitation_registrations r LEFT JOIN users u ON u.id=r.invitee_user_id WHERE r.inviter_user_id=$1 ORDER BY r.created_at DESC,r.invitee_user_id DESC LIMIT $2 OFFSET $3")
        .bind(auth.subject).bind(i64::from(page_size)).bind(i64::from(page-1)*i64::from(page_size))
        .fetch_all(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(Json(InvitationRecordPage {
        items,
        pagination: PaginationMeta {
            page,
            page_size,
            total,
            total_pages: (total + i64::from(page_size) - 1) / i64::from(page_size),
        },
    }))
}
