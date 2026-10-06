mod account_deletion_support;
mod wordlists_support;
use account_deletion_support::{apply, call, deadline, setup_bound};
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use wordlists_support::*;

#[sqlx::test]
async fn private_roundtrip_idempotency_revision_and_archive_boundary(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let (_, other) = setup_bound(&pool).await;
    let a = entry(&pool, "apple").await;
    let b = entry(&pool, "banana").await;
    let input = create_body(&[a, b]);
    let (one, two) = tokio::join!(
        call(&state, &auth, "POST", "/api/v1/me/wordlists", input.clone()),
        call(&state, &auth, "POST", "/api/v1/me/wordlists", input.clone())
    );
    assert_eq!(one.0, StatusCode::OK, "{}", one.1);
    assert_eq!(one, two);
    let id = one.1["id"].as_str().unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    let mut changed = input.clone();
    changed["name"] = json!("different");
    assert_eq!(
        call(&state, &auth, "POST", "/api/v1/me/wordlists", changed)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&state, &other, "GET", &path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("/api/v1/wordlists/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let items = call(&state, &auth, "GET", &format!("{path}/items"), Value::Null).await;
    assert_eq!(items.0, StatusCode::OK, "{}", items.1);
    assert_eq!(items.1["items"][0]["private_note"], "私密备注");
    assert_eq!(items.1["items"][0]["entry"]["label"], "apple");
    assert!(!items.1.to_string().contains("ADMIN PRIVATE"));
    let update = json!({"expected_revision":1,"content":{"name":"排序","entry_ids":[b,a]},"note_updates":[{"entry_id":a,"expected_note_revision":1,"private_note":"changed"}]});
    assert_eq!(
        call(&state, &auth, "PUT", &path, update.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&state, &auth, "PUT", &path, update).await.0,
        StatusCode::CONFLICT
    );
    let edit = call(&state, &auth, "GET", &format!("{path}/edit"), Value::Null)
        .await
        .1;
    assert_eq!(edit["entry_ids"], json!([b, a]));
    assert_eq!(edit["wordlist"]["revision"], 2);
    let note = json!({"expected_revision":2,"content":null,"note_updates":[{"entry_id":b,"expected_note_revision":1,"private_note":"only note"}]});
    let saved = call(&state, &auth, "PUT", &path, note).await;
    assert_eq!(saved.0, StatusCode::OK);
    assert_eq!(saved.1["revision"], 2);
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let items = call(&state, &auth, "GET", &format!("{path}/items"), Value::Null)
        .await
        .1;
    assert!(items["items"][1]["entry"].is_null());
    assert!(!items.to_string().contains("apple"));
    let catalog = call(
        &state,
        &auth,
        "GET",
        "/api/v1/wordlists/catalog?q=apple",
        Value::Null,
    )
    .await;
    assert_eq!(catalog.0, StatusCode::OK, "{}", catalog.1);
    assert_eq!(catalog.1["items"], json!([]));
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/wordlists",
            create_body(&[a])
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let legacy = uuid::Uuid::now_v7();
    let publisher = admin(&pool).await;
    sqlx::query("INSERT INTO lexicon.entry_publications(id,entry_id,publication_number,source_revision,content_schema_version,snapshot,snapshot_hash,published_by_admin_id) VALUES($1,$2,2,2,2,'{\"schema_version\":2}',$3,$4)").bind(legacy).bind(b).bind(legacy.as_bytes().to_vec()).bind(publisher).execute(&pool).await.unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id=$2 WHERE id=$1")
        .bind(b)
        .bind(legacy)
        .execute(&pool)
        .await
        .unwrap();
    let legacy_items = call(&state, &auth, "GET", &format!("{path}/items"), Value::Null).await;
    assert_eq!(legacy_items.0, StatusCode::OK);
    assert!(legacy_items.1["items"][0]["entry"].is_null());
}

#[sqlx::test]
async fn deletion_deadline_and_cascade_preserve_financial_history(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let a = entry(&pool, "expiry").await;
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a]),
    )
    .await;
    assert_eq!(created.0, StatusCode::OK, "{}", created.1);
    let request = apply(&state, &auth, "0").await;
    deadline(&pool, request.id, -1).await;
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            "/api/v1/me/wordlists",
            create_body(&[a])
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlists")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlist_items")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM account_deletion_requests")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn removed_and_readded_entry_cannot_reuse_stale_note_version(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let a = entry(&pool, "aba-a").await;
    let b = entry(&pool, "aba-b").await;
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a, b]),
    )
    .await;
    let id = created.1["id"].as_str().unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    let note = |revision, note_revision, text| json!({"expected_revision":revision,"content":null,"note_updates":[{"entry_id":a,"expected_note_revision":note_revision,"private_note":text}]});
    assert_eq!(
        call(&state, &auth, "PUT", &path, note(1, 1, "old")).await.0,
        StatusCode::OK
    );
    assert_eq!(call(&state,&auth,"PUT",&path,json!({"expected_revision":1,"content":{"name":"removed","entry_ids":[b]},"note_updates":[]})).await.0,StatusCode::OK);
    assert_eq!(call(&state,&auth,"PUT",&path,json!({"expected_revision":2,"content":{"name":"readded","entry_ids":[a,b]},"note_updates":[{"entry_id":a,"expected_note_revision":1,"private_note":"new"}]})).await.0,StatusCode::OK);
    assert_eq!(
        call(&state, &auth, "PUT", &path, note(1, 2, "stale overwrite"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let items = call(&state, &auth, "GET", &format!("{path}/items"), Value::Null)
        .await
        .1;
    assert_eq!(items["items"][0]["private_note"], "new");
}

#[sqlx::test]
async fn writer_waiting_on_list_rechecks_deletion_deadline(pool: PgPool) {
    let (state, auth) = setup_bound(&pool).await;
    let a = entry(&pool, "wait").await;
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a]),
    )
    .await;
    let id = created.1["id"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .unwrap();
    let request = apply(&state, &auth, "0").await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM wordlists WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let state2 = state.clone();
    let auth2 = tsz_rust::auth::extract::AuthUser {
        subject: auth.subject,
        role: auth.role.clone(),
        security_version: auth.security_version,
    };
    let writer = tokio::spawn(async move {
        call(&state2,&auth2,"PUT",&format!("/api/v1/me/wordlists/{id}"),json!({"expected_revision":1,"content":null,"note_updates":[{"entry_id":a,"expected_note_revision":1,"private_note":"too late"}]})).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5),async{loop{let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();if waiting{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await.unwrap();
    deadline(&pool, request.id, -1).await;
    blocker.commit().await.unwrap();
    assert_eq!(writer.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT private_note FROM wordlist_items WHERE wordlist_id=$1"
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "私密备注"
    );
}
