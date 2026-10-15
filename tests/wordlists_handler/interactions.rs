use super::*;
use uuid::Uuid;

async fn interaction(
    state: &tsz_rust::state::AppState,
    user: &tsz_rust::auth::extract::AuthUser,
    id: &str,
    kind: &str,
    active: bool,
) -> (StatusCode, Value) {
    call(
        state,
        user,
        "PUT",
        &format!("/api/v1/me/wordlists/{id}/interactions"),
        json!({"kind":kind,"active":active}),
    )
    .await
}
#[sqlx::test]
async fn references_concurrent_intents_visibility_and_deletion(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, reader) = setup_bound(&pool).await;
    let (_, other) = setup_bound(&pool).await;
    let e = entry(&pool, "reference").await;
    let created = call(
        &state,
        &author,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[e]),
    )
    .await;
    assert_eq!(created.0, StatusCode::OK, "{}", created.1);
    let id = created.1["id"].as_str().unwrap();
    let uuid: Uuid = id.parse().unwrap();
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(uuid)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        interaction(&state, &author, id, "like", true).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        interaction(&state, &author, id, "favorite", true).await.0,
        StatusCode::FORBIDDEN
    );
    for kind in ["like", "favorite"] {
        let (a, b) = tokio::join!(
            interaction(&state, &reader, id, kind, true),
            interaction(&state, &reader, id, kind, true)
        );
        assert_eq!(a.0, StatusCode::OK, "{}", a.1);
        assert_eq!(a, b);
    }
    let stats_path = format!("/api/v1/wordlists/interactions?ids={id}");
    let stats = call(&state, &reader, "GET", &stats_path, Value::Null).await;
    assert_eq!(stats.0, StatusCode::OK, "{}", stats.1);
    assert_eq!(
        stats.1["items"][0],
        json!({"wordlist_id":id,"like_count":1,"favorite_count":1,"tip_amount":"0"})
    );
    let state_path = format!("/api/v1/me/wordlist-interactions?ids={id}");
    let other_state = call(&state, &other, "GET", &state_path, Value::Null).await;
    assert_eq!(other_state.1["items"][0]["liked"], false);
    assert_eq!(other_state.1["items"][0]["favorited"], false);
    let favorites = "/api/v1/me/wordlist-favorites";
    sqlx::query("UPDATE wordlists SET name='Updated source' WHERE id=$1")
        .bind(uuid)
        .execute(&pool)
        .await
        .unwrap();
    let saved = call(&state, &reader, "GET", favorites, Value::Null).await;
    assert_eq!(saved.1["items"][0]["wordlist"]["name"], "Updated source");
    assert_eq!(
        saved.1["items"][0]["wordlist"]["owner_user_id"],
        author.subject.to_string()
    );
    assert_eq!(
        call(&state, &other, "GET", favorites, Value::Null).await.1["pagination"]["total"],
        0
    );
    let search = call(
        &state,
        &reader,
        "GET",
        "/api/v1/me/wordlist-favorites?q=Updated&page_size=1",
        Value::Null,
    )
    .await;
    assert_eq!(search.1["pagination"]["total"], 1);
    sqlx::query("UPDATE wordlists SET state='withdrawn',name='PRIVATE SECRET' WHERE id=$1")
        .bind(uuid)
        .execute(&pool)
        .await
        .unwrap();
    let hidden = call(&state, &reader, "GET", favorites, Value::Null).await;
    assert_eq!(
        hidden.1["items"][0],
        json!({"wordlist_id":id,"wordlist":null})
    );
    assert!(!hidden.1.to_string().contains("PRIVATE"));
    assert_eq!(
        call(
            &state,
            &reader,
            "GET",
            "/api/v1/me/wordlist-favorites?q=PRIVATE",
            Value::Null
        )
        .await
        .1["pagination"]["total"],
        0
    );
    assert_eq!(
        call(&state, &reader, "GET", &stats_path, Value::Null)
            .await
            .1["items"],
        json!([])
    );
    assert_eq!(
        interaction(&state, &other, id, "like", true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        interaction(&state, &other, id, "favorite", true).await.0,
        StatusCode::NOT_FOUND
    );
    // Re-publication restores the same reference and both relationships.
    sqlx::query("UPDATE wordlists SET state='published',name='Public again' WHERE id=$1")
        .bind(uuid)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        call(&state, &reader, "GET", favorites, Value::Null).await.1["items"][0]["wordlist"]["name"],
        "Public again"
    );
    assert_eq!(
        call(&state, &reader, "GET", &stats_path, Value::Null)
            .await
            .1["items"][0]["like_count"],
        1
    );
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(author.subject)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        call(&state, &reader, "GET", favorites, Value::Null).await.1["items"][0]["wordlist"]
            .is_null()
    );
    assert_eq!(
        interaction(&state, &other, id, "favorite", true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        interaction(&state, &reader, id, "favorite", false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        interaction(&state, &reader, id, "favorite", false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&state, &reader, "GET", favorites, Value::Null).await.1["pagination"]["total"],
        0
    );
    // Administrative soft deletion also cleans remaining likes.
    let manager = admin(&pool).await;
    let deleted = admin_call(
        &state,
        manager,
        "POST",
        &format!("/api/v1/admin/wordlists/{id}/delete"),
        json!({"expected_revision":1}),
    )
    .await;
    assert_eq!(deleted.0, StatusCode::NO_CONTENT, "{}", deleted.1);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wordlist_interactions WHERE wordlist_id=$1")
            .bind(uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn platform_interactions_guest_stats_and_batch_bounds(pool: PgPool) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let (state, user) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO wordlists(id,owner_admin_id,name,state,create_key,create_hash) VALUES($1,$2,'Platform','published',$1,'\\x01')",
    )
    .bind(id)
    .bind(manager)
    .execute(&pool)
    .await
    .unwrap();
    for kind in ["like", "favorite"] {
        assert_eq!(
            interaction(&state, &user, &id.to_string(), kind, true)
                .await
                .0,
            StatusCode::OK
        );
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/v1/wordlists/interactions?ids={id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["items"][0]["favorite_count"], 1);
    let invalid = call(
        &state,
        &user,
        "GET",
        "/api/v1/wordlists/interactions?ids=nope",
        Value::Null,
    )
    .await;
    assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
    let ids = vec![id.to_string(); 101].join(",");
    assert_eq!(
        call(
            &state,
            &user,
            "GET",
            &format!("/api/v1/wordlists/interactions?ids={ids}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test]
async fn withdrawal_wins_against_waiting_addition(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, reader) = setup_bound(&pool).await;
    let e = entry(&pool, "race").await;
    let created = call(
        &state,
        &author,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[e]),
    )
    .await;
    let id: Uuid = created.1["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE wordlists SET state='withdrawn' WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let request = tokio::spawn(async move {
        interaction(&state, &reader, &id.to_string(), "favorite", true).await
    });
    // Prove the request is waiting on this exact transaction before committing withdrawal.
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(pid)
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(request.await.unwrap().0, StatusCode::NOT_FOUND);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wordlist_interactions WHERE wordlist_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}
