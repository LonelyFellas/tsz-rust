use std::{collections::HashMap, time::Duration as StdDuration};

use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::lexicon::{
    detection_store::{DetectionStore, DetectionStoreError},
    dialect_provider::{DialectSuggestionProvider, DictionaryRegionRulesProvider, evidence_keys},
    dto::{
        AdminWordListItem, AdminWordListPage, AdminWordListQuery, AdminWordListResponse,
        AdminWordStats, AdminWordStatus, AdminWordV2, AdminWordV2Envelope,
        BuiltinDictionaryResultV2, CreateAdminWordV2Input, DetectWordInputV2, DetectWordResponseV2,
        DetectionRequestEcho, Dialect, DialectRulesV2, DialectSuggestionFieldKind,
        DialectSuggestionProviderV2, DialectVariantSlotV2, DialectVariantSuggestionItemV2,
        DraftFormsStepContent, DraftMeaningsStepContent, DraftReferenceLocation,
        DraftValidationIssue, DraftValidationResponse, DuplicateWordMatchV2, EnglishTextV2,
        EntryKind, EntryLifecycleBatchInput, EntryLifecycleBatchResponse, EntryLifecycleInput,
        EntryLifecycleTarget, FormsImpactItemV2, FormsImpactResponseV2, GrammarStructureV2,
        GrammarVariantV2, PersistedWordStep, PreviewFormsImpactInputV2, PronunciationStyle,
        PublishAdminWordV2Input, RelatedSearchQuery, RelatedSearchResponse, RelatedWordResult,
        RelatedWordSense, RichText, SaveFormsStepInput, SaveMeaningsStepInput, SenseGroupV2,
        SmartDictionaryResultV2, SourceDialect, StepSaveIntent, SuggestDialectVariantsInputV2,
        SuggestDialectVariantsResponseV2, TextOrigin, TextVariantV2, ValidateAdminWordV2Input,
        WordBaseFormSlotV2, WordCreationStep, WordDefinitionV2, WordFormGroupV2, WordFormVariantV2,
        WordHeadwordsV2, WordPosFormsV2, WordPosMeaningsV2, WordPronunciationV2, WordSenseV2,
        WordSentenceLinkV2, WordSentenceV2,
    },
    impact_store::{ImpactConfirmation, ImpactStore, ImpactStoreError},
    model::{
        CatalogPartRecord, EntryRecord, ListFilter, NewPublicationSenseReference,
        PublicationSenseReferenceKind, RegionSurfaceRecord, ResolvedSenseTargetRecord,
        SenseTargetKey,
    },
    normalization::{HeadwordNormalizationError, normalize_headword, sha256_json},
    repository::{LexiconRepository, LexiconRepositoryError},
    rich_text::canonicalize_meanings,
    validation::{
        proposed_nodes, validate_forms, validate_meanings, validate_node_identities,
        validate_persisted_text,
    },
};

mod editing;
mod entry;
mod helpers;
mod lifecycle;
mod publishing;
mod queries;

use editing::*;
use entry::*;
use helpers::*;
use publishing::*;

const DETECTION_TTL: StdDuration = StdDuration::from_secs(5 * 60);
// Keep the serialized detection beyond its logical expiry so create can
// distinguish an expired context (410) from a missing/mismatched one (422).
const DETECTION_RETENTION_TTL: StdDuration = StdDuration::from_secs(65 * 60);
const CREATE_SCOPE: &str = "lexicon.entry.create";
const PUBLISH_SCOPE: &str = "lexicon.entry.publish";
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
    #[error("configured part of speech disappeared")]
    CatalogMismatch,
    #[error("entry revision conflict")]
    RevisionConflict { current_revision: i64 },
    #[error("entry lifecycle revision conflict")]
    LifecycleRevisionConflict { current_lifecycle_revision: i64 },
    #[error("entry is archived")]
    EntryArchived,
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
}

impl LexiconService {
    pub fn new(
        repository: LexiconRepository,
        detections: DetectionStore,
        impacts: ImpactStore,
    ) -> Self {
        Self {
            repository,
            detections,
            impacts,
        }
    }
}

struct CatalogContext {
    part_codes: std::collections::HashSet<String>,
    part_ids: HashMap<String, Uuid>,
    sub_part_ids: HashMap<String, Uuid>,
    sub_part_parents: HashMap<String, String>,
}
