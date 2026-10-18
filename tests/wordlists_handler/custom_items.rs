use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;

async fn modern(state: &AppState, subject: Uuid, admin: bool, path: &str) -> (StatusCode, Value) {
    let token = if admin {
        state
            .admin_token_manager
            .generate_with_version(subject, "super_admin", 0)
            .unwrap()
    } else {
        state
            .token_manager
            .generate_with_version(subject, "student", 0)
            .unwrap()
    };
    let result = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .header("X-TSZ-Wordlist-Custom-Items", "v1")
                .header("X-TSZ-Wordlist-Annotations", "v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = result.status();
    let bytes = result.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn custom(id: Uuid, word: &str) -> Value {
    json!({"item_id":id,"custom_word":{"word":word,"definition":"自定义释义"},"author_note":"给读者的注"})
}
async fn create_custom(state: &AppState, auth: &AuthUser, items: Vec<Value>) -> Value {
    let body = json!({"idempotency_key":Uuid::now_v7(),"name":"混合词表","items":items});
    let created = call(state, auth, "POST", "/api/v1/me/wordlists", body.clone()).await;
    assert_eq!(created.0, StatusCode::OK, "{}", created.1);
    assert_eq!(
        created,
        call(state, auth, "POST", "/api/v1/me/wordlists", body).await
    );
    created.1
}

#[sqlx::test]
async fn mixed_items_keep_identity_notes_review_snapshots_and_learning_sources(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let (_, other) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let platform = entry(&pool, "apple").await;
    let key = Uuid::now_v7();
    let mut initial = custom(key, "my custom phrase");
    initial["private_note"] = json!("CUSTOM PRIVATE");
    let list = create_custom(
        &state,
        &auth,
        vec![
            json!({"entry_id":platform,"private_note":"PLATFORM PRIVATE"}),
            initial,
        ],
    )
    .await;
    let id = Uuid::parse_str(list["id"].as_str().unwrap()).unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    let items = modern(&state, auth.subject, false, &format!("{path}/items")).await;
    assert_eq!(items.0, 200, "{}", items.1);
    assert_eq!(items.1["items"][1]["item_id"], key.to_string());
    assert_eq!(items.1["items"][1]["entry_id"], Value::Null);
    assert_eq!(
        items.1["items"][1]["custom_word"]["word"],
        "my custom phrase"
    );
    assert_eq!(items.1["items"][1]["private_note"], "CUSTOM PRIVATE");
    assert_eq!(
        modern(&state, other.subject, false, &format!("{path}/items"))
            .await
            .0,
        404
    );
    assert_eq!(
        call(&state, &auth, "GET", &format!("{path}/items"), Value::Null)
            .await
            .0,
        409
    );
    assert_eq!(
        call(&state, &auth, "GET", &format!("{path}/edit"), Value::Null)
            .await
            .0,
        409
    );
    let edit = modern(&state, auth.subject, false, &format!("{path}/edit")).await;
    assert_eq!(edit.1["entry_ids"], json!([platform]));
    assert_eq!(edit.1["items"][1]["item_id"], key.to_string());
    let available = modern(&state, auth.subject, false, &format!("{path}/availability")).await;
    assert_eq!(
        available.1,
        json!({"revision":1,"available_count":2,"unavailable_count":0,"custom_count":1})
    );
    let membership: Uuid = sqlx::query_scalar(
        "SELECT learning_membership_id FROM wordlist_items WHERE wordlist_id=$1 AND entry_id=$2",
    )
    .bind(id)
    .bind(platform)
    .fetch_one(&pool)
    .await
    .unwrap();
    let legacy = json!({"expected_revision":1,"content":{"name":"legacy","entry_ids":[platform]},"note_updates":[]});
    assert_eq!(call(&state, &auth, "PUT", &path, legacy).await.0, 409);
    let changed=call(&state,&auth,"PUT",&path,json!({"expected_revision":1,"content":{"name":"混合词表","items":[custom(key,"renamed phrase"),{"item_id":platform,"entry_id":platform,"author_note":"平台注"}]},"note_updates":[{"item_id":key,"expected_note_revision":1,"private_note":"CHANGED PRIVATE"}]})).await;
    assert_eq!(changed.0, 200, "{}", changed.1);
    let reread = modern(
        &state,
        auth.subject,
        false,
        &format!("{path}/items?sort=label_desc&q=renamed"),
    )
    .await;
    assert_eq!(reread.1["pagination"]["total"], 1);
    assert_eq!(reread.1["items"][0]["item_id"], key.to_string());
    assert_eq!(reread.1["items"][0]["private_note"], "CHANGED PRIVATE");
    assert_eq!(reread.1["items"][0]["note_revision"], 2);
    let retained: Uuid = sqlx::query_scalar(
        "SELECT learning_membership_id FROM wordlist_items WHERE wordlist_id=$1 AND entry_id=$2",
    )
    .bind(id)
    .bind(platform)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(membership, retained);
    sqlx::query(
        "INSERT INTO student_profiles(user_id,cefr_level,english_variant) VALUES($1,'A1','BrE')",
    )
    .bind(auth.subject)
    .execute(&pool)
    .await
    .unwrap();
    let preview=call(&state,&auth,"POST","/api/v1/me/learning-tasks/preview",json!({"task_type":"longterm","wordlist_ids":[id],"daily_question_count":null,"ends_at":null})).await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert_eq!(preview.1["entry_count"], 1);
    let submitted = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/publish"),
        json!({"expected_revision":2,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    assert_eq!(submitted.0, 200, "{}", submitted.1);
    assert_eq!(submitted.1["state"], "pending");
    let request = submitted.1["id"].as_str().unwrap();
    let review_path = format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/items");
    let snapshot = modern(&state, manager, true, &review_path).await;
    assert_eq!(snapshot.0, 200, "{}", snapshot.1);
    assert_eq!(
        snapshot.1["items"][0]["custom_word"]["word"],
        "renamed phrase"
    );
    assert!(!snapshot.1.to_string().contains("PRIVATE"));
    assert!(!snapshot.1.to_string().contains("private_note"));
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &format!("{path}/withdraw"),
            json!({"expected_revision":3})
        )
        .await
        .0,
        200
    );
    let updated=call(&state,&auth,"PUT",&path,json!({"expected_revision":4,"content":{"name":"新版本","items":[custom(key,"new version"),{"item_id":platform,"entry_id":platform,"author_note":""}]},"note_updates":[]})).await;
    assert_eq!(updated.0, 200, "{}", updated.1);
    assert_eq!(
        modern(&state, manager, true, &review_path).await.1["items"][0]["custom_word"]["word"],
        "renamed phrase"
    );
    assert_eq!(
        admin_call(
            &state,
            manager,
            "POST",
            &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
            json!({"expected_revision":3,"approve":true,"reason":null})
        )
        .await
        .0,
        409
    );
    let submission = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/publish"),
        json!({"expected_revision":5,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = submission.1["id"].as_str().unwrap();
    // Recommendation metadata has its own revision; it must not invalidate an unchanged review snapshot.
    assert_eq!(
        admin_call(
            &state,
            manager,
            "POST",
            &format!("/api/v1/admin/wordlists/{id}/recommendation"),
            json!({"expected_revision":6,"recommended":false})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        admin_call(
            &state,
            manager,
            "POST",
            &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
            json!({"expected_revision":6,"approve":true,"reason":null})
        )
        .await
        .0,
        200
    );
    let public = modern(
        &state,
        other.subject,
        false,
        &format!("/api/v1/wordlists/{id}/items"),
    )
    .await;
    assert_eq!(public.0, 200, "{}", public.1);
    assert_eq!(public.1["items"][0]["custom_word"]["word"], "new version");
    assert!(!public.1.to_string().contains("private_note"));
}

#[sqlx::test]
async fn custom_only_lists_validate_content_and_have_no_learning_entries(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let key = Uuid::now_v7();
    for (word, definition) in [
        ("".to_string(), "definition".to_string()),
        ("a".repeat(51), "d".into()),
        ("a".into(), " ".into()),
        ("a".into(), "d".repeat(201)),
    ] {
        let result=call(&state,&auth,"POST","/api/v1/me/wordlists",json!({"idempotency_key":Uuid::now_v7(),"name":"invalid","items":[{"item_id":key,"custom_word":{"word":word,"definition":definition},"private_note":""}]})).await;
        assert_eq!(result.0, 400, "{}", result.1);
    }
    let mut item = custom(key, "自定义");
    item["private_note"] = json!("");
    let list = create_custom(&state, &auth, vec![item]).await;
    let id = Uuid::parse_str(list["id"].as_str().unwrap()).unwrap();
    sqlx::query(
        "INSERT INTO student_profiles(user_id,cefr_level,english_variant) VALUES($1,'A1','BrE')",
    )
    .bind(auth.subject)
    .execute(&pool)
    .await
    .unwrap();
    let preview=call(&state,&auth,"POST","/api/v1/me/learning-tasks/preview",json!({"task_type":"longterm","wordlist_ids":[id],"daily_question_count":null,"ends_at":null})).await;
    assert_eq!(preview.0, 200, "{}", preview.1);
    assert_eq!(preview.1["entry_count"], 0);
    assert_eq!(preview.1["can_start"], false);
    let submitted = call(
        &state,
        &auth,
        "POST",
        &format!("/api/v1/me/wordlists/{id}/publish"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    assert_eq!(submitted.0, 200, "{}", submitted.1);
    // Guard persists even after live custom rows are gone: historical review text must survive.
    sqlx::query("DELETE FROM wordlist_items WHERE wordlist_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let error = sqlx::raw_sql(include_str!(
        "../../migrations/20261018120000_wordlist_custom_items.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("custom wordlist content"),
        "{error}"
    );
    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn custom_item_migration_empty_roundtrip(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261018120000_wordlist_custom_items.down.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20261018120000_wordlist_custom_items.up.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
