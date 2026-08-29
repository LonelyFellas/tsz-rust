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
    #[serde(flatten)]
    pub smart_dictionary: WordDetectionSnapshotSmartDictionaryV2,
    pub headwords: WordHeadwordsV2,
    pub suggested_pos: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_provider: Option<DictionaryProviderV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_coverage: Option<DictionaryCoverageV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_provenance: Option<DictionaryProvenanceV2>,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(
    tag = "smart_dictionary_status",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WordDetectionSnapshotSmartDictionaryV2 {
    Clear {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(schema_with = null_surface_warning_schema)]
        surface_warning: Option<()>,
    },
    Warning {
        surface_warning: DetectionSurfaceWarningAuditV2,
    },
}

fn null_surface_warning_schema() -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::Null)
        .build()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcknowledgedTrue;

impl Serialize for AcknowledgedTrue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for AcknowledgedTrue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("acknowledged must be true"))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectionSurfaceMatchPreviewV2 {
    pub match_id: String,
    pub match_category: SurfaceMatchCategoryV2,
    pub existing_word_id: Uuid,
    pub existing_headword: String,
    pub existing_kind: EntryKind,
    pub existing_status: AdminWordStatus,
    pub existing_dialect: Dialect,
    #[schema(max_items = 5)]
    pub pos_labels: Vec<String>,
    #[schema(max_items = 5)]
    pub gloss_previews: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectionSurfaceWarningAuditV2 {
    pub total: u64,
    pub match_digest: String,
    #[schema(schema_with = acknowledged_true_schema)]
    pub acknowledged: AcknowledgedTrue,
    pub acknowledged_at: DateTime<Utc>,
    pub acknowledged_by: Uuid,
    pub policy_name: SurfacePolicyNameV2,
    pub policy_epoch: u64,
    #[schema(max_items = 5)]
    pub preview: Vec<DetectionSurfaceMatchPreviewV2>,
    pub truncated: bool,
}

fn acknowledged_true_schema() -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::Boolean)
        .enum_values(Some([true]))
        .build()
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
    #[serde(deserialize_with = "deserialize_schema_version_2")]
    #[schema(schema_with = schema_version_2_schema)]
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

/// 一个已退役但仍被永久占用的稳定槽位身份。
///
/// 稳定槽位的键是 `(entry_id, parent_node_id, node_role)`，方言编在 `node_role`
/// 里（`forms.form_variant:common`）。这个键一旦保存过就永久绑定同一个节点 ID：
/// 节点从草稿里消失只是被标记退役，重新出现时必须沿用原 ID，否则报
/// `stable_node_id_changed`。草稿本身只含当前在用的节点，所以刷新或换设备之后
/// 前端无从得知退役身份——本数组就是找回它们的唯一渠道。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetiredStableSlotV2 {
    /// 该槽位永久绑定的节点 ID，重新出现时原样提交。
    pub id: Uuid,
    /// 槽位所挂的父节点 ID。稳定槽位必有父节点。
    pub parent_node_id: Uuid,
    /// 槽位角色，方言在冒号之后，例如 `forms.form_variant:common`。
    pub node_role: String,
}

/// `GET /entries/{id}` 的响应：草稿本体 + 重建编辑态所需的节点身份信息。
///
/// 命令类接口仍返回 [`AdminWordV2Envelope`]（只有 `word`）；退役身份是编辑器
/// 恢复用的元数据，不属于词条内容，也不会进入不可变的 publication 快照。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminWordDraftV2Envelope {
    pub word: AdminWordV2,
    /// 该词条下所有已退役的稳定槽位身份，按 `(parent_node_id, node_role)` 排序。
    pub retired_stable_slots: Vec<RetiredStableSlotV2>,
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
pub struct WordSentenceAssociationV2 {
    pub id: Uuid,
    /// 位置落在 `en_text` 的哪一侧。distinguish 例句的 uk/us 两份正文下标会错位，
    /// 所以位置必须绑到具体一侧；unified 例句只有 `common`。
    pub source_dialect: Dialect,
    pub source_range: SentenceSourceRangeV1,
    pub target_word_id: Uuid,
    pub target_sense_id: Uuid,
    /// 命中目标词条的哪个词形槽位——按读者方言把 `centre`/`center` 显示成哪一个，
    /// 靠的是它而不是原句词面。人工关联的词面在目标词条已发布词形里找不到时缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_form_slot_id: Option<Uuid>,
    pub origin: SentenceAssociationOriginV2,
    /// 写入时从目标词条当前发布快照取的值，读取不做跨词条 JOIN。
    #[schema(read_only)]
    pub target_headword: String,
    #[schema(read_only)]
    pub target_gloss: String,
    #[schema(read_only)]
    pub resolved_pos: String,
    /// 与 `target_form_slot_id` 同生共死。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, read_only)]
    pub resolved_form_type: Option<String>,
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

/// 关联词有两种形态，由 `lexicon_relations_target_shape_check` 在库层保证互斥：
///
/// - **已绑定**：`target_word_id` + `target_sense_id` 指向真实义项，
///   `target_headword` / `target_gloss` 是服务端回填的快照。
/// - **待物化**：目标词还没有词条，`pending_target_headword` 承载管理员录入的词面，
///   `pending_target_gloss` 可选地承载创建目标草稿时预填的中文词义。这种形态只允许存在
///   于草稿；发布时会先建出词条再回填 target，所以发布出去的关联词永远是已绑定的。
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn warning_acknowledgement(acknowledged: bool) -> serde_json::Value {
        json!({
            "smart_dictionary_status": "warning",
            "surface_warning": {
                "total": 1,
                "match_digest": "digest",
                "acknowledged": acknowledged,
                "acknowledged_at": "2026-08-15T00:00:00Z",
                "acknowledged_by": Uuid::nil(),
                "policy_name": "surface_warning_acknowledgement",
                "policy_epoch": 1,
                "preview": [],
                "truncated": true
            }
        })
    }

    fn snapshot_with(smart_dictionary: serde_json::Value) -> serde_json::Value {
        let mut snapshot = json!({
            "detection_id": Uuid::now_v7(),
            "request": {"language": "en", "headword": "workspace"},
            "normalized_headword": "workspace",
            "entry_kind": "word",
            "matched_dialect": "common",
            "builtin_dictionary_status": "matched",
            "headwords": {"mode": "unified", "common": "workspace"},
            "suggested_pos": [],
            "detected_at": "2026-08-15T00:00:00Z"
        });
        snapshot
            .as_object_mut()
            .unwrap()
            .extend(smart_dictionary.as_object().unwrap().clone());
        snapshot
    }

    #[test]
    fn smart_dictionary_snapshot_is_a_strict_clear_or_acknowledged_warning_union() {
        let clear = serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(json!({
            "smart_dictionary_status": "clear"
        }));
        assert!(matches!(
            clear,
            Ok(WordDetectionSnapshotSmartDictionaryV2::Clear {
                surface_warning: None
            })
        ));

        let explicit_null =
            serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(json!({
                "smart_dictionary_status": "clear",
                "surface_warning": null
            }));
        assert!(matches!(
            explicit_null,
            Ok(WordDetectionSnapshotSmartDictionaryV2::Clear {
                surface_warning: None
            })
        ));

        assert!(
            serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(json!({
                "smart_dictionary_status": "clear",
                "surface_warning": warning_acknowledgement(true)["surface_warning"].clone()
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(json!({
                "smart_dictionary_status": "warning"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(
                warning_acknowledgement(false)
            )
            .is_err()
        );
        let mut warning_with_extra = warning_acknowledgement(true);
        warning_with_extra["unexpected"] = json!(true);
        assert!(
            serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(warning_with_extra)
                .is_err()
        );
        let mut audit_with_extra = warning_acknowledgement(true);
        audit_with_extra["surface_warning"]["unexpected"] = json!(true);
        assert!(
            serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(audit_with_extra)
                .is_err()
        );

        let warning = serde_json::from_value::<WordDetectionSnapshotSmartDictionaryV2>(
            warning_acknowledgement(true),
        )
        .unwrap();
        let serialized = serde_json::to_value(warning).unwrap();
        assert_eq!(serialized["surface_warning"]["acknowledged"], true);

        assert!(
            serde_json::from_value::<WordDetectionSnapshotV2>(snapshot_with(json!({
                "smart_dictionary_status": "clear",
                "surface_warning": null
            })))
            .is_ok()
        );
        let mut snapshot_with_extra = snapshot_with(json!({
            "smart_dictionary_status": "clear"
        }));
        snapshot_with_extra["unexpected"] = json!(true);
        assert!(serde_json::from_value::<WordDetectionSnapshotV2>(snapshot_with_extra).is_err());
    }
}
