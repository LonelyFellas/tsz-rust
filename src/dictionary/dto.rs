use crate::{
    api::PaginationMeta,
    lexicon::dto::RichTextV3,
    wordlists::dto::{WordlistEntry, WordlistText},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Clone, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryAudio {
    pub target_id: Uuid,
    pub asset_id: Uuid,
    pub locale: String,
    /// Present on locale-capable responses; optional in the contract for older servers.
    #[schema(required = false)]
    pub accent_group: crate::speech::locale::AccentGroup,
    /// Source text dialect, independent of the voice's native locale.
    #[schema(required = false)]
    pub source_dialect: crate::lexicon::dto::Dialect,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) hd_flash: bool,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) setting: Option<crate::lexicon::dto::VoiceSettingV3>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryExample {
    pub id: Uuid,
    pub sense_id: Uuid,
    pub texts: Vec<WordlistText>,
    pub translation: RichTextV3,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryEntry {
    pub content: WordlistEntry,
    pub audio: Vec<DictionaryAudio>,
    pub examples: Vec<DictionaryExample>,
}
#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DictionaryPage {
    pub items: Vec<DictionaryEntry>,
    pub pagination: PaginationMeta,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct DictionaryQuery {
    #[param(min_length = 1, max_length = 100)]
    pub q: String,
    #[param(minimum = 1, default = 1)]
    pub page: Option<u32>,
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub page_size: Option<u32>,
}
