mod avatar_support;
use avatar_support::{setup, upload};
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tsz_rust::platform::storage::{
    BackendErrorKind, ObjectMetadata, ObjectStore, PresignedRequest, PutOptions, StorageError,
    StorageOperation, StoragePolicy, StorageRegistry, StorageSpace,
};
use tsz_rust::{
    avatar::{cleanup, repository, service},
    platform::storage::ObjectKey,
    user::repository::UserRepository,
};
use uuid::Uuid;

#[sqlx::test]
async fn candidate_late_write_is_reclaimed_after_initial_successful_delete(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    let task = Uuid::now_v7();
    let key = format!("images/avatars/{}.webp", Uuid::now_v7());
    repository::register_candidate(&pool, &auth, record.id, &key, task)
        .await
        .unwrap();
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE id=$1")
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    store
        .put(
            &ObjectKey::parse(&key).unwrap(),
            vec![1],
            Default::default(),
        )
        .await
        .unwrap();
    assert!(store.read(&ObjectKey::parse(&key).unwrap()).await.is_ok());
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE id=$1")
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    assert!(store.read(&ObjectKey::parse(&key).unwrap()).await.is_err());
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM avatar_cleanup_tasks WHERE id=$1")
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempts, 2);
}

#[sqlx::test]
async fn bare_delete_enqueues_and_reclaims_formal_avatar(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    service::confirm(&state, &auth, &source).await.unwrap();
    let (id, key): (Uuid, String) =
        sqlx::query_as("SELECT id,canonical_key FROM avatar_uploads WHERE source_key=$1")
            .bind(&source)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM avatar_cleanup_tasks WHERE upload_id=$1 AND kind='canonical'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert!(service::read_public(&state, id).await.is_err());
    cleanup::sweep(&state).await.unwrap();
    assert!(store.read(&ObjectKey::parse(&key).unwrap()).await.is_err());
}

#[sqlx::test]
async fn late_original_after_initial_delete_is_reclaimed(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let key = ObjectKey::parse(&source).unwrap();
    let bytes = store.read(&key).await.unwrap();
    sqlx::query(
        "UPDATE avatar_uploads SET expires_at=now()-interval '121 seconds' WHERE source_key=$1",
    )
    .bind(&source)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE object_key=$1")
        .bind(&source)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    store.put(&key, bytes, PutOptions::default()).await.unwrap();
    assert!(service::confirm(&state, &auth, &source).await.is_err());
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE object_key=$1")
        .bind(&source)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    assert!(store.read(&key).await.is_err());
}

#[sqlx::test]
async fn known_written_failed_candidate_is_not_redeleted_forever(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    sqlx::raw_sql("CREATE FUNCTION reject_avatar_change() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected write failure'; END $$; CREATE TRIGGER reject_avatar_change BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION reject_avatar_change();").execute(&pool).await.unwrap();
    assert!(service::confirm(&state, &auth, &source).await.is_err());
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE kind='candidate'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE kind='candidate'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 0);
}

#[sqlx::test]
async fn cleanup_claim_only_takes_immediately_executable_slots(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    for _ in 0..25 {
        sqlx::query("INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before) VALUES ($1,$2,$3,'candidate',now())")
            .bind(Uuid::now_v7()).bind(record.id).bind(format!("images/avatars/{}.webp",Uuid::now_v7())).execute(&pool).await.unwrap();
    }
    assert_eq!(cleanup::claim(&pool).await.unwrap().len(), 4);
}

#[sqlx::test]
async fn completion_after_unknown_delete_requeues_final_delete(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    let task = Uuid::now_v7();
    let key = ObjectKey::parse(format!("images/avatars/{}.webp", Uuid::now_v7())).unwrap();
    repository::register_candidate(&pool, &auth, record.id, key.as_str(), task)
        .await
        .unwrap();
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE id=$1")
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    store
        .put(&key, vec![1], PutOptions::default())
        .await
        .unwrap();
    repository::mark_candidate_complete(&pool, task)
        .await
        .unwrap();
    let row: (String, i32, bool) = sqlx::query_as(
        "SELECT status,attempts,write_completed FROM avatar_cleanup_tasks WHERE id=$1",
    )
    .bind(task)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("pending".into(), 1, true));
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    assert!(store.read(&key).await.is_err());
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE id=$1")
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 0);
    assert!(
        sqlx::query("UPDATE avatar_cleanup_tasks SET write_completed=false WHERE id=$1")
            .bind(task)
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test]
async fn late_write_during_delete_response_requires_another_delete(pool: PgPool) {
    let (mut state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    let task = Uuid::now_v7();
    let key = ObjectKey::parse(format!("images/avatars/{}.webp", Uuid::now_v7())).unwrap();
    repository::register_candidate(&pool, &auth, record.id, key.as_str(), task)
        .await
        .unwrap();
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE id=$1")
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let controlled = Arc::new(FaultStore {
        inner: store.clone(),
        fail_put: AtomicBool::new(false),
        fail_delete: AtomicBool::new(false),
        presign_delay: std::time::Duration::ZERO,
        signed_at: std::sync::Mutex::new(None),
        pause_delete: std::sync::Mutex::new(Some((started.clone(), release.clone()))),
    });
    state.object_storage =
        StorageRegistry::from_stores([controlled as Arc<dyn ObjectStore>]).unwrap();
    let sweep_state = state.clone();
    let sweep = tokio::spawn(async move { cleanup::sweep(&sweep_state).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    store
        .put(&key, vec![1], PutOptions::default())
        .await
        .unwrap();
    repository::mark_candidate_complete(&pool, task)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(sweep.await.unwrap().unwrap(), 1);
    assert!(store.read(&key).await.is_ok());
    let status: String = sqlx::query_scalar("SELECT status FROM avatar_cleanup_tasks WHERE id=$1")
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    assert!(store.read(&key).await.is_err());
}

#[sqlx::test]
async fn cleanup_reserves_capacity_for_new_work_and_unknown_rechecks(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    for status in ["pending", "done"] {
        for _ in 0..8 {
            sqlx::query("INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,status,not_before) VALUES ($1,$2,$3,'candidate',$4,now()-interval '1 day')")
                .bind(Uuid::now_v7()).bind(record.id).bind(format!("images/avatars/{status}/{}.webp",Uuid::now_v7())).bind(status).execute(&pool).await.unwrap();
        }
    }
    let claims = cleanup::claim(&pool).await.unwrap();
    assert_eq!(claims.len(), 4);
    assert_eq!(
        claims
            .iter()
            .filter(|task| task.object_key.contains("/pending/"))
            .count(),
        3
    );
    assert_eq!(
        claims
            .iter()
            .filter(|task| task.object_key.contains("/done/"))
            .count(),
        1
    );
}

#[sqlx::test]
async fn completion_marker_failure_keeps_unknown_recovery_task(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    sqlx::raw_sql("CREATE FUNCTION reject_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected completion failure'; END $$; CREATE TRIGGER reject_completion BEFORE UPDATE ON avatar_cleanup_tasks FOR EACH ROW WHEN (NEW.write_completed AND NOT OLD.write_completed) EXECUTE FUNCTION reject_completion();").execute(&pool).await.unwrap();
    assert!(service::confirm(&state, &auth, &source).await.is_err());
    let completed: bool = sqlx::query_scalar(
        "SELECT write_completed FROM avatar_cleanup_tasks WHERE kind='candidate'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!completed);
    assert_eq!(
        service::current_profile(&state, &auth)
            .await
            .unwrap()
            .avatar_url,
        ""
    );
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE kind='candidate'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    sqlx::raw_sql("DROP TRIGGER reject_completion ON avatar_cleanup_tasks; DROP FUNCTION reject_completion();").execute(&pool).await.unwrap();
    assert!(
        !service::confirm(&state, &auth, &source)
            .await
            .unwrap()
            .avatar_url
            .is_empty()
    );
}

#[sqlx::test]
async fn finalization_delay_cannot_return_an_expired_upload_url(pool: PgPool) {
    let (mut state, auth, _) = setup(&pool).await;
    let short = tsz_rust::platform::storage::MemoryAdapter::object_store(
        "avatars".parse().unwrap(),
        StoragePolicy::new(
            tsz_rust::platform::storage::StoragePrivacy::Private,
            5242880,
            std::time::Duration::from_secs(2),
            None,
        )
        .unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([short]).unwrap();
    sqlx::raw_sql("CREATE FUNCTION delay_source_finalization() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(3); RETURN NEW; END $$; CREATE TRIGGER delay_source_finalization BEFORE UPDATE ON avatar_cleanup_tasks FOR EACH ROW WHEN (NEW.kind='source') EXECUTE FUNCTION delay_source_finalization();").execute(&pool).await.unwrap();
    assert!(
        service::create_upload(
            &state,
            &auth,
            tsz_rust::avatar::dto::AvatarUploadRequest {
                content_type: "image/png".into(),
                size: 1
            }
        )
        .await
        .is_err()
    );
}

struct FaultStore {
    inner: Arc<dyn ObjectStore>,
    fail_put: AtomicBool,
    fail_delete: AtomicBool,
    presign_delay: std::time::Duration,
    signed_at: std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>,
    pause_delete: std::sync::Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}

#[sqlx::test]
async fn delayed_signatures_keep_permit_deadline_in_sync(pool: PgPool) {
    let (mut state, auth, store) = setup(&pool).await;
    let delayed = Arc::new(FaultStore {
        inner: store,
        fail_put: AtomicBool::new(false),
        fail_delete: AtomicBool::new(false),
        presign_delay: std::time::Duration::from_secs(1),
        signed_at: std::sync::Mutex::new(None),
        pause_delete: std::sync::Mutex::new(None),
    });
    state.object_storage =
        StorageRegistry::from_stores([delayed.clone() as Arc<dyn ObjectStore>]).unwrap();
    let permit = service::create_upload(
        &state,
        &auth,
        tsz_rust::avatar::dto::AvatarUploadRequest {
            content_type: "image/png".into(),
            size: 1,
        },
    )
    .await
    .unwrap()
    .upload;
    let expires_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT expires_at FROM avatar_uploads WHERE source_key=$1")
            .bind(&permit.key)
            .fetch_one(&pool)
            .await
            .unwrap();
    let signed_at = delayed.signed_at.lock().unwrap().unwrap();
    assert!(expires_at >= signed_at + chrono::Duration::seconds(600));
}

#[sqlx::test]
async fn failed_old_confirmation_retry_cannot_overwrite_later_avatar(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let a = upload(&state, &auth, store.as_ref()).await;
    let b = upload(&state, &auth, store.as_ref()).await;
    sqlx::raw_sql("CREATE FUNCTION reject_avatar_change() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected write failure'; END $$; CREATE TRIGGER reject_avatar_change BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION reject_avatar_change();").execute(&pool).await.unwrap();
    let failed = service::confirm(&state, &auth, &a).await.err().unwrap();
    use axum::response::IntoResponse;
    assert_eq!(
        failed.into_response().status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
    sqlx::raw_sql(
        "DROP TRIGGER reject_avatar_change ON users; DROP FUNCTION reject_avatar_change();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let current = service::confirm(&state, &auth, &b).await.unwrap();
    assert!(service::confirm(&state, &auth, &a).await.is_err());
    assert_eq!(
        service::current_profile(&state, &auth)
            .await
            .unwrap()
            .avatar_url,
        current.avatar_url
    );
    let next = upload(&state, &auth, store.as_ref()).await;
    assert_ne!(
        service::confirm(&state, &auth, &next)
            .await
            .unwrap()
            .avatar_url,
        current.avatar_url
    );
}

#[sqlx::test]
async fn earlier_unattempted_permit_can_confirm_after_later_permit(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let a = upload(&state, &auth, store.as_ref()).await;
    let b = upload(&state, &auth, store.as_ref()).await;
    let first = service::confirm(&state, &auth, &b).await.unwrap();
    let later = service::confirm(&state, &auth, &a).await.unwrap();
    assert_ne!(first.avatar_url, later.avatar_url);
}

impl FaultStore {
    fn error(&self, operation: StorageOperation) -> StorageError {
        StorageError::Backend {
            space: self.inner.space().clone(),
            operation,
            kind: BackendErrorKind::TemporarilyUnavailable,
        }
    }
}

#[async_trait::async_trait]
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
        bytes: Vec<u8>,
        options: PutOptions,
    ) -> Result<ObjectMetadata, StorageError> {
        if self.fail_put.load(Ordering::SeqCst) {
            return Err(self.error(StorageOperation::Put));
        }
        self.inner.put(key, bytes, options).await
    }
    async fn read(&self, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
        self.inner.read(key).await
    }
    async fn stat(&self, key: &ObjectKey) -> Result<ObjectMetadata, StorageError> {
        self.inner.stat(key).await
    }
    async fn presign_read(&self, key: &ObjectKey) -> Result<PresignedRequest, StorageError> {
        self.inner.presign_read(key).await
    }
    async fn presign_write(
        &self,
        key: &ObjectKey,
        length: u64,
        options: PutOptions,
    ) -> Result<PresignedRequest, StorageError> {
        tokio::time::sleep(self.presign_delay).await;
        *self.signed_at.lock().unwrap() = Some(chrono::Utc::now());
        self.inner.presign_write(key, length, options).await
    }
    async fn copy(
        &self,
        source: &ObjectKey,
        dest: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        self.inner.copy(source, dest).await
    }
    async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(self.error(StorageOperation::Delete));
        }
        let result = self.inner.delete(key).await;
        let pause = self.pause_delete.lock().unwrap().take();
        if let Some((started, release)) = pause {
            started.notify_one();
            release.notified().await;
        }
        result
    }
}

#[sqlx::test]
async fn storage_failure_keeps_durable_cleanup_and_retries_without_reset(pool: PgPool) {
    let (mut state, auth, store) = setup(&pool).await;
    let key = upload(&state, &auth, store.as_ref()).await;
    let faults = Arc::new(FaultStore {
        inner: store.clone(),
        fail_put: AtomicBool::new(true),
        fail_delete: AtomicBool::new(false),
        presign_delay: std::time::Duration::ZERO,
        signed_at: std::sync::Mutex::new(None),
        pause_delete: std::sync::Mutex::new(None),
    });
    state.object_storage =
        StorageRegistry::from_stores([faults.clone() as Arc<dyn ObjectStore>]).unwrap();
    let error = service::confirm(&state, &auth, &key).await.err().unwrap();
    use axum::response::IntoResponse;
    assert_eq!(
        error.into_response().status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        service::current_profile(&state, &auth)
            .await
            .unwrap()
            .avatar_url,
        ""
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM avatar_cleanup_tasks WHERE kind='candidate' AND status='pending'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    faults.fail_put.store(false, Ordering::SeqCst);
    let current = service::confirm(&state, &auth, &key).await.unwrap();
    faults.fail_delete.store(true, Ordering::SeqCst);
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE status='pending'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 0);
    let attempts: Vec<i32> =
        sqlx::query_scalar("SELECT attempts FROM avatar_cleanup_tasks WHERE status='pending'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(attempts, vec![1, 1]);
    faults.fail_delete.store(false, Ordering::SeqCst);
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE status='pending'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 2);
    let attempts: Vec<i32> =
        sqlx::query_scalar("SELECT attempts FROM avatar_cleanup_tasks WHERE status='done'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(attempts, vec![2, 2]);
    assert_eq!(
        service::confirm(&state, &auth, &key)
            .await
            .unwrap()
            .avatar_url,
        current.avatar_url
    );
}

#[sqlx::test]
async fn confirmation_lock_excludes_cleanup_and_current_reference_is_never_deleted(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    let task = Uuid::now_v7();
    let key = format!("images/avatars/{}.webp", Uuid::now_v7());
    repository::register_candidate(&pool, &auth, record.id, &key, task)
        .await
        .unwrap();
    store
        .put(
            &ObjectKey::parse(&key).unwrap(),
            vec![1],
            Default::default(),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE id=$1")
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    repository::commit_avatar_in(
        &mut tx,
        &auth,
        record.id,
        task,
        &key,
        "https://api.example/avatars/current",
    )
    .await
    .unwrap();
    assert!(cleanup::claim(&pool).await.unwrap().is_empty());
    tx.commit().await.unwrap();
    sqlx::query("INSERT INTO avatar_cleanup_tasks (id,upload_id,object_key,kind,not_before) VALUES ($1,$2,$3,'canonical',now())")
        .bind(Uuid::now_v7()).bind(record.id).bind(&key).execute(&pool).await.unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 0);
    assert!(store.read(&ObjectKey::parse(&key).unwrap()).await.is_ok());
    let code: String = sqlx::query_scalar(
        "SELECT last_error FROM avatar_cleanup_tasks WHERE object_key=$1 AND kind='canonical'",
    )
    .bind(&key)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(code, "currently_referenced");
}

#[sqlx::test]
async fn retries_concurrency_replacement_and_hard_delete(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let key = upload(&state, &auth, store.as_ref()).await;
    let (a, b) = tokio::join!(
        service::confirm(&state, &auth, &key),
        service::confirm(&state, &auth, &key)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.avatar_url, b.avatar_url);
    let current: String =
        sqlx::query_scalar("SELECT canonical_key FROM avatar_uploads WHERE source_key=$1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM avatar_uploads WHERE source_key=$1")
        .bind(&key)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        image::load_from_memory(&service::read_public(&state, id).await.unwrap())
            .unwrap()
            .width(),
        512
    );
    let next = upload(&state, &auth, store.as_ref()).await;
    let new = service::confirm(&state, &auth, &next).await.unwrap();
    assert_ne!(a.avatar_url, new.avatar_url);
    assert_eq!(
        service::confirm(&state, &auth, &key)
            .await
            .unwrap()
            .avatar_url,
        new.avatar_url
    );
    assert!(service::read_public(&state, id).await.is_err());
    let new_id: Uuid = sqlx::query_scalar("SELECT avatar_upload_id FROM users WHERE id=$1")
        .bind(auth.subject)
        .fetch_one(&pool)
        .await
        .unwrap();
    let active_key: String =
        sqlx::query_scalar("SELECT canonical_key FROM avatar_uploads WHERE id=$1")
            .bind(new_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE kind!='source'")
        .execute(&pool)
        .await
        .unwrap();
    cleanup::sweep(&state).await.unwrap();
    assert!(
        store
            .read(&ObjectKey::parse(&current).unwrap())
            .await
            .is_err()
    );
    assert!(
        store
            .read(&ObjectKey::parse(&active_key).unwrap())
            .await
            .is_ok()
    );
    let mut tx = pool.begin().await.unwrap();
    repository::schedule_user_cleanup_in(&mut tx, auth.subject)
        .await
        .unwrap();
    UserRepository::delete_account_in(&mut tx, auth.subject)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(service::read_public(&state, new_id).await.is_err());
    cleanup::sweep(&state).await.unwrap();
    assert!(
        store
            .read(&ObjectKey::parse(&active_key).unwrap())
            .await
            .is_err()
    );
    assert!(store.read(&ObjectKey::parse(&next).unwrap()).await.is_ok());
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE kind='source'")
        .execute(&pool)
        .await
        .unwrap();
    cleanup::sweep(&state).await.unwrap();
    assert!(store.read(&ObjectKey::parse(&next).unwrap()).await.is_err());
}

#[sqlx::test]
async fn cleanup_claim_history_prevents_candidate_confirmation_even_after_lease_expiry(
    pool: PgPool,
) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    let task = Uuid::now_v7();
    let key = format!("images/avatars/{}.webp", Uuid::now_v7());
    repository::register_candidate(&pool, &auth, record.id, &key, task)
        .await
        .unwrap();
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE id=$1")
        .bind(task)
        .execute(&pool)
        .await
        .unwrap();
    let claims = cleanup::claim(&pool).await.unwrap();
    assert_eq!(claims.len(), 1);
    sqlx::query(
        "UPDATE avatar_cleanup_tasks SET lease_until=now()-interval '1 second' WHERE id=$1",
    )
    .bind(task)
    .execute(&pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        repository::commit_avatar_in(
            &mut tx,
            &auth,
            record.id,
            task,
            &key,
            "https://api.example/avatars/test"
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(
        sqlx::query("UPDATE avatar_cleanup_tasks SET attempts=0 WHERE id=$1")
            .bind(task)
            .execute(&pool)
            .await
            .is_err()
    );
    let reclaimed = cleanup::claim(&pool).await.unwrap();
    assert_eq!(reclaimed.len(), 1);
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM avatar_cleanup_tasks WHERE id=$1")
        .bind(task)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempts, 2);
    let user = service::confirm(&state, &auth, &source).await.unwrap();
    assert!(user.avatar_url.ends_with(&record.id.to_string()));
}

#[sqlx::test]
async fn candidate_failure_rolls_back_reference_and_keeps_cleanup(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let old = upload(&state, &auth, store.as_ref()).await;
    let profile = service::confirm(&state, &auth, &old).await.unwrap();
    let next = upload(&state, &auth, store.as_ref()).await;
    sqlx::raw_sql("CREATE FUNCTION reject_avatar_change() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected write failure'; END $$; CREATE TRIGGER reject_avatar_change BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION reject_avatar_change();").execute(&pool).await.unwrap();
    assert!(service::confirm(&state, &auth, &next).await.is_err());
    assert_eq!(
        service::current_profile(&state, &auth)
            .await
            .unwrap()
            .avatar_url,
        profile.avatar_url
    );
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM avatar_cleanup_tasks WHERE kind='candidate' AND status='pending'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 1);
    sqlx::raw_sql(
        "DROP TRIGGER reject_avatar_change ON users; DROP FUNCTION reject_avatar_change();",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_ne!(
        service::confirm(&state, &auth, &next)
            .await
            .unwrap()
            .avatar_url,
        profile.avatar_url
    );
}

#[sqlx::test]
async fn original_remains_until_signature_and_grace_end(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    service::confirm(&state, &auth, &source).await.unwrap();
    let safe:bool=sqlx::query_scalar("SELECT t.not_before>=a.expires_at+interval '120 seconds' FROM avatar_cleanup_tasks t JOIN avatar_uploads a ON a.id=t.upload_id WHERE t.object_key=$1 AND kind='source'").bind(&source).fetch_one(&pool).await.unwrap();
    assert!(safe);
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 0);
    let bytes = store
        .read(&ObjectKey::parse(&source).unwrap())
        .await
        .unwrap();
    store
        .put(
            &ObjectKey::parse(&source).unwrap(),
            bytes,
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 0);
    sqlx::query("UPDATE avatar_cleanup_tasks SET not_before=now() WHERE kind='source'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&state).await.unwrap(), 1);
    assert!(
        store
            .read(&ObjectKey::parse(&source).unwrap())
            .await
            .is_err()
    );
}

#[sqlx::test]
async fn confirmation_rechecks_security_version_and_ttl(pool: PgPool) {
    let (state, auth, store) = setup(&pool).await;
    let source = upload(&state, &auth, store.as_ref()).await;
    let record = repository::find(&pool, &auth, &source).await.unwrap();
    let task = Uuid::now_v7();
    let key = format!("images/avatars/{}.webp", Uuid::now_v7());
    repository::register_candidate(&pool, &auth, record.id, &key, task)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET security_version=security_version+1 WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        repository::commit_avatar_in(
            &mut tx,
            &auth,
            record.id,
            task,
            &key,
            "https://api.example/avatars/test"
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE users SET security_version=0 WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE avatar_uploads SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(record.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(service::confirm(&state, &auth, &source).await.is_err());
    assert_eq!(
        service::current_profile(&state, &auth)
            .await
            .unwrap()
            .avatar_url,
        ""
    );
}
