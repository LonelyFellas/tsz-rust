use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use tsz_rust::{
    lexicon::dto::RichTextV2,
    platform::storage::{
        BackendErrorKind, MemoryAdapter, ObjectKey, ObjectMetadata, ObjectStore, PresignedRequest,
        PutOptions, StorageError, StorageOperation, StoragePolicy, StoragePrivacy, StorageSpace,
    },
    speech::{
        SpeechError, SpeechProvider, SynthesisRequest, SynthesizedAudio, Voice,
        preview::{
            CacheRecord, PreviewRepositoryPort, PreviewService, PreviewServiceError, VoiceRecord,
            dto::{CreatePreviewRequest, PreviewCacheStatus, VoiceListResponse},
        },
    },
};
use uuid::Uuid;

const AUDIO_SENTINEL: &[u8] = b"secret-audio-sentinel";
const URL_SENTINEL: &str = "memory://signed-url-secret";

#[derive(Default)]
struct RepoState {
    active: Option<CacheRecord>,
    stale: Option<CacheRecord>,
    save_result: SaveResult,
    save_calls: usize,
}

#[derive(Default)]
enum SaveResult {
    #[default]
    Store,
    Loser {
        winner: CacheRecord,
    },
    DatabaseError,
}

#[derive(Clone, Default)]
struct FakeRepository {
    state: Arc<Mutex<RepoState>>,
}

impl FakeRepository {
    fn with_stale(stale: CacheRecord) -> Self {
        Self {
            state: Arc::new(Mutex::new(RepoState {
                stale: Some(stale),
                ..RepoState::default()
            })),
        }
    }

    fn with_active(active: CacheRecord) -> Self {
        Self {
            state: Arc::new(Mutex::new(RepoState {
                active: Some(active),
                ..RepoState::default()
            })),
        }
    }

    fn set_save_result(&self, result: SaveResult) {
        self.state.lock().unwrap().save_result = result;
    }

    fn snapshot(&self) -> (Option<CacheRecord>, Option<CacheRecord>, usize) {
        let state = self.state.lock().unwrap();
        (state.active.clone(), state.stale.clone(), state.save_calls)
    }
}

#[async_trait]
impl PreviewRepositoryPort for FakeRepository {
    async fn list_voices(&self) -> Result<VoiceListResponse, sqlx::Error> {
        Ok(VoiceListResponse { items: vec![] })
    }

    async fn voice_by_alias(&self, _alias: &str) -> Result<Option<VoiceRecord>, sqlx::Error> {
        Ok(Some(VoiceRecord {
            id: Uuid::now_v7(),
            alias: "en-us-jenny".to_owned(),
            provider_version: "v1".to_owned(),
            voice: Voice::new(
                "azure",
                "provider-sensitive-id",
                "en-US",
                vec!["chat".to_owned()],
            )
            .unwrap(),
            min_rate_percent: -50,
            max_rate_percent: 100,
            min_pitch_semitones: -12,
            max_pitch_semitones: 12,
            gender: "female".to_owned(),
        }))
    }

    async fn active_cache(&self, _hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error> {
        Ok(self.state.lock().unwrap().active.clone())
    }

    async fn cache_by_hash(&self, _hash: &[u8]) -> Result<Option<CacheRecord>, sqlx::Error> {
        Ok(self.state.lock().unwrap().stale.clone())
    }

    async fn save_cache(
        &self,
        _request_hash: &[u8],
        _content_hash: &[u8],
        _voice_id: Uuid,
        object_key: &str,
        _size_bytes: i64,
    ) -> Result<Option<String>, sqlx::Error> {
        let mut state = self.state.lock().unwrap();
        state.save_calls += 1;
        let result = std::mem::take(&mut state.save_result);
        match result {
            SaveResult::Store => {
                state.active = Some(cache(object_key));
                Ok(Some(object_key.to_owned()))
            }
            SaveResult::Loser { winner } => {
                state.active = Some(winner);
                Ok(None)
            }
            SaveResult::DatabaseError => Err(sqlx::Error::Protocol("db-save-sentinel".to_owned())),
        }
    }
}

struct FakeProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl SpeechProvider for FakeProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }

    async fn synthesize(
        &self,
        _request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SynthesizedAudio {
            bytes: AUDIO_SENTINEL.to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: Some("provider-response-sensitive".to_owned()),
        })
    }
}

struct FaultStore {
    inner: Arc<dyn ObjectStore>,
    put_errors: Mutex<VecDeque<StorageError>>,
    presign_errors: Mutex<VecDeque<StorageError>>,
    delete_errors: Mutex<VecDeque<StorageError>>,
    put_keys: Mutex<Vec<String>>,
    presign_keys: Mutex<Vec<String>>,
    delete_keys: Mutex<Vec<String>>,
}

impl FaultStore {
    fn new(ttl: Duration) -> Self {
        Self {
            inner: MemoryAdapter::object_store(
                space(),
                StoragePolicy::new(StoragePrivacy::Private, 1024, ttl, None).unwrap(),
            ),
            put_errors: Mutex::new(VecDeque::new()),
            presign_errors: Mutex::new(VecDeque::new()),
            delete_errors: Mutex::new(VecDeque::new()),
            put_keys: Mutex::new(vec![]),
            presign_keys: Mutex::new(vec![]),
            delete_keys: Mutex::new(vec![]),
        }
    }

    fn fail_put(&self, error: StorageError) {
        self.put_errors.lock().unwrap().push_back(error);
    }
    fn fail_presign(&self, error: StorageError) {
        self.presign_errors.lock().unwrap().push_back(error);
    }
    fn fail_delete(&self, error: StorageError) {
        self.delete_errors.lock().unwrap().push_back(error);
    }
}

#[async_trait]
impl ObjectStore for FaultStore {
    fn space(&self) -> &StorageSpace {
        self.inner.space()
    }
    fn policy(&self) -> &StoragePolicy {
        self.inner.policy()
    }

    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: PutOptions,
    ) -> Result<ObjectMetadata, StorageError> {
        self.put_keys.lock().unwrap().push(key.to_string());
        if let Some(error) = self.put_errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        self.inner.put(key, body, options).await
    }

    async fn read(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
        self.inner.read(key).await
    }
    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        self.inner.stat(key).await
    }

    async fn presign_read(&self, key: &ObjectKey) -> Result<PresignedRequest, StorageError> {
        self.presign_keys.lock().unwrap().push(key.to_string());
        if let Some(error) = self.presign_errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        self.inner.presign_read(key).await
    }

    async fn presign_write(
        &self,
        key: &ObjectKey,
        content_length: u64,
        options: PutOptions,
    ) -> Result<PresignedRequest, StorageError> {
        self.inner.presign_write(key, content_length, options).await
    }

    async fn copy(
        &self,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        self.inner.copy(source, destination).await
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        self.delete_keys.lock().unwrap().push(key.to_string());
        if let Some(error) = self.delete_errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        self.inner.delete(key).await
    }
}

fn space() -> StorageSpace {
    StorageSpace::parse("speech").unwrap()
}

fn backend_error(operation: StorageOperation, kind: BackendErrorKind) -> StorageError {
    StorageError::Backend {
        space: space(),
        operation,
        kind,
    }
}

fn cache(key: &str) -> CacheRecord {
    CacheRecord {
        object_key: key.to_owned(),
        expires_at: Utc::now() + chrono::Duration::hours(24),
    }
}

fn request(label: &str) -> CreatePreviewRequest {
    CreatePreviewRequest {
        content: RichTextV2 {
            version: 2,
            text: format!("ssml-sensitive-{label}"),
            annotations: vec![],
        },
        voice_alias: "en-us-jenny".to_owned(),
        style: Some("chat".to_owned()),
        rate_percent: 0,
        pitch_semitones: 0,
    }
}

fn redis_pool() -> deadpool_redis::Pool {
    deadpool_redis::Config::from_url("redis://127.0.0.1:6379/0")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap()
}

fn service(
    repo: FakeRepository,
    provider: Arc<FakeProvider>,
    store: Arc<FaultStore>,
) -> PreviewService {
    PreviewService::new(repo, redis_pool(), Some(provider), Some(store))
}

#[tokio::test]
async fn put_backend_failures_do_not_save_presign_delete_or_replace_stale_cache() {
    for kind in [
        BackendErrorKind::AccessDenied,
        BackendErrorKind::RateLimited,
        BackendErrorKind::TemporarilyUnavailable,
        BackendErrorKind::Unexpected,
    ] {
        let stale = cache(&format!("previews/stale-{kind:?}.mp3"));
        let repo = FakeRepository::with_stale(stale.clone());
        let provider = Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        });
        let store = Arc::new(FaultStore::new(Duration::from_secs(73)));
        store.fail_put(backend_error(StorageOperation::Put, kind));
        let error = service(repo.clone(), provider.clone(), store.clone())
            .create_preview(request(&format!("put-{kind:?}")))
            .await
            .unwrap_err();

        assert!(
            matches!(error, PreviewServiceError::Storage(StorageError::Backend { operation: StorageOperation::Put, kind: actual, .. }) if actual == kind)
        );
        let (active, preserved_stale, saves) = repo.snapshot();
        assert!(active.is_none());
        assert_eq!(preserved_stale.unwrap().object_key, stale.object_key);
        assert_eq!(saves, 0);
        assert!(store.presign_keys.lock().unwrap().is_empty());
        assert!(store.delete_keys.lock().unwrap().is_empty());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let debug = format!("{error:?}");
        for secret in [
            "secret-audio-sentinel",
            "ssml-sensitive",
            "provider-response-sensitive",
            URL_SENTINEL,
        ] {
            assert!(!debug.contains(secret));
        }
    }
}

#[tokio::test]
async fn database_failure_deletes_only_new_key_and_delete_failure_preserves_database_error() {
    for delete_fails in [false, true] {
        let stale = cache("previews/original-cache.mp3");
        let repo = FakeRepository::with_stale(stale.clone());
        repo.set_save_result(SaveResult::DatabaseError);
        let provider = Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        });
        let store = Arc::new(FaultStore::new(Duration::from_secs(60)));
        if delete_fails {
            store.fail_delete(backend_error(
                StorageOperation::Delete,
                BackendErrorKind::Unexpected,
            ));
        }
        let error = service(repo.clone(), provider, store.clone())
            .create_preview(request(&format!("db-{delete_fails}")))
            .await
            .unwrap_err();

        assert!(
            matches!(&error, PreviewServiceError::Database(error) if error.to_string().contains("db-save-sentinel"))
        );
        let put_key = store.put_keys.lock().unwrap()[0].clone();
        assert_eq!(*store.delete_keys.lock().unwrap(), vec![put_key]);
        assert_ne!(store.delete_keys.lock().unwrap()[0], stale.object_key);
        let (active, preserved_stale, saves) = repo.snapshot();
        assert!(active.is_none());
        assert_eq!(preserved_stale.unwrap().object_key, stale.object_key);
        assert_eq!(saves, 1);
    }
}

#[tokio::test]
async fn conflict_loser_deletes_only_its_object_and_signs_winner_even_if_delete_fails() {
    for delete_fails in [false, true] {
        let winner = cache("previews/winner.mp3");
        let repo = FakeRepository::default();
        repo.set_save_result(SaveResult::Loser {
            winner: winner.clone(),
        });
        let provider = Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        });
        let store = Arc::new(FaultStore::new(Duration::from_secs(91)));
        if delete_fails {
            store.fail_delete(backend_error(
                StorageOperation::Delete,
                BackendErrorKind::AccessDenied,
            ));
        }
        let response = service(repo, provider, store.clone())
            .create_preview(request(&format!("loser-{delete_fails}")))
            .await
            .unwrap();

        assert!(matches!(response.cache_status, PreviewCacheStatus::Hit));
        assert_eq!(response.url_expires_in_seconds, 91);
        let loser = store.put_keys.lock().unwrap()[0].clone();
        assert_eq!(*store.delete_keys.lock().unwrap(), vec![loser.clone()]);
        assert_ne!(loser, winner.object_key);
        assert_eq!(*store.presign_keys.lock().unwrap(), vec![winner.object_key]);
    }
}

#[tokio::test]
async fn stale_delete_failure_does_not_change_successful_replacement() {
    let stale = cache("previews/stale-replacement.mp3");
    let repo = FakeRepository::with_stale(stale.clone());
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(FaultStore::new(Duration::from_secs(44)));
    store.fail_delete(backend_error(
        StorageOperation::Delete,
        BackendErrorKind::TemporarilyUnavailable,
    ));
    let response = service(repo.clone(), provider, store.clone())
        .create_preview(request("stale-delete"))
        .await
        .unwrap();

    assert!(matches!(
        response.cache_status,
        PreviewCacheStatus::Generated
    ));
    assert_eq!(*store.delete_keys.lock().unwrap(), vec![stale.object_key]);
    let (active, _, _) = repo.snapshot();
    assert_eq!(
        active.unwrap().object_key,
        store.put_keys.lock().unwrap()[0]
    );
}

#[tokio::test]
async fn generated_cache_survives_presign_failure_and_recovers_as_hit_without_regeneration() {
    let repo = FakeRepository::default();
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(FaultStore::new(Duration::from_secs(137)));
    store.fail_presign(backend_error(
        StorageOperation::PresignRead,
        BackendErrorKind::RateLimited,
    ));
    let service = service(repo.clone(), provider.clone(), store.clone());
    let error = service
        .create_preview(request("generated-presign"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        PreviewServiceError::Storage(StorageError::Backend {
            operation: StorageOperation::PresignRead,
            kind: BackendErrorKind::RateLimited,
            ..
        })
    ));
    assert!(repo.snapshot().0.is_some());
    assert!(store.delete_keys.lock().unwrap().is_empty());

    let response = service
        .create_preview(request("generated-presign"))
        .await
        .unwrap();
    assert!(matches!(response.cache_status, PreviewCacheStatus::Hit));
    assert_eq!(response.url_expires_in_seconds, 137);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.put_keys.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn active_cache_presign_failure_recovers_with_store_ttl_without_provider_or_put() {
    let repo = FakeRepository::with_active(cache("previews/active.mp3"));
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(FaultStore::new(Duration::from_secs(211)));
    store.fail_presign(backend_error(
        StorageOperation::PresignRead,
        BackendErrorKind::TemporarilyUnavailable,
    ));
    let service = service(repo, provider.clone(), store.clone());
    assert!(matches!(
        service
            .create_preview(request("active-presign"))
            .await
            .unwrap_err(),
        PreviewServiceError::Storage(StorageError::Backend {
            operation: StorageOperation::PresignRead,
            ..
        })
    ));
    let response = service
        .create_preview(request("active-presign"))
        .await
        .unwrap();
    assert!(matches!(response.cache_status, PreviewCacheStatus::Hit));
    assert_eq!(response.url_expires_in_seconds, 211);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(store.put_keys.lock().unwrap().is_empty());
    assert!(store.delete_keys.lock().unwrap().is_empty());
}

#[test]
fn storage_errors_and_presigned_debug_output_redact_sensitive_payloads() {
    let error = backend_error(StorageOperation::Put, BackendErrorKind::Unexpected);
    let debug = format!("{error:?}");
    for secret in [
        "secret-audio-sentinel",
        "ssml-sensitive",
        "provider-response-sensitive",
        URL_SENTINEL,
    ] {
        assert!(!debug.contains(secret));
    }
}
