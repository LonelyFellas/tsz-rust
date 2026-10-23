use super::custom_audio::fixture;
use super::*;
use tsz_rust::wordlists::custom_audio::worker;
use uuid::Uuid;

#[sqlx::test]
async fn create_key_reused_by_update_still_charges_and_refunds_each_batch(pool: PgPool) {
    let (state, auth, _) = fixture(&pool, 10).await;
    let key = Uuid::now_v7();
    let item = Uuid::now_v7();
    let saved = call(&state,&auth,"POST","/api/v1/me/wordlists",json!({
        "idempotency_key":key,"name":"charge replay","items":[{"item_id":item,"custom_word":{"word":"first","definition":"一"},"private_note":""}],
        "audio_generation":{"item_ids":[item],"expected_cost":"2"}
    })).await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    let list = saved.1["id"].as_str().unwrap();
    let changed = call(&state,&auth,"PUT",&format!("/api/v1/me/wordlists/{list}"),json!({
        "idempotency_key":key,"expected_revision":1,"content":{"name":"charge replay","items":[{"item_id":item,"custom_word":{"word":"second","definition":"二"}}]},"note_updates":[],
        "audio_generation":{"item_ids":[item],"expected_cost":"2"}
    })).await;
    assert_eq!(changed.0, StatusCode::OK, "{}", changed.1);
    let balance: i64 = sqlx::query_scalar("SELECT balance FROM coin_wallets WHERE owner_id=$1")
        .bind(auth.subject)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        balance, 6,
        "two independently accepted generations must each charge"
    );
    worker::sweep(&state).await.unwrap();
    let outcome: (i64,i64) = sqlx::query_as("SELECT (SELECT balance FROM coin_wallets WHERE owner_id=$1),(SELECT count(*) FROM wordlist_audio_jobs WHERE state='refunded')").bind(auth.subject).fetch_one(&pool).await.unwrap();
    assert_eq!(outcome, (8, 1));
}

#[sqlx::test]
async fn recycle_and_restore_retains_paid_audio_without_regeneration(pool: PgPool) {
    let (state, auth, provider) = fixture(&pool, 10).await;
    let item = Uuid::now_v7();
    let saved=call(&state,&auth,"POST","/api/v1/me/wordlists",json!({"idempotency_key":Uuid::now_v7(),"name":"recycle sound","items":[{"item_id":item,"custom_word":{"word":"sound","definition":"声音"},"private_note":""}],"audio_generation":{"item_ids":[item],"expected_cost":"2"}})).await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    let list = Uuid::parse_str(saved.1["id"].as_str().unwrap()).unwrap();
    worker::sweep(&state).await.unwrap();
    let node: Uuid = sqlx::query_scalar(
        "SELECT id FROM wordlist_library_nodes WHERE wordlist_id=$1 AND user_id=$2",
    )
    .bind(list)
    .bind(auth.subject)
    .fetch_one(&pool)
    .await
    .unwrap();
    for action in ["trash", "restore"] {
        let current = call(
            &state,
            &auth,
            "GET",
            "/api/v1/me/wordlist-library/owned",
            Value::Null,
        )
        .await;
        let result=call(&state,&auth,"POST","/api/v1/me/wordlist-library/owned/operations",json!({"expected_version":current.1["version"],"action":action,"ids":[node],"target_id":null,"original_location":action=="restore"})).await;
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
        if action == "trash" {
            sqlx::query("UPDATE wordlist_audio_objects SET cleanup_after=clock_timestamp()-interval '1 second'").execute(&pool).await.unwrap();
            worker::cleanup(&state).await.unwrap();
        }
    }
    let signed = call(
        &state,
        &auth,
        "GET",
        &format!("/api/v1/me/wordlists/{list}/custom-audio/{item}/en-GB"),
        Value::Null,
    )
    .await;
    assert_eq!(signed.0, StatusCode::OK, "{}", signed.1);
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[sqlx::test]
async fn deleted_objects_leave_cleanup_until_a_late_upload_reopens_them(pool: PgPool) {
    let (state, auth, _) = fixture(&pool, 10).await;
    let item = Uuid::now_v7();
    let saved=call(&state,&auth,"POST","/api/v1/me/wordlists",json!({"idempotency_key":Uuid::now_v7(),"name":"cleanup once","items":[{"item_id":item,"custom_word":{"word":"sound","definition":"声音"},"private_note":""}],"audio_generation":{"item_ids":[item],"expected_cost":"2"}})).await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    worker::sweep(&state).await.unwrap();
    sqlx::query("DELETE FROM wordlist_items WHERE item_id=$1")
        .bind(item)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE wordlist_audio_objects SET cleanup_after=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    worker::cleanup(&state).await.unwrap();
    let first: Vec<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT object_key,deleted_at FROM wordlist_audio_objects ORDER BY object_key",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(first.len(), 2);
    sqlx::query(
        "UPDATE wordlist_audio_objects SET cleanup_after=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    worker::cleanup(&state).await.unwrap();
    let second: Vec<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT object_key,deleted_at FROM wordlist_audio_objects ORDER BY object_key",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        first, second,
        "completed cleanup must not delete the same key again"
    );
    let store = state.object_storage.get(&"audio".parse().unwrap()).unwrap();
    let key = tsz_rust::platform::storage::ObjectKey::parse(&first[0].0).unwrap();
    store
        .put(
            &key,
            b"late upload".to_vec(),
            tsz_rust::platform::storage::PutOptions::default(),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE wordlist_audio_objects SET deleted_at=NULL WHERE object_key=$1")
        .bind(key.as_str())
        .execute(&pool)
        .await
        .unwrap();
    worker::cleanup(&state).await.unwrap();
    assert!(matches!(
        store.stat(&key).await,
        Err(tsz_rust::platform::storage::StorageError::ObjectNotFound { .. })
    ));
}
