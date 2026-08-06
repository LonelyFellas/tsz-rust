//! `GET /api/v1/admin/admins` 管理员列表端到端测试。

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::admin::{AdminRepository, AdminRole, NewAdmin};
use tsz_rust::state::AppState;

async fn seed_admin(
    pool: &PgPool,
    phone: &str,
    display_name: &str,
    role: AdminRole,
    must_change_password: bool,
    created_by_admin_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: phone.to_owned(),
            display_name: display_name.to_owned(),
            password_hash: "hashed-pw".to_owned(),
            role,
            must_change_password,
            created_by_admin_id,
        })
        .await
        .expect("seed admin 应成功");
    id
}

fn admin_token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .expect("签 admin token 应成功")
}

async fn get_admins(state: &AppState, bearer: Option<&str>, query: &str) -> (StatusCode, String) {
    let uri = if query.is_empty() {
        "/api/v1/admin/admins".to_owned()
    } else {
        format!("/api/v1/admin/admins?{query}")
    };
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }

    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[sqlx::test]
async fn super_admin_gets_safe_list_with_creator(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor_id = seed_admin(
        &pool,
        "13800000001",
        "系统超级管理员",
        AdminRole::SuperAdmin,
        false,
        None,
    )
    .await;
    let child_id = seed_admin(
        &pool,
        "13800000002",
        "运营管理员",
        AdminRole::Admin,
        true,
        Some(actor_id),
    )
    .await;
    let token = admin_token(&state, actor_id, AdminRole::SuperAdmin);

    let (status, body) = get_admins(&state, Some(&token), "").await;
    assert_eq!(status, StatusCode::OK, "super admin 应能查询：{body}");

    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["pagination"]["page"], 1);
    assert_eq!(json["pagination"]["page_size"], 20);
    assert_eq!(json["pagination"]["total"], 2);
    assert_eq!(json["pagination"]["total_pages"], 1);
    let items = json["items"].as_array().expect("items 应为数组");
    let child = items
        .iter()
        .find(|item| item["id"] == child_id.to_string())
        .expect("列表应包含创建的普通管理员");
    assert_eq!(child["phone"], "13800000002");
    assert_eq!(child["display_name"], "运营管理员");
    assert_eq!(child["role"], "admin");
    assert_eq!(child["status"], "active");
    assert_eq!(child["created_by"]["id"], actor_id.to_string());
    assert_eq!(child["created_by"]["display_name"], "系统超级管理员");

    for item in items {
        for forbidden in [
            "password_hash",
            "must_change_password",
            "failed_login_count",
            "locked_until",
        ] {
            assert!(
                item.get(forbidden).is_none(),
                "列表不得泄漏 {forbidden}：{body}"
            );
        }
    }
}

#[sqlx::test]
async fn filters_and_pagination_are_applied(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor_id = seed_admin(
        &pool,
        "13800000001",
        "Root",
        AdminRole::SuperAdmin,
        false,
        None,
    )
    .await;
    let first_id = seed_admin(
        &pool,
        "13800000002",
        "OpsAlpha",
        AdminRole::Admin,
        false,
        Some(actor_id),
    )
    .await;
    let second_id = seed_admin(
        &pool,
        "13800000003",
        "OpsBeta",
        AdminRole::Admin,
        false,
        Some(actor_id),
    )
    .await;
    let token = admin_token(&state, actor_id, AdminRole::SuperAdmin);

    let (status, body) = get_admins(&state, Some(&token), "role=admin&page=1&page_size=1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first_page: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(first_page["pagination"]["page"], 1);
    assert_eq!(first_page["pagination"]["page_size"], 1);
    assert_eq!(first_page["pagination"]["total"], 2);
    assert_eq!(first_page["pagination"]["total_pages"], 2);
    assert_eq!(first_page["items"].as_array().unwrap().len(), 1);

    let (_, body) = get_admins(&state, Some(&token), "role=admin&page=2&page_size=1").await;
    let second_page: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(second_page["pagination"]["page"], 2);
    let page_ids = [
        first_page["items"][0]["id"].as_str().unwrap(),
        second_page["items"][0]["id"].as_str().unwrap(),
    ];
    assert!(page_ids.contains(&first_id.to_string().as_str()));
    assert!(page_ids.contains(&second_id.to_string().as_str()));

    let (_, body) = get_admins(&state, Some(&token), "role=admin&page=3&page_size=1").await;
    let empty_page: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(empty_page["pagination"]["total"], 2);
    assert_eq!(empty_page["pagination"]["total_pages"], 2);
    assert!(empty_page["items"].as_array().unwrap().is_empty());

    let (_, body) = get_admins(&state, Some(&token), "display_name=opsalpha").await;
    let by_display_name: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(by_display_name["pagination"]["total"], 1);
    assert_eq!(by_display_name["items"][0]["id"], first_id.to_string());

    let (_, body) = get_admins(&state, Some(&token), "phone=00000003").await;
    let by_phone: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(by_phone["pagination"]["total"], 1);
    assert_eq!(by_phone["items"][0]["id"], second_id.to_string());

    let (_, body) = get_admins(
        &state,
        Some(&token),
        "role=admin&phone=00000002&display_name=opsalpha",
    )
    .await;
    let combined: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(combined["pagination"]["total"], 1);
    assert_eq!(combined["items"][0]["id"], first_id.to_string());

    let (_, body) = get_admins(&state, Some(&token), "phone=00000002&display_name=opsbeta").await;
    let mismatched: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(mismatched["pagination"]["total"], 0);
    assert!(mismatched["items"].as_array().unwrap().is_empty());
}

#[sqlx::test]
async fn like_metacharacters_are_searched_literally(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor_id = seed_admin(
        &pool,
        "13800000001",
        "Root",
        AdminRole::SuperAdmin,
        false,
        None,
    )
    .await;
    let target_id = seed_admin(
        &pool,
        "13800000002",
        r"literal%_marker\end",
        AdminRole::Admin,
        false,
        Some(actor_id),
    )
    .await;
    seed_admin(
        &pool,
        "13800000003",
        "ordinary marker",
        AdminRole::Admin,
        false,
        Some(actor_id),
    )
    .await;
    let token = admin_token(&state, actor_id, AdminRole::SuperAdmin);

    for encoded_query in ["%25", "_", "%5C"] {
        let (_, body) = get_admins(
            &state,
            Some(&token),
            &format!("display_name={encoded_query}"),
        )
        .await;
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["pagination"]["total"], 1, "元字符应按字面匹配：{body}");
        assert_eq!(json["items"][0]["id"], target_id.to_string());
    }
}

#[sqlx::test]
async fn invalid_pagination_returns_json_400(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor_id = seed_admin(
        &pool,
        "13800000001",
        "Root",
        AdminRole::SuperAdmin,
        false,
        None,
    )
    .await;
    let token = admin_token(&state, actor_id, AdminRole::SuperAdmin);

    for (query, expected) in [
        ("page=0", "page must be at least 1"),
        ("page_size=101", "page_size must be between 1 and 100"),
        ("role=owner", "invalid query parameters"),
    ] {
        let (status, body) = get_admins(&state, Some(&token), query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        let json: Value = serde_json::from_str(&body).expect("错误响应应为 JSON");
        assert!(
            json["detail"]
                .as_str()
                .is_some_and(|message| message.contains(expected)),
            "{query} 应返回对应错误：{body}"
        );
    }
}

#[sqlx::test]
async fn governance_guard_rejects_invalid_callers(pool: PgPool) {
    let state = AppState::for_test(pool.clone());

    let (status, _) = get_admins(&state, None, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let plain_id = seed_admin(&pool, "13800000001", "Plain", AdminRole::Admin, false, None).await;
    let plain_token = admin_token(&state, plain_id, AdminRole::Admin);
    let (status, body) = get_admins(&state, Some(&plain_token), "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["code"], "forbidden");
    assert_eq!(json["status"], 403);

    let forged_super_token = admin_token(&state, plain_id, AdminRole::SuperAdmin);
    let (status, body) = get_admins(&state, Some(&forged_super_token), "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["code"], "forbidden");
    assert_eq!(json["status"], 403);

    let must_change_id = seed_admin(
        &pool,
        "13800000002",
        "MustChange",
        AdminRole::SuperAdmin,
        true,
        None,
    )
    .await;
    let must_change_token = admin_token(&state, must_change_id, AdminRole::SuperAdmin);
    let (status, body) = get_admins(&state, Some(&must_change_token), "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["code"], "must_change_password");
    assert_eq!(json["status"], 403);

    let disabled_id = seed_admin(
        &pool,
        "13800000003",
        "Disabled",
        AdminRole::SuperAdmin,
        false,
        None,
    )
    .await;
    sqlx::query("UPDATE admins SET status = 'disabled' WHERE id = $1")
        .bind(disabled_id)
        .execute(&pool)
        .await
        .unwrap();
    let disabled_token = admin_token(&state, disabled_id, AdminRole::SuperAdmin);
    let (status, _) = get_admins(&state, Some(&disabled_token), "").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let web_token = state.token_manager.generate(plain_id, "student").unwrap();
    let (status, _) = get_admins(&state, Some(&web_token), "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
