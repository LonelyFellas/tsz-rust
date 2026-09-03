//! OpenAPI 文档聚合入口。
//!
//! 规范（往上加接口时照此办理）：
//! 1. handler 上加 `#[utoipa::path(...)]`，写清 method / path / tag / responses；
//!    带鉴权的接口再加 `security(("bearer_auth" = []))`。
//! 2. 请求/响应 DTO（`XxxRequest` / `XxxResponse`）加 `#[derive(ToSchema)]`。
//! 3. 把 handler 登记到下面 `paths(...)`，把 DTO 登记到 `components(schemas(...))`。
//! 4. path 里的 `path = "..."` 要带上 nest 前缀（如 `/api/v1/auth/me`），
//!    因为 utoipa 不感知 axum 的 `.nest()`，前缀得手写全。

use utoipa::{
    Modify, OpenApi,
    openapi::{
        Content, Ref, RefOr,
        path::Operation,
        schema::Schema,
        security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

/// 全局 API 文档。新增接口只需在 `paths` / `components.schemas` 两处登记。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "tsz-rust API",
        version = "0.1.0",
        description = "tsz 核心服务（Rust 版）接口文档"
    ),
    modifiers(
        &SecurityAddon,
        &ProblemDetailsAddon,
        &DetectionSnapshotSchemaAddon,
        &SmartLexiconV3SchemaAddon
    ),
    paths(
        // auth 域
        crate::auth::handler::login,
        crate::auth::handler::login_otp,
        crate::auth::handler::refresh_token,
        crate::auth::handler::logout,
        crate::auth::handler::me,
        crate::auth::handler::register,
        crate::auth::handler::request_account_deletion_code,
        crate::auth::handler::confirm_account_deletion,
        // otp 域
        crate::otp::handler::send_otp,
        // admin 域
        crate::admin::auth::handler::admin_login,
        crate::admin::auth::handler::admin_refresh,
        crate::admin::auth::handler::admin_login_code,
        crate::admin::auth::handler::admin_logout,
        crate::admin::auth::handler::admin_logout_all,
        crate::admin::auth::handler::change_password,
        crate::admin::profile::handler::admin_profile,
        crate::admin::profile::handler::update_admin_preferences,
        crate::admin::accounts::handler::create_admin,
        crate::admin::accounts::handler::request_create_admin_code,
        crate::admin::accounts::handler::list_admins,
        crate::admin::accounts::handler::set_admin_status,
        crate::admin::accounts::handler::reset_admin_password,
        crate::admin::accounts::handler::list_users,
        crate::admin::accounts::handler::get_user,
        crate::admin::accounts::handler::set_user_status,
        crate::admin::accounts::handler::update_user,
        // catalog 域
        crate::catalog::handler::catalog,
        crate::catalog::handler::list_parts,
        crate::catalog::handler::create_part,
        crate::catalog::handler::update_part,
        crate::catalog::handler::delete_part,
        crate::catalog::handler::list_sub_parts,
        crate::catalog::handler::create_sub_part,
        crate::catalog::handler::update_sub_part,
        crate::catalog::handler::delete_sub_part,
        // lexicon 域
        crate::lexicon::handler::query::list,
        crate::lexicon::handler::query::related_search,
        crate::lexicon::handler::query::stats,
        crate::lexicon::handler::commands::detect,
        crate::lexicon::handler::query::surface_match_snapshot_page,
        crate::lexicon::handler::commands::suggest_dialect_variants,
        crate::lexicon::handler::commands::create,
        crate::lexicon::handler::lifecycle::archive_batch,
        crate::lexicon::handler::lifecycle::restore_batch,
        crate::lexicon::handler::query::get,
        crate::lexicon::handler::query::list_publications,
        crate::lexicon::handler::query::get_publication,
        crate::lexicon::handler::lifecycle::archive,
        crate::lexicon::handler::lifecycle::delete_draft,
        crate::lexicon::handler::lifecycle::delete_batch,
        crate::lexicon::handler::lifecycle::restore,
        crate::lexicon::handler::commands::preview_forms_impact,
        crate::lexicon::handler::commands::save_forms,
        crate::lexicon::handler::commands::save_meanings,
        crate::lexicon::handler::commands::validate,
        crate::lexicon::handler::commands::publish,
        crate::lexicon::handler::commands::activate_publication,
        crate::lexicon::handler::commands::replace_sentence_associations,
        crate::lexicon::handler::query::list_pending_sentence_associations,
        crate::lexicon::handler::query::resolve_sentence_targets,
        crate::lexicon::handler::query::search_component_targets,
        crate::lexicon::handler::commands::claim_pending_sentence_association,
        crate::lexicon::content_completion::handler::create_content_completion_job,
        crate::lexicon::content_completion::handler::get_content_completion_job,
        crate::lexicon::content_completion::handler::retry_content_completion_job,
        crate::speech::preview::handler::list_voices,
        crate::speech::preview::handler::create_preview,
    ),
    components(
        schemas(
            crate::error::ErrorCode,
            crate::error::ProblemDetails,
            crate::error::ProblemMeta,
            crate::error::ProblemReferenceLocation,
            // auth
            crate::auth::handler::UserProfile,
            crate::auth::handler::LoginRequest,
            crate::auth::handler::LoginResponse,
            crate::auth::handler::LoginOtpRequest,
            crate::auth::handler::RegisterRequest,
            crate::auth::handler::Token,
            crate::auth::handler::RefreshResponse,
            crate::auth::handler::AccountDeletionCodeRequest,
            crate::auth::handler::ConfirmAccountDeletionRequest,
            // user
            crate::user::model::UserRole,
            crate::user::model::AccountDeletionChannel,
            // otp
            crate::otp::handler::SendOtpRequest,
            crate::otp::model::Purpose,
            // admin
            crate::admin::auth::handler::AdminLoginRequest,
            crate::admin::auth::handler::AdminLoginResponse,
            crate::admin::auth::handler::AdminRefreshResponse,
            crate::admin::auth::handler::AdminLoginOtpRequest,
            crate::admin::auth::handler::ChangePasswordRequest,
            crate::admin::auth::handler::AdminProfile,
            crate::admin::profile::handler::AdminProfileResponse,
            crate::admin::profile::handler::AdminPreferences,
            crate::admin::profile::handler::UpdateAdminPreferencesRequest,
            crate::admin::profile::handler::UpdateAdminPreferencesResponse,
            crate::admin::auth::handler::AdminToken,
            crate::admin::AdminRole,
            crate::admin::AdminStatus,
            crate::admin::AdminDialectPreference,
            crate::admin::accounts::AdminAccountAdminResponse,
            crate::admin::accounts::AdminCreatorResponse,
            crate::admin::accounts::AdminAccountUserResponse,
            crate::admin::accounts::AdminUserListResponse,
            crate::api::PaginationMeta,
            crate::api::PaginatedResponse<crate::admin::accounts::AdminAccountAdminResponse>,
            crate::admin::accounts::handler::CreateAdminRequest,
            crate::admin::accounts::handler::CreateAdminResponse,
            crate::admin::accounts::handler::UpdateAdminStatusRequest,
            crate::admin::accounts::handler::ResetAdminPasswordResponse,
            crate::admin::accounts::handler::UpdateUserStatusRequest,
            crate::admin::accounts::handler::UpdateUserRequest,
            crate::user::model::UserStatus,
            // catalog
            crate::catalog::model::Actor,
            crate::catalog::model::CatalogResponse,
            crate::catalog::model::CatalogPart,
            crate::catalog::model::CatalogSubPart,
            crate::catalog::model::PartOfSpeechConfig,
            crate::catalog::model::SubPartOfSpeechConfig,
            crate::catalog::model::SubPartListResponse,
            crate::catalog::model::CreatePartRequest,
            crate::catalog::model::UpdatePartRequest,
            crate::catalog::model::CreateSubPartRequest,
            crate::catalog::model::UpdateSubPartRequest,
            crate::api::PaginatedResponse<crate::catalog::model::PartOfSpeechConfig>,
            // lexicon
            crate::lexicon::dto::EntryKind,
            crate::lexicon::dto::Dialect,
            crate::lexicon::dto::SourceDialect,
            crate::lexicon::dto::TextOrigin,
            crate::lexicon::dto::WordHeadwordsV2,
            crate::lexicon::dto::DetectWordInputV2,
            crate::lexicon::dto::DetectionRequestEcho,
            crate::lexicon::dto::DuplicateWordMatchV2,
            crate::lexicon::dto::SurfaceMatchCandidateV2,
            crate::lexicon::dto::ExistingSurfaceSourceV2,
            crate::lexicon::dto::SurfaceContentScopeV2,
            crate::lexicon::dto::SurfaceConfirmationReasonV2,
            crate::lexicon::dto::SurfaceMatchCategoryV2,
            crate::lexicon::dto::SurfaceAttentionLevelV2,
            crate::lexicon::dto::SurfaceMatchSeverityV2,
            crate::lexicon::dto::ExistingSurfaceMatchV2,
            crate::lexicon::dto::LexiconSurfaceMatchV2,
            crate::lexicon::dto::RelationTypeV2,
            crate::lexicon::dto::RelationReferenceCountsV2,
            crate::lexicon::dto::RelationReferencePreviewV2,
            crate::lexicon::dto::RelationReferenceSummaryV2,
            crate::lexicon::dto::MatchedEntryContextV2,
            crate::lexicon::dto::SurfacePolicyNameV2,
            crate::lexicon::dto::SurfacePolicyBlockCodeV2,
            crate::lexicon::dto::SurfaceContinuationEnabledV2,
            crate::lexicon::dto::SurfaceContinuationDisabledV2,
            crate::lexicon::dto::SurfaceMatchPageBaseV2,
            crate::lexicon::dto::SurfaceMatchEnabledNextPageV2,
            crate::lexicon::dto::SurfaceMatchEnabledTerminalPageV2,
            crate::lexicon::dto::SurfaceMatchTemporarilyDisabledPageV2,
            crate::lexicon::dto::SurfaceMatchPageV2,
            crate::lexicon::dto::SmartDictionaryResultV2,
            crate::lexicon::dto::BuiltinDictionaryResultV2,
            crate::lexicon::dto::DetectWordResponseV2,
            crate::lexicon::dto::CreateAdminWordV2Input,
            crate::lexicon::dto::PronunciationStyle,
            crate::lexicon::dto::WordPronunciationV2,
            crate::lexicon::dto::WordFormVariantV2,
            crate::lexicon::dto::WordBaseFormSlotV2,
            crate::lexicon::dto::WordDerivedFormSlotV2,
            crate::lexicon::dto::WordFormGroupV2,
            crate::lexicon::dto::DialectRulesV2,
            crate::lexicon::dto::WordPosFormsV2,
            crate::lexicon::dto::DraftFormsStepContent,
            crate::lexicon::dto::RichTextSpan,
            crate::lexicon::dto::RichTextSpanKind,
            crate::lexicon::dto::RichTextEmphasisLevel,
            crate::lexicon::dto::RichTextPhonemeAlphabet,
            crate::lexicon::dto::RichTextHighlightColor,
            crate::lexicon::dto::RichTextAnnotation,
            crate::lexicon::dto::RichTextV1,
            crate::lexicon::dto::RichTextV2,
            crate::lexicon::dto::RichText,
            crate::lexicon::dto::TextVariantV2<crate::lexicon::dto::RichText>,
            crate::lexicon::dto::DialectVariantSlotV2<crate::lexicon::dto::RichText>,
            crate::lexicon::dto::EnglishTextV2,
            crate::lexicon::dto::GrammarVariantV2,
            crate::lexicon::dto::GrammarStructureV2,
            crate::lexicon::dto::WordDefinitionV2,
            crate::lexicon::dto::WordSentenceLinkV2,
            crate::lexicon::dto::SentenceSourceRangeV1,
            crate::lexicon::dto::SentenceAssociationOriginV2,
            crate::lexicon::dto::SentenceAssociationStateV1,
            crate::lexicon::dto::SentenceAssociationsStateV2,
            crate::lexicon::dto::WordSentenceAssociationV2,
            crate::lexicon::dto::SentenceAssociationInputV2,
            crate::lexicon::dto::SentenceAssociationInputV3,
            crate::lexicon::dto::ReplaceSentenceAssociationsInputV2,
            crate::lexicon::dto::ReplaceSentenceAssociationsInputV3,
            crate::lexicon::dto::ReplaceSentenceAssociationsInput,
            crate::lexicon::dto::PendingSentenceAssociationItemV1,
            crate::lexicon::dto::PendingSentenceAssociationItemV3,
            crate::lexicon::dto::PendingSentenceAssociationListResponse,
            crate::lexicon::dto::ClaimPendingSentenceAssociationInput,
            crate::lexicon::dto::WordSentenceV2,
            crate::lexicon::dto::WordRelationV2,
            crate::lexicon::dto::WordSenseV2,
            crate::lexicon::dto::SenseGroupV2,
            crate::lexicon::dto::WordPosMeaningsV2,
            crate::lexicon::dto::DraftMeaningsStepContent,
            crate::lexicon::dto::WordDetectionSnapshotV2,
            crate::lexicon::dto::WordDetectionSnapshotSmartDictionaryV2,
            crate::lexicon::dto::DetectionSurfaceWarningAuditV2,
            crate::lexicon::dto::DetectionSurfaceMatchPreviewV2,
            crate::lexicon::dto::PersistedWordStep,
            crate::lexicon::dto::WordCreationStep,
            crate::lexicon::dto::AdminWordStatus,
            crate::lexicon::dto::AdminWordV2,
            crate::lexicon::dto::AdminWordV2Envelope,
            crate::lexicon::dto::RetiredStableSlotV2,
            crate::lexicon::dto::AdminWordDraftV2Envelope,
            crate::lexicon::dto::AdminWordListItem,
            crate::lexicon::dto::AdminWordListPage,
            crate::lexicon::dto::AdminWordListResponse,
            crate::lexicon::dto::RelatedWordSense,
            crate::lexicon::dto::RelatedWordResult,
            crate::lexicon::dto::HeadwordVariant,
            crate::lexicon::dto::RelatedSearchMatchMode,
            crate::lexicon::dto::RelatedSearchLegacyResponse,
            crate::lexicon::dto::RelatedSearchV2Response,
            crate::lexicon::dto::RelatedSearchResponse,
            crate::lexicon::dto::AdminWordStats,
            crate::lexicon::dto::StepSaveIntent,
            crate::lexicon::dto::SaveFormsStepInput,
            crate::lexicon::dto::SaveMeaningsStepInput,
            crate::lexicon::dto::PreviewFormsImpactInputV2,
            crate::lexicon::dto::FormsImpactItemV2,
            crate::lexicon::dto::FormsImpactResponseV2,
            crate::lexicon::dto::DraftValidationIssue,
            crate::lexicon::dto::DraftValidationIssueV2,
            crate::lexicon::dto::DraftValidationIssueAny,
            crate::lexicon::dto::DraftReferenceLocation,
            crate::lexicon::dto::DraftNodeLocation,
            crate::lexicon::dto::DraftValidationResponse,
            crate::lexicon::dto::ValidateAdminWordV2Input,
            crate::lexicon::dto::PublishAdminWordV2Input,
            crate::lexicon::dto::ActivatePublicationInput,
            crate::lexicon::dto::DeleteDraftInput,
            crate::lexicon::dto::EntryLifecycleInput,
            crate::lexicon::dto::EntryLifecycleTarget,
            crate::lexicon::dto::EntryLifecycleBatchInput,
            crate::lexicon::dto::EntryDeleteBatchInput,
            crate::lexicon::dto::EntryReferenceKind,
            crate::lexicon::dto::EntryReferencePreview,
            crate::lexicon::dto::EntryReferenceSummary,
            crate::lexicon::dto::EntryDeleteBatchResponse,
            crate::lexicon::dto::EntryLifecycleBatchResponse,
            // Smart Lexicon V3 Phase 1 contract (storage remains capability-gated)
            crate::lexicon::dto::EnglishLanguageV3,
            crate::lexicon::dto::WordEntryKindV3,
            crate::lexicon::dto::WordFormTypeV3,
            crate::lexicon::dto::CommonDialectV3,
            crate::lexicon::dto::UkDialectV3,
            crate::lexicon::dto::UsDialectV3,
            crate::lexicon::dto::WordPronunciationV3,
            crate::lexicon::dto::WordCommonFormVariantV3,
            crate::lexicon::dto::WordUkFormVariantV3,
            crate::lexicon::dto::WordUsFormVariantV3,
            crate::lexicon::dto::WordRegionalVariantsV3,
            crate::lexicon::dto::WordConcreteFormV3,
            crate::lexicon::dto::WordFormGroupMemberV3,
            crate::lexicon::dto::WordFormGroupV3,
            crate::lexicon::dto::DialectModeV3,
            crate::lexicon::dto::DialectRulesV3,
            crate::lexicon::dto::WordPosFormsV3,
            crate::lexicon::dto::DraftFormsStepContentV3,
            crate::lexicon::dto::RichTextSpanV3,
            crate::lexicon::dto::RichTextAnnotationV3,
            crate::lexicon::dto::RichTextV1V3,
            crate::lexicon::dto::RichTextV2V3,
            crate::lexicon::dto::RichTextV3,
            crate::lexicon::dto::RichTextVariantV3,
            crate::lexicon::dto::DialectVariantRichTextSlotV3,
            crate::lexicon::dto::EnglishTextV3,
            crate::lexicon::dto::GrammarVariantV3,
            crate::lexicon::dto::GrammarStructureV3,
            crate::lexicon::dto::WordDefinitionV3,
            crate::lexicon::dto::WordSentenceLinkV3,
            crate::lexicon::dto::WordSentenceAssociationV3,
            crate::lexicon::dto::ResolveSentenceTargetsV3Input,
            crate::lexicon::dto::ResolveSentenceTargetsV3Response,
            crate::lexicon::dto::SentenceTargetDiscoveryCompletenessV3,
            crate::lexicon::dto::SentenceTargetMatchKindV3,
            crate::lexicon::dto::SentenceTargetMatchEvidenceV3,
            crate::lexicon::dto::SentenceTargetSenseV3,
            crate::lexicon::dto::SentenceTargetCandidateFormV3,
            crate::lexicon::dto::PublishedSentenceTargetCandidateV3,
            crate::lexicon::dto::SentenceTargetDraftStateV3,
            crate::lexicon::dto::SentenceTargetDraftLinkabilityV3,
            crate::lexicon::dto::DraftSentenceTargetCandidateV3,
            crate::lexicon::dto::SentenceTargetRangeResultV3,
            crate::lexicon::dto::SearchComponentTargetsV3Input,
            crate::lexicon::dto::SearchComponentTargetsV3Response,
            crate::lexicon::dto::WordSentenceV3,
            crate::lexicon::dto::WordRelationV3,
            crate::lexicon::dto::WordSenseV3,
            crate::lexicon::dto::SenseGroupV3,
            crate::lexicon::dto::WordPosMeaningsV3,
            crate::lexicon::dto::DraftMeaningsStepContentV3,
            crate::lexicon::dto::WordSentenceWritableV3,
            crate::lexicon::dto::WordRelationWritableV3,
            crate::lexicon::dto::WordSenseWritableV3,
            crate::lexicon::dto::WordPosMeaningsWritableV3,
            crate::lexicon::dto::DraftMeaningsStepContentWritableV3,
            crate::lexicon::dto::EntryPresentationV3,
            crate::lexicon::dto::LegacyHeadwordsCompatibilityV3,
            crate::lexicon::dto::AdminWordV3Compatibility,
            crate::lexicon::dto::V3PublicationBlockCode,
            crate::lexicon::dto::V3PublicationCapability,
            crate::lexicon::dto::PronunciationNormalizationVersionV3,
            crate::lexicon::dto::AdminWordV3Capabilities,
            crate::lexicon::dto::AdminWordV3,
            crate::lexicon::dto::AdminWordAny,
            crate::lexicon::dto::AdminWordAnyEnvelope,
            crate::lexicon::dto::AdminWordDraftV3Envelope,
            crate::lexicon::dto::V3RetiredNodeRole,
            crate::lexicon::dto::RetiredStableNodeV3,
            crate::lexicon::dto::AdminWordDraftAnyEnvelope,
            crate::lexicon::dto::EntryLifecycleBatchResponseAny,
            crate::lexicon::dto::CreateAdminWordV3Input,
            crate::lexicon::dto::CreateAdminWordAnyInput,
            crate::lexicon::dto::PreviewFormsImpactInputV3,
            crate::lexicon::dto::PreviewFormsImpactInputAny,
            crate::lexicon::dto::SaveFormsStepInputV3,
            crate::lexicon::dto::SaveFormsStepInputAny,
            crate::lexicon::dto::SaveMeaningsStepInputV3,
            crate::lexicon::dto::SaveMeaningsStepInputAny,
            crate::lexicon::dto::ValidateAdminWordV3Input,
            crate::lexicon::dto::ValidateAdminWordAnyInput,
            crate::lexicon::dto::PublishAdminWordV3Input,
            crate::lexicon::dto::PublishAdminWordAnyInput,
            crate::lexicon::dto::ActivatePublicationV3Input,
            crate::lexicon::dto::ActivatePublicationAnyInput,
            crate::lexicon::dto::FormsImpactResponseV3,
            crate::lexicon::dto::FormsImpactNodeTypeV3,
            crate::lexicon::dto::FormsImpactItemV3,
            crate::lexicon::dto::FormsImpactResponseAny,
            crate::lexicon::dto::V3ValidationIssueCode,
            crate::lexicon::dto::V3DraftNodeLocation,
            crate::lexicon::dto::V3DraftValidationIssue,
            crate::lexicon::dto::DraftValidationResponseV3,
            crate::lexicon::dto::DraftValidationResponseAny,
            crate::lexicon::dto::FormSurfaceMatchV3,
            crate::lexicon::dto::LegacySurfaceMatchV3,
            crate::lexicon::dto::SurfaceMatchItemV3,
            crate::lexicon::dto::RelationReferencePreviewV3,
            crate::lexicon::dto::RelationReferenceSummaryV3,
            crate::lexicon::dto::MatchedEntryContextV3,
            crate::lexicon::dto::SurfaceMatchPageBaseV3,
            crate::lexicon::dto::SurfaceMatchEnabledNextPageV3,
            crate::lexicon::dto::SurfaceMatchEnabledTerminalPageV3,
            crate::lexicon::dto::SurfaceMatchTemporarilyDisabledPageV3,
            crate::lexicon::dto::SurfaceMatchPageV3,
            crate::lexicon::dto::SurfaceMatchPageAny,
            crate::lexicon::dto::DetectLexiconSurfaceV3Input,
            crate::lexicon::dto::DetectLexiconInputAny,
            crate::lexicon::dto::DetectionSurfaceRequestEchoV3,
            crate::lexicon::dto::DictionaryProviderEvidenceV3,
            crate::lexicon::dto::DictionaryCoverageV3,
            crate::lexicon::dto::DictionaryProvenanceV3,
            crate::lexicon::dto::DictionaryPronunciationEvidenceV3,
            crate::lexicon::dto::SuggestedCommonFormVariantV3,
            crate::lexicon::dto::SuggestedUkFormVariantV3,
            crate::lexicon::dto::SuggestedUsFormVariantV3,
            crate::lexicon::dto::SuggestedRegionalVariantsV3,
            crate::lexicon::dto::SuggestedConcreteFormV3,
            crate::lexicon::dto::BuiltinDictionaryEvidenceV3,
            crate::lexicon::dto::DetectLexiconSurfaceResponseV3,
            crate::lexicon::dto::DetectLexiconResponseAny,
            crate::lexicon::dto::AdminWordPublicationV2,
            crate::lexicon::dto::AdminWordPublicationV3,
            crate::lexicon::dto::AdminWordPublicationAny,
            crate::lexicon::dto::AdminWordPublicationEnvelope,
            crate::lexicon::dto::AdminWordPublicationListResponse,
            crate::lexicon::dto::AdminWordListItemV3,
            crate::lexicon::dto::AdminWordListItemAny,
            crate::lexicon::dto::RelatedWordMatchV3,
            crate::lexicon::dto::RelatedWordSenseV3,
            crate::lexicon::dto::RelatedWordResultV3,
            crate::lexicon::dto::RelatedWordResultAny,
            crate::lexicon::dto::DialectSuggestionFieldKind,
            crate::lexicon::dto::DialectVariantSuggestionItemV2,
            crate::lexicon::dto::SuggestDialectVariantsInputV2,
            crate::lexicon::dto::DialectSuggestionProviderV2,
            crate::lexicon::dto::SuggestDialectVariantsResponseV2,
            crate::lexicon::content_completion::dto::ContentCompletionScope,
            crate::lexicon::content_completion::dto::ContentCompletionFillPolicy,
            crate::lexicon::content_completion::dto::ContentCompletionJobStatus,
            crate::lexicon::content_completion::dto::ContentCompletionPartitionStatus,
            crate::lexicon::content_completion::dto::CreateContentCompletionJobInput,
            crate::lexicon::content_completion::dto::RetryContentCompletionJobInput,
            crate::lexicon::content_completion::dto::ContentCompletionDictionaryProvenance,
            crate::lexicon::content_completion::dto::ContentCompletionGenerationProvenance,
            crate::lexicon::content_completion::dto::ContentCompletionEvidenceKind,
            crate::lexicon::content_completion::dto::ContentCompletionFieldOrigins,
            crate::lexicon::content_completion::dto::ContentCompletionProvenance,
            crate::lexicon::content_completion::dto::ContentCompletionPartition,
            crate::lexicon::content_completion::dto::ContentCompletionJob,
            crate::lexicon::content_completion::dto::ContentCompletionJobEnvelope,
            crate::speech::preview::dto::CreatePreviewRequest,
            crate::speech::preview::dto::VoiceCapabilities,
            crate::speech::preview::dto::VoiceResponse,
            crate::speech::preview::dto::VoiceListResponse,
            crate::speech::preview::dto::PreviewCacheStatus,
            crate::speech::preview::dto::PreviewResponse,
        )
    ),
    tags(
        (name = "auth", description = "认证 / 会话"),
        (name = "user", description = "用户"),
        (name = "otp", description = "验证码"),
        (name = "admin", description = "管理后台认证 / 会话"),
        (name = "admin-accounts", description = "管理后台管理员账号治理"),
        (name = "admin-users", description = "管理后台 C 端用户管理"),
        (name = "admin-catalog", description = "管理后台词性目录配置"),
        (name = "admin-lexicon", description = "管理后台智能词库创编与发布"),
        (name = "admin-speech", description = "管理后台语音目录与试听"),
    )
)]
pub struct ApiDoc;

/// `utoipa` represents a flattened Rust enum as `allOf(base, oneOf(...))`.
/// That shape cannot make both the base and variant objects closed without
/// accidentally rejecting one another's fields. Replace only this aggregate
/// with two complete, closed branches so OpenAPI mirrors serde's strict union.
struct DetectionSnapshotSchemaAddon;

impl Modify for DetectionSnapshotSchemaAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .as_mut()
            .expect("derived OpenAPI must contain components");

        let smart_dictionary = components
            .schemas
            .get("WordDetectionSnapshotSmartDictionaryV2")
            .cloned()
            .expect("smart dictionary snapshot union must be registered");
        let mut smart_dictionary_json =
            serde_json::to_value(smart_dictionary).expect("snapshot union schema must serialize");
        for branch in smart_dictionary_json["oneOf"]
            .as_array_mut()
            .expect("snapshot union must have oneOf branches")
        {
            branch["additionalProperties"] = serde_json::json!(false);
        }
        components.schemas.insert(
            "WordDetectionSnapshotSmartDictionaryV2".to_owned(),
            serde_json::from_value(smart_dictionary_json)
                .expect("closed snapshot union schema must deserialize"),
        );

        for name in [
            "SmartDictionaryResultV2",
            "SurfaceMatchCandidateV2",
            "ExistingSurfaceSourceV2",
            "SurfaceMatchItemV3",
        ] {
            let mut tagged_union = component_schema_json(components, name);
            for branch in tagged_union["oneOf"]
                .as_array_mut()
                .unwrap_or_else(|| panic!("schema {name} must contain oneOf branches"))
            {
                branch["additionalProperties"] = serde_json::json!(false);
            }
            components.schemas.insert(
                name.to_owned(),
                serde_json::from_value(tagged_union)
                    .unwrap_or_else(|_| panic!("closed schema {name} must deserialize")),
            );
        }

        let aggregate = components
            .schemas
            .get("WordDetectionSnapshotV2")
            .cloned()
            .expect("word detection snapshot must be registered");
        let aggregate_json =
            serde_json::to_value(aggregate).expect("word detection snapshot schema must serialize");
        let base = aggregate_json["allOf"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["properties"]["detection_id"].is_object())
            })
            .cloned()
            .expect("flattened snapshot schema must contain its base object");

        let mut clear = base.clone();
        close_detection_snapshot_branch(&mut clear, "clear", false);
        let mut warning = base;
        close_detection_snapshot_branch(&mut warning, "warning", true);
        let strict = serde_json::json!({
            "oneOf": [clear, warning],
            "discriminator": {"propertyName": "smart_dictionary_status"}
        });
        components.schemas.insert(
            "WordDetectionSnapshotV2".to_owned(),
            serde_json::from_value::<RefOr<Schema>>(strict)
                .expect("strict word detection snapshot schema must deserialize"),
        );

        let page_base = component_schema_json(components, "SurfaceMatchPageBaseV2");
        let mut complete_pages = Vec::new();
        for name in [
            "SurfaceMatchEnabledNextPageV2",
            "SurfaceMatchEnabledTerminalPageV2",
            "SurfaceMatchTemporarilyDisabledPageV2",
        ] {
            let flattened = component_schema_json(components, name);
            let extension = flattened["allOf"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["properties"].is_object()))
                .cloned()
                .expect("flattened surface page must contain variant fields");
            let complete = complete_surface_page_branch(&page_base, &extension);
            components.schemas.insert(
                name.to_owned(),
                serde_json::from_value(complete.clone())
                    .expect("closed surface page branch must deserialize"),
            );
            complete_pages.push(complete);
        }
        components.schemas.insert(
            "SurfaceMatchPageV2".to_owned(),
            serde_json::from_value::<RefOr<Schema>>(serde_json::json!({
                "oneOf": complete_pages
            }))
            .expect("complete surface page union must deserialize"),
        );
    }
}

fn component_schema_json(
    components: &utoipa::openapi::schema::Components,
    name: &str,
) -> serde_json::Value {
    serde_json::to_value(
        components
            .schemas
            .get(name)
            .unwrap_or_else(|| panic!("schema {name} must be registered")),
    )
    .expect("component schema must serialize")
}

/// Utoipa emits internally tagged enum branches as open objects. Close the V3
/// tagged unions so the generated contract matches serde's `deny_unknown_fields`.
struct SmartLexiconV3SchemaAddon;

impl Modify for SmartLexiconV3SchemaAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .as_mut()
            .expect("derived OpenAPI must contain components");
        for (name, discriminator) in [
            ("WordHeadwordsV2", "mode"),
            ("WordRegionalVariantsV3", "mode"),
            ("V3PublicationCapability", "mode"),
            ("DialectVariantRichTextSlotV3", "state"),
            ("EnglishTextV3", "mode"),
            ("WordDefinitionV3", "definition_mode"),
            ("RichTextAnnotationV3", "type"),
            ("LegacyHeadwordsCompatibilityV3", "mode"),
            ("SuggestedRegionalVariantsV3", "mode"),
            ("BuiltinDictionaryEvidenceV3", "status"),
            ("ResolveSentenceTargetsV3Input", "mode"),
            ("PhraseComponentUsageV3", "state"),
            ("WordSentenceAssociationV2", "state"),
            ("WordSentenceAssociationV3", "state"),
        ] {
            let mut union = component_schema_json(components, name);
            for branch in union["oneOf"]
                .as_array_mut()
                .unwrap_or_else(|| panic!("schema {name} must contain oneOf branches"))
            {
                branch["additionalProperties"] = serde_json::json!(false);
            }
            union["discriminator"] = serde_json::json!({"propertyName": discriminator});
            components.schemas.insert(
                name.to_owned(),
                serde_json::from_value(union)
                    .unwrap_or_else(|_| panic!("closed schema {name} must deserialize")),
            );
        }

        // These response/error containers are shared names, but are server-owned typed
        // objects. Closing them is required for the V3 reachable graph and cannot reject a
        // legacy request because none of them is accepted as request input.
        for name in [
            "AdminWordListPage",
            "ProblemDetails",
            "ProblemMeta",
            "ProblemReferenceLocation",
            "DraftNodeLocation",
            "DraftReferenceLocation",
        ] {
            let mut schema = component_schema_json(components, name);
            schema["additionalProperties"] = serde_json::json!(false);
            components.schemas.insert(
                name.to_owned(),
                serde_json::from_value(schema)
                    .unwrap_or_else(|_| panic!("closed schema {name} must deserialize")),
            );
        }

        let build_relation_union =
            |name: &str, branch_specs: Vec<(Vec<&str>, Vec<&str>, Option<&str>)>| {
                let derived = component_schema_json(components, name);
                let derived_properties = derived["properties"]
                    .as_object()
                    .expect("relation schema properties must be an object");
                let one_of = branch_specs
                    .into_iter()
                    .map(|(allowed, required, prebinding_state)| {
                        let mut properties = serde_json::Map::new();
                        for field in ["id", "relation", "score"].into_iter().chain(allowed) {
                            properties.insert(field.to_owned(), derived_properties[field].clone());
                        }
                        if let Some(state) = prebinding_state {
                            properties.insert(
                                "prebinding_state".to_owned(),
                                serde_json::json!({
                                    "type": "string",
                                    "enum": [state],
                                    "readOnly": true
                                }),
                            );
                        }
                        let required = ["id", "relation", "score"]
                            .into_iter()
                            .chain(required)
                            .collect::<Vec<_>>();
                        serde_json::json!({
                            "type": "object",
                            "additionalProperties": false,
                            "required": required,
                            "properties": properties
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({"oneOf": one_of})
            };
        let relation_response = build_relation_union(
            "WordRelationV3",
            vec![
                (
                    vec![
                        "target_word_id",
                        "target_sense_id",
                        "target_headword",
                        "target_gloss",
                        "target_status",
                    ],
                    vec![
                        "target_word_id",
                        "target_sense_id",
                        "target_headword",
                        "target_gloss",
                    ],
                    None,
                ),
                (
                    vec!["pending_target_headword", "pending_target_gloss"],
                    vec!["pending_target_headword"],
                    None,
                ),
                // 预绑定不携带待建词面：词条身份在 prebound id 上，词面回显走只读 target_headword。
                (
                    vec![
                        "prebound_target_word_id",
                        "pending_target_gloss",
                        "target_headword",
                        "target_status",
                    ],
                    vec![
                        "prebound_target_word_id",
                        "target_headword",
                        "prebinding_state",
                    ],
                    Some("waiting_first_sense"),
                ),
                (
                    vec![
                        "prebound_target_word_id",
                        "pending_target_gloss",
                        "target_headword",
                        "target_status",
                    ],
                    vec![
                        "prebound_target_word_id",
                        "target_headword",
                        "prebinding_state",
                    ],
                    Some("target_sense_deleted"),
                ),
            ],
        );
        let relation_writable = build_relation_union(
            "WordRelationWritableV3",
            vec![
                (
                    vec!["target_word_id", "target_sense_id"],
                    vec!["target_word_id", "target_sense_id"],
                    None,
                ),
                (
                    vec!["pending_target_headword", "pending_target_gloss"],
                    vec!["pending_target_headword"],
                    None,
                ),
                // 预绑定写入只带稳定 id 与可选预定义词义。
                (
                    vec!["prebound_target_word_id", "pending_target_gloss"],
                    vec!["prebound_target_word_id"],
                    None,
                ),
            ],
        );
        for (name, schema) in [
            ("WordRelationV3", relation_response),
            ("WordRelationWritableV3", relation_writable),
        ] {
            components.schemas.insert(
                name.to_owned(),
                serde_json::from_value(schema)
                    .unwrap_or_else(|_| panic!("strict schema {name} must deserialize")),
            );
        }
        let page_base = component_schema_json(components, "SurfaceMatchPageBaseV3");
        let mut complete_pages = Vec::new();
        for name in [
            "SurfaceMatchEnabledNextPageV3",
            "SurfaceMatchEnabledTerminalPageV3",
            "SurfaceMatchTemporarilyDisabledPageV3",
        ] {
            let flattened = component_schema_json(components, name);
            let extension = flattened["allOf"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["properties"].is_object()))
                .cloned()
                .expect("flattened V3 surface page must contain variant fields");
            let complete = complete_surface_page_branch(&page_base, &extension);
            components.schemas.insert(
                name.to_owned(),
                serde_json::from_value(complete.clone())
                    .expect("closed V3 surface page branch must deserialize"),
            );
            complete_pages.push(complete);
        }
        components.schemas.insert(
            "SurfaceMatchPageV3".to_owned(),
            serde_json::from_value::<RefOr<Schema>>(serde_json::json!({
                "oneOf": complete_pages
            }))
            .expect("complete V3 surface page union must deserialize"),
        );
    }
}

fn complete_surface_page_branch(
    base: &serde_json::Value,
    extension: &serde_json::Value,
) -> serde_json::Value {
    let mut complete = base.clone();
    complete["additionalProperties"] = serde_json::json!(false);
    complete["properties"]
        .as_object_mut()
        .expect("surface page base properties must be an object")
        .extend(
            extension["properties"]
                .as_object()
                .expect("surface page extension properties must be an object")
                .clone(),
        );
    let required = complete["required"]
        .as_array_mut()
        .expect("surface page base required fields must be an array");
    for field in extension["required"]
        .as_array()
        .expect("surface page extension required fields must be an array")
    {
        if !required.contains(field) {
            required.push(field.clone());
        }
    }
    complete
}

fn close_detection_snapshot_branch(branch: &mut serde_json::Value, status: &str, warning: bool) {
    branch["additionalProperties"] = serde_json::json!(false);
    branch["properties"]["smart_dictionary_status"] = serde_json::json!({
        "type": "string",
        "enum": [status]
    });
    branch["properties"]["surface_warning"] = if warning {
        serde_json::json!({"$ref": "#/components/schemas/DetectionSurfaceWarningAuditV2"})
    } else {
        serde_json::json!({"type": "null"})
    };
    let required = branch["required"]
        .as_array_mut()
        .expect("snapshot base required fields must be an array");
    required.push(serde_json::json!("smart_dictionary_status"));
    if warning {
        required.push(serde_json::json!("surface_warning"));
    }
}

/// 注入 Bearer JWT 安全方案，让 Swagger UI 出现 "Authorize" 按钮。
/// 接口上用 `security(("bearer_auth" = []))` 引用这个名字。
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // components 一定存在（derive 已建好），直接取用
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .build(),
                ),
            );
        }
    }
}

/// 给所有已声明的 4xx/5xx 自动挂上统一 ProblemDetails，避免每个 handler 重复标注。
struct ProblemDetailsAddon;

impl Modify for ProblemDetailsAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        for path in openapi.paths.paths.values_mut() {
            add_error_response_schema(path.get.as_mut());
            add_error_response_schema(path.put.as_mut());
            add_error_response_schema(path.post.as_mut());
            add_error_response_schema(path.delete.as_mut());
            add_error_response_schema(path.options.as_mut());
            add_error_response_schema(path.head.as_mut());
            add_error_response_schema(path.patch.as_mut());
            add_error_response_schema(path.trace.as_mut());
        }
    }
}

fn add_error_response_schema(operation: Option<&mut Operation>) {
    let Some(operation) = operation else {
        return;
    };
    for (status, response) in &mut operation.responses.responses {
        let is_error = status
            .parse::<u16>()
            .is_ok_and(|status| (400..600).contains(&status));
        if !is_error {
            continue;
        }
        if let RefOr::T(response) = response
            && response.content.is_empty()
        {
            response.content.insert(
                "application/problem+json".to_owned(),
                Content::new(Some(Ref::from_schema_name("ProblemDetails"))),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_ref_aware_strict_graph(
        schemas: &serde_json::Map<String, serde_json::Value>,
        root: &str,
    ) {
        fn walk(
            schema: &serde_json::Value,
            schemas: &serde_json::Map<String, serde_json::Value>,
            path: &str,
            visited_refs: &mut std::collections::HashSet<String>,
        ) {
            if let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
                let name = reference
                    .strip_prefix("#/components/schemas/")
                    .unwrap_or_else(|| panic!("{path} contains unsupported ref {reference}"));
                if visited_refs.insert(name.to_owned()) {
                    let target = schemas
                        .get(name)
                        .unwrap_or_else(|| panic!("{path} references missing schema {name}"));
                    walk(target, schemas, &format!("{path} -> {name}"), visited_refs);
                }
                return;
            }

            let is_object = schema.get("type").and_then(serde_json::Value::as_str)
                == Some("object")
                || schema
                    .get("properties")
                    .is_some_and(serde_json::Value::is_object);
            if is_object {
                assert_eq!(
                    schema.get("additionalProperties"),
                    Some(&serde_json::Value::Bool(false)),
                    "V3 reachable object must fail closed at {path}"
                );
            }

            if let Some(properties) = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                for (name, property) in properties {
                    walk(
                        property,
                        schemas,
                        &format!("{path}.properties.{name}"),
                        visited_refs,
                    );
                }
            }
            for keyword in ["oneOf", "allOf", "anyOf"] {
                if let Some(branches) = schema.get(keyword).and_then(serde_json::Value::as_array) {
                    for (index, branch) in branches.iter().enumerate() {
                        walk(
                            branch,
                            schemas,
                            &format!("{path}.{keyword}[{index}]"),
                            visited_refs,
                        );
                    }
                }
            }
            if let Some(items) = schema.get("items") {
                walk(items, schemas, &format!("{path}.items"), visited_refs);
            }
            if let Some(additional) = schema
                .get("additionalProperties")
                .filter(|value| value.is_object())
            {
                walk(
                    additional,
                    schemas,
                    &format!("{path}.additionalProperties"),
                    visited_refs,
                );
            }
        }

        let mut visited_refs = std::collections::HashSet::new();
        let schema = schemas
            .get(root)
            .unwrap_or_else(|| panic!("missing strict graph root {root}"));
        visited_refs.insert(root.to_owned());
        walk(schema, schemas, root, &mut visited_refs);
    }

    /// spec 能生成、能序列化，且示范接口 + 安全方案都在。
    /// 往上加接口时若忘了在 `paths(...)` 登记，这里的断言会替你兜住。
    #[test]
    fn openapi_spec_is_well_formed() {
        let spec = ApiDoc::openapi();
        let json = serde_json::to_value(&spec).expect("spec 应能序列化为 JSON");

        // 所有已标注路由都在（注意带 nest 前缀）。漏登记会在这里失败。
        for (method, path) in [
            ("post", "/api/v1/auth/login"),
            ("post", "/api/v1/auth/login-otp"),
            ("post", "/api/v1/auth/refresh"),
            ("post", "/api/v1/auth/logout"),
            ("get", "/api/v1/auth/me"),
            ("post", "/api/v1/auth/register"),
            ("post", "/api/v1/otp/send"),
            ("post", "/api/v1/admin/auth/login"),
            ("post", "/api/v1/admin/auth/refresh"),
            ("post", "/api/v1/admin/auth/login-code"),
            ("post", "/api/v1/admin/auth/logout"),
            ("post", "/api/v1/admin/auth/logout-all"),
            ("post", "/api/v1/admin/auth/change-password"),
            ("get", "/api/v1/admin/profile"),
            ("patch", "/api/v1/admin/profile/preferences"),
            ("post", "/api/v1/admin/admins"),
            ("post", "/api/v1/admin/admins/create-code"),
            ("get", "/api/v1/admin/admins"),
            ("patch", "/api/v1/admin/admins/{admin_id}/status"),
            ("post", "/api/v1/admin/admins/{admin_id}/reset-password"),
            ("get", "/api/v1/admin/users"),
            ("get", "/api/v1/admin/users/{id}"),
            ("patch", "/api/v1/admin/users/{id}"),
            ("patch", "/api/v1/admin/users/{id}/status"),
            ("get", "/api/v1/admin/settings/parts-of-speech/catalog"),
            ("get", "/api/v1/admin/settings/parts-of-speech"),
            ("post", "/api/v1/admin/settings/parts-of-speech"),
            ("patch", "/api/v1/admin/settings/parts-of-speech/{id}"),
            ("delete", "/api/v1/admin/settings/parts-of-speech/{id}"),
            (
                "get",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts",
            ),
            (
                "post",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts",
            ),
            (
                "patch",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
            ),
            (
                "delete",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
            ),
            ("get", "/api/v1/admin/lexicon/entries"),
            ("get", "/api/v1/admin/lexicon/entries/stats"),
            ("get", "/api/v1/admin/lexicon/entries/related-search"),
            ("post", "/api/v1/admin/lexicon/detections"),
            (
                "get",
                "/api/v1/admin/lexicon/surface-match-snapshots/{snapshot_id}",
            ),
            ("post", "/api/v1/admin/lexicon/entries"),
            ("get", "/api/v1/admin/lexicon/entries/{id}"),
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/steps/forms/impact",
            ),
            ("put", "/api/v1/admin/lexicon/entries/{id}/steps/forms"),
            ("put", "/api/v1/admin/lexicon/entries/{id}/steps/meanings"),
            (
                "put",
                "/api/v1/admin/lexicon/entries/{id}/sentences/{sentence_id}/associations",
            ),
            (
                "get",
                "/api/v1/admin/lexicon/entries/{id}/pending-sentence-associations",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/pending-sentence-associations/{association_id}/claim",
            ),
            ("post", "/api/v1/admin/lexicon/entries/{id}/validate"),
            ("post", "/api/v1/admin/lexicon/entries/{id}/publications"),
        ] {
            assert!(
                json["paths"][path][method].is_object(),
                "{} {} 应出现在 spec 中",
                method.to_uppercase(),
                path
            );
        }
        // Bearer 安全方案已注入
        assert_eq!(
            json["components"]["securitySchemes"]["bearer_auth"]["scheme"],
            "bearer"
        );
        let admin_word_required = json["components"]["schemas"]["AdminWordV2"]["required"]
            .as_array()
            .expect("AdminWordV2 应声明必填响应字段");
        assert!(
            admin_word_required
                .iter()
                .any(|value| value == "has_unpublished_changes"),
            "has_unpublished_changes 每次响应都会返回，OpenAPI 也必须标为 required"
        );
        // 响应 DTO 已登记
        assert!(
            json["components"]["schemas"]["UserProfile"].is_object(),
            "UserProfile schema 应出现在 spec 中"
        );
        assert!(
            json["components"]["schemas"]["RefreshResponse"].is_object(),
            "RefreshResponse schema 应出现在 spec 中（refresh 响应含 refresh_token_expires_at，不是裸 Token）"
        );
        let register = &json["paths"]["/api/v1/auth/register"]["post"];
        assert_eq!(
            register["summary"], "POST /api/v1/auth/register",
            "register 摘要不得残留旧 /user/register 路径"
        );
        assert!(
            register["description"]
                .as_str()
                .is_some_and(|text| text.contains("注册成功无需再次调用登录接口")),
            "register 描述应明确注册成功已经建立会话"
        );
        assert_eq!(
            register["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/RegisterRequest",
            "register 应引用 auth 域的请求 DTO"
        );
        assert_eq!(
            register["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/LoginResponse",
            "register 成功应直接返回登录响应"
        );
        let register_required = json["components"]["schemas"]["RegisterRequest"]["required"]
            .as_array()
            .expect("RegisterRequest 应声明必填字段");
        for field in ["phone", "password", "code"] {
            assert!(
                register_required.iter().any(|value| value == field),
                "RegisterRequest 应要求 {field}"
            );
        }
        assert!(
            json["components"]["schemas"]["RegisterRequest"]["properties"]
                .get("email")
                .is_none(),
            "当前注册契约不应暴露 email"
        );
        assert_eq!(
            json["components"]["schemas"]["RegisterRequest"]["properties"]["phone"]["description"],
            "中国大陆手机号",
            "注册 phone 描述应明确当前只支持手机号"
        );
        let change_password = &json["paths"]["/api/v1/admin/auth/change-password"]["post"];
        assert_eq!(
            change_password["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ChangePasswordRequest",
            "change-password 应引用明确的请求 DTO"
        );
        assert_eq!(
            change_password["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "change-password 必须声明 Bearer 鉴权"
        );
        let change_password_required =
            json["components"]["schemas"]["ChangePasswordRequest"]["required"]
                .as_array()
                .expect("ChangePasswordRequest 应声明必填字段");
        for field in ["current_password", "new_password"] {
            assert!(
                change_password_required.iter().any(|value| value == field),
                "ChangePasswordRequest 应要求 {field}"
            );
        }
        // profile 响应：flatten+inline 的 4 字段概要 + permissions + preferences 必须都出现在
        // schema 里（utoipa 对 serde flatten 字段需要 #[schema(inline)]，漏了 spec 会缺概要字段）。
        let profile_props = &json["components"]["schemas"]["AdminProfileResponse"]["properties"];
        for field in [
            "id",
            "phone",
            "display_name",
            "role",
            "permissions",
            "preferences",
        ] {
            assert!(
                profile_props[field].is_object(),
                "AdminProfileResponse schema 应含 {field} 字段（flatten 展开后），实际：{profile_props}"
            );
        }

        let create_admin = &json["paths"]["/api/v1/admin/admins"]["post"];
        assert_eq!(
            create_admin["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "创建管理员接口必须声明 Bearer 鉴权"
        );
        assert_eq!(
            create_admin["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/CreateAdminRequest",
            "创建管理员接口应引用明确的请求 DTO"
        );
        assert_eq!(
            create_admin["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/CreateAdminResponse",
            "创建管理员接口的 201 应引用明确的响应 DTO"
        );
        for status in ["400", "401", "403", "409", "500", "503"] {
            assert!(
                create_admin["responses"][status].is_object(),
                "创建管理员接口应声明 {status} 响应"
            );
        }
        let required = json["components"]["schemas"]["CreateAdminRequest"]["required"]
            .as_array()
            .expect("CreateAdminRequest 应声明必填字段");
        for field in ["phone", "code"] {
            assert!(
                required.iter().any(|value| value == field),
                "CreateAdminRequest 应要求 {field}"
            );
        }
        assert!(
            !required.iter().any(|value| value == "display_name"),
            "display_name 可由系统生成，不应标记为必填"
        );

        let create_admin_code = &json["paths"]["/api/v1/admin/admins/create-code"]["post"];
        assert_eq!(
            create_admin_code["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "创建管理员发码接口必须声明 Bearer 鉴权"
        );
        for status in ["202", "401", "403", "429", "503"] {
            assert!(
                create_admin_code["responses"][status].is_object(),
                "创建管理员发码接口应声明 {status} 响应"
            );
        }

        let list_admins = &json["paths"]["/api/v1/admin/admins"]["get"];
        assert_eq!(
            list_admins["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "管理员列表接口必须声明 Bearer 鉴权"
        );
        for status in ["200", "400", "401", "403", "500"] {
            assert!(
                list_admins["responses"][status].is_object(),
                "管理员列表接口应声明 {status} 响应"
            );
        }

        let parameters = list_admins["parameters"]
            .as_array()
            .expect("管理员列表接口应声明查询参数");
        for name in ["role", "phone", "display_name", "page", "page_size"] {
            assert!(
                parameters
                    .iter()
                    .any(|parameter| parameter["name"] == name && parameter["in"] == "query"),
                "管理员列表接口应声明 query 参数 {name}"
            );
        }
        let page = parameters
            .iter()
            .find(|parameter| parameter["name"] == "page")
            .expect("管理员列表接口应声明 page");
        assert_eq!(page["schema"]["default"], 1);
        assert_eq!(page["schema"]["minimum"], 1);
        let page_size = parameters
            .iter()
            .find(|parameter| parameter["name"] == "page_size")
            .expect("管理员列表接口应声明 page_size");
        assert_eq!(page_size["schema"]["default"], 20);
        assert_eq!(page_size["schema"]["minimum"], 1);
        assert_eq!(page_size["schema"]["maximum"], 100);

        let list_schema_ref =
            list_admins["responses"]["200"]["content"]["application/json"]["schema"]["$ref"]
                .as_str()
                .expect("管理员列表 200 响应应引用分页响应 schema");
        let list_schema_name = list_schema_ref
            .strip_prefix("#/components/schemas/")
            .expect("管理员列表响应应引用 components schema");
        let list_properties = &json["components"]["schemas"][list_schema_name]["properties"];
        assert!(list_properties["items"].is_object());
        assert!(list_properties["pagination"].is_object());

        let pagination_properties = &json["components"]["schemas"]["PaginationMeta"]["properties"];
        for field in ["page", "page_size", "total", "total_pages"] {
            assert!(
                pagination_properties[field].is_object(),
                "PaginationMeta schema 应包含 {field}"
            );
        }
        assert!(
            json["components"]["schemas"]["AdminAccountAdminResponse"]["properties"]["created_by"]
                .is_object(),
            "管理员公开响应 schema 应包含 created_by"
        );

        let list_users = &json["paths"]["/api/v1/admin/users"]["get"];
        assert_eq!(
            list_users["security"][0]["bearer_auth"],
            serde_json::json!([]),
            "用户列表接口必须声明 Bearer 鉴权"
        );
        for status in ["200", "400", "401", "403", "500"] {
            assert!(
                list_users["responses"][status].is_object(),
                "用户列表接口应声明 {status} 响应"
            );
        }
        let user_parameters = list_users["parameters"]
            .as_array()
            .expect("用户列表接口应声明查询参数");
        for name in [
            "role",
            "q",
            "registered_from",
            "registered_to",
            "page",
            "page_size",
        ] {
            assert!(
                user_parameters
                    .iter()
                    .any(|parameter| parameter["name"] == name && parameter["in"] == "query"),
                "用户列表接口应声明 query 参数 {name}"
            );
        }
        for parameter in user_parameters {
            assert_ne!(
                parameter["required"],
                serde_json::json!(true),
                "用户列表筛选参数都应可选：{parameter}"
            );
        }
        assert_eq!(
            list_users["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/AdminUserListResponse"
        );
        let user_list_properties =
            &json["components"]["schemas"]["AdminUserListResponse"]["properties"];
        assert!(user_list_properties["items"].is_object());
        assert!(user_list_properties["page"].is_object());
        let user_properties =
            &json["components"]["schemas"]["AdminAccountUserResponse"]["properties"];
        for field in [
            "id",
            "phone",
            "email",
            "display_name",
            "avatar_url",
            "roles",
            "status",
            "created_at",
            "updated_at",
        ] {
            assert!(
                user_properties[field].is_object(),
                "AdminAccountUserResponse schema 应包含 {field}"
            );
        }
        let user_required = json["components"]["schemas"]["AdminAccountUserResponse"]["required"]
            .as_array()
            .expect("AdminAccountUserResponse required 应为数组");
        for field in ["phone", "email"] {
            assert_eq!(
                user_properties[field]["type"], "string",
                "{field} 应为可选的非 null string"
            );
            assert!(
                !user_required.iter().any(|required| required == field),
                "{field} 不应出现在 required 中"
            );
        }
        // 三条单用户端点：鉴权 + 路径参数 + 与列表条目同一个响应 schema。
        for (method, path) in [
            ("get", "/api/v1/admin/users/{id}"),
            ("patch", "/api/v1/admin/users/{id}"),
            ("patch", "/api/v1/admin/users/{id}/status"),
        ] {
            let operation = &json["paths"][path][method];
            assert_eq!(
                operation["security"][0]["bearer_auth"],
                serde_json::json!([]),
                "{method} {path} 必须声明 Bearer 鉴权"
            );
            assert!(
                operation["parameters"]
                    .as_array()
                    .expect("应声明路径参数")
                    .iter()
                    .any(|parameter| parameter["name"] == "id"
                        && parameter["in"] == "path"
                        && parameter["required"] == serde_json::json!(true)),
                "{method} {path} 应声明必填的 path 参数 id"
            );
            for status in ["200", "401", "403", "404"] {
                assert!(
                    operation["responses"][status].is_object(),
                    "{method} {path} 应声明 {status} 响应"
                );
            }
            assert_eq!(
                operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/AdminAccountUserResponse",
                "{method} {path} 应返回与列表条目同形状的 AdminUser"
            );
        }

        assert!(
            json["paths"]["/api/v1/admin/admins/users"].is_null(),
            "用户列表不得继续暴露在错误的 /api/v1/admin/admins/users 路径"
        );
    }

    #[test]
    fn surface_match_expand_contract_is_documented_without_enabling_creation() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let schemas = &json["components"]["schemas"];
        let path =
            &json["paths"]["/api/v1/admin/lexicon/surface-match-snapshots/{snapshot_id}"]["get"];

        assert_eq!(
            path["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SurfaceMatchPageAny"
        );
        for status in ["400", "401", "403", "410", "503"] {
            assert_eq!(
                path["responses"][status]["content"]["application/problem+json"]["schema"]["$ref"],
                "#/components/schemas/ProblemDetails"
            );
        }
        assert!(path["parameters"].as_array().is_some_and(|parameters| {
            parameters.iter().any(|parameter| {
                parameter["name"] == "cursor"
                    && parameter["in"] == "query"
                    && parameter["required"] == true
            })
        }));

        let statuses = schemas["SmartDictionaryResultV2"]["oneOf"]
            .as_array()
            .unwrap();
        for status in ["clear", "duplicate", "warning", "unavailable"] {
            assert!(statuses.iter().any(|branch| {
                branch["properties"]["status"]["enum"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|value| value == status))
            }));
        }
        let warning = statuses
            .iter()
            .find(|branch| branch["properties"]["status"]["enum"][0] == "warning")
            .unwrap();
        for field in [
            "duplicates",
            "surface_match_page",
            "matched_entry_contexts",
            "status",
        ] {
            assert!(
                warning["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|required| required == field)
            );
        }
        assert_eq!(warning["properties"]["duplicates"]["maxItems"], 0);
        assert_eq!(schemas["DuplicateWordMatchV2"]["deprecated"], true);
        // duplicate 与 warning 两条路径的信息量必须一致：前者也要带命中原因。
        assert!(
            schemas["DuplicateWordMatchV2"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "match_category")
        );
        assert_eq!(
            schemas["DuplicateWordMatchV2"]["properties"]["match_category"]["$ref"],
            "#/components/schemas/SurfaceMatchCategoryV2"
        );
        // 被引用上下文同理：duplicate 分支没有 surface_match_page 可挂
        // matched_entry_contexts，摘要必须直接挂在命中项上。
        assert!(
            schemas["DuplicateWordMatchV2"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "inbound_relations")
        );
        assert_eq!(
            schemas["DuplicateWordMatchV2"]["properties"]["inbound_relations"]["$ref"],
            "#/components/schemas/RelationReferenceSummaryV2"
        );

        let snapshot_union = schemas["WordDetectionSnapshotSmartDictionaryV2"]["oneOf"]
            .as_array()
            .expect("persisted smart dictionary status must be a union");
        assert_eq!(snapshot_union.len(), 2);
        let clear = snapshot_union
            .iter()
            .find(|branch| branch["properties"]["smart_dictionary_status"]["enum"][0] == "clear")
            .unwrap();
        assert_eq!(clear["properties"]["surface_warning"]["type"], "null");
        assert_eq!(clear["additionalProperties"], false);
        let persisted_warning = snapshot_union
            .iter()
            .find(|branch| branch["properties"]["smart_dictionary_status"]["enum"][0] == "warning")
            .unwrap();
        assert!(
            persisted_warning["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "surface_warning")
        );
        assert_eq!(persisted_warning["additionalProperties"], false);

        let aggregate_snapshot = schemas["WordDetectionSnapshotV2"]["oneOf"]
            .as_array()
            .expect("persisted detection snapshot must expose complete strict branches");
        assert_eq!(aggregate_snapshot.len(), 2);
        for branch in aggregate_snapshot {
            assert_eq!(branch["additionalProperties"], false);
            assert!(branch["properties"]["detection_id"].is_object());
            assert!(branch["properties"]["smart_dictionary_status"].is_object());
        }
        let aggregate_clear = aggregate_snapshot
            .iter()
            .find(|branch| branch["properties"]["smart_dictionary_status"]["enum"][0] == "clear")
            .unwrap();
        assert_eq!(
            aggregate_clear["properties"]["surface_warning"]["type"],
            "null"
        );
        assert!(
            !aggregate_clear["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "surface_warning")
        );
        assert_eq!(
            schemas["DetectionSurfaceWarningAuditV2"]["properties"]["acknowledged"]["enum"],
            serde_json::json!([true])
        );
        let preview_required = schemas["DetectionSurfaceMatchPreviewV2"]["required"]
            .as_array()
            .expect("warning preview context fields must be required");
        for field in [
            "existing_word_id",
            "existing_kind",
            "existing_status",
            "existing_dialect",
            "pos_labels",
            "gloss_previews",
        ] {
            assert!(preview_required.iter().any(|required| required == field));
        }
        assert!(
            schemas["AdminWordListItem"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "dialects")
        );
        assert_eq!(
            schemas["DetectionSurfaceWarningAuditV2"]["additionalProperties"],
            false
        );

        let page_variants = schemas["SurfaceMatchPageV2"]["oneOf"].as_array().unwrap();
        assert_eq!(page_variants.len(), 3);
        for branch in page_variants {
            assert_eq!(branch["additionalProperties"], false);
            assert!(branch["properties"]["snapshot_id"].is_object());
            assert!(branch["properties"]["continuation_policy"].is_object());
            assert!(branch["allOf"].is_null());
        }
        let terminal_page = page_variants
            .iter()
            .find(|branch| branch["properties"]["surface_confirmation_token"].is_object())
            .expect("enabled terminal page must be present");
        assert_eq!(terminal_page["properties"]["next_cursor"]["type"], "null");
        assert_eq!(
            schemas["LexiconSurfaceMatchV2"]["properties"]["can_continue"]["enum"],
            serde_json::json!([true])
        );
        assert_eq!(
            schemas["WordFormTypeV2"]["enum"],
            serde_json::json!([
                "base",
                "third_person_singular",
                "present_participle",
                "past_tense",
                "past_participle",
                "plural",
                "comparative",
                "superlative"
            ])
        );
        assert_eq!(
            schemas["SurfaceMatchCategoryV2"]["enum"],
            serde_json::json!([
                "exact_headword",
                "cross_kind_headword",
                "headword_form",
                "form_headword",
                "form_form",
                "headword_relation"
            ])
        );
        let relation_source = schemas["ExistingSurfaceSourceV2"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| branch["properties"]["source_kind"]["enum"][0] == "relation")
            .expect("关联词维度必须有独立的 source_kind 分支");
        for field in [
            "relation_type",
            "referencing_word_id",
            "referencing_headword",
            "referencing_status",
        ] {
            assert!(
                relation_source["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|required| required == field),
                "relation 分支必须带 {field}，前端才说得出是近义词还是反义词"
            );
        }
        assert_eq!(
            relation_source["properties"]["relation_type"]["$ref"],
            "#/components/schemas/RelationTypeV2"
        );
        assert_eq!(
            schemas["SurfaceMatchPageBaseV2"]["properties"]["items"]["minItems"],
            1
        );
        assert_eq!(
            schemas["SurfaceMatchPageBaseV2"]["properties"]["items"]["maxItems"],
            50
        );
        for schema_name in [
            "SurfaceMatchPageBaseV2",
            "LexiconSurfaceMatchV2",
            "ExistingSurfaceMatchV2",
            "MatchedEntryContextV2",
            "RelationReferencePreviewV2",
            "RelationReferenceSummaryV2",
        ] {
            assert_eq!(
                schemas[schema_name]["additionalProperties"], false,
                "{schema_name} 必须拒绝未知字段"
            );
        }
        let relation_preview = &schemas["RelationReferencePreviewV2"];
        assert!(
            relation_preview["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "source_status")
        );
        assert_eq!(
            relation_preview["properties"]["source_status"]["$ref"],
            "#/components/schemas/AdminWordStatus"
        );
        for schema_name in [
            "SurfaceMatchEnabledNextPageV2",
            "SurfaceMatchEnabledTerminalPageV2",
            "SurfaceMatchTemporarilyDisabledPageV2",
        ] {
            assert_eq!(schemas[schema_name]["additionalProperties"], false);
            assert!(schemas[schema_name]["allOf"].is_null());
        }
        for schema_name in [
            "SmartDictionaryResultV2",
            "SurfaceMatchCandidateV2",
            "ExistingSurfaceSourceV2",
        ] {
            for branch in schemas[schema_name]["oneOf"].as_array().unwrap() {
                assert_eq!(
                    branch["additionalProperties"], false,
                    "{schema_name} 的每个 tagged-union 分支必须拒绝未知字段"
                );
            }
        }

        assert!(
            schemas["CreateAdminWordV2Input"]["properties"]["confirmed_surface_match_token"]
                .is_object()
        );
        let save_forms = &schemas["SaveFormsStepInput"];
        assert_eq!(save_forms["additionalProperties"], false);
        assert!(save_forms["properties"]["confirmed_surface_match_token"].is_object());
        assert_eq!(
            save_forms["properties"]["confirmed_impact_token"]["format"],
            "uuid"
        );
        assert!(
            schemas["SaveMeaningsStepInput"]["properties"]["confirmed_surface_match_token"]
                .is_null(),
            "surface token 只能属于 Forms save"
        );
        assert!(
            schemas["SaveMeaningsStepInput"]["properties"]["confirmed_impact_token"].is_null(),
            "impact token 只能属于 Forms save"
        );
        assert!(schemas["FormsImpactResponseV2"]["properties"]["surface_match_page"].is_object());
        assert_eq!(
            terminal_page["properties"]["impact_confirmation_token"]["format"],
            "uuid"
        );
        assert!(
            json["paths"]["/api/v1/admin/lexicon/entries/{id}/steps/forms"]["put"]
                ["responses"]["410"]
                .is_object()
        );
        assert!(
            json["paths"]["/api/v1/admin/lexicon/entries/{id}/steps/forms/impact"]["post"]
                ["responses"]["410"]
                .is_null(),
            "impact preview 不消费 snapshot token，不应声明不可达的 410"
        );
        let problem_meta = &schemas["ProblemMeta"]["properties"];
        for field in [
            "surface_match_page",
            "current_policy_name",
            "current_policy_epoch",
        ] {
            assert!(problem_meta[field].is_object(), "ProblemMeta 缺少 {field}");
        }

        let error_codes = schemas["ErrorCode"]["enum"].as_array().unwrap();
        for code in [
            "surface_match_acknowledgement_required",
            "surface_matches_changed",
            "surface_match_snapshot_expired",
            "surface_policy_changed",
            "exact_headword_creation_temporarily_disabled",
            "multiple_active_exact_headword_publications_not_enabled",
        ] {
            assert!(error_codes.iter().any(|value| value == code));
        }
    }

    #[test]
    fn problem_details_field_issues_use_the_versioned_draft_issue_union() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert_eq!(
            json["components"]["schemas"]["ProblemDetails"]["properties"]["field_issues"]["items"]
                ["$ref"],
            "#/components/schemas/DraftValidationIssueAny"
        );
        let schemas = &json["components"]["schemas"];
        assert_eq!(
            schemas["DraftValidationIssueAny"]["discriminator"]["propertyName"],
            "schema_version"
        );
        assert_eq!(
            schemas["DraftValidationIssueAny"]["oneOf"],
            serde_json::json!([
                {"$ref": "#/components/schemas/DraftValidationIssueV2"},
                {"$ref": "#/components/schemas/V3DraftValidationIssue"}
            ])
        );
        let issue = &schemas["DraftValidationIssueV2"];
        for field in [
            "schema_version",
            "step",
            "node_id",
            "field",
            "code",
            "message",
            "reference_location",
            "node_location",
        ] {
            assert!(
                issue["properties"][field].is_object(),
                "DraftValidationIssue 应稳定暴露 {field}"
            );
        }
        assert_eq!(
            schemas["V3DraftValidationIssue"]["properties"]["code"]["$ref"],
            "#/components/schemas/V3ValidationIssueCode"
        );
        assert_eq!(
            schemas["ProblemMeta"]["properties"]["surface_match_page"]["$ref"],
            "#/components/schemas/SurfaceMatchPageAny"
        );
    }

    #[test]
    fn catalog_contract_is_documented() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();

        for (method, path, status, schema) in [
            (
                "get",
                "/api/v1/admin/settings/parts-of-speech/catalog",
                "200",
                Some("CatalogResponse"),
            ),
            (
                "get",
                "/api/v1/admin/settings/parts-of-speech",
                "200",
                Some("PaginatedResponse_PartOfSpeechConfig"),
            ),
            (
                "post",
                "/api/v1/admin/settings/parts-of-speech",
                "201",
                Some("PartOfSpeechConfig"),
            ),
            (
                "patch",
                "/api/v1/admin/settings/parts-of-speech/{id}",
                "200",
                Some("PartOfSpeechConfig"),
            ),
            (
                "delete",
                "/api/v1/admin/settings/parts-of-speech/{id}",
                "204",
                None,
            ),
            (
                "get",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts",
                "200",
                Some("SubPartListResponse"),
            ),
            (
                "post",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts",
                "201",
                Some("SubPartOfSpeechConfig"),
            ),
            (
                "patch",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
                "200",
                Some("SubPartOfSpeechConfig"),
            ),
            (
                "delete",
                "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
                "204",
                None,
            ),
        ] {
            let operation = &json["paths"][path][method];
            assert_eq!(
                operation["security"][0]["bearer_auth"],
                serde_json::json!([]),
                "{method} {path} 必须声明管理员 Bearer 鉴权"
            );
            let response = &operation["responses"][status];
            assert!(
                response.is_object(),
                "{method} {path} 应声明成功状态 {status}"
            );
            match schema {
                Some(schema) => assert_eq!(
                    response["content"]["application/json"]["schema"]["$ref"],
                    format!("#/components/schemas/{schema}"),
                    "{method} {path} 成功响应 schema 漂移"
                ),
                None => assert!(
                    response.get("content").is_none(),
                    "{method} {path} 的 204 不得声明响应 body"
                ),
            }
        }

        for path in [
            "/api/v1/admin/settings/parts-of-speech/{id}",
            "/api/v1/admin/settings/parts-of-speech/{id}/sub-parts/{sub_id}",
        ] {
            let parameters = json["paths"][path]["delete"]["parameters"]
                .as_array()
                .expect("DELETE 应声明路径和 revision 参数");
            assert!(parameters.iter().any(|parameter| {
                parameter["name"] == "base_revision"
                    && parameter["in"] == "query"
                    && parameter["required"] == true
                    && parameter["schema"]["minimum"] == 1
            }));
        }

        for schema in [
            "CreatePartRequest",
            "UpdatePartRequest",
            "CreateSubPartRequest",
            "UpdateSubPartRequest",
        ] {
            assert_eq!(
                json["components"]["schemas"][schema]["additionalProperties"], false,
                "{schema} 必须拒绝未知或只读字段"
            );
        }

        for schema in ["UpdatePartRequest", "UpdateSubPartRequest"] {
            assert_eq!(
                json["components"]["schemas"][schema]["properties"]["base_revision"]["minimum"], 1,
                "{schema}.base_revision 必须在 OpenAPI 中声明正整数下界"
            );
        }

        for (schema, pattern) in [
            ("CreatePartRequest", "^[a-z][a-z0-9_]{0,31}$"),
            ("CreateSubPartRequest", "^[A-Z][A-Z0-9_-]{0,31}$"),
        ] {
            let code = &json["components"]["schemas"][schema]["properties"]["code"];
            assert_eq!(code["minLength"], 1);
            assert_eq!(code["maxLength"], 32);
            assert_eq!(code["pattern"], pattern);
        }

        let create_part_properties =
            &json["components"]["schemas"]["CreatePartRequest"]["properties"];
        for field in ["name_zh", "name_en"] {
            assert_eq!(create_part_properties[field]["minLength"], 1);
            assert_eq!(create_part_properties[field]["maxLength"], 64);
        }
        assert_eq!(create_part_properties["abbreviation"]["minLength"], 1);
        assert_eq!(create_part_properties["abbreviation"]["maxLength"], 16);

        let error_variants = json["components"]["schemas"]["ErrorCode"]["enum"]
            .as_array()
            .expect("ErrorCode 应生成枚举 schema");
        for code in [
            "invalid_path_parameter",
            "invalid_part_of_speech",
            "part_of_speech_not_found",
            "sub_part_of_speech_not_found",
            "part_of_speech_conflict",
            "sub_part_of_speech_conflict",
            "revision_conflict",
            "part_of_speech_in_use",
            "sub_part_of_speech_in_use",
        ] {
            assert!(
                error_variants.iter().any(|value| value == code),
                "ErrorCode schema 缺少 {code}"
            );
        }
    }

    /// cookie 契约必须写进 spec：refresh/logout 的入参是 Cookie 而非 body——
    /// 不声明的话，照 swagger 生成的客户端会以为这两个 POST 无需任何凭证，调用必 401。
    /// login/login-otp/refresh 的 200 同理要声明 Set-Cookie 响应头。
    #[test]
    fn cookie_contract_is_documented() {
        let spec = ApiDoc::openapi();
        let json = serde_json::to_value(&spec).expect("spec 应能序列化为 JSON");

        // refresh 与 logout 都声明了名为 refresh_token 的 Cookie 参数
        for path in ["/api/v1/auth/refresh", "/api/v1/auth/logout"] {
            let params = json["paths"][path]["post"]["parameters"]
                .as_array()
                .unwrap_or_else(|| panic!("{path} 应声明 parameters（refresh_token cookie）"));
            assert!(
                params
                    .iter()
                    .any(|p| p["name"] == "refresh_token" && p["in"] == "cookie"),
                "{path} 的 parameters 里应有 in=cookie 的 refresh_token，实际：{params:?}"
            );
        }

        // 下发 refresh cookie 的三个成功响应都声明了 Set-Cookie 头
        for (path, status) in [
            ("/api/v1/auth/login", "200"),
            ("/api/v1/auth/login-otp", "200"),
            ("/api/v1/auth/refresh", "200"),
            ("/api/v1/auth/logout", "204"),
        ] {
            assert!(
                json["paths"][path]["post"]["responses"][status]["headers"]["Set-Cookie"]
                    .is_object(),
                "{path} 的 {status} 响应应声明 Set-Cookie 头"
            );
        }

        // logout 已幂等化：spec 里不得再出现 401
        assert!(
            json["paths"]["/api/v1/auth/logout"]["post"]["responses"]["401"].is_null(),
            "logout 无失败分支，401 应从 spec 移除"
        );

        // —— admin 域同样的 cookie 契约（名字/路径与 C 端隔离，见 ADMIN_REFRESH_TOKEN_COOKIE）——
        // admin refresh 与 logout 都声明了 admin_refresh_token cookie 参数
        for path in ["/api/v1/admin/auth/refresh", "/api/v1/admin/auth/logout"] {
            let params = json["paths"][path]["post"]["parameters"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("{path} 应声明 parameters（admin_refresh_token cookie）")
                });
            assert!(
                params
                    .iter()
                    .any(|p| p["name"] == "admin_refresh_token" && p["in"] == "cookie"),
                "{path} 的 parameters 里应有 in=cookie 的 admin_refresh_token，实际：{params:?}"
            );
        }

        // admin login 200 / refresh 200 / logout 204 都声明了 Set-Cookie 头
        for (path, status) in [
            ("/api/v1/admin/auth/login", "200"),
            ("/api/v1/admin/auth/refresh", "200"),
            ("/api/v1/admin/auth/logout", "204"),
        ] {
            assert!(
                json["paths"][path]["post"]["responses"][status]["headers"]["Set-Cookie"]
                    .is_object(),
                "{path} 的 {status} 响应应声明 Set-Cookie 头"
            );
        }

        // admin login-code 的反枚举契约：只有 202 成功态，绝不能出现 401/403/429 等
        // 可探测态（那会把「这号是不是管理员」暴露成 oracle）。
        for leaky in ["401", "403", "423", "429"] {
            assert!(
                json["paths"]["/api/v1/admin/auth/login-code"]["post"]["responses"][leaky]
                    .is_null(),
                "admin login-code 反枚举契约：不得声明可探测状态码 {leaky}"
            );
        }
    }

    #[test]
    fn account_deletion_contract_is_documented() {
        let value = serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI 应可序列化");
        let request = &value["paths"]["/api/v1/auth/account/deletion-code"]["post"];
        let confirm = &value["paths"]["/api/v1/auth/account"]["delete"];

        assert_eq!(
            request["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/AccountDeletionCodeRequest"
        );
        assert_eq!(
            confirm["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ConfirmAccountDeletionRequest"
        );
        assert!(confirm["responses"]["401"]["content"]["application/problem+json"].is_object());
        for status in ["400", "401", "409", "422", "500", "503"] {
            assert!(
                confirm["responses"][status]["content"]["application/problem+json"].is_object(),
                "确认注销应声明 {status} Problem Details"
            );
        }
        for status in ["400", "401", "409", "422", "429", "500", "503"] {
            assert!(
                request["responses"][status]["content"]["application/problem+json"].is_object(),
                "申请注销码应声明 {status} Problem Details"
            );
        }
        assert!(confirm["responses"]["204"]["headers"]["Set-Cookie"].is_object());
        assert!(request["security"].is_array() && confirm["security"].is_array());
    }

    #[test]
    fn related_search_v2_contract_has_registered_and_required_pagination_schemas() {
        let json = serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI 应可序列化");
        let schemas = &json["components"]["schemas"];
        assert!(schemas["RelatedSearchMatchMode"].is_object());
        assert!(schemas["RelatedSearchLegacyResponse"].is_object());
        assert!(schemas["RelatedSearchV2Response"].is_object());
        assert_eq!(
            schemas["RelatedSearchLegacyResponse"]["additionalProperties"],
            false
        );
        assert_eq!(
            schemas["RelatedSearchV2Response"]["additionalProperties"],
            false
        );

        let required = schemas["RelatedSearchV2Response"]["required"]
            .as_array()
            .expect("V2 response 应声明 required");
        for field in ["results", "total", "next_cursor"] {
            assert!(
                required.iter().any(|value| value == field),
                "V2 response 缺少 required 字段 {field}"
            );
        }
        assert_eq!(
            schemas["RelatedSearchV2Response"]["properties"]["next_cursor"]["type"],
            serde_json::json!(["string", "null"])
        );
    }

    #[test]
    fn error_contract_is_documented() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert!(json["components"]["schemas"]["ErrorCode"].is_object());
        assert!(json["components"]["schemas"]["ProblemDetails"].is_object());

        let properties = &json["components"]["schemas"]["ProblemDetails"]["properties"];
        for field in ["type", "title", "status", "detail", "code", "field", "meta"] {
            assert!(properties[field].is_object(), "ProblemDetails 缺少 {field}");
        }
        assert!(json["components"]["schemas"]["ProblemMeta"].is_object());
        assert!(properties["error"].is_null());

        let schema = &json["paths"]["/api/v1/auth/register"]["post"]["responses"]["400"]["content"]
            ["application/problem+json"]["schema"];
        assert_eq!(schema["$ref"], "#/components/schemas/ProblemDetails");

        for (path_name, path) in json["paths"].as_object().unwrap() {
            for (method, operation) in path.as_object().unwrap() {
                if ![
                    "get", "put", "post", "delete", "options", "head", "patch", "trace",
                ]
                .contains(&method.as_str())
                {
                    continue;
                }
                for (status, response) in operation["responses"].as_object().unwrap() {
                    if status
                        .parse::<u16>()
                        .is_ok_and(|status| (400..600).contains(&status))
                    {
                        assert_eq!(
                            response["content"]["application/problem+json"]["schema"]["$ref"],
                            "#/components/schemas/ProblemDetails",
                            "{method} {path_name} 的 {status} 应使用 ProblemDetails"
                        );
                        assert!(
                            response["content"]["application/json"].is_null(),
                            "错误响应不得继续声明 application/json"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn speech_preview_contract_is_documented_without_server_owned_fields() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert!(json["paths"]["/api/v1/admin/speech/voices"]["get"].is_object());
        assert!(json["paths"]["/api/v1/admin/speech/previews"]["post"].is_object());

        let request = &json["components"]["schemas"]["CreatePreviewRequest"]["properties"];
        for field in [
            "content",
            "voice_alias",
            "style",
            "rate_percent",
            "pitch_semitones",
        ] {
            assert!(request[field].is_object(), "试听请求缺少 {field}");
        }
        for forbidden in [
            "ssml",
            "provider_voice_id",
            "request_hash",
            "object_key",
            "audio",
            "audio_url",
        ] {
            assert!(request[forbidden].is_null(), "试听请求不得暴露 {forbidden}");
        }

        let voice = &json["components"]["schemas"]["VoiceResponse"]["properties"];
        assert!(voice["alias"].is_object());
        assert!(voice["provider"].is_null());
        assert!(voice["provider_voice_id"].is_null());
    }

    /// Approved C1 matrix: C01/C02/C05/C06, I12/I13/I14/I16 and R01a's
    /// contract-only publication gate. Successful V3 persistence/migration is deliberately C2.
    #[test]
    fn smart_lexicon_v3_c1_contract_is_versioned_closed_and_fail_closed() {
        let json = serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI 应可序列化");
        let schemas = &json["components"]["schemas"];

        assert_eq!(
            schemas["AdminWordV2"]["properties"]["schema_version"]["enum"],
            serde_json::json!([2])
        );
        assert_eq!(
            schemas["AdminWordV3"]["properties"]["schema_version"]["enum"],
            serde_json::json!([3])
        );
        assert_eq!(
            schemas["AdminWordAny"]["oneOf"],
            serde_json::json!([
                {"$ref": "#/components/schemas/AdminWordV2"},
                {"$ref": "#/components/schemas/AdminWordV3"}
            ])
        );
        assert_eq!(
            schemas["AdminWordAny"]["discriminator"],
            serde_json::json!({
                "propertyName": "schema_version",
                "mapping": {
                    "2": "#/components/schemas/AdminWordV2",
                    "3": "#/components/schemas/AdminWordV3"
                }
            })
        );

        assert_eq!(
            schemas["WordFormTypeV3"]["enum"],
            serde_json::json!([
                "base",
                "third_person_singular",
                "present_participle",
                "past_tense",
                "past_participle",
                "plural",
                "comparative",
                "superlative"
            ])
        );
        assert_eq!(
            schemas["WordEntryKindV3"]["enum"],
            serde_json::json!(["word", "phrase"])
        );
        assert_eq!(
            schemas["DialectModeV3"]["enum"],
            serde_json::json!(["unified", "distinguish"])
        );
        assert_eq!(
            schemas["WordPosFormsV3"]["properties"]["dialect_rules"]["$ref"],
            "#/components/schemas/DialectRulesV3"
        );
        assert!(
            schemas["WordPosFormsV3"]["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "dialect_rules"))
        );
        assert_eq!(
            schemas["PronunciationNormalizationVersionV3"]["enum"],
            serde_json::json!(["nfkc_trim_lower_v1"])
        );
        let surface_match_branches = schemas["SurfaceMatchItemV3"]["oneOf"]
            .as_array()
            .expect("V3 surface match must be a oneOf union");
        assert_eq!(surface_match_branches.len(), 2);
        assert_eq!(
            surface_match_branches
                .iter()
                .map(|branch| {
                    branch["properties"]["match_kind"]["enum"][0]
                        .as_str()
                        .unwrap()
                })
                .collect::<std::collections::BTreeSet<_>>(),
            ["form_variant_v3", "legacy_v2"].into_iter().collect()
        );
        assert!(surface_match_branches.iter().all(|branch| {
            branch["additionalProperties"] == false
                && branch["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|field| field == "match_kind"))
        }));
        assert_eq!(
            schemas["LegacySurfaceMatchV3"]["additionalProperties"],
            false
        );
        assert_eq!(schemas["FormSurfaceMatchV3"]["additionalProperties"], false);
        assert_eq!(
            schemas["FormSurfaceMatchV3"]["properties"]["entry_kind"]["$ref"],
            "#/components/schemas/WordEntryKindV3"
        );
        assert!(
            schemas["FormSurfaceMatchV3"]["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "entry_kind"))
        );
        for schema in [
            "AdminWordV3",
            "AdminWordAnyEnvelope",
            "AdminWordDraftV3Envelope",
            "EntryLifecycleBatchResponseAny",
            "WordConcreteFormV3",
            "WordFormGroupMemberV3",
            "WordFormGroupV3",
            "DialectRulesV3",
            "WordPosFormsV3",
            "WordPronunciationV3",
            "DraftFormsStepContentV3",
            "RichTextVariantV3",
            "GrammarVariantV3",
            "GrammarStructureV3",
            "WordSentenceLinkV3",
            "WordSentenceV3",
            "WordSenseV3",
            "SenseGroupV3",
            "WordPosMeaningsV3",
            "DraftMeaningsStepContentV3",
            "AdminWordListItemV3",
            "AdminWordListResponse",
            "FormsImpactItemV3",
            "FormsImpactResponseV3",
            "DraftValidationResponseV3",
            "AdminWordPublicationV2",
            "AdminWordPublicationV3",
            "AdminWordPublicationEnvelope",
            "AdminWordPublicationListResponse",
        ] {
            assert_eq!(
                schemas[schema]["additionalProperties"], false,
                "{schema} 必须拒绝未声明字段"
            );
        }
        assert_eq!(
            schemas["AdminWordV3"]["properties"]["meanings"]["$ref"],
            "#/components/schemas/DraftMeaningsStepContentV3"
        );
        for (name, schema) in schemas.as_object().unwrap() {
            if !name.contains("V3") {
                continue;
            }
            if schema["type"] == "object" {
                assert_eq!(
                    schema["additionalProperties"], false,
                    "V3 object schema {name} 必须递归关闭 extra keys"
                );
            }
            if let Some(branches) = schema["oneOf"].as_array() {
                for branch in branches.iter().filter(|branch| branch["type"] == "object") {
                    assert_eq!(
                        branch["additionalProperties"], false,
                        "V3 union {name} 的 inline object branch 必须关闭 extra keys"
                    );
                }
            }
        }
        assert_eq!(
            schemas["SaveMeaningsStepInputV3"]["properties"]["content"]["$ref"],
            "#/components/schemas/DraftMeaningsStepContentWritableV3"
        );
        let sentence = &schemas["WordSentenceV3"];
        let sentence_required = sentence["required"]
            .as_array()
            .expect("V3 sentence response fields must be required");
        for field in ["zh_translations", "associations", "associations_state"] {
            assert!(
                sentence_required.iter().any(|required| required == field),
                "{field} must remain required in sentence responses"
            );
            assert!(sentence["properties"][field].is_object());
            if field != "zh_translations" {
                assert!(
                    schemas["WordSentenceWritableV3"]["properties"][field].is_null(),
                    "{field} must not exist in the strict meanings request shape"
                );
            }
        }
        // 变体级与候选级成分用词 B2 起停输出 / 恒空，spec 里必须先松成可选，
        // 否则前端 runtime schema 会在那次上线时集体 missing_required_property。
        for schema in [
            "WordCommonFormVariantV3",
            "WordUkFormVariantV3",
            "WordUsFormVariantV3",
            "PublishedSentenceTargetCandidateV3",
        ] {
            assert!(
                schemas[schema]["properties"]["component_usages"].is_object(),
                "{schema} must keep serialising component_usages during B1"
            );
            assert!(
                schemas[schema]["required"]
                    .as_array()
                    .is_none_or(|required| required
                        .iter()
                        .all(|field| field != "component_usages")),
                "{schema} must not require component_usages any more"
            );
        }
        // 释义级成分用词：请求与响应两侧同形、可选、单 sense 上限 100。
        for schema in [
            "WordSenseV3",
            "WordSenseWritableV3",
            "SentenceTargetSenseV3",
        ] {
            assert_eq!(
                schemas[schema]["properties"]["component_usages"]["maxItems"], 100,
                "{schema}.component_usages must declare the per-sense limit"
            );
            assert!(
                schemas[schema]["required"]
                    .as_array()
                    .is_none_or(|required| required
                        .iter()
                        .all(|field| field != "component_usages")),
                "{schema}.component_usages must stay optional"
            );
        }
        assert!(
            schemas["AdminWordV3Capabilities"]["properties"]["sense_component_usages"].is_object(),
            "释义级成分用词能力位必须出现在 capabilities schema 里"
        );
        assert!(
            schemas["AdminWordV3Capabilities"]["required"]
                .as_array()
                .is_none_or(|required| required
                    .iter()
                    .all(|field| field != "sense_component_usages")),
            "能力位关闭时该键缺席，因此不能是 required"
        );
        let response_relations = schemas["WordRelationV3"]["oneOf"]
            .as_array()
            .expect("relation response must be a strict union");
        let writable_relations = schemas["WordRelationWritableV3"]["oneOf"]
            .as_array()
            .expect("relation request must be a strict union");
        for field in ["target_headword", "target_gloss"] {
            assert!(
                writable_relations
                    .iter()
                    .all(|branch| branch["properties"][field].is_null()),
                "{field} must not exist in strict meanings request branches"
            );
            assert!(
                response_relations
                    .iter()
                    .any(|branch| branch["properties"][field].is_object()),
                "{field} must remain available in a bound relation response"
            );
        }
        assert_eq!(
            schemas["WordRelationV2"]["properties"]["pending_target_gloss"]["maxLength"],
            5000
        );
        for (name, branches) in [
            ("WordRelationV3", response_relations),
            ("WordRelationWritableV3", writable_relations),
        ] {
            assert!(
                branches
                    .iter()
                    .all(|branch| branch["additionalProperties"] == false),
                "{name} 的每个互斥分支都必须拒绝未声明字段"
            );
            assert!(
                branches.iter().any(|branch| {
                    branch["properties"]["pending_target_gloss"]["maxLength"] == 5000
                }),
                "{name} must retain the optional pending gloss contract"
            );
        }
        let association_input = &schemas["SentenceAssociationInputV2"];
        let association_required = association_input["required"]
            .as_array()
            .expect("例句关联输入应声明公共必填字段");
        for field in ["id", "source_dialect", "source_range"] {
            assert!(association_required.iter().any(|value| value == field));
        }
        for field in ["target_word_id", "target_sense_id"] {
            assert!(
                !association_required.iter().any(|value| value == field),
                "linked target 在 Pending 扩展后必须是互斥可选字段"
            );
        }
        for field in [
            "pending_target_kind",
            "pending_target_headword",
            "pending_target_gloss",
        ] {
            assert!(association_input["properties"][field].is_object());
        }
        assert_eq!(
            schemas["WordSentenceAssociationV2"]["discriminator"]["propertyName"],
            "state"
        );
        let association_v2_branches = schemas["WordSentenceAssociationV2"]["oneOf"]
            .as_array()
            .expect("V2 associations must be a strict linked/pending union");
        for (state, required, forbidden) in [
            (
                "linked",
                [
                    "target_word_id",
                    "target_sense_id",
                    "target_headword",
                    "target_gloss",
                    "resolved_pos",
                ]
                .as_slice(),
                ["pending_target_kind", "pending_target_headword"].as_slice(),
            ),
            (
                "pending",
                [
                    "pending_target_kind",
                    "pending_target_headword",
                    "normalized_pending_target_headword",
                ]
                .as_slice(),
                ["target_word_id", "target_sense_id"].as_slice(),
            ),
        ] {
            let branch = association_v2_branches
                .iter()
                .find(|branch| branch["properties"]["state"]["enum"][0] == state)
                .unwrap_or_else(|| panic!("missing V2 association {state} branch"));
            let branch_required = branch["required"]
                .as_array()
                .expect("V2 association branch must declare required fields");
            for field in required {
                assert!(
                    branch_required.iter().any(|value| value == field),
                    "V2 association {state} branch must require {field}"
                );
            }
            for field in forbidden {
                assert!(
                    branch["properties"][field].is_null(),
                    "V2 association {state} branch must not expose {field}"
                );
            }
        }
        assert_eq!(
            schemas["WordSentenceAssociationV3"]["discriminator"]["propertyName"],
            "state"
        );
        let association_states = schemas["WordSentenceAssociationV3"]["oneOf"]
            .as_array()
            .expect("V3 associations must be a strict linked/pending union")
            .iter()
            .filter_map(|branch| branch["properties"]["state"]["enum"][0].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            association_states,
            ["linked", "pending"].into_iter().collect()
        );
        assert_eq!(
            schemas["PendingSentenceAssociationListResponse"]["properties"]["results"]["items"]["$ref"],
            "#/components/schemas/PendingSentenceAssociationItemV3"
        );
        for schema in [
            "SentenceAssociationInputV3",
            "PendingSentenceAssociationItemV3",
        ] {
            assert!(schemas[schema]["properties"]["source_segments"].is_object());
            assert!(schemas[schema]["properties"]["source_range"].is_null());
        }
        assert!(
            schemas["WordSentenceAssociationV3"]["oneOf"]
                .as_array()
                .is_some_and(|branches| branches.iter().all(|branch| {
                    branch["properties"]["source_segments"].is_object()
                        && branch["properties"]["source_range"].is_null()
                }))
        );
        for schema in ["RichTextV1V3", "RichTextV2V3"] {
            assert_eq!(schemas[schema]["properties"]["text"]["maxLength"], 200);
        }
        for (schema, field) in [
            ("RichTextV1V3", "spans"),
            ("RichTextV1V3", "liaisons"),
            ("RichTextV2V3", "annotations"),
        ] {
            assert_eq!(schemas[schema]["properties"][field]["maxItems"], 2000);
        }
        assert!(
            schemas["RichTextAnnotationV3"]["oneOf"]
                .as_array()
                .is_some_and(|branches| branches.len() == 5
                    && branches
                        .iter()
                        .all(|branch| branch["additionalProperties"] == false))
        );
        assert_eq!(
            schemas["AdminWordV3Compatibility"]["properties"]["legacy_headwords"]["$ref"],
            "#/components/schemas/LegacyHeadwordsCompatibilityV3"
        );
        assert_eq!(
            schemas["RelatedWordResultV3"]["properties"]["senses"]["items"]["$ref"],
            "#/components/schemas/RelatedWordSenseV3"
        );
        for schema in [
            "AdminWordListPage",
            "ProblemDetails",
            "ProblemMeta",
            "ProblemReferenceLocation",
        ] {
            assert_eq!(schemas[schema]["additionalProperties"], false);
        }
        let pronunciation = &schemas["WordPronunciationV3"];
        assert!(
            !pronunciation["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "style"),
            "draft pronunciation 必须能表达未选择 style"
        );
        assert_eq!(
            pronunciation["properties"]["dict_phonetic"]["maxLength"],
            200
        );
        assert_eq!(pronunciation["properties"]["actual_pron"]["maxLength"], 200);
        assert_eq!(
            schemas["WordCommonFormVariantV3"]["properties"]["spelling"]["maxLength"],
            200
        );
        assert_eq!(
            schemas["DraftFormsStepContentV3"]["properties"]["pos"]["maxItems"],
            2000
        );
        assert_eq!(
            schemas["AdminWordDraftV3Envelope"]["properties"]["retired_stable_nodes"]["items"]["$ref"],
            "#/components/schemas/RetiredStableNodeV3"
        );
        for node_type in [
            "form_group",
            "membership",
            "form",
            "variant",
            "pronunciation",
            "surface",
            "publication",
        ] {
            assert!(
                schemas["FormsImpactNodeTypeV3"]["enum"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value == node_type)
            );
        }
        assert_eq!(
            schemas["FormsImpactResponseV3"]["properties"]["affected"]["items"]["$ref"],
            "#/components/schemas/FormsImpactItemV3"
        );
        let v3_surface_branches = schemas["SurfaceMatchPageV3"]["oneOf"]
            .as_array()
            .expect("V3 surface page 应为严格三分支");
        assert_eq!(v3_surface_branches.len(), 3);
        assert!(v3_surface_branches.iter().all(|branch| {
            branch["additionalProperties"] == false
                && branch["properties"]["matched_entry_contexts"].is_object()
                && branch["properties"]["confirmation_reasons"].is_object()
                && branch["properties"]["continuation_policy"].is_object()
        }));
        assert!(
            v3_surface_branches
                .iter()
                .any(|branch| { branch["properties"]["policy_block_code"].is_object() })
        );
        for forbidden in [
            "base_form",
            "parent_form_id",
            "derived_from_form_id",
            "sort_order",
            "headwords",
        ] {
            assert!(
                schemas["WordConcreteFormV3"]["properties"][forbidden].is_null(),
                "V3 concrete form 不得暴露 {forbidden}"
            );
            assert!(
                schemas["SaveFormsStepInputV3"]["properties"][forbidden].is_null(),
                "V3 forms 写契约不得暴露 {forbidden}"
            );
        }
        let regional = schemas["WordRegionalVariantsV3"]["oneOf"]
            .as_array()
            .expect("地区形状应为 oneOf");
        assert_eq!(regional.len(), 2);
        assert!(
            regional
                .iter()
                .all(|branch| branch["additionalProperties"] == false)
        );
        assert_eq!(
            schemas["WordRegionalVariantsV3"]["discriminator"]["propertyName"],
            "mode"
        );
        assert_eq!(
            regional[0]["required"],
            serde_json::json!(["common", "mode"])
        );
        assert_eq!(
            regional[1]["required"],
            serde_json::json!(["uk", "us", "mode"])
        );

        let paths = &json["paths"];
        assert_eq!(
            paths["/api/v1/admin/lexicon/detections"]["post"]["requestBody"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/DetectLexiconInputAny"
        );
        assert_eq!(
            paths["/api/v1/admin/lexicon/detections"]["post"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/DetectLexiconResponseAny"
        );
        assert_eq!(
            schemas["DetectLexiconResponseAny"]["discriminator"]["propertyName"],
            "schema_version"
        );
        assert!(
            schemas["DetectWordResponseV2"]["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "schema_version")),
            "discriminator 的 V2 response branch 必须要求 literal schema_version=2"
        );
        let v3_detection = &schemas["DetectLexiconSurfaceResponseV3"]["properties"];
        assert_eq!(
            v3_detection["builtin_dictionary"]["$ref"],
            "#/components/schemas/BuiltinDictionaryEvidenceV3"
        );
        let matched_evidence = schemas["BuiltinDictionaryEvidenceV3"]["oneOf"]
            .as_array()
            .and_then(|branches| {
                branches.iter().find(|branch| {
                    branch["properties"]["status"]["enum"] == serde_json::json!(["matched"])
                })
            })
            .expect("V3 dictionary evidence must expose matched branch");
        for field in [
            "provider",
            "suggested_pos",
            "suggested_forms",
            "coverage",
            "provenance",
        ] {
            assert!(matched_evidence["properties"][field].is_object());
        }
        assert_eq!(
            matched_evidence["properties"]["suggested_forms"]["items"]["$ref"],
            "#/components/schemas/SuggestedConcreteFormV3"
        );
        assert!(paths["/api/v1/admin/lexicon/detections"]["post"]["responses"]["503"].is_object());
        for (method, path, request_schema) in [
            (
                "post",
                "/api/v1/admin/lexicon/entries",
                "CreateAdminWordAnyInput",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/steps/forms/impact",
                "PreviewFormsImpactInputAny",
            ),
            (
                "put",
                "/api/v1/admin/lexicon/entries/{id}/steps/forms",
                "SaveFormsStepInputAny",
            ),
            (
                "put",
                "/api/v1/admin/lexicon/entries/{id}/steps/meanings",
                "SaveMeaningsStepInputAny",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/validate",
                "ValidateAdminWordAnyInput",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/publications",
                "PublishAdminWordAnyInput",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/publications/{publication_id}/activate",
                "ActivatePublicationAnyInput",
            ),
        ] {
            assert_eq!(
                paths[path][method]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
                format!("#/components/schemas/{request_schema}"),
                "{method} {path} 应引用版本化请求"
            );
        }
        for schema in [
            "PreviewFormsImpactInputAny",
            "SaveFormsStepInputAny",
            "SaveMeaningsStepInputAny",
            "ValidateAdminWordAnyInput",
            "PublishAdminWordAnyInput",
            "ActivatePublicationAnyInput",
        ] {
            assert!(schemas[schema]["discriminator"].is_null());
            assert!(
                schemas[schema]["description"]
                    .as_str()
                    .is_some_and(|description| description.contains("V2 body omits schema_version")),
                "{schema} 必须公开说明 legacy V2 body 是兼容例外"
            );
        }

        for (method, path, status, response_schema) in [
            (
                "post",
                "/api/v1/admin/lexicon/entries",
                "201",
                "AdminWordAnyEnvelope",
            ),
            (
                "get",
                "/api/v1/admin/lexicon/entries/{id}",
                "200",
                "AdminWordDraftAnyEnvelope",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/archive",
                "200",
                "AdminWordAnyEnvelope",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/restore",
                "200",
                "AdminWordAnyEnvelope",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/archive-batch",
                "200",
                "EntryLifecycleBatchResponseAny",
            ),
            (
                "post",
                "/api/v1/admin/lexicon/entries/restore-batch",
                "200",
                "EntryLifecycleBatchResponseAny",
            ),
            (
                "get",
                "/api/v1/admin/lexicon/surface-match-snapshots/{snapshot_id}",
                "200",
                "SurfaceMatchPageAny",
            ),
        ] {
            assert_eq!(
                paths[path][method]["responses"][status]["content"]["application/json"]["schema"]["$ref"],
                format!("#/components/schemas/{response_schema}"),
                "{method} {path} 应返回可判别版本"
            );
        }
        assert_eq!(
            schemas["AdminWordListResponse"]["properties"]["words"]["items"]["$ref"],
            "#/components/schemas/AdminWordListItemAny"
        );
        assert_eq!(
            schemas["RelatedSearchV2Response"]["properties"]["results"]["items"]["$ref"],
            "#/components/schemas/RelatedWordResultAny"
        );
        assert_eq!(
            schemas["AdminWordPublicationAny"]["discriminator"]["propertyName"],
            "schema_version"
        );
        for (path, schema) in [
            (
                "/api/v1/admin/lexicon/entries/{id}/publications",
                "AdminWordPublicationListResponse",
            ),
            (
                "/api/v1/admin/lexicon/entries/{id}/publications/{publication_id}",
                "AdminWordPublicationEnvelope",
            ),
        ] {
            assert_eq!(
                paths[path]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
                format!("#/components/schemas/{schema}")
            );
            assert_eq!(
                paths[path]["get"]["responses"]["422"]["content"]["application/problem+json"]["schema"]
                    ["$ref"],
                "#/components/schemas/ProblemDetails"
            );
        }
        for (schema, v2, v3) in [
            (
                "FormsImpactResponseAny",
                "FormsImpactResponseV2",
                "FormsImpactResponseV3",
            ),
            (
                "DraftValidationResponseAny",
                "DraftValidationResponse",
                "DraftValidationResponseV3",
            ),
        ] {
            assert_eq!(
                schemas[schema]["discriminator"]["propertyName"],
                "schema_version"
            );
            assert_eq!(
                schemas[schema]["oneOf"],
                serde_json::json!([
                    {"$ref": format!("#/components/schemas/{v2}")},
                    {"$ref": format!("#/components/schemas/{v3}")}
                ])
            );
        }

        for path in [
            "/api/v1/admin/lexicon/entries",
            "/api/v1/admin/lexicon/entries/{id}/publications",
            "/api/v1/admin/lexicon/entries/{id}/publications/{publication_id}/activate",
        ] {
            let parameters = paths[path]["post"]["parameters"]
                .as_array()
                .expect("命令应声明参数");
            assert!(parameters.iter().any(|parameter| {
                parameter["name"] == "Idempotency-Key"
                    && parameter["in"] == "header"
                    && parameter["required"] == true
            }));
        }

        for field in [
            "form_group_id",
            "membership_id",
            "form_id",
            "variant_id",
            "pronunciation_id",
        ] {
            assert!(schemas["DraftNodeLocation"]["properties"][field].is_object());
        }
        for code in [
            "unsupported_schema_version",
            "stable_node_id_changed",
            "form_reference_conflict",
            "smart_lexicon_v3_storage_unavailable",
            "smart_lexicon_v3_detection_unavailable",
            "smart_lexicon_v3_publication_requires_migration_canary",
        ] {
            assert!(
                schemas["ErrorCode"]["enum"]
                    .as_array()
                    .is_some_and(|codes| codes.iter().any(|value| value == code)),
                "ErrorCode 应冻结 {code}"
            );
        }
        for (path, status) in [
            ("/api/v1/admin/lexicon/entries", "503"),
            ("/api/v1/admin/lexicon/entries/{id}/publications", "409"),
            (
                "/api/v1/admin/lexicon/entries/{id}/publications/{publication_id}/activate",
                "409",
            ),
        ] {
            assert_eq!(
                paths[path]["post"]["responses"][status]["content"]["application/problem+json"]["schema"]
                    ["$ref"],
                "#/components/schemas/ProblemDetails"
            );
        }
        for (method, path) in [
            (
                "post",
                "/api/v1/admin/lexicon/entries/{id}/steps/forms/impact",
            ),
            ("put", "/api/v1/admin/lexicon/entries/{id}/steps/forms"),
        ] {
            let conflict = &paths[path][method]["responses"]["409"];
            assert_eq!(
                conflict["content"]["application/problem+json"]["schema"]["$ref"],
                "#/components/schemas/ProblemDetails"
            );
            let description = conflict["description"].as_str().unwrap();
            assert!(description.contains("stable_node_id_changed"));
            assert!(description.contains("form_reference_conflict"));
        }

        for root in [
            // Writable V3 request closure, including bounded RichText aliases.
            "DetectLexiconSurfaceV3Input",
            "CreateAdminWordV3Input",
            "PreviewFormsImpactInputV3",
            "SaveFormsStepInputV3",
            "SaveMeaningsStepInputV3",
            "ValidateAdminWordV3Input",
            "PublishAdminWordV3Input",
            "ActivatePublicationV3Input",
            // Representative success/read roots cover aggregate, list, related-search,
            // detection evidence, publication and surface snapshots.
            "DetectLexiconSurfaceResponseV3",
            "AdminWordV3",
            "AdminWordDraftV3Envelope",
            "AdminWordListPage",
            "RelatedWordResultV3",
            "FormsImpactResponseV3",
            "DraftValidationResponseV3",
            "AdminWordPublicationV3",
            "SurfaceMatchPageV3",
            // Every V3 operation error is the same RFC 9457 typed root.
            "ProblemDetails",
        ] {
            assert_ref_aware_strict_graph(schemas.as_object().unwrap(), root);
        }
    }
}
