use std::{
    collections::{BTreeMap, HashMap},
    time::Duration as StdDuration,
};

use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::lexicon::{
    detection_store::{DetectionStore, DetectionStoreError},
    dto::{
        AdminWordListItemV3, AdminWordListPage, AdminWordListQuery, AdminWordListResponse,
        AdminWordPublicationEnvelope, AdminWordPublicationListResponse, AdminWordPublicationV3,
        AdminWordStats, AdminWordStatus, AdminWordV3, AdminWordV3Envelope, DeleteDraftInput,
        Dialect, DialectRulesV2, DialectVariantSlotV2, DictionaryCoverageStateV2,
        DraftFormsStepContent, DraftFormsStepContentV3, DraftMeaningsStepContent,
        DraftReferenceLocation, DraftValidationIssue, EnglishTextV2, EntryDeleteBatchResponse,
        EntryKind, EntryLifecycleBatchInput, EntryLifecycleBatchResponse, EntryLifecycleInput,
        EntryLifecycleTarget, EntryPresentationV3, EntryReferenceKind, EntryReferencePreview,
        EntryReferenceSummary, LexiconSurfaceMatchV2, MatchedEntryContextV2, PersistedWordStep,
        RelatedSearchLegacyResponse, RelatedSearchMatchMode, RelatedSearchQuery,
        RelatedSearchResponse, RelatedSearchV2Response, RelatedWordMatchV3, RelatedWordResultV3,
        RelatedWordSenseV3, RelationReferencePreviewV2, RelationTypeV2,
        ResolveSentenceTargetsV3Input, ResolveSentenceTargetsV3Response,
        SentenceAssociationOriginV2, SentenceAssociationStateV1, SentenceAssociationsStateV2,
        SentenceSourceRangeV1, SourceDialect, StepSaveIntent, SurfaceConfirmationReasonV2,
        SurfaceMatchPageV3, SurfacePolicyBlockCodeV2, SurfacePolicyNameV2, WordBaseFormSlotV2,
        WordCreationStep, WordDefinitionV2, WordEntryKindV3, WordFormGroupV2, WordHeadwordsV2,
        WordPosFormsV2, WordRegionalVariantsV3, WordRelationV2, WordSenseV2, WordSenseV3,
    },
    impact_store::{ImpactConfirmation, ImpactStore, ImpactStoreError},
    model::{
        DictionaryCandidateRecord, EntryRecord, EntryReferenceRow,
        FormsSurfaceAcknowledgementRecord, ListFilter, NewPublicationSenseReference,
        PublicationReadRecord, PublicationSenseReferenceKind, PublicationTargetContentScope,
        RegionSurfaceRecord, RelatedSearchFilter, ResolvedRelationTargetRecord,
        ResolvedSenseTargetRecord, SenseTargetKey, SentenceDiscoveryDraftRecord,
        SentenceDiscoverySurfaceRecord,
    },
    normalization::{
        HeadwordNormalizationError, NormalizedHeadword, normalize_headword, sha256_json,
    },
    repository::{LexiconRepository, LexiconRepositoryError},
    rich_text::canonicalize_meanings,
    surface_policy::{SurfaceCreationPolicy, SurfacePolicyStore, SurfacePolicyStoreError},
    surface_snapshot::{
        CreateSurfaceSnapshot, DEFAULT_SURFACE_PAGE_SIZE, ExpectedSurfaceConfirmation,
        ExpectedSurfaceOwner, SurfaceConfirmationBinding, SurfaceConsumptionCommand,
        SurfaceSnapshotError, SurfaceSnapshotStore, VerifiedSurfaceConfirmation,
        surface_context_digest, surface_owner_bundle_digest,
    },
    validation::{ProposedNode, proposed_nodes, validate_meanings, validate_node_identities},
};

mod annotations;
mod dictionary_suggestions;
mod editing;
mod entry;
mod helpers;
mod lifecycle;
mod publishing;
mod queries;
mod sentence_association;
mod sentence_target_discovery;
pub(crate) mod text_links;
mod v3;
mod v3_publication;
mod v3_surface;

use editing::*;
use entry::*;
use helpers::*;
use publishing::*;

const IMPACT_TTL: StdDuration = StdDuration::from_secs(10 * 60);

#[derive(Debug, thiserror::Error)]
pub enum LexiconServiceError {
    #[error("invalid field {field}: {message}")]
    InvalidField {
        field: &'static str,
        message: &'static str,
    },
    #[error("unprocessable field {field}: {message}")]
    UnprocessableField {
        field: &'static str,
        message: &'static str,
    },
    #[error("unsupported language")]
    UnsupportedLanguage,
    #[error("unsupported lexicon schema version {0}")]
    UnsupportedSchemaVersion(i16),
    #[error("Smart Lexicon V3 storage or projection capability is disabled")]
    V3StorageUnavailable,
    #[error("detection does not exist")]
    DetectionMismatch,
    #[error("detection expired")]
    DetectionExpired,
    #[error("headword already exists")]
    DuplicateWord,
    #[error("an unfinished draft already exists")]
    ExistingEmptyDraft(Uuid),
    #[error("idempotency key was reused with a different request")]
    IdempotencyConflict,
    #[error("word not found")]
    WordNotFound,
    #[error("sentence not found")]
    SentenceNotFound,
    #[error("sentence associations are not resolved for the current text")]
    SentenceAssociationsUnresolved,
    #[error("publication not found")]
    PublicationNotFound,
    #[error("configured part of speech disappeared")]
    CatalogMismatch,
    #[error("entry annotation conflict")]
    AnnotationConflict(Box<crate::lexicon::dto::EntryAnnotationConflict>),
    #[error("entry revision conflict")]
    RevisionConflict { current_revision: i64 },
    #[error("entry lifecycle revision conflict")]
    LifecycleRevisionConflict { current_lifecycle_revision: i64 },
    #[error("entry is archived")]
    EntryArchived,
    #[error("entry has publication history or inbound references and cannot be deleted")]
    EntryNotDeletable,
    #[error("entry can only be deleted by its creator")]
    EntryDeleteForbidden,
    #[error("unpublished draft can only be edited by its creator")]
    EntryEditForbidden,
    #[error("entry annotation can only be edited by its creator")]
    EntryAnnotationForbidden,
    #[error("entry has inbound prebound relations and cannot be deleted")]
    EntryHasInboundPreboundRelations,
    #[error("entry has inbound publication references")]
    EntryHasInboundPublicationRefs(Vec<crate::lexicon::model::InboundSenseReferenceRecord>),
    #[error("entry has active shared sentence annotations and cannot be archived")]
    EntryHasInboundSharedSentenceRefs,
    #[error("entry has unavailable outbound publication references")]
    EntryHasUnavailablePublicationRefs(Vec<crate::lexicon::model::InboundSenseReferenceRecord>),
    #[error("a referenced publication is changing")]
    ReferenceConflict,
    #[error("relation prebinding reconciliation fanout exceeds 500 eligible relations")]
    RelationPrebindingFanoutExceeded,
    #[error("a stable V3 node identity changed")]
    StableNodeIdChanged,
    #[error("a V3 form operation would break an existing reference")]
    FormReferenceConflict,
    #[error("step is not reachable")]
    StepNotReachable,
    #[error("V3 draft validation failed")]
    ValidationFailedV3(Vec<crate::lexicon::dto::V3DraftValidationIssue>),
    #[error("downstream confirmation is required")]
    DownstreamConfirmationRequired(Vec<Uuid>),
    #[error("V3 surface match acknowledgement is required")]
    SurfaceMatchAcknowledgementRequiredV3(Box<SurfaceMatchPageV3>),
    #[error("V3 surface matches changed")]
    SurfaceMatchesChangedV3(Box<SurfaceMatchPageV3>),
    #[error("surface matches changed and no replacement snapshot is required")]
    SurfaceMatchesChangedWithoutSnapshot,
    #[error("surface confirmation snapshot expired")]
    SurfaceMatchSnapshotExpired,
    #[error("surface policy changed")]
    SurfacePolicyChanged(SurfaceCreationPolicy),
    #[error("multiple active exact headword publications are not enabled for a V3 command")]
    MultipleActiveExactHeadwordPublicationsNotEnabledV3(Box<SurfaceMatchPageV3>),
    #[error("surface snapshot store failed")]
    SurfaceSnapshot(#[source] SurfaceSnapshotError),
    #[error("surface policy store failed")]
    SurfacePolicy(#[source] SurfacePolicyStoreError),
    #[error("detection store failed")]
    DetectionStore(#[source] DetectionStoreError),
    #[error("impact confirmation store failed")]
    ImpactStore(#[source] ImpactStoreError),
    #[error("lexicon repository failed")]
    Repository(#[source] LexiconRepositoryError),
}

fn v3_validation_failed(
    issues: Vec<crate::lexicon::dto::DraftValidationIssue>,
) -> LexiconServiceError {
    LexiconServiceError::ValidationFailedV3(crate::lexicon::v3_contract::v3_issues(&issues))
}

fn v3_meaning_validation_forms(forms: &DraftFormsStepContentV3) -> DraftFormsStepContent {
    DraftFormsStepContent {
        pos: forms
            .pos
            .iter()
            .map(|pos| WordPosFormsV2 {
                pos_id: pos.pos_id,
                pos: pos.pos.clone(),
                dialect_rules: DialectRulesV2 {
                    spelling_mode: "distinguish".to_owned(),
                    phonetic_mode: "distinguish".to_owned(),
                },
                // V3 meanings are POS-owned. This adapter exists only to reuse the
                // established meanings validator and is never persisted or exposed.
                base_form: WordBaseFormSlotV2 {
                    id: Uuid::nil(),
                    form_type: "base".to_owned(),
                    variants: Vec::new(),
                },
                form_groups: Vec::new(),
            })
            .collect(),
    }
}

fn v3_meaning_validation_headwords() -> WordHeadwordsV2 {
    // Distinguish mode accepts either a common grammar variant or a complete
    // UK/US pair. The placeholder is validation-only and never becomes a V3
    // identity, presentation label, surface, or stored compatibility value.
    WordHeadwordsV2::Distinguish {
        uk: "v3".to_owned(),
        us: "v3".to_owned(),
        source_dialect: SourceDialect::Uk,
    }
}

pub struct LexiconService {
    repository: LexiconRepository,
    detections: DetectionStore,
    impacts: ImpactStore,
    surface_snapshots: SurfaceSnapshotStore,
    surface_policies: SurfacePolicyStore,
    related_search_cursor_key: std::sync::Arc<[u8]>,
}

impl LexiconService {
    pub fn new(
        repository: LexiconRepository,
        detections: DetectionStore,
        impacts: ImpactStore,
        surface_snapshots: SurfaceSnapshotStore,
        surface_policies: SurfacePolicyStore,
        related_search_cursor_key: std::sync::Arc<[u8]>,
    ) -> Self {
        Self {
            repository,
            detections,
            impacts,
            surface_snapshots,
            surface_policies,
            related_search_cursor_key,
        }
    }
}

struct CatalogContext {
    sub_part_ids: HashMap<String, Uuid>,
    sub_part_parents: HashMap<String, String>,
}
