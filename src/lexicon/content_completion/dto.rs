use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::lexicon::dto::DraftMeaningsStepContent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentCompletionScope {
    GrammarStructures,
    Meanings,
    Examples,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentCompletionFillPolicy {
    MissingOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentCompletionJobStatus {
    Pending,
    Running,
    Completed,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentCompletionPartitionStatus {
    Pending,
    Running,
    Completed,
    Missing,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateContentCompletionJobInput {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(min_items = 3, max_items = 3)]
    pub scope: Vec<ContentCompletionScope>,
    pub fill_policy: ContentCompletionFillPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryContentCompletionJobInput {
    #[schema(min_items = 1, max_items = 32)]
    pub pos_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContentCompletionDictionaryProvenance {
    pub provider: String,
    pub dataset_version: String,
    pub source_record_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContentCompletionGenerationProvenance {
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentCompletionEvidenceKind {
    DictionaryGroundedTranslation,
    ModelInferred,
    ModelGenerated,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContentCompletionFieldOrigins {
    pub grammar_structures: ContentCompletionEvidenceKind,
    pub meanings: ContentCompletionEvidenceKind,
    pub examples: ContentCompletionEvidenceKind,
    pub cefr: ContentCompletionEvidenceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContentCompletionProvenance {
    pub dictionary: ContentCompletionDictionaryProvenance,
    pub generation: ContentCompletionGenerationProvenance,
    pub field_origins: ContentCompletionFieldOrigins,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContentCompletionPartition {
    pub pos_id: Uuid,
    pub pos: String,
    pub status: ContentCompletionPartitionStatus,
    pub attempt: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ContentCompletionProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContentCompletionJob {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub base_revision: i64,
    pub status: ContentCompletionJobStatus,
    pub requested_scope: Vec<ContentCompletionScope>,
    pub fill_policy: ContentCompletionFillPolicy,
    pub partitions: Vec<ContentCompletionPartition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<DraftMeaningsStepContent>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ContentCompletionJobEnvelope {
    pub job: ContentCompletionJob,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ContentCompletionJobPath {
    pub id: Uuid,
    pub job_id: Uuid,
}
