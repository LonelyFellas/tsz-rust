use super::*;

// --- entry ---

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

// --- forms ---

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
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

/// 例句正文里某个词的位置。
///
/// `start` / `end` 是所在 [`RichText`] `text` 的 **Unicode 码点**下标，左闭右开，
/// 与 `RichTextAnnotation` 的 span 口径完全一致；`surface` 是该区间的原文词面，
/// 既供前端直接展示，也让读取时能自检区间有没有跟着正文漂走。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceSourceRangeV1 {
    pub start: usize,
    pub end: usize,
    pub surface: String,
}

/// 关联是怎么来的。仅供展示与口径质量评估，不参与任何判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationOriginV2 {
    /// 发布时自动解析产出。
    Auto,
    /// 管理员事后改过或补过。
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationStateV1 {
    Linked,
    Pending,
}

/// `WordSentenceV2::associations` 是不是当前正文的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationsStateV2 {
    /// 当前正文还没被解析过：草稿从未发布，或正文改动后尚未重新发布。
    /// 此时 `associations` 恒为空数组，**不代表「这句话没有可关联的词」**。
    #[default]
    Unresolved,
    /// `associations` 就是当前正文的解析结果，空数组即句中确实没有可关联的词。
    Resolved,
}

/// 例句里某个词指向别的词条某个词义的关联。
///
/// 整个结构是**只读投影**：发布时由后端解析正文产出，事后由
/// `PUT /entries/{id}/sentences/{sentence_id}/associations` 修正。
/// 草稿保存路径收到这个字段会直接丢弃，客户端填不进来。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WordSentenceAssociationV2 {
    Linked {
        id: Uuid,
        /// 位置落在 `en_text` 的哪一侧。distinguish 例句的 uk/us 两份正文下标会错位，
        /// 所以位置必须绑到具体一侧；unified 例句只有 `common`。
        source_dialect: Dialect,
        source_range: SentenceSourceRangeV1,
        target_word_id: Uuid,
        target_sense_id: Uuid,
        /// 命中目标词条的哪个词形槽位——按读者方言把 `centre`/`center` 显示成哪一个，
        /// 靠的是它而不是原句词面。人工关联的词面在目标词条已发布词形里找不到时缺省。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        target_form_slot_id: Option<Uuid>,
        origin: SentenceAssociationOriginV2,
        /// 写入时从目标词条当前发布快照取的值，读取不做跨词条 JOIN。
        #[schema(read_only)]
        target_headword: String,
        #[schema(read_only)]
        target_gloss: String,
        #[schema(read_only)]
        resolved_pos: String,
        /// 与 `target_form_slot_id` 同生共死。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false, read_only)]
        resolved_form_type: Option<String>,
    },
    Pending {
        id: Uuid,
        source_dialect: Dialect,
        source_range: SentenceSourceRangeV1,
        origin: SentenceAssociationOriginV2,
        pending_target_kind: EntryKind,
        #[schema(max_length = 200)]
        pending_target_headword: String,
        #[schema(read_only, max_length = 200)]
        normalized_pending_target_headword: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false, max_length = 5000)]
        pending_target_gloss: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordSentenceV2 {
    pub id: Uuid,
    pub level: String,
    pub en_text: EnglishTextV2,
    pub zh_text_id: Uuid,
    pub zh_text: RichText,
    pub links: Vec<WordSentenceLinkV2>,
    /// 只读，按 `(source_dialect, source_range.start)` 升序。
    /// `associations_state` 为 `unresolved` 时恒为空数组。
    ///
    /// `serde(default)` 是给存量编辑器投影用的——那些 JSONB 里没有这两个字段；
    /// 服务端返回时一定带，所以契约上仍标成必填。
    #[serde(default)]
    #[schema(required = true)]
    pub associations: Vec<WordSentenceAssociationV2>,
    #[serde(default)]
    #[schema(required = true)]
    pub associations_state: SentenceAssociationsStateV2,
}

/// 关联词支持显式词义绑定与纯文本两种形态：
///
/// - `target_word_id` + `target_sense_id` 指向管理员选择的真实义项，
///   `target_headword` / `target_gloss` 是服务端回填的快照。
/// - `pending_target_headword` 是独立展示文本，保存和发布均不创建词条或自动绑定。
///   `pending_target_gloss` 仅保留旧数据兼容，不再用于创建或匹配目标。
///
/// 旧预绑定字段仅供内部历史数据兼容；写入不接受，迁移会移除旧预绑定关联。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WordRelationV2 {
    pub id: Uuid,
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_word_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_sense_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(ignore)]
    pub prebound_target_word_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(ignore)]
    pub prebinding_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pending_target_headword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 5000)]
    pub pending_target_gloss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only)]
    pub target_headword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only)]
    pub target_gloss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(ignore)]
    pub target_status: Option<String>,
    pub score: String,
}

impl WordRelationV2 {
    /// 已绑定形态的目标键；待物化时返回 `None`。
    pub fn bound_target(&self) -> Option<(Uuid, Uuid)> {
        self.target_word_id.zip(self.target_sense_id)
    }
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
