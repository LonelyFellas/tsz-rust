mod azure;
mod catalog;
mod config;
mod model;
pub mod preview;
mod product_catalog;
mod provider;
mod ssml;

pub use azure::AzureSpeechProvider;
pub use catalog::CatalogVoice;
pub use config::{AzureSpeechConfig, SpeechConfigError};
pub use model::{
    AudioOutputFormat, CACHE_SCHEMA_VERSION, MAX_SPEECH_RATE_PERCENT, MIN_SPEECH_RATE_PERCENT,
    SSML_BUILDER_VERSION, SpeechModelError, SpeechOptions, SynthesisFingerprint, SynthesisRequest,
    SynthesizedAudio, Voice,
};
pub use product_catalog::{
    CatalogEntry, ProductVoice, catalog_snapshot, product_voices, voice_display_name,
};
pub use provider::{SpeechError, SpeechErrorKind, SpeechProvider};
pub use ssml::build_ssml;

#[cfg(test)]
mod tests;
