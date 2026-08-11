use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    catalog::model::{
        CatalogFlatRecord, NewPart, NewSubPart, PartChanges, PartListFilter, PartRecord,
        SubPartChanges, SubPartRecord,
    },
    platform::{is_foreign_key_violation, is_unique_violation},
};

const PART_LIST_SQL: &str = r#"
    SELECT p.id, p.code, p.name_zh, p.name_en, p.abbreviation, p.sort_order, p.revision,
           p.created_by_admin_id, creator.display_name AS created_by_display_name,
           p.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           p.created_at, p.updated_at,
           0::bigint AS usage_count,
           count(child.id)::bigint AS sub_part_count
    FROM catalog.parts_of_speech p
    LEFT JOIN catalog.sub_parts_of_speech child ON child.part_of_speech_id = p.id
    LEFT JOIN admins creator ON creator.id = p.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = p.updated_by_admin_id
    WHERE $1::text IS NULL
       OR strpos(lower(p.code), lower($1)) > 0
       OR strpos(lower(p.name_zh), lower($1)) > 0
       OR strpos(lower(p.name_en), lower($1)) > 0
       OR strpos(lower(p.abbreviation), lower($1)) > 0
    GROUP BY p.id, creator.id, updater.id
    ORDER BY p.sort_order, p.created_at, p.id
    LIMIT $2 OFFSET $3
"#;

const PART_BY_ID_SQL: &str = r#"
    SELECT p.id, p.code, p.name_zh, p.name_en, p.abbreviation, p.sort_order, p.revision,
           p.created_by_admin_id, creator.display_name AS created_by_display_name,
           p.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           p.created_at, p.updated_at,
           0::bigint AS usage_count,
           count(child.id)::bigint AS sub_part_count
    FROM catalog.parts_of_speech p
    LEFT JOIN catalog.sub_parts_of_speech child ON child.part_of_speech_id = p.id
    LEFT JOIN admins creator ON creator.id = p.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = p.updated_by_admin_id
    WHERE p.id = $1
    GROUP BY p.id, creator.id, updater.id
"#;

const SUB_PART_LIST_SQL: &str = r#"
    SELECT s.id, s.part_of_speech_id, s.code, s.name_zh, s.name_en, s.sort_order, s.revision,
           s.created_by_admin_id, creator.display_name AS created_by_display_name,
           s.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           s.created_at, s.updated_at,
           0::bigint AS usage_count
    FROM catalog.sub_parts_of_speech s
    LEFT JOIN admins creator ON creator.id = s.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = s.updated_by_admin_id
    WHERE s.part_of_speech_id = $1
    ORDER BY s.sort_order, s.created_at, s.id
"#;

const SUB_PART_BY_ID_SQL: &str = r#"
    SELECT s.id, s.part_of_speech_id, s.code, s.name_zh, s.name_en, s.sort_order, s.revision,
           s.created_by_admin_id, creator.display_name AS created_by_display_name,
           s.updated_by_admin_id, updater.display_name AS updated_by_display_name,
           s.created_at, s.updated_at,
           0::bigint AS usage_count
    FROM catalog.sub_parts_of_speech s
    LEFT JOIN admins creator ON creator.id = s.created_by_admin_id
    LEFT JOIN admins updater ON updater.id = s.updated_by_admin_id
    WHERE s.part_of_speech_id = $1 AND s.id = $2
"#;

#[derive(Debug, thiserror::Error)]
pub enum CatalogRepositoryError {
    #[error("part of speech conflicts on {0}")]
    PartConflict(&'static str),
    #[error("sub part of speech conflicts on {0}")]
    SubPartConflict(&'static str),
    #[error("part of speech is in use")]
    PartInUse,
    #[error("sub part of speech is in use")]
    SubPartInUse,
    #[error("parent part of speech no longer exists")]
    ParentNotFound,
    #[error("catalog invariant violated: {0}")]
    Invariant(&'static str),
    #[error("database operation failed")]
    Database(#[source] sqlx::Error),
}

pub struct CatalogRepository {
    pool: PgPool,
}

impl CatalogRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 单条 SELECT 同时读取 metadata 与目录内容，保证版本和 items 来自同一 MVCC 快照。
    pub(crate) async fn catalog(&self) -> Result<Vec<CatalogFlatRecord>, CatalogRepositoryError> {
        sqlx::query_as::<_, CatalogFlatRecord>(
            r#"
            SELECT m.version AS catalog_version,
                   p.id AS part_id, p.code AS part_code, p.name_zh AS part_name_zh,
                   p.name_en AS part_name_en, p.abbreviation AS part_abbreviation,
                   p.sort_order AS part_sort_order,
                   s.id AS sub_id, s.code AS sub_code, s.name_zh AS sub_name_zh,
                   s.name_en AS sub_name_en, s.sort_order AS sub_sort_order
            FROM catalog.metadata m
            LEFT JOIN catalog.parts_of_speech p ON m.id = TRUE
            LEFT JOIN catalog.sub_parts_of_speech s ON s.part_of_speech_id = p.id
            ORDER BY p.sort_order, p.created_at, p.id,
                     s.sort_order, s.created_at, s.id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(CatalogRepositoryError::Database)
    }

    pub(crate) async fn list_parts(
        &self,
        filter: &PartListFilter,
    ) -> Result<(Vec<PartRecord>, i64), CatalogRepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(CatalogRepositoryError::Database)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(CatalogRepositoryError::Database)?;

        let total = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT count(*)
            FROM catalog.parts_of_speech p
            WHERE $1::text IS NULL
               OR strpos(lower(p.code), lower($1)) > 0
               OR strpos(lower(p.name_zh), lower($1)) > 0
               OR strpos(lower(p.name_en), lower($1)) > 0
               OR strpos(lower(p.abbreviation), lower($1)) > 0
            "#,
        )
        .bind(filter.q.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(CatalogRepositoryError::Database)?;

        let records = sqlx::query_as::<_, PartRecord>(PART_LIST_SQL)
            .bind(filter.q.as_deref())
            .bind(filter.limit())
            .bind(filter.offset())
            .fetch_all(&mut *tx)
            .await
            .map_err(CatalogRepositoryError::Database)?;

        tx.commit()
            .await
            .map_err(CatalogRepositoryError::Database)?;
        Ok((records, total))
    }

    pub(crate) async fn list_sub_parts(
        &self,
        part_id: Uuid,
    ) -> Result<Option<Vec<SubPartRecord>>, CatalogRepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(CatalogRepositoryError::Database)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(CatalogRepositoryError::Database)?;

        let parent_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM catalog.parts_of_speech WHERE id = $1)",
        )
        .bind(part_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(CatalogRepositoryError::Database)?;
        if !parent_exists {
            tx.commit()
                .await
                .map_err(CatalogRepositoryError::Database)?;
            return Ok(None);
        }

        let records = sqlx::query_as::<_, SubPartRecord>(SUB_PART_LIST_SQL)
            .bind(part_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(CatalogRepositoryError::Database)?;
        tx.commit()
            .await
            .map_err(CatalogRepositoryError::Database)?;
        Ok(Some(records))
    }

    pub(crate) async fn insert_part(
        tx: &mut Transaction<'_, Postgres>,
        value: &NewPart,
    ) -> Result<(), CatalogRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO catalog.parts_of_speech (
                id, code, name_zh, name_en, abbreviation, sort_order, created_by_admin_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(value.id)
        .bind(&value.code)
        .bind(&value.name_zh)
        .bind(&value.name_en)
        .bind(&value.abbreviation)
        .bind(value.sort_order)
        .bind(value.actor_id)
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(map_part_write_error)
    }

    pub(crate) async fn update_part(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
        base_revision: i64,
        actor_id: Uuid,
        changes: &PartChanges,
    ) -> Result<bool, CatalogRepositoryError> {
        sqlx::query(
            r#"
            UPDATE catalog.parts_of_speech
            SET name_zh = $3, name_en = $4, abbreviation = $5, sort_order = $6,
                revision = revision + 1, updated_by_admin_id = $7, updated_at = now()
            WHERE id = $1 AND revision = $2
            "#,
        )
        .bind(id)
        .bind(base_revision)
        .bind(&changes.name_zh)
        .bind(&changes.name_en)
        .bind(&changes.abbreviation)
        .bind(changes.sort_order)
        .bind(actor_id)
        .execute(&mut **tx)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(map_part_write_error)
    }

    pub(crate) async fn insert_sub_part(
        tx: &mut Transaction<'_, Postgres>,
        value: &NewSubPart,
    ) -> Result<(), CatalogRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO catalog.sub_parts_of_speech (
                id, part_of_speech_id, code, name_zh, name_en, sort_order, created_by_admin_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(value.id)
        .bind(value.part_of_speech_id)
        .bind(&value.code)
        .bind(&value.name_zh)
        .bind(&value.name_en)
        .bind(value.sort_order)
        .bind(value.actor_id)
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(map_sub_part_write_error)
    }

    pub(crate) async fn update_sub_part(
        tx: &mut Transaction<'_, Postgres>,
        part_id: Uuid,
        sub_id: Uuid,
        base_revision: i64,
        actor_id: Uuid,
        changes: &SubPartChanges,
    ) -> Result<bool, CatalogRepositoryError> {
        sqlx::query(
            r#"
            UPDATE catalog.sub_parts_of_speech
            SET name_zh = $4, name_en = $5, sort_order = $6,
                revision = revision + 1, updated_by_admin_id = $7, updated_at = now()
            WHERE part_of_speech_id = $1 AND id = $2 AND revision = $3
            "#,
        )
        .bind(part_id)
        .bind(sub_id)
        .bind(base_revision)
        .bind(&changes.name_zh)
        .bind(&changes.name_en)
        .bind(changes.sort_order)
        .bind(actor_id)
        .execute(&mut **tx)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(map_sub_part_write_error)
    }

    pub(crate) async fn part_revision(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
        lock: bool,
    ) -> Result<Option<(i64, String)>, CatalogRepositoryError> {
        let query = if lock {
            sqlx::query_as::<_, (i64, String)>(
                "SELECT revision, code FROM catalog.parts_of_speech WHERE id = $1 FOR UPDATE",
            )
        } else {
            sqlx::query_as::<_, (i64, String)>(
                "SELECT revision, code FROM catalog.parts_of_speech WHERE id = $1",
            )
        };
        query
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(CatalogRepositoryError::Database)
    }

    pub(crate) async fn sub_part_revision(
        tx: &mut Transaction<'_, Postgres>,
        part_id: Uuid,
        sub_id: Uuid,
        lock: bool,
    ) -> Result<Option<(i64, String)>, CatalogRepositoryError> {
        let query = if lock {
            sqlx::query_as::<_, (i64, String)>(
                "SELECT revision, code FROM catalog.sub_parts_of_speech \
                 WHERE part_of_speech_id = $1 AND id = $2 FOR UPDATE",
            )
        } else {
            sqlx::query_as::<_, (i64, String)>(
                "SELECT revision, code FROM catalog.sub_parts_of_speech \
                 WHERE part_of_speech_id = $1 AND id = $2",
            )
        };
        query
            .bind(part_id)
            .bind(sub_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(CatalogRepositoryError::Database)
    }

    pub(crate) async fn delete_part(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<(), CatalogRepositoryError> {
        sqlx::query("DELETE FROM catalog.parts_of_speech WHERE id = $1")
            .bind(id)
            .execute(&mut **tx)
            .await
            .map(|_| ())
            .map_err(map_part_delete_error)
    }

    pub(crate) async fn delete_sub_part(
        tx: &mut Transaction<'_, Postgres>,
        part_id: Uuid,
        sub_id: Uuid,
    ) -> Result<(), CatalogRepositoryError> {
        sqlx::query(
            "DELETE FROM catalog.sub_parts_of_speech WHERE part_of_speech_id = $1 AND id = $2",
        )
        .bind(part_id)
        .bind(sub_id)
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(map_sub_part_delete_error)
    }

    pub(crate) async fn bump_version(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(), CatalogRepositoryError> {
        sqlx::query(
            "UPDATE catalog.metadata SET version = version + 1, updated_at = now() WHERE id = TRUE",
        )
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(CatalogRepositoryError::Database)
    }

    pub(crate) async fn part_by_id(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<Option<PartRecord>, CatalogRepositoryError> {
        sqlx::query_as::<_, PartRecord>(PART_BY_ID_SQL)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(CatalogRepositoryError::Database)
    }

    pub(crate) async fn sub_part_by_id(
        tx: &mut Transaction<'_, Postgres>,
        part_id: Uuid,
        sub_id: Uuid,
    ) -> Result<Option<SubPartRecord>, CatalogRepositoryError> {
        sqlx::query_as::<_, SubPartRecord>(SUB_PART_BY_ID_SQL)
            .bind(part_id)
            .bind(sub_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(CatalogRepositoryError::Database)
    }
}

fn map_part_write_error(error: sqlx::Error) -> CatalogRepositoryError {
    for (constraint, field) in [
        ("catalog_parts_of_speech_code_unique_idx", "code"),
        ("catalog_parts_of_speech_name_zh_unique_idx", "name_zh"),
        ("catalog_parts_of_speech_name_en_unique_idx", "name_en"),
        (
            "catalog_parts_of_speech_abbreviation_unique_idx",
            "abbreviation",
        ),
    ] {
        if is_unique_violation(&error, constraint) {
            return CatalogRepositoryError::PartConflict(field);
        }
    }
    CatalogRepositoryError::Database(error)
}

fn map_sub_part_write_error(error: sqlx::Error) -> CatalogRepositoryError {
    for (constraint, field) in [
        ("catalog_sub_parts_code_unique_idx", "code"),
        ("catalog_sub_parts_name_zh_unique_idx", "name_zh"),
        ("catalog_sub_parts_name_en_unique_idx", "name_en"),
    ] {
        if is_unique_violation(&error, constraint) {
            return CatalogRepositoryError::SubPartConflict(field);
        }
    }
    if is_foreign_key_violation(&error, "catalog_sub_parts_parent_fkey") {
        return CatalogRepositoryError::ParentNotFound;
    }
    CatalogRepositoryError::Database(error)
}

fn map_part_delete_error(error: sqlx::Error) -> CatalogRepositoryError {
    for constraint in [
        "lexicon_entry_pos_catalog_pos_fkey",
        "lexicon_senses_catalog_sub_pos_fkey",
        "lexicon_publication_pos_refs_catalog_fkey",
        "lexicon_publication_sub_pos_refs_catalog_fkey",
    ] {
        if is_foreign_key_violation(&error, constraint) {
            return CatalogRepositoryError::PartInUse;
        }
    }
    CatalogRepositoryError::Database(error)
}

fn map_sub_part_delete_error(error: sqlx::Error) -> CatalogRepositoryError {
    for constraint in [
        "lexicon_senses_catalog_sub_pos_fkey",
        "lexicon_publication_sub_pos_refs_catalog_fkey",
    ] {
        if is_foreign_key_violation(&error, constraint) {
            return CatalogRepositoryError::SubPartInUse;
        }
    }
    CatalogRepositoryError::Database(error)
}
