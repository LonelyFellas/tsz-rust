use anyhow::{Context, ensure};
use serde::Serialize;
use sqlx::PgPool;

#[derive(Debug, Serialize)]
pub struct UndoReport {
    pub previous_version: i64,
    pub target_version: i64,
    pub current_version: i64,
}

/// 撤销一次失败部署新引入的迁移。调用方必须先停止所有业务写入，并提供部署前后
/// 两个精确版本；全部 down migration 与迁移账本更新在一个外层事务里提交。
pub async fn undo(
    pool: &PgPool,
    target_version: i64,
    expected_version: i64,
) -> anyhow::Result<UndoReport> {
    ensure!(
        target_version > 0,
        "target migration version must be positive"
    );
    ensure!(
        expected_version > target_version,
        "expected migration version must be newer than target"
    );

    let migrator = sqlx::migrate!("./migrations");
    ensure!(
        migrator.version_exists(target_version),
        "target migration version is not embedded in this binary"
    );
    ensure!(
        migrator.version_exists(expected_version),
        "expected migration version is not embedded in this binary"
    );

    let mut transaction = pool.begin().await?;
    let previous_version: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success IS TRUE",
    )
    .fetch_one(&mut *transaction)
    .await
    .context("failed to read current migration version")?;
    ensure!(
        previous_version == expected_version,
        "database migration version does not match the failed deployment"
    );

    let incompatible_payloads: i64 = sqlx::query_scalar(
        r#"
        SELECT
            (SELECT count(*)
             FROM lexicon.entry_editor_projection
             WHERE jsonb_path_exists(forms, '$.**.text_links'::jsonpath)
                OR jsonb_path_exists(meanings, '$.**.text_links'::jsonpath)
                OR jsonb_path_exists(forms, '$.**.audio_assets'::jsonpath)
                OR jsonb_path_exists(meanings, '$.**.audio_assets'::jsonpath))
          + (SELECT count(*)
             FROM lexicon.entry_publications
             WHERE content_schema_version = 3
               AND (jsonb_path_exists(snapshot, '$.**.text_links'::jsonpath)
                 OR jsonb_path_exists(snapshot, '$.**.audio_assets'::jsonpath)))
        "#,
    )
    .fetch_one(&mut *transaction)
    .await
    .context("failed to inspect persisted V3 payload compatibility")?;
    ensure!(
        incompatible_payloads == 0,
        "persisted V3 payloads are not readable by the rollback release"
    );

    migrator
        .undo(&mut *transaction, target_version)
        .await
        .context("failed to undo deployment migrations")?;

    let current_version: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success IS TRUE",
    )
    .fetch_one(&mut *transaction)
    .await
    .context("failed to verify migration rollback")?;
    ensure!(
        current_version == target_version,
        "migration rollback did not reach the requested target"
    );
    transaction.commit().await?;

    Ok(UndoReport {
        previous_version,
        target_version,
        current_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const PREVIOUS_RELEASE_VERSION: i64 = 20260906180000;
    const CURRENT_RELEASE_VERSION: i64 = 20260909150000;

    #[sqlx::test]
    async fn deployment_undo_reaches_the_previous_ledger_version(pool: PgPool) {
        let report = undo(&pool, PREVIOUS_RELEASE_VERSION, CURRENT_RELEASE_VERSION)
            .await
            .unwrap();

        assert_eq!(report.previous_version, CURRENT_RELEASE_VERSION);
        assert_eq!(report.target_version, PREVIOUS_RELEASE_VERSION);
        assert_eq!(report.current_version, PREVIOUS_RELEASE_VERSION);
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM _sqlx_migrations WHERE version > $1 AND success IS TRUE",
        )
        .bind(PREVIOUS_RELEASE_VERSION)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
    }

    #[sqlx::test]
    async fn translation_slot_migration_restores_active_and_retired_node_identity(pool: PgPool) {
        sqlx::raw_sql(include_str!(
            "../migrations/20260907180000_allow_multiple_sentence_translations.down.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();

        let admin_id = Uuid::now_v7();
        let entry_id = Uuid::now_v7();
        let sentence_id = Uuid::now_v7();
        let active_id = Uuid::now_v7();
        let retired_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'rollback test')",
        )
        .bind(admin_id)
        .bind(format!("rollback-{}", admin_id.simple()))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision,
                headword_mode, detection_snapshot,
                created_by_admin_id, updated_by_admin_id
            ) VALUES ($1, 3, 'en', 'word', 1, NULL, '{}', $2, $2)
            "#,
        )
        .bind(entry_id)
        .bind(admin_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.nodes (
                id, entry_id, node_type, parent_node_id, node_role, stable_slot
            ) VALUES
                ($1, $3, 'sentence', NULL, 'meanings.sentence', FALSE),
                ($2, $3, 'text_variant', $1,
                 'meanings.zh_translation_a1_a2:zh:common', TRUE),
                ($4, $3, 'text_variant', $1,
                 'meanings.zh_translation_b1_b2:zh:common', TRUE)
            "#,
        )
        .bind(sentence_id)
        .bind(active_id)
        .bind(entry_id)
        .bind(retired_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.text_variants (
                id, entry_id, owner_node_id, field_role, language, dialect,
                rich_text_version, content, plain_text, content_hash, origin, sort_order
            ) VALUES (
                $1, $2, $3, 'zh_translation_a1_a2', 'zh', 'common',
                2, '{"version":2,"text":"译文","annotations":[]}', '译文',
                decode('00', 'hex'), 'manual', 0
            )
            "#,
        )
        .bind(active_id)
        .bind(entry_id)
        .bind(sentence_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE lexicon.nodes SET removed_from_draft_at = now() WHERE id = $1")
            .bind(retired_id)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::raw_sql(include_str!(
            "../migrations/20260907180000_allow_multiple_sentence_translations.up.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let migrated: Vec<(Uuid, String, bool)> = sqlx::query_as(
            "SELECT id, node_role, stable_slot FROM lexicon.nodes WHERE id = ANY($1) ORDER BY id",
        )
        .bind([active_id, retired_id])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(migrated.len(), 2);
        assert!(
            migrated
                .iter()
                .all(|(_, role, stable)| role == "meanings.zh_translation" && !stable)
        );

        sqlx::query(
            "UPDATE lexicon.text_variants SET field_role = 'zh_translation_b1_b2' WHERE id = $1",
        )
        .bind(active_id)
        .execute(&pool)
        .await
        .unwrap();
        let rebanded = sqlx::raw_sql(include_str!(
            "../migrations/20260907180000_allow_multiple_sentence_translations.down.sql"
        ))
        .execute(&pool)
        .await;
        assert!(rebanded.is_err());
        sqlx::query(
            "UPDATE lexicon.text_variants SET field_role = 'zh_translation_a1_a2' WHERE id = $1",
        )
        .bind(active_id)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(include_str!(
            "../migrations/20260907180000_allow_multiple_sentence_translations.down.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let restored: Vec<(Uuid, String, bool)> = sqlx::query_as(
            "SELECT id, node_role, stable_slot FROM lexicon.nodes WHERE id = ANY($1) ORDER BY id",
        )
        .bind([active_id, retired_id])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(restored.contains(&(
            active_id,
            "meanings.zh_translation_a1_a2:zh:common".to_owned(),
            true
        )));
        assert!(restored.contains(&(
            retired_id,
            "meanings.zh_translation_b1_b2:zh:common".to_owned(),
            true
        )));
    }

    #[sqlx::test]
    async fn translation_slot_rollback_rejects_nodes_created_after_migration(pool: PgPool) {
        let admin_id = Uuid::now_v7();
        let entry_id = Uuid::now_v7();
        let new_node_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'rollback guard')",
        )
        .bind(admin_id)
        .bind(format!("rollback-guard-{}", admin_id.simple()))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision,
                headword_mode, detection_snapshot,
                created_by_admin_id, updated_by_admin_id
            ) VALUES ($1, 3, 'en', 'word', 1, NULL, '{}', $2, $2)
            "#,
        )
        .bind(entry_id)
        .bind(admin_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.nodes (
                id, entry_id, node_type, parent_node_id, node_role, stable_slot
            ) VALUES ($1, $2, 'text_variant', NULL, 'meanings.zh_translation', FALSE)
            "#,
        )
        .bind(new_node_id)
        .bind(entry_id)
        .execute(&pool)
        .await
        .unwrap();

        let error = undo(&pool, PREVIOUS_RELEASE_VERSION, CURRENT_RELEASE_VERSION)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to undo deployment migrations")
        );
        let latest: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success IS TRUE",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(latest, CURRENT_RELEASE_VERSION);
        let audio_reference_table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('lexicon.v3_audio_asset_references')::text")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            audio_reference_table.as_deref(),
            Some("lexicon.v3_audio_asset_references")
        );
    }

    #[sqlx::test]
    async fn deployment_undo_is_atomic_when_an_older_down_guard_fails(pool: PgPool) {
        let admin_id = Uuid::now_v7();
        let asset_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'audio rollback guard')",
        )
        .bind(admin_id)
        .bind(format!("audio-rollback-{}", admin_id.simple()))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.audio_assets (
                id, object_key, source_key, content_type, size_bytes,
                locale, gender, original_name, created_by_admin_id
            ) VALUES ($1, $2, $3, 'audio/mpeg', 1, 'en-GB', 'female', 'guard.mp3', $4)
            "#,
        )
        .bind(asset_id)
        .bind(format!("audio/{asset_id}"))
        .bind(format!("tmp/{asset_id}"))
        .bind(admin_id)
        .execute(&pool)
        .await
        .unwrap();

        let error = undo(&pool, PREVIOUS_RELEASE_VERSION, CURRENT_RELEASE_VERSION)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to undo deployment migrations")
        );
        let latest: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success IS TRUE",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(latest, CURRENT_RELEASE_VERSION);
        let asset_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM lexicon.audio_assets WHERE id = $1")
                .bind(asset_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(asset_count, 1);
        let translation_rollback_table: Option<String> = sqlx::query_scalar(
            "SELECT to_regclass('lexicon.sentence_translation_slot_rollback_v20260907180000')::text",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            translation_rollback_table.as_deref(),
            Some("lexicon.sentence_translation_slot_rollback_v20260907180000")
        );
    }

    #[sqlx::test]
    async fn deployment_undo_rejects_new_keys_in_persisted_v3_payloads(pool: PgPool) {
        let admin_id = Uuid::now_v7();
        let entry_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'payload rollback guard')",
        )
        .bind(admin_id)
        .bind(format!("payload-rollback-{}", admin_id.simple()))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entries (
                id, content_schema_version, language, kind, revision,
                headword_mode, detection_snapshot,
                created_by_admin_id, updated_by_admin_id
            ) VALUES ($1, 3, 'en', 'word', 1, NULL, '{}', $2, $2)
            "#,
        )
        .bind(entry_id)
        .bind(admin_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_editor_projection (
                entry_id, forms, meanings, rebuilt_revision
            ) VALUES ($1, '{}', '{"pos":[{"text_links":[]}]}', 1)
            "#,
        )
        .bind(entry_id)
        .execute(&pool)
        .await
        .unwrap();

        let error = undo(&pool, PREVIOUS_RELEASE_VERSION, CURRENT_RELEASE_VERSION)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("persisted V3 payloads are not readable")
        );
        let latest: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success IS TRUE",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(latest, CURRENT_RELEASE_VERSION);
    }
}
