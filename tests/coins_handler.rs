mod coins_support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use coins_support::*;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    coins::{model::*, service},
    state::AppState,
};
use uuid::Uuid;
async fn get(state: &AppState, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(path);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = tsz_rust::router(state.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[sqlx::test]
async fn personal_wallets_are_realm_isolated_and_logical_zero_does_not_open(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let id = Uuid::now_v7();
    let user = seed(&pool, OwnerType::User, id).await;
    let admin = seed(&pool, OwnerType::Admin, id).await;
    let user_token = state.token_manager.generate(id, "student").unwrap();
    let admin_token = state.admin_token_manager.generate(id, "admin").unwrap();
    let paths = ["/api/v1/me/coins/wallet", "/api/v1/admin/me/coins/wallet"];
    for (path, token) in paths.iter().zip([&user_token, &admin_token]) {
        let (status, body) = get(&state, path, Some(token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["balance"], "0");
        assert_eq!(body["status"], "open");
        assert_eq!(get(&state, path, None).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM coin_wallets")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    credit(&pool, user, 9_007_199_254_740_993, "user").await;
    credit(&pool, admin, 7, "admin").await;
    for role in ["student", "teacher"] {
        let token = state.token_manager.generate(id, role).unwrap();
        let (status, body) = get(&state, paths[0], Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["balance"], "9007199254740993");
        assert_eq!(body["owner_type"], "user");
    }
    let (status, body) = get(&state, paths[1], Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["balance"], "7");
    assert_eq!(body["owner_type"], "admin");
    assert_eq!(
        get(&state, paths[0], Some(&admin_token)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&state, paths[1], Some(&user_token)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Query parameters cannot select a different wallet; no owner is accepted by entries.
    assert_eq!(
        get(
            &state,
            "/api/v1/me/coins/entries?owner_id=someone",
            Some(&user_token)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    reconciled(&pool).await;
}
#[sqlx::test]
async fn entries_are_private_stable_and_paginated_without_precision_loss(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let owner = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let token = state
        .token_manager
        .generate(owner.owner_id, "student")
        .unwrap();
    let empty = get(&state, "/api/v1/me/coins/entries", Some(&token)).await;
    assert_eq!(empty.0, StatusCode::OK);
    assert_eq!(empty.1["items"], serde_json::json!([]));
    assert_eq!(empty.1["pagination"]["total"], 0);
    for i in 0..3 {
        credit(&pool, owner, 10, &format!("entry-{i}")).await;
    }
    let (status, first) = get(&state, "/api/v1/me/coins/entries?page_size=2", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["pagination"]["total_pages"], 2);
    let snapshot = first["snapshot"].as_str().unwrap();
    credit(&pool, owner, 10, "late").await;
    let (_, second) = get(
        &state,
        &format!("/api/v1/me/coins/entries?page_size=2&page=2&snapshot={snapshot}"),
        Some(&token),
    )
    .await;
    assert_eq!(second["pagination"]["total"], 3);
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    for item in first["items"].as_array().unwrap() {
        assert_eq!(item["delta"], "10");
        for private in [
            "reason",
            "evidence_ref",
            "actor_id",
            "actor_type",
            "source_id",
            "wallet_id",
        ] {
            assert!(item.get(private).is_none(), "{private}");
        }
    }
    assert!(!first.to_string().contains("private"));
    for query in [
        "page=0",
        "page_size=0",
        "page_size=101",
        "snapshot=-1",
        "snapshot=01",
        "snapshot=9223372036854775808",
        "page=abc",
    ] {
        let (status, body) = get(
            &state,
            &format!("/api/v1/me/coins/entries?{query}"),
            Some(&token),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert_eq!(body["code"], "invalid_query");
    }
    let other = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let other_token = state
        .token_manager
        .generate(other.owner_id, "student")
        .unwrap();
    let (_, body) = get(
        &state,
        &format!("/api/v1/me/coins/entries?snapshot={snapshot}"),
        Some(&other_token),
    )
    .await;
    assert_eq!(body["items"], serde_json::json!([]));
    assert_eq!(balance(&pool, owner).await, "40");
    reconciled(&pool).await;
}
#[sqlx::test]
async fn disabled_and_deleted_sessions_fail_while_pending_wallet_is_readable(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let admin = seed(&pool, OwnerType::Admin, Uuid::now_v7()).await;
    let token = state
        .token_manager
        .generate(user.owner_id, "student")
        .unwrap();
    let admin_token = state
        .admin_token_manager
        .generate(admin.owner_id, "admin")
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    service::pause_in(&mut tx, user).await.unwrap();
    tx.commit().await.unwrap();
    let (status, body) = get(&state, "/api/v1/me/coins/wallet", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "deletion_pending");
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(user.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    for endpoint in ["wallet", "entries"] {
        assert_eq!(
            get(
                &state,
                &format!("/api/v1/me/coins/{endpoint}"),
                Some(&token)
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    sqlx::query("UPDATE admins SET must_change_password=true WHERE id=$1")
        .bind(admin.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&state, "/api/v1/admin/me/coins/wallet", Some(&admin_token))
            .await
            .1["code"],
        "must_change_password"
    );
    sqlx::query("UPDATE admins SET must_change_password=false,status='disabled' WHERE id=$1")
        .bind(admin.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&state, "/api/v1/admin/me/coins/entries", Some(&admin_token))
            .await
            .1["code"],
        "account_disabled"
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&state, "/api/v1/me/coins/wallet", Some(&token)).await.0,
        StatusCode::UNAUTHORIZED
    );
}
#[sqlx::test]
async fn query_failure_is_not_a_zero_wallet_and_no_public_writer_exists(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let user = seed(&pool, OwnerType::User, Uuid::now_v7()).await;
    let token = state
        .token_manager
        .generate(user.owner_id, "student")
        .unwrap();
    sqlx::query("ALTER TABLE coin_wallets RENAME TO coin_wallets_unavailable")
        .execute(&pool)
        .await
        .unwrap();
    let (status, body) = get(&state, "/api/v1/me/coins/wallet", Some(&token)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "internal_error");
    assert!(body.get("balance").is_none());
    for endpoint in ["wallet", "credit", "transfer"] {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/me/coins/{endpoint}"))
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = tsz_rust::router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ));
    }
}
