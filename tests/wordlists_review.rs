mod account_deletion_support;
mod wordlists_support;
use account_deletion_support::{call, setup};
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use wordlists_support::*;
#[sqlx::test]
async fn review_snapshot_notes_publication_and_disabled_author_withdraw(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    let a = entry(&pool, "review").await;
    let reviewer = admin(&pool).await;
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a]),
    )
    .await;
    assert_eq!(created.0, StatusCode::OK, "{}", created.1);
    let id = created.1["id"].as_str().unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    let submit = json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()});
    let pending = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/review-requests"),
        submit.clone(),
    )
    .await;
    assert_eq!(pending.0, StatusCode::OK, "{}", pending.1);
    assert_eq!(
        pending,
        call(
            &state,
            &auth,
            "POST",
            &format!("{path}/review-requests"),
            submit
        )
        .await
    );
    let request = pending.1["id"].as_str().unwrap();
    let note = json!({"expected_revision":2,"content":null,"note_updates":[{"entry_id":a,"expected_note_revision":1,"private_note":"TOP SECRET"}]});
    let saved = call(&state, &auth, "PUT", &path, note).await;
    assert_eq!(saved.0, StatusCode::OK);
    assert_eq!(saved.1["revision"], 2);
    assert_eq!(saved.1["state"], "pending");
    let audit_items = admin_call(
        &state,
        reviewer,
        "GET",
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/items"),
        Value::Null,
    )
    .await;
    assert_eq!(audit_items.0, StatusCode::OK, "{}", audit_items.1);
    assert!(!audit_items.1.to_string().contains("private_note"));
    assert!(!audit_items.1.to_string().contains("TOP SECRET"));
    let published = admin_call(
        &state,
        reviewer,
        "POST",
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
        json!({"expected_revision":2,"approve":true,"reason":null}),
    )
    .await;
    assert_eq!(published.0, StatusCode::OK, "{}", published.1);
    assert_eq!(published.1["state"], "published");
    let public = call(
        &state,
        &auth,
        "GET",
        &format!("/api/v1/wordlists/{id}/items"),
        Value::Null,
    )
    .await;
    assert_eq!(public.0, StatusCode::OK, "{}", public.1);
    assert!(!public.1.to_string().contains("private_note"));
    let content = json!({"expected_revision":3,"content":{"name":"bypass","entry_ids":[a]},"note_updates":[]});
    assert_eq!(
        call(&state, &auth, "PUT", &path, content).await.0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
    let removed = admin_call(
        &state,
        reviewer,
        "POST",
        &format!("/api/v1/admin/wordlists/{id}/withdraw"),
        json!({"expected_revision":3,"reason":"平台下架"}),
    )
    .await;
    assert_eq!(removed.0, StatusCode::OK, "{}", removed.1);
    assert_eq!(removed.1["state"], "withdrawn");
    sqlx::query("UPDATE users SET status='active' WHERE id=$1")
        .bind(auth.subject)
        .execute(&pool)
        .await
        .unwrap();
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
    let own_reviews = call(
        &state,
        &auth,
        "GET",
        &format!("{path}/review-requests"),
        Value::Null,
    )
    .await;
    assert_eq!(own_reviews.1["withdraw_reason"], "平台下架");
    let audit: String = sqlx::query_scalar(
        "SELECT jsonb_agg(metadata)::text FROM audit.admin_actions WHERE resource_type='wordlist'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!audit.contains("TOP SECRET"));
}
#[sqlx::test]
async fn withdrawn_request_cannot_be_approved_and_audit_failure_rolls_back(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    let a = entry(&pool, "cancel").await;
    let reviewer = admin(&pool).await;
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a]),
    )
    .await;
    let id = created.1["id"].as_str().unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    let submit = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/review-requests"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = submit.1["id"].as_str().unwrap();
    assert_eq!(
        call(
            &state,
            &auth,
            "POST",
            &format!("{path}/withdraw"),
            json!({"expected_revision":2})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        admin_call(
            &state,
            reviewer,
            "POST",
            &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
            json!({"expected_revision":2,"approve":true,"reason":null})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let submit = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/review-requests"),
        json!({"expected_revision":3,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = submit.1["id"].as_str().unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_wordlist_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.resource_type='wordlist' THEN RAISE EXCEPTION 'test failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_wordlist_audit BEFORE INSERT ON audit.admin_actions FOR EACH ROW EXECUTE FUNCTION reject_wordlist_audit();").execute(&pool).await.unwrap();
    assert_eq!(
        admin_call(
            &state,
            reviewer,
            "POST",
            &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
            json!({"expected_revision":4,"approve":true,"reason":null})
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        call(&state, &auth, "GET", &path, Value::Null).await.1["state"],
        "pending"
    );
}

#[sqlx::test]
async fn private_drafts_are_not_discovered_and_live_publications_never_leak_archives(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    let a = entry(&pool, "first publication").await;
    let reviewer = admin(&pool).await;
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a]),
    )
    .await;
    let id = created.1["id"].as_str().unwrap();
    let path = format!("/api/v1/me/wordlists/{id}");
    let queue = admin_call(
        &state,
        reviewer,
        "GET",
        "/api/v1/admin/wordlists",
        Value::Null,
    )
    .await;
    assert_eq!(queue.0, StatusCode::OK, "{}", queue.1);
    assert_eq!(queue.1["items"], json!([]));
    let submit = call(
        &state,
        &auth,
        "POST",
        &format!("{path}/review-requests"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = submit.1["id"].as_str().unwrap();
    assert_eq!(
        admin_call(
            &state,
            reviewer,
            "POST",
            &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision"),
            json!({"expected_revision":2,"approve":true,"reason":null})
        )
        .await
        .0,
        StatusCode::OK
    );
    publish(&pool, a, reviewer, "updated publication", 2).await;
    let public = call(
        &state,
        &auth,
        "GET",
        &format!("/api/v1/wordlists/{id}/items"),
        Value::Null,
    )
    .await;
    assert_eq!(
        public.1["items"][0]["entry"]["label"],
        "updated publication"
    );
    sqlx::query("UPDATE lexicon.entries SET archived_at=clock_timestamp() WHERE id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    for route in [
        format!("/api/v1/wordlists/{id}/items"),
        format!("{path}/items"),
    ] {
        let data = call(&state, &auth, "GET", &route, Value::Null).await;
        assert_eq!(data.0, StatusCode::OK);
        assert!(data.1["items"][0]["entry"].is_null());
        assert!(!data.1.to_string().contains("updated publication"));
    }
    let audit = admin_call(
        &state,
        reviewer,
        "GET",
        &format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/items"),
        Value::Null,
    )
    .await;
    assert!(audit.1["items"][0]["entry"].is_null());
}

#[sqlx::test]
async fn revoked_permission_is_rechecked_after_waiting_for_owner(pool: PgPool) {
    let (state, auth) = setup(&pool).await;
    let a = entry(&pool, "permission wait").await;
    let reviewer = admin(&pool).await;
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(reviewer)
        .execute(&pool)
        .await
        .unwrap();
    for key in ["wordlists.access", "wordlists.review"] {
        sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,$2,$1)").bind(reviewer).bind(key).execute(&pool).await.unwrap();
    }
    let created = call(
        &state,
        &auth,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[a]),
    )
    .await;
    let id = created.1["id"].as_str().unwrap().to_owned();
    let pending = call(
        &state,
        &auth,
        "POST",
        &format!("/api/v1/me/wordlists/{id}/review-requests"),
        json!({"expected_revision":1,"idempotency_key":Uuid::now_v7()}),
    )
    .await;
    let request = pending.1["id"].as_str().unwrap().to_owned();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(auth.subject)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let state2 = state.clone();
    let path = format!("/api/v1/admin/wordlists/{id}/review-requests/{request}/decision");
    let action = tokio::spawn(async move {
        admin_call(
            &state2,
            reviewer,
            "POST",
            &path,
            json!({"expected_revision":2,"approve":true,"reason":null}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5),async{loop{let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();if waiting{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await.unwrap();
    let mut revoke = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM admins WHERE id=$1 FOR UPDATE")
        .bind(reviewer)
        .execute(&mut *revoke)
        .await
        .unwrap();
    sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='wordlists.review'").bind(reviewer).execute(&mut *revoke).await.unwrap();
    revoke.commit().await.unwrap();
    blocker.commit().await.unwrap();
    assert_eq!(action.await.unwrap().0, StatusCode::FORBIDDEN);
    assert_eq!(
        call(
            &state,
            &auth,
            "GET",
            &format!("/api/v1/me/wordlists/{id}"),
            Value::Null
        )
        .await
        .1["state"],
        "pending"
    );
}
