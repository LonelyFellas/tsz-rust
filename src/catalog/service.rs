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

    pub async fn catalog(&self) -> Result<CatalogResponse, CatalogServiceError> {
        let records = self
            .repository
            .catalog()
            .await
            .map_err(map_repository_error)?;
        let catalog_version = records
            .first()
            .map(|record| record.catalog_version)
            .ok_or_else(|| {
                CatalogServiceError::Repository(CatalogRepositoryError::Invariant(
                    "metadata row is missing",
                ))
            })?;

        let mut items: Vec<CatalogPart> = Vec::new();
        for record in records {
            let Some(part_id) = record.part_id else {
                continue;
            };

            if items.last().is_none_or(|item| item.id != part_id) {
                items.push(CatalogPart {
                    id: part_id,
                    code: required(record.part_code, "part code is null")?,
                    name_zh: required(record.part_name_zh, "part name_zh is null")?,
                    name_en: required(record.part_name_en, "part name_en is null")?,
                    abbreviation: required(record.part_abbreviation, "part abbreviation is null")?,
                    sort_order: required(record.part_sort_order, "part sort_order is null")?,
                    sub_parts: Vec::new(),
                });
            }

            if let Some(sub_id) = record.sub_id {
                items
                    .last_mut()
                    .expect("part was inserted above")
                    .sub_parts
                    .push(CatalogSubPart {
                        id: sub_id,
                        code: required(record.sub_code, "sub part code is null")?,
                        name_zh: required(record.sub_name_zh, "sub part name_zh is null")?,
                        name_en: required(record.sub_name_en, "sub part name_en is null")?,
                        sort_order: required(record.sub_sort_order, "sub part sort_order is null")?,
                    });
            }
        }

        Ok(CatalogResponse {
            catalog_version,
            items,
        })
    }

    pub async fn list_parts(
        &self,
        query: PartListQuery,
    ) -> Result<PartListResponse, CatalogServiceError> {
        let page = query.page.unwrap_or(1);
        let page_size = query.page_size.unwrap_or(10);
        if page == 0 {
            return Err(CatalogServiceError::InvalidQuery("page must be at least 1"));
        }
        if !(1..=100).contains(&page_size) {
            return Err(CatalogServiceError::InvalidQuery(
                "page_size must be between 1 and 100",
            ));
        }
        let q = query.q.and_then(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        });
        let filter = PartListFilter { q, page, page_size };
        let (records, total) = self
            .repository
            .list_parts(&filter)
            .await
            .map_err(map_repository_error)?;
        Ok(PaginatedResponse {
            items: records.into_iter().map(Into::into).collect(),
            pagination: filter.pagination(total),
        })
    }

    pub async fn list_sub_parts(
        &self,
        part_id: Uuid,
    ) -> Result<SubPartListResponse, CatalogServiceError> {
        let Some(records) = self
            .repository
            .list_sub_parts(part_id)
            .await
            .map_err(map_repository_error)?
        else {
            return Err(CatalogServiceError::PartNotFound);
        };
        Ok(SubPartListResponse {
            items: records.into_iter().map(Into::into).collect(),
        })
    }

    pub async fn create_part(
        &self,
        actor_id: Uuid,
        request: CreatePartRequest,
    ) -> Result<PartOfSpeechConfig, CatalogServiceError> {
        let value = NewPart {
            id: Uuid::now_v7(),
            code: valid_part_code(request.code)?,
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            abbreviation: normalized_text(request.abbreviation, "abbreviation", 16)?,
            sort_order: request.sort_order,
            actor_id,
        };

        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        CatalogRepository::insert_part(&mut tx, &value)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::part_by_id(&mut tx, value.id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn update_part(
        &self,
        actor_id: Uuid,
        id: Uuid,
        request: UpdatePartRequest,
    ) -> Result<PartOfSpeechConfig, CatalogServiceError> {
        validate_revision(request.base_revision)?;
        let changes = PartChanges {
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            abbreviation: normalized_text(request.abbreviation, "abbreviation", 16)?,
            sort_order: request.sort_order,
        };
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let updated =
            CatalogRepository::update_part(&mut tx, id, request.base_revision, actor_id, &changes)
                .await
                .map_err(map_repository_error)?;
        if !updated {
            return Err(revision_or_part_not_found(&mut tx, id, request.base_revision).await?);
        }
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::part_by_id(&mut tx, id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn delete_part(
        &self,
        id: Uuid,
        base_revision: i64,
    ) -> Result<(), CatalogServiceError> {
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let Some((current_revision, code)) = CatalogRepository::part_revision(&mut tx, id, true)
            .await
            .map_err(map_repository_error)?
        else {
            return Err(CatalogServiceError::PartNotFound);
        };
        if current_revision != base_revision {
            return Err(CatalogServiceError::RevisionConflict {
                current_revision,
                part_of_speech_id: id,
                code,
            });
        }
        CatalogRepository::delete_part(&mut tx, id)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(())
    }

    pub async fn create_sub_part(
        &self,
        actor_id: Uuid,
        part_id: Uuid,
        request: CreateSubPartRequest,
    ) -> Result<SubPartOfSpeechConfig, CatalogServiceError> {
        let value = NewSubPart {
            id: Uuid::now_v7(),
            part_of_speech_id: part_id,
            code: valid_sub_part_code(request.code)?,
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            sort_order: request.sort_order,
            actor_id,
        };
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        if CatalogRepository::part_revision(&mut tx, part_id, false)
            .await
            .map_err(map_repository_error)?
            .is_none()
        {
            return Err(CatalogServiceError::PartNotFound);
        }
        CatalogRepository::insert_sub_part(&mut tx, &value)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::sub_part_by_id(&mut tx, part_id, value.id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_sub_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn update_sub_part(
        &self,
        actor_id: Uuid,
        part_id: Uuid,
        sub_id: Uuid,
        request: UpdateSubPartRequest,
    ) -> Result<SubPartOfSpeechConfig, CatalogServiceError> {
        validate_revision(request.base_revision)?;
        let changes = SubPartChanges {
            name_zh: normalized_text(request.name_zh, "name_zh", 64)?,
            name_en: normalized_text(request.name_en, "name_en", 64)?,
            sort_order: request.sort_order,
        };
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let updated = CatalogRepository::update_sub_part(
            &mut tx,
            part_id,
            sub_id,
            request.base_revision,
            actor_id,
            &changes,
        )
        .await
        .map_err(map_repository_error)?;
        if !updated {
            return Err(revision_or_sub_part_not_found(
                &mut tx,
                part_id,
                sub_id,
                request.base_revision,
            )
            .await?);
        }
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        let response = CatalogRepository::sub_part_by_id(&mut tx, part_id, sub_id)
            .await
            .map_err(map_repository_error)?
            .ok_or_else(invariant_missing_sub_part)?
            .into();
        tx.commit().await.map_err(database_error)?;
        Ok(response)
    }

    pub async fn delete_sub_part(
        &self,
        part_id: Uuid,
        sub_id: Uuid,
        base_revision: i64,
    ) -> Result<(), CatalogServiceError> {
        let mut tx = self
            .repository
            .pool()
            .begin()
            .await
            .map_err(database_error)?;
        let Some((current_revision, code)) =
            CatalogRepository::sub_part_revision(&mut tx, part_id, sub_id, true)
                .await
                .map_err(map_repository_error)?
        else {
            return Err(CatalogServiceError::SubPartNotFound);
        };
        if current_revision != base_revision {
            return Err(CatalogServiceError::RevisionConflict {
                current_revision,
                part_of_speech_id: part_id,
                code,
            });
        }
        CatalogRepository::delete_sub_part(&mut tx, part_id, sub_id)
            .await
            .map_err(map_repository_error)?;
        CatalogRepository::bump_version(&mut tx)
            .await
            .map_err(map_repository_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(())
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
