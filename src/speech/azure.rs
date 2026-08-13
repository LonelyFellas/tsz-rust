use std::{fmt, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header};

use super::{
    SpeechError, SpeechErrorKind, SpeechProvider, SynthesisRequest, SynthesizedAudio, build_ssml,
    config::{SpeechConfigError, valid_region},
    model::PROVIDER_NAME,
};

const SUBSCRIPTION_KEY: &str = "ocp-apim-subscription-key";
const OUTPUT_FORMAT: &str = "x-microsoft-outputformat";
const REQUEST_ID: &str = "x-requestid";
const USER_AGENT: &str = "tsz-rust-speech/1";

#[derive(Clone)]
struct SecretKey(String);

impl fmt::Debug for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone)]
pub struct AzureSpeechProvider {
    client: Client,
    endpoint: String,
    key: SecretKey,
    max_response_bytes: usize,
}

impl fmt::Debug for AzureSpeechProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AzureSpeechProvider")
            .field("endpoint", &self.endpoint)
            .field("key", &self.key)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl AzureSpeechProvider {
    pub fn new(
        region: &str,
        key: String,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, SpeechConfigError> {
        if !valid_region(region) || key.is_empty() || max_response_bytes == 0 {
            return Err(SpeechConfigError::InvalidClient);
        }
        let endpoint = format!("https://{region}.tts.speech.microsoft.com/cognitiveservices/v1");
        Self::with_endpoint(
            endpoint,
            key,
            connect_timeout,
            request_timeout,
            max_response_bytes,
        )
    }

    fn with_endpoint(
        endpoint: String,
        key: String,
        connect_timeout: Duration,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, SpeechConfigError> {
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| SpeechConfigError::InvalidClient)?;
        Ok(Self {
            client,
            endpoint,
            key: SecretKey(key),
            max_response_bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        endpoint: String,
        request_timeout: Duration,
        max_response_bytes: usize,
    ) -> Self {
        Self::with_endpoint(
            endpoint,
            "test-key".to_owned(),
            request_timeout,
            request_timeout,
            max_response_bytes,
        )
        .expect("test provider configuration is valid")
    }
}

#[async_trait]
impl SpeechProvider for AzureSpeechProvider {
    fn provider_name(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn synthesize(
        &self,
        request: &SynthesisRequest,
    ) -> Result<SynthesizedAudio, SpeechError> {
        if request.voice().provider() != PROVIDER_NAME {
            return Err(SpeechError::new(SpeechErrorKind::InvalidRequest, None));
        }
        let ssml = build_ssml(request)
            .map_err(|_| SpeechError::new(SpeechErrorKind::InvalidRequest, None))?;
        let response = self
            .client
            .post(&self.endpoint)
            .header(SUBSCRIPTION_KEY, &self.key.0)
            .header(header::CONTENT_TYPE, "application/ssml+xml; charset=utf-8")
            .header(
                OUTPUT_FORMAT,
                request.options().output_format().azure_value(),
            )
            .body(ssml)
            .send()
            .await
            .map_err(map_transport_error)?;

        let status = response.status();
        if !status.is_success() {
            return Err(map_status(status));
        }
        if response.content_length().is_some_and(|length| {
            length > u64::try_from(self.max_response_bytes).unwrap_or(u64::MAX)
        }) {
            return Err(SpeechError::new(
                SpeechErrorKind::ResponseTooLarge,
                Some(status.as_u16()),
            ));
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        if !content_type.is_some_and(|value| value.eq_ignore_ascii_case("audio/mpeg")) {
            return Err(SpeechError::new(
                SpeechErrorKind::InvalidResponse,
                Some(status.as_u16()),
            ));
        }
        let provider_request_id = response
            .headers()
            .get(REQUEST_ID)
            .and_then(|value| value.to_str().ok())
            .filter(|value| safe_request_id(value))
            .map(ToOwned::to_owned);

        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_transport_error)?;
            if bytes.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(SpeechError::new(
                    SpeechErrorKind::ResponseTooLarge,
                    Some(status.as_u16()),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(SynthesizedAudio {
            bytes,
            content_type: "audio/mpeg",
            provider_request_id,
        })
    }
}

fn map_transport_error(error: reqwest::Error) -> SpeechError {
    let kind = if error.is_timeout() {
        SpeechErrorKind::Timeout
    } else {
        SpeechErrorKind::Unavailable
    };
    SpeechError::new(kind, error.status().map(|status| status.as_u16()))
}

fn map_status(status: StatusCode) -> SpeechError {
    let kind = match status {
        StatusCode::BAD_REQUEST => SpeechErrorKind::InvalidRequest,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => SpeechErrorKind::Authentication,
        StatusCode::TOO_MANY_REQUESTS => SpeechErrorKind::RateLimited,
        status if status.is_server_error() => SpeechErrorKind::Unavailable,
        _ => SpeechErrorKind::InvalidResponse,
    };
    SpeechError::new(kind, Some(status.as_u16()))
}

fn safe_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
