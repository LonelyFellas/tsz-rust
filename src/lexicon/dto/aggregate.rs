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
    pub existing_status: AdminWordStatus,
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
