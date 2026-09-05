use std::{
    collections::{BTreeMap, HashMap},
    time::Duration as StdDuration,
};

use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::lexicon::{
    detection_store::{DetectionStore, DetectionStoreError},
    dialect_provider::{DialectSuggestionProvider, DictionaryRegionRulesProvider, evidence_keys},
    dto::{
        AcknowledgedTrue, ActivatePublicationInput, AdminWordAny, AdminWordAnyEnvelope,
        AdminWordDraftV2Envelope, AdminWordListItem, AdminWordListItemAny, AdminWordListItemV3,
        AdminWordListPage, AdminWordListQuery, AdminWordListResponse, AdminWordPublicationAny,
        AdminWordPublicationEnvelope, AdminWordPublicationListResponse, AdminWordPublicationV2,
        AdminWordPublicationV3, AdminWordStats, AdminWordStatus, AdminWordV2, AdminWordV2Envelope,
        AdminWordV3, BuiltinDictionaryResultV2, CreateAdminWordV2Input, DeleteDraftInput,
        DetectWordInputV2, DetectWordResponseV2, DetectionRequestEcho,
        DetectionSurfaceMatchPreviewV2, DetectionSurfaceWarningAuditV2, Dialect, DialectRulesV2,
        DialectSuggestionFieldKind, DialectSuggestionProviderV2, DialectVariantSlotV2,
        DialectVariantSuggestionItemV2, DictionaryCoverageStateV2, DictionaryCoverageV2,
        DictionaryProvenanceV2, DictionaryProviderV2, DraftFormsStepContent,
        DraftFormsStepContentV3, DraftMeaningsStepContent, DraftReferenceLocation,
        DraftValidationIssue, DraftValidationResponse, DuplicateWordMatchV2, EnglishTextV2,
        EntryDeleteBatchResponse, EntryKind, EntryLifecycleBatchInput,
        EntryLifecycleBatchResponseAny, EntryLifecycleInput, EntryLifecycleTarget,
        EntryPresentationV3, EntryReferenceKind, EntryReferencePreview, EntryReferenceSummary,
        ExistingSurfaceMatchV2, ExistingSurfaceSourceV2, FormsImpactItemV2, FormsImpactNodeType,
        FormsImpactResponseV2, GrammarStructureV2, GrammarVariantV2, HeadwordVariant,
        LegacyHeadwordsCompatibilityV3, LexiconSurfaceMatchV2, MatchedEntryContextV2,
        PersistedWordStep, PreviewFormsImpactInputV2, PronunciationStyle, PublishAdminWordV2Input,
        RelatedSearchLegacyResponse, RelatedSearchMatchMode, RelatedSearchQuery,
        RelatedSearchResponse, RelatedSearchV2Response, RelatedWordMatchV3, RelatedWordResult,
        RelatedWordResultAny, RelatedWordResultV3, RelatedWordSense, RelatedWordSenseV3,
        RelationReferenceCountsV2, RelationReferencePreviewV2, RelationReferenceSummaryV2,
        RelationTypeV2, ResolveSentenceTargetsV3Input, ResolveSentenceTargetsV3Response,
        RetiredStableSlotV2, RichText, SaveFormsStepInput, SaveMeaningsStepInput, SenseGroupV2,
        SentenceAssociationOriginV2, SentenceAssociationStateV1, SentenceAssociationsStateV2,
        SentenceSourceRangeV1, SmartDictionaryResultV2, SourceDialect, StepSaveIntent,
        SuggestDialectVariantsInputV2, SuggestDialectVariantsResponseV2, SurfaceAttentionLevelV2,
        SurfaceCanContinueTrue, SurfaceConfirmationReasonV2, SurfaceContentScopeV2,
        SurfaceMatchCandidateV2, SurfaceMatchCategoryV2, SurfaceMatchPageV2, SurfaceMatchPageV3,
        SurfaceMatchSeverityV2, SurfacePolicyBlockCodeV2, SurfacePolicyNameV2, TextOrigin,
        TextVariantV2, ValidateAdminWordV2Input, WordBaseFormSlotV2, WordCreationStep,
        WordDefinitionV2, WordDetectionSnapshotSmartDictionaryV2, WordDetectionSnapshotV2,
        WordEntryKindV3, WordFormGroupV2, WordFormTypeV2, WordFormVariantV2, WordHeadwordsV2,
        WordPosFormsV2, WordPosMeaningsV2, WordPronunciationV2, WordRegionalVariantsV3,
        WordRelationV2, WordSenseV2, WordSenseV3, WordSentenceAssociationV2, WordSentenceLinkV2,
        WordSentenceV2,
    },
    impact_store::{ImpactConfirmation, ImpactStore, ImpactStoreError},
    model::{
        CatalogPartRecord, DictionaryCandidateRecord, EntryRecord, EntryReferenceRow,
        FormsSurfaceAcknowledgementRecord, HeadwordSurfaceAcknowledgementRecord, ListFilter,
        NewPublicationSenseReference, PublicationReadRecord, PublicationSenseReferenceKind,
        PublicationTargetContentScope, RegionSurfaceRecord, RelatedSearchFilter,
        ResolvedRelationTargetRecord, ResolvedSenseTargetRecord, SenseTargetKey,
        SentenceDiscoveryDraftRecord, SentenceDiscoverySurfaceRecord,
    },
    normalization::{
        HeadwordNormalizationError, NormalizedHeadword, normalize_headword, sha256_json,
    },
    provenance::headword_origin,
    repository::{LexiconRepository, LexiconRepositoryError},
    rich_text::canonicalize_meanings,
    surface_policy::{SurfaceCreationPolicy, SurfacePolicyStore, SurfacePolicyStoreError},
    surface_snapshot::{
        CreateSurfaceSnapshot, CreatedSurfaceSnapshot, DEFAULT_SURFACE_PAGE_SIZE,
        ExpectedSurfaceConfirmation, ExpectedSurfaceOwner, SurfaceConfirmationBinding,
        SurfaceConsumptionCommand, SurfaceSnapshotError, SurfaceSnapshotStore,
        VerifiedSurfaceConfirmation, surface_context_digest, surface_owner_bundle_digest,
    },
    validation::{
        ProposedNode, proposed_nodes, validate_forms, validate_meanings, validate_node_identities,
        validate_node_limit, validate_persisted_text,
    },
};

mod dictionary_suggestions;
mod editing;
mod entry;
mod helpers;
mod lifecycle;
mod publishing;
mod queries;
mod sentence_association;
mod sentence_target_discovery;
mod v3;
mod v3_publication;
mod v3_surface;

use editing::*;
pub(crate) use entry::entry_from_record;
use entry::*;
use helpers::*;
use publishing::*;

const DETECTION_TTL: StdDuration = StdDuration::from_secs(5 * 60);
// Keep the serialized detection beyond its logical expiry so create can
// distinguish an expired context (410) from a missing/mismatched one (422).
const DETECTION_RETENTION_TTL: StdDuration = StdDuration::from_secs(65 * 60);
const CREATE_SCOPE: &str = "lexicon.entry.create";
const PUBLISH_SCOPE: &str = "lexicon.entry.publish";
const ACTIVATE_PUBLICATION_SCOPE: &str = "lexicon.publication.activate";
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
    #[error("entry has inbound prebound relations and cannot be deleted")]
    EntryHasInboundPreboundRelations,
    #[error("entry has inbound publication references")]
    EntryHasInboundPublicationRefs(Vec<crate::lexicon::model::InboundSenseReferenceRecord>),
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
    #[error("V3 publication requires a whitelisted migrated entry")]
    V3PublicationRequiresMigrationCanary,
    #[error("step is not reachable")]
    StepNotReachable,
    #[error("draft validation failed")]
    ValidationFailed(Vec<crate::lexicon::dto::DraftValidationIssue>),
    #[error("V3 draft validation failed")]
    ValidationFailedV3(Vec<crate::lexicon::dto::V3DraftValidationIssue>),
    #[error("downstream confirmation is required")]
    DownstreamConfirmationRequired(Vec<Uuid>),
    #[error("surface match acknowledgement is required")]
    SurfaceMatchAcknowledgementRequired(Box<SurfaceMatchPageV2>),
    #[error("V3 surface match acknowledgement is required")]
    SurfaceMatchAcknowledgementRequiredV3(Box<SurfaceMatchPageV3>),
    #[error("surface matches changed")]
    SurfaceMatchesChanged(Box<SurfaceMatchPageV2>),
    #[error("V3 surface matches changed")]
    SurfaceMatchesChangedV3(Box<SurfaceMatchPageV3>),
    #[error("surface matches changed and no replacement snapshot is required")]
    SurfaceMatchesChangedWithoutSnapshot,
    #[error("surface confirmation snapshot expired")]
    SurfaceMatchSnapshotExpired,
    #[error("surface policy changed")]
    SurfacePolicyChanged(SurfaceCreationPolicy),
    #[error("exact headword creation is temporarily disabled")]
    ExactHeadwordCreationTemporarilyDisabled(Box<SurfaceMatchPageV2>),
    #[error("multiple active exact headword publications are not enabled")]
    MultipleActiveExactHeadwordPublicationsNotEnabled(Box<SurfaceMatchPageV2>),
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
    part_codes: std::collections::HashSet<String>,
    part_ids: HashMap<String, Uuid>,
    sub_part_ids: HashMap<String, Uuid>,
    sub_part_parents: HashMap<String, String>,
}
