use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tsz_rust::{auth::extract::AuthUser, state::AppState};
use uuid::Uuid;

async fn annotated(
    state: &AppState,
    subject: Uuid,
    admin: bool,
    path: &str,
    version: &str,
) -> (StatusCode, Value) {
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
                .header("X-TSZ-Wordlist-Annotations", version)
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
fn content(id: Uuid, note: &str) -> Value {
    json!({"name":"我的词表","entry_ids":[id],"author_notes":[{"entry_id":id,"author_note":note}]})
}
async fn create(state: &AppState, owner: &AuthUser, entry: Uuid, note: &str) -> Value {
    let mut input = create_body(&[entry]);
    input["items"][0]["author_note"] = json!(note);
    let result = call(state, owner, "POST", "/api/v1/me/wordlists", input.clone()).await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    assert_eq!(
        result,
        call(state, owner, "POST", "/api/v1/me/wordlists", input).await
    );
    result.1
}

#[sqlx::test]
async fn author_annotations_are_public_content_and_old_clients_keep_their_shape(pool: PgPool) {
    let (state, owner) = setup_bound(&pool).await;
    let (_, other) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let entry = entry(&pool, "author-notes").await;
    let created = create(&state, &owner, entry, "给读者的原注释").await;
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    let old = call(&state, &owner, "GET", &format!("{path}/items"), Value::Null).await;
    assert_eq!(old.0, StatusCode::OK);
    assert!(old.1["items"][0].get("author_note").is_none());
    assert_eq!(old.1["items"][0]["private_note"], "私密备注");
    assert_eq!(
        annotated(&state, other.subject, false, &format!("{path}/items"), "v1")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let published = call(
        &state,
        &owner,
        "POST",
        &format!("{path}/publish"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    assert_eq!(published.0, StatusCode::OK, "{}", published.1);
    let request = published.1["id"].as_str().unwrap();
    let public_path = format!("/api/v1/wordlists/{id}/items");
    let visible = annotated(&state, other.subject, false, &public_path, "v1").await;
    assert_eq!(visible.0, StatusCode::OK);
    assert_eq!(visible.1["items"][0]["author_note"], "给读者的原注释");
    assert!(!visible.1.to_string().contains("私密备注"));
    assert!(visible.1["items"][0].get("private_note").is_none());
    let legacy = call(&state, &other, "GET", &public_path, Value::Null).await;
    assert_eq!(legacy.1["items"][0].as_object().unwrap().len(), 3);
    assert!(
        annotated(&state, other.subject, false, &public_path, "v2")
            .await
            .1["items"][0]
            .get("author_note")
            .is_none()
    );
    let update =
        json!({"expected_revision":2,"content":content(entry,"更新后的注释"),"note_updates":[]});
    assert_eq!(
        call(&state, &owner, "PUT", &path, update.clone()).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&state, &other, "PUT", &path, update).await.0,
        StatusCode::NOT_FOUND
    );
    let private = call(&state,&owner,"PUT",&path,json!({"expected_revision":2,"content":null,"note_updates":[{"entry_id":entry,"expected_note_revision":1,"private_note":"仍然只有作者能看"}]})).await;
    assert_eq!(private.0, StatusCode::OK);
    assert_eq!(private.1["revision"], 2);
    assert_eq!(private.1["state"], "published");
    assert_eq!(
        call(
            &state,
            &owner,
            "POST",
            &format!("{path}/withdraw"),
            json!({"expected_revision":2})
        )
        .await
        .0,
        StatusCode::OK
    );
    let saved = call(
        &state,
        &owner,
        "PUT",
        &path,
        json!({"expected_revision":3,"content":content(entry,"更新后的注释"),"note_updates":[]}),
    )
    .await;
    assert_eq!(saved.0, StatusCode::OK, "{}", saved.1);
    assert_eq!(saved.1["revision"], 4);
    let mine = annotated(&state, owner.subject, false, &format!("{path}/items"), "v1").await;
    assert_eq!(mine.1["items"][0]["author_note"], "更新后的注释");
    assert_eq!(mine.1["items"][0]["private_note"], "仍然只有作者能看");
    assert_eq!(mine.1["items"][0]["note_revision"], 2);
    let snapshot = annotated(
        &state,
        manager,
        true,
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/items"),
        "v1",
    )
    .await;
    assert_eq!(snapshot.1["items"][0]["author_note"], "给读者的原注释");
    assert!(!snapshot.1.to_string().contains("仍然只有作者能看"));
    // An old client changing retained content must not erase annotations it cannot read.
    let legacy_save = call(&state,&owner,"PUT",&path,json!({"expected_revision":4,"content":{"name":"旧客户端改名","entry_ids":[entry]},"note_updates":[]})).await;
    assert_eq!(legacy_save.0, StatusCode::OK);
    assert_eq!(
        annotated(&state, owner.subject, false, &format!("{path}/items"), "v1")
            .await
            .1["items"][0]["author_note"],
        "更新后的注释"
    );
}

#[sqlx::test]
async fn pending_annotations_are_immutable_and_invalid_updates_do_not_write(pool: PgPool) {
    let (state, owner) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let entry = entry(&pool, "pending-author-note").await;
    let created = create(&state, &owner, entry, "待审核注释").await;
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    for invalid in [
        json!({"name":"我的词表","entry_ids":[entry],"author_notes":[]}),
        content(Uuid::now_v7(), "错行"),
        content(entry, &"字".repeat(1001)),
    ] {
        let invalid = if invalid["author_notes"][0]["author_note"] == "错行" {
            json!({"name":"我的词表","entry_ids":[entry],"author_notes":invalid["author_notes"]})
        } else {
            invalid
        };
        assert_eq!(
            call(
                &state,
                &owner,
                "PUT",
                &path,
                json!({"expected_revision":1,"content":invalid,"note_updates":[]})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(&state, &owner, "GET", &path, Value::Null).await.1["revision"],
        1
    );
    let pending = call(
        &state,
        &owner,
        "POST",
        &format!("{path}/review-requests"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    assert_eq!(pending.0, StatusCode::OK);
    let request = pending.1["id"].as_str().unwrap();
    assert_eq!(
        call(
            &state,
            &owner,
            "PUT",
            &path,
            json!({"expected_revision":2,"content":content(entry,"绕过待审修改"),"note_updates":[]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let items = annotated(
        &state,
        manager,
        true,
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/items"),
        "v1",
    )
    .await;
    assert_eq!(items.1["items"][0]["author_note"], "待审核注释");
    let approved = admin_call(
        &state,
        manager,
        "POST",
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
        json!({"expected_revision":2,"approve":true,"reason":null}),
    )
    .await;
    assert_eq!(approved.0, StatusCode::OK);
    assert_eq!(
        annotated(
            &state,
            owner.subject,
            false,
            &format!("/api/v1/wordlists/{id}/items"),
            "v1"
        )
        .await
        .1["items"][0]["author_note"],
        "待审核注释"
    );
}

#[sqlx::test]
async fn migration_preserves_private_notes_and_guards_lossy_rollback(pool: PgPool) {
    let down = include_str!("../../migrations/20261013160000_wordlist_author_notes.down.sql");
    let up = include_str!("../../migrations/20261013160000_wordlist_author_notes.up.sql");
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    let (_, owner) = setup_bound(&pool).await;
    let entry = entry(&pool, "legacy-private").await;
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO wordlists(id,owner_user_id,name,create_key,create_hash) VALUES($1,$2,'历史词表',$3,'hash')").bind(id).bind(owner.subject).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO wordlist_items(wordlist_id,entry_id,position,private_note,note_revision) VALUES($1,$2,0,'不能自动公开的历史备注',7)").bind(id).bind(entry).execute(&pool).await.unwrap();
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    let notes: (String, String, i64) = sqlx::query_as(
        "SELECT author_note,private_note,note_revision FROM wordlist_items WHERE wordlist_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(notes, ("".into(), "不能自动公开的历史备注".into(), 7));
    sqlx::raw_sql(down).execute(&pool).await.unwrap();
    sqlx::raw_sql(up).execute(&pool).await.unwrap();
    sqlx::query("UPDATE wordlist_items SET author_note='新公开内容' WHERE wordlist_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(sqlx::raw_sql(down).execute(&pool).await.is_err());
    sqlx::query("UPDATE wordlist_items SET author_note='' WHERE wordlist_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO wordlist_review_requests(id,wordlist_id,submitted_revision,name,entry_ids,state,submit_key,request_hash,author_notes) VALUES($1,$2,1,'历史快照',ARRAY[$3]::uuid[],'rejected',$4,'hash',$5)")
        .bind(Uuid::now_v7()).bind(id).bind(entry).bind(Uuid::now_v7())
        .bind(json!({(entry.to_string()): "历史公开注释"})).execute(&pool).await.unwrap();
    assert!(sqlx::raw_sql(down).execute(&pool).await.is_err());
    let preserved: String =
        sqlx::query_scalar("SELECT private_note FROM wordlist_items WHERE wordlist_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(preserved, "不能自动公开的历史备注");
}

#[test]
fn old_creation_serialization_and_idempotency_hash_are_unchanged() {
    let input = create_body(&[Uuid::now_v7()]);
    let parsed: tsz_rust::wordlists::dto::CreateWordlist =
        serde_json::from_value(input.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), input);
    let expected = format!(
        r#"{{"idempotency_key":"{}","name":"我的词表","items":[{{"entry_id":"{}","private_note":"私密备注"}}]}}"#,
        parsed.idempotency_key, parsed.items[0].entry_id
    );
    assert_eq!(serde_json::to_vec(&parsed).unwrap(), expected.as_bytes());
}
