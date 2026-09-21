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

// --- shared sentence projection vocabulary ---

/// 正文内的 Unicode 码点区间 [start, end)，surface 用于验证区间未漂移。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SentenceSourceRangeV1 {
    pub start: usize,
    pub end: usize,
    pub surface: String,
}

/// 关联来源，只用于展示与质量评估。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationOriginV2 {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationStateV1 {
    Linked,
    Pending,
}

/// 当前正文的关联投影是否已经解析；不是词条内容格式版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SentenceAssociationsStateV2 {
    #[default]
    Unresolved,
    Resolved,
}
