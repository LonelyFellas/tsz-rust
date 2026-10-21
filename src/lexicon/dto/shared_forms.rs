use super::*;

fn optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum SharedFormsFormatV3 {
    #[serde(rename = "shared_v1")]
    SharedV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormContentVariantV3 {
    /// Persistent content slot, distinct from each binding's reference node.
    pub id: Uuid,
    #[schema(max_length = 200)]
    pub spelling: String,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(nullable = false)]
    pub spelling_rich: Option<RichTextV2V3>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(nullable = false)]
    pub spelling_speech: Option<Box<SpellingSpeechV3>>,
    pub origin: TextOrigin,
    /// Pronunciation IDs identify persistent content slots here, not reference nodes.
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<WordPronunciationV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum FormContentRegionalVariantsV3 {
    Common {
        common: FormContentVariantV3,
    },
    UkUs {
        uk: FormContentVariantV3,
        us: FormContentVariantV3,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormPronunciationSlotV3 {
    pub id: Uuid,
    pub variant_slot_id: Uuid,
}

/// Identities joined by an explicit region split or merge; contains no editable text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormPronunciationSlotsV3 {
    pub id: Uuid,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(nullable = false)]
    pub common: Option<FormPronunciationSlotV3>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(nullable = false)]
    pub uk: Option<FormPronunciationSlotV3>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(nullable = false)]
    pub us: Option<FormPronunciationSlotV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormContentV3 {
    pub id: Uuid,
    pub dialect_rules: DialectRulesV3,
    pub regional_variants: FormContentRegionalVariantsV3,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 2000)]
    pub pronunciation_slots: Vec<FormPronunciationSlotsV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormPronunciationBindingV3 {
    pub id: Uuid,
    pub content_slot_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormVariantBindingV3 {
    pub id: Uuid,
    pub content_slot_id: Uuid,
    pub is_regular: bool,
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<FormPronunciationBindingV3>,
    #[serde(default)]
    #[schema(value_type = Vec<PhraseComponentUsageV3>, max_items = 100)]
    pub component_usages: PresenceAwareVec<PhraseComponentUsageV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum FormRegionalBindingsV3 {
    Common {
        common: FormVariantBindingV3,
    },
    UkUs {
        uk: FormVariantBindingV3,
        us: FormVariantBindingV3,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormContentBindingV3 {
    pub id: Uuid,
    #[schema(value_type = String, min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub form_type: WordFormTypeV3,
    pub content_id: Uuid,
    pub regional_bindings: FormRegionalBindingsV3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedFormGroupV3 {
    pub id: Uuid,
    pub is_regular: bool,
    pub scope: FormGroupScopeV3,
    #[schema(max_items = 2000)]
    pub members: Vec<WordFormGroupMemberV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedPosFormsV3 {
    pub pos_id: Uuid,
    pub pos: String,
    #[schema(max_items = 2000)]
    pub forms: Vec<FormContentBindingV3>,
    #[schema(max_items = 2000)]
    pub form_groups: Vec<SharedFormGroupV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SharedFormsStepContentV3 {
    pub forms_format: SharedFormsFormatV3,
    #[schema(max_items = 2000)]
    pub contents: Vec<FormContentV3>,
    #[schema(max_items = 2000)]
    pub pos: Vec<SharedPosFormsV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpandedFormsStepContentV3 {
    #[schema(max_items = 2000)]
    pub pos: Vec<WordPosFormsV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct RetiredContentSlotV3 {
    pub id: Uuid,
    pub content_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub parent_slot_id: Option<Uuid>,
    #[schema(pattern = "^(content|common|uk|us|pronunciation)$")]
    pub role: String,
    pub retired_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum FormsStepRepresentationV3 {
    Shared(SharedFormsStepContentV3),
    Expanded(ExpandedFormsStepContentV3),
}
