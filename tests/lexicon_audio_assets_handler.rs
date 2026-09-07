use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use tsz_rust::{
    admin::{AdminRepository, AdminRole, NewAdmin},
    platform::storage::{
        MemoryAdapter, ObjectContentType, ObjectKey, ObjectStore, PutOptions, StoragePolicy,
        StoragePrivacy, StorageRegistry, StorageSpace,
    },
    state::AppState,
};
use uuid::Uuid;

const MAX_BYTES: u64 = 1024;
const UPLOAD_URL: &str = "/api/v1/admin/lexicon/audio-assets/upload-url";
const ASSETS_URL: &str = "/api/v1/admin/lexicon/audio-assets";

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("audio-{}", id.simple()),
            display_name: "Audio Admin".to_owned(),
            password_hash: "hash".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    id
}

fn configure_audio(state: &mut AppState) -> Arc<dyn ObjectStore> {
    let store: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(
            StoragePrivacy::Private,
            MAX_BYTES,
            Duration::from_secs(60),
            None,
        )
        .unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([store.clone()]).unwrap();
    store
}

fn token(state: &AppState, admin_id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(admin_id, AdminRole::Admin.as_str())
        .unwrap()
}

async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&body).unwrap())
    } else {
        Body::empty()
    };
    let response = tsz_rust::router(state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap(),
        content_type,
    )
}

/// 走完「申请许可 → 客户端直传 → confirm」，返回 confirm 的响应体。
async fn upload_asset(
    state: &AppState,
    store: &Arc<dyn ObjectStore>,
    token: &str,
    body: Vec<u8>,
) -> Value {
    let (status, ticket, _) = call(
        state,
        Method::POST,
        UPLOAD_URL,
        Some(token),
        Some(json!({"content_type": "audio/mpeg", "size": body.len()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ticket}");
    let key = ObjectKey::parse(ticket["upload"]["key"].as_str().unwrap()).unwrap();
    store
        .put(
            &key,
            body,
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    let (status, asset, _) = call(
        state,
        Method::POST,
        ASSETS_URL,
        Some(token),
        Some(json!({
            "key": key.as_str(),
            "locale": "en-GB",
            "gender": "female",
            "original_name": "slow.mp3"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{asset}");
    asset
}

#[sqlx::test]
async fn audio_endpoints_require_admin_and_configured_storage(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let state = AppState::for_test(pool);

    let (status, body, content_type) = call(&state, Method::POST, UPLOAD_URL, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "invalid_token");
    assert_eq!(content_type, "application/problem+json");

    // 没有 audio 空间时三个端点都以 501 报「未开通」，前端据此把面板置灰。
    let token = token(&state, admin_id);
    let (status, body, content_type) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&token),
        Some(json!({"content_type": "audio/mpeg", "size": 10})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["code"], "audio_storage_not_configured");
    assert_eq!(content_type, "application/problem+json");

    let (status, body, _) = call(
        &state,
        Method::GET,
        &format!("{ASSETS_URL}/{}/url", Uuid::now_v7()),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["code"], "audio_storage_not_configured");
}

#[sqlx::test]
async fn upload_ticket_enforces_whitelist_and_size_then_signs_a_pending_key(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let mut state = AppState::for_test(pool);
    configure_audio(&mut state);
    let token = token(&state, admin_id);

    let (status, body, _) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&token),
        Some(json!({"content_type": "audio/flac", "size": 10})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "unsupported_audio_content_type");

    let (status, body, _) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&token),
        Some(json!({"content_type": "audio/mpeg", "size": MAX_BYTES + 1})),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["code"], "audio_file_too_large");

    let (status, body, _) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&token),
        Some(json!({"content_type": "audio/mpeg", "size": 512})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let upload = &body["upload"];
    let key = upload["key"].as_str().unwrap();
    assert!(key.starts_with("uploads/"), "{key}");
    assert!(key.ends_with(".mp3"), "{key}");
    // 客户端必须原样回发签名 headers，否则 OSS 验签失败。
    assert_eq!(upload["headers"]["content-type"], "audio/mpeg");
    assert_eq!(upload["headers"]["content-length"], "512");
    assert_eq!(upload["expires_in"], 60);
    assert_eq!(upload["max_bytes"], MAX_BYTES);
}

#[sqlx::test]
async fn confirm_promotes_the_object_and_registers_the_asset(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let mut state = AppState::for_test(pool);
    let store = configure_audio(&mut state);
    let token = token(&state, admin_id);

    let (status, ticket, _) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&token),
        Some(json!({"content_type": "audio/mpeg", "size": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pending_key = ObjectKey::parse(ticket["upload"]["key"].as_str().unwrap()).unwrap();
    store
        .put(
            &pending_key,
            vec![1, 2, 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();

    let (status, body, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&token),
        Some(json!({
            "key": pending_key.as_str(),
            "locale": "en-GB",
            "gender": "female",
            "original_name": "slow.mp3"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let asset = &body["asset"];
    assert_eq!(asset["locale"], "en-GB");
    assert_eq!(asset["gender"], "female");
    assert_eq!(asset["content_type"], "audio/mpeg");
    assert_eq!(asset["size_bytes"], 3);
    assert_eq!(asset["duration_ms"], Value::Null);
    assert_eq!(asset["original_name"], "slow.mp3");
    let asset_id = asset["id"].as_str().unwrap();

    // 已确认对象搬出暂存前缀，否则会被生命周期规则当孤儿删掉。
    let asset_key = ObjectKey::parse(format!("assets/{asset_id}.mp3")).unwrap();
    assert_eq!(store.stat(&asset_key).await.unwrap().content_length, 3);
    assert!(store.stat(&pending_key).await.is_err());

    let (status, body, _) = call(
        &state,
        Method::GET,
        &format!("{ASSETS_URL}/{asset_id}/url"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["url"].as_str().unwrap().contains(asset_id));
    assert_eq!(body["url_expires_in_seconds"], 60);
    assert!(body["expires_at"].as_str().is_some());
}

#[sqlx::test]
async fn confirm_rejects_foreign_keys_and_incomplete_uploads(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let mut state = AppState::for_test(pool);
    let store = configure_audio(&mut state);
    let token = token(&state, admin_id);

    let confirm = |key: String| {
        let state = state.clone();
        let token = token.clone();
        async move {
            call(
                &state,
                Method::POST,
                ASSETS_URL,
                Some(&token),
                Some(json!({
                    "key": key,
                    "locale": "en-US",
                    "gender": "male",
                    "original_name": "a.mp3"
                })),
            )
            .await
        }
    };

    // 只接受本服务签发过的暂存键形状：正式前缀、非 UUID 名、白名单外扩展名都拒。
    for key in [
        format!("assets/{}.mp3", Uuid::now_v7()),
        "uploads/not-a-uuid.mp3".to_owned(),
        format!("uploads/{}.txt", Uuid::now_v7()),
        format!("uploads/../{}.mp3", Uuid::now_v7()),
    ] {
        let (status, body, _) = confirm(key.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "key={key}");
        assert_eq!(body["code"], "invalid_audio_key", "key={key}");
    }

    // 形状合法但对象不存在 = 客户端还没 PUT 成功。
    let (status, body, _) = confirm(format!("uploads/{}.mp3", Uuid::now_v7())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "audio_upload_not_completed");

    // 展示名的长度上限在应用层挡住，不能一路撞到数据库 CHECK 变成 500。
    let named_key = ObjectKey::parse(format!("uploads/{}.mp3", Uuid::now_v7())).unwrap();
    store
        .put(
            &named_key,
            vec![7; 8],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    for name in [String::new(), "n".repeat(121)] {
        let (status, body, _) = call(
            &state,
            Method::POST,
            ASSETS_URL,
            Some(&token),
            Some(json!({
                "key": named_key.as_str(),
                "locale": "en-US",
                "gender": "male",
                "original_name": name
            })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "invalid_request_body");
    }

    // 0 字节对象同样按「没传完」处理，不落库。
    let empty_key = ObjectKey::parse(format!("uploads/{}.mp3", Uuid::now_v7())).unwrap();
    store
        .put(
            &empty_key,
            Vec::new(),
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    let (status, body, _) = confirm(empty_key.as_str().to_owned()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "audio_upload_not_completed");
}

#[sqlx::test]
async fn audio_asset_url_is_limited_to_the_creator(pool: PgPool) {
    let owner_id = seed_admin(&pool).await;
    let other_id = seed_admin(&pool).await;
    let mut state = AppState::for_test(pool);
    let store = configure_audio(&mut state);
    let owner_token = token(&state, owner_id);
    let other_token = token(&state, other_id);

    let asset = upload_asset(&state, &store, &owner_token, vec![9; 16]).await;
    let asset_id = asset["asset"]["id"].as_str().unwrap();

    // 资产尚未与词条建立引用关系，本期只有创建者可读；对他人一律按不存在处理。
    let (status, body, _) = call(
        &state,
        Method::GET,
        &format!("{ASSETS_URL}/{asset_id}/url"),
        Some(&other_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "audio_asset_not_found");

    let (status, body, _) = call(
        &state,
        Method::GET,
        &format!("{ASSETS_URL}/{}/url", Uuid::now_v7()),
        Some(&owner_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "audio_asset_not_found");
}
