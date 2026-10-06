use super::{dto::*, service};
use crate::{
    api::{ApiJson, ApiPath},
    auth::extract::SessionUser,
    error::AppError,
    state::AppState,
};
use axum::{Json, extract::State, http::StatusCode};
use uuid::Uuid;
#[utoipa::path(get,path="/api/v1/me/account-deletion",tag="account-deletion",security(("bearer_auth"=[])),responses((status=200,description="注销状态和当前签署内容",body=AccountDeletionState),(status=401,description="会话无效或已到注销生效时间"),(status=500,description="查询失败")))]
pub async fn get(
    State(state): State<AppState>,
    SessionUser(auth): SessionUser,
) -> Result<Json<AccountDeletionState>, AppError> {
    Ok(Json(service::state(&state, &auth).await?))
}
#[utoipa::path(post,path="/api/v1/me/account-deletion",tag="account-deletion",security(("bearer_auth"=[])),request_body=CreateAccountDeletionRequest,responses((status=202,description="申请已保存，72小时后生效；当前会话保留",body=AccountDeletionRequest),(status=400,description="签署或金额无效"),(status=401,description="会话或验证码无效"),(status=409,description="余额、签署版本、幂等意图或申请状态冲突"),(status=422,description="请求字段无效"),(status=429,description="验证码限流"),(status=503,description="验证码服务不可用"),(status=500,description="保存失败")))]
pub async fn create(
    State(state): State<AppState>,
    SessionUser(auth): SessionUser,
    ApiJson(input): ApiJson<CreateAccountDeletionRequest>,
) -> Result<(StatusCode, Json<AccountDeletionRequest>), AppError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(service::create(&state, &auth, input).await?),
    ))
}
#[utoipa::path(post,path="/api/v1/me/account-deletion/{id}/cancel",tag="account-deletion",security(("bearer_auth"=[])),params(("id"=Uuid,Path,description="本人的注销申请")),responses((status=200,description="已撤销，余额保留",body=AccountDeletionRequest),(status=400,description="路径无效"),(status=401,description="会话无效或已到注销生效时间"),(status=404,description="不是本人的申请"),(status=409,description="撤销期限已过"),(status=500,description="撤销失败")))]
pub async fn cancel(
    State(state): State<AppState>,
    SessionUser(auth): SessionUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AccountDeletionRequest>, AppError> {
    Ok(Json(service::cancel(&state, &auth, id).await?))
}
