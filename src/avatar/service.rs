use super::{
    IO_TIMEOUT, MAX_BYTES,
    dto::{AvatarUpload, AvatarUploadRequest, AvatarUploadResponse},
    repository,
};
use crate::{
    auth::{
        extract::AuthUser,
        handler::{UserProfile, load_user_profile},
    },
    error::{AppError, ErrorCode},
    platform::storage::{
        ObjectContentType, ObjectKey, ObjectStore, PutOptions, StorageError, StoragePrivacy,
    },
    state::AppState,
    user::repository::UserRepository,
};
use std::sync::{Arc, OnceLock};
use tokio::{sync::Semaphore, time::timeout};
use uuid::Uuid;

pub fn unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::AvatarStorageUnavailable,
        "avatar storage unavailable",
    )
}

pub fn store(state: &AppState) -> Result<Arc<dyn ObjectStore>, AppError> {
    let disabled = || {
        AppError::request_error(
            ErrorCode::AvatarStorageNotConfigured,
            "avatar storage not configured",
        )
    };
    if state.avatar_public_base_url.is_none() {
        return Err(disabled());
    }
    let store = state
        .object_storage
        .get(&"avatars".parse().expect("constant space is valid"))
        .map_err(|_| disabled())?;
    if store.policy().privacy() != StoragePrivacy::Private
        || store.policy().max_object_size() != MAX_BYTES
        || store.policy().presign_ttl().as_secs() > 600
    {
        return Err(disabled());
    }
    Ok(store)
}

pub async fn current_profile(state: &AppState, auth: &AuthUser) -> Result<UserProfile, AppError> {
    let invalid_token = || AppError::unauthorized(ErrorCode::InvalidToken, "invalid token");
    let user = UserRepository::new(state.pool.clone())
        .get_by_id(&auth.subject)
        .await
        .map_err(|error| match error {
            crate::user::repository::UserError::NotFound => invalid_token(),
            _ => AppError::internal(error),
        })?;
    if user.security_version != auth.security_version
        || user.status != crate::user::model::UserStatus::Active
    {
        return Err(invalid_token());
    }
    load_user_profile(state, &user).await
}

pub async fn create_upload(
    state: &AppState,
    auth: &AuthUser,
    request: AvatarUploadRequest,
) -> Result<AvatarUploadResponse, AppError> {
    let store = store(state)?;
    let (_, extension) = super::image::format(&request.content_type)?;
    if request.size > MAX_BYTES {
        return Err(AppError::request_error(
            ErrorCode::AvatarFileTooLarge,
            "avatar file too large",
        ));
    }
    if request.size == 0 {
        return Err(AppError::validation(
            ErrorCode::InvalidAvatarSize,
            "size",
            "invalid avatar size",
        ));
    }
    let id = Uuid::now_v7();
    let key = ObjectKey::parse(format!("uploads/avatars/{id}/original.{extension}"))
        .map_err(AppError::internal)?;
    let ttl = store.policy().presign_ttl().as_secs();
    repository::create(&state.pool, auth, id, key.as_str(), &request, ttl as i64).await?;
    let options = PutOptions::new(Some(
        ObjectContentType::parse(request.content_type).map_err(AppError::internal)?,
    ));
    let signed = timeout(IO_TIMEOUT, store.presign_write(&key, request.size, options))
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    let permit_remaining =
        repository::signed(&state.pool, auth, id, signed.expires_at().into()).await?;
    let signature_remaining = signed
        .expires_at()
        .duration_since(std::time::SystemTime::now())
        .map_err(|_| repository::invalid_key())?
        .as_secs();
    let expires_in = permit_remaining
        .min(signature_remaining)
        .min(signed.expires_in().as_secs());
    if expires_in == 0 {
        return Err(repository::invalid_key());
    }
    Ok(AvatarUploadResponse {
        upload: AvatarUpload {
            key: key.as_str().to_owned(),
            url: signed.url().to_owned(),
            headers: signed.headers().clone(),
            expires_in,
            max_bytes: MAX_BYTES,
        },
    })
}

pub async fn confirm(
    state: &AppState,
    auth: &AuthUser,
    key: &str,
) -> Result<UserProfile, AppError> {
    let store = store(state)?;
    let upload = repository::find(&state.pool, auth, key).await?;
    if upload.state == "confirmed" {
        return current_profile(state, auth).await;
    }
    static PROCESSING: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = timeout(
        IO_TIMEOUT,
        PROCESSING
            .get_or_init(|| Arc::new(Semaphore::new(4)))
            .clone()
            .acquire_owned(),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(AppError::internal)?;
    let source = ObjectKey::parse(upload.source_key).map_err(AppError::internal)?;
    let bytes = timeout(IO_TIMEOUT, store.read(&source))
        .await
        .map_err(|_| unavailable())?
        .map_err(|error| match error {
            StorageError::ObjectNotFound { .. } => AppError::conflict(
                ErrorCode::AvatarUploadNotCompleted,
                Some("key"),
                "avatar upload not completed",
            ),
            StorageError::ObjectTooLarge { .. } => {
                AppError::request_error(ErrorCode::AvatarFileTooLarge, "avatar file too large")
            }
            _ => unavailable(),
        })?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(AppError::request_error(
            ErrorCode::AvatarFileTooLarge,
            "avatar file too large",
        ));
    }
    if bytes.is_empty() || bytes.len() as i64 != upload.size_bytes {
        return Err(AppError::validation(
            ErrorCode::InvalidAvatarSize,
            "size",
            "invalid avatar size",
        ));
    }
    let processed = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        super::image::normalize_avatar(&bytes, &upload.declared_type)
    })
    .await
    .map_err(AppError::internal)??;
    let key = ObjectKey::parse(format!("images/avatars/{}.webp", Uuid::now_v7()))
        .map_err(AppError::internal)?;
    let task = Uuid::now_v7();
    if !repository::register_candidate(&state.pool, auth, upload.id, key.as_str(), task).await? {
        return current_profile(state, auth).await;
    }
    timeout(
        IO_TIMEOUT,
        store.put(
            &key,
            processed,
            PutOptions::new(Some(
                ObjectContentType::parse("image/webp").expect("constant content type is valid"),
            )),
        ),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    repository::mark_candidate_complete(&state.pool, task).await?;
    let url = format!(
        "{}/{}",
        state
            .avatar_public_base_url
            .as_ref()
            .expect("store requires base URL")
            .trim_end_matches('/'),
        upload.id
    );
    let mut tx = state.pool.begin().await.map_err(AppError::internal)?;
    let applied =
        repository::commit_avatar_in(&mut tx, auth, upload.id, task, key.as_str(), &url).await?;
    tx.commit().await.map_err(AppError::internal)?;
    if !applied {
        return Err(repository::invalid_key());
    }
    current_profile(state, auth).await
}

pub async fn read_public(state: &AppState, id: Uuid) -> Result<Vec<u8>, AppError> {
    let query = "SELECT a.canonical_key FROM avatar_uploads a JOIN users u ON u.id=a.user_id AND u.avatar_upload_id=a.id WHERE a.id=$1 AND a.state='confirmed'";
    let key: String = sqlx::query_scalar(query)
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("avatar not found"))?;
    let bytes = timeout(
        IO_TIMEOUT,
        store(state)?.read(&ObjectKey::parse(&key).map_err(AppError::internal)?),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    let current: Option<String> = sqlx::query_scalar(query)
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?;
    if current.as_deref() != Some(&key) {
        return Err(AppError::not_found("avatar not found"));
    }
    Ok(bytes)
}
