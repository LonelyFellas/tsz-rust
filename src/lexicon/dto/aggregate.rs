use super::*;

// --- entry ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordDetectionSnapshotV2 {
    pub detection_id: Uuid,
    pub request: DetectionRequestEcho,
    pub normalized_headword: String,
    pub entry_kind: EntryKind,
    pub matched_dialect: Dialect,
    pub builtin_dictionary_status: String,
    pub smart_dictionary_status: String,
    pub headwords: WordHeadwordsV2,
    pub suggested_pos: Vec<String>,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersistedWordStep {
    Basics,
    Forms,
    Meanings,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WordCreationStep {
    Basics,
    Forms,
    Meanings,
    Preview,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdminWordStatus {
    Draft,
    Published,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminWordV2 {
    pub schema_version: u8,
    pub id: Uuid,
    pub language: String,
    pub kind: EntryKind,
    pub status: AdminWordStatus,
    pub revision: i64,
    #[serde(default = "default_lifecycle_revision")]
    #[schema(required = true)]
    pub lifecycle_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub published_revision: Option<i64>,
    #[serde(default)]
    #[schema(required = true)]
    pub has_unpublished_changes: bool,
    pub headwords: WordHeadwordsV2,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency: Option<String>,
    pub detection_snapshot: WordDetectionSnapshotV2,
    pub forms: DraftFormsStepContent,
    pub meanings: DraftMeaningsStepContent,
    pub completed_steps: Vec<PersistedWordStep>,
    pub max_reachable_step: WordCreationStep,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub archived_by: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
}

const fn default_lifecycle_revision() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminWordV2Envelope {
    pub word: AdminWordV2,
}

// --- forms ---

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PronunciationStyle {
    Normal,
    Strong,
    Weak,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordPronunciationV2 {
    pub id: Uuid,
    pub dict_phonetic: String,
    pub actual_pron: String,
    pub style: PronunciationStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordFormVariantV2 {
    pub id: Uuid,
    pub dialect: Dialect,
    pub spelling: String,
    pub origin: TextOrigin,
    pub pronunciations: Vec<WordPronunciationV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordBaseFormSlotV2 {
    pub id: Uuid,
    pub form_type: String,
    pub variants: Vec<WordFormVariantV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordDerivedFormSlotV2 {
    pub id: Uuid,
    pub form_type: String,
    pub variants: Vec<WordFormVariantV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordFormGroupV2 {
    pub id: Uuid,
    pub is_regular: bool,
    pub slots: Vec<WordDerivedFormSlotV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DialectRulesV2 {
    pub spelling_mode: String,
    pub phonetic_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordPosFormsV2 {
    pub pos_id: Uuid,
    pub pos: String,
    pub dialect_rules: DialectRulesV2,
    pub base_form: WordBaseFormSlotV2,
    pub form_groups: Vec<WordFormGroupV2>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct DraftFormsStepContent {
    pub pos: Vec<WordPosFormsV2>,
}

// --- meanings ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TextVariantV2<T> {
    pub id: Uuid,
    pub value: T,
    pub origin: TextOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DialectVariantSlotV2<T> {
    Missing,
    Ready { variant: TextVariantV2<T> },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EnglishTextV2 {
    Unified {
        common: TextVariantV2<RichText>,
    },
    Distinguish {
        source_dialect: SourceDialect,
        uk: DialectVariantSlotV2<RichText>,
        us: DialectVariantSlotV2<RichText>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrammarVariantV2 {
    pub id: Uuid,
    pub dialect: Dialect,
    pub content: RichText,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrammarStructureV2 {
    pub id: Uuid,
    pub variants: Vec<GrammarVariantV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "definition_mode", rename_all = "snake_case")]
pub enum WordDefinitionV2 {
    ZhDefinition {
        id: Uuid,
        content_id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        grammar_structure_id: Option<Uuid>,
        content: RichText,
    },
    ZhSentence {
        id: Uuid,
        content_id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        grammar_structure_id: Option<Uuid>,
        content: RichText,
    },
    EnDefinition {
        id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        grammar_structure_id: Option<Uuid>,
        content: EnglishTextV2,
    },
    EnSentence {
        id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        grammar_structure_id: Option<Uuid>,
        content: EnglishTextV2,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordSentenceLinkV2 {
    pub word_id: Uuid,
    pub sense_id: Uuid,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordSentenceV2 {
    pub id: Uuid,
    pub level: String,
    pub en_text: EnglishTextV2,
    pub zh_text_id: Uuid,
    pub zh_text: RichText,
    pub links: Vec<WordSentenceLinkV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordRelationV2 {
    pub id: Uuid,
    pub relation: String,
    pub target_word_id: Uuid,
    pub target_sense_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only)]
    pub target_headword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only)]
    pub target_gloss: Option<String>,
    pub score: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordSenseV2 {
    pub id: Uuid,
    pub sub_pos: String,
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sense_group_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency: Option<String>,
    pub depends_on_context: bool,
    pub definitions: Vec<WordDefinitionV2>,
    pub sentences: Vec<WordSentenceV2>,
    pub relations: Vec<WordRelationV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SenseGroupV2 {
    pub id: Uuid,
    pub name_zh: String,
    pub name_en: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordPosMeaningsV2 {
    pub pos_id: Uuid,
    pub grammar_structures: Vec<GrammarStructureV2>,
    pub senses: Vec<WordSenseV2>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct DraftMeaningsStepContent {
    pub sense_groups: Vec<SenseGroupV2>,
    pub pos: Vec<WordPosMeaningsV2>,
}
