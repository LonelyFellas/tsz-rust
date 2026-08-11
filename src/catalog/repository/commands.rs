use super::*;

impl CatalogRepository {
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

    /// 删除预检与管理端 usage_count 共用相同去重语义。
    ///
    /// 基本词性按 entry 去重；除直接 POS 引用外也覆盖其细分词性的引用，确保预检能解释
    /// 删除父项时可能由子项 FK 触发的阻止原因。
    pub(crate) async fn part_usage_count(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<i64, CatalogRepositoryError> {
        sqlx::query_scalar(
            r#"
            SELECT count(*)::bigint
            FROM (
                SELECT draft.entry_id
                FROM lexicon.entry_pos draft
                WHERE draft.part_of_speech_id = $1
                UNION
                SELECT publication.entry_id
                FROM lexicon.entry_publication_part_of_speech_refs publication
                WHERE publication.part_of_speech_id = $1
                UNION
                SELECT draft_sense.entry_id
                FROM lexicon.senses draft_sense
                JOIN catalog.sub_parts_of_speech used_sub
                  ON used_sub.id = draft_sense.sub_part_of_speech_id
                WHERE used_sub.part_of_speech_id = $1
                UNION
                SELECT publication_sense.entry_id
                FROM lexicon.entry_publication_sub_part_of_speech_refs publication_sense
                JOIN catalog.sub_parts_of_speech used_sub
                  ON used_sub.id = publication_sense.sub_part_of_speech_id
                WHERE used_sub.part_of_speech_id = $1
            ) usage_entries
            "#,
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(CatalogRepositoryError::Database)
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

    /// 细分词性按稳定 sense node 去重；同一 sense 同时存在于当前草稿和多个 publication
    /// 时只计一次。
    pub(crate) async fn sub_part_usage_count(
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<i64, CatalogRepositoryError> {
        sqlx::query_scalar(
            r#"
            SELECT count(*)::bigint
            FROM (
                SELECT draft.id AS source_node_id
                FROM lexicon.senses draft
                WHERE draft.sub_part_of_speech_id = $1
                UNION
                SELECT publication.source_node_id
                FROM lexicon.entry_publication_sub_part_of_speech_refs publication
                WHERE publication.sub_part_of_speech_id = $1
            ) usage_senses
            "#,
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(CatalogRepositoryError::Database)
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
