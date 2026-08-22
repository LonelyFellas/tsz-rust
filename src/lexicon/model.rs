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
pub(crate) struct DuplicateRecord {
    pub entry_id: Uuid,
    pub headword: String,
    pub dialect: String,
    pub is_archived: bool,
    pub is_published: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SurfaceLookupKey {
    pub dialect_scope: String,
    pub normalized_surface: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SurfaceSourceRecord {
    pub matched_dialect_scope: String,
    pub entry_id: Uuid,
    pub entry_headword: String,
    pub entry_headword_dialect: String,
    pub entry_kind: String,
    pub lifecycle_status: String,
    pub source_id: String,
    pub source_kind: String,
    pub source_node_id: Option<Uuid>,
    pub content_scope: String,
    pub publication_id: Option<Uuid>,
    pub surface: String,
    pub normalized_surface: String,
    pub dialect: String,
    pub normalization_version: i16,
    pub source_revision: i64,
    pub event_offset: i64,
    pub pos_id: Option<Uuid>,
    pub pos: Option<String>,
    pub form_type: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct SurfaceEntryContextRecord {
    pub entry_id: Uuid,
    pub forms: Value,
    pub meanings: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct SurfaceInboundRelationRecord {
    pub target_entry_id: Uuid,
    pub source_entry_id: Uuid,
    pub source_node_id: Uuid,
    pub source_status: String,
    pub source_headword_mode: String,
    pub source_dialect: Option<String>,
    pub source_common_headword: Option<String>,
    pub source_uk_headword: Option<String>,
    pub source_us_headword: Option<String>,
    pub draft_relation_type: Option<String>,
    pub source_snapshot: Option<Value>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct FormsSurfaceAcknowledgementRecord {
    pub entry_id: Uuid,
    pub forms_revision: i64,
    pub forms_content_digest: String,
    pub match_ids: Vec<String>,
    pub match_digest: String,
    pub acknowledged_by_admin_id: Uuid,
    pub acknowledged_at: DateTime<Utc>,
    pub policy_name: String,
    pub policy_epoch: i64,
    pub normalization_version: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct HeadwordSurfaceAcknowledgementRecord {
    pub entry_id: Uuid,
    pub headwords_content_digest: String,
    pub match_ids: Vec<String>,
    pub policy_name: String,
    pub policy_epoch: i64,
    pub normalization_version: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct HistoricalPublicationRecord {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub publication_number: i32,
    pub source_revision: i64,
    pub snapshot: Value,
    pub published_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct IdempotencyRecord {
    pub request_hash: Vec<u8>,
    pub resource_id: Option<Uuid>,
    pub response_status: i16,
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

/// 一个已退役的稳定槽位：`(entry_id, parent_node_id, node_role)` 的唯一索引不带
/// `removed_from_draft_at IS NULL`，所以退役之后这个键仍然永久绑定同一个节点 ID。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct RetiredStableSlotRecord {
    pub id: Uuid,
    pub parent_node_id: Uuid,
    pub node_role: String,
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
    pub target_revision: i64,
    pub snapshot: Value,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ResolvedRelationTargetRecord {
    pub target_entry_id: Uuid,
    pub target_sense_id: Uuid,
    pub target_revision: i64,
    pub target_archived: bool,
    pub target_removed: bool,
    pub headword_mode: String,
    pub source_dialect: Option<String>,
    pub common_headword: Option<String>,
    pub uk_headword: Option<String>,
    pub us_headword: Option<String>,
    pub draft_meanings: Value,
    pub target_publication_id: Option<Uuid>,
    pub published_snapshot: Option<Value>,
    pub published_revision: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationSenseReferenceKind {
    Relation,
    SentenceContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationTargetContentScope {
    Draft,
    Publication,
}

impl PublicationTargetContentScope {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Publication => "publication",
        }
    }
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
    pub target_publication_id: Option<Uuid>,
    pub target_content_scope: PublicationTargetContentScope,
    pub target_revision: i64,
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
    pub pos_labels: Vec<String>,
    pub sort_headword: String,
    pub total: i64,
}

pub(crate) struct RelatedSearchFilter<'a> {
    pub q: &'a str,
    pub kind: Option<crate::lexicon::dto::EntryKind>,
    pub exact: bool,
    pub exclude_exact: bool,
    pub limit: i64,
    pub last_kind: Option<crate::lexicon::dto::EntryKind>,
    pub last_headword: Option<&'a str>,
    pub last_word_id: Option<Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ListEntryRecord {
    pub id: Uuid,
    pub kind: String,
    pub source_dialect: Option<String>,
    pub dialects: Vec<String>,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub headword_spellings: Vec<String>,
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
