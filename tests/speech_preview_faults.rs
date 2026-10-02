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
    saved_request_hash: Option<Vec<u8>>,
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

    fn saved_lock_key(&self) -> String {
        let state = self.state.lock().unwrap();
        let hash = state.saved_request_hash.as_ref().expect("缓存已写入");
        let encoded: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("speech:preview:lock:{encoded}")
    }

    /// 清掉命中缓存，迫使下一次请求重新走 Redis 锁。
    fn clear_active(&self) {
        self.state.lock().unwrap().active = None;
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
        request_hash: &[u8],
        _content_hash: &[u8],
        _voice_id: Uuid,
        object_key: &str,
        _size_bytes: i64,
    ) -> Result<Option<String>, sqlx::Error> {
        let mut state = self.state.lock().unwrap();
        state.save_calls += 1;
        state.saved_request_hash = Some(request_hash.to_vec());
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

struct PresignGate {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    proceed: tokio::sync::Notify,
}

struct FaultStore {
    inner: Arc<dyn ObjectStore>,
    put_errors: Mutex<VecDeque<StorageError>>,
    presign_errors: Mutex<VecDeque<StorageError>>,
    delete_errors: Mutex<VecDeque<StorageError>>,
    put_keys: Mutex<Vec<String>>,
    presign_keys: Mutex<Vec<String>>,
    delete_keys: Mutex<Vec<String>>,
    revoke_on_presign: Mutex<Option<(sqlx::PgPool, Uuid)>>,
    presign_gate: Mutex<Option<Arc<PresignGate>>>,
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
            revoke_on_presign: Mutex::new(None),
            presign_gate: Mutex::new(None),
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
        let signed = self.inner.presign_read(key).await?;
        let gate = self.presign_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let started = gate.started.lock().unwrap().take();
            if let Some(started) = started {
                started.send(()).unwrap();
            }
            gate.proceed.notified().await;
        }
        let revoke = self.revoke_on_presign.lock().unwrap().take();
        if let Some((pool, admin_id)) = revoke {
            revoke_generate(&pool, admin_id).await;
        }
        Ok(signed)
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

async fn seed_admin(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    tsz_rust::admin::AdminRepository::new(pool.clone())
        .create(tsz_rust::admin::NewAdmin {
            id,
            phone: format!("speech-fault-{}", id.simple()),
            display_name: "Speech Admin".to_owned(),
            password_hash: "hash".to_owned(),
            role: tsz_rust::admin::AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants (admin_id, permission_key, granted_by) SELECT $1, unnest(ARRAY['words.access', 'speech.generate']), $1")
        .bind(id).execute(pool).await.unwrap();
    id
}

async fn revoke_generate(pool: &sqlx::PgPool, admin_id: Uuid) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM admins WHERE id=$1 FOR UPDATE")
        .bind(admin_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='speech.generate'")
        .bind(admin_id).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE admins SET permission_version=permission_version+1 WHERE id=$1")
        .bind(admin_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

fn redis_pool() -> deadpool_redis::Pool {
    deadpool_redis::Config::from_url(
        std::env::var("TEST_REDIS_URL").expect("isolated test Redis required"),
    )
    .create_pool(Some(deadpool_redis::Runtime::Tokio1))
    .unwrap()
}

fn service(
    pool: &sqlx::PgPool,
    repo: FakeRepository,
    provider: Arc<FakeProvider>,
    store: Arc<FaultStore>,
) -> PreviewService {
    PreviewService::new(
        pool.clone(),
        repo,
        redis_pool(),
        Some(provider),
        Some(store),
    )
}

#[sqlx::test]
async fn put_backend_failures_do_not_save_presign_delete_or_replace_stale_cache(
    pool: sqlx::PgPool,
) {
    let admin_id = seed_admin(&pool).await;
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
        let error = service(&pool, repo.clone(), provider.clone(), store.clone())
            .create_preview(admin_id, request(&format!("put-{kind:?}")))
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

#[sqlx::test]
async fn database_failure_deletes_only_new_key_and_delete_failure_preserves_database_error(
    pool: sqlx::PgPool,
) {
    let admin_id = seed_admin(&pool).await;
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
        let error = service(&pool, repo.clone(), provider, store.clone())
            .create_preview(admin_id, request(&format!("db-{delete_fails}")))
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

#[sqlx::test]
async fn conflict_loser_deletes_only_its_object_and_signs_winner_even_if_delete_fails(
    pool: sqlx::PgPool,
) {
    let admin_id = seed_admin(&pool).await;
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
        let response = service(&pool, repo, provider, store.clone())
            .create_preview(admin_id, request(&format!("loser-{delete_fails}")))
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

#[sqlx::test]
async fn stale_delete_failure_does_not_change_successful_replacement(pool: sqlx::PgPool) {
    let admin_id = seed_admin(&pool).await;
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
    let response = service(&pool, repo.clone(), provider, store.clone())
        .create_preview(admin_id, request("stale-delete"))
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

#[sqlx::test]
async fn generated_cache_survives_presign_failure_and_recovers_as_hit_without_regeneration(
    pool: sqlx::PgPool,
) {
    let admin_id = seed_admin(&pool).await;
    let repo = FakeRepository::default();
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(FaultStore::new(Duration::from_secs(137)));
    store.fail_presign(backend_error(
        StorageOperation::PresignRead,
        BackendErrorKind::RateLimited,
    ));
    let service = service(&pool, repo.clone(), provider.clone(), store.clone());
    let error = service
        .create_preview(admin_id, request("generated-presign"))
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
        .create_preview(admin_id, request("generated-presign"))
        .await
        .unwrap();
    assert!(matches!(response.cache_status, PreviewCacheStatus::Hit));
    assert_eq!(response.url_expires_in_seconds, 137);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.put_keys.lock().unwrap().len(), 1);
}

#[sqlx::test]
async fn active_cache_presign_failure_recovers_with_store_ttl_without_provider_or_put(
    pool: sqlx::PgPool,
) {
    let admin_id = seed_admin(&pool).await;
    let repo = FakeRepository::with_active(cache("previews/active.mp3"));
    let provider = Arc::new(FakeProvider {
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(FaultStore::new(Duration::from_secs(211)));
    store.fail_presign(backend_error(
        StorageOperation::PresignRead,
        BackendErrorKind::TemporarilyUnavailable,
    ));
    let service = service(&pool, repo, provider.clone(), store.clone());
    assert!(matches!(
        service
            .create_preview(admin_id, request("active-presign"))
            .await
            .unwrap_err(),
        PreviewServiceError::Storage(StorageError::Backend {
            operation: StorageOperation::PresignRead,
            ..
        })
    ));
    let response = service
        .create_preview(admin_id, request("active-presign"))
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

/// 进入合成时发一次信号，随后慢到足以让调用方在合成途中取消。
/// 用信号而不是猜一个超时：否则「在拿锁前就被取消」和「detach 失效」会表现成同一种失败。
struct SlowProvider {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    delay: Duration,
    calls: AtomicUsize,
}

#[async_trait]
impl SpeechProvider for SlowProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }

    async fn synthesize(
        &self,
        _request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        tokio::time::sleep(self.delay).await;
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SynthesizedAudio {
            bytes: AUDIO_SENTINEL.to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: Some("provider-response-sensitive".to_owned()),
        })
    }
}

async fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..300 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("等待超时：{label}");
}

#[sqlx::test]
async fn cancelled_request_still_completes_generation_and_releases_lock(pool: sqlx::PgPool) {
    let admin_id = seed_admin(&pool).await;
    // fingerprint 决定 Redis 锁键。用唯一文本，避免上一轮遗留的锁把本轮
    // 挤进 InProgress 分支——本测试恰好会制造这种遗留，不能依赖 Redis 干净。
    let label = format!("cancel-{}", Uuid::now_v7());
    let repo = FakeRepository::default();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let provider = Arc::new(SlowProvider {
        started: Mutex::new(Some(started_tx)),
        delay: Duration::from_millis(400),
        calls: AtomicUsize::new(0),
    });
    let (presigning_tx, presigning_rx) = tokio::sync::oneshot::channel();
    let gate = Arc::new(PresignGate {
        started: Mutex::new(Some(presigning_tx)),
        proceed: tokio::sync::Notify::new(),
    });
    let store = Arc::new(FaultStore::new(Duration::from_secs(73)));
    *store.presign_gate.lock().unwrap() = Some(gate.clone());
    let redis = redis_pool();
    let service = PreviewService::new(
        pool.clone(),
        repo.clone(),
        redis.clone(),
        Some(provider.clone()),
        Some(store.clone()),
    );

    // 客户端在合成途中 abort：axum 会丢弃 handler future。
    // 等 provider 报告「已进入合成」再丢弃，取消时机因此是确定的，不依赖机器快慢。
    let mut pending = Box::pin(service.create_preview(admin_id, request(&label)));
    tokio::select! {
        _ = &mut pending => panic!("前置条件：不应在合成完成前返回"),
        started = started_rx => started.expect("provider 应报告已进入合成"),
    }
    drop(pending);

    // detach 出去的任务不受调用方消失影响，应当照常跑完。
    wait_until("被取消的生成任务没有写入缓存", || {
        repo.snapshot().2 == 1
    })
    .await;

    tokio::time::timeout(Duration::from_secs(3), presigning_rx)
        .await
        .expect("后台任务必须进入签名阶段")
        .expect("缓存已写入，但签名阶段仍在执行");
    let key = repo.saved_lock_key();
    let mut connection = redis.get().await.unwrap();
    let held: bool = deadpool_redis::redis::cmd("EXISTS")
        .arg(&key)
        .query_async(&mut connection)
        .await
        .unwrap();
    assert!(held, "缓存写入不代表后台任务已释放锁");

    let (active, _, saves) = repo.snapshot();
    assert_eq!(saves, 1, "已经付过费的合成结果必须落缓存");
    assert!(active.is_some(), "缓存行应当可被后续请求命中");
    assert_eq!(store.put_keys.lock().unwrap().len(), 1, "对象只应写入一次");
    assert!(
        store.delete_keys.lock().unwrap().is_empty(),
        "不应留下需要补偿删除的对象"
    );

    // 缓存写入后仍有签名及权限复核；只等待 save_cache 会与后台收尾竞态。
    // 放行签名，等待这个 fingerprint 的真实 Redis 锁消失，再清缓存验证重新生成。
    gate.proceed.notify_one();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let held: bool = deadpool_redis::redis::cmd("EXISTS")
                .arg(&key)
                .query_async(&mut connection)
                .await
                .unwrap();
            if !held {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("后台任务完成后必须释放锁，不能等待租约到期");
    drop(connection);
    repo.clear_active();
    let response = service
        .create_preview(admin_id, request(&label))
        .await
        .expect("锁已释放，同一 fingerprint 应能重新生成");
    assert!(matches!(
        response.cache_status,
        PreviewCacheStatus::Generated
    ));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "两次生成都应真正调用 provider"
    );
}

struct GatedProvider {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    proceed: tokio::sync::Notify,
    calls: AtomicUsize,
}

#[async_trait]
impl SpeechProvider for GatedProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }

    async fn synthesize(&self, _: &SynthesisRequest) -> Result<SynthesizedAudio, SpeechError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let started = self.started.lock().unwrap().take();
        if let Some(started) = started {
            started.send(()).unwrap();
            self.proceed.notified().await;
        }
        Ok(SynthesizedAudio {
            bytes: AUDIO_SENTINEL.to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

#[sqlx::test]
async fn revoked_during_synthesis_denies_next_effect_and_releases_lock(pool: sqlx::PgPool) {
    let admin_id = seed_admin(&pool).await;
    let repo = FakeRepository::default();
    let store = Arc::new(FaultStore::new(Duration::from_secs(60)));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let provider = Arc::new(GatedProvider {
        started: Mutex::new(Some(started_tx)),
        proceed: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    });
    let service = PreviewService::new(
        pool.clone(),
        repo.clone(),
        redis_pool(),
        Some(provider.clone()),
        Some(store.clone()),
    );
    let label = format!("revoke-synthesis-{}", Uuid::now_v7());
    let worker = service.clone();
    let input = request(&label);
    let task = tokio::spawn(async move { worker.create_preview(admin_id, input).await });
    started_rx.await.unwrap();
    revoke_generate(&pool, admin_id).await;
    provider.proceed.notify_one();
    assert!(matches!(
        task.await.unwrap(),
        Err(PreviewServiceError::Authorization(_))
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert!(
        store.put_keys.lock().unwrap().is_empty(),
        "撤权后不可上传合成结果"
    );
    assert!(store.presign_keys.lock().unwrap().is_empty());
    assert_eq!(repo.snapshot().2, 0);
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.generate',$1)")
        .bind(admin_id).execute(&pool).await.unwrap();
    assert!(matches!(
        service
            .create_preview(admin_id, request(&label))
            .await
            .unwrap()
            .cache_status,
        PreviewCacheStatus::Generated
    ));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "撤权失败路径也必须释放生成锁"
    );
}

#[sqlx::test]
async fn revoked_before_delivery_blocks_generated_and_cached_urls_without_undoing_cache(
    pool: sqlx::PgPool,
) {
    for cached in [false, true] {
        let admin_id = seed_admin(&pool).await;
        let repo = if cached {
            FakeRepository::with_active(cache("previews/existing-revoked.mp3"))
        } else {
            FakeRepository::default()
        };
        let provider = Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        });
        let store = Arc::new(FaultStore::new(Duration::from_secs(60)));
        *store.revoke_on_presign.lock().unwrap() = Some((pool.clone(), admin_id));
        let service = service(&pool, repo.clone(), provider.clone(), store.clone());
        let label = format!("revoke-delivery-{}", Uuid::now_v7());
        assert!(matches!(
            service.create_preview(admin_id, request(&label)).await,
            Err(PreviewServiceError::Authorization(_))
        ));
        assert!(
            repo.snapshot().0.is_some(),
            "撤权不能回滚已经完成的缓存副作用"
        );
        assert!(store.delete_keys.lock().unwrap().is_empty());
        assert_eq!(store.presign_keys.lock().unwrap().len(), 1);
        assert!(matches!(
            service.create_preview(admin_id, request(&label)).await,
            Err(PreviewServiceError::Authorization(_))
        ));
        assert_eq!(
            store.presign_keys.lock().unwrap().len(),
            1,
            "无权重放不得再次签名"
        );
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'speech.generate',$1)")
            .bind(admin_id).execute(&pool).await.unwrap();
        assert!(matches!(
            service
                .create_preview(admin_id, request(&label))
                .await
                .unwrap()
                .cache_status,
            PreviewCacheStatus::Hit
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), usize::from(!cached));
    }
}
