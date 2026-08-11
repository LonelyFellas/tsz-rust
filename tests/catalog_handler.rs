//! 词性配置九个端点的鉴权、wire、事务版本与乐观锁契约测试。

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    state::AppState,
};

const ROOT: &str = "/api/v1/admin/settings/parts-of-speech";

async fn seed_admin(pool: &PgPool, role: AdminRole, must_change_password: bool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("catalog-{}", id.simple()),
            display_name: match role {
                AdminRole::SuperAdmin => "目录超级管理员".to_owned(),
                AdminRole::Admin => "目录普通管理员".to_owned(),
            },
            password_hash: "hashed-password".to_owned(),
            role,
            must_change_password,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin 应成功");
    id
}

fn token(state: &AppState, id: Uuid, role: AdminRole) -> String {
    state
        .admin_token_manager
        .generate(id, role.as_str())
        .expect("签发测试 token 应成功")
}

async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "响应应为 JSON：{error}，body={}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, headers, json, bytes)
}

#[sqlx::test]
async fn catalog_read_allows_active_admin_but_management_requires_super_admin(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::Admin, false).await;
    let admin_token = token(&state, admin_id, AdminRole::Admin);

    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&admin_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "普通管理员应能读 catalog：{body}");
    assert_eq!(body["catalog_version"], 1);
    assert_eq!(body["items"].as_array().map(Vec::len), Some(11));
    assert_eq!(body["items"][0]["code"], "noun");
    assert_eq!(
        body["items"][0]["sub_parts"].as_array().map(Vec::len),
        Some(5)
    );

    let (status, _, body, _) = call(&state, Method::GET, ROOT, Some(&admin_token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");

    let super_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let super_token = token(&state, super_id, AdminRole::SuperAdmin);
    let (status, _, body, _) = call(&state, Method::GET, ROOT, Some(&super_token), None).await;
    assert_eq!(status, StatusCode::OK, "超级管理员应能读管理列表：{body}");
    assert_eq!(body["pagination"]["page_size"], 10);
    assert_eq!(body["pagination"]["total"], 11);
    let noun = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["code"] == "noun")
        .expect("第一页应包含 noun 种子");
    assert_eq!(noun["created_by"]["id"], "system");
    assert_eq!(noun["created_by"]["display_name"], "系统");
    assert!(noun.get("updated_by").is_none());

    let forced_id = seed_admin(&pool, AdminRole::SuperAdmin, true).await;
    let forced_token = token(&state, forced_id, AdminRole::SuperAdmin);
    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&forced_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "must_change_password");
}

#[sqlx::test]
async fn part_and_sub_part_lifecycle_is_transactional_and_revision_safe(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, admin_id, AdminRole::SuperAdmin);

    let (status, _, created, _) = call(
        &state,
        Method::POST,
        ROOT,
        Some(&bearer),
        Some(json!({
            "code": "particle",
            "name_zh": "  小品词  ",
            "name_en": "  Particle  ",
            "abbreviation": " part. ",
            "sort_order": -10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建基本词性失败：{created}");
    let part_id = created["id"].as_str().unwrap();
    assert_eq!(created["name_zh"], "小品词");
    assert_eq!(created["abbreviation"], "part.");
    assert_eq!(created["revision"], 1);
    assert_eq!(created["usage_count"], 0);
    assert_eq!(created["sub_part_count"], 0);
    assert_eq!(created["created_by"]["id"], admin_id.to_string());
    assert!(created.get("updated_by").is_none());

    let (status, _, body, _) = call(
        &state,
        Method::POST,
        ROOT,
        Some(&bearer),
        Some(json!({
            "code": "noun",
            "name_zh": "另一个中文名",
            "name_en": "Another English Name",
            "abbreviation": "another.",
            "sort_order": 999
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "part_of_speech_conflict");
    assert_eq!(body["field"], "code");

    let (status, _, body, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "code": "changed_code",
            "name_zh": "小品词",
            "name_en": "Particle",
            "abbreviation": "part.",
            "sort_order": 120
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_request_body");

    let (status, _, body, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 0,
            "name_zh": "小品词",
            "name_en": "Particle",
            "abbreviation": "part.",
            "sort_order": 120
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_part_of_speech");
    assert_eq!(body["field"], "base_revision");

    let (status, _, updated, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "新小品词",
            "name_en": "Particle updated",
            "abbreviation": "pt.",
            "sort_order": 120
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "更新基本词性失败：{updated}");
    assert_eq!(updated["code"], "particle");
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["updated_by"]["id"], admin_id.to_string());

    let (status, _, stale, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "过期修改",
            "name_en": "Stale update",
            "abbreviation": "stale.",
            "sort_order": 0
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["code"], "revision_conflict");
    assert_eq!(stale["field"], "base_revision");
    assert_eq!(stale["meta"]["current_revision"], 2);
    assert_eq!(stale["meta"]["part_of_speech_id"], part_id);
    assert_eq!(stale["meta"]["code"], "particle");

    let (status, _, sub, _) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/{part_id}/sub-parts"),
        Some(&bearer),
        Some(json!({
            "code": "PRT-FOCUS",
            "name_zh": "焦点小品词",
            "name_en": "Focus particle",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建细分词性失败：{sub}");
    let sub_id = sub["id"].as_str().unwrap();
    assert_eq!(sub["part_of_speech_id"], part_id);
    assert_eq!(sub["revision"], 1);
    assert!(sub.get("updated_by").is_none());

    let (status, _, sub_list, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/{part_id}/sub-parts"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sub_list["items"].as_array().map(Vec::len), Some(1));
    assert!(sub_list.get("pagination").is_none());

    let noun_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'noun'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _, body, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{noun_id}/sub-parts/{sub_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "错误父级",
            "name_en": "Wrong parent",
            "sort_order": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "sub_part_of_speech_not_found");

    let (status, _, updated_sub, _) = call(
        &state,
        Method::PATCH,
        &format!("{ROOT}/{part_id}/sub-parts/{sub_id}"),
        Some(&bearer),
        Some(json!({
            "base_revision": 1,
            "name_zh": "焦点助词",
            "name_en": "Focus marker",
            "sort_order": -20
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "更新细分词性失败：{updated_sub}");
    assert_eq!(updated_sub["revision"], 2);

    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}/sub-parts/{sub_id}?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["meta"]["current_revision"], 2);

    let (status, _, _, bytes) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}/sub-parts/{sub_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty(), "204 响应 body 必须为空");

    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_query");

    let (status, _, _, bytes) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{part_id}?base_revision=2"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty());

    let (status, _, catalog, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/catalog"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["catalog_version"], 7);
    assert!(
        catalog["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["code"] != "particle")
    );
}

#[sqlx::test]
async fn query_and_path_rejections_follow_problem_details_contract(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let admin_id = seed_admin(&pool, AdminRole::SuperAdmin, false).await;
    let bearer = token(&state, admin_id, AdminRole::SuperAdmin);

    for query in ["q=%25", "q=_"] {
        let (status, _, body, _) = call(
            &state,
            Method::GET,
            &format!("{ROOT}?{query}"),
            Some(&bearer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["pagination"]["total"], 0, "通配符必须按字面匹配");
    }

    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}?page=0"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_query");

    let missing_parent = Uuid::now_v7();
    let (status, _, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/{missing_parent}/sub-parts"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "part_of_speech_not_found");

    let (status, headers, body, _) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/not-a-uuid/sub-parts"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(body["code"], "invalid_path_parameter");
    assert_eq!(body["field"], "id");
    assert!(body.get("meta").is_none());

    let noun_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'noun'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _, body, _) = call(
        &state,
        Method::DELETE,
        &format!("{ROOT}/{noun_id}/sub-parts/not-a-uuid?base_revision=1"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_path_parameter");
    assert_eq!(body["field"], "sub_id");
}
