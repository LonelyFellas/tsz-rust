use super::*;

impl CatalogRepository {
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
}
