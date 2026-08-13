use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::lexicon::{
    dto::{RichText, RichTextV2},
    rich_text,
};

pub const CACHE_SCHEMA_VERSION: &str = "speech-cache-v1";
pub const SSML_BUILDER_VERSION: &str = "rich-text-v2-ssml-v1";
pub const PROVIDER_NAME: &str = "azure";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SpeechModelError {
    #[error("voice field '{field}' is invalid")]
    InvalidVoiceField { field: &'static str },
    #[error("speech style is not supported by this voice")]
    UnsupportedStyle,
    #[error("speech rate must be between -50 and 100 percent")]
    InvalidRate,
    #[error("speech pitch must be between -12 and 12 semitones")]
    InvalidPitch,
    #[error("only canonical RichTextV2 can be synthesized")]
    InvalidRichText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    provider: String,
    provider_voice_id: String,
    locale: String,
    styles: BTreeSet<String>,
}

impl Voice {
    pub fn new(
        provider: impl Into<String>,
        provider_voice_id: impl Into<String>,
        locale: impl Into<String>,
        styles: impl IntoIterator<Item = String>,
    ) -> Result<Self, SpeechModelError> {
        let provider = provider.into();
        let provider_voice_id = provider_voice_id.into();
        let locale = locale.into();
        validate_token(&provider, 32, "provider")?;
        validate_token(&provider_voice_id, 128, "provider_voice_id")?;
        validate_token(&locale, 35, "locale")?;
        let mut validated_styles = BTreeSet::new();
        for style in styles {
            validate_token(&style, 64, "style")?;
            validated_styles.insert(style);
        }
        Ok(Self {
            provider,
            provider_voice_id,
            locale,
            styles: validated_styles,
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn provider_voice_id(&self) -> &str {
        &self.provider_voice_id
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    pub fn supports_style(&self, style: &str) -> bool {
        self.styles.contains(style)
    }
}

fn validate_token(
    value: &str,
    max_len: usize,
    field: &'static str,
) -> Result<(), SpeechModelError> {
    if value.is_empty()
        || value.len() > max_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(SpeechModelError::InvalidVoiceField { field });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputFormat {
    Mp3_24Khz96KbpsMono,
}

impl AudioOutputFormat {
    pub const fn azure_value(self) -> &'static str {
        "audio-24khz-96kbitrate-mono-mp3"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechOptions {
    style: Option<String>,
    rate_percent: i16,
    pitch_semitones: i8,
    output_format: AudioOutputFormat,
}

impl SpeechOptions {
    pub fn new(
        voice: &Voice,
        style: Option<String>,
        rate_percent: i16,
        pitch_semitones: i8,
    ) -> Result<Self, SpeechModelError> {
        Self::validate(voice, style.as_deref(), rate_percent, pitch_semitones)?;
        Ok(Self {
            style,
            rate_percent,
            pitch_semitones,
            output_format: AudioOutputFormat::Mp3_24Khz96KbpsMono,
        })
    }

    fn validate(
        voice: &Voice,
        style: Option<&str>,
        rate_percent: i16,
        pitch_semitones: i8,
    ) -> Result<(), SpeechModelError> {
        if !(-50..=100).contains(&rate_percent) {
            return Err(SpeechModelError::InvalidRate);
        }
        if !(-12..=12).contains(&pitch_semitones) {
            return Err(SpeechModelError::InvalidPitch);
        }
        if let Some(style) = style {
            validate_token(style, 64, "style")?;
            if !voice.supports_style(style) {
                return Err(SpeechModelError::UnsupportedStyle);
            }
        }
        Ok(())
    }

    pub fn style(&self) -> Option<&str> {
        self.style.as_deref()
    }

    pub const fn rate_percent(&self) -> i16 {
        self.rate_percent
    }

    pub const fn pitch_semitones(&self) -> i8 {
        self.pitch_semitones
    }

    pub const fn output_format(&self) -> AudioOutputFormat {
        self.output_format
    }
}

#[derive(Debug, Clone)]
pub struct SynthesisRequest {
    voice: Voice,
    options: SpeechOptions,
    content: RichTextV2,
    normalized_content: Vec<u8>,
}

impl SynthesisRequest {
    pub fn new(
        voice: Voice,
        options: SpeechOptions,
        content: RichTextV2,
    ) -> Result<Self, SpeechModelError> {
        SpeechOptions::validate(
            &voice,
            options.style(),
            options.rate_percent(),
            options.pitch_semitones(),
        )?;
        let mut rich_text = RichText::V2(content);
        rich_text::canonicalize(&mut rich_text).map_err(|_| SpeechModelError::InvalidRichText)?;
        let RichText::V2(content) = rich_text else {
            unreachable!("the input variant is V2")
        };
        let normalized_content = serde_json::to_vec(&content)
            .expect("RichTextV2 serialization cannot fail for an in-memory value");
        Ok(Self {
            voice,
            options,
            content,
            normalized_content,
        })
    }

    pub fn fingerprint(&self) -> SynthesisFingerprint {
        let mut encoder = FingerprintEncoder::default();
        encoder.field(CACHE_SCHEMA_VERSION.as_bytes());
        encoder.field(SSML_BUILDER_VERSION.as_bytes());
        encoder.field(self.voice.provider.as_bytes());
        encoder.field(self.voice.provider_voice_id.as_bytes());
        encoder.field(self.voice.locale.as_bytes());
        match &self.options.style {
            Some(style) => {
                encoder.field(b"some");
                encoder.field(style.as_bytes());
            }
            None => encoder.field(b"none"),
        }
        encoder.field(self.options.rate_percent.to_string().as_bytes());
        encoder.field(self.options.pitch_semitones.to_string().as_bytes());
        encoder.field(self.options.output_format.azure_value().as_bytes());
        encoder.field(&self.normalized_content);
        SynthesisFingerprint(encoder.finish())
    }

    pub fn normalized_content(&self) -> &[u8] {
        &self.normalized_content
    }

    pub fn voice(&self) -> &Voice {
        &self.voice
    }

    pub fn options(&self) -> &SpeechOptions {
        &self.options
    }

    pub fn content(&self) -> &RichTextV2 {
        &self.content
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SynthesisFingerprint([u8; 32]);

impl SynthesisFingerprint {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for SynthesisFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SynthesisFingerprint([REDACTED])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesizedAudio {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
    pub provider_request_id: Option<String>,
}

#[derive(Default)]
struct FingerprintEncoder(Sha256);

impl FingerprintEncoder {
    fn field(&mut self, value: &[u8]) {
        let len = u64::try_from(value.len()).expect("field length fits u64");
        self.0.update(len.to_be_bytes());
        self.0.update(value);
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}
