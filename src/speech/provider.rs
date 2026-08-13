use async_trait::async_trait;
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
    pub(crate) const fn new(kind: SpeechErrorKind, status: Option<u16>) -> Self {
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

    async fn synthesize(&self, request: &SynthesisRequest)
    -> Result<SynthesizedAudio, SpeechError>;
}
