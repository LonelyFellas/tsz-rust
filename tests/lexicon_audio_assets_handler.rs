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
        CacheControl, MemoryAdapter, ObjectContentType, ObjectKey, ObjectStore, PutOptions,
        StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
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
            role: AdminRole::SuperAdmin,
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
        // 与 ops/audio-asset-lifecycle/README.md 一样配 Cache-Control：它是必填项，
        // 会被签进 PUT 请求，测试不配就看不到这个头，CORS 少放行它的问题也测不出来。
        StoragePolicy::new(
            StoragePrivacy::Private,
            MAX_BYTES,
            Duration::from_secs(60),
            Some(CacheControl::parse("private, max-age=86400").unwrap()),
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
        Method::POST,
        ASSETS_URL,
        Some(&token),
        Some(json!({
            "key": format!("uploads/{}.mp3", Uuid::now_v7()),
            "locale": "en-GB",
            "gender": "female",
            "original_name": "a.mp3"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["code"], "audio_storage_not_configured");

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
    assert!(key.starts_with(&format!("uploads/{admin_id}/")), "{key}");
    assert!(key.ends_with(".mp3"), "{key}");
    // 客户端必须原样回发签名 headers，否则 OSS 验签失败。断言**完整键集合**：
    // 签名头增减会直接改变 bucket CORS 的放行清单，逐键取值发现不了新增的那个。
    let mut header_names = upload["headers"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    header_names.sort();
    assert_eq!(
        header_names,
        ["cache-control", "content-length", "content-type"]
    );
    assert_eq!(upload["headers"]["content-type"], "audio/mpeg");
    assert_eq!(upload["headers"]["content-length"], "512");
    assert_eq!(upload["headers"]["cache-control"], "private, max-age=86400");
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
    let (status, body, _) = confirm(format!("uploads/{admin_id}/{}.mp3", Uuid::now_v7())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "audio_upload_not_completed");

    // 展示名的长度上限在应用层挡住，不能一路撞到数据库 CHECK 变成 500。
    let named_key = ObjectKey::parse(format!("uploads/{admin_id}/{}.mp3", Uuid::now_v7())).unwrap();
    store
        .put(
            &named_key,
            vec![7; 8],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    // 空串、超长、以及含 NUL / 换行的名字都必须是 400 + field，而不是撞到数据库变 500。
    for name in [
        String::new(),
        "   ".to_owned(),
        "n".repeat(121),
        "a\u{0}b".to_owned(),
        "a\nb".to_owned(),
    ] {
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
        assert_eq!(status, StatusCode::BAD_REQUEST, "name={name:?}");
        assert_eq!(body["code"], "invalid_request_body", "name={name:?}");
        assert_eq!(body["field"], "original_name", "name={name:?}");
    }

    // 0 字节对象同样按「没传完」处理，不落库。
    let empty_key = ObjectKey::parse(format!("uploads/{admin_id}/{}.mp3", Uuid::now_v7())).unwrap();
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

#[sqlx::test]
async fn confirm_is_idempotent_for_a_replayed_key(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let mut state = AppState::for_test(pool.clone());
    let store = configure_audio(&mut state);
    let token = token(&state, admin_id);

    let (_, ticket, _) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&token),
        Some(json!({"content_type": "audio/mpeg", "size": 4})),
    )
    .await;
    let pending_key = ObjectKey::parse(ticket["upload"]["key"].as_str().unwrap()).unwrap();
    store
        .put(
            &pending_key,
            vec![4; 4],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();

    let confirm = json!({
        "key": pending_key.as_str(),
        "locale": "en-GB",
        "gender": "female",
        "original_name": "slow.mp3"
    });
    let (first_status, first, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&token),
        Some(confirm.clone()),
    )
    .await;
    assert_eq!(first_status, StatusCode::CREATED);

    // 201 丢在网络上时前端会重试同一个 key。此时暂存对象已被删掉，若不按 source_key 回查，
    // 这里会报 audio_upload_not_completed，前端只能让用户重传，第一份资产就成了没人清的孤儿。
    let (second_status, second, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&token),
        Some(confirm),
    )
    .await;
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(second["asset"]["id"], first["asset"]["id"]);
    assert_eq!(second["asset"]["size_bytes"], 4);

    let rows: (i64,) = sqlx::query_as("SELECT count(*) FROM lexicon.audio_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows.0, 1, "重放不得登记出第二份资产");
}

async fn ordinary_with_audio_permissions(pool: &PgPool, keys: &[&str]) -> Uuid {
    let id = seed_admin(pool).await;
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) SELECT $1, unnest($2::text[]), $1")
        .bind(id).bind(keys).execute(pool).await.unwrap();
    id
}

#[sqlx::test]
async fn audio_write_anyof_and_read_anyof_preserve_private_creator_boundary(pool: PgPool) {
    let mut state = AppState::for_test(pool.clone());
    let store = configure_audio(&mut state);
    for action in [
        "words.create",
        "words.edit",
        "sentences.create",
        "sentences.edit",
    ] {
        let access = if action.starts_with("words.") {
            "words.access"
        } else {
            "sentences.access"
        };
        let owner = ordinary_with_audio_permissions(&pool, &[access, action]).await;
        let bearer = token(&state, owner);
        let asset = upload_asset(&state, &store, &bearer, vec![9; 8]).await;
        let path = format!(
            "{ASSETS_URL}/{}/url",
            asset["asset"]["id"].as_str().unwrap()
        );
        let (status, response, _) = call(&state, Method::GET, &path, Some(&bearer), None).await;
        assert_eq!(status, StatusCode::OK, "{action}: {response}");
        sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key=$2")
            .bind(owner)
            .bind(action)
            .execute(&pool)
            .await
            .unwrap();
        let (status, response, _) = call(
            &state,
            Method::POST,
            UPLOAD_URL,
            Some(&bearer),
            Some(json!({"content_type":"audio/mpeg","size":8})),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "仅access不得上传：{response}"
        );
        let private_reader = ordinary_with_audio_permissions(&pool, &[access]).await;
        let (status, response, _) = call(
            &state,
            Method::GET,
            &path,
            Some(&token(&state, private_reader)),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "读取动作不泄漏他人私有资产：{response}"
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioRevokePhase {
    Stat,
    Copy,
    ReadSignature,
    WriteSignature,
    WordReadSignature,
    RemoveWordReference,
}

struct RevokingAudioStore {
    inner: Arc<dyn ObjectStore>,
    pool: PgPool,
    admin_id: Uuid,
    phase: AudioRevokePhase,
    fired: std::sync::atomic::AtomicBool,
    stats: std::sync::atomic::AtomicUsize,
    copies: std::sync::atomic::AtomicUsize,
    deleted: std::sync::Mutex<Vec<String>>,
}

impl RevokingAudioStore {
    async fn revoke(&self, phase: AudioRevokePhase) {
        if self.phase != phase || self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let mut tx = self.pool.begin().await.unwrap();
        sqlx::query("UPDATE admins SET permission_version=permission_version+1 WHERE id=$1")
            .bind(self.admin_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM admin_permission_grants WHERE admin_id=$1 AND NOT($2::boolean AND permission_key='sentences.access')")
            .bind(self.admin_id)
            .bind(phase == AudioRevokePhase::WordReadSignature)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
}

#[async_trait::async_trait]
impl ObjectStore for RevokingAudioStore {
    fn space(&self) -> &StorageSpace {
        self.inner.space()
    }
    fn policy(&self) -> &StoragePolicy {
        self.inner.policy()
    }
    async fn put(
        &self,
        key: &ObjectKey,
        body: Vec<u8>,
        options: PutOptions,
    ) -> Result<
        tsz_rust::platform::storage::ObjectMetadata,
        tsz_rust::platform::storage::StorageError,
    > {
        self.inner.put(key, body, options).await
    }
    async fn read(
        &self,
        key: &ObjectKey,
    ) -> Result<Vec<u8>, tsz_rust::platform::storage::StorageError> {
        self.inner.read(key).await
    }
    async fn stat(
        &self,
        key: &ObjectKey,
    ) -> Result<
        tsz_rust::platform::storage::ObjectMetadata,
        tsz_rust::platform::storage::StorageError,
    > {
        self.stats.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let metadata = self.inner.stat(key).await?;
        self.revoke(AudioRevokePhase::Stat).await;
        Ok(metadata)
    }
    async fn copy(
        &self,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<
        tsz_rust::platform::storage::ObjectMetadata,
        tsz_rust::platform::storage::StorageError,
    > {
        self.copies
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let metadata = self.inner.copy(source, destination).await?;
        self.revoke(AudioRevokePhase::Copy).await;
        Ok(metadata)
    }
    async fn delete(
        &self,
        key: &ObjectKey,
    ) -> Result<(), tsz_rust::platform::storage::StorageError> {
        self.deleted.lock().unwrap().push(key.to_string());
        self.inner.delete(key).await
    }
    async fn presign_read(
        &self,
        key: &ObjectKey,
    ) -> Result<
        tsz_rust::platform::storage::PresignedRequest,
        tsz_rust::platform::storage::StorageError,
    > {
        let signed = self.inner.presign_read(key).await?;
        self.revoke(AudioRevokePhase::ReadSignature).await;
        self.revoke(AudioRevokePhase::WordReadSignature).await;
        if self.phase == AudioRevokePhase::RemoveWordReference
            && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let id = Uuid::parse_str(
                key.as_str()
                    .strip_prefix("assets/")
                    .unwrap()
                    .strip_suffix(".mp3")
                    .unwrap(),
            )
            .unwrap();
            sqlx::query("DELETE FROM lexicon.v3_audio_asset_references WHERE asset_id=$1")
                .bind(id)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        Ok(signed)
    }
    async fn presign_write(
        &self,
        key: &ObjectKey,
        length: u64,
        options: PutOptions,
    ) -> Result<
        tsz_rust::platform::storage::PresignedRequest,
        tsz_rust::platform::storage::StorageError,
    > {
        let signed = self.inner.presign_write(key, length, options).await?;
        self.revoke(AudioRevokePhase::WriteSignature).await;
        Ok(signed)
    }
}

#[sqlx::test]
async fn audio_detached_promotion_rechecks_before_copy_and_delivery_without_undoing_completion(
    pool: PgPool,
) {
    let mut state = AppState::for_test(pool.clone());
    let inner = configure_audio(&mut state);
    for phase in [AudioRevokePhase::Stat, AudioRevokePhase::Copy] {
        let admin_id =
            ordinary_with_audio_permissions(&pool, &["sentences.access", "sentences.create"]).await;
        let pending_key =
            ObjectKey::parse(format!("uploads/{admin_id}/{}.mp3", Uuid::now_v7())).unwrap();
        inner
            .put(
                &pending_key,
                vec![1; 8],
                PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
            )
            .await
            .unwrap();
        let store = Arc::new(RevokingAudioStore {
            inner: inner.clone(),
            pool: pool.clone(),
            admin_id,
            phase,
            fired: std::sync::atomic::AtomicBool::new(false),
            stats: std::sync::atomic::AtomicUsize::new(0),
            copies: std::sync::atomic::AtomicUsize::new(0),
            deleted: std::sync::Mutex::new(vec![]),
        });
        state.object_storage =
            StorageRegistry::from_stores([store.clone() as Arc<dyn ObjectStore>]).unwrap();
        let input = json!({"key":pending_key.as_str(),"locale":"en-GB","gender":"female","original_name":"revocation.mp3"});
        let bearer = token(&state, admin_id);
        let (status, response, _) = call(
            &state,
            Method::POST,
            ASSETS_URL,
            Some(&bearer),
            Some(input.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
        assert_eq!(response["code"], "forbidden");
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT object_key FROM lexicon.audio_assets WHERE created_by_admin_id=$1",
        )
        .bind(admin_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        if phase == AudioRevokePhase::Stat {
            assert_eq!(store.copies.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(keys.is_empty());
            assert!(store.deleted.lock().unwrap().is_empty());
            assert!(inner.stat(&pending_key).await.is_ok());
        } else {
            assert_eq!(store.copies.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(keys.len(), 1, "完成的登记副作用不得被撤权回滚");
            assert!(
                inner
                    .stat(&ObjectKey::parse(&keys[0]).unwrap())
                    .await
                    .is_ok()
            );
            assert_eq!(
                *store.deleted.lock().unwrap(),
                vec![pending_key.to_string()],
                "只能清理暂存对象"
            );
            let (status, response, _) =
                call(&state, Method::POST, ASSETS_URL, Some(&bearer), Some(input)).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "无权重放不得交付已登记资产：{response}"
            );
        }
    }
}

#[sqlx::test]
async fn audio_first_confirm_cannot_claim_another_admins_pending_upload(pool: PgPool) {
    let mut state = AppState::for_test(pool.clone());
    let inner = configure_audio(&mut state);
    let uploader = ordinary_with_audio_permissions(&pool, &["words.access", "words.create"]).await;
    let intruder = ordinary_with_audio_permissions(&pool, &["words.access", "words.create"]).await;
    let uploader_token = token(&state, uploader);
    let intruder_token = token(&state, intruder);
    let store = Arc::new(RevokingAudioStore {
        inner: inner.clone(),
        pool: pool.clone(),
        admin_id: uploader,
        phase: AudioRevokePhase::ReadSignature,
        fired: std::sync::atomic::AtomicBool::new(false),
        stats: std::sync::atomic::AtomicUsize::new(0),
        copies: std::sync::atomic::AtomicUsize::new(0),
        deleted: std::sync::Mutex::new(vec![]),
    });
    state.object_storage =
        StorageRegistry::from_stores([store.clone() as Arc<dyn ObjectStore>]).unwrap();
    let (status, ticket, _) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&uploader_token),
        Some(json!({"content_type":"audio/mpeg","size":3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ticket}");
    let pending = ObjectKey::parse(ticket["upload"]["key"].as_str().unwrap()).unwrap();
    inner
        .put(
            &pending,
            vec![1, 2, 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    let input = json!({"key":pending.as_str(),"locale":"en-GB","gender":"female","original_name":"owner.mp3"});
    let (status, rejected, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&intruder_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "别人先confirm不能抢占签发人的上传：{rejected}"
    );
    assert_eq!(rejected["code"], "forbidden");
    assert_eq!(
        store.stats.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "归属检查必须先于stat"
    );
    assert_eq!(store.copies.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(store.deleted.lock().unwrap().is_empty());
    let assets: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.audio_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(assets, 0, "拒绝必须先于copy和资产登记");
    assert_eq!(
        inner.read(&pending).await.unwrap(),
        vec![1, 2, 3],
        "原对象必须保留"
    );
    assert!(
        pending
            .as_str()
            .starts_with(&format!("uploads/{uploader}/")),
        "新key绑定签发人且保留固定临时prefix"
    );
    let (status, confirmed, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&uploader_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "A的合法确认仍应成功：{confirmed}"
    );
    assert_eq!(store.copies.load(std::sync::atomic::Ordering::SeqCst), 1);
    let asset_id = Uuid::parse_str(confirmed["asset"]["id"].as_str().unwrap()).unwrap();
    let creator: Uuid =
        sqlx::query_scalar("SELECT created_by_admin_id FROM lexicon.audio_assets WHERE id=$1")
            .bind(asset_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(creator, uploader);
    let (status, replay, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&uploader_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay, confirmed);
    assert_eq!(
        store.copies.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "重放不应再次copy"
    );
    let (status, denied, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&intruder_token),
        Some(input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "已确认资产仍以真实creator保护重放：{denied}"
    );
}

#[sqlx::test]
async fn audio_first_confirm_rejects_ownerless_legacy_pending_but_replays_confirmed_legacy(
    pool: PgPool,
) {
    let mut state = AppState::for_test(pool.clone());
    let inner = configure_audio(&mut state);
    let uploader = ordinary_with_audio_permissions(&pool, &["words.access", "words.create"]).await;
    let intruder = ordinary_with_audio_permissions(&pool, &["words.access", "words.create"]).await;
    let uploader_token = token(&state, uploader);
    let store = Arc::new(RevokingAudioStore {
        inner: inner.clone(),
        pool: pool.clone(),
        admin_id: uploader,
        phase: AudioRevokePhase::ReadSignature,
        fired: std::sync::atomic::AtomicBool::new(false),
        stats: std::sync::atomic::AtomicUsize::new(0),
        copies: std::sync::atomic::AtomicUsize::new(0),
        deleted: std::sync::Mutex::new(vec![]),
    });
    state.object_storage =
        StorageRegistry::from_stores([store.clone() as Arc<dyn ObjectStore>]).unwrap();
    let legacy = ObjectKey::parse(format!("uploads/{}.mp3", Uuid::now_v7())).unwrap();
    inner
        .put(
            &legacy,
            vec![4; 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    let input = json!({"key":legacy.as_str(),"locale":"en-GB","gender":"female","original_name":"legacy.mp3"});
    let (status, response, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&uploader_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "未知owner的旧待确认key必须拒绝：{response}"
    );
    assert_eq!(response["code"], "invalid_audio_key");
    assert!(
        response["detail"].as_str().unwrap().contains("new upload"),
        "应提示重新申请上传：{response}"
    );
    assert_eq!(store.stats.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(store.copies.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(store.deleted.lock().unwrap().is_empty());
    assert_eq!(inner.read(&legacy).await.unwrap(), vec![4; 3]);
    let assets: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.audio_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(assets, 0);
    // 已确认的旧key用数据库creator作归属证据，不能因为没有新namespace就破坏幂等重放。
    let id = Uuid::now_v7();
    let asset_key = ObjectKey::parse(format!("assets/{id}.mp3")).unwrap();
    inner
        .put(
            &asset_key,
            vec![4; 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO lexicon.audio_assets(id,object_key,source_key,content_type,size_bytes,locale,gender,original_name,created_by_admin_id) VALUES($1,$2,$3,'audio/mpeg',3,'en-GB','female','legacy.mp3',$4)")
        .bind(id).bind(asset_key.as_str()).bind(legacy.as_str()).bind(uploader).execute(&pool).await.unwrap();
    inner.delete(&legacy).await.unwrap();
    let (status, replay, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&uploader_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["asset"]["id"], id.to_string());
    assert_eq!(
        store.stats.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "旧已确认重放无须stat已清理的源"
    );
    assert_eq!(
        store.copies.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "旧已确认重放不触碰原对象"
    );
    assert!(store.deleted.lock().unwrap().is_empty());
    let (status, denied, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&token(&state, intruder)),
        Some(input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "旧已确认key仍不可由B重放：{denied}"
    );
}

#[sqlx::test]
async fn audio_first_confirm_and_replay_require_current_permissions(pool: PgPool) {
    let mut state = AppState::for_test(pool.clone());
    let inner = configure_audio(&mut state);
    let uploader = ordinary_with_audio_permissions(&pool, &["words.access", "words.create"]).await;
    let uploader_token = token(&state, uploader);
    let store = Arc::new(RevokingAudioStore {
        inner: inner.clone(),
        pool: pool.clone(),
        admin_id: uploader,
        phase: AudioRevokePhase::ReadSignature,
        fired: std::sync::atomic::AtomicBool::new(false),
        stats: std::sync::atomic::AtomicUsize::new(0),
        copies: std::sync::atomic::AtomicUsize::new(0),
        deleted: std::sync::Mutex::new(vec![]),
    });
    state.object_storage =
        StorageRegistry::from_stores([store.clone() as Arc<dyn ObjectStore>]).unwrap();
    let (status, ticket, _) = call(
        &state,
        Method::POST,
        UPLOAD_URL,
        Some(&uploader_token),
        Some(json!({"content_type":"audio/mpeg","size":3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ticket}");
    let pending = ObjectKey::parse(ticket["upload"]["key"].as_str().unwrap()).unwrap();
    inner
        .put(
            &pending,
            vec![1; 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    let input = json!({"key":pending.as_str(),"locale":"en-GB","gender":"female","original_name":"revoked.mp3"});
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.create'",
    )
    .bind(uploader)
    .execute(&pool)
    .await
    .unwrap();
    let (status, denied, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&uploader_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "签发后撤权不得首次confirm：{denied}"
    );
    assert_eq!(store.stats.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(store.copies.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(store.deleted.lock().unwrap().is_empty());
    assert_eq!(inner.read(&pending).await.unwrap(), vec![1; 3]);
    let assets: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.audio_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(assets, 0);
    sqlx::query("INSERT INTO admin_permission_grants(admin_id,permission_key,granted_by) VALUES($1,'words.create',$1)")
        .bind(uploader).execute(&pool).await.unwrap();
    let (status, confirmed, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&uploader_token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{confirmed}");
    sqlx::query(
        "DELETE FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='words.create'",
    )
    .bind(uploader)
    .execute(&pool)
    .await
    .unwrap();
    let (status, denied, _) = call(
        &state,
        Method::POST,
        ASSETS_URL,
        Some(&uploader_token),
        Some(input),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "已确认重放同样必须验证当前写权限：{denied}"
    );
    assert_eq!(store.copies.load(std::sync::atomic::Ordering::SeqCst), 1);
    let assets: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.audio_assets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(assets, 1, "撤权不回滚已完成登记");
}

#[sqlx::test]
async fn audio_word_reference_and_word_access_are_rechecked_before_url_delivery(pool: PgPool) {
    let mut state = AppState::for_test(pool.clone());
    let inner = configure_audio(&mut state);
    for phase in [
        AudioRevokePhase::WordReadSignature,
        AudioRevokePhase::RemoveWordReference,
    ] {
        let uploader = seed_admin(&pool).await;
        let uploader_token = token(&state, uploader);
        let asset = upload_asset(&state, &inner, &uploader_token, vec![7; 8]).await;
        let asset_id = Uuid::parse_str(asset["asset"]["id"].as_str().unwrap()).unwrap();
        let entry_id = Uuid::now_v7();
        sqlx::query("INSERT INTO lexicon.entries(id,content_schema_version,language,kind,revision,detection_snapshot,created_by_admin_id,updated_by_admin_id) VALUES($1,3,'en','word',1,'{}',$2,$2)")
            .bind(entry_id).bind(uploader).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO lexicon.v3_audio_asset_references(asset_id,entry_id,scope,variant_id) VALUES($1,$2,'draft',$3)")
            .bind(asset_id).bind(entry_id).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
        let reader =
            ordinary_with_audio_permissions(&pool, &["words.access", "sentences.access"]).await;
        let reader_token = token(&state, reader);
        let path = format!("{ASSETS_URL}/{asset_id}/url");
        let (status, response, _) =
            call(&state, Method::GET, &path, Some(&reader_token), None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "前置：词条权限和真实引用同时成立：{response}"
        );
        let store = Arc::new(RevokingAudioStore {
            inner: inner.clone(),
            pool: pool.clone(),
            admin_id: reader,
            phase,
            fired: std::sync::atomic::AtomicBool::new(false),
            stats: std::sync::atomic::AtomicUsize::new(0),
            copies: std::sync::atomic::AtomicUsize::new(0),
            deleted: std::sync::Mutex::new(vec![]),
        });
        state.object_storage =
            StorageRegistry::from_stores([store as Arc<dyn ObjectStore>]).unwrap();
        let (status, response, _) =
            call(&state, Method::GET, &path, Some(&reader_token), None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "签名期间撤销词条读取或真实引用，交付前必须复核具体条件：{response}"
        );
        assert_eq!(response["code"], "audio_asset_not_found");
        let sentences_retained: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admin_permission_grants WHERE admin_id=$1 AND permission_key='sentences.access')")
            .bind(reader).fetch_one(&pool).await.unwrap();
        assert!(sentences_retained, "基础AnyOf仍成立，不能仅重查AnyOf");
        let (status, response, _) =
            call(&state, Method::GET, &path, Some(&uploader_token), None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "引用改变不剥夺创建者本人读取：{response}"
        );
    }
}

#[sqlx::test]
async fn audio_signatures_recheck_before_delivery(pool: PgPool) {
    let mut state = AppState::for_test(pool.clone());
    let inner = configure_audio(&mut state);
    for phase in [
        AudioRevokePhase::WriteSignature,
        AudioRevokePhase::ReadSignature,
    ] {
        let admin_id =
            ordinary_with_audio_permissions(&pool, &["sentences.access", "sentences.create"]).await;
        let asset = upload_asset(&state, &inner, &token(&state, admin_id), vec![7; 8]).await;
        let store = Arc::new(RevokingAudioStore {
            inner: inner.clone(),
            pool: pool.clone(),
            admin_id,
            phase,
            fired: std::sync::atomic::AtomicBool::new(false),
            stats: std::sync::atomic::AtomicUsize::new(0),
            copies: std::sync::atomic::AtomicUsize::new(0),
            deleted: std::sync::Mutex::new(vec![]),
        });
        state.object_storage =
            StorageRegistry::from_stores([store as Arc<dyn ObjectStore>]).unwrap();
        let bearer = token(&state, admin_id);
        let (status, response, _) = if phase == AudioRevokePhase::WriteSignature {
            call(
                &state,
                Method::POST,
                UPLOAD_URL,
                Some(&bearer),
                Some(json!({"content_type":"audio/mpeg","size":8})),
            )
            .await
        } else {
            let path = format!(
                "{ASSETS_URL}/{}/url",
                asset["asset"]["id"].as_str().unwrap()
            );
            call(&state, Method::GET, &path, Some(&bearer), None).await
        };
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "签名生成期间撤权须在交付前阻断：{response}"
        );
        assert_eq!(response["code"], "forbidden");
    }
}
