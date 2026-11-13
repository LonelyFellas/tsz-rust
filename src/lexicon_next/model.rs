use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextEntryKind {
    Word,
    Phrase,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextForm {
    pub id: Uuid,
    /// Actual content locale, never the editor's current selection.
    pub locale: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NextDefinitionStyle {
    Definition,
    Sentence,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDefinition {
    pub id: Uuid,
    pub locale: String,
    pub text: String,
    pub style: NextDefinitionStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextEquivalent {
    pub id: Uuid,
    pub locale: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSense {
    pub id: Uuid,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_group_id"
    )]
    #[schema(nullable = false)]
    pub sense_group_id: Option<Uuid>,
    /// Language-specific code; null is an incomplete draft, not an inferred POS.
    #[schema(required)]
    pub part_of_speech: Option<String>,
    pub definitions: Vec<NextDefinition>,
    pub equivalents: Vec<NextEquivalent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSenseGroup {
    pub id: Uuid,
    #[schema(max_length = 200)]
    pub name_zh: String,
    #[schema(max_length = 200)]
    pub name_en: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDraft {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "non_null_sense_groups"
    )]
    #[schema(nullable = false)]
    pub sense_groups: Option<Vec<NextSenseGroup>>,
    #[schema(minimum = 1, maximum = 1)]
    pub schema_version: i32,
    pub headword: String,
    pub forms: Vec<NextForm>,
    pub senses: Vec<NextSense>,
    /// V1 editor content, with independent lexicon-next identities.
    #[serde(
        default,
        deserialize_with = "non_null_form_editor",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(nullable = false)]
    pub form_editor: Option<crate::lexicon::dto::SharedFormsStepContentV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextEntry {
    pub id: Uuid,
    pub target_language: String,
    pub kind: NextEntryKind,
    pub owner_id: Uuid,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
    pub draft: NextDraft,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateNextEntry {
    pub target_language: String,
    pub kind: NextEntryKind,
    pub headword: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveNextDraft {
    #[schema(minimum = 1)]
    pub expected_revision: i64,
    /// Complete draft, including every locale. Omitted nodes are removed from the current draft.
    pub draft: NextDraft,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextLanguageCapability {
    pub target_language: String,
    pub target_locales: Vec<String>,
    pub medium_locales: Vec<String>,
    pub common_target_locale: String,
    pub part_of_speech_codes: Vec<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextCapabilities {
    #[schema(minimum = 1, maximum = 1)]
    pub schema_version: i32,
    pub language_rules_version: String,
    pub languages: Vec<NextLanguageCapability>,
    pub manual_drafts: bool,
    pub generation: bool,
    pub publishing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub form_editor: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sense_groups: Option<bool>,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct NextViewQuery {
    pub target_locale: String,
    pub medium_locale: String,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct NextListQuery {
    pub target_language: Option<String>,
    pub medium_locale: Option<String>,
    pub headword: Option<String>,
    pub kind: Option<NextEntryKind>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct NextEntrySummary {
    pub id: Uuid,
    pub target_language: String,
    #[schema(value_type = NextEntryKind)]
    pub kind: String,
    pub headword: String,
    pub owner_id: Uuid,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
    pub part_of_speech: Vec<String>,
    #[schema(required)]
    pub gloss: Option<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextEntryList {
    pub items: Vec<NextEntrySummary>,
    pub pagination: crate::api::PaginationMeta,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSenseView {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sense_group_id: Option<Uuid>,
    pub target_definitions: Vec<NextDefinition>,
    pub medium_definitions: Vec<NextDefinition>,
    pub equivalents: Vec<NextEquivalent>,
    pub missing_target_definitions: bool,
    pub missing_medium_definitions: bool,
    pub missing_equivalents: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextEntryView {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sense_groups: Option<Vec<NextSenseGroup>>,
    pub entry_id: Uuid,
    pub revision: i64,
    pub target_locale: String,
    pub medium_locale: String,
    pub forms: Vec<NextForm>,
    pub missing_forms: bool,
    pub senses: Vec<NextSenseView>,
}

fn non_null_form_editor<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::lexicon::dto::SharedFormsStepContentV3>, D::Error> {
    crate::lexicon::dto::SharedFormsStepContentV3::deserialize(deserializer).map(Some)
}

fn non_null_group_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Uuid>, D::Error> {
    Uuid::deserialize(deserializer).map(Some)
}
fn non_null_sense_groups<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<NextSenseGroup>>, D::Error> {
    Vec::<NextSenseGroup>::deserialize(deserializer).map(Some)
}
