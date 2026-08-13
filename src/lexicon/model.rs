use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct DictionaryTermRecord {
    pub term: String,
    pub kind: String,
    pub pos: Vec<String>,
    pub region_family: String,
    pub provider_name: String,
    pub provider_version: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct RegionSurfaceRecord {
    pub normalized_term: String,
    pub term: String,
    pub region_family: String,
    pub targets: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct DictionaryCandidateRecord {
    pub normalized_term: String,
    pub term: String,
    pub region_family: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct CatalogPartRecord {
    pub id: Uuid,
    pub code: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct CatalogSubPartRecord {
    pub id: Uuid,
    pub code: String,
    pub part_code: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct DuplicateRecord {
    pub entry_id: Uuid,
    pub headword: String,
    pub dialect: String,
    pub is_archived: bool,
    pub is_published: bool,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct IdempotencyRecord {
    pub request_hash: Vec<u8>,
    pub resource_id: Option<Uuid>,
    pub response_body: Value,
    pub expired: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct NodeIdentityRecord {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub node_type: String,
    pub parent_node_id: Option<Uuid>,
    pub node_role: String,
    pub stable_slot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SenseTargetKey {
    pub target_entry_id: Uuid,
    pub target_sense_id: Uuid,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ResolvedSenseTargetRecord {
    pub target_entry_id: Uuid,
    pub target_sense_id: Uuid,
    pub target_publication_id: Uuid,
    pub snapshot: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationSenseReferenceKind {
    Relation,
    SentenceContext,
}

impl PublicationSenseReferenceKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Relation => "relation",
            Self::SentenceContext => "sentence_context",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NewPublicationSenseReference {
    pub source_node_id: Uuid,
    pub reference_kind: PublicationSenseReferenceKind,
    pub target_entry_id: Uuid,
    pub target_sense_id: Uuid,
    pub target_publication_id: Uuid,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InboundSenseReferenceRecord {
    pub target_sense_id: Uuid,
    pub source_entry_id: Uuid,
    pub source_publication_id: Uuid,
    pub source_node_id: Uuid,
    pub reference_kind: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct EntryRecord {
    pub id: Uuid,
    pub content_schema_version: i16,
    pub language: String,
    pub kind: String,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub headword_mode: String,
    pub source_dialect: Option<String>,
    pub frequency: Option<String>,
    pub detection_snapshot: Value,
    pub current_publication_id: Option<Uuid>,
    pub current_publication_source_revision: Option<i64>,
    pub current_published_at: Option<DateTime<Utc>>,
    pub common_headword: Option<String>,
    pub uk_headword: Option<String>,
    pub us_headword: Option<String>,
    pub forms: Value,
    pub meanings: Value,
    pub completed_steps: Vec<String>,
    pub created_by_admin_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub archived_by_admin_id: Option<Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct RelatedSearchRecord {
    pub snapshot: Value,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ListEntryRecord {
    pub id: Uuid,
    pub kind: String,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub headword: String,
    pub gloss: String,
    pub pos_list: Vec<String>,
    pub levels: Vec<String>,
    pub is_published: bool,
    pub published_revision: Option<i64>,
    pub has_unpublished_changes: bool,
    pub is_archived: bool,
    pub completed_steps: Vec<String>,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct StatsRecord {
    pub total: i64,
    pub today: i64,
    pub month: i64,
}

#[derive(Debug)]
pub(crate) struct ListFilter {
    pub q: Option<String>,
    pub gloss: Option<String>,
    pub kind: Option<String>,
    pub pos: Option<String>,
    pub level: Option<String>,
    pub status: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub limit: i64,
    pub offset: i64,
}
