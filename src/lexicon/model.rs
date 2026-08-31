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

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct RegionSurfaceRecord {
    pub normalized_term: String,
    pub term: String,
    pub region_family: String,
    pub pos: Vec<String>,
    pub targets: Vec<String>,
    pub is_headword: bool,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct RegionEvidenceRecord {
    pub normalized_term: String,
    pub evidence_type: String,
    pub raw_tags: Vec<String>,
    pub pos: String,
    pub targets: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct DictionaryCandidateRecord {
    pub normalized_term: String,
    pub term: String,
    pub region_family: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct DictionaryContentRecord {
    pub normalized_term: String,
    pub pos: String,
    pub senses: Value,
    pub forms: Value,
    pub sounds: Value,
    pub provider_name: String,
    pub provider_version: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct DuplicateRecord {
    pub entry_id: Uuid,
    pub headword: String,
    pub dialect: String,
    pub is_archived: bool,
    pub is_published: bool,
}

/// 词头唯一键上的同名词条。
///
/// `is_archived` 必须跟着回出：归档只写 `entries.archived_at`，
/// `entry_headword_keys` 那行还在，`lexicon_entry_headword_keys_unique_idx`
/// 也建在 keys 表上（`archived_at` 在 entries 表，做不成部分索引），
/// 所以归档词条依然占着词面——既不能当作可绑目标，也不能绕过它另建同名新条。
#[derive(Debug, Clone, Copy, sqlx::FromRow)]
pub(crate) struct HeadwordKeyRecord {
    pub entry_id: Uuid,
    pub is_archived: bool,
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
    pub content_schema_version: i16,
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
    pub source_headword_mode: Option<String>,
    pub source_dialect: Option<String>,
    pub source_common_headword: Option<String>,
    pub source_uk_headword: Option<String>,
    pub source_us_headword: Option<String>,
    pub source_presentation_label: Option<String>,
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

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PublicationReadRecord {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub publication_number: i32,
    pub source_revision: i64,
    pub content_schema_version: i16,
    pub snapshot: Value,
    pub published_by_admin_id: Uuid,
    pub published_at: DateTime<Utc>,
    pub is_current: bool,
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
    pub content_schema_version: i16,
    pub headword_mode: Option<String>,
    pub source_dialect: Option<String>,
    pub common_headword: Option<String>,
    pub uk_headword: Option<String>,
    pub us_headword: Option<String>,
    pub presentation_label: Option<String>,
    pub draft_meanings: Value,
    pub target_publication_id: Option<Uuid>,
    pub published_snapshot: Option<Value>,
    pub published_revision: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationSenseReferenceKind {
    Relation,
    SentenceContext,
    PhraseComponent,
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
            Self::PhraseComponent => "phrase_component",
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
    pub headword_mode: Option<String>,
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
    pub entry_id: Uuid,
    pub kind: String,
    pub content_schema_version: i16,
    pub snapshot: Value,
    pub status: String,
    pub status_rank: i16,
    pub pos_labels: Vec<String>,
    pub sort_headword: String,
    pub total: i64,
}

pub(crate) struct RelatedSearchFilter<'a> {
    pub q: &'a str,
    pub kind: Option<crate::lexicon::dto::EntryKind>,
    pub include_v3: bool,
    pub include_drafts: bool,
    pub exact: bool,
    pub exclude_exact: bool,
    pub limit: i64,
    pub last_kind: Option<crate::lexicon::dto::EntryKind>,
    pub last_headword: Option<&'a str>,
    pub last_status_rank: Option<i16>,
    pub last_word_id: Option<Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ListEntryRecord {
    pub id: Uuid,
    pub content_schema_version: i16,
    pub kind: String,
    pub source_dialect: Option<String>,
    pub dialects: Vec<String>,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub headword_spellings: Vec<String>,
    pub forms: Value,
    pub presentation_label: Option<String>,
    pub presentation_surfaces: Option<Vec<String>>,
    pub presentation_strategy: Option<String>,
    pub gloss: String,
    pub pos_list: Vec<String>,
    pub levels: Vec<String>,
    pub is_published: bool,
    pub published_revision: Option<i64>,
    pub has_unpublished_changes: bool,
    pub is_archived: bool,
    pub completed_steps: Vec<String>,
    pub created_by_name: String,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total: i64,
}

/// 一条「谁引用了谁」的展开行；同一 target 最多 5 行（预览上限），
/// `total` 是该 target 去重后的引用方总数，由窗口函数在同一次查询里算出。
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct EntryReferenceRow {
    pub target_id: Uuid,
    pub source_id: Uuid,
    pub kind: String,
    pub source_headword: String,
    pub source_status: String,
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
    pub include_v3: bool,
}

// --- 例句关联 ---

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct SentenceAssociationRecord {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub sentence_id: Uuid,
    pub source_dialect: String,
    pub association_schema_version: i16,
    pub segment_count: i16,
    pub segments_fingerprint: Option<Vec<u8>>,
    pub source_segments: Value,
    pub range_start: i32,
    pub range_end: i32,
    pub surface: String,
    pub state: String,
    pub target_entry_id: Option<Uuid>,
    pub target_sense_id: Option<Uuid>,
    pub target_form_slot_id: Option<Uuid>,
    pub target_publication_id: Option<Uuid>,
    pub target_form_variant_id: Option<Uuid>,
    pub target_component_usages_snapshot: Option<Value>,
    pub origin: String,
    pub target_headword_snapshot: Option<String>,
    pub target_gloss_snapshot: Option<String>,
    pub resolved_pos: Option<String>,
    pub resolved_form_type: Option<String>,
    pub pending_target_kind: Option<String>,
    pub pending_target_headword: Option<String>,
    pub normalized_pending_target_headword: Option<String>,
    pub pending_target_gloss: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PendingSentenceAssociationListRecord {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub owner_revision: i64,
    pub owner_lifecycle_revision: i64,
    pub sentence_id: Uuid,
    pub source_dialect: String,
    pub association_schema_version: i16,
    pub segment_count: i16,
    pub source_segments: Value,
    pub sentence_text: String,
    pub pending_target_kind: String,
    pub pending_target_headword: String,
    pub pending_target_gloss: Option<String>,
    pub scan_text_hash: Vec<u8>,
    pub scan_resolver_version: i16,
}

/// 某条例句的某一侧正文解析到哪个版本了。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct SentenceAssociationScanRecord {
    pub sentence_id: Uuid,
    pub source_dialect: String,
    pub text_hash: Vec<u8>,
    pub resolver_version: i16,
}

/// 待写入的关联。`source_dialect` / `origin` 用库层的文本形态，与列上的 CHECK 一一对应。
#[derive(Debug, Clone)]
pub(crate) struct NewSentenceAssociation {
    pub id: Uuid,
    pub sentence_id: Uuid,
    pub source_dialect: String,
    pub association_schema_version: i16,
    pub source_segments: Vec<NewSentenceAssociationSegment>,
    pub segments_fingerprint: Option<Vec<u8>>,
    pub range_start: i32,
    pub range_end: i32,
    pub surface: String,
    pub state: String,
    pub target_entry_id: Option<Uuid>,
    pub target_sense_id: Option<Uuid>,
    pub target_form_slot_id: Option<Uuid>,
    pub target_publication_id: Option<Uuid>,
    pub target_form_variant_id: Option<Uuid>,
    pub target_component_usages_snapshot: Option<Value>,
    pub origin: String,
    pub target_headword_snapshot: Option<String>,
    pub target_gloss_snapshot: Option<String>,
    pub resolved_pos: Option<String>,
    pub resolved_form_type: Option<String>,
    pub pending_target_kind: Option<String>,
    pub pending_target_headword: Option<String>,
    pub normalized_pending_target_headword: Option<String>,
    pub pending_target_gloss: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct NewSentenceAssociationSegment {
    pub range_start: i32,
    pub range_end: i32,
    pub surface: String,
}

/// `surface_sources` 里一条当前发布版本的词形行。自动关联的候选就从这里来。
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PublishedFormSurfaceRecord {
    pub normalized_surface: String,
    pub dialect_scope: String,
    pub entry_id: Uuid,
    /// 命中的 `form_variant` 节点。
    pub source_node_id: Uuid,
    pub pos_id: Uuid,
    pub pos: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PublishedEntrySnapshotRecord {
    pub entry_id: Uuid,
    pub publication_id: Uuid,
    pub snapshot: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, sqlx::FromRow)]
pub(crate) struct SentenceDiscoverySurfaceRecord {
    pub normalized_surface: String,
    pub surface: String,
    pub entry_kind: String,
    pub entry_id: Uuid,
    pub publication_id: Uuid,
    pub pos_id: Uuid,
    pub pos: String,
    pub matched_form_id: Uuid,
    pub matched_variant_id: Uuid,
    pub dialect_scope: String,
    pub event_offset: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct SentenceDiscoveryDraftRecord {
    pub entry_id: Uuid,
    pub entry_revision: i64,
    pub headword: String,
}
