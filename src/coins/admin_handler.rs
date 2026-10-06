use super::{
    admin_dto::*,
    admin_service,
    dto::{CoinEntriesQuery, CoinEntryPage},
    model::{Owner, OwnerType},
    repository,
};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiJson, ApiPath, ApiQuery},
    error::AppError,
    state::AppState,
};
use axum::{Json, extract::State};
use uuid::Uuid;

#[utoipa::path(get,path="/api/v1/admin/coins/accounts",tag="coins",security(("bearer_auth"=[])),params(CoinAccountsQuery),
responses((status=200,description="按身份查找账户，未开户返回真实零余额",body=CoinAccountPage),(status=400,description="查询参数错误"),(status=401,description="会话无效"),(status=403,description="需要 coins.access")))]
pub async fn accounts(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<CoinAccountsQuery>,
) -> Result<Json<CoinAccountPage>, AppError> {
    Ok(Json(
        admin_service::accounts(&state.pool, &auth, query).await?,
    ))
}
#[utoipa::path(get,path="/api/v1/admin/coins/accounts/{owner_type}/{owner_id}/entries",tag="coins",security(("bearer_auth"=[])),
params(("owner_type"=OwnerType,Path),("owner_id"=Uuid,Path),CoinEntriesQuery),
responses((status=200,description="指定主体的公开流水投影；含已关闭钱包",body=CoinEntryPage),(status=400,description="查询参数错误"),(status=401,description="会话无效"),(status=403,description="需要 coins.access")))]
pub async fn entries(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath((owner_type, owner_id)): ApiPath<(OwnerType, Uuid)>,
    ApiQuery(query): ApiQuery<CoinEntriesQuery>,
) -> Result<Json<CoinEntryPage>, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("coins.access")?;
    let response = repository::entries(
        &state.pool,
        Owner {
            owner_type,
            owner_id,
        },
        query,
    )
    .await?;
    permissions::reload(&state, &auth)
        .await?
        .require("coins.access")?;
    Ok(Json(response))
}
#[utoipa::path(post,path="/api/v1/admin/coins/manual-credits",tag="coins",security(("bearer_auth"=[])),request_body=ManualCreditRequest,
responses((status=200,description="已记账操作；重试返回原操作",body=ManualCoinOperation),(status=400,description="金额或载荷错误"),(status=401,description="会话无效"),(status=403,description="coins.credit、目标身份及本人限制"),(status=409,description="重复事件、幂等冲突或钱包暂停")))]
pub async fn credit(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiJson(input): ApiJson<ManualCreditRequest>,
) -> Result<Json<ManualCoinOperation>, AppError> {
    Ok(Json(
        admin_service::credit(&state.pool, &auth, input).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/admin/coins/manual-credits/{operation_id}/reversal",tag="coins",security(("bearer_auth"=[])),params(("operation_id"=Uuid,Path)),request_body=ManualReversalRequest,
responses((status=200,description="原人工入账全额冲正；重试返回原操作",body=ManualCoinOperation),(status=400,description="载荷错误"),(status=401,description="会话无效"),(status=403,description="coins.reverse、目标身份及本人限制"),(status=404,description="原操作不存在"),(status=409,description="已冲正、余额不足或钱包暂停")))]
pub async fn reverse(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<ManualReversalRequest>,
) -> Result<Json<ManualCoinOperation>, AppError> {
    Ok(Json(
        admin_service::reverse(&state.pool, &auth, id, input).await?,
    ))
}
#[utoipa::path(get,path="/api/v1/admin/coins/operations",tag="coins",security(("bearer_auth"=[])),params(CoinOperationsQuery),
responses((status=200,description="管理操作记录；内部原因和凭据仅管理端可见",body=ManualCoinOperationPage),(status=400,description="查询参数错误"),(status=401,description="会话无效"),(status=403,description="需要 coins.access")))]
pub async fn operations(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<CoinOperationsQuery>,
) -> Result<Json<ManualCoinOperationPage>, AppError> {
    Ok(Json(
        admin_service::operations(&state.pool, &auth, query).await?,
    ))
}
