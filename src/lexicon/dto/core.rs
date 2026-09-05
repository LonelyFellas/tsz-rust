use super::*;

// --- fundamentals ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Word,
    Phrase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    Common,
    Uk,
    Us,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
/// `distinguish` 词条由管理员选择的主词侧；用于主词展示顺序及后续方言内容初始化，
/// 不要求与内置词典的命中方言相同。
pub enum SourceDialect {
    Uk,
    Us,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextOrigin {
    Dictionary,
    Converted,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
/// 英美主词。检测响应中的值是建议；创建 matched 草稿时，管理员可选择任一模式并编辑
/// 各侧拼写。服务端仍会规范化并拒绝空值或非法结构。
pub enum WordHeadwordsV2 {
    Unified {
        common: String,
    },
    Distinguish {
        uk: String,
        us: String,
        /// 管理员决定的主词侧，而不是不可修改的词典检测结论。
        source_dialect: SourceDialect,
    },
}

// --- rich text ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RichTextSpan {
    pub start: usize,
    pub end: usize,
    #[serde(rename = "type")]
    pub kind: RichTextSpanKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RichTextSpanKind {
    Bold,
    Blue,
}

/// 语音编辑器的「语法结构」三分类。
///
/// `Strong` 是三分类落地之前的存量取值，读到时按「核心词」理解；新内容只写
/// `Function` / `Core` / `Grammar`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RichTextEmphasisLevel {
    Strong,
    Function,
    Core,
    Grammar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RichTextPhonemeAlphabet {
    Ipa,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RichTextHighlightColor {
    Yellow,
    Green,
    Pink,
    Blue,
    Orange,
}

/// 连读两端各自的宽度默认一个码点，也就是加宽度字段之前的形状。
pub(crate) const fn liaison_anchor_len_default() -> usize {
    1
}

/// 默认宽度不上 wire：存量内容重新序列化后逐字节不变，内容哈希才不会整体漂移。
pub(crate) fn is_liaison_anchor_len_default(value: &usize) -> bool {
    *value == liaison_anchor_len_default()
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RichTextAnnotation {
    Emphasis {
        start: usize,
        end: usize,
        level: RichTextEmphasisLevel,
    },
    Phoneme {
        start: usize,
        end: usize,
        alphabet: RichTextPhonemeAlphabet,
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RichTextV1 {
    pub version: u8,
    pub text: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_null")]
    pub spans: Vec<RichTextSpan>,
    #[serde(default, deserialize_with = "deserialize_vec_or_null")]
    pub liaisons: Vec<usize>,
}

fn deserialize_vec_or_null<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RichTextV2 {
    pub version: u8,
    pub text: String,
    pub annotations: Vec<RichTextAnnotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum RichText {
    V1(RichTextV1),
    V2(RichTextV2),
}

impl RichText {
    pub fn empty() -> Self {
        Self::V1(RichTextV1 {
            version: 1,
            text: String::new(),
            spans: Vec::new(),
            liaisons: Vec::new(),
        })
    }

    pub fn text(&self) -> &str {
        match self {
            Self::V1(value) => &value.text,
            Self::V2(value) => &value.text,
        }
    }

    pub fn version(&self) -> u8 {
        match self {
            Self::V1(value) => value.version,
            Self::V2(value) => value.version,
        }
    }
}
