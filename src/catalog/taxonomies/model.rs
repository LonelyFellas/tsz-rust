use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::error::{AppError, ErrorCode};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LexiconTaxonomyKind {
    Registers,
    Domains,
}
impl LexiconTaxonomyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registers => "registers",
            Self::Domains => "domains",
        }
    }
}
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct TaxonomyPath {
    pub kind: LexiconTaxonomyKind,
}
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub struct TaxonomyItemPath {
    pub kind: LexiconTaxonomyKind,
    pub id: Uuid,
}
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TaxonomyListQuery {
    pub q: Option<String>,
    #[param(default = 1, minimum = 1)]
    pub page: Option<u32>,
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub page_size: Option<u32>,
}
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct LexiconTaxonomy {
    pub id: Uuid,
    /// 稳定编码，创建后不可修改。
    pub code: String,
    pub name_zh: String,
    pub name_en: String,
    pub sort_order: i32,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTaxonomyRequest {
    #[schema(min_length = 1, max_length = 32, pattern = "^[a-z][a-z0-9_]{0,31}$")]
    pub code: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    pub sort_order: i32,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateTaxonomyRequest {
    #[schema(minimum = 1)]
    pub base_revision: i64,
    #[schema(min_length = 1, max_length = 64)]
    pub name_zh: String,
    #[schema(min_length = 1, max_length = 64)]
    pub name_en: String,
    pub sort_order: i32,
}
pub fn text(value: String, field: &'static str) -> Result<String, AppError> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.chars().count() > 64 {
        return Err(AppError::validation(
            ErrorCode::InvalidRequestBody,
            field,
            "expected 1–64 characters",
        ));
    }
    Ok(value)
}
pub fn revision(value: i64) -> Result<(), AppError> {
    if value < 1 {
        return Err(AppError::validation(
            ErrorCode::InvalidRequestBody,
            "base_revision",
            "expected positive revision",
        ));
    }
    Ok(())
}
