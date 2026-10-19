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

#[sqlx::test]
async fn likes_notify_only_the_author_and_read_is_owner_scoped(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, reader) = setup_bound(&pool).await;
    let e = entry(&pool, "notifications").await;
    let created = call(
        &state,
        &author,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[e]),
    )
    .await;
    let id = created.1["id"].as_str().unwrap();
    let uuid: Uuid = id.parse().unwrap();
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(uuid)
        .execute(&pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        interaction(&state, &reader, id, "like", true),
        interaction(&state, &reader, id, "like", true)
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(a, b);
    assert_eq!(
        interaction(&state, &reader, id, "favorite", true).await.0,
        StatusCode::OK
    );
    assert_eq!(
        interaction(&state, &reader, id, "like", false).await.0,
        StatusCode::OK
    );
    let list = call(
        &state,
        &author,
        "GET",
        "/api/v1/me/notifications",
        Value::Null,
    )
    .await;
    assert_eq!(list.0, StatusCode::OK, "{}", list.1);
    assert_eq!(list.1["total"], 1);
    assert_eq!(list.1["unread_count"], 1);
    let item = &list.1["items"][0];
    assert_eq!(item["kind"], "wordlist_liked");
    assert_eq!(item["wordlist_id"], id);
    assert_eq!(item["wordlist_name"], "我的词表");
    assert_eq!(item["actor_name"], "Deletion");
    assert!(item.get("amount").is_none());
    let read_path = format!(
        "/api/v1/me/notifications/{}/read",
        item["id"].as_str().unwrap()
    );
    assert_eq!(
        call(
            &state,
            &reader,
            "GET",
            "/api/v1/me/notifications",
            Value::Null
        )
        .await
        .1["total"],
        0
    );
    assert_eq!(
        call(&state, &reader, "PATCH", &read_path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let read = call(&state, &author, "PATCH", &read_path, Value::Null).await;
    assert_eq!(read.0, StatusCode::OK);
    assert_eq!(read.1["wordlist_name"], "我的词表");
    assert!(!read.1["read_at"].is_null());
    assert_eq!(
        call(&state, &author, "PATCH", &read_path, Value::Null).await,
        read
    );
    assert_eq!(
        call(
            &state,
            &author,
            "GET",
            "/api/v1/me/notifications",
            Value::Null
        )
        .await
        .1["unread_count"],
        0
    );
    assert_eq!(
        interaction(&state, &reader, id, "like", true).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            &author,
            "GET",
            "/api/v1/me/notifications",
            Value::Null
        )
        .await
        .1["unread_count"],
        1
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(reader.subject)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM wordlists WHERE id=$1")
        .bind(uuid)
        .execute(&pool)
        .await
        .unwrap();
    let list = call(
        &state,
        &author,
        "GET",
        "/api/v1/me/notifications",
        Value::Null,
    )
    .await;
    assert_eq!(list.1["total"], 2);
    for item in list.1["items"].as_array().unwrap() {
        assert!(item.get("wordlist_id").is_none());
        assert!(item.get("wordlist_name").is_none());
        assert!(item.get("actor_name").is_none());
    }
    let mut tx = pool.begin().await.unwrap();
    assert!(
        sqlx::raw_sql(include_str!(
            "../../migrations/20261015020000_wordlist_notifications.down.sql"
        ))
        .execute(&mut *tx)
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn notification_failure_rolls_back_a_new_like(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, reader) = setup_bound(&pool).await;
    let e = entry(&pool, "rollback notification").await;
    let created = call(
        &state,
        &author,
        "POST",
        "/api/v1/me/wordlists",
        create_body(&[e]),
    )
    .await;
    let id = created.1["id"].as_str().unwrap();
    sqlx::query("UPDATE wordlists SET state='published' WHERE id=$1")
        .bind(id.parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_notification() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'notification failure'; END $$; CREATE TRIGGER reject_notification BEFORE INSERT ON user_notifications FOR EACH ROW EXECUTE FUNCTION reject_notification();").execute(&pool).await.unwrap();
    assert_eq!(
        interaction(&state, &reader, id, "like", true).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wordlist_interactions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn management_statistics_include_private_lists_and_follow_removed_interactions(pool: PgPool) {
    let (state, author) = setup_bound(&pool).await;
    let (_, reader) = setup_bound(&pool).await;
    let (_, other) = setup_bound(&pool).await;
    let manager = admin(&pool).await;
    let e = entry(&pool, "statistics").await;
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
    for (user, kind) in [(&reader, "like"), (&other, "like"), (&reader, "favorite")] {
        let result = interaction(&state, user, id, kind, true).await;
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    }
    let path = "/api/v1/admin/wordlists/management?include_stats=true";
    for state_name in ["published", "withdrawn"] {
        sqlx::query("UPDATE wordlists SET state=$2 WHERE id=$1")
            .bind(uuid)
            .bind(state_name)
            .execute(&pool)
            .await
            .unwrap();
        let result = admin_call(&state, manager, "GET", path, Value::Null).await;
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
        let row = &result.1["items"][0];
        assert_eq!(row["wordlist"]["id"], id);
        assert_eq!(row["like_count"], 2);
        assert_eq!(row["favorite_count"], 1);
        assert_eq!(row["has_custom_words"], false);
    }
    for kind in ["like", "favorite"] {
        let result = interaction(&state, &reader, id, kind, false).await;
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    }
    let refreshed = admin_call(&state, manager, "GET", path, Value::Null).await;
    assert_eq!(refreshed.0, StatusCode::OK, "{}", refreshed.1);
    assert_eq!(refreshed.1["items"][0]["like_count"], 1);
    assert_eq!(refreshed.1["items"][0]["favorite_count"], 0);
}
