use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tsz_rust::{
    auth::extract::AuthUser,
    platform::storage::{
        MemoryAdapter, StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
    },
    speech::{SpeechError, SpeechErrorKind, SpeechProvider, SynthesisRequest, SynthesizedAudio},
    state::AppState,
    wordlists::custom_audio::worker,
};
use uuid::Uuid;

pub(super) struct Provider {
    fail: AtomicBool,
    pub(super) calls: AtomicUsize,
}
#[async_trait::async_trait]
impl SpeechProvider for Provider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    async fn synthesize(
        &self,
        request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(matches!(request.voice().locale(), "en-GB" | "en-US"));
        if self.fail.load(Ordering::SeqCst) && request.voice().locale() == "en-US" {
            return Err(SpeechError::new(SpeechErrorKind::Unavailable, None));
        }
        Ok(SynthesizedAudio {
            bytes: b"ID3 test sound".to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

pub(super) async fn fixture(pool: &PgPool, coins: i64) -> (AppState, AuthUser, Arc<Provider>) {
    let (mut state, auth) = setup_bound(pool).await;
    let provider = Arc::new(Provider {
        fail: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    state.speech_provider = Some(provider.clone());
    state.object_storage = StorageRegistry::from_stores([MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(
            StoragePrivacy::Private,
            1024 * 1024,
            std::time::Duration::from_secs(60),
            None,
        )
        .unwrap(),
    )])
    .unwrap();
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    if coins > 0 {
        account_deletion_support::fund(pool, &auth, coins).await;
    }
    (state, auth, provider)
}
fn item(id: Uuid, word: &str) -> Value {
    json!({"item_id":id,"custom_word":{"word":word,"definition":"释义"},"private_note":""})
}
fn body(items: Vec<Value>, ids: &[Uuid], cost: &str) -> Value {
    json!({"idempotency_key":Uuid::now_v7(),"name":"自定义语音","items":items,"audio_generation":{"item_ids":ids,"expected_cost":cost}})
}
async fn balance(pool: &PgPool, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COALESCE((SELECT balance FROM coin_wallets WHERE owner_type='user' AND owner_id=$1),0)").bind(user).fetch_one(pool).await.unwrap()
}
async fn due(pool: &PgPool) {
    sqlx::query(
        "UPDATE wordlist_audio_jobs SET next_attempt_at=clock_timestamp()-interval '1 second'",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn audio_save_atomic_charge_and_replay_with_two_locales(pool: PgPool) {
    let (state, auth, provider) = fixture(&pool, 10).await;
    let ids = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let input = body(
        vec![
            item(ids[0], "look forward to"),
            item(ids[1], "custom phrase"),
            item(ids[2], "read only"),
        ],
        &ids[..2],
        "4",
    );
    let (one, two) = tokio::join!(
        call(&state, &auth, "POST", "/api/v1/me/wordlists", input.clone()),
        call(&state, &auth, "POST", "/api/v1/me/wordlists", input.clone())
    );
    assert_eq!(one.0, StatusCode::OK, "{}", one.1);
    assert_eq!(one, two);
    let mut unavailable = state.clone();
    unavailable.speech_provider = None;
    unavailable.object_storage = StorageRegistry::default();
    assert_eq!(
        call(&unavailable, &auth, "POST", "/api/v1/me/wordlists", input).await,
        one
    );
    assert_eq!(balance(&pool, auth.subject).await, 6);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlist_audio_jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    worker::sweep(&state).await.unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    let list = one.1["id"].as_str().unwrap();
    let states = call(
        &state,
        &auth,
        "GET",
        &format!(
            "/api/v1/me/wordlists/{list}/custom-audio?item_ids={},{}",
            ids[0], ids[1]
        ),
        Value::Null,
    )
    .await;
    assert_eq!(states.0, StatusCode::OK, "{}", states.1);
    assert!(
        states.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["status"] == "ready" && v["locales"] == json!(["en-GB", "en-US"]))
    );
    for locale in ["en-GB", "en-US"] {
        assert_eq!(
            call(
                &state,
                &auth,
                "GET",
                &format!(
                    "/api/v1/me/wordlists/{list}/custom-audio/{}/{locale}",
                    ids[0]
                ),
                Value::Null
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let update = json!({"idempotency_key":Uuid::now_v7(),"expected_revision":1,"content":{"name":"改注","items":[{"item_id":ids[0],"custom_word":{"word":"look forward to","definition":"新释义"},"author_note":"新注"},{"item_id":ids[1],"custom_word":{"word":"custom phrase","definition":"释义"},"author_note":""},{"item_id":ids[2],"custom_word":{"word":"read only","definition":"释义"},"author_note":""}]},"note_updates":[],"audio_generation":{"item_ids":&ids[..2],"expected_cost":"0"}});
    let saved = call(
        &state,
        &auth,
        "PUT",
        &format!("/api/v1/me/wordlists/{list}"),
        update.clone(),
    )
    .await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    assert_eq!(
        call(
            &unavailable,
            &auth,
            "PUT",
            &format!("/api/v1/me/wordlists/{list}"),
            update
        )
        .await,
        saved
    );
    assert_eq!(balance(&pool, auth.subject).await, 6);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
}

#[sqlx::test]
async fn audio_insufficient_balance_and_cost_conflict_leave_no_save(pool: PgPool) {
    let (state, auth, _) = fixture(&pool, 1).await;
    let id = Uuid::now_v7();
    let input = body(vec![item(id, "sound")], &[id], "2");
    let result = call(&state, &auth, "POST", "/api/v1/me/wordlists", input.clone()).await;
    assert_eq!(
        result.1["code"], "coin_insufficient_balance",
        "{}",
        result.1
    );
    let mut mismatch = input;
    mismatch["audio_generation"]["expected_cost"] = json!("0");
    let result = call(&state, &auth, "POST", "/api/v1/me/wordlists", mismatch).await;
    assert_eq!(
        result.1["code"], "word_list_audio_cost_changed",
        "{}",
        result.1
    );
    assert_eq!(balance(&pool, auth.subject).await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlists")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlist_audio_jobs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn audio_partial_failure_refunds_once_even_after_account_disabled(pool: PgPool) {
    let (state, auth, provider) = fixture(&pool, 10).await;
    provider.fail.store(true, Ordering::SeqCst);
    let id = Uuid::now_v7();
    let saved = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        body(vec![item(id, "failure")], &[id], "2"),
    )
    .await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    for _ in 0..3 {
        due(&pool).await;
        worker::sweep(&state).await.unwrap();
    }
    assert_eq!(balance(&pool, auth.subject).await, 10);
    worker::sweep(&state).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM coin_operations WHERE source_type='wordlist_custom_audio_refund'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    sqlx::query(
        "UPDATE wordlist_audio_objects SET cleanup_after=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    worker::cleanup(&state).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM wordlist_audio_objects WHERE deleted_at IS NULL"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );

    let second = Uuid::now_v7();
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/wordlists",
            body(vec![item(second, "disabled")], &[second], "2")
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    worker::sweep(&state).await.unwrap();
    assert_eq!(balance(&pool, auth.subject).await, 10);
}

#[sqlx::test]
async fn audio_legacy_edit_cancels_pending_and_deletion_waits_for_settlement(pool: PgPool) {
    let (state, auth, _) = fixture(&pool, 10).await;
    let id = Uuid::now_v7();
    let saved = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        body(vec![item(id, "before")], &[id], "2"),
    )
    .await;
    let list = saved.1["id"].as_str().unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        tsz_rust::wordlists::custom_audio::ensure_settled(&mut tx, auth.subject)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let update = json!({"expected_revision":1,"content":{"name":"changed","items":[{"item_id":id,"custom_word":{"word":"after","definition":"释义"},"author_note":""}]},"note_updates":[]});
    assert_eq!(
        call(
            &state,
            &auth,
            "PUT",
            &format!("/api/v1/me/wordlists/{list}"),
            update
        )
        .await
        .0,
        StatusCode::OK
    );
    worker::sweep(&state).await.unwrap();
    assert_eq!(balance(&pool, auth.subject).await, 10);
    let mut tx = pool.begin().await.unwrap();
    tsz_rust::wordlists::custom_audio::ensure_settled(&mut tx, auth.subject)
        .await
        .unwrap();
}
