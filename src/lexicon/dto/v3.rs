use super::*;

use std::ops::{Deref, DerefMut};

use crate::lexicon::audio_assets::dto::AudioAsset;

use serde::Serializer;
use utoipa::openapi::schema::{Object, ObjectBuilder, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceAwareVec<T> {
    values: Vec<T>,
    present: bool,
}

impl<T> PresenceAwareVec<T> {
    pub(crate) const fn was_present(&self) -> bool {
        self.present
    }

    /// `skip_serializing_if` 只接受路径而不走 `Deref`，所以固有方法不能省。
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub(crate) fn preserve_missing_from(&mut self, values: &[T])
    where
        T: Clone,
    {
        if !self.present {
            self.values = values.to_vec();
            self.present = true;
        }
    }
}

impl<T> Default for PresenceAwareVec<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            present: false,
        }
    }
}

impl<T> From<Vec<T>> for PresenceAwareVec<T> {
    fn from(values: Vec<T>) -> Self {
        Self {
            values,
            present: true,
        }
    }
}

impl<T> Deref for PresenceAwareVec<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl<T> DerefMut for PresenceAwareVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.present = true;
        &mut self.values
    }
}

impl<'a, T> IntoIterator for &'a PresenceAwareVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut PresenceAwareVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter_mut()
    }
}

impl<T> IntoIterator for PresenceAwareVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter()
    }
}

impl<T> FromIterator<T> for PresenceAwareVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        iter.into_iter().collect::<Vec<_>>().into()
    }
}

impl<T> Serialize for PresenceAwareVec<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.values.serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for PresenceAwareVec<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<T>::deserialize(deserializer).map(Into::into)
    }
}

pub(super) fn schema_version_2_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .enum_values(Some([2_u8]))
        .build()
}

pub(super) fn schema_version_3_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .enum_values(Some([3_u8]))
        .build()
}

pub(super) fn deserialize_schema_version_2<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_schema_version::<2, D>(deserializer)
}

pub(super) fn deserialize_schema_version_3<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_schema_version::<3, D>(deserializer)
}

fn deserialize_schema_version<'de, const VERSION: u8, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if value == VERSION {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "schema_version must be {VERSION}"
        )))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum EnglishLanguageV3 {
    #[serde(rename = "en")]
    En,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WordEntryKindV3 {
    Word,
    Phrase,
}

/// Stable catalog code; membership is validated in the write transaction.
pub type WordFormTypeV3 = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommonDialectV3 {
    Common,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UkDialectV3 {
    Uk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsDialectV3 {
    Us,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PhraseComponentUsageV3 {
    Unresolved {
        id: Uuid,
        #[schema(max_length = 200)]
        literal: String,
    },
    Resolved {
        id: Uuid,
        #[schema(max_length = 200)]
        literal: String,
        target_word_id: Uuid,
        /// 缺省 = 目标单词还是从未发布的草稿，语义同 `TextLinkV3.target_publication_id`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        target_publication_id: Option<Uuid>,
        target_pos_id: Uuid,
        target_base_form_id: Uuid,
        target_sense_id: Uuid,
        target_form_id: Uuid,
        target_variant_id: Uuid,
        target_dialect: Dialect,
        #[schema(value_type = String, min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
        target_form_type: WordFormTypeV3,
        #[schema(max_length = 500)]
        target_headword: String,
        #[schema(max_length = 5000)]
        target_gloss: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordPronunciationV3 {
    pub id: Uuid,
    #[schema(max_length = 200)]
    pub dict_phonetic: String,
    /// 编辑器正文与 dict_phonetic 保持一致；旧记录不输出扩展字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub dict_phonetic_rich: Option<RichTextV3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub voice_profile: Option<VoiceProfileV3>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 8)]
    pub audio_assets: Vec<AudioAsset>,
    #[schema(max_length = 200)]
    pub actual_pron: String,
    /// 编辑器正文与 actual_pron 保持一致。连读只标在这里：字典音标那一侧负责
    /// 喂语音合成，连读是纯展示的标注，两边分工不混。旧记录不输出扩展字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub actual_pron_rich: Option<RichTextV3>,
    /// Draft 可暂未选择；complete/publish 必须有值。
    #[serde(
        default,
        deserialize_with = "deserialize_optional_pronunciation_style",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(nullable = false)]
    pub style: Option<PronunciationStyle>,
}

fn deserialize_optional_pronunciation_style<'de, D>(
    deserializer: D,
) -> Result<Option<PronunciationStyle>, D::Error>
where
    D: Deserializer<'de>,
{
    PronunciationStyle::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordCommonFormVariantV3 {
    pub id: Uuid,
    pub dialect: CommonDialectV3,
    #[schema(max_length = 200)]
    pub spelling: String,
    pub origin: TextOrigin,
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<WordPronunciationV3>,
    #[serde(default)]
    #[schema(value_type = Vec<PhraseComponentUsageV3>, max_items = 100)]
    pub component_usages: PresenceAwareVec<PhraseComponentUsageV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordUkFormVariantV3 {
    pub id: Uuid,
    pub dialect: UkDialectV3,
    #[schema(max_length = 200)]
    pub spelling: String,
    pub origin: TextOrigin,
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<WordPronunciationV3>,
    #[serde(default)]
    #[schema(value_type = Vec<PhraseComponentUsageV3>, max_items = 100)]
    pub component_usages: PresenceAwareVec<PhraseComponentUsageV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordUsFormVariantV3 {
    pub id: Uuid,
    pub dialect: UsDialectV3,
    #[schema(max_length = 200)]
    pub spelling: String,
    pub origin: TextOrigin,
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<WordPronunciationV3>,
    #[serde(default)]
    #[schema(value_type = Vec<PhraseComponentUsageV3>, max_items = 100)]
    pub component_usages: PresenceAwareVec<PhraseComponentUsageV3>,
}

/// 地区形状严格为 common xor 完整 uk+us；draft 也不能缺整个 uk/us 节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum WordRegionalVariantsV3 {
    Common {
        common: WordCommonFormVariantV3,
    },
    UkUs {
        uk: WordUkFormVariantV3,
        us: WordUsFormVariantV3,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordConcreteFormV3 {
    pub id: Uuid,
    #[schema(value_type = String, min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub form_type: WordFormTypeV3,
    pub regional_variants: WordRegionalVariantsV3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordFormGroupMemberV3 {
    pub id: Uuid,
    pub form_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordFormGroupV3 {
    pub id: Uuid,
    /// 仅保留 V2 迁移元数据，不表达 base/derived 父子关系。
    pub is_regular: bool,
    #[schema(max_items = 2000)]
    pub members: Vec<WordFormGroupMemberV3>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DialectModeV3 {
    Unified,
    Distinguish,
}

impl DialectModeV3 {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unified => "unified",
            Self::Distinguish => "distinguish",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DialectRulesV3 {
    pub spelling_mode: DialectModeV3,
    pub phonetic_mode: DialectModeV3,
}

impl DialectRulesV3 {
    pub(crate) const UNIFIED: Self = Self {
        spelling_mode: DialectModeV3::Unified,
        phonetic_mode: DialectModeV3::Unified,
    };
    pub(crate) const UNIFIED_DISTINGUISH: Self = Self {
        spelling_mode: DialectModeV3::Unified,
        phonetic_mode: DialectModeV3::Distinguish,
    };
    pub(crate) const DISTINGUISH: Self = Self {
        spelling_mode: DialectModeV3::Distinguish,
        phonetic_mode: DialectModeV3::Distinguish,
    };

    pub(crate) const fn is_valid(self) -> bool {
        !matches!(
            (self.spelling_mode, self.phonetic_mode),
            (DialectModeV3::Distinguish, DialectModeV3::Unified)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordPosFormsV3 {
    pub pos_id: Uuid,
    pub pos: String,
    pub dialect_rules: DialectRulesV3,
    #[schema(max_items = 2000)]
    pub forms: Vec<WordConcreteFormV3>,
    #[schema(max_items = 2000)]
    pub form_groups: Vec<WordFormGroupV3>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftFormsStepContentV3 {
    #[schema(max_items = 2000)]
    pub pos: Vec<WordPosFormsV3>,
}

fn rich_text_version_1_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .enum_values(Some([1_u8]))
        .build()
}

fn rich_text_version_2_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::Integer)
        .enum_values(Some([2_u8]))
        .build()
}

fn deserialize_rich_text_version_1<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_rich_text_version::<1, D>(deserializer)
}

fn deserialize_rich_text_version_2<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_rich_text_version::<2, D>(deserializer)
}

fn deserialize_rich_text_version<'de, const VERSION: u8, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if value == VERSION {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "rich text version must be {VERSION}"
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RichTextSpanV3 {
    pub start: usize,
    pub end: usize,
    #[serde(rename = "type")]
    pub kind: RichTextSpanKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RichTextAnnotationV3 {
    Emphasis {
        start: usize,
        end: usize,
        level: RichTextEmphasisLevel,
    },
    Phoneme {
        start: usize,
        end: usize,
        alphabet: RichTextPhonemeAlphabet,
        #[schema(max_length = 200)]
        phoneme: String,
    },
    Liaison {
        start: usize,
        end: usize,
        /// 起点锚点的宽度：锚点占 `[start, start + start_len)`。
        #[serde(
            default = "liaison_anchor_len_default",
            skip_serializing_if = "is_liaison_anchor_len_default"
        )]
        #[schema(default = 1, minimum = 1)]
        start_len: usize,
        /// 终点锚点的宽度：锚点占 `[end - end_len, end)`。
        #[serde(
            default = "liaison_anchor_len_default",
            skip_serializing_if = "is_liaison_anchor_len_default"
        )]
        #[schema(default = 1, minimum = 1)]
        end_len: usize,
    },
    Highlight {
        start: usize,
        end: usize,
        color: RichTextHighlightColor,
    },
    Pause {
        at: usize,
        duration_ms: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RichTextV1V3 {
    #[serde(deserialize_with = "deserialize_rich_text_version_1")]
    #[schema(schema_with = rich_text_version_1_schema)]
    pub version: u8,
    #[schema(max_length = 5000)]
    pub text: String,
    #[schema(max_items = 2000)]
    pub spans: Vec<RichTextSpanV3>,
    #[schema(max_items = 2000)]
    pub liaisons: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RichTextV2V3 {
    #[serde(deserialize_with = "deserialize_rich_text_version_2")]
    #[schema(schema_with = rich_text_version_2_schema)]
    pub version: u8,
    #[schema(max_length = 5000)]
    pub text: String,
    #[schema(max_items = 2000)]
    pub annotations: Vec<RichTextAnnotationV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum RichTextV3 {
    V1(RichTextV1V3),
    V2(RichTextV2V3),
}

impl RichTextV3 {
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::V1(value) => &value.text,
            Self::V2(value) => &value.text,
        }
    }

    /// 抹掉连读标注。字典音标只负责喂语音合成，连读是展示用的，归实际发音那一侧。
    pub(crate) fn strip_liaisons(&mut self) {
        match self {
            Self::V1(value) => value.liaisons.clear(),
            Self::V2(value) => value
                .annotations
                .retain(|annotation| !matches!(annotation, RichTextAnnotationV3::Liaison { .. })),
        }
    }

    pub(crate) fn decoration_count(&self) -> usize {
        match self {
            Self::V1(value) => value.spans.len().saturating_add(value.liaisons.len()),
            Self::V2(value) => value.annotations.len(),
        }
    }
}

/// 单个音色的 C 端启用选择和独立语速；取消勾选后仍保留语速。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceSettingV3 {
    pub voice_id: String,
    pub enabled: bool,
    #[schema(minimum = -50, maximum = 100)]
    pub rate_percent: i16,
}

/// 此段文本的逐音色配置。未出现的音色默认不启用、原速。
/// voice_id 是目录 alias；历史下线音色仍可保存，不做外键式校验。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceProfileV3 {
    #[schema(max_items = 2000)]
    pub voices: Vec<VoiceSettingV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RichTextVariantV3 {
    pub id: Uuid,
    pub value: RichTextV3,
    pub origin: TextOrigin,
    /// 缺省即「没配过」。未配置时不上 wire，存量内容重新序列化后逐字节不变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub voice_profile: Option<VoiceProfileV3>,
    /// 人工指定的正文关联；缺键保留存量，显式空数组清除。
    #[serde(default, skip_serializing_if = "PresenceAwareVec::is_empty")]
    #[schema(value_type = Vec<TextLinkV3>, max_items = 100)]
    pub text_links: PresenceAwareVec<TextLinkV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TextLinkViaPhraseV3 {
    pub word_id: Uuid,
    /// 缺省 = 该短语还是从未发布的草稿，按其当前草稿内容校验成分。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub publication_id: Option<Uuid>,
    pub sense_id: Uuid,
    pub component_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TextLinkV3 {
    pub id: Uuid,
    #[schema(min_items = 1, max_items = 20)]
    pub source_segments: Vec<SentenceSourceRangeV1>,
    pub target_word_id: Uuid,
    /// 缺省 = 目标是从未发布的草稿：保存 / 发布时按目标当前草稿内容校验，发布引用记 `draft` 范围；
    /// 目标发布后，宿主下一次保存 / 发布由服务端补上当前发布版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_publication_id: Option<Uuid>,
    pub target_pos_id: Uuid,
    pub target_base_form_id: Uuid,
    pub target_form_id: Uuid,
    pub target_variant_id: Uuid,
    pub target_sense_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub via_phrase: Option<TextLinkViaPhraseV3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, read_only)]
    pub target_headword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, read_only)]
    pub target_gloss: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DialectVariantRichTextSlotV3 {
    Missing,
    Ready { variant: RichTextVariantV3 },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnglishTextV3 {
    Unified {
        common: RichTextVariantV3,
    },
    Distinguish {
        source_dialect: SourceDialect,
        uk: DialectVariantRichTextSlotV3,
        us: DialectVariantRichTextSlotV3,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GrammarVariantV3 {
    pub id: Uuid,
    pub dialect: Dialect,
    pub content: RichTextV3,
    /// 缺省即「没配过」。未配置时不上 wire，存量内容重新序列化后逐字节不变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub voice_profile: Option<VoiceProfileV3>,
    /// 已上传的真人录音。空数组不上 wire，存量内容重新序列化后逐字节不变。
    /// 元数据以服务端为准：保存时按 id 从 `lexicon.audio_assets` 重新灌入，客户端改了也无效。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 8)]
    pub audio_assets: Vec<AudioAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GrammarStructureV3 {
    pub id: Uuid,
    #[schema(max_items = 2000)]
    pub variants: Vec<GrammarVariantV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(
    tag = "definition_mode",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WordDefinitionV3 {
    ZhDefinition {
        id: Uuid,
        content_id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        grammar_structure_id: Option<Uuid>,
        content: RichTextV3,
    },
    ZhSentence {
        id: Uuid,
        content_id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        grammar_structure_id: Option<Uuid>,
        content: RichTextV3,
    },
    EnDefinition {
        id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        grammar_structure_id: Option<Uuid>,
        content: EnglishTextV3,
    },
    EnSentence {
        id: Uuid,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        grammar_structure_id: Option<Uuid>,
        content: EnglishTextV3,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordSentenceLinkV3 {
    pub word_id: Uuid,
    pub sense_id: Uuid,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WordSentenceAssociationV3 {
    Linked {
        id: Uuid,
        #[schema(schema_with = schema_version_3_schema)]
        #[serde(deserialize_with = "deserialize_schema_version_3")]
        association_schema_version: u8,
        source_dialect: Dialect,
        #[schema(min_items = 1, max_items = 20)]
        source_segments: Vec<SentenceSourceRangeV1>,
        target_word_id: Uuid,
        target_sense_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        target_form_slot_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        target_publication_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        target_form_variant_id: Option<Uuid>,
        #[serde(default)]
        #[schema(required = true, read_only, max_items = 100)]
        target_component_usages: Vec<PhraseComponentUsageV3>,
        origin: SentenceAssociationOriginV2,
        #[schema(read_only)]
        target_headword: String,
        #[schema(read_only)]
        target_gloss: String,
        #[schema(read_only)]
        resolved_pos: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false, read_only)]
        resolved_form_type: Option<String>,
    },
    Pending {
        id: Uuid,
        #[schema(schema_with = schema_version_3_schema)]
        #[serde(deserialize_with = "deserialize_schema_version_3")]
        association_schema_version: u8,
        source_dialect: Dialect,
        #[schema(min_items = 1, max_items = 20)]
        source_segments: Vec<SentenceSourceRangeV1>,
        origin: SentenceAssociationOriginV2,
        pending_target_kind: EntryKind,
        #[schema(max_length = 200)]
        pending_target_headword: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false, read_only, max_length = 200)]
        normalized_pending_target_headword: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false, max_length = 5000)]
        pending_target_gloss: Option<String>,
    },
}

/// 译文风格而非难度等级：初阶逐字直译、中阶语句通顺、高阶深层重构。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceTranslationBandV3 {
    // 别名是改名前的 CEFR 式取值，留给历史快照和部署窗口期的旧前端；
    // 两侧都上线一轮后可以摘掉。
    #[serde(alias = "c1_c2")]
    WordForWord,
    #[serde(alias = "b1_b2")]
    BalancedFluency,
    #[serde(alias = "a1_a2")]
    AdaptedCreation,
}

impl SentenceTranslationBandV3 {
    /// 老数据只有单条译文、拿不到风格时的落点：中阶既不假定逐字也不假定重构。
    pub(crate) const DEFAULT: Self = Self::BalancedFluency;

    pub(crate) const fn display_order(self) -> u8 {
        match self {
            Self::WordForWord => 0,
            Self::BalancedFluency => 1,
            Self::AdaptedCreation => 2,
        }
    }

    pub(crate) const fn field_role(self) -> &'static str {
        match self {
            Self::WordForWord => "zh_translation_word_for_word",
            Self::BalancedFluency => "zh_translation_balanced_fluency",
            Self::AdaptedCreation => "zh_translation_adapted_creation",
        }
    }
}

/// 译文语言。现阶段只开放汉语，结构为将来的多语言译文预留。
///
/// 真要放开第二种语言时，第一步是新开迁移放宽
/// `lexicon_text_variants_language_check`，否则新枚举值要到写库那一刻才撞 23514。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum TranslationLanguageV3 {
    #[serde(rename = "zh")]
    Zh,
}

impl TranslationLanguageV3 {
    /// 请求缺省 `language` 时的落点：旧前端不发该字段，一律按汉语处理。
    pub(crate) const DEFAULT: Self = Self::Zh;

    /// `lexicon.text_variants.language` 的语言码。
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordSentenceTranslationV3 {
    pub id: Uuid,
    pub band: SentenceTranslationBandV3,
    pub content: RichTextV3,
    /// 缺省按汉语处理，因而在 spec 上非必填——前端先于后端部署时靠这一点解析旧响应。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub language: Option<TranslationLanguageV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordSentenceV3 {
    pub id: Uuid,
    pub level: String,
    pub en_text: EnglishTextV3,
    pub zh_text_id: Uuid,
    pub zh_text: RichTextV3,
    /// Canonical V3 translations. Empty is accepted only as a compatibility
    /// read/write shape and is promoted from `zh_text` before persistence.
    #[serde(default)]
    #[schema(required = true, value_type = Vec<WordSentenceTranslationV3>, max_items = 2000)]
    pub zh_translations: PresenceAwareVec<WordSentenceTranslationV3>,
    #[schema(max_items = 2000)]
    pub links: Vec<WordSentenceLinkV3>,
    /// Server-owned read-only projection. Meanings writes use
    /// `WordSentenceWritableV3`, where this field does not exist.
    #[serde(default)]
    #[schema(required = true, read_only, max_items = 2000)]
    pub associations: Vec<WordSentenceAssociationV3>,
    /// Server-owned read-only projection. Meanings writes use
    /// `WordSentenceWritableV3`, where this field does not exist.
    #[serde(default)]
    #[schema(required = true, read_only)]
    pub associations_state: SentenceAssociationsStateV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordRelationV3 {
    pub id: Uuid,
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_word_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_sense_id: Option<Uuid>,
    /// Unlinked display text; saving and publishing never creates or binds a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pending_target_headword: Option<String>,
    /// Legacy text annotation; never used to create or bind a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 5000)]
    pub pending_target_gloss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, read_only)]
    pub target_headword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, read_only)]
    pub target_gloss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, read_only)]
    pub target_status: Option<AdminWordStatus>,
    pub score: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordSenseV3 {
    pub id: Uuid,
    pub sub_pos: String,
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sense_group_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub frequency: Option<String>,
    pub depends_on_context: bool,
    #[schema(max_items = 2000)]
    pub definitions: Vec<WordDefinitionV3>,
    #[schema(max_items = 2000)]
    pub sentences: Vec<WordSentenceV3>,
    #[schema(max_items = 2000)]
    pub relations: Vec<WordRelationV3>,
    /// 短语成分用词：仅 `kind = phrase` 可携带，随词义步保存；缺键表示不改动，
    /// 显式 `[]` 表示清空。
    #[serde(default, skip_serializing_if = "PresenceAwareVec::is_empty")]
    #[schema(value_type = Vec<PhraseComponentUsageV3>, max_items = 100)]
    pub component_usages: PresenceAwareVec<PhraseComponentUsageV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SenseGroupV3 {
    pub id: Uuid,
    #[schema(max_length = 200)]
    pub name_zh: String,
    #[schema(max_length = 200)]
    pub name_en: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub name_en_rich: Option<RichTextV3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub voice_profile: Option<VoiceProfileV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordPosMeaningsV3 {
    pub pos_id: Uuid,
    #[schema(max_items = 2000)]
    pub grammar_structures: Vec<GrammarStructureV3>,
    #[schema(max_items = 2000)]
    pub senses: Vec<WordSenseV3>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftMeaningsStepContentV3 {
    #[schema(max_items = 2000)]
    pub sense_groups: Vec<SenseGroupV3>,
    #[schema(max_items = 2000)]
    pub pos: Vec<WordPosMeaningsV3>,
}

/// Strict writable sentence shape. Publication associations are a server-owned
/// response projection and are intentionally absent from this request DTO.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordSentenceWritableV3 {
    pub id: Uuid,
    pub level: String,
    pub en_text: EnglishTextV3,
    pub zh_text_id: Uuid,
    pub zh_text: RichTextV3,
    #[schema(max_items = 2000)]
    pub zh_translations: Vec<WordSentenceTranslationV3>,
    #[schema(max_items = 2000)]
    pub links: Vec<WordSentenceLinkV3>,
}

/// Strict writable relation shape. Resolved target presentation is a
/// server-owned response projection and is intentionally absent here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordRelationWritableV3 {
    pub id: Uuid,
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_word_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub target_sense_id: Option<Uuid>,
    /// Unlinked display text; saving and publishing never creates or binds a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pending_target_headword: Option<String>,
    /// Legacy text annotation; never used to create or bind a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 5000)]
    pub pending_target_gloss: Option<String>,
    pub score: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordSenseWritableV3 {
    pub id: Uuid,
    pub sub_pos: String,
    pub level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sense_group_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub frequency: Option<String>,
    pub depends_on_context: bool,
    #[schema(max_items = 2000)]
    pub definitions: Vec<WordDefinitionV3>,
    #[schema(max_items = 2000)]
    pub sentences: Vec<WordSentenceWritableV3>,
    #[schema(max_items = 2000)]
    pub relations: Vec<WordRelationWritableV3>,
    /// 短语成分用词：仅 `kind = phrase` 可携带，随词义步保存；缺键表示不改动，
    /// 显式 `[]` 表示清空。
    #[serde(default, skip_serializing_if = "PresenceAwareVec::is_empty")]
    #[schema(value_type = Vec<PhraseComponentUsageV3>, max_items = 100)]
    pub component_usages: PresenceAwareVec<PhraseComponentUsageV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WordPosMeaningsWritableV3 {
    pub pos_id: Uuid,
    #[schema(max_items = 2000)]
    pub grammar_structures: Vec<GrammarStructureV3>,
    #[schema(max_items = 2000)]
    pub senses: Vec<WordSenseWritableV3>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftMeaningsStepContentWritableV3 {
    #[schema(max_items = 2000)]
    pub sense_groups: Vec<SenseGroupV3>,
    #[schema(max_items = 2000)]
    pub pos: Vec<WordPosMeaningsWritableV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryPresentationV3 {
    pub label: String,
    pub matched_surfaces: Vec<String>,
    pub strategy_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PronunciationNormalizationVersionV3 {
    NfkcTrimLowerV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum V3PublicationCapability {
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordV3Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub text_links: Option<bool>,
    pub publication: V3PublicationCapability,
    /// Phase 1 发音三元组规范化算法版本。
    pub pronunciation_normalization_version: PronunciationNormalizationVersionV3,
    /// Pending/claim/人工整组保存的独立发布闸；旧后端响应可能尚无此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sentence_associations: Option<bool>,
    /// 句内单词/短语发现的独立运行时能力；旧后端响应可能尚无此字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sentence_target_discovery: Option<bool>,
    /// Retired capability; always false. Drafts with senses remain searchable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub draft_relation_prebinding: Option<bool>,
    /// 释义级短语成分用词的编辑能力，恒为 `true`（开关已移除）。键继续下发，
    /// 好让按能力位判断的客户端不必与后端同批部署。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sense_component_usages: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordV3 {
    #[serde(default)]
    #[schema(required = true)]
    pub annotation: Option<String>,
    #[serde(default = "crate::lexicon::dto::default_annotation_revision")]
    #[schema(required = true, minimum = 1)]
    pub annotation_revision: i64,
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub id: Uuid,
    pub language: EnglishLanguageV3,
    pub kind: WordEntryKindV3,
    pub status: AdminWordStatus,
    pub revision: i64,
    pub lifecycle_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub published_revision: Option<i64>,
    pub has_unpublished_changes: bool,
    #[schema(read_only)]
    pub presentation: EntryPresentationV3,
    #[schema(read_only)]
    pub capabilities: AdminWordV3Capabilities,
    /// 原始检测 surface 唯一命中的英美侧；与管理员偏好和最终主词侧无关。
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, read_only)]
    pub detection_basis_dialect: Option<SourceDialect>,
    pub forms: DraftFormsStepContentV3,
    /// Phase 1 sense 仍只按 `pos_id` 归属，不含 group/form 可选字段。
    pub meanings: DraftMeaningsStepContentV3,
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
    #[schema(nullable = false)]
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordV3Envelope {
    pub word: AdminWordV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordDraftV3Envelope {
    pub word: AdminWordV3,
    /// response-only 的已退役稳定节点；刷新/换设备后恢复地区模式时不得生成新 UUID。
    pub retired_stable_nodes: Vec<RetiredStableNodeV3>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum V3RetiredNodeRole {
    Pos,
    FormGroup,
    GroupMembership,
    ConcreteForm,
    CommonVariant,
    UkVariant,
    UsVariant,
    Pronunciation,
    PhraseComponentUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetiredStableNodeV3 {
    pub id: Uuid,
    pub node_role: V3RetiredNodeRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub parent_node_id: Option<Uuid>,
    pub retired_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryLifecycleBatchResponse {
    pub words: Vec<AdminWordV3>,
    pub affected: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAdminWordV3Input {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub detection_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(max_length = 20)]
    pub annotation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotation_updates: Vec<EntryAnnotationUpdate>,
    pub kind: WordEntryKindV3,
    /// Step 1 最终确认值；兼容窗口内旧客户端可省略，由服务端按旧检测规则补齐。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub headwords: Option<WordHeadwordsV2>,
    /// 检测阶段 surface warning 的服务端签名确认 token；用于确认匹配集合没有漂移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewFormsImpactInputV3 {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    pub content: DraftFormsStepContentV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveFormsStepInputV3 {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    pub intent: StepSaveIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_impact_token: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
    pub content: DraftFormsStepContentV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveMeaningsStepInputV3 {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    pub intent: StepSaveIntent,
    #[schema(value_type = DraftMeaningsStepContentWritableV3)]
    pub content: DraftMeaningsStepContentV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateAdminWordV3Input {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    #[schema(minimum = 1)]
    pub base_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishAdminWordV3Input {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivatePublicationV3Input {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(minimum = 1)]
    pub base_lifecycle_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmed_surface_match_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FormsImpactNodeTypeV3 {
    Pos,
    FormGroup,
    Membership,
    Form,
    Variant,
    Pronunciation,
    PhraseComponentUsage,
    Surface,
    Publication,
    GrammarStructure,
    TextVariant,
    Sense,
    Definition,
    Sentence,
    Relation,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormsImpactItemV3 {
    pub node_id: Uuid,
    pub node_type: FormsImpactNodeTypeV3,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormsImpactResponseV3 {
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub base_revision: i64,
    pub requires_confirmation: bool,
    pub affected: Vec<FormsImpactItemV3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub confirmation_token: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub surface_match_page: Option<SurfaceMatchPageV3>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum V3ValidationIssueCode {
    InvalidRegionalVariantShape,
    DialectRulesInvalid,
    InvalidFormTypeForPartOfSpeech,
    PartOfSpeechKindMismatch,
    ForbiddenV3Field,
    DuplicateNodeId,
    DuplicatePosCode,
    PosRequired,
    FormGroupMembershipInvalid,
    OrphanForm,
    FormGroupRequired,
    EmptyFormGroup,
    BaseFormRequiredInGroup,
    VariantSpellingRequired,
    PronunciationRequired,
    DuplicatePronunciation,
    ContentLimitExceeded,
    SenseGroupRequired,
    SenseGroupNameRequired,
    SenseGroupNameTooLong,
    PosNotFound,
    DuplicatePosMeanings,
    GrammarRequired,
    GrammarVariantsInvalid,
    SenseRequired,
    LevelInvalid,
    SubPosRequired,
    InvalidSubPartOfSpeech,
    FrequencyInvalid,
    SenseGroupNotFound,
    DefinitionRequired,
    DefinitionLevelInvalid,
    DefinitionInvalid,
    NativeDefinitionRequired,
    SentenceLevelInvalid,
    SentenceIncomplete,
    SentenceTranslationRequired,
    SentenceTranslationInvalid,
    DuplicateSentenceTranslationBand,
    SentenceLinkRoleInvalid,
    DuplicateSentenceLink,
    RelationScoreInvalid,
    RelationTypeInvalid,
    RelationSelfTarget,
    RelationTargetArchived,
    RelationTargetHasNoSense,
    RelationTargetUnavailable,
    RelationTargetStale,
    SentenceContextTargetUnavailable,
    RelationPendingHeadwordInvalid,
    RelationTargetShapeInvalid,
    RelationPendingGlossWithoutHeadword,
    RelationPendingGlossInvalid,
    RelationPendingGlossConflict,
    RelationPendingGlossTargetExists,
    RelationPreboundTargetNotFound,
    RelationPreboundTargetArchived,
    RelationPreboundTargetHasNoSense,
    RelationTargetSenseDeleted,
    NodeIdReused,
    NodeBindingUnknown,
    NodeBindingChanged,
    MeaningsStorageUnsafe,
    PosMeaningsRequired,
    SenseHasInboundPublicationRefs,
    PhraseComponentNotAllowed,
    PhraseComponentLimitExceeded,
    PhraseComponentLiteralInvalid,
    PhraseComponentSelfTarget,
    PhraseComponentTargetUnavailable,
    PhraseComponentTargetNested,
    PhraseComponentTargetStale,
    PhoneticRichTextInvalid,
    VoiceProfileInvalid,
    AudioAssetInvalid,
}

impl V3ValidationIssueCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRegionalVariantShape => "invalid_regional_variant_shape",
            Self::DialectRulesInvalid => "dialect_rules_invalid",
            Self::InvalidFormTypeForPartOfSpeech => "invalid_form_type_for_part_of_speech",
            Self::PartOfSpeechKindMismatch => "part_of_speech_kind_mismatch",
            Self::ForbiddenV3Field => "forbidden_v3_field",
            Self::DuplicateNodeId => "duplicate_node_id",
            Self::DuplicatePosCode => "duplicate_pos_code",
            Self::PosRequired => "pos_required",
            Self::FormGroupMembershipInvalid => "form_group_membership_invalid",
            Self::OrphanForm => "orphan_form",
            Self::FormGroupRequired => "form_group_required",
            Self::EmptyFormGroup => "empty_form_group",
            Self::BaseFormRequiredInGroup => "base_form_required_in_group",
            Self::VariantSpellingRequired => "variant_spelling_required",
            Self::PronunciationRequired => "pronunciation_required",
            Self::DuplicatePronunciation => "duplicate_pronunciation",
            Self::ContentLimitExceeded => "content_limit_exceeded",
            Self::SenseGroupRequired => "sense_group_required",
            Self::SenseGroupNameRequired => "sense_group_name_required",
            Self::SenseGroupNameTooLong => "sense_group_name_too_long",
            Self::PosNotFound => "pos_not_found",
            Self::DuplicatePosMeanings => "duplicate_pos_meanings",
            Self::GrammarRequired => "grammar_required",
            Self::GrammarVariantsInvalid => "grammar_variants_invalid",
            Self::SenseRequired => "sense_required",
            Self::LevelInvalid => "level_invalid",
            Self::SubPosRequired => "sub_pos_required",
            Self::InvalidSubPartOfSpeech => "invalid_sub_part_of_speech",
            Self::FrequencyInvalid => "frequency_invalid",
            Self::SenseGroupNotFound => "sense_group_not_found",
            Self::DefinitionRequired => "definition_required",
            Self::DefinitionLevelInvalid => "definition_level_invalid",
            Self::DefinitionInvalid => "definition_invalid",
            Self::NativeDefinitionRequired => "native_definition_required",
            Self::SentenceLevelInvalid => "sentence_level_invalid",
            Self::SentenceIncomplete => "sentence_incomplete",
            Self::SentenceTranslationRequired => "sentence_translation_required",
            Self::SentenceTranslationInvalid => "sentence_translation_invalid",
            Self::DuplicateSentenceTranslationBand => "duplicate_sentence_translation_band",
            Self::SentenceLinkRoleInvalid => "sentence_link_role_invalid",
            Self::DuplicateSentenceLink => "duplicate_sentence_link",
            Self::RelationScoreInvalid => "relation_score_invalid",
            Self::RelationTypeInvalid => "relation_type_invalid",
            Self::RelationSelfTarget => "relation_self_target",
            Self::RelationTargetArchived => "relation_target_archived",
            Self::RelationTargetHasNoSense => "relation_target_has_no_sense",
            Self::RelationTargetUnavailable => "relation_target_unavailable",
            Self::RelationTargetStale => "relation_target_stale",
            Self::SentenceContextTargetUnavailable => "sentence_context_target_unavailable",
            Self::RelationPendingHeadwordInvalid => "relation_pending_headword_invalid",
            Self::RelationTargetShapeInvalid => "relation_target_shape_invalid",
            Self::RelationPendingGlossWithoutHeadword => "relation_pending_gloss_without_headword",
            Self::RelationPendingGlossInvalid => "relation_pending_gloss_invalid",
            Self::RelationPendingGlossConflict => "relation_pending_gloss_conflict",
            Self::RelationPendingGlossTargetExists => "relation_pending_gloss_target_exists",
            Self::RelationPreboundTargetNotFound => "relation_prebound_target_not_found",
            Self::RelationPreboundTargetArchived => "relation_prebound_target_archived",
            Self::RelationPreboundTargetHasNoSense => "relation_prebound_target_has_no_sense",
            Self::RelationTargetSenseDeleted => "relation_target_sense_deleted",
            Self::NodeIdReused => "node_id_reused",
            Self::NodeBindingUnknown => "node_binding_unknown",
            Self::NodeBindingChanged => "node_binding_changed",
            Self::MeaningsStorageUnsafe => "meanings_storage_unsafe",
            Self::PosMeaningsRequired => "pos_meanings_required",
            Self::SenseHasInboundPublicationRefs => "sense_has_inbound_publication_refs",
            Self::PhraseComponentNotAllowed => "phrase_component_not_allowed",
            Self::PhraseComponentLimitExceeded => "phrase_component_limit_exceeded",
            Self::PhraseComponentLiteralInvalid => "phrase_component_literal_invalid",
            Self::PhraseComponentSelfTarget => "phrase_component_self_target",
            Self::PhraseComponentTargetUnavailable => "phrase_component_target_unavailable",
            Self::PhraseComponentTargetNested => "phrase_component_target_nested",
            Self::PhraseComponentTargetStale => "phrase_component_target_stale",
            Self::PhoneticRichTextInvalid => "phonetic_rich_text_invalid",
            Self::VoiceProfileInvalid => "voice_profile_invalid",
            Self::AudioAssetInvalid => "audio_asset_invalid",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "invalid_regional_variant_shape" => Self::InvalidRegionalVariantShape,
            "dialect_rules_invalid" => Self::DialectRulesInvalid,
            "invalid_form_type_for_part_of_speech" => Self::InvalidFormTypeForPartOfSpeech,
            "part_of_speech_kind_mismatch" => Self::PartOfSpeechKindMismatch,
            "forbidden_v3_field" => Self::ForbiddenV3Field,
            "duplicate_node_id" => Self::DuplicateNodeId,
            "duplicate_pos_code" => Self::DuplicatePosCode,
            "pos_required" => Self::PosRequired,
            "form_group_membership_invalid" => Self::FormGroupMembershipInvalid,
            "orphan_form" => Self::OrphanForm,
            "form_group_required" => Self::FormGroupRequired,
            "empty_form_group" => Self::EmptyFormGroup,
            "base_form_required_in_group" => Self::BaseFormRequiredInGroup,
            "variant_spelling_required" => Self::VariantSpellingRequired,
            "pronunciation_required" => Self::PronunciationRequired,
            "duplicate_pronunciation" => Self::DuplicatePronunciation,
            "content_limit_exceeded" => Self::ContentLimitExceeded,
            "sense_group_required" => Self::SenseGroupRequired,
            "sense_group_name_required" => Self::SenseGroupNameRequired,
            "sense_group_name_too_long" => Self::SenseGroupNameTooLong,
            "pos_not_found" => Self::PosNotFound,
            "duplicate_pos_meanings" => Self::DuplicatePosMeanings,
            "grammar_required" => Self::GrammarRequired,
            "grammar_variants_invalid" => Self::GrammarVariantsInvalid,
            "sense_required" => Self::SenseRequired,
            "level_invalid" => Self::LevelInvalid,
            "sub_pos_required" => Self::SubPosRequired,
            "invalid_sub_part_of_speech" => Self::InvalidSubPartOfSpeech,
            "frequency_invalid" => Self::FrequencyInvalid,
            "sense_group_not_found" => Self::SenseGroupNotFound,
            "definition_required" => Self::DefinitionRequired,
            "definition_level_invalid" => Self::DefinitionLevelInvalid,
            "definition_invalid" => Self::DefinitionInvalid,
            "native_definition_required" => Self::NativeDefinitionRequired,
            "sentence_level_invalid" => Self::SentenceLevelInvalid,
            "sentence_incomplete" => Self::SentenceIncomplete,
            "sentence_translation_required" => Self::SentenceTranslationRequired,
            "sentence_translation_invalid" => Self::SentenceTranslationInvalid,
            "duplicate_sentence_translation_band" => Self::DuplicateSentenceTranslationBand,
            "sentence_link_role_invalid" => Self::SentenceLinkRoleInvalid,
            "duplicate_sentence_link" => Self::DuplicateSentenceLink,
            "relation_score_invalid" => Self::RelationScoreInvalid,
            "relation_type_invalid" => Self::RelationTypeInvalid,
            "relation_self_target" => Self::RelationSelfTarget,
            "relation_target_archived" => Self::RelationTargetArchived,
            "relation_target_has_no_sense" => Self::RelationTargetHasNoSense,
            "relation_target_unavailable" => Self::RelationTargetUnavailable,
            "relation_target_stale" => Self::RelationTargetStale,
            "sentence_context_target_unavailable" => Self::SentenceContextTargetUnavailable,
            "relation_pending_headword_invalid" => Self::RelationPendingHeadwordInvalid,
            "relation_target_shape_invalid" => Self::RelationTargetShapeInvalid,
            "relation_pending_gloss_without_headword" => Self::RelationPendingGlossWithoutHeadword,
            "relation_pending_gloss_invalid" => Self::RelationPendingGlossInvalid,
            "relation_pending_gloss_conflict" => Self::RelationPendingGlossConflict,
            "relation_pending_gloss_target_exists" => Self::RelationPendingGlossTargetExists,
            "relation_prebound_target_not_found" => Self::RelationPreboundTargetNotFound,
            "relation_prebound_target_archived" => Self::RelationPreboundTargetArchived,
            "relation_prebound_target_has_no_sense" => Self::RelationPreboundTargetHasNoSense,
            "relation_target_sense_deleted" => Self::RelationTargetSenseDeleted,
            "node_id_reused" => Self::NodeIdReused,
            "node_binding_unknown" => Self::NodeBindingUnknown,
            "node_binding_changed" => Self::NodeBindingChanged,
            "meanings_storage_unsafe" => Self::MeaningsStorageUnsafe,
            "pos_meanings_required" => Self::PosMeaningsRequired,
            "sense_has_inbound_publication_refs" => Self::SenseHasInboundPublicationRefs,
            "phrase_component_not_allowed" => Self::PhraseComponentNotAllowed,
            "phrase_component_limit_exceeded" => Self::PhraseComponentLimitExceeded,
            "phrase_component_literal_invalid" => Self::PhraseComponentLiteralInvalid,
            "phrase_component_self_target" => Self::PhraseComponentSelfTarget,
            "phrase_component_target_unavailable" => Self::PhraseComponentTargetUnavailable,
            "phrase_component_target_nested" => Self::PhraseComponentTargetNested,
            "phrase_component_target_stale" => Self::PhraseComponentTargetStale,
            "phonetic_rich_text_invalid" => Self::PhoneticRichTextInvalid,
            "voice_profile_invalid" => Self::VoiceProfileInvalid,
            "audio_asset_invalid" => Self::AudioAssetInvalid,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct V3DraftNodeLocation {
    pub node_role: String,
    pub ancestor_node_ids: Vec<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pos_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub form_group_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub membership_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub form_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub variant_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pronunciation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    #[schema(value_type = Option<String>, min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub form_type: Option<WordFormTypeV3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub dialect: Option<Dialect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct V3DraftValidationIssue {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub step: PersistedWordStep,
    pub node_id: Uuid,
    pub field: String,
    pub code: V3ValidationIssueCode,
    pub message: String,
    pub node_location: V3DraftNodeLocation,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftValidationIssueV2 {
    #[schema(schema_with = schema_version_2_schema)]
    pub schema_version: u8,
    pub step: PersistedWordStep,
    pub node_id: Uuid,
    pub field: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reference_location: Option<DraftReferenceLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub node_location: Option<DraftNodeLocation>,
}

impl From<&DraftValidationIssue> for DraftValidationIssueV2 {
    fn from(issue: &DraftValidationIssue) -> Self {
        Self {
            schema_version: 2,
            step: issue.step,
            node_id: issue.node_id,
            field: issue.field.clone(),
            code: issue.code.clone(),
            message: issue.message.clone(),
            reference_location: issue.reference_location.clone(),
            node_location: issue.node_location.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftValidationResponseV3 {
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub validated_revision: i64,
    pub valid: bool,
    pub issues: Vec<V3DraftValidationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FormSurfaceMatchV3 {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub source_schema_version: u8,
    pub entry_id: Uuid,
    pub entry_kind: WordEntryKindV3,
    pub status: AdminWordStatus,
    pub content_scope: SurfaceContentScopeV2,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub publication_id: Option<Uuid>,
    pub pos_id: Uuid,
    pub group_ids: Vec<Uuid>,
    pub form_id: Uuid,
    pub variant_id: Uuid,
    #[schema(value_type = String, min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub form_type: WordFormTypeV3,
    pub dialect: Dialect,
    pub spelling: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(
    tag = "match_kind",
    content = "match",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SurfaceMatchItemV3 {
    FormVariantV3(FormSurfaceMatchV3),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationReferencePreviewV3 {
    pub source_entry_id: Uuid,
    pub source_presentation: EntryPresentationV3,
    pub source_status: AdminWordStatus,
    pub relation: RelationTypeV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationReferenceSummaryV3 {
    pub total: u32,
    pub by_type: RelationReferenceCountsV2,
    #[schema(max_items = 5)]
    pub previews: Vec<RelationReferencePreviewV3>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MatchedEntryContextV3 {
    #[serde(default)]
    #[schema(required = true)]
    pub annotation: Option<String>,
    #[serde(default = "crate::lexicon::dto::default_annotation_revision")]
    #[schema(required = true, minimum = 1)]
    pub annotation_revision: i64,
    pub entry_id: Uuid,
    /// 创建人 admin id，供前端判断「冲突弹窗里这一行我能不能改」——超管可改任何词条，
    /// 其他管理员只能改自己创建的（含自己的草稿），见 §24.2「标注的修改权限」。
    ///
    /// **刻意是 `Option` 且不标 required**，两个方向各挡一种事故：
    /// - 对外（wire）：schema 不要求这个键，前端可以先 `sync:openapi` 并部署——旧后端不
    ///   下发时新前端按「可改」降级（等同本字段出现前的行为），之后再切后端二进制。
    ///   标成 required 则前后端两个方向都硬失败，只能同批部署。
    /// - 对内（Redis 快照）：本结构同时是 `V3SurfaceSnapshotPageData` 的存储格式。二进制
    ///   刚切换的那 ≤10 分钟里会读到升级前写的快照，那里没有这个键。用 `Uuid` + `default`
    ///   会静默填成 nil UUID，前端把本人的行误判成他人的；`None` 则是诚实的「不知道」，
    ///   序列化时整个键缺席，前端走同一条降级。
    ///
    /// `nullable = false` 是同一件事的另一半：未知时**整个键缺席、绝不会是 `null`**，
    /// schema 就不该描述一种不会发生的取值，否则前端要为一条永远走不到的分支写降级。
    ///
    /// 正常路径上服务端始终下发。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub created_by: Option<Uuid>,
    pub presentation: EntryPresentationV3,
    #[schema(max_items = 5)]
    pub pos_labels: Vec<String>,
    #[schema(max_items = 5)]
    pub gloss_previews: Vec<String>,
    pub updated_at: DateTime<Utc>,
    pub inbound_relations: RelationReferenceSummaryV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchPageBaseV3 {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub snapshot_id: Uuid,
    #[schema(max_items = 50)]
    pub items: Vec<SurfaceMatchItemV3>,
    pub total: u64,
    #[schema(max_items = 50)]
    pub matched_entry_contexts: Vec<MatchedEntryContextV3>,
    #[schema(min_items = 1, max_items = 2)]
    pub confirmation_reasons: Vec<SurfaceConfirmationReasonV2>,
    pub policy_name: SurfacePolicyNameV2,
    pub policy_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchEnabledNextPageV3 {
    #[serde(flatten)]
    pub page: SurfaceMatchPageBaseV3,
    pub continuation_policy: SurfaceContinuationEnabledV2,
    pub next_cursor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchEnabledTerminalPageV3 {
    #[serde(flatten)]
    pub page: SurfaceMatchPageBaseV3,
    pub continuation_policy: SurfaceContinuationEnabledV2,
    #[schema(schema_with = literal_null_schema_v3)]
    pub next_cursor: (),
    pub surface_confirmation_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub impact_confirmation_token: Option<Uuid>,
}

fn literal_null_schema_v3() -> Object {
    ObjectBuilder::new().schema_type(Type::Null).build()
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SurfaceMatchTemporarilyDisabledPageV3 {
    #[serde(flatten)]
    pub page: SurfaceMatchPageBaseV3,
    pub continuation_policy: SurfaceContinuationDisabledV2,
    #[schema(required = true, nullable = true)]
    pub next_cursor: Option<String>,
    pub policy_block_code: SurfacePolicyBlockCodeV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SurfaceMatchPageV3 {
    EnabledNext(SurfaceMatchEnabledNextPageV3),
    EnabledTerminal(SurfaceMatchEnabledTerminalPageV3),
    TemporarilyDisabled(SurfaceMatchTemporarilyDisabledPageV3),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordListItemV3 {
    /// Whether another visible active entry shares any base/headword prototype.
    pub annotation_visible: bool,
    #[serde(default)]
    #[schema(required = true)]
    pub annotation: Option<String>,
    #[serde(default = "crate::lexicon::dto::default_annotation_revision")]
    #[schema(required = true, minimum = 1)]
    pub annotation_revision: i64,
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub id: Uuid,
    pub kind: WordEntryKindV3,
    #[schema(read_only)]
    pub presentation: EntryPresentationV3,
    /// 方言摘要，按词性**当前**设置聚合：任一词性区分英美拼写 → `[uk, us]`，否则 `[common]`；
    /// 还没有词性的空白草稿为空数组。建条 step 1 的英美选择只是初始快照，不参与此处。
    pub dialects: Vec<Dialect>,
    pub revision: i64,
    pub lifecycle_revision: i64,
    /// response-only list projection；不得由客户端从 canonical forms 推断。
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectLexiconSurfaceV3Input {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub language: EnglishLanguageV3,
    pub kind: WordEntryKindV3,
    #[schema(max_length = 200)]
    pub surface: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectionSurfaceRequestEchoV3 {
    pub language: EnglishLanguageV3,
    pub kind: WordEntryKindV3,
    pub surface: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryProviderEvidenceV3 {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryCoverageV3 {
    pub forms: DictionaryCoverageStateV2,
    pub pronunciations: DictionaryCoverageStateV2,
    pub meanings: DictionaryCoverageStateV2,
    pub examples: DictionaryCoverageStateV2,
    pub frequency: DictionaryCoverageStateV2,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryProvenanceV3 {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub forms: Option<DictionaryProviderEvidenceV3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pronunciations: Option<DictionaryProviderEvidenceV3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub meanings: Option<DictionaryProviderEvidenceV3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub examples: Option<DictionaryProviderEvidenceV3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub frequency: Option<DictionaryProviderEvidenceV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryPronunciationEvidenceV3 {
    #[schema(max_length = 200)]
    pub dict_phonetic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, max_length = 200)]
    pub actual_pron: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub style: Option<PronunciationStyle>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestedCommonFormVariantV3 {
    pub dialect: CommonDialectV3,
    #[schema(max_length = 200)]
    pub spelling: String,
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<DictionaryPronunciationEvidenceV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestedUkFormVariantV3 {
    pub dialect: UkDialectV3,
    #[schema(max_length = 200)]
    pub spelling: String,
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<DictionaryPronunciationEvidenceV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestedUsFormVariantV3 {
    pub dialect: UsDialectV3,
    #[schema(max_length = 200)]
    pub spelling: String,
    #[schema(max_items = 2000)]
    pub pronunciations: Vec<DictionaryPronunciationEvidenceV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SuggestedRegionalVariantsV3 {
    Common {
        common: SuggestedCommonFormVariantV3,
    },
    UkUs {
        uk: SuggestedUkFormVariantV3,
        us: SuggestedUsFormVariantV3,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestedConcreteFormV3 {
    /// POS ownership is explicit; no suggested form becomes a unique entry-level headword.
    pub pos: String,
    #[schema(value_type = String, min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub form_type: WordFormTypeV3,
    pub regional_variants: SuggestedRegionalVariantsV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)] // evidence stays flat and mirrors the public detection wire
pub enum BuiltinDictionaryEvidenceV3 {
    Matched {
        provider: DictionaryProviderEvidenceV3,
        #[schema(max_items = 2000)]
        suggested_pos: Vec<String>,
        #[schema(max_items = 2000)]
        suggested_forms: Vec<SuggestedConcreteFormV3>,
        coverage: DictionaryCoverageV3,
        provenance: DictionaryProvenanceV3,
    },
    NotFound,
    Unavailable {
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        retry_after_seconds: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectLexiconSurfaceResponseV3 {
    /// Own unfinished V3 draft without saved surface sources; not a surface match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub existing_draft_id: Option<Uuid>,
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub detection_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub request: DetectionSurfaceRequestEchoV3,
    pub normalized_surface: String,
    pub builtin_dictionary: BuiltinDictionaryEvidenceV3,
    /// 内置词典与合法同表面已有词条 POS 的服务端权威合并结果。
    /// builtin evidence 本身保持来源纯净；已有词条只贡献 POS code。
    #[schema(max_items = 2000)]
    pub suggested_pos: Vec<String>,
    pub matches: Vec<SurfaceMatchItemV3>,
    pub requires_acknowledgement: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub surface_match_page: Option<SurfaceMatchPageV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordPublicationV3 {
    #[serde(deserialize_with = "deserialize_schema_version_3")]
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub publication_id: Uuid,
    pub entry_id: Uuid,
    pub publication_number: i32,
    pub source_revision: i64,
    pub word: AdminWordV3,
    pub published_by_admin_id: Uuid,
    pub published_at: DateTime<Utc>,
    pub is_current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordPublicationEnvelope {
    pub publication: AdminWordPublicationV3,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminWordPublicationListResponse {
    pub publications: Vec<AdminWordPublicationV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelatedWordMatchV3 {
    pub pos_id: Uuid,
    pub form_id: Uuid,
    pub variant_id: Uuid,
    #[schema(value_type = String, min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub form_type: WordFormTypeV3,
    pub dialect: Dialect,
    pub spelling: String,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelatedWordSenseV3 {
    pub sense_id: Uuid,
    pub gloss: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelatedWordStatusV3 {
    Draft,
    Published,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelatedWordResultV3 {
    #[schema(schema_with = schema_version_3_schema)]
    pub schema_version: u8,
    pub entry_id: Uuid,
    pub kind: WordEntryKindV3,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub status: Option<RelatedWordStatusV3>,
    #[schema(read_only)]
    pub presentation: EntryPresentationV3,
    pub matches: Vec<RelatedWordMatchV3>,
    pub senses: Vec<RelatedWordSenseV3>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_band_values_still_deserialize_but_never_serialize_back() {
        for (legacy, expected) in [
            ("c1_c2", SentenceTranslationBandV3::WordForWord),
            ("b1_b2", SentenceTranslationBandV3::BalancedFluency),
            ("a1_a2", SentenceTranslationBandV3::AdaptedCreation),
        ] {
            let parsed: SentenceTranslationBandV3 =
                serde_json::from_value(serde_json::json!(legacy)).unwrap();
            assert_eq!(parsed, expected, "{legacy}");
            assert_ne!(
                serde_json::to_value(parsed).unwrap(),
                serde_json::json!(legacy),
                "{legacy} must not round-trip back to the CEFR-style name"
            );
        }
    }

    #[test]
    fn sentence_translation_language_rejects_unknown_value() {
        // 未开放的语言必须在反序列化时就被拒，否则一路落库才撞
        // `lexicon_text_variants_language_check`（23514），错误面目全非。
        let translation = |language: serde_json::Value| {
            serde_json::json!({
                "id": Uuid::now_v7(),
                "band": "balanced_fluency",
                "content": {"version": 2, "text": "译文", "annotations": []},
                "language": language,
            })
        };
        assert!(
            serde_json::from_value::<WordSentenceTranslationV3>(translation(serde_json::json!(
                "fr"
            )))
            .is_err()
        );
        let parsed = serde_json::from_value::<WordSentenceTranslationV3>(translation(
            serde_json::json!("zh"),
        ))
        .expect("汉语是当前唯一开放的取值");
        assert_eq!(parsed.language, Some(TranslationLanguageV3::Zh));
        assert_eq!(TranslationLanguageV3::Zh.code(), "zh");

        // 旧前端不发 language：必须能解析，并且序列化回去时不留 null。
        let absent = serde_json::from_value::<WordSentenceTranslationV3>(serde_json::json!({
            "id": Uuid::now_v7(),
            "band": "balanced_fluency",
            "content": {"version": 2, "text": "译文", "annotations": []},
        }))
        .expect("缺省 language 必须可解析");
        assert_eq!(absent.language, None);
        assert!(
            !serde_json::to_value(&absent)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("language"),
            "缺省的 language 不上 wire——spec 声明了 nullable = false，null 会被前端拒收"
        );
    }

    #[test]
    fn stripping_liaisons_keeps_the_other_annotations() {
        let mut rich = RichTextV3::V2(RichTextV2V3 {
            version: 2,
            text: "ab".to_owned(),
            annotations: vec![
                RichTextAnnotationV3::Liaison {
                    start: 0,
                    end: 2,
                    start_len: 1,
                    end_len: 1,
                },
                RichTextAnnotationV3::Emphasis {
                    start: 0,
                    end: 1,
                    level: RichTextEmphasisLevel::Core,
                },
            ],
        });
        rich.strip_liaisons();
        let RichTextV3::V2(value) = &rich else {
            panic!("expected a v2 rich text");
        };
        assert_eq!(value.annotations.len(), 1);
        assert!(matches!(
            value.annotations[0],
            RichTextAnnotationV3::Emphasis { .. }
        ));
    }

    fn matched_entry_context(created_by: Option<Uuid>) -> MatchedEntryContextV3 {
        MatchedEntryContextV3 {
            annotation: None,
            annotation_revision: 1,
            entry_id: Uuid::now_v7(),
            created_by,
            presentation: EntryPresentationV3 {
                label: "harbour".to_owned(),
                matched_surfaces: vec!["harbour".to_owned()],
                strategy_version: "surface_summary_v1".to_owned(),
            },
            pos_labels: Vec::new(),
            gloss_previews: Vec::new(),
            updated_at: Utc::now(),
            inbound_relations: RelationReferenceSummaryV3 {
                total: 0,
                by_type: RelationReferenceCountsV2 {
                    synonym: 0,
                    antonym: 0,
                    derivative: 0,
                },
                previews: Vec::new(),
                truncated: false,
            },
        }
    }

    /// `created_by` 缺席与 `null` 对前端不是一回事：缺席 = 「后端还没下发归属」，按可改降级；
    /// `null` 会被当成一个具体的、不等于自己的值，把本人的行误判成只读。所以未知时整个键
    /// 必须消失。
    #[test]
    fn matched_entry_context_v3_omits_unknown_creator_instead_of_nulling_it() {
        let json = serde_json::to_value(matched_entry_context(None)).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("created_by"),
            "未知创建人时该键必须缺席，不能是 null：{json}"
        );
        let actor = Uuid::now_v7();
        let json = serde_json::to_value(matched_entry_context(Some(actor))).unwrap();
        assert_eq!(json["created_by"], serde_json::json!(actor));
    }

    /// 本结构同时是 Redis 快照的存储格式（`V3SurfaceSnapshotPageData`）。二进制刚切换的
    /// 那 ≤10 分钟里会读到升级前写的快照，那里没有 `created_by`——不能因此整份快照报废。
    #[test]
    fn matched_entry_context_v3_reads_pre_upgrade_snapshots() {
        let mut json = serde_json::to_value(matched_entry_context(Some(Uuid::now_v7()))).unwrap();
        json.as_object_mut().unwrap().remove("created_by");
        let revived: MatchedEntryContextV3 = serde_json::from_value(json).unwrap();
        assert_eq!(revived.created_by, None);
    }

    #[test]
    fn surface_match_item_v3_is_strictly_discriminated_without_synthetic_ids() {
        let entry_id = Uuid::now_v7();
        let form = SurfaceMatchItemV3::FormVariantV3(FormSurfaceMatchV3 {
            source_schema_version: 3,
            entry_id,
            entry_kind: WordEntryKindV3::Word,
            status: AdminWordStatus::Published,
            content_scope: SurfaceContentScopeV2::CurrentPublication,
            publication_id: Some(Uuid::now_v7()),
            pos_id: Uuid::now_v7(),
            group_ids: vec![Uuid::now_v7()],
            form_id: Uuid::now_v7(),
            variant_id: Uuid::now_v7(),
            form_type: "base".to_owned(),
            dialect: Dialect::Uk,
            spelling: "colour".to_owned(),
        });
        let mut form_json = serde_json::to_value(&form).unwrap();
        assert_eq!(form_json["match_kind"], "form_variant_v3");
        assert!(matches!(
            serde_json::from_value::<SurfaceMatchItemV3>(form_json.clone()).unwrap(),
            SurfaceMatchItemV3::FormVariantV3(_)
        ));
        form_json["match_kind"] = serde_json::json!("legacy_v2");
        assert!(
            serde_json::from_value::<SurfaceMatchItemV3>(form_json).is_err(),
            "已下线的 legacy_v2 判别值不得再被接受"
        );
    }
}

pub const fn default_annotation_revision() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryAnnotationUpdate {
    pub entry_id: Uuid,
    #[schema(max_length = 20)]
    pub annotation: Option<String>,
    #[schema(minimum = 1)]
    pub base_annotation_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateEntryAnnotationInput {
    #[schema(max_length = 20)]
    pub annotation: Option<String>,
    #[schema(minimum = 1)]
    pub base_annotation_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryAnnotationResponse {
    pub entry_id: Uuid,
    #[schema(required = true)]
    pub annotation: Option<String>,
    #[schema(minimum = 1)]
    pub annotation_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryAnnotationGroup {
    pub dialect_scope: String,
    pub normalized_surface: String,
    pub entry_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryAnnotationConflictReason {
    Required,
    Duplicate,
    RevisionConflict,
    GroupChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryAnnotationConflict {
    pub reason: EntryAnnotationConflictReason,
    pub entries: Vec<MatchedEntryContextV3>,
    pub groups: Vec<EntryAnnotationGroup>,
}
