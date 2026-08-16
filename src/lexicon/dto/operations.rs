use super::*;

// --- detection ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectWordInputV2 {
    pub language: String,
    pub headword: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetectionRequestEcho {
    pub language: String,
    pub headword: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(deprecated)]
pub struct DuplicateWordMatchV2 {
    pub word_id: Uuid,
    pub headword: String,
    pub dialect: Dialect,
    pub status: AdminWordStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SmartDictionaryResultV2 {
    Clear {
        #[schema(max_items = 0)]
        duplicates: Vec<DuplicateWordMatchV2>,
    },
    #[schema(deprecated)]
    Duplicate {
        #[schema(min_items = 1)]
        duplicates: Vec<DuplicateWordMatchV2>,
    },
    Warning {
        #[schema(max_items = 0)]
        duplicates: Vec<DuplicateWordMatchV2>,
        surface_match_page: Box<SurfaceMatchPageV2>,
        #[schema(max_items = 0)]
        matched_entry_contexts: Vec<MatchedEntryContextV2>,
    },
    Unavailable {
        #[schema(max_items = 0)]
        duplicates: Vec<DuplicateWordMatchV2>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "candidate_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SurfaceMatchCandidateV2 {
    Headword {
        candidate_ref: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        candidate_word_id: Option<Uuid>,
        surface: String,
        normalized_surface: String,
        dialect: Dialect,
        entry_kind: EntryKind,
    },
    Form {
        candidate_ref: String,
        candidate_word_id: Uuid,
        candidate_node_id: Uuid,
        surface: String,
        normalized_surface: String,
        dialect: Dialect,
        pos_id: Uuid,
        pos: String,
        form_type: WordFormTypeV2,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "source_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExistingSurfaceSourceV2 {
    Headword {
        source_id: String,
        content_scope: SurfaceContentScopeV2,
        surface: String,
        dialect: Dialect,
    },
    Form {
        source_id: String,
        source_node_id: Uuid,
        content_scope: SurfaceContentScopeV2,
        surface: String,
        dialect: Dialect,
        pos_id: Uuid,
        pos: String,
        form_type: WordFormTypeV2,
    },
}

/// Closed wire enum for every explicitly persisted form surface. Unlike the
/// catalog's derived-form capability enum this includes the base slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WordFormTypeV2 {
    Base,
    ThirdPersonSingular,
    PresentParticiple,
    PastTense,
    PastParticiple,
    Plural,
    Comparative,
    Superlative,
}

impl TryFrom<&str> for WordFormTypeV2 {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "base" => Ok(Self::Base),
            "third_person_singular" => Ok(Self::ThirdPersonSingular),
            "present_participle" => Ok(Self::PresentParticiple),
            "past_tense" => Ok(Self::PastTense),
            "past_participle" => Ok(Self::PastParticiple),
            "plural" => Ok(Self::Plural),
            "comparative" => Ok(Self::Comparative),
            "superlative" => Ok(Self::Superlative),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceContentScopeV2 {
    Draft,
    CurrentPublication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceConfirmationReasonV2 {
    UnacknowledgedSurfaceMatches,
    VisibilityActivation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceMatchCategoryV2 {
    ExactHeadword,
    CrossKindHeadword,
    HeadwordForm,
    FormHeadword,
    FormForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceAttentionLevelV2 {
    High,
    Normal,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExistingSurfaceMatchV2 {
    pub word_id: Uuid,
    pub headword: String,
    pub kind: EntryKind,
    pub status: AdminWordStatus,
    pub source: ExistingSurfaceSourceV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LexiconSurfaceMatchV2 {
    pub match_id: String,
    pub match_category: SurfaceMatchCategoryV2,
    pub severity: SurfaceMatchSeverityV2,
    pub attention_level: SurfaceAttentionLevelV2,
    #[schema(schema_with = literal_true_schema)]
    pub can_continue: SurfaceCanContinueTrue,
    #[schema(min_items = 1, max_items = 2)]
    pub confirmation_reasons: Vec<SurfaceConfirmationReasonV2>,
    pub candidate: SurfaceMatchCandidateV2,
    pub existing: ExistingSurfaceMatchV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceCanContinueTrue;

impl Serialize for SurfaceCanContinueTrue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for SurfaceCanContinueTrue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("can_continue must be true"))
        }
    }
}

fn literal_true_schema() -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::Boolean)
        .enum_values(Some([true]))
        .build()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceMatchSeverityV2 {
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationTypeV2 {
    Synonym,
    Antonym,
    Derivative,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationReferenceCountsV2 {
    pub synonym: u32,
    pub antonym: u32,
    pub derivative: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationReferencePreviewV2 {
    pub source_word_id: Uuid,
    pub source_headword: String,
    pub relation: RelationTypeV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationReferenceSummaryV2 {
    pub total: u32,
    pub by_type: RelationReferenceCountsV2,
    #[schema(max_items = 5)]
    pub previews: Vec<RelationReferencePreviewV2>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MatchedEntryContextV2 {
    pub word_id: Uuid,
    #[schema(max_items = 5)]
    pub pos_labels: Vec<String>,
    #[schema(max_items = 5)]
    pub gloss_previews: Vec<String>,
    pub updated_at: DateTime<Utc>,
    pub inbound_relations: RelationReferenceSummaryV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfacePolicyNameV2 {
    SurfaceWarningAcknowledgement,
    AllowNewExactHeadwordEntries,
    AllowMultipleActiveExactHeadwordPublications,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfacePolicyBlockCodeV2 {
    ExactHeadwordCreationTemporarilyDisabled,
    MultipleActiveExactHeadwordPublicationsNotEnabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceContinuationEnabledV2 {
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceContinuationDisabledV2 {
    TemporarilyDisabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchPageBaseV2 {
    pub snapshot_id: Uuid,
    #[schema(min_items = 1, max_items = 50)]
    pub items: Vec<LexiconSurfaceMatchV2>,
    pub total: u64,
    #[schema(min_items = 1, max_items = 50)]
    pub matched_entry_contexts: Vec<MatchedEntryContextV2>,
    #[schema(min_items = 1, max_items = 2)]
    pub confirmation_reasons: Vec<SurfaceConfirmationReasonV2>,
    pub policy_name: SurfacePolicyNameV2,
    pub policy_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchEnabledNextPageV2 {
    #[serde(flatten)]
    pub page: SurfaceMatchPageBaseV2,
    pub continuation_policy: SurfaceContinuationEnabledV2,
    pub next_cursor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchEnabledTerminalPageV2 {
    #[serde(flatten)]
    pub page: SurfaceMatchPageBaseV2,
    pub continuation_policy: SurfaceContinuationEnabledV2,
    #[schema(schema_with = literal_null_schema)]
    pub next_cursor: (),
    pub surface_confirmation_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub impact_confirmation_token: Option<Uuid>,
}

fn literal_null_schema() -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::Null)
        .build()
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchTemporarilyDisabledPageV2 {
    #[serde(flatten)]
    pub page: SurfaceMatchPageBaseV2,
    pub continuation_policy: SurfaceContinuationDisabledV2,
    #[schema(required = true, nullable = true)]
    pub next_cursor: Option<String>,
    pub policy_block_code: SurfacePolicyBlockCodeV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SurfaceMatchPageV2 {
    EnabledNext(SurfaceMatchEnabledNextPageV2),
    EnabledTerminal(SurfaceMatchEnabledTerminalPageV2),
    TemporarilyDisabled(SurfaceMatchTemporarilyDisabledPageV2),
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct SurfaceMatchSnapshotPathV2 {
    pub snapshot_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchSnapshotQueryV2 {
    pub cursor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DictionaryProviderV2 {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DictionaryCoverageStateV2 {
    Complete,
    Partial,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DictionaryCoverageV2 {
    pub forms: DictionaryCoverageStateV2,
    pub pronunciations: DictionaryCoverageStateV2,
    pub meanings: DictionaryCoverageStateV2,
    pub examples: DictionaryCoverageStateV2,
    pub frequency: DictionaryCoverageStateV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DictionaryProvenanceV2 {
    pub forms: Option<DictionaryProviderV2>,
    pub pronunciations: Option<DictionaryProviderV2>,
    pub meanings: Option<DictionaryProviderV2>,
    pub examples: Option<DictionaryProviderV2>,
    pub frequency: Option<DictionaryProviderV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // wire shape stays flat and backward-compatible
pub enum BuiltinDictionaryResultV2 {
    Matched {
        provider: DictionaryProviderV2,
        headwords: WordHeadwordsV2,
        suggested_forms: Box<DraftFormsStepContent>,
        suggested_meanings: Box<DraftMeaningsStepContent>,
        suggested_frequency: Option<String>,
        coverage: DictionaryCoverageV2,
        provenance: DictionaryProvenanceV2,
    },
    NotFound,
    Unavailable {
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_seconds: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetectWordResponseV2 {
    pub detection_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub request: DetectionRequestEcho,
    pub normalized_headword: String,
    pub entry_kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub matched_dialect: Option<Dialect>,
    pub builtin_dictionary: BuiltinDictionaryResultV2,
    pub smart_dictionary: SmartDictionaryResultV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAdminWordV2Input {
    pub schema_version: u8,
    pub detection_id: Uuid,
    pub headwords: WordHeadwordsV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

// --- dialect suggestion ---

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DialectSuggestionFieldKind {
    Form,
    Definition,
    Example,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum DialectVariantSuggestionItemV2 {
    Form {
        client_id: String,
        field_kind: DialectSuggestionFieldKind,
        value: String,
    },
    RichText {
        client_id: String,
        field_kind: DialectSuggestionFieldKind,
        value: RichText,
    },
}

impl DialectVariantSuggestionItemV2 {
    pub fn client_id(&self) -> &str {
        match self {
            Self::Form { client_id, .. } | Self::RichText { client_id, .. } => client_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestDialectVariantsInputV2 {
    pub source_dialect: SourceDialect,
    pub target_dialect: SourceDialect,
    pub items: Vec<DialectVariantSuggestionItemV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DialectSuggestionProviderV2 {
    pub kind: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SuggestDialectVariantsResponseV2 {
    pub provider: DialectSuggestionProviderV2,
    pub suggestions: Vec<DialectVariantSuggestionItemV2>,
}

// --- editor ---

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct EntryPath {
    pub id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct PublicationPath {
    pub id: Uuid,
    pub publication_id: Uuid,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepSaveIntent {
    Save,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveFormsStepInput {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    pub intent: StepSaveIntent,
    #[serde(default)]
    #[schema(nullable = false)]
    pub confirmed_impact_token: Option<Uuid>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
    pub content: DraftFormsStepContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveMeaningsStepInput {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    pub intent: StepSaveIntent,
    pub content: DraftMeaningsStepContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewFormsImpactInputV2 {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    pub content: DraftFormsStepContent,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FormsImpactNodeType {
    Pos,
    GrammarStructure,
    TextVariant,
    Sense,
    Definition,
    Sentence,
    Relation,
}

impl FormsImpactNodeType {
    pub(crate) fn from_internal(value: &str) -> Self {
        match value {
            "pos" => Self::Pos,
            "grammar_structure" => Self::GrammarStructure,
            "text_variant" => Self::TextVariant,
            "sense" => Self::Sense,
            "definition" => Self::Definition,
            "sentence" => Self::Sentence,
            "relation" => Self::Relation,
            _ => unreachable!("forms impact emitted unsupported node type: {value}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FormsImpactItemV2 {
    pub node_id: Uuid,
    pub node_type: FormsImpactNodeType,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FormsImpactResponseV2 {
    pub base_revision: i64,
    pub requires_confirmation: bool,
    pub affected: Vec<FormsImpactItemV2>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmation_token: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub surface_match_page: Option<SurfaceMatchPageV2>,
}

#[cfg(test)]
mod forms_impact_node_type_tests {
    use super::FormsImpactNodeType;

    #[test]
    fn internal_types_serialize_to_the_documented_wire_values() {
        for value in [
            "pos",
            "grammar_structure",
            "text_variant",
            "sense",
            "definition",
            "sentence",
            "relation",
        ] {
            assert_eq!(
                serde_json::to_value(FormsImpactNodeType::from_internal(value)).unwrap(),
                value
            );
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DraftValidationIssue {
    pub step: PersistedWordStep,
    pub node_id: Uuid,
    pub field: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_location: Option<DraftReferenceLocation>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DraftReferenceLocation {
    pub source_entry_id: Uuid,
    pub source_publication_id: Uuid,
    pub source_node_id: Uuid,
    pub reference_kind: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DraftValidationResponse {
    pub validated_revision: i64,
    pub valid: bool,
    pub issues: Vec<DraftValidationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateAdminWordV2Input {
    #[schema(minimum = 1)]
    pub base_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishAdminWordV2Input {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivatePublicationInput {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

// --- lifecycle ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteDraftInput {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryLifecycleInput {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryLifecycleTarget {
    pub id: Uuid,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryLifecycleBatchInput {
    pub entries: Vec<EntryLifecycleTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EntryLifecycleBatchResponse {
    pub words: Vec<AdminWordV2>,
    pub affected: usize,
}

// --- listing ---

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AdminWordListQuery {
    #[param(default = 1, minimum = 1)]
    pub page: Option<u32>,
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
    pub q: Option<String>,
    pub gloss: Option<String>,
    pub kind: Option<EntryKind>,
    pub pos: Option<String>,
    pub level: Option<String>,
    pub status: Option<AdminWordStatus>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RelatedSearchQuery {
    pub q: Option<String>,
    pub kind: Option<EntryKind>,
    pub match_mode: Option<RelatedSearchMatchMode>,
    pub exclude_exact: Option<bool>,
    #[param(minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelatedSearchMatchMode {
    Exact,
    Contains,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct RelatedWordSense {
    pub sense_id: Uuid,
    pub gloss: String,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct RelatedWordResult {
    pub word_id: Uuid,
    pub headword: String,
    pub kind: EntryKind,
    pub dialects: Vec<Dialect>,
    pub pos_labels: Vec<String>,
    pub senses: Vec<RelatedWordSense>,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelatedSearchLegacyResponse {
    pub results: Vec<RelatedWordResult>,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelatedSearchV2Response {
    pub results: Vec<RelatedWordResult>,
    pub total: u64,
    #[schema(required = true, nullable = true)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(untagged)]
pub enum RelatedSearchResponse {
    Legacy(RelatedSearchLegacyResponse),
    V2(RelatedSearchV2Response),
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminWordListItem {
    pub schema_version: u8,
    pub id: Uuid,
    pub headword: String,
    pub kind: EntryKind,
    pub dialects: Vec<Dialect>,
    pub revision: i64,
    pub lifecycle_revision: i64,
    pub gloss: String,
    pub pos_list: Vec<String>,
    pub levels: Vec<String>,
    pub status: AdminWordStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub published_revision: Option<i64>,
    pub has_unpublished_changes: bool,
    pub max_reachable_step: WordCreationStep,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminWordListPage {
    pub page: u32,
    pub page_size: u32,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminWordListResponse {
    pub words: Vec<AdminWordListItem>,
    pub page: AdminWordListPage,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminWordStats {
    pub total: i64,
    pub today: i64,
    pub month: i64,
}
