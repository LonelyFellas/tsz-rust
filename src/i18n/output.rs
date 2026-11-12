//! Explicit client opt-in; no identity lookup and no process-global active language.
use super::{InterfaceLanguages, InterfaceLocaleV1 as Locale, InterfaceSurface};
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, header},
    middleware::Next,
    response::Response,
};

pub const LOCALIZATION_HEADER: &str = "x-tsz-localization";
tokio::task_local! { static REQUEST_LOCALE: Option<Locale>; }

pub fn current_locale() -> Option<Locale> {
    REQUEST_LOCALE.try_with(|locale| *locale).ok().flatten()
}

impl Locale {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::EnGb => "en-GB",
            Self::EnUs => "en-US",
        }
    }
}

fn match_language(tag: &str) -> Option<Locale> {
    // Read only the actual language identifier; private-use/Unicode extensions
    // must not impersonate its script or region. Mirrors the frontend Intl.Locale path.
    let parsed = tag.parse::<icu_locale_core::Locale>().ok()?;
    let id = parsed.id;
    match (id.language.as_str(), id.script.as_ref().map(|s| s.as_str())) {
        ("zh", Some("Hans")) => Some(Locale::ZhHans),
        ("zh", Some("Hant")) => Some(Locale::ZhHant),
        ("zh", None) => Some(
            if matches!(
                id.region.as_ref().map(|r| r.as_str()),
                Some("TW" | "HK" | "MO")
            ) {
                Locale::ZhHant
            } else {
                Locale::ZhHans
            },
        ),
        ("en", None | Some("Latn")) => {
            Some(if id.region.as_ref().map(|r| r.as_str()) == Some("US") {
                Locale::EnUs
            } else {
                Locale::EnGb
            })
        }
        _ => None,
    }
}

// Integer quality values avoid accepting NaN, exponent notation or out-of-range weights.
fn quality(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if !matches!(whole, "0" | "1")
        || fraction.len() > 3
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    if whole == "1" {
        return fraction.bytes().all(|b| b == b'0').then_some(1000);
    }
    Some(fraction.parse::<u16>().unwrap_or(0) * 10u16.pow((3 - fraction.len()) as u32))
}

pub fn negotiate(
    headers: &HeaderMap,
    path: &str,
    languages: &InterfaceLanguages,
) -> Option<Locale> {
    if !path.starts_with("/api/v1/") || path == "/api/v1/i18n/capabilities" {
        return None;
    }
    let mut markers = headers.get_all(LOCALIZATION_HEADER).iter();
    if markers.next().and_then(|h| h.to_str().ok()) != Some("v1") || markers.next().is_some() {
        return None;
    }
    let surface = if path == "/api/v1/admin" || path.starts_with("/api/v1/admin/") {
        InterfaceSurface::Admin
    } else {
        InterfaceSurface::Web
    };
    let directory = languages.for_surface(surface);
    let mut candidates = Vec::new();
    // Bound parsing work independently of the HTTP server's header size limit.
    let values: Vec<_> = headers.get_all(header::ACCEPT_LANGUAGE).iter().collect();
    if values.iter().map(|v| v.as_bytes().len()).sum::<usize>() > 1024 {
        return Some(directory.default_locale);
    }
    for value in values {
        let Ok(value) = value.to_str() else {
            return Some(directory.default_locale);
        };
        for item in value.split(',') {
            let mut parts = item.trim().split(';');
            let tag = parts.next().unwrap_or("").trim();
            let q = match parts.next() {
                None => Some(1000),
                Some(part) => part.trim().strip_prefix("q=").and_then(quality),
            };
            if parts.next().is_some() || q.is_none() {
                return Some(directory.default_locale);
            }
            if let Some(locale) =
                match_language(tag).filter(|l| directory.supported_locales.contains(l))
            {
                candidates.push((q.unwrap(), locale));
            }
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    Some(
        candidates
            .into_iter()
            .find(|(q, _)| *q > 0)
            .map_or(directory.default_locale, |(_, l)| l),
    )
}

pub async fn localize_request(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let api = request.uri().path().starts_with("/api/v1/");
    let locale = negotiate(
        request.headers(),
        request.uri().path(),
        &state.interface_languages,
    );
    let mut response = REQUEST_LOCALE.scope(locale, next.run(request)).await;
    if api {
        // Append rather than replacing existing Vary dimensions (or Vary: *).
        response.headers_mut().append(
            header::VARY,
            HeaderValue::from_static("X-TSZ-Localization, Accept-Language"),
        );
    }
    response
}

pub fn localized_headers(locale: Locale, headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_LANGUAGE,
        HeaderValue::from_static(locale.as_str()),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppError, ErrorCode};
    use axum::{
        Router, body::Body, http::Request as HttpRequest, response::IntoResponse, routing::get,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn languages() -> InterfaceLanguages {
        InterfaceLanguages::from_pairs(&[(
            "I18N_WEB_SUPPORTED_LOCALES".into(),
            "zh-Hans,zh-Hant,en-GB,en-US".into(),
        )])
        .unwrap()
    }

    #[tokio::test]
    async fn typed_details_preserve_legacy_wire_and_concurrent_locales() {
        async fn body(error: AppError) -> (http::HeaderMap, serde_json::Value) {
            let response = error.into_response();
            assert_eq!(response.status(), 409);
            let headers = response.headers().clone();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (headers, serde_json::from_slice(&bytes).unwrap())
        }
        fn error() -> AppError {
            crate::learning_tasks::service::insufficient_questions(3, 20)
                .with_meta(crate::error::ProblemMeta {
                    current_revision: Some(9_007_199_254_740_993),
                    ..Default::default()
                })
                .with_retry_after(17)
        }
        let (legacy_headers, legacy) = body(error()).await;
        assert_eq!(
            legacy["detail"],
            "仅有 3 道可出题内容，需要 20 道；请查看预览排除原因"
        );
        assert!(!legacy_headers.contains_key(header::CONTENT_LANGUAGE));
        let requests = [Locale::ZhHans, Locale::ZhHant, Locale::EnGb, Locale::EnUs].map(|locale| {
            let legacy = legacy.clone();
            REQUEST_LOCALE.scope(Some(locale), async move {
                tokio::task::yield_now().await;
                let (headers, mut localized) = body(error()).await;
                assert_eq!(headers[header::CONTENT_LANGUAGE], locale.as_str());
                assert_eq!(headers[header::RETRY_AFTER], "17");
                assert_eq!(localized["code"], "learning_insufficient_questions");
                assert_eq!(localized["meta"]["current_revision"], 9_007_199_254_740_993_i64);
                assert_eq!(localized.as_object().unwrap().len(), 6);
                assert_eq!(localized["detail"], match locale {
                    Locale::ZhHans => "仅有 3 道可出题内容，需要 20 道；请查看预览排除原因",
                    Locale::ZhHant => "僅有 3 道可出題內容，需要 20 道；請查看預覽排除原因",
                    Locale::EnGb | Locale::EnUs => "Only 3 questions are available; 20 are required. Check the preview for exclusion reasons.",
                });
                let mut expected = legacy;
                for key in ["title", "detail"] {
                    expected.as_object_mut().unwrap().remove(key);
                    localized.as_object_mut().unwrap().remove(key);
                }
                assert_eq!(localized, expected);

                let response = AppError::internal(anyhow::anyhow!("password=private"))
                    .into_response();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                assert!(!String::from_utf8_lossy(&bytes).contains("private"));
            })
        });
        futures_util::future::join_all(requests).await;
        assert!(current_locale().is_none());
    }

    #[tokio::test]
    async fn typed_field_diagnostic_keeps_legacy_and_node_identity() {
        use crate::lexicon::dto::{
            PersistedWordStep, V3DraftNodeLocation, V3DraftValidationIssue, V3ValidationIssueCode,
        };
        let id = uuid::Uuid::new_v4();
        let make_error = || {
            AppError::unprocessable(ErrorCode::ValidationFailed, "invalid shared form mapping")
                .with_v3_field_issues(vec![V3DraftValidationIssue {
                    schema_version: 3,
                    step: PersistedWordStep::Forms,
                    node_id: id,
                    field: "content_id".into(),
                    code: V3ValidationIssueCode::FormContentMappingInvalid,
                    message: format!("invalid content {id}"),
                    node_location: V3DraftNodeLocation {
                        content_id: Some(id),
                        content_slot_id: None,
                        node_role: "forms.content".into(),
                        ancestor_node_ids: vec![],
                        pos_id: None,
                        form_group_id: None,
                        membership_id: None,
                        form_id: None,
                        variant_id: None,
                        pronunciation_id: None,
                        form_type: None,
                        dialect: None,
                    },
                }])
                .with_issue_template(Some(crate::i18n::errors::IssueTemplate::InvalidContent(id)))
        };
        async fn json(error: AppError) -> serde_json::Value {
            let bytes = error
                .into_response()
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes();
            serde_json::from_slice(&bytes).unwrap()
        }
        let legacy = json(make_error()).await;
        assert_eq!(
            legacy["field_issues"][0]["message"],
            format!("invalid content {id}")
        );
        let localized = REQUEST_LOCALE
            .scope(Some(Locale::ZhHant), json(make_error()))
            .await;
        assert_eq!(
            localized["field_issues"][0]["message"],
            format!("內容 {id} 無效")
        );
        assert_eq!(
            localized["field_issues"][0]["node_id"],
            legacy["field_issues"][0]["node_id"]
        );
        assert_eq!(
            localized["field_issues"][0]["node_location"],
            legacy["field_issues"][0]["node_location"]
        );
    }

    #[tokio::test]
    async fn duplicate_default_recordings_return_localized_conflict_details() {
        use crate::lexicon::audio_assets::{dto::AudioAsset, hydrate_recordings};
        let first: AudioAsset = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::now_v7(), "locale": "en-GB", "gender": "female",
            "content_type": "audio/mpeg", "size_bytes": 3, "duration_ms": null,
            "original_name": "fixture.mp3", "created_at": "2026-11-10T00:00:00Z"
        }))
        .unwrap();
        let second = AudioAsset {
            id: uuid::Uuid::now_v7(),
            ..first.clone()
        };
        let canonical = [(first.id, first.clone()), (second.id, second.clone())].into();
        for (locale, detail) in [
            (None, "每种口音只能选择一条默认播放录音"),
            (
                Some(Locale::EnUs),
                "Choose only one default recording per accent.",
            ),
            (Some(Locale::ZhHant), "每種口音只能選擇一條預設播放錄音"),
        ] {
            let mut selected = [first.clone(), second.clone()];
            selected
                .iter_mut()
                .for_each(|asset| asset.is_default = Some(true));
            let error = hydrate_recordings(&mut selected, &canonical).unwrap_err();
            let response = REQUEST_LOCALE
                .scope(locale, async { error.into_response() })
                .await;
            assert_eq!(response.status(), 400);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_LANGUAGE)
                    .and_then(|value| value.to_str().ok()),
                locale.map(Locale::as_str)
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let problem: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(problem["code"], "invalid_speech_preview");
            assert_eq!(problem["detail"], detail);
        }
    }

    #[test]
    fn opt_in_weights_surfaces_and_fallbacks() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));
        assert_eq!(negotiate(&headers, "/api/v1/me", &languages()), None);
        headers.insert(LOCALIZATION_HEADER, HeaderValue::from_static("v1"));
        for (input, expected) in [
            ("en-US", Locale::EnUs),
            ("en-GB-x-US", Locale::EnGb),
            ("zh-Hant-x-Hans", Locale::ZhHant),
            ("en-Cyrl,en-US;q=0.9", Locale::EnUs),
            ("zh-Latn,en-US;q=0.9", Locale::EnUs),
            ("en-US-US,en-GB;q=0.9", Locale::EnGb),
            ("en-u-rg-uszzzz", Locale::EnGb),
            ("zh-Hans-CN-u-nu-hanidec", Locale::ZhHans),
            ("en-Latn-US", Locale::EnUs),
            ("en-US-u-ca-gregory-u-nu-latn,zh-Hant;q=0.9", Locale::ZhHant),
            ("zh-TW", Locale::ZhHant),
            ("zh-Hans-TW", Locale::ZhHans),
            ("en-AU", Locale::EnGb),
            ("zh;q=0.1,en-US;q=0.9", Locale::EnUs),
            ("en-US;q=0,en-GB;q=1", Locale::EnGb),
            ("en-US;q=NaN", Locale::ZhHans),
            ("en-US;q=1.001", Locale::ZhHans),
            ("en-US;q=0.9999", Locale::ZhHans),
            ("ja", Locale::ZhHans),
            ("*", Locale::ZhHans),
            ("", Locale::ZhHans),
        ] {
            headers.insert(
                header::ACCEPT_LANGUAGE,
                HeaderValue::from_str(input).unwrap(),
            );
            assert_eq!(
                negotiate(&headers, "/api/v1/me", &languages()),
                Some(expected),
                "{input}"
            );
        }
        headers.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));
        assert_eq!(
            negotiate(&headers, "/api/v1/admin/profile", &languages()),
            Some(Locale::ZhHans)
        );
        assert_eq!(negotiate(&headers, "/healthz", &languages()), None);
        assert_eq!(
            negotiate(&headers, "/api/v1/i18n/capabilities", &languages()),
            None
        );
        headers.remove(header::ACCEPT_LANGUAGE);
        assert_eq!(
            negotiate(&headers, "/api/v1/me", &languages()),
            Some(Locale::ZhHans)
        );
        headers.append(LOCALIZATION_HEADER, HeaderValue::from_static("v1"));
        assert_eq!(negotiate(&headers, "/api/v1/me", &languages()), None);
    }

    #[tokio::test]
    async fn concurrent_requests_keep_problem_structure_and_retry_headers() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        let mut state = AppState::for_test(pool);
        state.interface_languages = languages();
        let app = Router::new()
            .route(
                "/api/v1/error",
                get(|| async {
                    tokio::task::yield_now().await;
                    AppError::rate_limited(ErrorCode::OtpRateLimited, "too many requests")
                        .with_retry_after(7)
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state,
                localize_request,
            ));
        let requests = ["zh-Hans", "zh-Hant", "en-GB", "en-US"].map(|locale| {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/api/v1/error")
                            .header(LOCALIZATION_HEADER, "v1")
                            .header(header::ACCEPT_LANGUAGE, locale)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), 429);
                assert_eq!(response.headers()[header::RETRY_AFTER], "7");
                assert_eq!(response.headers()[header::CONTENT_LANGUAGE], locale);
                assert_eq!(
                    response.headers()[header::CACHE_CONTROL],
                    "private, no-store"
                );
                let body: serde_json::Value = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(body["code"], "otp_rate_limited");
                assert_eq!(body["type"], "urn:tsz:problem:otp_rate_limited");
                assert_eq!(body["status"], 429);
                assert_eq!(body.as_object().unwrap().len(), 5);
                body["detail"].as_str().unwrap().to_owned()
            }
        });
        let results = futures_util::future::join_all(requests).await;
        assert_ne!(results[0], results[1]);
        assert_eq!(results[2], results[3]);
        assert!(current_locale().is_none());
        let legacy = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/error")
                    .header(header::ACCEPT_LANGUAGE, "en-US")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!legacy.headers().contains_key(header::CONTENT_LANGUAGE));
        let body: serde_json::Value =
            serde_json::from_slice(&legacy.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["detail"], "too many requests");
    }
}

#[cfg(test)]
mod router_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn authentication_and_extraction_failures_localize_before_identity_lookup() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1:1/never_connect")
            .unwrap();
        let mut state = AppState::for_test(pool);
        state.interface_languages = InterfaceLanguages::from_pairs(&[(
            "I18N_WEB_SUPPORTED_LOCALES".into(),
            "zh-Hans,zh-Hant,en-GB,en-US".into(),
        )])
        .unwrap();
        let app = crate::router(state);
        for (method, path, payload, expected_code, status) in [
            ("GET", "/api/v1/me/preferences", "", "invalid_token", 401),
            ("POST", "/api/v1/otp/send", "{", "invalid_json", 400),
            (
                "POST",
                "/api/v1/otp/send",
                r#"{"phone":"invalid","purpose":"register"}"#,
                "invalid_phone",
                400,
            ),
        ] {
            for locale in [None, Some("zh-Hant"), Some("en-US")] {
                let mut request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json");
                if let Some(locale) = locale {
                    request = request
                        .header(LOCALIZATION_HEADER, "v1")
                        .header(header::ACCEPT_LANGUAGE, locale);
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::from(payload)).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), status);
                assert_eq!(
                    response
                        .headers()
                        .get(header::CONTENT_LANGUAGE)
                        .and_then(|v| v.to_str().ok()),
                    locale
                );
                let body: serde_json::Value = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(body["code"], expected_code);
                assert_eq!(body["status"], status);
                if expected_code == "invalid_phone" {
                    assert_eq!(body["field"], "phone");
                }
            }
        }
    }
}
