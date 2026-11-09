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
    let parts: Vec<_> = tag.split('-').collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || p.len() > 8 || !p.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return None;
    }
    if parts[0].eq_ignore_ascii_case("zh") {
        if parts.iter().any(|p| p.eq_ignore_ascii_case("Hans")) {
            return Some(Locale::ZhHans);
        }
        if parts.iter().any(|p| p.eq_ignore_ascii_case("Hant")) {
            return Some(Locale::ZhHant);
        }
        return Some(
            if parts
                .iter()
                .any(|p| ["TW", "HK", "MO"].iter().any(|r| p.eq_ignore_ascii_case(r)))
            {
                Locale::ZhHant
            } else {
                Locale::ZhHans
            },
        );
    }
    parts[0].eq_ignore_ascii_case("en").then(|| {
        if parts.iter().any(|p| p.eq_ignore_ascii_case("US")) {
            Locale::EnUs
        } else {
            Locale::EnGb
        }
    })
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
    use axum::{Router, body::Body, http::Request as HttpRequest, routing::get};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn languages() -> InterfaceLanguages {
        InterfaceLanguages::from_pairs(&[(
            "I18N_WEB_SUPPORTED_LOCALES".into(),
            "zh-Hans,zh-Hant,en-GB,en-US".into(),
        )])
        .unwrap()
    }

    #[test]
    fn opt_in_weights_surfaces_and_fallbacks() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));
        assert_eq!(negotiate(&headers, "/api/v1/me", &languages()), None);
        headers.insert(LOCALIZATION_HEADER, HeaderValue::from_static("v1"));
        for (input, expected) in [
            ("en-US", Locale::EnUs),
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
