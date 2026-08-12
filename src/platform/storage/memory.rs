use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use super::{
    error::StorageError,
    model::{ObjectKey, ObjectMetadata, PresignedRequest, StoragePolicy, StorageSpace},
    service::{
        BackendReadResult, BackendWriteOptions, ObjectStore, ObjectStoreBackend, StorageService,
    },
};

#[derive(Clone)]
struct StoredObject {
    body: Vec<u8>,
    metadata: ObjectMetadata,
}

/// 供领域测试使用的行为型 fake。每个实例代表一个独立的物理空间。
pub struct MemoryAdapter {
    space: StorageSpace,
    objects: Mutex<HashMap<ObjectKey, StoredObject>>,
}

impl MemoryAdapter {
    pub fn object_store(space: StorageSpace, policy: StoragePolicy) -> Arc<dyn ObjectStore> {
        let backend = Arc::new(Self {
            space: space.clone(),
            objects: Mutex::new(HashMap::new()),
        });
        Arc::new(StorageService::new(space, policy, backend))
    }

    fn lock_objects(&self) -> std::sync::MutexGuard<'_, HashMap<ObjectKey, StoredObject>> {
        self.objects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fake_url(&self, operation: &str, key: &ObjectKey, expires_in: Duration) -> String {
        format!(
            "memory://{}/{key}?operation={operation}&expires={}",
            self.space,
            expires_in.as_secs()
        )
    }
}

#[async_trait]
impl ObjectStoreBackend for MemoryAdapter {
    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: &BackendWriteOptions,
    ) -> Result<ObjectMetadata, StorageError> {
        let etag = URL_SAFE_NO_PAD.encode(Sha256::digest(&body));
        let metadata = ObjectMetadata {
            content_length: options.content_length,
            content_type: options.content_type.clone(),
            cache_control: options.cache_control.clone(),
            etag: Some(etag),
            last_modified: Some(SystemTime::now()),
        };
        self.lock_objects().insert(
            key.clone(),
            StoredObject {
                body,
                metadata: metadata.clone(),
            },
        );
        Ok(metadata)
    }

    async fn read(&self, key: &ObjectKey, limit: u64) -> Result<BackendReadResult, StorageError> {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        self.lock_objects()
            .get(key)
            .map(|object| BackendReadResult {
                body: object.body[..object.body.len().min(limit)].to_vec(),
                metadata: object.metadata.clone(),
            })
            .ok_or_else(|| StorageError::not_found(&self.space, key))
    }

    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        self.lock_objects()
            .get(key)
            .map(|object| object.metadata.clone())
            .ok_or_else(|| StorageError::not_found(&self.space, key))
    }

    async fn presign_read(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
    ) -> Result<PresignedRequest, StorageError> {
        Ok(PresignedRequest::new(
            "GET",
            self.fake_url("read", key, expires_in),
            BTreeMap::new(),
            expires_in,
        ))
    }

    async fn presign_write(
        &self,
        key: &ObjectKey,
        expires_in: Duration,
        options: &BackendWriteOptions,
    ) -> Result<PresignedRequest, StorageError> {
        let mut headers = BTreeMap::from([(
            "content-length".to_owned(),
            options.content_length.to_string(),
        )]);
        if let Some(content_type) = &options.content_type {
            headers.insert("content-type".to_owned(), content_type.clone());
        }
        if let Some(cache_control) = &options.cache_control {
            headers.insert("cache-control".to_owned(), cache_control.clone());
        }
        Ok(PresignedRequest::new(
            "PUT",
            self.fake_url("write", key, expires_in),
            headers,
            expires_in,
        ))
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        self.lock_objects().remove(key);
        Ok(())
    }
}
