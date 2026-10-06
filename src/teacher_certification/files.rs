use super::service::{conflict, invalid};
use crate::{
    admin::{AdminAuth, permissions},
    api::{ApiPath, ApiQuery},
    auth::extract::AuthUser,
    error::{AppError, ErrorCode},
    platform::storage::{ObjectContentType, ObjectKey, ObjectStore, PutOptions, StoragePrivacy},
    state::AppState,
};
use axum::{
    Json,
    body::Bytes,
    extract::{State, rejection::BytesRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::{io::Cursor, sync::Arc};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UploadQuery {
    pub kind: String,
}

#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct CertificationImage(pub Vec<u8>);

#[derive(Serialize, FromRow, ToSchema)]
pub struct CertificationFile {
    pub id: Uuid,
    pub kind: String,
    pub content_type: String,
    pub size_bytes: i64,
}

pub fn store(state: &AppState) -> Result<Arc<dyn ObjectStore>, AppError> {
    let space = "teacher-certification"
        .parse()
        .expect("constant space is valid");
    let store = state
        .object_storage
        .get(&space)
        .map_err(|_| unavailable())?;
    if store.policy().privacy() != StoragePrivacy::Private {
        return Err(unavailable());
    }
    Ok(store)
}

fn unavailable() -> AppError {
    AppError::unavailable(ErrorCode::ServiceUnavailable, "认证材料存储暂不可用")
}

#[utoipa::path(post, path = "/api/v1/me/teacher-certification/files", tag = "teacher-certification",
    params(UploadQuery), request_body(content((inline(CertificationImage) = "image/jpeg"), (inline(CertificationImage) = "image/png"), (inline(CertificationImage) = "image/webp"))),
    responses((status = 403, description = "phone_binding_required：需先绑定手机号"), (status = 201, body = CertificationFile), (status = 401), (status = 409), (status = 413), (status = 422), (status = 503)), security(("bearer_auth" = [])))]
pub async fn upload(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiQuery(query): ApiQuery<UploadQuery>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<CertificationFile>), AppError> {
    let body = body.map_err(|error| {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            AppError::request_error(ErrorCode::PayloadTooLarge, "图片不能超过10MB")
        } else {
            AppError::request_error(ErrorCode::InvalidRequestBody, "无法读取图片")
        }
    })?;
    if !["id_front", "id_back", "education", "language"].contains(&query.kind.as_str()) {
        return Err(invalid("材料类型无效"));
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let format = match content_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        _ => return Err(invalid("只支持JPEG、PNG和WebP图片")),
    };
    if body.is_empty() || body.len() > MAX_FILE_BYTES {
        return Err(invalid("图片需在10MB以内且不能为空"));
    }
    let bytes = body.clone();
    tokio::task::spawn_blocking(move || {
        if image::guess_format(&bytes).ok() != Some(format) {
            return Err(invalid("图片格式与内容不一致"));
        }
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        reader.decode().map_err(|_| invalid("图片损坏或像素过大"))?;
        Ok::<_, AppError>(())
    })
    .await
    .map_err(AppError::internal)??;
    let storage = store(&state)?;
    let id = Uuid::now_v7();
    let key = ObjectKey::parse(format!("materials/{id}")).map_err(AppError::internal)?;
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let owner = sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE id = $1 AND status = 'active' AND security_version = $2 FOR UPDATE")
        .bind(auth.subject).bind(auth.security_version).fetch_optional(&mut *tx).await.map_err(AppError::internal)?;
    if owner.is_none() {
        return Err(AppError::unauthorized(
            ErrorCode::InvalidToken,
            "invalid token",
        ));
    }
    let blocked = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM teacher_applications WHERE user_id = $1 AND status IN ('pending','approved')) OR EXISTS(SELECT 1 FROM teacher_profiles WHERE user_id = $1 AND verified) OR NOT EXISTS(SELECT 1 FROM user_roles WHERE user_id = $1 AND role = 'student')")
        .bind(auth.subject).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if blocked {
        return Err(conflict());
    }
    let unclaimed: i64 = sqlx::query_scalar("SELECT count(*) FROM teacher_certification_files WHERE user_id = $1 AND expires_at IS NOT NULL AND state <> 'delete_pending'")
        .bind(auth.subject).fetch_one(&mut *tx).await.map_err(AppError::internal)?;
    if unclaimed >= 44 {
        return Err(invalid("待提交材料过多，请删除不需要的图片后重试"));
    }
    sqlx::query("INSERT INTO teacher_certification_files (id,user_id,kind,object_key,content_type,size_bytes) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(auth.subject).bind(&query.kind).bind(key.as_str()).bind(content_type).bind(body.len() as i64)
        .execute(&mut *tx).await.map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    let options = PutOptions::new(Some(
        ObjectContentType::parse(content_type).map_err(AppError::internal)?,
    ));
    if storage.put(&key, body.to_vec(), options).await.is_err() {
        sqlx::query(
            "UPDATE teacher_certification_files SET state = 'delete_pending' WHERE id = $1",
        )
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(AppError::internal)?;
        return Err(unavailable());
    }
    let file = sqlx::query_as::<_, CertificationFile>("UPDATE teacher_certification_files SET state = 'ready' WHERE id = $1 AND user_id = $2 AND state = 'uploading' RETURNING *")
        .bind(id).bind(auth.subject).fetch_optional(&state.pool).await.map_err(AppError::internal)?;
    match file {
        Some(file) => Ok((StatusCode::CREATED, Json(file))),
        None => {
            sqlx::query(
                "UPDATE teacher_certification_files SET state = 'delete_pending' WHERE id = $1",
            )
            .bind(id)
            .execute(&state.pool)
            .await
            .map_err(AppError::internal)?;
            Err(conflict())
        }
    }
}

async fn read(state: &AppState, id: Uuid, owner: Option<Uuid>) -> Result<Response, AppError> {
    let record = sqlx::query_as::<_, (String, String)>("SELECT object_key, content_type FROM teacher_certification_files f WHERE id = $1 AND state = 'ready' AND (expires_at IS NULL OR expires_at > now()) AND (($2::uuid IS NOT NULL AND user_id = $2) OR ($2::uuid IS NULL AND EXISTS(SELECT 1 FROM teacher_application_files a WHERE a.file_id = f.id)))")
        .bind(id).bind(owner).fetch_optional(&state.pool).await.map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("材料不存在"))?;
    let body = store(state)?
        .read(&ObjectKey::parse(record.0).map_err(AppError::internal)?)
        .await
        .map_err(|_| unavailable())?;
    Ok((
        [
            (header::CONTENT_TYPE, record.1),
            (header::CACHE_CONTROL, "no-store".into()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        body,
    )
        .into_response())
}

#[utoipa::path(get, path = "/api/v1/me/teacher-certification/files/{id}", tag = "teacher-certification",
    params(("id" = Uuid, Path)), responses((status = 403, description = "phone_binding_required：需先绑定手机号"), (status = 200, content((inline(CertificationImage) = "image/jpeg"), (inline(CertificationImage) = "image/png"), (inline(CertificationImage) = "image/webp"))), (status = 401), (status = 404)), security(("bearer_auth" = [])))]
pub async fn read_own(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, AppError> {
    read(&state, id, Some(auth.subject)).await
}

#[utoipa::path(get, path = "/api/v1/admin/teacher-certification/files/{id}", tag = "teacher-certification",
    params(("id" = Uuid, Path)), responses((status = 200, content((inline(CertificationImage) = "image/jpeg"), (inline(CertificationImage) = "image/png"), (inline(CertificationImage) = "image/webp"))), (status = 403), (status = 404)), security(("bearer_auth" = [])))]
pub async fn read_admin(
    State(state): State<AppState>,
    auth: AdminAuth,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, AppError> {
    permissions::load(&state, &auth)
        .await?
        .require("teacherapply.read_sensitive")?;
    let response = read(&state, id, None).await?;
    // 对象存储读取可能耗时；交付字节前再次核验撤权、禁用及会话状态。
    permissions::reload(&state, &auth)
        .await?
        .require("teacherapply.read_sensitive")?;
    Ok(response)
}

#[utoipa::path(delete, path = "/api/v1/me/teacher-certification/files/{id}", tag = "teacher-certification",
    params(("id" = Uuid, Path)), responses((status = 403, description = "phone_binding_required：需先绑定手机号"), (status = 204), (status = 401), (status = 404), (status = 409)), security(("bearer_auth" = [])))]
pub async fn remove(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, AppError> {
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let attached = sqlx::query_scalar::<_, bool>("SELECT expires_at IS NULL FROM teacher_certification_files WHERE id = $1 AND user_id = $2 FOR UPDATE")
        .bind(id).bind(auth.subject).fetch_optional(&mut *tx).await.map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("材料不存在"))?;
    if attached {
        return Err(conflict());
    }
    sqlx::query("UPDATE teacher_certification_files SET state = 'delete_pending' WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::internal)?;
    tx.commit().await.map_err(AppError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}
