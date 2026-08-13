use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::{PaginatedResponse, PaginationMeta};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PartListQuery {
    /// code、中文名、英文名或缩写的忽略大小写字面子串。
    pub q: Option<String>,
    #[param(default = 1, minimum = 1)]
    pub page: Option<u32>,
    #[param(default = 10, minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteRevisionQuery {
    #[param(minimum = 1)]
    pub base_revision: i64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct PartPath {
    pub id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct SubPartPath {
    pub id: Uuid,
    pub sub_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePartRequest {
    #[schema(min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub code: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    #[schema(min_length = 1, max_length = 16)]
    pub abbreviation: String,
    pub sort_order: i32,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdatePartRequest {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    #[schema(min_length = 1, max_length = 16)]
    pub abbreviation: String,
    pub sort_order: i32,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSubPartRequest {
    #[schema(min_length = 1, max_length = 32, pattern = "^[A-Z][A-Z0-9_-]{0,31}$")]
    pub code: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    pub sort_order: i32,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSubPartRequest {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    pub sort_order: i32,
}

#[derive(Debug)]
pub(crate) struct NewPart {
    pub id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub abbreviation: String,
    pub sort_order: i32,
    pub actor_id: Uuid,
}

#[derive(Debug)]
pub(crate) struct PartChanges {
    pub name_zh: String,
    pub name_en: String,
    pub abbreviation: String,
    pub sort_order: i32,
}

#[derive(Debug)]
pub(crate) struct NewSubPart {
    pub id: Uuid,
    pub part_of_speech_id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub sort_order: i32,
    pub actor_id: Uuid,
}

#[derive(Debug)]
pub(crate) struct SubPartChanges {
    pub name_zh: String,
    pub name_en: String,
    pub sort_order: i32,
}

#[derive(Debug, Serialize, ToSchema, PartialEq, Eq)]
pub struct Actor {
    pub id: String,
    pub display_name: String,
}

impl Actor {
    fn system() -> Self {
        Self {
            id: "system".to_owned(),
            display_name: "系统".to_owned(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PartOfSpeechConfig {
    pub id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub abbreviation: String,
    pub sort_order: i32,
    pub usage_count: i64,
    pub sub_part_count: i64,
    pub allowed_form_types: Vec<String>,
    pub default_form_types: Vec<String>,
    pub revision: i64,
    pub created_by: Actor,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub updated_by: Option<Actor>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubPartOfSpeechConfig {
    pub id: Uuid,
    pub part_of_speech_id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub sort_order: i32,
    pub usage_count: i64,
    pub revision: i64,
    pub created_by: Actor,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub updated_by: Option<Actor>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogResponse {
    pub catalog_version: i64,
    pub items: Vec<CatalogPart>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogPart {
    pub id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub abbreviation: String,
    pub sort_order: i32,
    pub allowed_form_types: Vec<String>,
    pub default_form_types: Vec<String>,
    pub sub_parts: Vec<CatalogSubPart>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogSubPart {
    pub id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub sort_order: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubPartListResponse {
    pub items: Vec<SubPartOfSpeechConfig>,
}

pub type PartListResponse = PaginatedResponse<PartOfSpeechConfig>;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct PartRecord {
    pub id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub abbreviation: String,
    pub sort_order: i32,
    pub revision: i64,
    pub created_by_admin_id: Option<Uuid>,
    pub created_by_display_name: Option<String>,
    pub updated_by_admin_id: Option<Uuid>,
    pub updated_by_display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub usage_count: i64,
    pub sub_part_count: i64,
}

impl From<PartRecord> for PartOfSpeechConfig {
    fn from(value: PartRecord) -> Self {
        let allowed_form_types = crate::lexicon::form_types::owned_allowed_form_types(&value.code);
        Self {
            id: value.id,
            code: value.code,
            name_zh: value.name_zh,
            name_en: value.name_en,
            abbreviation: value.abbreviation,
            sort_order: value.sort_order,
            usage_count: value.usage_count,
            sub_part_count: value.sub_part_count,
            default_form_types: allowed_form_types.clone(),
            allowed_form_types,
            revision: value.revision,
            created_by: actor(value.created_by_admin_id, value.created_by_display_name)
                .unwrap_or_else(Actor::system),
            created_at: value.created_at,
            updated_by: actor(value.updated_by_admin_id, value.updated_by_display_name),
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct SubPartRecord {
    pub id: Uuid,
    pub part_of_speech_id: Uuid,
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub sort_order: i32,
    pub revision: i64,
    pub created_by_admin_id: Option<Uuid>,
    pub created_by_display_name: Option<String>,
    pub updated_by_admin_id: Option<Uuid>,
    pub updated_by_display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub usage_count: i64,
}

impl From<SubPartRecord> for SubPartOfSpeechConfig {
    fn from(value: SubPartRecord) -> Self {
        Self {
            id: value.id,
            part_of_speech_id: value.part_of_speech_id,
            code: value.code,
            name_zh: value.name_zh,
            name_en: value.name_en,
            sort_order: value.sort_order,
            usage_count: value.usage_count,
            revision: value.revision,
            created_by: actor(value.created_by_admin_id, value.created_by_display_name)
                .unwrap_or_else(Actor::system),
            created_at: value.created_at,
            updated_by: actor(value.updated_by_admin_id, value.updated_by_display_name),
            updated_at: value.updated_at,
        }
    }
}

fn actor(id: Option<Uuid>, display_name: Option<String>) -> Option<Actor> {
    id.zip(display_name).map(|(id, display_name)| Actor {
        id: id.to_string(),
        display_name,
    })
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct CatalogFlatRecord {
    pub catalog_version: i64,
    pub part_id: Option<Uuid>,
    pub part_code: Option<String>,
    pub part_name_zh: Option<String>,
    pub part_name_en: Option<String>,
    pub part_abbreviation: Option<String>,
    pub part_sort_order: Option<i32>,
    pub sub_id: Option<Uuid>,
    pub sub_code: Option<String>,
    pub sub_name_zh: Option<String>,
    pub sub_name_en: Option<String>,
    pub sub_sort_order: Option<i32>,
}

#[derive(Debug)]
pub(crate) struct PartListFilter {
    pub q: Option<String>,
    pub page: u32,
    pub page_size: u32,
}

impl PartListFilter {
    pub fn pagination(&self, total: i64) -> PaginationMeta {
        let total_pages = if total == 0 {
            0
        } else {
            (total + i64::from(self.page_size) - 1) / i64::from(self.page_size)
        };
        PaginationMeta {
            page: self.page,
            page_size: self.page_size,
            total,
            total_pages,
        }
    }

    pub fn limit(&self) -> i64 {
        i64::from(self.page_size)
    }

    pub fn offset(&self) -> i64 {
        i64::from(self.page - 1) * self.limit()
    }
}
