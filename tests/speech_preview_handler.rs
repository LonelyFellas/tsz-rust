use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
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
        MemoryAdapter, ObjectStore, StoragePolicy, StoragePrivacy, StorageRegistry, StorageSpace,
    },
    speech::{SpeechError, SpeechErrorKind, SpeechProvider, SynthesisRequest, SynthesizedAudio},
    state::AppState,
};
use uuid::Uuid;

#[sqlx::test]
async fn single_group_routes_protect_legacy_writes_and_project_unselected_candidates(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool).await;
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let token = state
        .admin_token_manager
        .generate(actor, "super_admin")
        .unwrap();
    let id = Uuid::now_v7();
    let input = json!({"base_revision":0,"group":{"id":id,"name":"HTTP group","areas":["word"],"uk":[{"id":Uuid::now_v7(),"name":"unselected","voice_alias":"en-gb-sonianeural","selected":false,"parameters":{"rate_percent":0,"pitch_semitones":0,"style":null}}],"us":[]}});
    let created = call(
        &state,
        Method::POST,
        "/api/v1/admin/speech/groups",
        Some(&token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(created.0, StatusCode::OK);
    let group = created.1["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["id"] == json!(id))
        .unwrap()
        .clone();
    assert_eq!(group["revision"], 1);
    let old = call(
        &state,
        Method::GET,
        "/api/v1/admin/speech/groups",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(old.1["groups"].as_array().unwrap().len(), 1);
    assert_eq!(old.1["groups"][0]["uk"], json!([]));
    assert!(old.1["groups"][0].get("revision").is_none());
    let new = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/speech/groups")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("X-TSZ-Dubbing-Groups", "v2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let new: Value =
        serde_json::from_slice(&new.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(new["groups"].as_array().unwrap().len(), 2);
    assert_eq!(new["groups"][1]["uk"][0]["selected"], false);
    let legacy = call(
        &state,
        Method::PUT,
        "/api/v1/admin/speech/groups",
        Some(&token),
        Some(json!({"groups":[],"base_revision":1})),
    )
    .await;
    assert_eq!(legacy.0, StatusCode::CONFLICT);
    let path = format!("/api/v1/admin/speech/groups/{id}");
    let stale = call(
        &state,
        Method::PUT,
        &path,
        Some(&token),
        Some(json!({"group":group,"base_revision":0})),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    let removed = call(
        &state,
        Method::DELETE,
        &path,
        Some(&token),
        Some(json!({"base_revision":1})),
    )
    .await;
    assert_eq!(removed.0, StatusCode::OK);
    assert_eq!(removed.1["groups"].as_array().unwrap().len(), 1);
    let global_id = removed.1["groups"][0]["id"].as_str().unwrap();
    let global = call(
        &state,
        Method::DELETE,
        &format!("/api/v1/admin/speech/groups/{global_id}"),
        Some(&token),
        Some(json!({"base_revision":0})),
    )
    .await;
    assert_eq!(global.0, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn dubbing_resolve_uses_real_routes_without_generating_and_respects_permissions(
    pool: PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool).await;
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let token = state
        .admin_token_manager
        .generate(actor, "super_admin")
        .unwrap();
    let input = json!({"profile":{"voices":[],"dubbing":{"mode":"inherit","area":"word"}},"accent":"gb","source":{"kind":"text","content":{"version":2,"text":"123","annotations":[{"type":"say_as","start":0,"end":3,"interpret_as":"digits"}]}}});
    let resolved = call(
        &state,
        Method::POST,
        "/api/v1/admin/speech/resolve",
        Some(&token),
        Some(input.clone()),
    )
    .await;
    assert_eq!(resolved.0, StatusCode::OK);
    assert!(!resolved.1["voices"].as_array().unwrap().is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lexicon.audio_generations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let phoneme = json!({"profile":{"voices":[],"dubbing":{"mode":"inherit","area":"azure"}},"accent":"gb","source":{"kind":"phoneme","phoneme_locale":"en-GB","alphabet":"ipa","phoneme":"kæt","content":{"version":2,"text":"cat","annotations":[{"type":"sub","start":0,"end":3,"alias":"kitten"}]}}});
    let replaced = call(
        &state,
        Method::POST,
        "/api/v1/admin/speech/resolve",
        Some(&token),
        Some(phoneme),
    )
    .await;
    assert_eq!(replaced.0, StatusCode::OK, "{:?}", replaced.1);
    assert!(
        replaced.1["voices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|voice| voice["locale"] == "en-GB")
    );
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let denied = call(
        &state,
        Method::POST,
        "/api/v1/admin/speech/resolve",
        Some(&token),
        Some(input),
    )
    .await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn current_accent_and_restore_routes_require_shared_permission_and_revision(pool: PgPool) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool).await;
    let repository = tsz_rust::speech::preview::PreviewRepository::new(pool.clone());
    repository.ensure_catalog_voices().await.unwrap();
    let token = state
        .admin_token_manager
        .generate(actor, "super_admin")
        .unwrap();
    let path = "/api/v1/admin/speech/library/en-sg-lunaneural/accent";
    let saved = call(
        &state,
        Method::PUT,
        path,
        Some(&token),
        Some(json!({"accent":"gb","base_revision":0})),
    )
    .await;
    assert_eq!(saved.0, StatusCode::OK);
    assert_eq!(saved.1["accent"], "gb");
    let stale = call(
        &state,
        Method::PUT,
        path,
        Some(&token),
        Some(json!({"accent":"us","base_revision":0})),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    let invalid = call(
        &state,
        Method::PUT,
        path,
        Some(&token),
        Some(json!({"accent":"both","base_revision":1})),
    )
    .await;
    assert_eq!(invalid.0, StatusCode::UNPROCESSABLE_ENTITY);
    let restored = call(
        &state,
        Method::POST,
        "/api/v1/admin/speech/library/en-sg-lunaneural/description/restore",
        Some(&token),
        Some(json!({"base_revision":0})),
    )
    .await;
    assert_eq!(restored.0, StatusCode::OK);
    assert_eq!(restored.1["description_zh"], Value::Null);
    assert_eq!(restored.1["description_revision"], 1);
    let new = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/speech/voices")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("X-TSZ-Voice-Models", "v1")
                .header("X-TSZ-Voice-Catalog", "v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&new.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        body["items"].as_array().unwrap().len(),
        tsz_rust::speech::capabilities::snapshot().items.len()
    );
    let luna = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["alias"] == "en-sg-lunaneural")
        .unwrap();
    assert_eq!(luna["accent"], "gb");
    assert_eq!(luna["locale"], "en-SG");
    assert_eq!(
        luna["capability_evidence"]["provider_voice_id"],
        "en-SG-LunaNeural"
    );
    sqlx::query("UPDATE admins SET role='admin' WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let denied = call(
        &state,
        Method::PUT,
        path,
        Some(&token),
        Some(json!({"accent":"none","base_revision":1})),
    )
    .await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn model_capability_negotiation_hides_flash_from_old_catalog_and_rejects_legacy_preview(
    pool: PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let actor = seed_admin(&pool).await;
    tsz_rust::speech::preview::PreviewRepository::new(pool.clone())
        .ensure_catalog_voices()
        .await
        .unwrap();
    let token = state
        .admin_token_manager
        .generate(actor, "super_admin")
        .unwrap();
    let old = call(
        &state,
        Method::GET,
        "/api/v1/admin/speech/voices",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(old.0, StatusCode::OK);
    assert_eq!(old.1["items"].as_array().unwrap().len(), 123);
    let response = tsz_rust::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/speech/voices")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("X-TSZ-Voice-Models", "v1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 126);
    let flash = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|voice| voice["metadata"]["model"] == "hd_flash")
        .unwrap();
    let rejected=call(&state,Method::POST,"/api/v1/admin/speech/previews",Some(&token),Some(json!({"voice_alias":flash["alias"],"content":{"version":2,"text":"hello","annotations":[]}}))).await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST);
    assert_eq!(rejected.1["code"], "invalid_speech_preview");
}

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("speech-{}", id.simple()),
            display_name: "Speech Admin".to_owned(),
            password_hash: "hash".to_owned(),
            role: AdminRole::SuperAdmin,
            must_change_password: false,
            created_by_admin_id: None,
        })
        .await
        .unwrap();
    id
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

fn valid_request() -> Value {
    json!({
        "content": {"version": 2, "text": "hello", "annotations": []},
        "voice_alias": "en-us-jenny",
        "rate_percent": 0,
        "pitch_semitones": 0
    })
}

struct FailingProvider(SpeechErrorKind);

#[async_trait]
impl SpeechProvider for FailingProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }

    async fn synthesize(
        &self,
        _request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        Err(SpeechError::new(self.0, None))
    }
}

fn configure_speech(state: &mut AppState, kind: SpeechErrorKind) {
    let store: Arc<dyn ObjectStore> = MemoryAdapter::object_store(
        StorageSpace::parse("speech").unwrap(),
        StoragePolicy::new(StoragePrivacy::Private, 1024, Duration::from_secs(60), None).unwrap(),
    );
    state.object_storage = StorageRegistry::from_stores([store]).unwrap();
    state.speech_provider = Some(Arc::new(FailingProvider(kind)));
}

async fn insert_voice(pool: &PgPool) {
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, provider_version)
           VALUES ($1, 'en-us-jenny', 'azure', 'en-US-JennyNeural', 'en-US', 'female', 'v1')"#,
    )
    .bind(Uuid::now_v7())
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn speech_endpoints_require_active_admin_and_return_problem_details(pool: PgPool) {
    let state = AppState::for_test(pool);
    let (status, body, content_type) = call(
        &state,
        Method::GET,
        "/api/v1/admin/speech/voices",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "invalid_token");
    assert_eq!(content_type, "application/problem+json");
}

#[sqlx::test]
async fn voice_list_keeps_provider_identity_in_descriptive_metadata_only(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version)
           VALUES ($1, 'en-us-jenny', 'azure', 'en-US-AvaNeural', 'en-US', 'female', '["chat"]', 'v1')"#,
    ).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    let state = AppState::for_test(pool);
    let token = state
        .admin_token_manager
        .generate(admin_id, AdminRole::Admin.as_str())
        .unwrap();
    let (status, body, _) = call(
        &state,
        Method::GET,
        "/api/v1/admin/speech/voices",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["alias"], "en-us-jenny");
    assert!(body["items"][0].get("provider_voice_id").is_none());
    assert_eq!(
        body["items"][0]["metadata"]["provider_voice_id"],
        "en-US-AvaNeural"
    );
    assert_eq!(body["items"][0]["display_name"], "Ava 爱娃");
}

#[sqlx::test]
async fn preview_rejects_ssml_and_unknown_fields_before_infrastructure(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    let state = AppState::for_test(pool);
    let token = state
        .admin_token_manager
        .generate(admin_id, AdminRole::Admin.as_str())
        .unwrap();
    for extra in [
        ("ssml", json!("<speak>hello</speak>")),
        ("object_key", json!("x.mp3")),
        ("audio_url", json!("https://example.test")),
    ] {
        let mut request = valid_request();
        request
            .as_object_mut()
            .unwrap()
            .insert(extra.0.to_owned(), extra.1);
        let (status, body, content_type) = call(
            &state,
            Method::POST,
            "/api/v1/admin/speech/previews",
            Some(&token),
            Some(request),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "invalid_request_body");
        assert_eq!(content_type, "application/problem+json");
    }
}

#[sqlx::test]
async fn provider_failures_have_stable_status_and_problem_code(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    insert_voice(&pool).await;
    for (kind, expected_status, expected_code) in [
        (
            SpeechErrorKind::InvalidRequest,
            StatusCode::BAD_REQUEST,
            "invalid_speech_preview",
        ),
        (
            SpeechErrorKind::RateLimited,
            StatusCode::TOO_MANY_REQUESTS,
            "speech_rate_limited",
        ),
        (
            SpeechErrorKind::Authentication,
            StatusCode::SERVICE_UNAVAILABLE,
            "speech_provider_unavailable",
        ),
        (
            SpeechErrorKind::Unavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "speech_provider_unavailable",
        ),
        (
            SpeechErrorKind::Timeout,
            StatusCode::SERVICE_UNAVAILABLE,
            "speech_provider_unavailable",
        ),
    ] {
        let mut state = AppState::for_test(pool.clone());
        configure_speech(&mut state, kind);
        let token = state
            .admin_token_manager
            .generate(admin_id, AdminRole::Admin.as_str())
            .unwrap();
        let (status, body, content_type) = call(
            &state,
            Method::POST,
            "/api/v1/admin/speech/previews",
            Some(&token),
            Some(valid_request()),
        )
        .await;
        assert_eq!(status, expected_status, "kind={kind:?}");
        assert_eq!(body["code"], expected_code, "kind={kind:?}");
        assert_eq!(content_type, "application/problem+json");
    }
}

#[sqlx::test]
async fn explicit_test_header_cannot_silently_generate_into_normal_storage(pool: PgPool) {
    let admin = seed_admin(&pool).await;
    let state = AppState::for_test(pool.clone());
    let token = state
        .admin_token_manager
        .generate(admin, AdminRole::SuperAdmin.as_str())
        .unwrap();
    for (run, expected) in [
        ("fixture-20261017", StatusCode::FORBIDDEN),
        ("../escape", StatusCode::BAD_REQUEST),
    ] {
        let response = tsz_rust::router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/admin/speech/previews")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-TSZ-Test-Run", run)
                    .body(Body::from(serde_json::to_vec(&valid_request()).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM speech.preview_cache")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
