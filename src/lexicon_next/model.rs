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
    /// Language-specific code; null is an incomplete draft, not an inferred POS.
    #[schema(required)]
    pub part_of_speech: Option<String>,
    pub definitions: Vec<NextDefinition>,
    pub equivalents: Vec<NextEquivalent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextDraft {
    #[schema(minimum = 1, maximum = 1)]
    pub schema_version: i32,
    pub headword: String,
    pub forms: Vec<NextForm>,
    pub senses: Vec<NextSense>,
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
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct NextViewQuery {
    pub target_locale: String,
    pub medium_locale: String,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NextSenseView {
    pub id: Uuid,
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
    pub entry_id: Uuid,
    pub revision: i64,
    pub target_locale: String,
    pub medium_locale: String,
    pub forms: Vec<NextForm>,
    pub missing_forms: bool,
    pub senses: Vec<NextSenseView>,
}
