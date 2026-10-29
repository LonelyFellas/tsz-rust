pub mod audio_updates;
mod azure;
pub mod capabilities;
mod catalog;
mod config;
pub mod dubbing;
pub mod generated;
pub(crate) mod generated_references;
pub mod generated_source;
pub mod library;
pub mod locale;
mod manual_ssml;
pub(crate) mod model;
pub mod preview;
mod product_catalog;
mod provider;
pub mod publication_audio;
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
    CatalogEntry, ProductVoice, VoiceMetadata, catalog_snapshot, product_voices,
    voice_display_name, voice_metadata,
};
pub use provider::{SpeechError, SpeechErrorKind, SpeechProvider};
pub use ssml::build_ssml;

#[cfg(test)]
mod generated_source_tests;
#[cfg(test)]
mod tests;
