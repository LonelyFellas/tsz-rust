use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;
use tracing::Instrument;
use uuid::Uuid;

use crate::platform::{
    is_unique_violation,
    storage::{ObjectContentType, ObjectKey, ObjectStore, PutOptions, StorageError},
};

use super::{
    dto::{
        AUDIO_ASSET_CONTENT_TYPES, AudioAsset, AudioAssetGender, AudioAssetLocale,
        AudioAssetUrlResponse, AudioUploadTicket, ConfirmAudioAssetRequest,
        ConfirmAudioAssetResponse, CreateAudioUploadRequest, CreateAudioUploadResponse,
        audio_extension,
    },
    repository::{AudioAssetRepository, NewAudioAsset, SOURCE_KEY_UNIQUE},
};

/// 未确认对象的暂存前缀。bucket 生命周期规则只按这个前缀回收孤儿，
/// 已确认资产因此必须搬出该前缀，见 `ops/audio-asset-lifecycle/README.md`。
const PENDING_PREFIX: &str = "uploads";
/// 已确认资产的前缀，不受生命周期规则影响。
const ASSET_PREFIX: &str = "assets";
/// 与数据库 CHECK 一致的展示名长度上限（码点）。
const MAX_ORIGINAL_NAME_CHARS: usize = 120;

#[derive(Debug, Error)]
pub enum AudioAssetServiceError {
    #[error("audio storage is not configured")]
    StorageNotConfigured,
    #[error("unsupported audio content type")]
    UnsupportedContentType,
    #[error("audio file exceeds the size limit")]
    FileTooLarge,
    #[error("invalid audio upload key")]
    InvalidKey,
    #[error("audio upload was not completed")]
    UploadNotCompleted,
    #[error("audio asset original name is invalid")]
    InvalidOriginalName,
    #[error("audio asset not found")]
    NotFound,
    #[error(transparent)]
    Storage(StorageError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
}

impl From<StorageError> for AudioAssetServiceError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::ObjectNotFound { .. } => Self::UploadNotCompleted,
            StorageError::ObjectTooLarge { .. } => Self::FileTooLarge,
            other => Self::Storage(other),
        }
    }
}

#[derive(Clone)]
pub struct AudioAssetService {
    repository: AudioAssetRepository,
    storage: Option<Arc<dyn ObjectStore>>,
}

impl AudioAssetService {
    pub fn new(repository: AudioAssetRepository, storage: Option<Arc<dyn ObjectStore>>) -> Self {
        Self {
            repository,
            storage,
        }
    }

    fn storage(&self) -> Result<&Arc<dyn ObjectStore>, AudioAssetServiceError> {
        self.storage
            .as_ref()
            .ok_or(AudioAssetServiceError::StorageNotConfigured)
    }

    /// 签发直传许可。对象键与 `Content-Type` 都由服务端定，客户端必须原样回发签名 headers。
    pub async fn create_upload(
        &self,
        request: CreateAudioUploadRequest,
    ) -> Result<CreateAudioUploadResponse, AudioAssetServiceError> {
        let storage = self.storage()?;
        let (content_type, extension) = canonical_content_type(&request.content_type)
            .ok_or(AudioAssetServiceError::UnsupportedContentType)?;
        let max_bytes = storage.policy().max_object_size();
        if request.size > max_bytes {
            return Err(AudioAssetServiceError::FileTooLarge);
        }
        let key = ObjectKey::generate(PENDING_PREFIX, Some(extension))
            .expect("常量前缀与白名单扩展名构成合法对象键");
        let options = PutOptions::new(Some(
            ObjectContentType::parse(content_type).expect("白名单 MIME 是合法媒体类型"),
        ));
        let signed = storage.presign_write(&key, request.size, options).await?;
        Ok(CreateAudioUploadResponse {
            upload: AudioUploadTicket {
                key: key.as_str().to_owned(),
                url: signed.url().to_owned(),
                headers: signed.headers().clone(),
                expires_in: signed.expires_in().as_secs(),
                max_bytes,
            },
        })
    }

    /// 核验真实对象后登记资产：`stat` 复核类型与大小，再把对象搬出暂存前缀。
    pub async fn confirm(
        &self,
        admin_id: Uuid,
        request: ConfirmAudioAssetRequest,
    ) -> Result<ConfirmAudioAssetResponse, AudioAssetServiceError> {
        let storage = self.storage()?;
        let original_name = normalize_original_name(&request.original_name)
            .ok_or(AudioAssetServiceError::InvalidOriginalName)?;
        let pending_key =
            parse_pending_key(&request.key).ok_or(AudioAssetServiceError::InvalidKey)?;

        // 重放：confirm 的 201 丢在网络上时前端会重试，而那时暂存对象已经删掉了。
        // 不按 source_key 回查就会报「没传完」，前端只能让用户重传，
        // 已登记的那份资产与对象则成为无人认领的孤儿（`assets/` 本期没有回收方）。
        if let Some(asset) = self
            .repository
            .find_by_source_key(pending_key.as_str())
            .await?
        {
            return Ok(ConfirmAudioAssetResponse { asset });
        }

        let metadata = storage.stat(&pending_key).await?;
        let (content_type, extension) = metadata
            .content_type
            .as_deref()
            .and_then(canonical_content_type)
            .ok_or(AudioAssetServiceError::UnsupportedContentType)?;
        // 预签名对空内容同样有效，落库前先挡掉：0 字节说明 PUT 没有真正写入。
        if metadata.content_length == 0 {
            return Err(AudioAssetServiceError::UploadNotCompleted);
        }
        let size_bytes = i64::try_from(metadata.content_length)
            .map_err(|_| AudioAssetServiceError::FileTooLarge)?;

        // 搬运与落库 detach 到独立任务：客户端中途 abort（关页面、刷新、断网）时 axum 会丢弃
        // handler future，就地跑就会在 copy 与 insert 之间留下 `assets/` 下无 DB 行的对象——
        // 那个前缀没有生命周期规则，底座又没有 list，这种孤儿永远发现不了。与 speech 试听同款处置。
        let service = self.clone();
        let storage = Arc::clone(storage);
        let promotion = tokio::spawn(
            async move {
                service
                    .promote(
                        admin_id,
                        storage,
                        pending_key,
                        content_type,
                        extension,
                        size_bytes,
                        request.locale,
                        request.gender,
                        original_name,
                    )
                    .await
            }
            .instrument(tracing::Span::current()),
        );
        promotion.await?
    }

    #[allow(clippy::too_many_arguments)]
    async fn promote(
        &self,
        admin_id: Uuid,
        storage: Arc<dyn ObjectStore>,
        pending_key: ObjectKey,
        content_type: &'static str,
        extension: &'static str,
        size_bytes: i64,
        locale: AudioAssetLocale,
        gender: AudioAssetGender,
        original_name: String,
    ) -> Result<ConfirmAudioAssetResponse, AudioAssetServiceError> {
        let id = Uuid::now_v7();
        let asset_key = ObjectKey::parse(format!("{ASSET_PREFIX}/{id}.{extension}"))
            .expect("常量前缀与 UUID 构成合法对象键");

        // copy 是「read 源 + put 目标」：OSS 提交了 PUT 但响应丢了同样会返回 Err，
        // 此时目标对象已经真实存在。delete 是幂等的，没写成也只是一次 no-op。
        if let Err(error) = storage.copy(&pending_key, &asset_key).await {
            compensate_delete(&storage, &asset_key).await;
            return Err(error.into());
        }

        let created_at = match self
            .repository
            .insert(NewAudioAsset {
                id,
                object_key: asset_key.as_str(),
                source_key: pending_key.as_str(),
                content_type,
                size_bytes,
                locale,
                gender,
                original_name: &original_name,
                created_by_admin_id: admin_id,
            })
            .await
        {
            Ok(created_at) => created_at,
            Err(error) => {
                // 落库失败则回收刚复制出来的正式对象；暂存对象留给生命周期规则。
                compensate_delete(&storage, &asset_key).await;
                // 并发 confirm 撞上 source_key 唯一约束：另一边已经登记好了，返回它那条，
                // 不把同一段录音登记成两份资产。
                if is_unique_violation(&error, SOURCE_KEY_UNIQUE)
                    && let Some(asset) = self
                        .repository
                        .find_by_source_key(pending_key.as_str())
                        .await?
                {
                    return Ok(ConfirmAudioAssetResponse { asset });
                }
                return Err(AudioAssetServiceError::Database(error));
            }
        };
        // 暂存对象已无用；删不掉也不影响正确性，生命周期规则会兜底。
        compensate_delete(&storage, &pending_key).await;

        Ok(ConfirmAudioAssetResponse {
            asset: AudioAsset {
                id,
                locale,
                gender,
                content_type: content_type.to_owned(),
                size_bytes,
                duration_ms: None,
                original_name,
                created_at,
            },
        })
    }

    /// 签发短期只读 URL。资产还没有与词条的引用关系，因此本期只有创建者可读；
    /// 引用关系落地后改为「能读该资产所在词条即可读」。
    pub async fn presign_url(
        &self,
        admin_id: Uuid,
        asset_id: Uuid,
    ) -> Result<AudioAssetUrlResponse, AudioAssetServiceError> {
        let storage = self.storage()?;
        let record = self
            .repository
            .find(asset_id)
            .await?
            .ok_or(AudioAssetServiceError::NotFound)?;
        // 非创建者与「不存在」返回同一个错误，不把资产是否存在变成可探测的信号。
        if record.created_by_admin_id != admin_id {
            return Err(AudioAssetServiceError::NotFound);
        }
        let key = ObjectKey::parse(record.object_key).map_err(|error| {
            AudioAssetServiceError::Database(sqlx::Error::Decode(Box::new(error)))
        })?;
        let signed = storage.presign_read(&key).await?;
        let ttl = signed.expires_in();
        Ok(AudioAssetUrlResponse {
            url: signed.url().to_owned(),
            expires_at: DateTime::<Utc>::from(std::time::SystemTime::now() + ttl),
            url_expires_in_seconds: ttl.as_secs(),
        })
    }
}

/// 展示名与 annotation / headword 同口径：trim 后判空、限长、拒控制字符。
/// 不挡控制字符的话，含 NUL 的名字会被 Postgres 拒收，本该 400 的输入变成 500。
fn normalize_original_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_ORIGINAL_NAME_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return None;
    }
    Some(trimmed.to_owned())
}

/// 归一到白名单里的 MIME 与扩展名，避免把客户端写法（大小写、charset 参数）带进签名或数据库。
fn canonical_content_type(value: &str) -> Option<(&'static str, &'static str)> {
    let extension = audio_extension(value)?;
    AUDIO_ASSET_CONTENT_TYPES
        .into_iter()
        .find(|(_, candidate)| *candidate == extension)
}

/// 只接受本服务签发过的暂存键形状，避免把任意对象（含别人的正式资产）登记成新资产。
fn parse_pending_key(value: &str) -> Option<ObjectKey> {
    let (name, extension) = value
        .strip_prefix(PENDING_PREFIX)?
        .strip_prefix('/')?
        .rsplit_once('.')?;
    Uuid::parse_str(name).ok()?;
    if !AUDIO_ASSET_CONTENT_TYPES
        .into_iter()
        .any(|(_, candidate)| candidate == extension)
    {
        return None;
    }
    ObjectKey::parse(value).ok()
}

async fn compensate_delete(storage: &Arc<dyn ObjectStore>, key: &ObjectKey) {
    if storage.delete(key).await.is_err() {
        tracing::warn!(
            object_key = %key,
            error_kind = "storage_delete",
            "audio asset compensation failed"
        );
    }
}
