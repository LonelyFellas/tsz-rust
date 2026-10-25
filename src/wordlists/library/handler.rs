use super::{dto::*, *};
use crate::wordlists::private_json;
use crate::{
    api::{ApiJson, ApiPath, ApiQuery},
    state::AppState,
};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::Response,
    routing::{get, post, put},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/me/wordlist-library/{space}/selection",
            post(select),
        )
        .route("/api/v1/me/wordlist-library/{space}", get(list))
        .route("/api/v1/me/wordlist-library/{space}/folders", post(folder))
        .route(
            "/api/v1/me/wordlist-library/{space}/nodes/{id}",
            put(rename),
        )
        .route("/api/v1/me/wordlist-library/{space}/preview", post(preview))
        .route(
            "/api/v1/me/wordlist-library/{space}/operations",
            post(apply),
        )
}
#[utoipa::path(get,path="/api/v1/me/wordlist-library/{space}",tag="wordlists",params(("space"=LibrarySpace,Path),LibraryQuery),security(("bearer_auth"=[])),responses((status=200,description="个人目录或回收站，用户和系统隔离",body=LibraryPage),(status=400,description="参数无效"),(status=401,description="未登录"),(status=403,description="账户不可用"),(status=404,description="目录不存在")))]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(space): ApiPath<LibrarySpace>,
    ApiQuery(query): ApiQuery<LibraryQuery>,
) -> Result<Response, AppError> {
    Ok(private_json(
        listing(&state.pool, &auth, space, query).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/me/wordlist-library/{space}/folders",tag="wordlists",params(("space"=LibrarySpace,Path)),security(("bearer_auth"=[])),request_body=LibraryFolderInput,responses((status=204,description="已创建"),(status=400,description="参数无效"),(status=404,description="目录不存在"),(status=409,description="目录版本冲突或重名")))]
pub async fn folder(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(space): ApiPath<LibrarySpace>,
    ApiJson(input): ApiJson<LibraryFolderInput>,
) -> Result<StatusCode, AppError> {
    create_folder(&state.pool, &auth, space, input).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(put,path="/api/v1/me/wordlist-library/{space}/nodes/{id}",tag="wordlists",params(("space"=LibrarySpace,Path),("id"=Uuid,Path)),security(("bearer_auth"=[])),request_body=LibraryRename,responses((status=204,description="已重命名"),(status=400,description="参数无效"),(status=404,description="项目不存在"),(status=409,description="版本或状态冲突")))]
pub async fn rename(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath((space, id)): ApiPath<(LibrarySpace, Uuid)>,
    ApiJson(input): ApiJson<LibraryRename>,
) -> Result<StatusCode, AppError> {
    rename_node(&state.pool, &auth, space, id, input).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post,path="/api/v1/me/wordlist-library/{space}/preview",tag="wordlists",params(("space"=LibrarySpace,Path)),security(("bearer_auth"=[])),request_body=LibraryOperation,responses((status=200,description="确认操作影响，版本变化后须重新预览",body=LibraryImpact),(status=400,description="参数无效"),(status=404,description="项目不存在"),(status=409,description="目录版本冲突")))]
pub async fn preview(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(space): ApiPath<LibrarySpace>,
    ApiJson(input): ApiJson<LibraryOperation>,
) -> Result<Response, AppError> {
    Ok(private_json(
        operation(&state.pool, &auth, space, input, true).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/me/wordlist-library/{space}/operations",tag="wordlists",params(("space"=LibrarySpace,Path)),security(("bearer_auth"=[])),request_body=LibraryOperation,responses((status=200,description="原子完成移动、删除、恢复或永久删除",body=LibraryImpact),(status=400,description="参数无效"),(status=404,description="项目不存在"),(status=409,description="目录版本冲突")))]
pub async fn apply(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(space): ApiPath<LibrarySpace>,
    ApiJson(input): ApiJson<LibraryOperation>,
) -> Result<Response, AppError> {
    Ok(private_json(
        operation(&state.pool, &auth, space, input, false).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/me/wordlist-library/{space}/selection",tag="wordlists",params(("space"=LibrarySpace,Path)),security(("bearer_auth"=[])),request_body=LibrarySelectionInput,responses((status=200,description="展开正常子树并返回去重词表",body=LibrarySelection),(status=400,description="选择为空或超限"),(status=401,description="会话失效"),(status=403,description="需要学生资格"),(status=404,description="节点不存在"),(status=409,description="目录版本变化或来源不可用"),(status=422,description="JSON不符合契约")))]
pub async fn select(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(space): ApiPath<LibrarySpace>,
    ApiJson(input): ApiJson<LibrarySelectionInput>,
) -> Result<Response, AppError> {
    Ok(private_json(
        expand_selection(&state.pool, &auth, space, input).await?,
    ))
}
