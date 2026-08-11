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
pub enum WordHeadwordsV2 {
    Unified {
        common: String,
    },
    Distinguish {
        uk: String,
        us: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RichTextEmphasisLevel {
    Strong,
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
