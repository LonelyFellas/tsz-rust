use super::{dto::*, service};
use crate::{api::ApiJson, auth::extract::AuthUser, error::AppError, state::AppState};
use axum::{
    Json,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};
use uuid::Uuid;

#[utoipa::path(post,path="/api/v1/me/avatar/upload-url",tag="avatar",request_body=AvatarUploadRequest,
    responses((status=200,body=AvatarUploadResponse),(status=400),(status=401),(status=413),(status=422),(status=429),(status=500),(status=501),(status=503)),security(("bearer_auth"=[])))]
pub async fn create_upload(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(request): ApiJson<AvatarUploadRequest>,
) -> Result<Json<AvatarUploadResponse>, AppError> {
    service::create_upload(&state, &auth, request)
        .await
        .map(Json)
}

#[utoipa::path(post,path="/api/v1/me/avatar",tag="avatar",request_body=AvatarConfirmRequest,
    responses((status=200,body=AvatarConfirmResponse),(status=400),(status=401),(status=409),(status=413),(status=422),(status=500),(status=501),(status=503)),security(("bearer_auth"=[])))]
pub async fn confirm(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(request): ApiJson<AvatarConfirmRequest>,
) -> Result<Json<AvatarConfirmResponse>, AppError> {
    Ok(Json(AvatarConfirmResponse {
        user: service::confirm(&state, &auth, &request.key).await?,
    }))
}

#[utoipa::path(get,path="/api/v1/avatars/{id}",tag="avatar",params(("id"=Uuid,Path)),
    responses((status=200,content((inline(AvatarImage)="image/webp"))),(status=404),(status=500),(status=501),(status=503)))]
pub async fn read_public(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let result = match id.parse::<Uuid>() {
        Ok(id) => service::read_public(&state, id).await,
        Err(_) => Err(AppError::not_found("avatar not found")),
    };
    match result {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "image/webp"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CACHE_CONTROL, "public, max-age=300"),
            ],
            bytes,
        )
            .into_response(),
        Err(error) => {
            let mut response = error.into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
    }
}
