use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::platform::storage::{
    ObjectContentType, ObjectKey, ObjectStore, PutOptions, StorageError,
};

use super::{
    dto::{
        AUDIO_ASSET_CONTENT_TYPES, AudioAsset, AudioAssetUrlResponse, AudioUploadTicket,
        ConfirmAudioAssetRequest, ConfirmAudioAssetResponse, CreateAudioUploadRequest,
        CreateAudioUploadResponse, audio_extension,
    },
    repository::{AudioAssetRepository, NewAudioAsset},
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
        // 与 `lexicon.audio_assets` 的 CHECK 同口径：应用层不挡，超长就会撞成 500。
        if !(1..=MAX_ORIGINAL_NAME_CHARS).contains(&request.original_name.chars().count()) {
            return Err(AudioAssetServiceError::InvalidOriginalName);
        }
        let pending_key =
            parse_pending_key(&request.key).ok_or(AudioAssetServiceError::InvalidKey)?;
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

        let id = Uuid::now_v7();
        let asset_key = ObjectKey::parse(format!("{ASSET_PREFIX}/{id}.{extension}"))
            .expect("常量前缀与 UUID 构成合法对象键");
        storage.copy(&pending_key, &asset_key).await?;

        let created_at = match self
            .repository
            .insert(NewAudioAsset {
                id,
                object_key: asset_key.as_str(),
                content_type,
                size_bytes,
                locale: request.locale.as_str(),
                gender: request.gender.as_str(),
                original_name: &request.original_name,
                created_by_admin_id: admin_id,
            })
            .await
        {
            Ok(created_at) => created_at,
            Err(error) => {
                // 落库失败则回收刚复制出来的正式对象；暂存对象留给生命周期规则。
                compensate_delete(storage, &asset_key).await;
                return Err(AudioAssetServiceError::Database(error));
            }
        };
        // 暂存对象已无用；删不掉也不影响正确性，生命周期规则会兜底。
        compensate_delete(storage, &pending_key).await;

        Ok(ConfirmAudioAssetResponse {
            asset: AudioAsset {
                id,
                locale: request.locale,
                gender: request.gender,
                content_type: content_type.to_owned(),
                size_bytes,
                duration_ms: None,
                original_name: request.original_name,
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
