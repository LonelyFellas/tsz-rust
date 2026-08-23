//! 智能词库聚合、稳定节点与发布引用的数据库底线测试。

use sqlx::PgPool;
use uuid::Uuid;

const UNIQUE_VIOLATION: &str = "23505";
const FOREIGN_KEY_VIOLATION: &str = "23503";
const CHECK_VIOLATION: &str = "23514";

fn assert_db_error<T: std::fmt::Debug>(
    result: Result<T, sqlx::Error>,
    expected_code: &str,
    expected_constraint: &str,
) {
    match result {
        Err(sqlx::Error::Database(error)) => {
            assert_eq!(error.code().as_deref(), Some(expected_code));
            assert_eq!(error.constraint(), Some(expected_constraint));
        }
        other => panic!("应返回约束错误，实际为 {other:?}"),
    }
}

async fn insert_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', '词库测试管理员')",
    )
    .bind(id)
    .bind(format!("lexicon-schema-{}", id.simple()))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn insert_entry(pool: &PgPool, admin_id: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, language, kind, revision, headword_mode, detection_snapshot,
            created_by_admin_id, updated_by_admin_id
        ) VALUES ($1, 'en', 'word', 1, 'unified', '{}', $2, $2)
        "#,
    )
    .bind(id)
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn part_id(pool: &PgPool, code: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn insert_pos_node(pool: &PgPool, entry_id: Uuid, part_id: Uuid) -> Uuid {
    let node_id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, 'pos')")
        .bind(node_id)
        .bind(entry_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_pos (
            id, entry_id, part_of_speech_id, spelling_mode, phonetic_mode, sort_order
        ) VALUES ($1, $2, $3, 'unified', 'unified', 0)
        "#,
    )
    .bind(node_id)
    .bind(entry_id)
    .bind(part_id)
    .execute(pool)
    .await
    .unwrap();
    node_id
}

async fn insert_node(pool: &PgPool, entry_id: Uuid, node_type: &str) -> Uuid {
    let node_id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, $3)")
        .bind(node_id)
        .bind(entry_id)
        .bind(node_type)
        .execute(pool)
        .await
        .unwrap();
    node_id
}

async fn insert_publication(
    pool: &PgPool,
    admin_id: Uuid,
    entry_id: Uuid,
    publication_number: i32,
) -> Uuid {
    let publication_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision, content_schema_version,
            snapshot, snapshot_hash, published_by_admin_id
        ) VALUES ($1, $2, $3, $3, 2, '{}', $4, $5)
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(publication_number)
    .bind(publication_id.as_bytes().to_vec())
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    publication_id
}

async fn insert_publication_node(
    pool: &PgPool,
    publication_id: Uuid,
    entry_id: Uuid,
    node_id: Uuid,
    node_type: &str,
) {
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_nodes (
            publication_id, entry_id, node_id, node_type
        ) VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(node_id)
    .bind(node_type)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn headword_keys_are_unique_per_language_kind_and_dialect_scope(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let first = insert_entry(&pool, admin_id).await;
    let second = insert_entry(&pool, admin_id).await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_headword_keys (
            entry_id, language, kind, dialect_scope, normalized_headword, normalization_version
        ) VALUES ($1, 'en', 'word', 'uk', 'colour', 1)
        "#,
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();

    let duplicate = sqlx::query(
        r#"
        INSERT INTO lexicon.entry_headword_keys (
            entry_id, language, kind, dialect_scope, normalized_headword, normalization_version
        ) VALUES ($1, 'en', 'word', 'uk', 'colour', 1)
        "#,
    )
    .bind(second)
    .execute(&pool)
    .await;
    assert_db_error(
        duplicate,
        UNIQUE_VIOLATION,
        "lexicon_entry_headword_keys_unique_idx",
    );

    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_headword_keys (
            entry_id, language, kind, dialect_scope, normalized_headword, normalization_version
        ) VALUES ($1, 'en', 'word', 'us', 'colour', 1)
        "#,
    )
    .bind(second)
    .execute(&pool)
    .await
    .expect("同一规范词头在另一方言 scope 中可独立存在");
}

#[sqlx::test]
async fn relational_nodes_cannot_cross_entry_boundaries(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let first = insert_entry(&pool, admin_id).await;
    let second = insert_entry(&pool, admin_id).await;
    let noun = part_id(&pool, "noun").await;
    let node_id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, 'pos')")
        .bind(node_id)
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();

    let crossed = sqlx::query(
        r#"
        INSERT INTO lexicon.entry_pos (
            id, entry_id, part_of_speech_id, spelling_mode, phonetic_mode, sort_order
        ) VALUES ($1, $2, $3, 'unified', 'unified', 0)
        "#,
    )
    .bind(node_id)
    .bind(second)
    .bind(noun)
    .execute(&pool)
    .await;
    assert_db_error(
        crossed,
        FOREIGN_KEY_VIOLATION,
        "lexicon_entry_pos_node_fkey",
    );
}

#[sqlx::test]
async fn node_registry_rejects_unknown_node_types(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_entry(&pool, admin_id).await;
    let invalid = sqlx::query(
        "INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, 'mystery')",
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .execute(&pool)
    .await;
    assert_db_error(invalid, CHECK_VIOLATION, "lexicon_nodes_type_check");
}

#[sqlx::test]
async fn node_parent_and_stable_slot_bindings_are_database_enforced(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let first = insert_entry(&pool, admin_id).await;
    let second = insert_entry(&pool, admin_id).await;
    let parent_id = insert_node(&pool, first, "sentence").await;
    let child_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        ) VALUES ($1, $2, 'text_variant', $3, 'meanings.zh_text:zh:common', TRUE)
        "#,
    )
    .bind(child_id)
    .bind(first)
    .bind(parent_id)
    .execute(&pool)
    .await
    .expect("同词条父子槽位应可登记");

    let replaced_slot = sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        ) VALUES ($1, $2, 'text_variant', $3, 'meanings.zh_text:zh:common', TRUE)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(first)
    .bind(parent_id)
    .execute(&pool)
    .await;
    assert_db_error(
        replaced_slot,
        UNIQUE_VIOLATION,
        "lexicon_nodes_stable_slot_key",
    );

    let crossed = sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role
        ) VALUES ($1, $2, 'definition', $3, 'meanings.definition:zh:definition')
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(second)
    .bind(parent_id)
    .execute(&pool)
    .await;
    assert_db_error(crossed, FOREIGN_KEY_VIOLATION, "lexicon_nodes_parent_fkey");

    sqlx::query("DELETE FROM lexicon.entries WHERE id = $1")
        .bind(first)
        .execute(&pool)
        .await
        .expect("删除词条应级联删除 registry 节点树");
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.nodes WHERE id = ANY($1)")
            .bind([parent_id, child_id])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, 0);
}

#[sqlx::test]
async fn publication_references_preserve_catalog_and_entry_integrity(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_entry(&pool, admin_id).await;
    let other_entry_id = insert_entry(&pool, admin_id).await;
    let noun = part_id(&pool, "noun").await;
    let pos_id = insert_pos_node(&pool, entry_id, noun).await;
    let publication_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision, content_schema_version,
            snapshot, snapshot_hash, published_by_admin_id
        ) VALUES ($1, $2, 1, 1, 2, '{}', decode('01', 'hex'), $3)
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(admin_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_nodes (
            publication_id, entry_id, node_id, node_type
        ) VALUES ($1, $2, $3, 'pos')
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(pos_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_part_of_speech_refs (
            publication_id, entry_id, source_node_id, part_of_speech_id
        ) VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(pos_id)
    .bind(noun)
    .execute(&pool)
    .await
    .unwrap();

    let cross_entry_pointer =
        sqlx::query("UPDATE lexicon.entries SET current_publication_id = $1 WHERE id = $2")
            .bind(publication_id)
            .bind(other_entry_id)
            .execute(&pool)
            .await;
    assert_db_error(
        cross_entry_pointer,
        FOREIGN_KEY_VIOLATION,
        "lexicon_entries_current_publication_fkey",
    );

    sqlx::query("DELETE FROM lexicon.entry_pos WHERE id = $1")
        .bind(pos_id)
        .execute(&pool)
        .await
        .unwrap();
    let delete_catalog = sqlx::query("DELETE FROM catalog.parts_of_speech WHERE id = $1")
        .bind(noun)
        .execute(&pool)
        .await;
    assert_db_error(
        delete_catalog,
        FOREIGN_KEY_VIOLATION,
        "lexicon_publication_pos_refs_catalog_fkey",
    );
}

#[sqlx::test]
async fn publication_relation_can_anchor_a_never_published_target_sense(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let source_entry_id = insert_entry(&pool, admin_id).await;
    let target_entry_id = insert_entry(&pool, admin_id).await;
    let source_relation_id = insert_node(&pool, source_entry_id, "relation").await;
    let target_sense_id = insert_node(&pool, target_entry_id, "sense").await;
    let source_publication_id = insert_publication(&pool, admin_id, source_entry_id, 1).await;
    insert_publication_node(
        &pool,
        source_publication_id,
        source_entry_id,
        source_relation_id,
        "relation",
    )
    .await;

    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_sense_refs (
            publication_id, entry_id, source_node_id, reference_kind,
            target_entry_id, target_sense_id, target_publication_id,
            target_content_scope, target_revision
        ) VALUES ($1, $2, $3, 'relation', $4, $5, NULL, 'draft', 1)
        "#,
    )
    .bind(source_publication_id)
    .bind(source_entry_id)
    .bind(source_relation_id)
    .bind(target_entry_id)
    .bind(target_sense_id)
    .execute(&pool)
    .await
    .expect("relation publication reference 应允许锚定从未发布的目标 sense");

    let delete_target = sqlx::query("DELETE FROM lexicon.nodes WHERE id = $1")
        .bind(target_sense_id)
        .execute(&pool)
        .await;
    assert_db_error(
        delete_target,
        FOREIGN_KEY_VIOLATION,
        "lexicon_publication_sense_refs_target_node_fkey",
    );

    let source_sentence_id = insert_node(&pool, source_entry_id, "sentence").await;
    insert_publication_node(
        &pool,
        source_publication_id,
        source_entry_id,
        source_sentence_id,
        "sentence",
    )
    .await;
    let draft_context = sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_sense_refs (
            publication_id, entry_id, source_node_id, reference_kind,
            target_entry_id, target_sense_id, target_publication_id,
            target_content_scope, target_revision
        ) VALUES ($1, $2, $3, 'sentence_context', $4, $5, NULL, 'draft', 1)
        "#,
    )
    .bind(source_publication_id)
    .bind(source_entry_id)
    .bind(source_sentence_id)
    .bind(target_entry_id)
    .bind(target_sense_id)
    .execute(&pool)
    .await;
    assert_db_error(
        draft_context,
        CHECK_VIOLATION,
        "lexicon_publication_sense_refs_context_target_check",
    );
}

#[sqlx::test]
async fn publication_sense_refs_bind_source_and_target_publication_membership(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let source_entry_id = insert_entry(&pool, admin_id).await;
    let target_entry_id = insert_entry(&pool, admin_id).await;
    let source_relation_id = insert_node(&pool, source_entry_id, "relation").await;
    let source_relation_without_membership = insert_node(&pool, source_entry_id, "relation").await;
    let target_sense_id = insert_node(&pool, target_entry_id, "sense").await;
    let source_publication_id = insert_publication(&pool, admin_id, source_entry_id, 1).await;
    let target_publication_id = insert_publication(&pool, admin_id, target_entry_id, 1).await;
    let target_publication_without_sense =
        insert_publication(&pool, admin_id, target_entry_id, 2).await;
    insert_publication_node(
        &pool,
        source_publication_id,
        source_entry_id,
        source_relation_id,
        "relation",
    )
    .await;
    insert_publication_node(
        &pool,
        target_publication_id,
        target_entry_id,
        target_sense_id,
        "sense",
    )
    .await;

    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_sense_refs (
            publication_id, entry_id, source_node_id, reference_kind,
            target_entry_id, target_sense_id, target_publication_id,
            target_content_scope, target_revision
        ) VALUES ($1, $2, $3, 'relation', $4, $5, $6, 'publication', 1)
        "#,
    )
    .bind(source_publication_id)
    .bind(source_entry_id)
    .bind(source_relation_id)
    .bind(target_entry_id)
    .bind(target_sense_id)
    .bind(target_publication_id)
    .execute(&pool)
    .await
    .unwrap();

    let source_not_in_publication = sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_sense_refs (
            publication_id, entry_id, source_node_id, reference_kind,
            target_entry_id, target_sense_id, target_publication_id,
            target_content_scope, target_revision
        ) VALUES ($1, $2, $3, 'relation', $4, $5, $6, 'publication', 1)
        "#,
    )
    .bind(source_publication_id)
    .bind(source_entry_id)
    .bind(source_relation_without_membership)
    .bind(target_entry_id)
    .bind(target_sense_id)
    .bind(target_publication_id)
    .execute(&pool)
    .await;
    assert_db_error(
        source_not_in_publication,
        FOREIGN_KEY_VIOLATION,
        "lexicon_publication_sense_refs_source_fkey",
    );

    let second_source_relation_id = insert_node(&pool, source_entry_id, "relation").await;
    insert_publication_node(
        &pool,
        source_publication_id,
        source_entry_id,
        second_source_relation_id,
        "relation",
    )
    .await;
    let mismatched_target_revision = sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_sense_refs (
            publication_id, entry_id, source_node_id, reference_kind,
            target_entry_id, target_sense_id, target_publication_id,
            target_content_scope, target_revision
        ) VALUES ($1, $2, $3, 'relation', $4, $5, $6, 'publication', 2)
        "#,
    )
    .bind(source_publication_id)
    .bind(source_entry_id)
    .bind(second_source_relation_id)
    .bind(target_entry_id)
    .bind(target_sense_id)
    .bind(target_publication_id)
    .execute(&pool)
    .await;
    assert_db_error(
        mismatched_target_revision,
        FOREIGN_KEY_VIOLATION,
        "lexicon_publication_sense_refs_target_revision_fkey",
    );
    let target_sense_not_in_publication = sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publication_sense_refs (
            publication_id, entry_id, source_node_id, reference_kind,
            target_entry_id, target_sense_id, target_publication_id,
            target_content_scope, target_revision
        ) VALUES ($1, $2, $3, 'relation', $4, $5, $6, 'publication', 2)
        "#,
    )
    .bind(source_publication_id)
    .bind(source_entry_id)
    .bind(second_source_relation_id)
    .bind(target_entry_id)
    .bind(target_sense_id)
    .bind(target_publication_without_sense)
    .execute(&pool)
    .await;
    assert_db_error(
        target_sense_not_in_publication,
        FOREIGN_KEY_VIOLATION,
        "lexicon_publication_sense_refs_target_fkey",
    );

    let delete_target_publication =
        sqlx::query("DELETE FROM lexicon.entry_publications WHERE id = $1")
            .bind(target_publication_id)
            .execute(&pool)
            .await;
    assert_db_error(
        delete_target_publication,
        FOREIGN_KEY_VIOLATION,
        "lexicon_publication_sense_refs_target_revision_fkey",
    );

    sqlx::query("DELETE FROM lexicon.entry_publications WHERE id = $1")
        .bind(source_publication_id)
        .execute(&pool)
        .await
        .expect("删除来源 publication 应级联删除其 sense refs");
    let remaining_refs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.entry_publication_sense_refs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining_refs, 0);
}

#[sqlx::test]
async fn outbox_event_identity_is_unique(pool: PgPool) {
    let aggregate_id = Uuid::now_v7();
    for index in 0..2 {
        let result = sqlx::query(
            r#"
            INSERT INTO platform.outbox_events (
                id, aggregate_type, aggregate_id, aggregate_revision, event_type, payload
            ) VALUES ($1, 'lexicon.entry', $2, 1, 'lexicon.entry_published', '{}')
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(aggregate_id)
        .execute(&pool)
        .await;
        if index == 0 {
            result.unwrap();
        } else {
            assert_db_error(result, UNIQUE_VIOLATION, "platform_outbox_event_key");
        }
    }
}
