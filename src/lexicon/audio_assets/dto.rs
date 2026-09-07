use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// 允许直传的音频类型。扩展名同时决定服务端生成的对象键后缀。
pub const AUDIO_ASSET_CONTENT_TYPES: [(&str, &str); 4] = [
    ("audio/mpeg", "mp3"),
    ("audio/mp4", "m4a"),
    ("audio/wav", "wav"),
    ("audio/ogg", "ogg"),
];

/// 取媒体类型主体（丢掉 `; charset=` 之类的参数）后匹配白名单，返回对象键扩展名。
pub fn audio_extension(content_type: &str) -> Option<&'static str> {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    AUDIO_ASSET_CONTENT_TYPES
        .into_iter()
        .find(|(candidate, _)| *candidate == media_type)
        .map(|(_, extension)| extension)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum AudioAssetLocale {
    #[serde(rename = "en-GB")]
    EnGb,
    #[serde(rename = "en-US")]
    EnUs,
}

impl AudioAssetLocale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnGb => "en-GB",
            Self::EnUs => "en-US",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioAssetGender {
    Female,
    Male,
}

impl AudioAssetGender {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Female => "female",
            Self::Male => "male",
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAudioUploadRequest {
    /// 白名单内的 MIME；客户端 PUT 时必须原样回发，否则 OSS 验签失败。
    pub content_type: String,
    pub size: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioUploadTicket {
    /// 服务端生成的暂存对象键，confirm 时原样回传。
    pub key: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub expires_in: u64,
    pub max_bytes: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateAudioUploadResponse {
    pub upload: AudioUploadTicket,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmAudioAssetRequest {
    pub key: String,
    pub locale: AudioAssetLocale,
    pub gender: AudioAssetGender,
    /// 仅作展示元数据，绝不参与对象键。
    #[schema(max_length = 120)]
    pub original_name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioAsset {
    pub id: Uuid,
    pub locale: AudioAssetLocale,
    pub gender: AudioAssetGender,
    pub content_type: String,
    pub size_bytes: i64,
    /// 服务端暂不探测时长，恒为 null。
    pub duration_ms: Option<i32>,
    pub original_name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConfirmAudioAssetResponse {
    pub asset: AudioAsset,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct AudioAssetPath {
    pub id: Uuid,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioAssetUrlResponse {
    pub url: String,
    pub expires_at: DateTime<Utc>,
    pub url_expires_in_seconds: u64,
}
