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
pub struct DuplicateWordMatchV2 {
    pub word_id: Uuid,
    pub headword: String,
    pub dialect: Dialect,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SmartDictionaryResultV2 {
    Clear {
        duplicates: Vec<DuplicateWordMatchV2>,
    },
    Duplicate {
        duplicates: Vec<DuplicateWordMatchV2>,
    },
    Unavailable {
        duplicates: Vec<DuplicateWordMatchV2>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BuiltinDictionaryResultV2 {
    Matched {
        headwords: WordHeadwordsV2,
        suggested_forms: DraftFormsStepContent,
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

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FormsImpactItemV2 {
    pub node_id: Uuid,
    pub node_type: String,
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
}

// --- lifecycle ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryLifecycleInput {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
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
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
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
    pub senses: Vec<RelatedWordSense>,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct RelatedSearchResponse {
    pub results: Vec<RelatedWordResult>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminWordListItem {
    pub schema_version: u8,
    pub id: Uuid,
    pub headword: String,
    pub kind: EntryKind,
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
