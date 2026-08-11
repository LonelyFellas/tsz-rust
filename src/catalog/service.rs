use regex::Regex;
use std::sync::OnceLock;
use uuid::Uuid;

use crate::{
    api::PaginatedResponse,
    catalog::{
        model::{
            CatalogPart, CatalogResponse, CatalogSubPart, CreatePartRequest, CreateSubPartRequest,
            NewPart, NewSubPart, PartChanges, PartListFilter, PartListQuery, PartListResponse,
            PartOfSpeechConfig, SubPartChanges, SubPartListResponse, SubPartOfSpeechConfig,
            UpdatePartRequest, UpdateSubPartRequest,
        },
        repository::{CatalogRepository, CatalogRepositoryError},
    },
};

mod commands;
mod query;

#[derive(Debug, thiserror::Error)]
pub enum CatalogServiceError {
    #[error("invalid part of speech field {field}: {message}")]
    InvalidPart {
        field: &'static str,
        message: &'static str,
    },
    #[error("invalid query: {0}")]
    InvalidQuery(&'static str),
    #[error("part of speech not found")]
    PartNotFound,
    #[error("sub part of speech not found")]
    SubPartNotFound,
    #[error("revision conflict")]
    RevisionConflict {
        current_revision: i64,
        part_of_speech_id: Uuid,
        code: String,
    },
    #[error("part of speech conflicts on {0}")]
    PartConflict(&'static str),
    #[error("sub part of speech conflicts on {0}")]
    SubPartConflict(&'static str),
    #[error("part of speech is in use")]
    PartInUse { usage_count: Option<i64> },
    #[error("sub part of speech is in use")]
    SubPartInUse { usage_count: Option<i64> },
    #[error("catalog repository failed")]
    Repository(#[source] CatalogRepositoryError),
}

pub struct CatalogService {
    repository: CatalogRepository,
}

impl CatalogService {
    pub fn new(repository: CatalogRepository) -> Self {
        Self { repository }
    }
}

fn required<T>(value: Option<T>, message: &'static str) -> Result<T, CatalogServiceError> {
    value.ok_or_else(|| CatalogServiceError::Repository(CatalogRepositoryError::Invariant(message)))
}

fn part_code_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]{0,31}$").expect("valid part code regex"))
}

fn sub_part_code_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^[A-Z][A-Z0-9_-]{0,31}$").expect("valid sub part code regex"))
}

fn valid_part_code(value: String) -> Result<String, CatalogServiceError> {
    if part_code_regex().is_match(&value) {
        Ok(value)
    } else {
        Err(invalid_part("code", "invalid part of speech code"))
    }
}

fn valid_sub_part_code(value: String) -> Result<String, CatalogServiceError> {
    if sub_part_code_regex().is_match(&value) {
        Ok(value)
    } else {
        Err(invalid_part("code", "invalid sub part of speech code"))
    }
}

fn normalized_text(
    value: String,
    field: &'static str,
    max_len: usize,
) -> Result<String, CatalogServiceError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_len {
        Err(invalid_part(field, "invalid text length"))
    } else {
        Ok(value.to_owned())
    }
}

fn validate_revision(value: i64) -> Result<(), CatalogServiceError> {
    if value < 1 {
        Err(invalid_part(
            "base_revision",
            "base_revision must be at least 1",
        ))
    } else {
        Ok(())
    }
}

fn invalid_part(field: &'static str, message: &'static str) -> CatalogServiceError {
    CatalogServiceError::InvalidPart { field, message }
}

async fn revision_or_part_not_found(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    base_revision: i64,
) -> Result<CatalogServiceError, CatalogServiceError> {
    match CatalogRepository::part_revision(tx, id, false)
        .await
        .map_err(map_repository_error)?
    {
        Some((current_revision, code)) if current_revision != base_revision => {
            Ok(CatalogServiceError::RevisionConflict {
                current_revision,
                part_of_speech_id: id,
                code,
            })
        }
        Some(_) => Ok(CatalogServiceError::Repository(
            CatalogRepositoryError::Invariant("conditional part update affected zero rows"),
        )),
        None => Ok(CatalogServiceError::PartNotFound),
    }
}

async fn revision_or_sub_part_not_found(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    part_id: Uuid,
    sub_id: Uuid,
    base_revision: i64,
) -> Result<CatalogServiceError, CatalogServiceError> {
    match CatalogRepository::sub_part_revision(tx, part_id, sub_id, false)
        .await
        .map_err(map_repository_error)?
    {
        Some((current_revision, code)) if current_revision != base_revision => {
            Ok(CatalogServiceError::RevisionConflict {
                current_revision,
                part_of_speech_id: part_id,
                code,
            })
        }
        Some(_) => Ok(CatalogServiceError::Repository(
            CatalogRepositoryError::Invariant("conditional sub part update affected zero rows"),
        )),
        None => Ok(CatalogServiceError::SubPartNotFound),
    }
}

fn invariant_missing_part() -> CatalogServiceError {
    CatalogServiceError::Repository(CatalogRepositoryError::Invariant(
        "written part cannot be read back",
    ))
}

fn invariant_missing_sub_part() -> CatalogServiceError {
    CatalogServiceError::Repository(CatalogRepositoryError::Invariant(
        "written sub part cannot be read back",
    ))
}

fn database_error(error: sqlx::Error) -> CatalogServiceError {
    CatalogServiceError::Repository(CatalogRepositoryError::Database(error))
}

fn map_repository_error(error: CatalogRepositoryError) -> CatalogServiceError {
    match error {
        CatalogRepositoryError::PartConflict(field) => CatalogServiceError::PartConflict(field),
        CatalogRepositoryError::SubPartConflict(field) => {
            CatalogServiceError::SubPartConflict(field)
        }
        CatalogRepositoryError::PartInUse => CatalogServiceError::PartInUse { usage_count: None },
        CatalogRepositoryError::SubPartInUse => {
            CatalogServiceError::SubPartInUse { usage_count: None }
        }
        CatalogRepositoryError::ParentNotFound => CatalogServiceError::PartNotFound,
        other => CatalogServiceError::Repository(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_names_but_not_codes() {
        assert_eq!(
            normalized_text("  名词  ".into(), "name_zh", 64).unwrap(),
            "名词"
        );
        assert!(valid_part_code(" noun ".into()).is_err());
        assert!(valid_sub_part_code("N-COUNT".into()).is_ok());
    }

    #[test]
    fn validates_unicode_length_by_characters() {
        assert!(normalized_text("中".repeat(64), "name_zh", 64).is_ok());
        assert!(normalized_text("中".repeat(65), "name_zh", 64).is_err());
    }
}
