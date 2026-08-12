use std::{sync::Arc, time::Duration};

use async_trait::async_trait;

use super::{
    error::StorageError,
    model::{ObjectKey, ObjectMetadata, PresignedRequest, PutOptions, StoragePolicy, StorageSpace},
};

#[derive(Debug, Clone)]
pub(crate) struct BackendWriteOptions {
    pub content_type: Option<String>,
    pub cache_control: Option<String>,
    pub content_length: u64,
}

#[derive(Debug)]
pub(crate) struct BackendReadResult {
    pub body: Vec<u8>,
    pub metadata: ObjectMetadata,
}

#[async_trait]
pub(crate) trait ObjectStoreBackend: Send + Sync {
    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: &BackendWriteOptions,
    ) -> Result<ObjectMetadata, StorageError>;

    /// 最多读取 `limit` 字节；后端不得先无界缓冲再截断。
    async fn read(&self, key: &ObjectKey, limit: u64) -> Result<BackendReadResult, StorageError>;

    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError>;

    async fn presign_read(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedRequest, StorageError>;

    async fn presign_write(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
        options: &BackendWriteOptions,
    ) -> Result<PresignedRequest, StorageError>;

    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError>;
}

/// 领域层依赖的稳定对象存储接口；不包含 list、prefix delete 或 bucket 管理能力。
#[async_trait]
pub trait ObjectStore: Send + Sync {
    fn space(&self) -> &StorageSpace;
    fn policy(&self) -> &StoragePolicy;

    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: PutOptions,
    ) -> Result<ObjectMetadata, StorageError>;

    async fn read(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError>;

    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError>;

    async fn presign_read(&self, key: &ObjectKey) -> Result<PresignedRequest, StorageError>;

    async fn presign_write(
        &self,
        key: &ObjectKey,
        content_length: u64,
        options: PutOptions,
    ) -> Result<PresignedRequest, StorageError>;

    async fn copy(
        &self,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError>;

    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError>;
}

pub(crate) struct StorageService {
    space: StorageSpace,
    policy: StoragePolicy,
    backend: Arc<dyn ObjectStoreBackend>,
}

impl StorageService {
    pub(crate) fn new(
        space: StorageSpace,
        policy: StoragePolicy,
        backend: Arc<dyn ObjectStoreBackend>,
    ) -> Self {
        Self {
            space,
            policy,
            backend,
        }
    }

    fn ensure_size(&self, actual: u64) -> Result<(), StorageError> {
        let max = self.policy.max_object_size();
        if actual > max {
            return Err(StorageError::ObjectTooLarge {
                space: self.space.clone(),
                max,
                actual,
            });
        }
        Ok(())
    }

    fn bounded_read_limit(&self) -> u64 {
        self.policy.max_object_size().saturating_add(1)
    }

    fn write_options(&self, content_length: u64, options: &PutOptions) -> BackendWriteOptions {
        BackendWriteOptions {
            content_type: options
                .content_type()
                .map(|content_type| content_type.as_str().to_owned()),
            cache_control: self
                .policy
                .cache_control()
                .map(|cache_control| cache_control.as_str().to_owned()),
            content_length,
        }
    }
}

#[async_trait]
impl ObjectStore for StorageService {
    fn space(&self) -> &StorageSpace {
        &self.space
    }

    fn policy(&self) -> &StoragePolicy {
        &self.policy
    }

    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: PutOptions,
    ) -> Result<ObjectMetadata, StorageError> {
        let content_length = u64::try_from(body.len()).unwrap_or(u64::MAX);
        self.ensure_size(content_length)?;
        let metadata = self
            .backend
            .put(key, body, &self.write_options(content_length, &options))
            .await?;
        self.ensure_size(metadata.content_length)?;
        Ok(metadata)
    }

    async fn read(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
        let result = self.backend.read(key, self.bounded_read_limit()).await?;
        self.ensure_size(u64::try_from(result.body.len()).unwrap_or(u64::MAX))?;
        self.ensure_size(result.metadata.content_length)?;
        Ok(result.body)
    }

    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        let metadata = self.backend.stat(key).await?;
        self.ensure_size(metadata.content_length)?;
        Ok(metadata)
    }

    async fn presign_read(&self, key: &ObjectKey) -> Result<PresignedRequest, StorageError> {
        self.backend
            .presign_read(key, self.policy.presign_ttl())
            .await
    }

    async fn presign_write(
        &self,
        key: &ObjectKey,
        content_length: u64,
        options: PutOptions,
    ) -> Result<PresignedRequest, StorageError> {
        self.ensure_size(content_length)?;
        self.backend
            .presign_write(
                key,
                self.policy.presign_ttl(),
                &self.write_options(content_length, &options),
            )
            .await
    }

    async fn copy(
        &self,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        let source = self.backend.read(source, self.bounded_read_limit()).await?;
        let content_length = u64::try_from(source.body.len()).unwrap_or(u64::MAX);
        self.ensure_size(content_length)?;
        self.ensure_size(source.metadata.content_length)?;
        let destination_metadata = self
            .backend
            .put(
                destination,
                source.body,
                &BackendWriteOptions {
                    content_type: source.metadata.content_type,
                    cache_control: self
                        .policy
                        .cache_control()
                        .map(|cache_control| cache_control.as_str().to_owned()),
                    content_length,
                },
            )
            .await?;
        self.ensure_size(destination_metadata.content_length)?;
        Ok(destination_metadata)
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        self.backend.delete(key).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::platform::storage::{CacheControl, StorageOperation, StoragePrivacy};

    struct ReplacedObjectBackend {
        space: StorageSpace,
        body: Vec<u8>,
        read_limits: Mutex<Vec<u64>>,
        put_count: AtomicUsize,
        put_content_types: Mutex<Vec<Option<String>>>,
    }

    impl ReplacedObjectBackend {
        fn new(space: StorageSpace, body: Vec<u8>) -> Self {
            Self {
                space,
                body,
                read_limits: Mutex::new(Vec::new()),
                put_count: AtomicUsize::new(0),
                put_content_types: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ObjectStoreBackend for ReplacedObjectBackend {
        async fn put(
            &self,
            _key: &ObjectKey,
            _body: Vec<u8>,
            options: &BackendWriteOptions,
        ) -> Result<ObjectMetadata, StorageError> {
            self.put_count.fetch_add(1, Ordering::SeqCst);
            self.put_content_types
                .lock()
                .expect("测试锁未中毒")
                .push(options.content_type.clone());
            Ok(ObjectMetadata {
                content_length: options.content_length,
                content_type: options.content_type.clone(),
                cache_control: options.cache_control.clone(),
                etag: Some("destination".to_owned()),
                last_modified: None,
            })
        }

        async fn read(
            &self,
            _key: &ObjectKey,
            limit: u64,
        ) -> Result<BackendReadResult, StorageError> {
            self.read_limits.lock().expect("测试锁未中毒").push(limit);
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let body = self.body[..self.body.len().min(limit)].to_vec();
            Ok(BackendReadResult {
                metadata: ObjectMetadata {
                    content_length: u64::try_from(self.body.len()).unwrap_or(u64::MAX),
                    content_type: Some("application/octet-stream".to_owned()),
                    cache_control: None,
                    etag: Some("source-after-replacement".to_owned()),
                    last_modified: None,
                },
                body,
            })
        }

        async fn stat(&self, _key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
            // 模拟 stat 时对象仍小，随后被并发替换为 `body` 中的大对象。
            Ok(ObjectMetadata {
                content_length: 1,
                content_type: Some("text/plain".to_owned()),
                cache_control: None,
                etag: Some("source-before-replacement".to_owned()),
                last_modified: None,
            })
        }

        async fn presign_read(
            &self,
            _key: &ObjectKey,
            _expires_in: Duration,
        ) -> Result<PresignedRequest, StorageError> {
            Err(StorageError::Unsupported {
                space: self.space.clone(),
                operation: StorageOperation::PresignRead,
            })
        }

        async fn presign_write(
            &self,
            _key: &ObjectKey,
            _expires_in: Duration,
            _options: &BackendWriteOptions,
        ) -> Result<PresignedRequest, StorageError> {
            Err(StorageError::Unsupported {
                space: self.space.clone(),
                operation: StorageOperation::PresignWrite,
            })
        }

        async fn delete(&self, _key: &ObjectKey) -> Result<(), StorageError> {
            Ok(())
        }
    }

    fn test_service(backend: Arc<ReplacedObjectBackend>) -> StorageService {
        let policy = StoragePolicy::new(
            StoragePrivacy::Private,
            4,
            Duration::from_secs(60),
            Some(CacheControl::parse("private, max-age=60").expect("固定策略合法")),
        )
        .expect("固定策略合法");
        StorageService::new(backend.space.clone(), policy, backend)
    }

    #[tokio::test]
    async fn read_never_buffers_more_than_max_plus_one() {
        let backend = Arc::new(ReplacedObjectBackend::new(
            StorageSpace::parse("attachments").expect("固定空间合法"),
            vec![0; 64],
        ));
        let service = test_service(backend.clone());
        let key = ObjectKey::parse("messages/a.bin").expect("固定键合法");

        assert!(matches!(
            service.read(&key).await,
            Err(StorageError::ObjectTooLarge {
                max: 4,
                actual: 5,
                ..
            })
        ));
        assert_eq!(*backend.read_limits.lock().expect("测试锁未中毒"), vec![5]);
    }

    #[tokio::test]
    async fn copy_does_not_create_destination_after_concurrent_oversize_replacement() {
        let backend = Arc::new(ReplacedObjectBackend::new(
            StorageSpace::parse("attachments").expect("固定空间合法"),
            vec![0; 64],
        ));
        let service = test_service(backend.clone());
        let source = ObjectKey::parse("messages/source.bin").expect("固定键合法");
        let destination = ObjectKey::parse("messages/destination.bin").expect("固定键合法");

        assert!(matches!(
            service.copy(&source, &destination).await,
            Err(StorageError::ObjectTooLarge {
                max: 4,
                actual: 5,
                ..
            })
        ));
        assert_eq!(backend.put_count.load(Ordering::SeqCst), 0);
        assert_eq!(*backend.read_limits.lock().expect("测试锁未中毒"), vec![5]);
    }

    #[tokio::test]
    async fn copy_uses_body_and_metadata_from_the_same_read_response() {
        let space = StorageSpace::parse("attachments").expect("固定空间合法");
        let backend = Arc::new(ReplacedObjectBackend::new(space, b"new!".to_vec()));
        let service = test_service(backend.clone());
        let source = ObjectKey::parse("messages/source.bin").expect("固定键合法");
        let destination = ObjectKey::parse("messages/destination.bin").expect("固定键合法");

        assert_eq!(
            service
                .copy(&source, &destination)
                .await
                .expect("同一读取响应的正文与元数据应一起复制")
                .content_type
                .as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(backend.put_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            *backend.put_content_types.lock().expect("测试锁未中毒"),
            vec![Some("application/octet-stream".to_owned())]
        );
    }
}
