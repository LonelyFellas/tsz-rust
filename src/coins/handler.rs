use super::{
    dto::*,
    model::{Owner, OwnerType},
    repository,
};
use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::ApiQuery,
    auth::extract::AuthUser,
    error::AppError,
    state::AppState,
};
use axum::{Json, extract::State};

#[utoipa::path(get,path="/api/v1/me/coins/wallet",tag="coins",security(("bearer_auth"=[])),
    responses((status=200,description="本人账本；金额为十进制字符串",body=CoinWallet),
    (status=400,description="分页参数无效"),(status=401,description="会话无效"),(status=403,description="账户不可用"),(status=500,description="查询失败，不返回零余额")))]
pub async fn user_wallet(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<CoinWallet>, AppError> {
    let owner = Owner {
        owner_type: OwnerType::User,
        owner_id: auth.subject,
    };
    Ok(Json(
        repository::wallet(&state.pool, owner)
            .await
            .map_err(AppError::internal)?,
    ))
}

#[utoipa::path(get,path="/api/v1/me/coins/entries",tag="coins",security(("bearer_auth"=[])),params(CoinEntriesQuery),
    responses((status=200,description="本人账本；金额为十进制字符串",body=CoinEntryPage),
    (status=400,description="分页参数无效"),(status=401,description="会话无效"),(status=403,description="账户不可用"),(status=500,description="查询失败，不返回零余额")))]
pub async fn user_entries(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<CoinEntriesQuery>,
) -> Result<Json<CoinEntryPage>, AppError> {
    let owner = Owner {
        owner_type: OwnerType::User,
        owner_id: auth.subject,
    };
    Ok(Json(repository::entries(&state.pool, owner, query).await?))
}

#[utoipa::path(get,path="/api/v1/admin/me/coins/wallet",tag="coins",security(("bearer_auth"=[])),
    responses((status=200,description="本人账本；金额为十进制字符串",body=CoinWallet),
    (status=400,description="分页参数无效"),(status=401,description="会话无效"),(status=403,description="账户不可用"),(status=500,description="查询失败，不返回零余额")))]
pub async fn admin_wallet(
    State(state): State<AppState>,
    auth: AdminAuth,
) -> Result<Json<CoinWallet>, AppError> {
    require_active_admin(&state, &auth).await?;
    let owner = Owner {
        owner_type: OwnerType::Admin,
        owner_id: auth.subject,
    };
    Ok(Json(
        repository::wallet(&state.pool, owner)
            .await
            .map_err(AppError::internal)?,
    ))
}

#[utoipa::path(get,path="/api/v1/admin/me/coins/entries",tag="coins",security(("bearer_auth"=[])),params(CoinEntriesQuery),
    responses((status=200,description="本人账本；金额为十进制字符串",body=CoinEntryPage),
    (status=400,description="分页参数无效"),(status=401,description="会话无效"),(status=403,description="账户不可用"),(status=500,description="查询失败，不返回零余额")))]
pub async fn admin_entries(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<CoinEntriesQuery>,
) -> Result<Json<CoinEntryPage>, AppError> {
    require_active_admin(&state, &auth).await?;
    let owner = Owner {
        owner_type: OwnerType::Admin,
        owner_id: auth.subject,
    };
    Ok(Json(repository::entries(&state.pool, owner, query).await?))
}
