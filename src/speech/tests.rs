use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderValue, Request, Response, StatusCode},
    routing::post,
};

use crate::lexicon::dto::{
    RichTextAnnotation, RichTextEmphasisLevel, RichTextHighlightColor, RichTextPhonemeAlphabet,
    RichTextV2,
};

use super::*;

fn voice() -> Voice {
    Voice::new(
        "azure",
        "en-US-JennyNeural",
        "en-US",
        ["cheerful".to_owned()],
    )
    .unwrap()
}

fn request(content: RichTextV2) -> SynthesisRequest {
    let voice = voice();
    let options = SpeechOptions::new(&voice, Some("cheerful".to_owned()), 10, -2).unwrap();
    SynthesisRequest::new(voice, options, content).unwrap()
}

fn plain(text: &str) -> RichTextV2 {
    RichTextV2 {
        version: 2,
        text: text.to_owned(),
        annotations: Vec::new(),
    }
}

#[test]
fn ssml_is_escaped_nested_and_deterministic() {
    let content = RichTextV2 {
        version: 2,
        text: "A<&😀B".to_owned(),
        annotations: vec![
            RichTextAnnotation::Highlight {
                start: 0,
                end: 1,
                color: RichTextHighlightColor::Blue,
            },
            RichTextAnnotation::Pause {
                at: 5,
                duration_ms: 250,
            },
            RichTextAnnotation::Phoneme {
                start: 3,
                end: 4,
                alphabet: RichTextPhonemeAlphabet::Ipa,
                phoneme: " a&b ".to_owned(),
            },
            RichTextAnnotation::Emphasis {
                start: 0,
                end: 5,
                level: RichTextEmphasisLevel::Strong,
            },
        ],
    };
    let request = request(content);
    let once = build_ssml(&request).unwrap();
    let twice = build_ssml(&request).unwrap();
    assert_eq!(once, twice);
    assert!(
        once.contains("A&lt;&amp;<phoneme alphabet=\"ipa\" ph=\"a&amp;b\">😀</phoneme>B"),
        "{once}"
    );
    assert!(once.contains("<emphasis level=\"strong\">"));
    assert!(once.contains("</emphasis><break time=\"250ms\"/>"));
    assert!(!once.contains("highlight"));
    assert!(once.contains("rate=\"+10%\" pitch=\"-2st\""));
}

#[test]
fn rich_text_boundaries_are_reused_and_v1_or_invalid_v2_are_impossible() {
    let voice = voice();
    let options = SpeechOptions::new(&voice, None, 0, 0).unwrap();
    let too_long = "a".repeat(crate::lexicon::rich_text::MAX_RICH_TEXT_CODEPOINTS + 1);
    assert_eq!(
        SynthesisRequest::new(voice.clone(), options.clone(), plain(&too_long)).unwrap_err(),
        SpeechModelError::InvalidRichText
    );
    let invalid = RichTextV2 {
        version: 1,
        text: "text".to_owned(),
        annotations: Vec::new(),
    };
    assert_eq!(
        SynthesisRequest::new(voice, options, invalid).unwrap_err(),
        SpeechModelError::InvalidRichText
    );
}

#[test]
fn voice_style_rate_and_pitch_are_validated() {
    assert!(Voice::new("azure", "bad voice", "en-US", []).is_err());
    let voice = voice();
    assert_eq!(
        SpeechOptions::new(&voice, Some("sad".to_owned()), 0, 0).unwrap_err(),
        SpeechModelError::UnsupportedStyle
    );
    assert_eq!(
        SpeechOptions::new(&voice, None, -51, 0).unwrap_err(),
        SpeechModelError::InvalidRate
    );
    assert_eq!(
        SpeechOptions::new(&voice, None, 0, 13).unwrap_err(),
        SpeechModelError::InvalidPitch
    );
    assert!(SpeechOptions::new(&voice, None, -50, -12).is_ok());
    assert!(SpeechOptions::new(&voice, None, 100, 12).is_ok());

    let cheerful_options = SpeechOptions::new(&voice, Some("cheerful".to_owned()), 0, 0).unwrap();
    let plain_voice = Voice::new("azure", "voice-2", "en-US", []).unwrap();
    assert_eq!(
        SynthesisRequest::new(plain_voice, cheerful_options, plain("hello")).unwrap_err(),
        SpeechModelError::UnsupportedStyle
    );
}

#[test]
fn fingerprint_is_stable_for_equivalent_canonical_content_and_versions_options() {
    let first = request(RichTextV2 {
        version: 2,
        text: "test".to_owned(),
        annotations: vec![
            RichTextAnnotation::Emphasis {
                start: 2,
                end: 4,
                level: RichTextEmphasisLevel::Strong,
            },
            RichTextAnnotation::Emphasis {
                start: 0,
                end: 2,
                level: RichTextEmphasisLevel::Strong,
            },
        ],
    });
    let second = request(RichTextV2 {
        version: 2,
        text: "test".to_owned(),
        annotations: vec![RichTextAnnotation::Emphasis {
            start: 0,
            end: 4,
            level: RichTextEmphasisLevel::Strong,
        }],
    });
    assert_eq!(first.normalized_content(), second.normalized_content());
    assert_eq!(first.fingerprint(), second.fingerprint());

    let voice = voice();
    let changed_options = SpeechOptions::new(&voice, None, 10, -2).unwrap();
    let changed = SynthesisRequest::new(voice, changed_options, second.content().clone()).unwrap();
    assert_ne!(first.fingerprint(), changed.fingerprint());

    let other_voice = Voice::new(
        "other-provider",
        "en-US-JennyNeural",
        "en-US",
        ["cheerful".to_owned()],
    )
    .unwrap();
    let other_options =
        SpeechOptions::new(&other_voice, Some("cheerful".to_owned()), 10, -2).unwrap();
    let other_provider =
        SynthesisRequest::new(other_voice, other_options, second.content().clone()).unwrap();
    assert_ne!(first.fingerprint(), other_provider.fingerprint());

    let other_locale_voice = Voice::new(
        "azure",
        "en-US-JennyNeural",
        "en-GB",
        ["cheerful".to_owned()],
    )
    .unwrap();
    let other_locale_options =
        SpeechOptions::new(&other_locale_voice, Some("cheerful".to_owned()), 10, -2).unwrap();
    let other_locale = SynthesisRequest::new(
        other_locale_voice,
        other_locale_options,
        second.content().clone(),
    )
    .unwrap();
    assert_ne!(first.fingerprint(), other_locale.fingerprint());
    assert_eq!(CACHE_SCHEMA_VERSION, "speech-cache-v1");
    assert_eq!(SSML_BUILDER_VERSION, "rich-text-v2-ssml-v1");
}

#[derive(Clone)]
struct StubResponse {
    status: StatusCode,
    content_type: &'static str,
    body: Vec<u8>,
    delay: Duration,
}

async fn stub_handler(State(stub): State<StubResponse>, request: Request<Body>) -> Response<Body> {
    assert_eq!(
        request.headers().get("x-microsoft-outputformat").unwrap(),
        "audio-24khz-96kbitrate-mono-mp3"
    );
    assert_eq!(
        request.headers().get("ocp-apim-subscription-key").unwrap(),
        "test-key"
    );
    tokio::time::sleep(stub.delay).await;
    let mut response = Response::new(Body::from(stub.body));
    *response.status_mut() = stub.status;
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static(stub.content_type));
    response
        .headers_mut()
        .insert("x-requestid", HeaderValue::from_static("safe-request-1"));
    response
}

async fn provider_for(stub: StubResponse, timeout: Duration, limit: usize) -> AzureSpeechProvider {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/cognitiveservices/v1", post(stub_handler))
                .with_state(stub),
        )
        .await
        .unwrap();
    });
    AzureSpeechProvider::for_test(
        format!("http://{address}/cognitiveservices/v1"),
        timeout,
        limit,
    )
}

#[tokio::test]
async fn azure_accepts_mp3_and_maps_http_statuses() {
    let ok = provider_for(
        StubResponse {
            status: StatusCode::OK,
            content_type: "audio/mpeg; codec=mp3",
            body: b"mp3".to_vec(),
            delay: Duration::ZERO,
        },
        Duration::from_secs(1),
        1024,
    )
    .await;
    let audio = ok.synthesize(&request(plain("hello"))).await.unwrap();
    assert_eq!(audio.bytes, b"mp3");
    assert_eq!(audio.provider_request_id.as_deref(), Some("safe-request-1"));

    for (status, kind) in [
        (StatusCode::BAD_REQUEST, SpeechErrorKind::InvalidRequest),
        (StatusCode::UNAUTHORIZED, SpeechErrorKind::Authentication),
        (StatusCode::TOO_MANY_REQUESTS, SpeechErrorKind::RateLimited),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            SpeechErrorKind::Unavailable,
        ),
    ] {
        let provider = provider_for(
            StubResponse {
                status,
                content_type: "text/plain",
                body: b"sensitive provider response".to_vec(),
                delay: Duration::ZERO,
            },
            Duration::from_secs(1),
            1024,
        )
        .await;
        let error = provider
            .synthesize(&request(plain("hello")))
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.status, Some(status.as_u16()));
        assert!(!error.to_string().contains("sensitive"));
    }
}

#[tokio::test]
async fn azure_rejects_timeout_oversize_and_wrong_content_type() {
    let timeout = provider_for(
        StubResponse {
            status: StatusCode::OK,
            content_type: "audio/mpeg",
            body: b"mp3".to_vec(),
            delay: Duration::from_millis(100),
        },
        Duration::from_millis(20),
        1024,
    )
    .await;
    assert_eq!(
        timeout
            .synthesize(&request(plain("hello")))
            .await
            .unwrap_err()
            .kind,
        SpeechErrorKind::Timeout
    );

    let oversize = provider_for(
        StubResponse {
            status: StatusCode::OK,
            content_type: "audio/mpeg",
            body: vec![1; 9],
            delay: Duration::ZERO,
        },
        Duration::from_secs(1),
        8,
    )
    .await;
    assert_eq!(
        oversize
            .synthesize(&request(plain("hello")))
            .await
            .unwrap_err()
            .kind,
        SpeechErrorKind::ResponseTooLarge
    );

    let wrong_type = provider_for(
        StubResponse {
            status: StatusCode::OK,
            content_type: "application/json",
            body: b"{}".to_vec(),
            delay: Duration::ZERO,
        },
        Duration::from_secs(1),
        1024,
    )
    .await;
    assert_eq!(
        wrong_type
            .synthesize(&request(plain("hello")))
            .await
            .unwrap_err()
            .kind,
        SpeechErrorKind::InvalidResponse
    );
}

#[tokio::test]
async fn azure_rejects_requests_for_another_provider_before_http() {
    let provider = AzureSpeechProvider::for_test(
        "http://127.0.0.1:1/cognitiveservices/v1".to_owned(),
        Duration::from_millis(50),
        1024,
    );
    let voice = Voice::new("other-provider", "voice-1", "en-US", []).unwrap();
    let options = SpeechOptions::new(&voice, None, 0, 0).unwrap();
    let request = SynthesisRequest::new(voice, options, plain("hello")).unwrap();
    assert_eq!(
        provider.synthesize(&request).await.unwrap_err().kind,
        SpeechErrorKind::InvalidRequest
    );
}

#[derive(Default)]
struct FakeProvider;

#[async_trait]
impl SpeechProvider for FakeProvider {
    fn provider_name(&self) -> &'static str {
        "fake"
    }

    async fn synthesize(
        &self,
        request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        Ok(SynthesizedAudio {
            bytes: request.content().text.as_bytes().to_vec(),
            content_type: "audio/mpeg",
            provider_request_id: None,
        })
    }
}

#[tokio::test]
async fn speech_provider_is_replaceable_with_a_fake() {
    let provider: Arc<dyn SpeechProvider> = Arc::new(FakeProvider);
    let audio = provider.synthesize(&request(plain("fake"))).await.unwrap();
    assert_eq!(provider.provider_name(), "fake");
    assert_eq!(audio.bytes, b"fake");
}

#[test]
fn config_is_disabled_by_default_and_enabled_all_or_nothing() {
    assert_eq!(AzureSpeechConfig::from_pairs(Vec::new()).unwrap(), None);
    assert_eq!(
        AzureSpeechConfig::from_pairs([("AZURE_SPEECH_ENABLED".to_owned(), "false".to_owned())])
            .unwrap(),
        None
    );
    let missing =
        AzureSpeechConfig::from_pairs([("AZURE_SPEECH_ENABLED".to_owned(), "true".to_owned())])
            .unwrap_err();
    assert_eq!(missing, SpeechConfigError::MissingField { field: "REGION" });
    let configured = AzureSpeechConfig::from_pairs([
        ("AZURE_SPEECH_ENABLED".to_owned(), "true".to_owned()),
        ("AZURE_SPEECH_REGION".to_owned(), "eastasia".to_owned()),
        ("AZURE_SPEECH_KEY".to_owned(), "secret-key".to_owned()),
    ])
    .unwrap()
    .unwrap();
    let debug = format!("{configured:?}");
    assert!(!debug.contains("secret-key"));
    configured.build_provider().unwrap();
}
