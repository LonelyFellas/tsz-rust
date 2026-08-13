use async_trait::async_trait;
use std::time::Duration;
use thiserror::Error;

use super::{SynthesisRequest, SynthesizedAudio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechErrorKind {
    InvalidRequest,
    Authentication,
    RateLimited,
    Unavailable,
    Timeout,
    ResponseTooLarge,
    InvalidResponse,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("speech provider failed: {kind:?}")]
pub struct SpeechError {
    pub kind: SpeechErrorKind,
    pub status: Option<u16>,
}

impl SpeechError {
    /// Constructor for provider adapters and deterministic fakes. `status` must contain only the
    /// upstream HTTP status, never a response body or credential-bearing detail.
    pub const fn new(kind: SpeechErrorKind, status: Option<u16>) -> Self {
        Self { kind, status }
    }

    pub const fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            SpeechErrorKind::RateLimited | SpeechErrorKind::Unavailable | SpeechErrorKind::Timeout
        )
    }
}

#[async_trait]
pub trait SpeechProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;

    /// Redis single-flight lease sizing hint. Implementations should return their complete
    /// synthesis request timeout; the orchestration layer adds storage/database tail room.
    fn synthesis_timeout(&self) -> Duration {
        Duration::from_secs(15)
    }

    async fn synthesize(&self, request: &SynthesisRequest)
    -> Result<SynthesizedAudio, SpeechError>;
}
