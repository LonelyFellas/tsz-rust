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
    #[serde(default = "default_duplicate_match_category")]
    #[schema(required = true)]
    pub match_category: SurfaceMatchCategoryV2,
    /// 与 warning 分支 `matched_entry_contexts[].inbound_relations` 同构，
    /// `previews` 同样最多 5 条。duplicate 分支没有 `surface_match_page` 可挂上下文，
    /// 缺了它前端就提示不出「这条空壳词条是 XX 的同义词」。
    // 与 match_category 同理，兼容缺字段的历史 Redis 快照：退化成「没有词条引用它」，
    // 前端按约定整项略去。这条是部署细节，不进对外契约的 description。
    #[serde(default)]
    #[schema(required = true)]
    pub inbound_relations: RelationReferenceSummaryV2,
}

const fn default_duplicate_match_category() -> SurfaceMatchCategoryV2 {
    // 兼容部署前最长存活 65 分钟的 Redis detection 快照。duplicate 分支只在 legacy
    // exact-headword 索引命中、而投影表尚未追平时触发，历史记录唯一可能的类别就是它。
    SurfaceMatchCategoryV2::ExactHeadword
}

#[cfg(test)]
mod duplicate_word_match_tests {
    use super::*;

    #[test]
    fn legacy_redis_duplicate_without_inbound_relations_degrades_to_no_reference() {
        let duplicate: DuplicateWordMatchV2 = serde_json::from_value(serde_json::json!({
            "word_id": Uuid::now_v7(),
            "headword": "legacy",
            "dialect": "common",
            "status": "draft",
            "match_category": "exact_headword"
        }))
        .unwrap();
        assert_eq!(duplicate.inbound_relations.total, 0);
        assert!(duplicate.inbound_relations.previews.is_empty());
        assert!(!duplicate.inbound_relations.truncated);
    }
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
    /// 命中词条的词面被**另一个**词条引用为关联词。
    ///
    /// `lexicon.relations` 的目标是已存在词条的义项（外键约束），所以关联词永远
    /// 不会引入新的词面：`surface` 必然等于 `ExistingSurfaceMatchV2.word_id` 这个
    /// 词条自己的主词。因此本分支描述的不是「词面存放在哪」，而是「谁在引用它」，
    /// `source_id` / `source_node_id` 指向的关联词节点属于 `referencing_word_id`。
    Relation {
        source_id: String,
        source_node_id: Uuid,
        content_scope: SurfaceContentScopeV2,
        surface: String,
        dialect: Dialect,
        relation_type: RelationTypeV2,
        referencing_word_id: Uuid,
        referencing_headword: String,
        referencing_status: AdminWordStatus,
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
    /// 本次录入的主词命中了某个词条的主词，且该词条已被别的词条引用为关联词。
    ///
    /// 它永远与同一个 `word_id` 上的 `exact_headword` / `cross_kind_headword`
    /// 并存（关联词目标必须是已存在词条），是对既有命中的补充说明，不是独立的
    /// 词面来源，因此不参与 surface policy 选择，`attention_level` 恒为 `normal`。
    HeadwordRelation,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
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
    #[serde(default = "default_relation_source_status")]
    #[schema(required = true)]
    pub source_status: AdminWordStatus,
    pub relation: RelationTypeV2,
}

const fn default_relation_source_status() -> AdminWordStatus {
    // 兼容部署前最长存活十分钟的 Redis snapshot：旧查询只收 current publication 且排除归档，
    // 因此缺字段的历史 preview 唯一可能的状态就是 published。
    AdminWordStatus::Published
}

#[cfg(test)]
mod relation_reference_preview_tests {
    use super::*;

    #[test]
    fn legacy_redis_preview_without_source_status_defaults_to_published() {
        let preview: RelationReferencePreviewV2 = serde_json::from_value(serde_json::json!({
            "source_word_id": Uuid::now_v7(),
            "source_headword": "legacy",
            "relation": "synonym"
        }))
        .unwrap();
        assert_eq!(preview.source_status, AdminWordStatus::Published);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
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
    #[serde(deserialize_with = "deserialize_schema_version_2")]
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
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
    #[serde(deserialize_with = "deserialize_schema_version_2")]
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
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
    #[serde(deserialize_with = "deserialize_schema_version_2")]
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
    pub detection_id: Uuid,
    /// matched 检测返回的英美主词仅作为建议；管理员可切换模式、编辑非空拼写并决定
    /// `source_dialect`。检测过期、消费、幂等及最终主词 surface 确认仍由服务端校验。
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResolveSentenceTargetsV3Input {
    AllPublishedTargets {
        #[schema(schema_with = schema_version_3_schema)]
        #[serde(deserialize_with = "deserialize_schema_version_3")]
        schema_version: u8,
        #[schema(max_length = 1000)]
        sentence_text: String,
        source_dialect: Dialect,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false, minimum = 1, maximum = 100)]
        page_size_per_range: Option<u32>,
    },
    SelectedSegments {
        #[schema(schema_with = schema_version_3_schema)]
        #[serde(deserialize_with = "deserialize_schema_version_3")]
        schema_version: u8,
        #[schema(max_length = 1000)]
        sentence_text: String,
        source_dialect: Dialect,
        #[schema(min_items = 1, max_items = 20)]
        selected_segments: Vec<SentenceSourceRangeV1>,
        include_drafts: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false, minimum = 1, maximum = 100)]
        page_size_per_range: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        /// 由同一位置上一页返回；绑定 discovery generation、source dialect 与片段指纹。
        cursor: Option<String>,
    },
}

impl ResolveSentenceTargetsV3Input {
    pub(crate) fn sentence_text(&self) -> &str {
        match self {
            Self::AllPublishedTargets { sentence_text, .. }
            | Self::SelectedSegments { sentence_text, .. } => sentence_text,
        }
    }

    pub(crate) const fn source_dialect(&self) -> Dialect {
        match self {
            Self::AllPublishedTargets { source_dialect, .. }
            | Self::SelectedSegments { source_dialect, .. } => *source_dialect,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceTargetDiscoveryCompletenessV3 {
    Complete,
    Overloaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceTargetMatchKindV3 {
    Word,
    ContiguousPhrase,
    SeparablePhrase,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceTargetMatchEvidenceV3 {
    pub surface: String,
    pub normalized_surface: String,
    pub match_kind: SentenceTargetMatchKindV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceTargetSenseV3 {
    pub sense_id: Uuid,
    pub publication_id: Uuid,
    pub pos_id: Uuid,
    /// 与候选行的 base_form_id 相同：词义挂在词性下而不挂在变化组下，这里只是原样带出，
    /// 改选词形时同样以词形自带的 base_form_ids 为准。
    pub base_form_id: Uuid,
    pub level: String,
    pub gloss: String,
    /// 该词义（发布快照里）自带的短语成分用词；空则省略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 100)]
    pub component_usages: Vec<PhraseComponentUsageV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceTargetCandidateFormV3 {
    pub form_id: Uuid,
    pub variant_id: Uuid,
    pub form_type: WordFormTypeV3,
    pub spelling: String,
    pub dialect: Dialect,
    /// 该词形可搭配的原形 id：短语成分保存要求 form 与 base 同组（或 form 自身就是那个 base），
    /// 这里给出满足该条件的全部
    /// base，按 id 排序去重，顺序不表示优先级。改选词形时，候选行自己的 base_form_id 若在
    /// 此列表内就沿用，否则任取一个，后端不区分同组内的多个原形。
    /// 为空即不可选：目标来自 V2 发布（成分只接受 V3 发布的目标），或词形没挂进任何带原形
    /// 的变化组。非空只表示同组这一条满足，成分保存另有不得自指、目标短语不得再套短语等
    /// 限制，仍可能被拒。
    // 上限同样取共享节点上限 2000：原形是词形的子集，不可能比词形还多。
    #[schema(max_items = 2000)]
    pub base_form_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishedSentenceTargetCandidateV3 {
    pub entry_id: Uuid,
    pub publication_id: Uuid,
    pub pos_id: Uuid,
    /// 命中词形（matched_form_id）所属的原形，标明这条候选的身份：命中词形挂在几个原形下就
    /// 出几条候选，跨组去重。它不表示可用作短语成分——能不能选、配哪个 base，一律以
    /// forms[].base_form_ids 为准（V2 发布的目标该列表恒为空，这里却仍有值）。
    pub base_form_id: Uuid,
    pub kind: EntryKind,
    pub headword: String,
    pub pos: String,
    pub matched_form_id: Uuid,
    pub matched_variant_id: Uuid,
    pub matched_dialect: Dialect,
    pub matched_form_type: WordFormTypeV3,
    /// 该词性下全部词形变体的清单，供改选屈折形之外的词形。
    // 上限取词条的共享节点上限 2000：每个词形与每个地区变体各占一个节点，
    // V2（validate_node_limit）与 V3（forms_node_count）保存时都按这个预算卡，
    // 所以一个词性下的 (词形, 变体) 组合数必然不超过它。
    #[schema(max_items = 2000)]
    pub forms: Vec<SentenceTargetCandidateFormV3>,
    /// 命中词形（`matched_variant_id`）自带的成分用词。B2 起恒为 `[]`，
    /// 所以 spec 里已经标成可选，避免停输出时旧前端 `missing_required_property`。
    #[serde(default)]
    #[schema(max_items = 100)]
    pub component_usages: Vec<PhraseComponentUsageV3>,
    pub matches: Vec<SentenceTargetMatchEvidenceV3>,
    pub senses: Vec<SentenceTargetSenseV3>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceTargetDraftStateV3 {
    Draft,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceTargetDraftLinkabilityV3 {
    PendingOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftSentenceTargetCandidateV3 {
    pub entry_id: Uuid,
    pub entry_revision: i64,
    pub headword: String,
    pub target_state: SentenceTargetDraftStateV3,
    pub linkability: SentenceTargetDraftLinkabilityV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceTargetRangeResultV3 {
    #[schema(min_items = 1, max_items = 20)]
    pub source_segments: Vec<SentenceSourceRangeV1>,
    pub segments_fingerprint: String,
    pub normalized_surface: String,
    pub published_total: u64,
    pub published_matches: Vec<PublishedSentenceTargetCandidateV3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    /// 自动发现或手选详情被截断时返回；下一页改用相同 `selected_segments` 携带此值。
    pub next_cursor: Option<String>,
    pub draft_matches: Vec<DraftSentenceTargetCandidateV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveSentenceTargetsV3Response {
    #[schema(schema_with = schema_version_3_schema)]
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    pub schema_version: u8,
    pub sentence_hash: String,
    pub discovery_generation: i64,
    pub completeness: SentenceTargetDiscoveryCompletenessV3,
    pub range_results: Vec<SentenceTargetRangeResultV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchComponentTargetsV3Input {
    #[schema(schema_with = schema_version_3_schema)]
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    pub schema_version: u8,
    /// 关键字：对已发布词面做大小写不敏感的包含匹配，1..=100 码点且两端不留空白。
    #[schema(min_length = 1, max_length = 100)]
    pub q: String,
    /// 只要单词或只要短语；不传则两者都返回。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub kind: Option<EntryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, minimum = 1, maximum = 200)]
    pub page_size: Option<u32>,
    /// 上一页返回的 `next_cursor`。绑定 discovery generation 与本次 `q` / `kind`：凡是写
    /// `surface_sources` 的变动（发布、词形步保存等）或换了关键字/kind 后即失效（400
    /// `invalid_query`，`field = "cursor"`）。归档只写 `entries.archived_at`、不推进 generation，
    /// 翻页期间被归档的词条只是从后续页消失，不会让游标失效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchComponentTargetsV3Response {
    #[schema(schema_with = schema_version_3_schema)]
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    pub schema_version: u8,
    /// 与 resolve 的 `published_matches` 完全同构。关键字检索没有句子区间，所以每条候选的
    /// `matches` 恒为空数组——没有「命中了句子里的哪一段」可言，后端不构造假证据。
    /// 顺序：词面等于 `q` 的词条最前，以 `q` 开头的其次，其余按 headword；同一词条的候选相邻。
    pub matches: Vec<PublishedSentenceTargetCandidateV3>,
    /// 本次扫描窗口内命中的候选总数（跨所有页）；撞了窗口上限时它是下界，不是全库命中数。
    pub total: u64,
    /// 还有未返回的候选：有下一页（同时给出 `next_cursor`），或触到了后端的扫描行/词条数上限
    /// （此时没有 `next_cursor`，只能换更具体的关键字）。
    pub truncated: bool,
    /// 还有下一页时返回；下一页用相同的 `q` / `kind` / `page_size` 携带此值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub next_cursor: Option<String>,
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
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
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
    /// 仅节点身份类问题（`stable_node_id_changed` / `node_binding_changed` /
    /// `node_binding_unknown`）带这个子对象，其余 issue 整体省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_location: Option<DraftNodeLocation>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftReferenceLocation {
    pub source_entry_id: Uuid,
    pub source_publication_id: Uuid,
    pub source_node_id: Uuid,
    pub reference_kind: String,
}

/// 把节点身份类问题还原到界面位置用的定位信息。
///
/// 所有字段都取自**本次提交的内容**，不含任何服务端存量节点 ID——旧 ID / 新 ID
/// 的对照只写服务端日志。`message` 面向实现，展示文案由前端按这里的字段自行拼装。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftNodeLocation {
    /// 出问题节点的角色，方言编在冒号之后，例如 `forms.form_variant:common`。
    pub node_role: String,
    /// 所属基本词性编码（`verb` / `noun` …）；不挂在基本词性下的节点省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pos: Option<String>,
    /// 所属基本词性的节点 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pos_id: Option<Uuid>,
    /// 所在词形组在 `pos.form_groups` 中的序号（从 0 开始）；共享原形不属于任何组，省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub form_group_index: Option<u32>,
    /// V3 词形组稳定 ID；V2 issue 省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub form_group_id: Option<Uuid>,
    /// V3 group membership 稳定 ID；V2 issue 省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub membership_id: Option<Uuid>,
    /// V3 concrete form 稳定 ID；V2 issue 省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub form_id: Option<Uuid>,
    /// V3 regional variant 稳定 ID；V2 issue 省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub variant_id: Option<Uuid>,
    /// V3 pronunciation 稳定 ID；V2 issue 省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pronunciation_id: Option<Uuid>,
    /// 所在词形槽位的类型；`base` 表示共享原形。词形之外的节点省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub form_type: Option<WordFormTypeV2>,
    /// 方言侧；节点角色不带方言时省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub dialect: Option<Dialect>,
    /// 从词条根到直接父节点的祖先链，全部是本次提交里的节点 ID。
    pub ancestor_node_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DraftValidationResponse {
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
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

/// 一条词条被谁引用的预览项。`source_kind` 说明引用来自哪一类内容，
/// 让管理员看到「被短语 X 当成分」与「被词条 Y 设为近义词」的区别。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EntryReferencePreview {
    pub source_word_id: Uuid,
    pub source_headword: String,
    pub source_status: AdminWordStatus,
    pub source_kind: EntryReferenceKind,
}

/// 引用来源分类。计数本身不按类型拆分（管理员的下一个问题是「谁」而非「哪类」），
/// 但预览项里标明来源类型几乎零成本且信息量高。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryReferenceKind {
    /// 关联词（近义/反义/派生）已绑定到本词条的词义。
    Relation,
    /// 关联词预绑定：填写时目标还没建条，本词条建出来后被绑上。
    RelationPrebound,
    /// 别的词条的例句把本词条标为 focus / context。
    SentenceLink,
    /// 已发布内容引用本词条的词义。
    PublicationSenseRef,
    /// 例句关联待认领。
    SentenceAssociation,
    /// V3 短语把本词条当作成分。
    PhraseComponent,
}

/// 词条被引用汇总。`total` 按**引用方词条去重**计数（同一词条多处引用只算 1），
/// 口径与删除时的入站引用拦截完全一致——否则会出现「显示 0 却删不掉」。
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct EntryReferenceSummary {
    pub total: u32,
    #[schema(max_items = 5)]
    pub previews: Vec<EntryReferencePreview>,
    /// total 超过 previews 长度时为 true。
    pub truncated: bool,
}

/// 批量永久删除入参；不带 confirmed_surface_match_token——
/// 删除只撤除 surface 贡献、不新增占位，不存在需要确认的同表面冲突。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryDeleteBatchInput {
    pub entries: Vec<EntryLifecycleTarget>,
}

/// 批量永久删除出参；词条已不存在，故不回实体，只回受影响条数。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EntryDeleteBatchResponse {
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
    pub include_drafts: Option<bool>,
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

/// 一侧词头的结构化形式。`headword` 是把这些拼写按同一顺序拼成的展示串，
/// 想按管理员方言偏好重排就读这个数组——**不要切分 `" / "`**，短语词条的拼写里可能有斜杠。
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct HeadwordVariant {
    pub dialect: Dialect,
    pub headword: String,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct RelatedWordResult {
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
    pub word_id: Uuid,
    pub headword: String,
    pub kind: EntryKind,
    pub dialects: Vec<Dialect>,
    /// 每侧拼写，与 `dialects` 同序；`headword` 即本数组按序拼接。
    pub headword_variants: Vec<HeadwordVariant>,
    pub pos_labels: Vec<String>,
    pub senses: Vec<RelatedWordSense>,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelatedSearchLegacyResponse {
    pub results: Vec<RelatedWordResultAny>,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelatedSearchV2Response {
    pub results: Vec<RelatedWordResultAny>,
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
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
    pub id: Uuid,
    /// 并列拼写按管理员主词侧在前拼接，与 `dialects` 同序。
    pub headword: String,
    pub kind: EntryKind,
    pub dialects: Vec<Dialect>,
    /// 每侧拼写，与 `dialects` 同序；`headword` 即本数组按序拼接。
    pub headword_variants: Vec<HeadwordVariant>,
    /// 管理员主词侧；`mode = unified` 的词条没有主词侧，字段整体省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub source_dialect: Option<SourceDialect>,
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
    /// 创建人 admin id；前端按「仅本人可删」判定归属时使用。
    pub created_by: Uuid,
    /// 被引用汇总；`total = 0` 即无人引用，可安全清理。
    pub reference_summary: EntryReferenceSummary,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordListPage {
    pub page: u32,
    pub page_size: u32,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordListResponse {
    pub words: Vec<AdminWordListItemAny>,
    pub page: AdminWordListPage,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminWordStats {
    pub total: i64,
    pub today: i64,
    pub month: i64,
}
