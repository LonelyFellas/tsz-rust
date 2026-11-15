use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::lexicon::{
    audio_assets::dto::AudioAsset,
    dto::{RichTextV3, VoiceProfileV3},
};

fn non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextSentenceTranslationStyle {
    WordForWord,
    BalancedFluency,
    AdaptedCreation,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextSentenceSourceType {
    Dictionary,
    Web,
    Adapted,
    AiComposed,
    HumanComposed,
}

/// A manually recorded attribution, not proof that a source has been verified.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceSource {
    pub id: Uuid,
    pub source_type: NextSentenceSourceType,
    #[schema(max_length = 500)]
    pub title: String,
    #[schema(max_length = 2000)]
    pub url: String,
    #[schema(max_length = 2000)]
    pub locator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceMetadata {
    #[schema(max_items = 1000)]
    pub register_ids: Vec<Uuid>,
    #[schema(max_items = 1000)]
    pub domain_ids: Vec<Uuid>,
    #[schema(max_items = 20)]
    pub sources: Vec<NextSentenceSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceTranslationMetadata {
    /// Unassessed is null. A score is translation freedom, never CEFR or accuracy.
    #[schema(minimum = 0, maximum = 100, multiple_of = 0.1)]
    pub freedom: Option<f64>,
    #[schema(max_items = 20)]
    pub sources: Vec<NextSentenceSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceTranslation {
    pub id: Uuid,
    pub locale: String,
    pub style: NextSentenceTranslationStyle,
    pub content: RichTextV3,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null"
    )]
    #[schema(nullable = false)]
    pub metadata: Option<NextSentenceTranslationMetadata>,
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
        deserialize_with = "non_null"
    )]
    #[schema(nullable = false)]
    pub metadata: Option<NextSentenceMetadata>,
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSentenceDraftChange {
    pub id: Uuid,
    #[schema(minimum = 1)]
    pub expected_revision: Option<i64>,
    pub target_locale: String,
    pub medium_locale: String,
    pub variant: NextSentenceVariant,
}
