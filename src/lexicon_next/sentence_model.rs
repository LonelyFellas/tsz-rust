use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::lexicon::{
    audio_assets::dto::AudioAsset,
    dto::{RichTextV3, VoiceProfileV3},
};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextSentenceTranslationStyle {
    WordForWord,
    BalancedFluency,
    AdaptedCreation,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceTranslation {
    pub id: Uuid,
    pub locale: String,
    pub style: NextSentenceTranslationStyle,
    pub content: RichTextV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceVariant {
    pub id: Uuid,
    pub locale: String,
    #[schema(pattern = "^(A1|A2|B1|B2|C1|C2)$")]
    pub level: String,
    pub content: RichTextV3,
    pub translations: Vec<NextSentenceTranslation>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::model::non_null_voice_profile"
    )]
    #[schema(nullable = false)]
    pub voice_profile: Option<VoiceProfileV3>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 8)]
    pub audio_assets: Vec<AudioAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceDraft {
    #[schema(minimum = 1, maximum = 1)]
    pub schema_version: i32,
    pub variants: Vec<NextSentenceVariant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentence {
    pub id: Uuid,
    pub target_language: String,
    pub owner_id: Uuid,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
    pub reference_count: i64,
    pub draft: NextSentenceDraft,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateNextSentence {
    pub id: Uuid,
    pub target_language: String,
    pub target_locale: String,
    pub medium_locale: String,
    pub variant: NextSentenceVariant,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveNextSentence {
    pub expected_revision: i64,
    pub target_locale: String,
    pub medium_locale: String,
    pub variant: NextSentenceVariant,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceQuery {
    pub target_language: String,
    pub target_locale: String,
    pub medium_locale: String,
    pub keyword: Option<String>,
    pub level: Option<String>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceListItem {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
    pub reference_count: i64,
    pub locale: String,
    pub level: String,
    pub content: RichTextV3,
    pub translation_count: usize,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceList {
    pub items: Vec<NextSentenceListItem>,
    pub pagination: crate::api::PaginationMeta,
}
