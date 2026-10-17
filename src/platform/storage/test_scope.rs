use std::sync::Arc;

use async_trait::async_trait;

use super::{
    BackendErrorKind, ObjectKey, ObjectKeyError, ObjectMetadata, ObjectStore, PresignedRequest,
    PutOptions, StorageError, StorageOperation, StoragePolicy, StorageSpace,
};

pub const TEST_PREFIX: &str = "temporary-tests/";

pub fn valid_test_run(run: &str) -> bool {
    (8..=80).contains(&run.len())
        && (run.as_bytes()[0].is_ascii_lowercase() || run.as_bytes()[0].is_ascii_digit())
        && run
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

pub fn test_key_parts(value: &str) -> Option<(&str, &str, &str)> {
    let (run, rest) = value.strip_prefix(TEST_PREFIX)?.split_once('/')?;
    let (space, inner) = rest.split_once('/')?;
    (valid_test_run(run) && !space.is_empty() && !inner.is_empty()).then_some((run, space, inner))
}

/// A derived object stays in its source's batch even when confirmation is retried without a header.
pub fn related_object_key(source: &ObjectKey, key: ObjectKey) -> Result<ObjectKey, ObjectKeyError> {
    match test_key_parts(source.as_str()) {
        Some((run, space, _)) => {
            ObjectKey::parse(format!("{TEST_PREFIX}{run}/{space}/{}", key.as_str()))
        }
        None => Ok(key),
    }
}

pub(crate) struct TestRoutedStore {
    pub normal: Arc<dyn ObjectStore>,
    pub temporary: Arc<dyn ObjectStore>,
    pub run: Option<String>,
}

impl TestRoutedStore {
    fn resolve(
        &self,
        key: &ObjectKey,
        operation: StorageOperation,
    ) -> Result<(&Arc<dyn ObjectStore>, ObjectKey), StorageError> {
        let denied =
            || StorageError::backend(self.space(), operation, BackendErrorKind::AccessDenied);
        if key.as_str().starts_with(TEST_PREFIX) {
            let (run, space, inner) = test_key_parts(key.as_str()).ok_or_else(denied)?;
            if space != self.space().as_str()
                || self.run.as_ref().is_some_and(|expected| expected != run)
            {
                return Err(denied());
            }
            let physical =
                ObjectKey::parse(format!("{run}/{space}/{inner}")).map_err(|_| denied())?;
            Ok((&self.temporary, physical))
        } else if self.run.is_some() {
            Err(denied())
        } else {
            Ok((&self.normal, key.clone()))
        }
    }
}

#[async_trait]
impl ObjectStore for TestRoutedStore {
    fn space(&self) -> &StorageSpace {
        self.normal.space()
    }
    fn policy(&self) -> &StoragePolicy {
        self.normal.policy()
    }
    fn qualify_key(&self, key: ObjectKey) -> Result<ObjectKey, ObjectKeyError> {
        if key.as_str().starts_with(TEST_PREFIX) {
            return Ok(key);
        }
        match &self.run {
            Some(run) => ObjectKey::parse(format!(
                "{TEST_PREFIX}{run}/{}/{}",
                self.space(),
                key.as_str()
            )),
            None => Ok(key),
        }
    }
    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: PutOptions,
    ) -> Result<ObjectMetadata, StorageError> {
        let (store, key) = self.resolve(key, StorageOperation::Put)?;
        store.put(&key, body, options).await
    }
    async fn read(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
        let (store, key) = self.resolve(key, StorageOperation::Read)?;
        store.read(&key).await
    }
    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        let (store, key) = self.resolve(key, StorageOperation::Stat)?;
        store.stat(&key).await
    }
    async fn presign_read(&self, key: &ObjectKey) -> Result<PresignedRequest, StorageError> {
        let (store, key) = self.resolve(key, StorageOperation::PresignRead)?;
        store.presign_read(&key).await
    }
    async fn presign_write(
        &self,
        key: &ObjectKey,
        size: u64,
        options: PutOptions,
    ) -> Result<PresignedRequest, StorageError> {
        let (store, key) = self.resolve(key, StorageOperation::PresignWrite)?;
        store.presign_write(&key, size, options).await
    }
    async fn copy(
        &self,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        if test_key_parts(source.as_str()).map(|(run, space, _)| (run, space))
            != test_key_parts(destination.as_str()).map(|(run, space, _)| (run, space))
        {
            return Err(StorageError::backend(
                self.space(),
                StorageOperation::Copy,
                BackendErrorKind::AccessDenied,
            ));
        }
        let (from, source) = self.resolve(source, StorageOperation::Copy)?;
        let (to, destination) = self.resolve(destination, StorageOperation::Copy)?;
        if !Arc::ptr_eq(from, to) {
            return Err(StorageError::backend(
                self.space(),
                StorageOperation::Copy,
                BackendErrorKind::AccessDenied,
            ));
        }
        from.copy(&source, &destination).await
    }
    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        let (store, key) = self.resolve(key, StorageOperation::Delete)?;
        store.delete(&key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::storage::{MemoryAdapter, StoragePrivacy};
    use std::time::Duration;

    fn key(value: &str) -> ObjectKey {
        ObjectKey::parse(value).unwrap()
    }
    fn stores() -> (Arc<dyn ObjectStore>, Arc<dyn ObjectStore>) {
        let policy =
            StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None)
                .unwrap();
        (
            MemoryAdapter::object_store("audio".parse().unwrap(), policy.clone()),
            MemoryAdapter::object_store("audio".parse().unwrap(), policy),
        )
    }

    #[tokio::test]
    async fn persisted_test_keys_survive_unscoped_reads_copies_and_cleanup() {
        let (normal, temporary) = stores();
        let test = TestRoutedStore {
            normal: normal.clone(),
            temporary: temporary.clone(),
            run: Some("fixture-20261017".into()),
        };
        let plain = key("uploads/original.mp3");
        normal
            .put(&plain, b"protected".to_vec(), PutOptions::new(None))
            .await
            .unwrap();
        let source = test.qualify_key(plain.clone()).unwrap();
        test.put(&source, b"test".to_vec(), PutOptions::new(None))
            .await
            .unwrap();
        assert_eq!(
            temporary
                .read(&key("fixture-20261017/audio/uploads/original.mp3"))
                .await
                .unwrap(),
            b"test"
        );
        let ordinary = TestRoutedStore {
            normal: normal.clone(),
            temporary,
            run: None,
        };
        assert_eq!(ordinary.read(&source).await.unwrap(), b"test");
        let destination = related_object_key(&source, key("assets/confirmed.mp3")).unwrap();
        ordinary.copy(&source, &destination).await.unwrap();
        assert_eq!(ordinary.read(&destination).await.unwrap(), b"test");
        assert!(
            ordinary
                .presign_read(&destination)
                .await
                .unwrap()
                .url()
                .contains("fixture-20261017/audio/assets/confirmed.mp3")
        );
        ordinary.delete(&source).await.unwrap();
        ordinary.delete(&destination).await.unwrap();
        assert!(ordinary.stat(&source).await.is_err());
        assert_eq!(normal.read(&plain).await.unwrap(), b"protected");
    }

    #[tokio::test]
    async fn test_scope_cannot_read_write_or_copy_normal_or_other_batches() {
        let (normal, temporary) = stores();
        let store = TestRoutedStore {
            normal,
            temporary,
            run: Some("fixture-20261017".into()),
        };
        for value in [
            "uploads/normal.mp3",
            "temporary-tests/another-20261017/audio/a.mp3",
            "temporary-tests/fixture-20261017/speech/a.mp3",
            "temporary-tests/bad/audio/a.mp3",
        ] {
            let key = key(value);
            assert!(store.read(&key).await.is_err(), "{value}");
            assert!(
                store
                    .presign_write(&key, 4, PutOptions::new(None))
                    .await
                    .is_err(),
                "{value}"
            );
            assert!(store.delete(&key).await.is_err(), "{value}");
        }
        let source = key("temporary-tests/fixture-20261017/audio/source.mp3");
        let other = key("temporary-tests/another-20261017/audio/dest.mp3");
        assert!(store.copy(&source, &other).await.is_err());
    }

    #[test]
    fn run_validation_matches_cleanup_script_and_rejects_traversal() {
        for run in [
            "",
            "short",
            "Uppercase-2026",
            "fixture/2026",
            "../fixture",
            "fixture.2026",
        ] {
            assert!(!valid_test_run(run));
        }
        assert!(valid_test_run("fixture_2026-10-17"));
        assert!(valid_test_run(&"a".repeat(80)));
        assert!(!valid_test_run(&"a".repeat(81)));
    }
}
