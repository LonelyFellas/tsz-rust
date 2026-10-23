use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;
use tsz_rust::{
    account_deletion::service as deletion,
    auth::extract::AuthUser,
    platform::storage::{ObjectKey, PutOptions, StorageError, StorageSpace},
    speech::{SpeechError, SpeechProvider, SynthesisRequest, SynthesizedAudio},
    state::AppState,
    wordlists::custom_audio::worker,
};
use uuid::Uuid;

use super::custom_audio::fixture;

async fn save_job(state: &AppState, auth: &AuthUser) -> (Uuid, Uuid, Uuid) {
    let item = Uuid::now_v7();
    let saved = call(
        state,
        auth,
        "POST",
        "/api/v1/me/wordlists",
        json!({
            "idempotency_key":Uuid::now_v7(),"name":"语音生命周期",
            "items":[{"item_id":item,"custom_word":{"word":"sound","definition":"声音"},"private_note":""}],
            "audio_generation":{"item_ids":[item],"expected_cost":"2"}
        }),
    )
    .await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    let list = Uuid::parse_str(saved.1["id"].as_str().unwrap()).unwrap();
    let job = sqlx::query_scalar(
        "SELECT id FROM wordlist_audio_jobs WHERE wordlist_id=$1 AND item_id=$2",
    )
    .bind(list)
    .bind(item)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    (list, item, job)
}

async fn wallet(pool: &PgPool, user: Uuid) -> (i64, String) {
    sqlx::query_as(
        "SELECT balance,status FROM coin_wallets WHERE owner_type='user' AND owner_id=$1",
    )
    .bind(user)
    .fetch_one(pool)
    .await
    .unwrap()
}

struct PausedProvider {
    inner: Arc<dyn SpeechProvider>,
    paused: AtomicBool,
    entered: Notify,
    release: Notify,
}

#[async_trait::async_trait]
impl SpeechProvider for PausedProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }

    async fn synthesize(
        &self,
        request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        if !self.paused.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.synthesize(request).await
    }
}

#[sqlx::test]
async fn concurrent_sweeps_deliver_one_pair_and_keep_one_charge(pool: PgPool) {
    let (mut state, auth, provider) = fixture(&pool, 10).await;
    let gate = Arc::new(PausedProvider {
        inner: provider.clone(),
        paused: AtomicBool::new(false),
        entered: Notify::new(),
        release: Notify::new(),
    });
    state.speech_provider = Some(gate.clone());
    let (_, _, job) = save_job(&state, &auth).await;
    let running_state = state.clone();
    let first = tokio::spawn(async move { worker::sweep(&running_state).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    // The first worker holds a live lease while synthesis is deliberately paused.
    let second =
        tokio::time::timeout(std::time::Duration::from_secs(5), worker::sweep(&state)).await;
    gate.release.notify_one();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first + second.unwrap().unwrap(), 1);
    worker::sweep(&state).await.unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let outcome: (String,i32,i64) = sqlx::query_as("SELECT state,attempts,(SELECT count(*) FROM wordlist_audio_objects o WHERE o.job_id=j.id AND o.uploaded) FROM wordlist_audio_jobs j WHERE id=$1")
        .bind(job).fetch_one(&pool).await.unwrap();
    assert_eq!(outcome, ("ready".into(), 1, 2));
    assert_eq!(wallet(&pool, auth.subject).await, (8, "open".into()));
    let charges: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM coin_operations WHERE source_type='wordlist_custom_audio'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(charges, 1);
}

#[sqlx::test]
async fn expired_processing_lease_recovers_and_never_signs_old_attempt(pool: PgPool) {
    let (state, auth, provider) = fixture(&pool, 10).await;
    let (list, item, job) = save_job(&state, &auth).await;
    let old = Uuid::now_v7();
    sqlx::query("UPDATE wordlist_audio_jobs SET state='processing',attempts=1,lease_token=$2,lease_until=clock_timestamp()-interval '1 second',next_attempt_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job).bind(old).execute(&pool).await.unwrap();
    let store = state
        .object_storage
        .get(&StorageSpace::parse("audio").unwrap())
        .unwrap();
    let mut old_keys = Vec::new();
    for locale in ["en-GB", "en-US"] {
        let key = ObjectKey::parse(format!("wordlist-custom/{job}/{old}/{locale}.mp3")).unwrap();
        store
            .put(&key, b"stale audio".to_vec(), PutOptions::new(None))
            .await
            .unwrap();
        sqlx::query("INSERT INTO wordlist_audio_objects(job_id,attempt_token,locale,object_key,uploaded,cleanup_after) VALUES($1,$2,$3,$4,true,clock_timestamp()+interval '1 hour')")
            .bind(job).bind(old).bind(locale).bind(key.as_str()).execute(&pool).await.unwrap();
        old_keys.push(key);
    }
    assert_eq!(worker::sweep(&state).await.unwrap(), 1);
    let (status, current, attempts): (String, Uuid, i32) =
        sqlx::query_as("SELECT state,lease_token,attempts FROM wordlist_audio_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "ready");
    assert_ne!(current, old);
    assert_eq!(attempts, 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(wallet(&pool, auth.subject).await.0, 8);
    for locale in ["en-GB", "en-US"] {
        let path = format!("/api/v1/me/wordlists/{list}/custom-audio/{item}/{locale}");
        let signed = call(&state, &auth, "GET", &path, Value::Null).await;
        assert_eq!(signed.0, StatusCode::OK, "{}", signed.1);
        let url = signed.1["url"].as_str().unwrap();
        assert!(url.contains(&current.to_string()));
        assert!(!url.contains(&old.to_string()));
    }
    // A missing current attempt must not fall back to the uploaded expired one.
    sqlx::query("UPDATE wordlist_audio_objects SET uploaded=false WHERE job_id=$1 AND attempt_token=$2 AND locale='en-GB'")
        .bind(job).bind(current).execute(&pool).await.unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("/api/v1/me/wordlists/{list}/custom-audio/{item}/en-GB"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE wordlist_audio_objects SET cleanup_after=clock_timestamp()-interval '1 second' WHERE job_id=$1 AND attempt_token=$2")
        .bind(job).bind(old).execute(&pool).await.unwrap();
    worker::cleanup(&state).await.unwrap();
    for key in old_keys {
        assert!(matches!(
            store.stat(&key).await,
            Err(StorageError::ObjectNotFound { .. })
        ));
    }
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM wordlist_audio_objects WHERE job_id=$1 AND attempt_token=$2 AND deleted_at IS NULL")
        .bind(job).bind(old).fetch_one(&pool).await.unwrap();
    assert_eq!(remaining, 0);
}

#[sqlx::test]
async fn delivered_audio_allows_account_deletion_and_cleans_objects_after_owner_removal(
    pool: PgPool,
) {
    let (state, auth, _) = fixture(&pool, 10).await;
    let (list, item, job) = save_job(&state, &auth).await;
    worker::sweep(&state).await.unwrap();
    // Persist an edit receipt and a review reference as well as the job/object rows.
    let edited=call(&state,&auth,"PUT",&format!("/api/v1/me/wordlists/{list}"),json!({
        "idempotency_key":Uuid::now_v7(),"expected_revision":1,
        "content":{"name":"语音生命周期已保存","items":[{"item_id":item,"custom_word":{"word":"sound","definition":"声音"}}]},
        "note_updates":[],"audio_generation":{"item_ids":[item],"expected_cost":"0"}
    })).await;
    assert_eq!(edited.0, StatusCode::OK, "{}", edited.1);
    let submitted = call(
        &state,
        &auth,
        "POST",
        &format!("/api/v1/me/wordlists/{list}/review-requests"),
        json!({
            "idempotency_key":Uuid::now_v7(),"expected_revision":edited.1["revision"]
        }),
    )
    .await;
    assert_eq!(submitted.0, StatusCode::OK, "{}", submitted.1);
    let refs: (i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM wordlist_update_requests WHERE wordlist_id=$1),(SELECT count(*) FROM wordlist_audio_review_refs WHERE job_id=$2)")
        .bind(list).bind(job).fetch_one(&pool).await.unwrap();
    assert_eq!(refs, (1, 1));
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT object_key FROM wordlist_audio_objects WHERE job_id=$1 AND uploaded",
    )
    .bind(job)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(keys.len(), 2);
    let store = state
        .object_storage
        .get(&StorageSpace::parse("audio").unwrap())
        .unwrap();
    for key in &keys {
        assert!(store.stat(&ObjectKey::parse(key).unwrap()).await.is_ok());
    }
    let request = account_deletion_support::apply(&state, &auth, "8").await;
    account_deletion_support::deadline(&pool, request.id, -1).await;
    assert!(
        deletion::complete(&pool, auth.subject, request.id)
            .await
            .unwrap()
    );
    let remaining: (bool,bool,i64,i64,i64) = sqlx::query_as("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1),EXISTS(SELECT 1 FROM wordlists WHERE id=$2),(SELECT count(*) FROM wordlist_audio_jobs WHERE id=$3),(SELECT count(*) FROM wordlist_update_requests WHERE wordlist_id=$2),(SELECT count(*) FROM wordlist_audio_review_refs WHERE job_id=$3)")
        .bind(auth.subject).bind(list).bind(job).fetch_one(&pool).await.unwrap();
    assert_eq!(remaining, (false, false, 1, 0, 0));
    let retained_word: String =
        sqlx::query_scalar("SELECT word FROM wordlist_audio_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retained_word, "已删除");
    sqlx::query("UPDATE wordlist_audio_objects SET cleanup_after=clock_timestamp()-interval '1 second' WHERE job_id=$1")
        .bind(job).execute(&pool).await.unwrap();
    worker::cleanup(&state).await.unwrap();
    for key in keys {
        assert!(matches!(
            store.stat(&ObjectKey::parse(key).unwrap()).await,
            Err(StorageError::ObjectNotFound { .. })
        ));
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wordlist_audio_objects WHERE job_id=$1 AND deleted_at IS NULL",
    )
    .bind(job)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(wallet(&pool, auth.subject).await, (0, "closed".into()));
}

#[sqlx::test]
async fn unsettled_audio_rejects_real_deletion_request_without_freezing_wallet(pool: PgPool) {
    let (state, auth, _) = fixture(&pool, 10).await;
    let (_, _, job) = save_job(&state, &auth).await;
    account_deletion_support::code(&state, &auth).await;
    let requested = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/account-deletion",
        account_deletion_support::input(Uuid::now_v7(), "8"),
    )
    .await;
    assert_eq!(requested.0, StatusCode::CONFLICT, "{}", requested.1);
    assert_eq!(requested.1["code"], "account_deletion_unsettled_audio");
    assert_eq!(wallet(&pool, auth.subject).await, (8, "open".into()));
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_deletion_requests WHERE user_id=$1")
            .bind(auth.subject)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, 0);
    let status: String = sqlx::query_scalar("SELECT state FROM wordlist_audio_jobs WHERE id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "queued");
    worker::sweep(&state).await.unwrap();
    let accepted = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/account-deletion",
        account_deletion_support::input(Uuid::now_v7(), "8"),
    )
    .await;
    assert_eq!(accepted.0, StatusCode::ACCEPTED, "{}", accepted.1);
    assert_eq!(
        wallet(&pool, auth.subject).await,
        (8, "deletion_pending".into())
    );
}

#[sqlx::test]
async fn custom_audio_migration_roundtrips_empty_and_refuses_job_loss(pool: PgPool) {
    let down = include_str!("../../migrations/20261023090000_wordlist_custom_audio.down.sql");
    let up = include_str!("../../migrations/20261023090000_wordlist_custom_audio.up.sql");
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(down).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(up).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let (state, auth, _) = fixture(&pool, 10).await;
    let (_, _, job) = save_job(&state, &auth).await;
    let mut tx = pool.begin().await.unwrap();
    let failure = sqlx::raw_sql(down).execute(&mut *tx).await.unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("refusing destructive rollback"),
        "{failure}"
    );
    tx.rollback().await.unwrap();
    let preserved: String = sqlx::query_scalar("SELECT state FROM wordlist_audio_jobs WHERE id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(preserved, "queued");
    assert_eq!(wallet(&pool, auth.subject).await, (8, "open".into()));
}
