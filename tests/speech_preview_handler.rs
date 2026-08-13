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

async fn seed_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    AdminRepository::new(pool.clone())
        .create(NewAdmin {
            id,
            phone: format!("speech-{}", id.simple()),
            display_name: "Speech Admin".to_owned(),
            password_hash: "hash".to_owned(),
            role: AdminRole::Admin,
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
async fn voice_list_is_public_contract_without_provider_identity(pool: PgPool) {
    let admin_id = seed_admin(&pool).await;
    sqlx::query(
        r#"INSERT INTO speech.voices
           (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version)
           VALUES ($1, 'en-us-jenny', 'azure', 'provider-secret-id', 'en-US', 'female', '["chat"]', 'v1')"#,
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
    assert!(body.to_string().find("provider-secret-id").is_none());
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
