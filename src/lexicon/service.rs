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
        AcknowledgedTrue, ActivatePublicationInput, AdminWordDraftV2Envelope, AdminWordListItem,
        AdminWordListPage, AdminWordListQuery, AdminWordListResponse, AdminWordStats,
        AdminWordStatus, AdminWordV2, AdminWordV2Envelope, BuiltinDictionaryResultV2,
        CreateAdminWordV2Input, DeleteDraftInput, DetectWordInputV2, DetectWordResponseV2,
        DetectionRequestEcho, DetectionSurfaceMatchPreviewV2, DetectionSurfaceWarningAuditV2,
        Dialect, DialectRulesV2, DialectSuggestionFieldKind, DialectSuggestionProviderV2,
        DialectVariantSlotV2, DialectVariantSuggestionItemV2, DictionaryCoverageStateV2,
        DictionaryCoverageV2, DictionaryProvenanceV2, DictionaryProviderV2, DraftFormsStepContent,
        DraftMeaningsStepContent, DraftReferenceLocation, DraftValidationIssue,
        DraftValidationResponse, DuplicateWordMatchV2, EnglishTextV2, EntryKind,
        EntryLifecycleBatchInput, EntryLifecycleBatchResponse, EntryLifecycleInput,
        EntryLifecycleTarget, ExistingSurfaceMatchV2, ExistingSurfaceSourceV2, FormsImpactItemV2,
        FormsImpactNodeType, FormsImpactResponseV2, GrammarStructureV2, GrammarVariantV2,
        HeadwordVariant, LexiconSurfaceMatchV2, MatchedEntryContextV2, PersistedWordStep,
        PreviewFormsImpactInputV2, PronunciationStyle, PublishAdminWordV2Input,
        RelatedSearchLegacyResponse, RelatedSearchMatchMode, RelatedSearchQuery,
        RelatedSearchResponse, RelatedSearchV2Response, RelatedWordResult, RelatedWordSense,
        RelationReferenceCountsV2, RelationReferencePreviewV2, RelationReferenceSummaryV2,
        RelationTypeV2, ReplaceSentenceAssociationsInput, RetiredStableSlotV2, RichText,
        SaveFormsStepInput, SaveMeaningsStepInput, SenseGroupV2, SentenceAssociationInputV2,
        SentenceAssociationOriginV2, SentenceAssociationsStateV2, SentenceSourceRangeV1,
        SmartDictionaryResultV2, SourceDialect, StepSaveIntent, SuggestDialectVariantsInputV2,
        SuggestDialectVariantsResponseV2, SurfaceAttentionLevelV2, SurfaceCanContinueTrue,
        SurfaceConfirmationReasonV2, SurfaceContentScopeV2, SurfaceMatchCandidateV2,
        SurfaceMatchCategoryV2, SurfaceMatchPageV2, SurfaceMatchSeverityV2,
        SurfacePolicyBlockCodeV2, SurfacePolicyNameV2, TextOrigin, TextVariantV2,
        ValidateAdminWordV2Input, WordBaseFormSlotV2, WordCreationStep, WordDefinitionV2,
        WordDetectionSnapshotSmartDictionaryV2, WordDetectionSnapshotV2, WordFormGroupV2,
        WordFormTypeV2, WordFormVariantV2, WordHeadwordsV2, WordPosFormsV2, WordPosMeaningsV2,
        WordPronunciationV2, WordRelationV2, WordSenseV2, WordSentenceAssociationV2,
        WordSentenceLinkV2, WordSentenceV2,
    },
    impact_store::{ImpactConfirmation, ImpactStore, ImpactStoreError},
    model::{
        CatalogPartRecord, DictionaryCandidateRecord, EntryRecord,
        FormsSurfaceAcknowledgementRecord, HeadwordSurfaceAcknowledgementRecord, ListFilter,
        NewPublicationSenseReference, PublicationSenseReferenceKind, PublicationTargetContentScope,
        RegionSurfaceRecord, RelatedSearchFilter, ResolvedRelationTargetRecord,
        ResolvedSenseTargetRecord, SenseTargetKey,
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

mod editing;
mod entry;
mod helpers;
mod lifecycle;
mod publishing;
mod queries;
mod sentence_association;

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
    #[error("entry has inbound publication references")]
    EntryHasInboundPublicationRefs(Vec<crate::lexicon::model::InboundSenseReferenceRecord>),
    #[error("entry has unavailable outbound publication references")]
    EntryHasUnavailablePublicationRefs(Vec<crate::lexicon::model::InboundSenseReferenceRecord>),
    #[error("a referenced publication is changing")]
    ReferenceConflict,
    #[error("step is not reachable")]
    StepNotReachable,
    #[error("draft validation failed")]
    ValidationFailed(Vec<crate::lexicon::dto::DraftValidationIssue>),
    #[error("downstream confirmation is required")]
    DownstreamConfirmationRequired(Vec<Uuid>),
    #[error("surface match acknowledgement is required")]
    SurfaceMatchAcknowledgementRequired(Box<SurfaceMatchPageV2>),
    #[error("surface matches changed")]
    SurfaceMatchesChanged(Box<SurfaceMatchPageV2>),
    #[error("surface confirmation snapshot expired")]
    SurfaceMatchSnapshotExpired,
    #[error("surface policy changed")]
    SurfacePolicyChanged(SurfaceCreationPolicy),
    #[error("exact headword creation is temporarily disabled")]
    ExactHeadwordCreationTemporarilyDisabled(Box<SurfaceMatchPageV2>),
    #[error("multiple active exact headword publications are not enabled")]
    MultipleActiveExactHeadwordPublicationsNotEnabled(Box<SurfaceMatchPageV2>),
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
