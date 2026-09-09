//! `GrammarVariantV3.audio_assets` 的草稿侧行为：活过 V2 往返、元数据以库为准、引用行随保存重建。

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
    config::SmartLexiconV3Flags,
    platform::{
        self,
        storage::{
            CacheControl, MemoryAdapter, ObjectContentType, ObjectKey, ObjectStore, PutOptions,
            StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
        },
    },
    state::AppState,
};
use uuid::Uuid;

const ROOT: &str = "/api/v1/admin/lexicon";

fn test_redis_url() -> String {
    std::env::var("TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned())
}

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("audio-ref-{}", id.simple()),
            display_name: "Audio reference tester".to_owned(),
            password_hash: "hashed-password".to_owned(),
            role: AdminRole::Admin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .expect("seed admin should succeed");
    id
}

fn bearer(state: &AppState, admin_id: Uuid) -> String {
    state
        .admin_token_manager
        .generate(admin_id, AdminRole::Admin.as_str())
        .expect("test token should be generated")
}

async fn call(
    state: &AppState,
    method: Method,
    uri: &str,
    bearer: &str,
    idempotency_key: Option<Uuid>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    if let Some(key) = idempotency_key {
        builder = builder.header("Idempotency-Key", key.to_string());
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn rich_text(text: &str) -> Value {
    json!({"version": 1, "text": text, "spans": [], "liaisons": []})
}

fn configure_audio(state: &mut AppState) -> Arc<dyn ObjectStore> {
    let store: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("audio").unwrap(),
        StoragePolicy::new(
            StoragePrivacy::Private,
            1024 * 1024,
            Duration::from_secs(60),
            Some(CacheControl::parse("private, max-age=86400").unwrap()),
        )
        .unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([store.clone()]).unwrap();
    store
}

/// 走完 PR1 的三步直传，返回登记好的资产。
async fn upload_asset(
    state: &AppState,
    store: &Arc<dyn ObjectStore>,
    bearer: &str,
    original_name: &str,
) -> Value {
    let (status, ticket) = call(
        state,
        Method::POST,
        &format!("{ROOT}/audio-assets/upload-url"),
        bearer,
        None,
        Some(json!({"content_type": "audio/mpeg", "size": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ticket}");
    let key = ObjectKey::parse(ticket["upload"]["key"].as_str().unwrap()).unwrap();
    store
        .put(
            &key,
            vec![1, 2, 3],
            PutOptions::new(Some(ObjectContentType::parse("audio/mpeg").unwrap())),
        )
        .await
        .unwrap();
    let (status, asset) = call(
        state,
        Method::POST,
        &format!("{ROOT}/audio-assets"),
        bearer,
        None,
        Some(json!({
            "key": key.as_str(),
            "locale": "en-GB",
            "gender": "female",
            "original_name": original_name
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{asset}");
    asset["asset"].clone()
}

async fn seed_dictionary_word(pool: &PgPool, word: &str) {
    let dataset_id: i64 = if let Some(dataset_id) =
        sqlx::query_scalar("SELECT id FROM dictionary.datasets WHERE status = 'active'")
            .fetch_optional(pool)
            .await
            .unwrap()
    {
        dataset_id
    } else {
        sqlx::query_scalar(
            r#"
            INSERT INTO dictionary.datasets (
                version, source_name, source_version, rules_version,
                terms_sha256, regions_sha256, status
            ) VALUES ($1, 'test', 'v1', 'v1', 'terms', 'regions', 'active')
            RETURNING id
            "#,
        )
        .bind(format!("audio-ref-{word}"))
        .fetch_one(pool)
        .await
        .unwrap()
    };
    sqlx::query(
        r#"
        INSERT INTO dictionary.terms (
            dataset_id, normalized_term, term, kind, pos, status,
            sense_count, filtered_cold_sense_count, region_family
        ) VALUES ($1, $2, $2, 'word', ARRAY['noun'], 'accepted', 1, 0, 'common_unmarked')
        "#,
    )
    .bind(dataset_id)
    .bind(word)
    .execute(pool)
    .await
    .unwrap();
}

fn v3_forms(surface: &str, pos_id: Uuid) -> Value {
    let form_id = Uuid::now_v7();
    json!({
        "pos": [{
            "pos_id": pos_id,
            "pos": "noun",
            "dialect_rules": {"spelling_mode": "unified", "phonetic_mode": "unified"},
            "forms": [{
                "id": form_id,
                "form_type": "base",
                "regional_variants": {
                    "mode": "common",
                    "common": {
                        "id": Uuid::now_v7(),
                        "dialect": "common",
                        "spelling": surface,
                        "origin": "manual",
                        "pronunciations": [{
                            "id": Uuid::now_v7(),
                            "dict_phonetic": "/test/",
                            "actual_pron": "test",
                            "style": "normal"
                        }]
                    }
                }
            }],
            "form_groups": [{
                "id": Uuid::now_v7(),
                "is_regular": true,
                "members": [{"id": Uuid::now_v7(), "form_id": form_id}]
            }]
        }]
    })
}

struct Entry {
    id: Uuid,
    pos_id: Uuid,
    grammar_id: Uuid,
    variant_id: Uuid,
    revision: i64,
}

async fn create_entry(state: &AppState, pool: &PgPool, bearer: &str, surface: &str) -> Entry {
    seed_dictionary_word(pool, surface).await;
    let (status, detection) = call(
        state,
        Method::POST,
        &format!("{ROOT}/detections"),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "language": "en",
            "kind": "word",
            "surface": surface
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detection}");
    let mut create_input = json!({
        "schema_version": 3,
        "detection_id": detection["detection_id"],
        "kind": "word"
    });
    if let Some(token) = detection["surface_match_page"]["surface_confirmation_token"].as_str() {
        create_input["confirmed_surface_match_token"] = json!(token);
    }
    let (status, created) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries"),
        bearer,
        Some(Uuid::now_v7()),
        Some(create_input),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let entry_id: Uuid = created["word"]["id"].as_str().unwrap().parse().unwrap();

    let pos_id = Uuid::now_v7();
    let forms = v3_forms(surface, pos_id);
    let (status, impact) = call(
        state,
        Method::POST,
        &format!("{ROOT}/entries/{entry_id}/steps/forms/impact"),
        bearer,
        None,
        Some(json!({"schema_version": 3, "base_revision": 1, "content": forms.clone()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    let mut input = json!({
        "schema_version": 3,
        "base_revision": 1,
        "intent": "complete",
        "content": forms
    });
    if let Some(token) = impact["confirmation_token"].as_str() {
        input["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = impact["surface_match_page"]["surface_confirmation_token"].as_str() {
        input["confirmed_surface_match_token"] = json!(token);
    }
    let (status, saved) = call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{entry_id}/steps/forms"),
        bearer,
        None,
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    Entry {
        id: entry_id,
        pos_id,
        grammar_id: Uuid::now_v7(),
        variant_id: Uuid::now_v7(),
        revision: saved["word"]["revision"].as_i64().unwrap(),
    }
}

/// 一条最小的词义草稿：只有一个语法结构变体，音频挂在它上面。
fn meanings_with_audio(entry: &Entry, audio_assets: Value) -> Value {
    json!({
        "sense_groups": [],
        "pos": [{
            "pos_id": entry.pos_id,
            "grammar_structures": [{
                "id": entry.grammar_id,
                "variants": [{
                    "id": entry.variant_id,
                    "dialect": "common",
                    "content": rich_text("used as a noun"),
                    "audio_assets": audio_assets
                }]
            }],
            "senses": []
        }]
    })
}

async fn save_meanings(
    state: &AppState,
    bearer: &str,
    entry: &Entry,
    meanings: Value,
) -> (StatusCode, Value) {
    call(
        state,
        Method::PUT,
        &format!("{ROOT}/entries/{}/steps/meanings", entry.id),
        bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": entry.revision,
            "intent": "save",
            "content": meanings
        })),
    )
    .await
}

async fn draft_reference_count(pool: &PgPool, entry_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_audio_asset_references WHERE entry_id = $1 AND scope = 'draft'",
    )
    .bind(entry_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn issue_codes(body: &Value) -> Vec<String> {
    body["field_issues"]
        .as_array()
        .or_else(|| body["issues"].as_array())
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| issue["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[sqlx::test]
async fn audio_assets_survive_the_v2_round_trip_and_carry_server_metadata(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let entry = create_entry(&state, &pool, &bearer, "audioword").await;
    let asset = upload_asset(&state, &store, &bearer, "slow.mp3").await;

    // 客户端故意回传被改过的展示元数据：服务端应当按 id 重新灌入库里的值。
    let mut tampered = asset.clone();
    tampered["original_name"] = json!("tampered.mp3");
    tampered["locale"] = json!("en-US");
    tampered["size_bytes"] = json!(999_999);

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([tampered])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let stored = &saved["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["audio_assets"]
        [0];
    assert_eq!(stored["id"], asset["id"], "字段必须活过 V2 往返");
    assert_eq!(stored["original_name"], "slow.mp3", "展示名以库为准");
    assert_eq!(stored["locale"], "en-GB", "归属以库为准");
    assert_eq!(stored["size_bytes"], asset["size_bytes"]);
    assert_eq!(stored["duration_ms"], Value::Null);

    // 重新读一次词条，确认落库而不只是响应体里对。
    let (status, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{}", entry.id),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(
        fetched["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["audio_assets"]
            [0]["id"],
        asset["id"]
    );
    assert_eq!(draft_reference_count(&pool, entry.id).await, 1);
}

#[sqlx::test]
async fn removing_audio_from_the_draft_drops_its_reference(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let mut entry = create_entry(&state, &pool, &bearer, "audiodrop").await;
    let asset = upload_asset(&state, &store, &bearer, "drop.mp3").await;

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([asset])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(draft_reference_count(&pool, entry.id).await, 1);

    entry.revision = saved["word"]["revision"].as_i64().unwrap();
    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        draft_reference_count(&pool, entry.id).await,
        0,
        "去掉引用后草稿引用行必须一起消失，否则回收永远不会发生"
    );
}

#[sqlx::test]
async fn invalid_audio_references_are_rejected_with_a_locatable_issue(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let entry = create_entry(&state, &pool, &bearer, "audiobad").await;
    let asset = upload_asset(&state, &store, &bearer, "ok.mp3").await;

    // 不存在的资产
    let mut missing = asset.clone();
    missing["id"] = json!(Uuid::now_v7());
    let (status, body) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([missing])),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        issue_codes(&body)
            .iter()
            .any(|code| code == "audio_asset_invalid"),
        "{body}"
    );
    // node_role 必须是 meanings。issue 的 node_location 留成 None 的话上 wire 会退化成 entry
    // ——那是整词条级错误的角色，前端按它分发就跳不到出问题的变体了。
    let issue = body["field_issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "audio_asset_invalid")
        .expect("audio issue");
    assert_eq!(issue["node_location"]["node_role"], "meanings", "{body}");
    assert_eq!(issue["node_id"], entry.variant_id.to_string(), "{body}");

    // 同一变体里重复引用同一条资产
    let (status, body) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([asset.clone(), asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        issue_codes(&body)
            .iter()
            .any(|code| code == "audio_asset_invalid"),
        "{body}"
    );

    assert_eq!(
        draft_reference_count(&pool, entry.id).await,
        0,
        "校验失败不得留下引用行"
    );
}

#[sqlx::test]
async fn audio_assets_must_not_be_shared_across_entries(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let first = create_entry(&state, &pool, &bearer, "audioone").await;
    let second = create_entry(&state, &pool, &bearer, "audiotwo").await;
    let asset = upload_asset(&state, &store, &bearer, "shared.mp3").await;

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &first,
        meanings_with_audio(&first, json!([asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // 跨词条引用必须拦住：回收按「还有没有人引用」判定，共享会让一次删除波及另一条词条。
    let (status, body) = save_meanings(
        &state,
        &bearer,
        &second,
        meanings_with_audio(&second, json!([asset])),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        issue_codes(&body)
            .iter()
            .any(|code| code == "audio_asset_invalid"),
        "{body}"
    );
}

/// 把资产的 created_at 拨到宽限期之前，让它进入回收候选。
async fn age_asset(pool: &PgPool, asset_id: &Value) {
    let id: Uuid = asset_id.as_str().unwrap().parse().unwrap();
    sqlx::query(
        "UPDATE lexicon.audio_assets SET created_at = now() - interval '30 days' WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

async fn asset_exists(pool: &PgPool, asset_id: &Value) -> bool {
    let id: Uuid = asset_id.as_str().unwrap().parse().unwrap();
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM lexicon.audio_assets WHERE id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn object_exists(store: &Arc<dyn ObjectStore>, pool: &PgPool, asset_id: &Value) -> bool {
    let id: Uuid = asset_id.as_str().unwrap().parse().unwrap();
    let key: Option<String> =
        sqlx::query_scalar("SELECT object_key FROM lexicon.audio_assets WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap();
    // 行没了就按对象键的固定形状还原，回收正确的话对象也应当已经不在。
    let key = key.unwrap_or_else(|| format!("assets/{id}.mp3"));
    store.stat(&ObjectKey::parse(key).unwrap()).await.is_ok()
}

/// 直接插一条发布行，避免为「历史发布仍在引用」这个断言拉起完整的发布流程。
async fn seed_publication(pool: &PgPool, entry_id: Uuid, admin_id: Uuid) -> Uuid {
    let publication_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision,
            content_schema_version, snapshot, snapshot_hash,
            published_by_admin_id, published_at
        ) VALUES ($1, $2, 1, 1, 3, '{}'::jsonb, $3, $4, now())
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(vec![7_u8; 32])
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    publication_id
}

#[sqlx::test]
async fn reclamation_only_touches_assets_nobody_references_any_more(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let entry = create_entry(&state, &pool, &bearer, "audioreclaim").await;

    let referenced = upload_asset(&state, &store, &bearer, "kept.mp3").await;
    let orphan = upload_asset(&state, &store, &bearer, "gone.mp3").await;
    let fresh_orphan = upload_asset(&state, &store, &bearer, "young.mp3").await;

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([referenced.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // 三条都拨老，只有「没人引用」的那条应当被回收；fresh_orphan 留在宽限期内。
    age_asset(&pool, &referenced["id"]).await;
    age_asset(&pool, &orphan["id"]).await;

    let reclaimed = tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &store)
        .await
        .unwrap();
    assert_eq!(reclaimed, 1, "只应回收那条零引用且过了宽限期的");
    assert!(asset_exists(&pool, &referenced["id"]).await, "草稿还在引用");
    assert!(object_exists(&store, &pool, &referenced["id"]).await);
    assert!(!asset_exists(&pool, &orphan["id"]).await);
    assert!(
        !object_exists(&store, &pool, &orphan["id"]).await,
        "行与对象要一起消失"
    );
    assert!(
        asset_exists(&pool, &fresh_orphan["id"]).await,
        "宽限期保护的是「已上传但还没保存进草稿」的那一段"
    );
}

#[sqlx::test]
async fn a_published_reference_keeps_audio_alive_after_it_leaves_the_draft(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let mut entry = create_entry(&state, &pool, &bearer, "audiopublished").await;
    let asset = upload_asset(&state, &store, &bearer, "published.mp3").await;

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    entry.revision = saved["word"]["revision"].as_i64().unwrap();

    // 模拟「这条录音已经随某次发布定格」。
    let publication_id = seed_publication(&pool, entry.id, admin_id).await;
    let asset_id: Uuid = asset["id"].as_str().unwrap().parse().unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_audio_asset_references
            (asset_id, entry_id, scope, publication_id, variant_id)
        VALUES ($1, $2, 'publication', $3, $4)
        "#,
    )
    .bind(asset_id)
    .bind(entry.id)
    .bind(publication_id)
    .bind(entry.variant_id)
    .execute(&pool)
    .await
    .unwrap();

    // 管理员把录音从草稿里去掉：草稿引用没了，但激活中的发布还指着它。
    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(draft_reference_count(&pool, entry.id).await, 0);
    age_asset(&pool, &asset["id"]).await;

    let reclaimed = tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &store)
        .await
        .unwrap();
    assert_eq!(reclaimed, 0);
    assert!(
        asset_exists(&pool, &asset["id"]).await,
        "历史发布还在引用就删掉，等于让已发布的词条播不出声，且不可恢复"
    );
    assert!(object_exists(&store, &pool, &asset["id"]).await);
}

/// 一条完整到可以发布的词义草稿，音频挂在语法结构变体上。
fn complete_meanings(entry: &Entry, audio_assets: Value) -> Value {
    let sense_group_id = Uuid::now_v7();
    let sense_id = Uuid::now_v7();
    json!({
        "sense_groups": [{
            "id": sense_group_id,
            "name_zh": "测试含义",
            "name_en": "test meaning"
        }],
        "pos": [{
            "pos_id": entry.pos_id,
            "grammar_structures": [{
                "id": entry.grammar_id,
                "variants": [{
                    "id": entry.variant_id,
                    "dialect": "common",
                    "content": rich_text("used as a noun"),
                    "audio_assets": audio_assets
                }]
            }],
            "senses": [{
                "id": sense_id,
                "sub_pos": "N-COUNT",
                "level": "A1",
                "sense_group_id": sense_group_id,
                "frequency": "50",
                "depends_on_context": false,
                "definitions": [{
                    "definition_mode": "zh_definition",
                    "id": Uuid::now_v7(),
                    "content_id": Uuid::now_v7(),
                    "level": "A1",
                    "grammar_structure_id": entry.grammar_id,
                    "content": rich_text("测试释义")
                }],
                "sentences": [],
                "relations": []
            }]
        }]
    })
}

#[sqlx::test]
async fn publishing_carries_audio_into_the_snapshot_and_records_its_reference(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let entry = create_entry(&state, &pool, &bearer, "audiopublish").await;
    let asset = upload_asset(&state, &store, &bearer, "published.mp3").await;

    let (status, saved) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{}/steps/meanings", entry.id),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": entry.revision,
            "intent": "complete",
            "content": complete_meanings(&entry, json!([asset.clone()]))
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{}/publications", entry.id),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"schema_version": 3, "base_revision": saved["word"]["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");

    // 快照必须自带音频：V3 发布存的是整个 AdminWordV3，激活历史发布时才播得出声。
    let asset_id: Uuid = asset["id"].as_str().unwrap().parse().unwrap();
    let snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE entry_id = $1")
            .bind(entry.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        snapshot["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["audio_assets"][0]
            ["id"]
            .as_str()
            .unwrap(),
        asset_id.to_string()
    );

    let publication_refs: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM lexicon.v3_audio_asset_references
           WHERE asset_id = $1 AND scope = 'publication'"#,
    )
    .bind(asset_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        publication_refs, 1,
        "发布必须留下引用行，否则回收会删掉已发布的录音"
    );
}

#[sqlx::test]
async fn entries_without_audio_keep_a_byte_identical_shape(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let entry = create_entry(&state, &pool, &bearer, "audioabsent").await;

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // 部署顺序的依据：没挂音频时这个键根本不出现，已部署的 admin（严格 runtime schema、
    // additionalProperties: false）不会因为未知字段整行拒收，后端因此可以先上。
    let variant = &saved["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0];
    assert!(
        variant.get("audio_assets").is_none(),
        "空数组不得上 wire：{variant}"
    );

    let (status, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{}", entry.id),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert!(
        fetched["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]
            .get("audio_assets")
            .is_none()
    );
}

/// 只在 delete 上注入失败的 store，用来覆盖回收里两条只在故障时才走到的分支。
struct FailingDeleteStore {
    inner: Arc<dyn ObjectStore>,
    delete_calls: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ObjectStore for FailingDeleteStore {
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
        self.inner.stat(key).await
    }
    async fn presign_read(
        &self,
        key: &ObjectKey,
    ) -> Result<
        tsz_rust::platform::storage::PresignedRequest,
        tsz_rust::platform::storage::StorageError,
    > {
        self.inner.presign_read(key).await
    }
    async fn presign_write(
        &self,
        key: &ObjectKey,
        content_length: u64,
        options: PutOptions,
    ) -> Result<
        tsz_rust::platform::storage::PresignedRequest,
        tsz_rust::platform::storage::StorageError,
    > {
        self.inner.presign_write(key, content_length, options).await
    }
    async fn copy(
        &self,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<
        tsz_rust::platform::storage::ObjectMetadata,
        tsz_rust::platform::storage::StorageError,
    > {
        self.inner.copy(source, destination).await
    }
    async fn delete(
        &self,
        key: &ObjectKey,
    ) -> Result<(), tsz_rust::platform::storage::StorageError> {
        self.delete_calls
            .lock()
            .unwrap()
            .push(key.as_str().to_owned());
        Err(
            tsz_rust::platform::storage::StorageError::SpaceNotConfigured(
                self.inner.space().clone(),
            ),
        )
    }
}

#[sqlx::test]
async fn forms_save_rebuilds_audio_references_when_a_pos_is_dropped(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let entry = create_entry(&state, &pool, &bearer, "audioformsdrop").await;
    let asset = upload_asset(&state, &store, &bearer, "dropped.mp3").await;

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(draft_reference_count(&pool, entry.id).await, 1);

    // 词形保存会按新的词性集合裁掉词义内容。把词性删空，挂在它下面的音频随之从草稿消失，
    // 引用行必须跟着没：否则资产既永远不会被回收，又被这条词条永久占用。
    let revision = saved["word"]["revision"].as_i64().unwrap();
    let (status, impact) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{}/steps/forms/impact", entry.id),
        &bearer,
        None,
        Some(json!({
            "schema_version": 3,
            "base_revision": revision,
            "content": {"pos": []}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{impact}");
    let mut input = json!({
        "schema_version": 3,
        "base_revision": revision,
        "intent": "save",
        "content": {"pos": []}
    });
    if let Some(token) = impact["confirmation_token"].as_str() {
        input["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = impact["surface_match_page"]["impact_confirmation_token"].as_str() {
        input["confirmed_impact_token"] = json!(token);
    }
    if let Some(token) = impact["surface_match_page"]["surface_confirmation_token"].as_str() {
        input["confirmed_surface_match_token"] = json!(token);
    }
    let (status, dropped) = call(
        &state,
        Method::PUT,
        &format!("{ROOT}/entries/{}/steps/forms", entry.id),
        &bearer,
        None,
        Some(input),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dropped}");
    assert_eq!(
        draft_reference_count(&pool, entry.id).await,
        0,
        "词形保存裁掉词性后，引用行必须跟着重建，否则资产永远回收不掉"
    );

    age_asset(&pool, &asset["id"]).await;
    let reclaimed = tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &store)
        .await
        .unwrap();
    assert_eq!(reclaimed, 1, "引用清干净后资产应当可以被回收");
}

#[sqlx::test]
async fn keeping_the_same_asset_across_saves_is_not_treated_as_cross_entry(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let mut entry = create_entry(&state, &pool, &bearer, "audiorepeat").await;
    let asset = upload_asset(&state, &store, &bearer, "kept.mp3").await;

    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    entry.revision = saved["word"]["revision"].as_i64().unwrap();

    // 「改词义但音频原样保留」是最普通的动作。此时上一轮自己的草稿引用行还在，
    // 跨词条判定必须靠 entry_id 把自己排除掉，否则第二次保存就会被自己顶成 422。
    let (status, again) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!([asset.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["word"]["meanings"]["pos"][0]["grammar_structures"][0]["variants"][0]["audio_assets"]
            [0]["id"],
        asset["id"]
    );
    assert_eq!(draft_reference_count(&pool, entry.id).await, 1);
}

#[sqlx::test]
async fn a_variant_may_reference_at_most_eight_assets(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    let mut entry = create_entry(&state, &pool, &bearer, "audiolimit").await;

    let mut assets = Vec::new();
    for index in 0..9 {
        assets.push(upload_asset(&state, &store, &bearer, &format!("a{index}.mp3")).await);
    }

    // 上限只有 service 层这一处生效：`#[schema(max_items = 8)]` 只进 OpenAPI，
    // 仓库没有按 schema 校验请求的中间件，删掉那段 if 不会有任何其它信号。
    let (status, body) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!(assets)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        issue_codes(&body)
            .iter()
            .any(|code| code == "audio_asset_invalid"),
        "{body}"
    );

    // 边界钉在 8，而不是「某个大数」。
    let (status, saved) = save_meanings(
        &state,
        &bearer,
        &entry,
        meanings_with_audio(&entry, json!(assets[..8])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    entry.revision = saved["word"]["revision"].as_i64().unwrap();
    assert_eq!(draft_reference_count(&pool, entry.id).await, 8);
}

#[sqlx::test]
async fn reclamation_yields_to_a_save_that_is_validating_the_same_asset(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    create_entry(&state, &pool, &bearer, "audiolocked").await;
    let asset = upload_asset(&state, &store, &bearer, "locked.mp3").await;
    age_asset(&pool, &asset["id"]).await;
    let asset_id: Uuid = asset["id"].as_str().unwrap().parse().unwrap();

    // 模拟「保存正在校验这条资产」的那一刻：校验路径取的正是 FOR SHARE。
    // 没有这对锁的话，worker 会在校验通过之后、引用行写入之前把对象删掉，
    // 留下一条引用完好却播不出声的资产。
    let mut holding = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.audio_assets WHERE id = $1 FOR SHARE")
        .bind(asset_id)
        .fetch_one(&mut *holding)
        .await
        .unwrap();

    // 加时限：去掉 SKIP LOCKED 之后这里会变成永久等锁，而挂起的测试在 CI 上只表现为超时，
    // 看不出是哪条不变量被破坏了。
    let reclaimed = tokio::time::timeout(
        Duration::from_secs(10),
        tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &store),
    )
    .await
    .expect("回收不得在资产行被锁住时干等——SKIP LOCKED 应当让它立刻让路")
    .unwrap();
    assert_eq!(reclaimed, 0, "资产行被保存路径锁住时，回收必须让路");
    assert!(asset_exists(&pool, &asset["id"]).await);
    assert!(
        object_exists(&store, &pool, &asset["id"]).await,
        "对象绝不能在这时被删"
    );

    holding.rollback().await.unwrap();

    // 锁释放之后照常回收，说明上面只是让路而不是漏判。
    let reclaimed = tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &store)
        .await
        .unwrap();
    assert_eq!(reclaimed, 1);
}

#[sqlx::test]
async fn a_failing_object_delete_keeps_the_row_and_does_not_burn_the_round(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let admin_id = seed_admin(&pool).await;
    let bearer = bearer(&state, admin_id);
    create_entry(&state, &pool, &bearer, "audiofaildelete").await;
    let asset = upload_asset(&state, &store, &bearer, "stuck.mp3").await;
    age_asset(&pool, &asset["id"]).await;

    let delete_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let failing: Arc<dyn ObjectStore> = Arc::new(FailingDeleteStore {
        inner: store.clone(),
        delete_calls: delete_calls.clone(),
    });
    let reclaimed = tsz_rust::lexicon::audio_assets::reclaim_once(&pool, &failing)
        .await
        .unwrap();

    assert_eq!(reclaimed, 0);
    assert!(
        asset_exists(&pool, &asset["id"]).await,
        "对象没删成就把行删掉，这个对象就再也没有任何记录指向它——底座没有 list，永远发现不了"
    );
    // 删失败的行会被 rollback 放回候选集；没有 skipped 集合的话，同一条会在这一轮里
    // 被 LIMIT 1 反复选中 100 次，把名额耗光、挡住其余资产。
    assert_eq!(
        delete_calls.lock().unwrap().len(),
        1,
        "同一轮里不得重复重试同一条"
    );
}

#[sqlx::test]
async fn playback_url_opens_up_once_the_asset_is_referenced_by_an_entry(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let owner_id = seed_admin(&pool).await;
    let other_id = seed_admin(&pool).await;
    let owner = bearer(&state, owner_id);
    let other = bearer(&state, other_id);
    let entry = create_entry(&state, &pool, &owner, "audioshared").await;

    let attached = upload_asset(&state, &store, &owner, "attached.mp3").await;
    let loose = upload_asset(&state, &store, &owner, "loose.mp3").await;

    // 还没挂进任何词条时，资产只是上传者的私有草稿。
    let (status, body) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/audio-assets/{}/url",
            attached["id"].as_str().unwrap()
        ),
        &other,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, saved) = save_meanings(
        &state,
        &owner,
        &entry,
        meanings_with_audio(&entry, json!([attached.clone()])),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    // 挂进词条之后，任何在职管理员都该能试听——词条本身对所有在职管理员可读，
    // 音频是词条内容的一部分，权限不该更窄。否则管理员 B 打开 A 录过音的词条，
    // 列表渲染正常但点播放一律 404。
    let (status, body) = call(
        &state,
        Method::GET,
        &format!(
            "{ROOT}/audio-assets/{}/url",
            attached["id"].as_str().unwrap()
        ),
        &other,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["url"].as_str().is_some_and(|url| !url.is_empty()),
        "{body}"
    );

    // 但放开的只是「被引用的那条」：没挂进任何词条的资产对他人仍然不可见。
    let (status, body) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/audio-assets/{}/url", loose["id"].as_str().unwrap()),
        &other,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "未被引用的资产不得对他人放开：{body}"
    );
}

#[sqlx::test]
async fn pronunciation_editor_survives_steps_and_publication(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let mut state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let store = configure_audio(&mut state);
    let bearer = bearer(&state, seed_admin(&pool).await);
    let mut entry = create_entry(&state, &pool, &bearer, "pronunciationeditor").await;
    let asset = upload_asset(&state, &store, &bearer, "pronunciation.mp3").await;
    let (_, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{}", entry.id),
        &bearer,
        None,
        None,
    )
    .await;
    let mut forms = fetched["word"]["forms"].clone();
    let row = &mut forms["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"][0];
    let text = row["dict_phonetic"].as_str().unwrap().to_owned();
    row["dict_phonetic_rich"] = json!({"version":2,"text":text,"annotations":[{"type":"highlight","start":0,"end":1,"color":"yellow"}]});
    row["voice_profile"] = json!({"voices":[{"voice_id":"en-GB-SoniaNeural","enabled":true,"rate_percent":-10},{"voice_id":"en-GB-RyanNeural","enabled":false,"rate_percent":25}]});
    let expected_profile = row["voice_profile"].clone();
    let mut tampered = asset.clone();
    tampered["original_name"] = json!("forged.mp3");
    row["audio_assets"] = json!([tampered]);
    let expected_rich = row["dict_phonetic_rich"].clone();
    let (status, saved) = call(&state, Method::PUT, &format!("{ROOT}/entries/{}/steps/forms",entry.id), &bearer, None, Some(json!({"schema_version":3,"base_revision":entry.revision,"intent":"complete","content":forms}))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let row = &saved["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"]
        [0];
    assert_eq!(row["audio_assets"][0]["original_name"], "pronunciation.mp3");
    assert_eq!(row["dict_phonetic_rich"], expected_rich);
    assert_eq!(draft_reference_count(&pool, entry.id).await, 1);
    entry.revision = saved["word"]["revision"].as_i64().unwrap();
    let (status, saved) = call(&state, Method::PUT, &format!("{ROOT}/entries/{}/steps/meanings", entry.id), &bearer, None, Some(json!({"schema_version":3,"base_revision":entry.revision,"intent":"complete","content":complete_meanings(&entry,json!([]))}))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        draft_reference_count(&pool, entry.id).await,
        1,
        "saving meanings must keep forms audio references"
    );
    let (status, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{}", entry.id),
        &bearer,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = &fetched["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"]
        [0];
    assert_eq!(row["dict_phonetic_rich"], expected_rich);
    assert_eq!(row["voice_profile"], expected_profile);
    let (status, published) = call(
        &state,
        Method::POST,
        &format!("{ROOT}/entries/{}/publications", entry.id),
        &bearer,
        Some(Uuid::now_v7()),
        Some(json!({"schema_version":3,"base_revision":saved["word"]["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{published}");
    let snapshot: Value =
        sqlx::query_scalar("SELECT snapshot FROM lexicon.entry_publications WHERE entry_id=$1")
            .bind(entry.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"][0]
            ["dict_phonetic_rich"],
        expected_rich
    );
    assert_eq!(
        snapshot["forms"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"][0]
            ["voice_profile"],
        expected_profile
    );
    let refs:i64=sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_audio_asset_references WHERE entry_id=$1 AND scope='publication'").bind(entry.id).fetch_one(&pool).await.unwrap();
    assert_eq!(refs, 1);
}

#[sqlx::test]
async fn pronunciation_editor_rejects_invalid_annotations_and_profiles(pool: PgPool) {
    let redis = platform::connect_redis(&test_redis_url()).await.unwrap();
    let state = AppState::for_test_with_redis(pool.clone(), redis)
        .with_smart_lexicon_v3_flags_for_test(SmartLexiconV3Flags::all_enabled());
    let bearer = bearer(&state, seed_admin(&pool).await);
    let entry = create_entry(&state, &pool, &bearer, "pronunciationinvalid").await;
    let (_, fetched) = call(
        &state,
        Method::GET,
        &format!("{ROOT}/entries/{}", entry.id),
        &bearer,
        None,
        None,
    )
    .await;
    for (text, rich_text, annotations, profile, code) in [
        (
            "😀a",
            "😀a",
            json!([{"type":"highlight","start":0,"end":3,"color":"yellow"}]),
            Value::Null,
            "phonetic_rich_text_invalid",
        ),
        (
            "abc",
            "other",
            json!([]),
            Value::Null,
            "phonetic_rich_text_invalid",
        ),
        (
            "abc",
            "abc",
            json!([]),
            json!({"voices":[{"voice_id":"sonia","enabled":false,"rate_percent":101}]}),
            "voice_profile_invalid",
        ),
    ] {
        let mut forms = fetched["word"]["forms"].clone();
        let row =
            &mut forms["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"][0];
        row["dict_phonetic"] = json!(text);
        row["dict_phonetic_rich"] = json!({"version":2,"text":rich_text,"annotations":annotations});
        if !profile.is_null() {
            row["voice_profile"] = profile;
        }
        let (status, problem)=call(&state,Method::PUT,&format!("{ROOT}/entries/{}/steps/forms",entry.id),&bearer,None,Some(json!({"schema_version":3,"base_revision":entry.revision,"intent":"save","content":forms}))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        let issue = problem["field_issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|issue| issue["code"] == code)
            .expect("pronunciation validation issue");
        assert_eq!(issue["step"], "forms", "{problem}");
        assert_eq!(issue["field"], "dict_phonetic", "{problem}");
        assert_eq!(
            issue["node_location"]["node_role"], "forms.pronunciation",
            "{problem}"
        );
        assert_eq!(
            issue["node_id"],
            fetched["word"]["forms"]["pos"][0]["forms"][0]["regional_variants"]["common"]["pronunciations"]
                [0]["id"]
        );
    }
}
