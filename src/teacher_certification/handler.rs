use super::{model::*, service};
use crate::api::{ApiJson, ApiPath, ApiQuery};
use crate::{
    admin::{AdminAuth, authorization::require_super_admin},
    auth::extract::AuthUser,
    error::AppError,
    state::AppState,
};
use axum::{Json, extract::State, http::StatusCode};
use uuid::Uuid;

#[utoipa::path(get, path = "/api/v1/me/teacher-certification", tag = "teacher-certification",
    responses((status = 200, body = TeacherCertification), (status = 401)), security(("bearer_auth" = [])))]
pub async fn mine(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<TeacherCertification>, AppError> {
    let teacher_verified = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM teacher_profiles WHERE user_id = $1 AND verified)",
    )
    .bind(auth.subject)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::internal)?;
    let application = sqlx::query_as::<_, TeacherApplication>(
        "SELECT * FROM teacher_applications WHERE user_id = $1 ORDER BY submitted_at DESC, id DESC LIMIT 1",
    ).bind(auth.subject).fetch_optional(&state.pool).await.map_err(AppError::internal)?;
    let files = match &application {
        Some(application) => application_files(&state, application.id).await?,
        None => Vec::new(),
    };
    Ok(Json(TeacherCertification {
        teacher_verified,
        application,
        files,
    }))
}

#[utoipa::path(get, path = "/api/v1/admin/teacher-applications", tag = "teacher-certification",
    params(ListQuery), responses((status = 200, body = TeacherApplicationList), (status = 401), (status = 403)), security(("bearer_auth" = [])))]
pub async fn list(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<TeacherApplicationList>, AppError> {
    require_super_admin(&state, &auth).await?;
    let (limit, offset) = query.pagination()?;
    if query
        .status
        .as_deref()
        .is_some_and(|s| !["pending", "approved", "rejected", "revoked"].contains(&s))
    {
        return Err(service::invalid("认证状态无效"));
    }
    let items = sqlx::query_as::<_, TeacherApplication>(
        "SELECT * FROM teacher_applications WHERE ($1::text IS NULL OR status = $1) ORDER BY submitted_at DESC, id DESC LIMIT $2 OFFSET $3",
    ).bind(&query.status).bind(limit).bind(offset).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    let total = sqlx::query_scalar(
        "SELECT count(*) FROM teacher_applications WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(&query.status)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(Json(TeacherApplicationList { items, total }))
}

#[utoipa::path(post, path = "/api/v1/me/teacher-certification/applications", tag = "teacher-certification",
    request_body = SubmitApplication, responses((status = 201, body = TeacherApplication), (status = 401), (status = 409), (status = 422)), security(("bearer_auth" = [])))]
pub async fn submit(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(input): ApiJson<SubmitApplication>,
) -> Result<(StatusCode, Json<TeacherApplication>), AppError> {
    Ok((
        StatusCode::CREATED,
        Json(service::submit(&state.pool, &auth, input).await?),
    ))
}

#[utoipa::path(post, path = "/api/v1/admin/teacher-applications/{id}/review", tag = "teacher-certification",
    params(("id" = Uuid, Path)), request_body = ReviewApplication,
    responses((status = 200, body = TeacherApplication), (status = 403), (status = 404), (status = 409), (status = 422)), security(("bearer_auth" = [])))]
pub async fn review(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<ReviewApplication>,
) -> Result<Json<TeacherApplication>, AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    Ok(Json(
        service::review(&state.pool, id, admin.id, input).await?,
    ))
}

#[utoipa::path(delete, path = "/api/v1/admin/users/{id}/teacher-certification", tag = "teacher-certification",
    params(("id" = Uuid, Path)), request_body = RevokeCertification,
    responses((status = 200), (status = 403), (status = 404), (status = 409), (status = 422)), security(("bearer_auth" = [])))]
pub async fn revoke(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(input): ApiJson<RevokeCertification>,
) -> Result<Json<serde_json::Value>, AppError> {
    let admin = require_super_admin(&state, &auth).await?;
    service::revoke(&state.pool, id, admin.id, &input.reason).await?;
    Ok(Json(serde_json::json!({ "teacher_verified": false })))
}

#[utoipa::path(get, path = "/api/v1/me/notifications", tag = "notifications",
    params(ListQuery), responses((status = 200, body = NotificationList), (status = 401)), security(("bearer_auth" = [])))]
pub async fn notifications(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<NotificationList>, AppError> {
    let (limit, offset) = query.pagination()?;
    let items = sqlx::query_as::<_, UserNotification>("SELECT * FROM user_notifications WHERE user_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3")
        .bind(auth.subject).bind(limit).bind(offset).fetch_all(&state.pool).await.map_err(AppError::internal)?;
    let (total, unread_count) = sqlx::query_as::<_, (i64,i64)>("SELECT count(*), count(*) FILTER (WHERE read_at IS NULL) FROM user_notifications WHERE user_id = $1")
        .bind(auth.subject).fetch_one(&state.pool).await.map_err(AppError::internal)?;
    Ok(Json(NotificationList {
        items,
        total,
        unread_count,
    }))
}

async fn application_files(
    state: &AppState,
    application: Uuid,
) -> Result<Vec<super::files::CertificationFile>, AppError> {
    sqlx::query_as("SELECT f.* FROM teacher_certification_files f JOIN teacher_application_files a ON a.file_id = f.id WHERE a.application_id = $1 ORDER BY a.position")
        .bind(application).fetch_all(&state.pool).await.map_err(AppError::internal)
}

async fn detail(
    state: &AppState,
    id: Uuid,
    owner: Option<Uuid>,
) -> Result<Json<TeacherApplicationDetail>, AppError> {
    let application = sqlx::query_as::<_, TeacherApplication>(
        "SELECT * FROM teacher_applications WHERE id = $1 AND ($2::uuid IS NULL OR user_id = $2)",
    )
    .bind(id)
    .bind(owner)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(|| AppError::not_found("申请不存在"))?;
    let files = application_files(state, id).await?;
    Ok(Json(TeacherApplicationDetail { application, files }))
}

#[utoipa::path(get, path = "/api/v1/me/teacher-certification/applications/{id}", tag = "teacher-certification",
    params(("id" = Uuid, Path)), responses((status = 200, body = TeacherApplicationDetail), (status = 401), (status = 404)), security(("bearer_auth" = [])))]
pub async fn own_detail(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<TeacherApplicationDetail>, AppError> {
    detail(&state, id, Some(auth.subject)).await
}

#[utoipa::path(get, path = "/api/v1/admin/teacher-applications/{id}", tag = "teacher-certification",
    params(("id" = Uuid, Path)), responses((status = 200, body = TeacherApplicationDetail), (status = 403), (status = 404)), security(("bearer_auth" = [])))]
pub async fn admin_detail(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<TeacherApplicationDetail>, AppError> {
    require_super_admin(&state, &auth).await?;
    detail(&state, id, None).await
}

#[utoipa::path(patch, path = "/api/v1/me/notifications/{id}/read", tag = "notifications",
    params(("id" = Uuid, Path)), responses((status = 200, body = UserNotification), (status = 401), (status = 404)), security(("bearer_auth" = [])))]
pub async fn read_notification(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<UserNotification>, AppError> {
    let notification = sqlx::query_as::<_, UserNotification>("UPDATE user_notifications SET read_at = COALESCE(read_at, now()) WHERE id = $1 AND user_id = $2 RETURNING *")
        .bind(id).bind(auth.subject).fetch_optional(&state.pool).await.map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("通知不存在"))?;
    Ok(Json(notification))
}
