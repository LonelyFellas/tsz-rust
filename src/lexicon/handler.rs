use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde_json::Value;

use crate::{
    admin::{AdminAuth, authorization::require_active_admin},
    api::{ApiJson, ApiPath, ApiQuery},
    config::SmartLexiconV3Flags,
    error::{AppError, ErrorCode, ProblemMeta},
    lexicon::{
        detection_store::DetectionStore,
        dto::{
            ActivatePublicationAnyInput, ActivatePublicationInput, ActivatePublicationV3Input,
            AdminWordAny, AdminWordAnyEnvelope, AdminWordDraftAnyEnvelope, AdminWordListQuery,
            AdminWordListResponse, AdminWordPublicationAny, AdminWordPublicationEnvelope,
            AdminWordPublicationListResponse, AdminWordStats, ClaimPendingSentenceAssociationInput,
            CreateAdminWordAnyInput, CreateAdminWordV2Input, CreateAdminWordV3Input,
            DeleteDraftInput, DetectLexiconInputAny, DetectLexiconResponseAny,
            DetectLexiconSurfaceV3Input, DetectWordInputV2, DraftValidationResponseAny,
            EntryLifecycleBatchInput, EntryLifecycleBatchResponseAny, EntryLifecycleInput,
            EntryPath, FormsImpactResponseAny, PendingSentenceAssociationListQuery,
            PendingSentenceAssociationListResponse, PendingSentenceAssociationPath,
            PreviewFormsImpactInputAny, PreviewFormsImpactInputV2, PreviewFormsImpactInputV3,
            PublicationPath, PublishAdminWordAnyInput, PublishAdminWordV2Input,
            PublishAdminWordV3Input, RelatedSearchQuery, RelatedSearchResponse,
            ReplaceSentenceAssociationsInput, ResolveSentenceTargetsV3Input,
            ResolveSentenceTargetsV3Response, SaveFormsStepInput, SaveFormsStepInputAny,
            SaveFormsStepInputV3, SaveMeaningsStepInput, SaveMeaningsStepInputAny,
            SaveMeaningsStepInputV3, SentencePath, StepSaveIntent, SuggestDialectVariantsInputV2,
            SuggestDialectVariantsResponseV2, SurfaceMatchPageAny, SurfaceMatchSnapshotPathV2,
            SurfaceMatchSnapshotQueryV2, ValidateAdminWordAnyInput, ValidateAdminWordV2Input,
            ValidateAdminWordV3Input,
        },
        impact_store::ImpactStore,
        repository::LexiconRepository,
        service::{LexiconService, LexiconServiceError},
        surface_policy::SurfacePolicyStore,
        surface_snapshot::SurfaceSnapshotStore,
        v3_contract,
    },
    request_id::RequestId,
    state::AppState,
};

pub(crate) mod commands;
pub(crate) mod lifecycle;
pub(crate) mod query;

pub use commands::{
    activate_publication, claim_pending_sentence_association, create, detect, preview_forms_impact,
    publish, replace_sentence_associations, save_forms, save_meanings, suggest_dialect_variants,
    validate,
};
pub use lifecycle::{archive, archive_batch, delete_draft, restore, restore_batch};
pub use query::{
    get, get_publication, list, list_pending_sentence_associations, list_publications,
    related_search, resolve_sentence_targets, stats, surface_match_snapshot_page,
};

fn service(state: &AppState) -> LexiconService {
    LexiconService::new(
        LexiconRepository::new(state.pool.clone()),
        DetectionStore::new(state.redis.clone()),
        ImpactStore::new(state.redis.clone()),
        SurfaceSnapshotStore::with_policy_prefix(
            state.redis.clone(),
            state.surface_policy_prefix.clone(),
        ),
        SurfacePolicyStore::with_prefix(state.redis.clone(), state.surface_policy_prefix.clone()),
        state.related_search_cursor_key.clone(),
    )
}

pub(crate) fn required_idempotency_key(headers: &HeaderMap) -> Result<uuid::Uuid, &'static str> {
    let value = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .ok_or("Idempotency-Key header is required")?;
    uuid::Uuid::parse_str(value).map_err(|_| "Idempotency-Key header must be a UUID")
}

pub(crate) fn idempotency_key_error(message: &'static str) -> AppError {
    AppError::validation(ErrorCode::InvalidRequestBody, "idempotency_key", message)
}

fn v3_storage_unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::SmartLexiconV3StorageUnavailable,
        "Smart Lexicon V3 storage or projection capability is disabled",
    )
}

fn v3_detection_unavailable() -> AppError {
    AppError::unavailable(
        ErrorCode::SmartLexiconV3DetectionUnavailable,
        "Smart Lexicon V3 form-surface detection requires the C2 projection capability",
    )
}

fn v3_publication_requires_migration_canary() -> AppError {
    AppError::conflict(
        ErrorCode::SmartLexiconV3PublicationRequiresMigrationCanary,
        None,
        "Phase 1 only permits server-whitelisted migrated V3 entries to publish",
    )
}

fn apply_legacy_bridge_read_flag(response: &mut AdminWordAnyEnvelope, enabled: bool) {
    if !enabled && let AdminWordAny::V3(word) = &mut response.word {
        word.compatibility = None;
    }
}

fn sentence_association_enabled(flags: SmartLexiconV3Flags) -> bool {
    flags.read && flags.edit && flags.projection && flags.sentence_associations
}

fn sentence_target_discovery_enabled(flags: SmartLexiconV3Flags) -> bool {
    sentence_association_enabled(flags) && flags.sentence_target_discovery
}

fn draft_relation_prebinding_enabled(flags: SmartLexiconV3Flags) -> bool {
    flags.read && flags.edit && flags.projection && flags.draft_relation_prebinding
}

fn apply_sentence_association_flag(
    response: &mut AdminWordAnyEnvelope,
    associations_enabled: bool,
    discovery_enabled: bool,
    relation_prebinding_enabled: bool,
) {
    if let AdminWordAny::V3(word) = &mut response.word {
        word.capabilities.sentence_associations = Some(associations_enabled);
        word.capabilities.sentence_target_discovery = Some(discovery_enabled);
        word.capabilities.draft_relation_prebinding = Some(relation_prebinding_enabled);
    }
}

fn apply_lifecycle_batch_legacy_bridge_read_flag(
    response: &mut EntryLifecycleBatchResponseAny,
    enabled: bool,
) {
    if enabled {
        return;
    }
    for word in &mut response.words {
        if let AdminWordAny::V3(word) = word {
            word.compatibility = None;
        }
    }
}

fn apply_draft_legacy_bridge_read_flag(response: &mut AdminWordDraftAnyEnvelope, enabled: bool) {
    if !enabled && let AdminWordDraftAnyEnvelope::V3(envelope) = response {
        envelope.word.compatibility = None;
    }
}

fn apply_draft_sentence_association_flag(
    response: &mut AdminWordDraftAnyEnvelope,
    associations_enabled: bool,
    discovery_enabled: bool,
    relation_prebinding_enabled: bool,
) {
    if let AdminWordDraftAnyEnvelope::V3(envelope) = response {
        envelope.word.capabilities.sentence_associations = Some(associations_enabled);
        envelope.word.capabilities.sentence_target_discovery = Some(discovery_enabled);
        envelope.word.capabilities.draft_relation_prebinding = Some(relation_prebinding_enabled);
    }
}

fn apply_publication_sentence_association_flag(
    publication: &mut AdminWordPublicationAny,
    associations_enabled: bool,
    discovery_enabled: bool,
    relation_prebinding_enabled: bool,
) {
    if let AdminWordPublicationAny::V3(publication) = publication {
        publication.word.capabilities.sentence_associations = Some(associations_enabled);
        publication.word.capabilities.sentence_target_discovery = Some(discovery_enabled);
        publication.word.capabilities.draft_relation_prebinding = Some(relation_prebinding_enabled);
    }
}

fn apply_publication_legacy_bridge_read_flag(
    publication: &mut AdminWordPublicationAny,
    enabled: bool,
) {
    if !enabled && let AdminWordPublicationAny::V3(publication) = publication {
        publication.word.compatibility = None;
    }
}

fn map_error(error: LexiconServiceError) -> AppError {
    match error {
        LexiconServiceError::InvalidField { field, message } => {
            let code = if matches!(field, "headword" | "surface") {
                ErrorCode::InvalidHeadword
            } else if matches!(
                field,
                "page"
                    | "page_size"
                    | "limit"
                    | "cursor"
                    | "q"
                    | "gloss"
                    | "pos"
                    | "level"
                    | "created_to"
            ) {
                ErrorCode::InvalidQuery
            } else {
                ErrorCode::InvalidRequestBody
            };
            AppError::validation(code, field, message)
        }
        LexiconServiceError::UnprocessableField { field, message } => {
            AppError::unprocessable(ErrorCode::ValidationFailed, message).with_meta(ProblemMeta {
                code: Some(field.to_owned()),
                ..ProblemMeta::default()
            })
        }
        LexiconServiceError::UnsupportedLanguage => {
            AppError::unprocessable(ErrorCode::UnsupportedLanguage, "unsupported language")
        }
        LexiconServiceError::UnsupportedSchemaVersion(_) => AppError::unprocessable(
            ErrorCode::UnsupportedSchemaVersion,
            "unsupported lexicon schema version",
        ),
        LexiconServiceError::V3StorageUnavailable => v3_storage_unavailable(),
        LexiconServiceError::DetectionMismatch => AppError::unprocessable(
            ErrorCode::DetectionMismatch,
            "detection does not match create request",
        ),
        LexiconServiceError::DetectionExpired => {
            AppError::gone(ErrorCode::DetectionExpired, "detection expired")
        }
        LexiconServiceError::SurfaceMatchAcknowledgementRequired(page) => AppError::conflict(
            ErrorCode::SurfaceMatchAcknowledgementRequired,
            None,
            "surface match acknowledgement is required",
        )
        .with_meta(ProblemMeta {
            surface_match_page: Some(SurfaceMatchPageAny::V2(*page)),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::SurfaceMatchAcknowledgementRequiredV3(page) => AppError::conflict(
            ErrorCode::SurfaceMatchAcknowledgementRequired,
            None,
            "surface match acknowledgement is required",
        )
        .with_meta(ProblemMeta {
            surface_match_page: Some(SurfaceMatchPageAny::V3(*page)),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::SurfaceMatchesChanged(page) => AppError::conflict(
            ErrorCode::SurfaceMatchesChanged,
            None,
            "surface matches changed since confirmation",
        )
        .with_meta(ProblemMeta {
            surface_match_page: Some(SurfaceMatchPageAny::V2(*page)),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::SurfaceMatchesChangedV3(page) => AppError::conflict(
            ErrorCode::SurfaceMatchesChanged,
            None,
            "surface matches changed since confirmation",
        )
        .with_meta(ProblemMeta {
            surface_match_page: Some(SurfaceMatchPageAny::V3(*page)),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::SurfaceMatchesChangedWithoutSnapshot => AppError::conflict(
            ErrorCode::SurfaceMatchesChanged,
            None,
            "surface matches changed since confirmation; retry without the stale token",
        ),
        LexiconServiceError::SurfaceMatchSnapshotExpired => AppError::gone(
            ErrorCode::SurfaceMatchSnapshotExpired,
            "surface confirmation snapshot expired",
        ),
        LexiconServiceError::SurfacePolicyChanged(policy) => AppError::conflict(
            ErrorCode::SurfacePolicyChanged,
            None,
            "surface policy changed since confirmation",
        )
        .with_meta(ProblemMeta {
            current_policy_name: Some(policy.name),
            current_policy_epoch: Some(policy.epoch),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::ExactHeadwordCreationTemporarilyDisabled(page) => AppError::conflict(
            ErrorCode::ExactHeadwordCreationTemporarilyDisabled,
            None,
            "exact headword creation is temporarily disabled",
        )
        .with_meta(ProblemMeta {
            surface_match_page: Some(SurfaceMatchPageAny::V2(*page)),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabled(page) => {
            AppError::conflict(
                ErrorCode::MultipleActiveExactHeadwordPublicationsNotEnabled,
                None,
                "multiple active exact headword publications are not enabled",
            )
            .with_meta(ProblemMeta {
                surface_match_page: Some(SurfaceMatchPageAny::V2(*page)),
                ..ProblemMeta::default()
            })
        }
        LexiconServiceError::MultipleActiveExactHeadwordPublicationsNotEnabledV3(page) => {
            AppError::conflict(
                ErrorCode::MultipleActiveExactHeadwordPublicationsNotEnabled,
                None,
                "multiple active exact headword publications are not enabled",
            )
            .with_meta(ProblemMeta {
                surface_match_page: Some(SurfaceMatchPageAny::V3(*page)),
                ..ProblemMeta::default()
            })
        }
        LexiconServiceError::DuplicateWord => AppError::conflict(
            ErrorCode::DuplicateWord,
            Some("headword"),
            "word already exists",
        ),
        LexiconServiceError::IdempotencyConflict => AppError::conflict(
            ErrorCode::IdempotencyConflict,
            None,
            "idempotency key was reused with a different request",
        ),
        LexiconServiceError::WordNotFound => {
            AppError::not_found_with_code(ErrorCode::WordNotFound, "word not found")
        }
        LexiconServiceError::SentenceNotFound => {
            AppError::not_found_with_code(ErrorCode::SentenceNotFound, "sentence not found")
        }
        LexiconServiceError::SentenceAssociationsUnresolved => AppError::conflict(
            ErrorCode::SentenceAssociationsUnresolved,
            None,
            "sentence associations are not resolved for the current text",
        ),
        LexiconServiceError::SentenceAssociationClientUpgradeRequired => AppError::conflict(
            ErrorCode::SentenceAssociationClientUpgradeRequired,
            None,
            "upgrade the editor before replacing segmented sentence associations",
        ),
        LexiconServiceError::PendingSentenceAssociationNotFound => AppError::not_found_with_code(
            ErrorCode::PendingSentenceAssociationNotFound,
            "pending sentence association not found",
        ),
        LexiconServiceError::PendingSentenceAssociationClaimed => AppError::conflict(
            ErrorCode::PendingSentenceAssociationClaimed,
            None,
            "pending sentence association was already claimed",
        ),
        LexiconServiceError::PublicationNotFound => {
            AppError::not_found_with_code(ErrorCode::PublicationNotFound, "publication not found")
        }
        LexiconServiceError::CatalogMismatch => AppError::conflict(
            ErrorCode::InvalidPartOfSpeech,
            Some("headwords"),
            "suggested part of speech is no longer configured",
        ),
        LexiconServiceError::RevisionConflict { current_revision } => AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_revision"),
            "entry changed since it was loaded",
        )
        .with_meta(ProblemMeta {
            current_revision: Some(current_revision),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::LifecycleRevisionConflict {
            current_lifecycle_revision,
        } => AppError::conflict(
            ErrorCode::RevisionConflict,
            Some("base_lifecycle_revision"),
            "entry lifecycle changed since it was loaded",
        )
        .with_meta(ProblemMeta {
            current_lifecycle_revision: Some(current_lifecycle_revision),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::EntryArchived => AppError::conflict(
            ErrorCode::EntryArchived,
            None,
            "entry is archived and must be restored before editing",
        ),
        LexiconServiceError::EntryNotDeletable => AppError::conflict(
            ErrorCode::EntryNotDeletable,
            None,
            "only never-published entries without inbound references can be deleted",
        ),
        LexiconServiceError::EntryHasInboundPreboundRelations => AppError::conflict(
            ErrorCode::EntryHasInboundPreboundRelations,
            None,
            "entry is selected by another draft relation; remove that prebinding first",
        ),
        LexiconServiceError::EntryHasInboundPublicationRefs(references) => AppError::conflict(
            ErrorCode::EntryHasInboundPublicationRefs,
            None,
            "entry is referenced by another active current publication",
        )
        .with_meta(ProblemMeta {
            reference_locations: Some(
                references
                    .into_iter()
                    .map(|reference| crate::error::ProblemReferenceLocation {
                        target_sense_id: reference.target_sense_id,
                        source_entry_id: reference.source_entry_id,
                        source_publication_id: reference.source_publication_id,
                        source_node_id: reference.source_node_id,
                        reference_kind: reference.reference_kind,
                    })
                    .collect(),
            ),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::EntryHasUnavailablePublicationRefs(references) => AppError::conflict(
            ErrorCode::EntryHasUnavailablePublicationRefs,
            None,
            "entry current publication references an archived or unavailable target",
        )
        .with_meta(ProblemMeta {
            reference_locations: Some(
                references
                    .into_iter()
                    .map(|reference| crate::error::ProblemReferenceLocation {
                        target_sense_id: reference.target_sense_id,
                        source_entry_id: reference.source_entry_id,
                        source_publication_id: reference.source_publication_id,
                        source_node_id: reference.source_node_id,
                        reference_kind: reference.reference_kind,
                    })
                    .collect(),
            ),
            ..ProblemMeta::default()
        }),
        LexiconServiceError::ReferenceConflict => AppError::conflict(
            ErrorCode::ReferenceConflict,
            None,
            "a referenced target is changing; retry the command",
        ),
        LexiconServiceError::RelationPrebindingFanoutExceeded => AppError::conflict(
            ErrorCode::RelationPrebindingFanoutExceeded,
            None,
            "relation prebinding reconciliation exceeds the 500 relation limit",
        ),
        LexiconServiceError::StableNodeIdChanged => AppError::conflict(
            ErrorCode::StableNodeIdChanged,
            None,
            "a stable V3 node identity changed",
        ),
        LexiconServiceError::FormReferenceConflict => AppError::conflict(
            ErrorCode::FormReferenceConflict,
            None,
            "the form operation would break an existing reference",
        ),
        LexiconServiceError::V3PublicationRequiresMigrationCanary => {
            v3_publication_requires_migration_canary()
        }
        LexiconServiceError::StepNotReachable => AppError::conflict(
            ErrorCode::StepNotReachable,
            None,
            "previous step is incomplete",
        ),
        LexiconServiceError::ValidationFailed(issues) => {
            AppError::unprocessable(ErrorCode::ValidationFailed, "draft validation failed")
                .with_field_issues(&issues)
        }
        LexiconServiceError::ValidationFailedV3(issues) => {
            AppError::unprocessable(ErrorCode::ValidationFailed, "V3 draft validation failed")
                .with_v3_field_issues(issues)
        }
        LexiconServiceError::DownstreamConfirmationRequired(affected_node_ids) => {
            AppError::conflict(
                ErrorCode::DownstreamConfirmationRequired,
                Some("confirmed_impact_token"),
                "downstream confirmation required",
            )
            .with_meta(ProblemMeta {
                affected_node_ids: Some(affected_node_ids),
                ..ProblemMeta::default()
            })
        }
        LexiconServiceError::DetectionStore(error) => AppError::unavailable_with_source(
            ErrorCode::ServiceUnavailable,
            "detection service unavailable",
            error,
        ),
        LexiconServiceError::ImpactStore(error) => AppError::unavailable_with_source(
            ErrorCode::ServiceUnavailable,
            "impact confirmation service unavailable",
            error,
        ),
        LexiconServiceError::SurfaceSnapshot(error) => AppError::unavailable_with_source(
            ErrorCode::ServiceUnavailable,
            "surface snapshot service unavailable",
            error,
        ),
        LexiconServiceError::SurfacePolicy(error) => AppError::unavailable_with_source(
            ErrorCode::ServiceUnavailable,
            "surface policy service unavailable",
            error,
        ),
        LexiconServiceError::Repository(error) => AppError::internal(error),
    }
}
