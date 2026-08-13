mod azure;
mod config;
mod model;
pub mod preview;
mod provider;
mod ssml;

pub use azure::AzureSpeechProvider;
pub use config::{AzureSpeechConfig, SpeechConfigError};
pub use model::{
    AudioOutputFormat, CACHE_SCHEMA_VERSION, SSML_BUILDER_VERSION, SpeechModelError, SpeechOptions,
    SynthesisFingerprint, SynthesisRequest, SynthesizedAudio, Voice,
};
pub use provider::{SpeechError, SpeechErrorKind, SpeechProvider};
pub use ssml::build_ssml;

#[cfg(test)]
mod tests;
