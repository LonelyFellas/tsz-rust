//! Smart Lexicon V3 expand-only storage constraints.
//!
//! These tests deliberately exercise PostgreSQL constraints directly. Service-level
//! completeness and authorization remain separate concerns.

use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
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
        other => panic!("expected database constraint error, got {other:?}"),
    }
}

async fn insert_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'V3 schema test')",
    )
    .bind(id)
    .bind(format!("v3-schema-{}", id.simple()))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn insert_entry(
    pool: &PgPool,
    admin_id: Uuid,
    schema_version: i16,
    kind: &str,
    headword_mode: Option<&str>,
    source_dialect: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, content_schema_version, language, kind, revision,
            headword_mode, source_dialect, detection_snapshot,
            created_by_admin_id, updated_by_admin_id
        ) VALUES ($1, $2, 'en', $3, 1, $4, $5, '{}', $6, $6)
        "#,
    )
    .bind(id)
    .bind(schema_version)
    .bind(kind)
    .bind(headword_mode)
    .bind(source_dialect)
    .bind(admin_id)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn catalog_pos_id(pool: &PgPool, code: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn insert_node(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    entry_id: Uuid,
    node_type: &str,
    parent_node_id: Option<Uuid>,
    node_role: &str,
    stable_slot: bool,
) {
    sqlx::query(
        r#"
        INSERT INTO lexicon.nodes (
            id, entry_id, node_type, parent_node_id, node_role, stable_slot
        ) VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(node_type)
    .bind(parent_node_id)
    .bind(node_role)
    .bind(stable_slot)
    .execute(&mut **tx)
    .await
    .unwrap();
}

async fn insert_v3_entry(pool: &PgPool, admin_id: Uuid) -> Uuid {
    let entry_id = insert_entry(pool, admin_id, 3, "word", None, None)
        .await
        .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_entry_state (
            entry_id, content_schema_version, origin
        ) VALUES ($1, 3, 'native')
        "#,
    )
    .bind(entry_id)
    .execute(pool)
    .await
    .unwrap();
    entry_id
}

async fn insert_v3_pos(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    part_of_speech_id: Uuid,
    ordinal: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    insert_node(tx, id, entry_id, "pos", None, "forms.pos", false).await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_pos (
            id, entry_id, part_of_speech_id, content_schema_version,
            spelling_mode, phonetic_mode, sort_order
        ) VALUES ($1, $2, $3, 3, 'unified', 'unified', $4)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(part_of_speech_id)
    .bind(ordinal)
    .execute(&mut **tx)
    .await
    .unwrap();
    id
}

async fn insert_v3_group(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    pos_id: Uuid,
    ordinal: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    insert_node(
        tx,
        id,
        entry_id,
        "form_group",
        Some(pos_id),
        "forms.form_group",
        false,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_form_groups (
            id, entry_id, entry_pos_id, is_regular, ordinal
        ) VALUES ($1, $2, $3, TRUE, $4)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(pos_id)
    .bind(ordinal)
    .execute(&mut **tx)
    .await
    .unwrap();
    id
}

#[sqlx::test]
async fn v3_dialect_rules_migration_backfills_in_order_and_rolls_back(pool: PgPool) {
    sqlx::raw_sql(include_str!(
        "../migrations/20260827100000_add_lexicon_v3_dialect_rules.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_v3_entry(&pool, admin_id).await;
    let pos_specs = [
        (Uuid::now_v7(), "noun", "common", "colour", "colour"),
        (Uuid::now_v7(), "verb", "uk_us", "learned", "learned"),
        (
            Uuid::now_v7(),
            "adjective",
            "uk_us",
            "colourful",
            "colorful",
        ),
    ];
    let mut tx = pool.begin().await.unwrap();
    for (ordinal, (pos_id, code, ..)) in pos_specs.iter().enumerate() {
        insert_node(&mut tx, *pos_id, entry_id, "pos", None, "forms.pos", false).await;
        let catalog_id = catalog_pos_id(&pool, code).await;
        sqlx::query(
            r#"
            INSERT INTO lexicon.entry_pos (
                id, entry_id, part_of_speech_id, content_schema_version,
                spelling_mode, phonetic_mode, sort_order
            ) VALUES ($1, $2, $3, 3, NULL, NULL, $4)
            "#,
        )
        .bind(pos_id)
        .bind(entry_id)
        .bind(catalog_id)
        .bind(ordinal as i32)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let pos_json = pos_specs
        .iter()
        .map(|(pos_id, code, mode, uk, us)| {
            let regional_variants = if *mode == "common" {
                json!({
                    "mode": "common",
                    "common": {
                        "id": Uuid::now_v7(),
                        "dialect": "common",
                        "spelling": uk,
                        "origin": "manual",
                        "pronunciations": []
                    }
                })
            } else {
                json!({
                    "mode": "uk_us",
                    "uk": {
                        "id": Uuid::now_v7(),
                        "dialect": "uk",
                        "spelling": uk,
                        "origin": "manual",
                        "pronunciations": []
                    },
                    "us": {
                        "id": Uuid::now_v7(),
                        "dialect": "us",
                        "spelling": us,
                        "origin": "manual",
                        "pronunciations": []
                    }
                })
            };
            json!({
                "pos_id": pos_id,
                "pos": code,
                "forms": [{
                    "id": Uuid::now_v7(),
                    "form_type": "base",
                    "regional_variants": regional_variants
                }],
                "form_groups": []
            })
        })
        .collect::<Vec<_>>();
    let original_pos_ids = pos_json
        .iter()
        .map(|pos| pos["pos_id"].clone())
        .collect::<Vec<_>>();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_editor_projection (
            entry_id, forms, meanings, rebuilt_revision
        ) VALUES ($1, $2, '{}', 1)
        "#,
    )
    .bind(entry_id)
    .bind(json!({"pos": pos_json}))
    .execute(&pool)
    .await
    .unwrap();

    sqlx::raw_sql(include_str!(
        "../migrations/20260827100000_add_lexicon_v3_dialect_rules.up.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();

    let forms: serde_json::Value =
        sqlx::query_scalar("SELECT forms FROM lexicon.entry_editor_projection WHERE entry_id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        forms["pos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pos| pos["pos_id"].clone())
            .collect::<Vec<_>>(),
        original_pos_ids
    );
    assert_eq!(
        forms["pos"][0]["dialect_rules"],
        json!({"spelling_mode": "unified", "phonetic_mode": "unified"})
    );
    assert_eq!(
        forms["pos"][1]["dialect_rules"],
        json!({"spelling_mode": "unified", "phonetic_mode": "distinguish"})
    );
    assert_eq!(
        forms["pos"][2]["dialect_rules"],
        json!({"spelling_mode": "distinguish", "phonetic_mode": "distinguish"})
    );
    let modes: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT spelling_mode, phonetic_mode
        FROM lexicon.entry_pos
        WHERE entry_id = $1
        ORDER BY sort_order
        "#,
    )
    .bind(entry_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        modes,
        [
            ("unified".to_owned(), "unified".to_owned()),
            ("unified".to_owned(), "distinguish".to_owned()),
            ("distinguish".to_owned(), "distinguish".to_owned()),
        ]
    );
    let invalid_du = sqlx::query(
        "UPDATE lexicon.entry_pos SET spelling_mode = 'distinguish', phonetic_mode = 'unified' WHERE id = $1",
    )
    .bind(pos_specs[0].0)
    .execute(&pool)
    .await;
    assert_db_error(
        invalid_du,
        CHECK_VIOLATION,
        "lexicon_entry_pos_versioned_modes_check",
    );
    let missing_rules =
        sqlx::query("UPDATE lexicon.entry_pos SET spelling_mode = NULL WHERE id = $1")
            .bind(pos_specs[0].0)
            .execute(&pool)
            .await;
    assert_db_error(
        missing_rules,
        CHECK_VIOLATION,
        "lexicon_entry_pos_versioned_modes_check",
    );

    sqlx::raw_sql(include_str!(
        "../migrations/20260827100000_add_lexicon_v3_dialect_rules.down.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let rolled_back: serde_json::Value =
        sqlx::query_scalar("SELECT forms FROM lexicon.entry_editor_projection WHERE entry_id = $1")
            .bind(entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        rolled_back["pos"]
            .as_array()
            .unwrap()
            .iter()
            .all(|pos| pos.get("dialect_rules").is_none())
    );
    let null_modes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entry_pos WHERE entry_id = $1 AND spelling_mode IS NULL AND phonetic_mode IS NULL",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(null_modes, 3);
}

async fn insert_v3_form(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    pos_id: Uuid,
    form_type: &str,
    ordinal: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    insert_node(
        tx,
        id,
        entry_id,
        "concrete_form",
        Some(pos_id),
        "forms.concrete_form",
        false,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_concrete_forms (
            id, entry_id, entry_pos_id, form_type, ordinal
        ) VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(pos_id)
    .bind(form_type)
    .bind(ordinal)
    .execute(&mut **tx)
    .await
    .unwrap();
    id
}

async fn insert_v3_membership(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    pos_id: Uuid,
    group_id: Uuid,
    form_id: Uuid,
    ordinal: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    insert_node(
        tx,
        id,
        entry_id,
        "group_membership",
        Some(group_id),
        "forms.group_membership",
        false,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_group_memberships (
            id, entry_id, entry_pos_id, form_group_id, form_id, ordinal
        ) VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(pos_id)
    .bind(group_id)
    .bind(form_id)
    .bind(ordinal)
    .execute(&mut **tx)
    .await
    .unwrap();
    id
}

async fn insert_v3_variant(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    form_id: Uuid,
    dialect: &str,
    spelling: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    insert_node(
        tx,
        id,
        entry_id,
        "form_variant",
        Some(form_id),
        &format!("forms.form_variant:{dialect}"),
        true,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_form_variants (
            id, entry_id, form_id, dialect, spelling,
            normalized_spelling, normalization_version, origin
        ) VALUES ($1, $2, $3, $4, $5, $5, 1, 'manual')
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(form_id)
    .bind(dialect)
    .bind(spelling)
    .execute(&mut **tx)
    .await
    .unwrap();
    id
}

async fn insert_valid_common_form(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    pos_id: Uuid,
    group_id: Uuid,
    form_type: &str,
    form_ordinal: i32,
    membership_ordinal: i32,
) -> (Uuid, Uuid, Uuid) {
    let form_id = insert_v3_form(tx, entry_id, pos_id, form_type, form_ordinal).await;
    let membership_id =
        insert_v3_membership(tx, entry_id, pos_id, group_id, form_id, membership_ordinal).await;
    let variant_id = insert_v3_variant(tx, entry_id, form_id, "common", "word").await;
    (form_id, membership_id, variant_id)
}

#[sqlx::test]
async fn entry_schema_version_controls_kind_and_legacy_headword_shape(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;

    let native_v3 = insert_entry(&pool, admin_id, 3, "word", None, None)
        .await
        .expect("native V3 word has no legacy headword");
    sqlx::query(
        "INSERT INTO lexicon.v3_entry_state (entry_id, origin, first_v3_write_revision) VALUES ($1, 'native', 1)",
    )
    .bind(native_v3)
    .execute(&pool)
    .await
    .unwrap();

    let migrated_v3 = insert_entry(&pool, admin_id, 3, "word", Some("unified"), None).await;
    assert!(
        migrated_v3.is_ok(),
        "migrated V3 may retain a read-only bridge"
    );

    assert_db_error(
        insert_entry(&pool, admin_id, 2, "word", None, None).await,
        CHECK_VIOLATION,
        "lexicon_entries_versioned_headword_shape_check",
    );
    assert_db_error(
        insert_entry(&pool, admin_id, 3, "phrase", None, None).await,
        CHECK_VIOLATION,
        "lexicon_entries_schema_kind_check",
    );
    assert_db_error(
        insert_entry(&pool, admin_id, 4, "word", None, None).await,
        CHECK_VIOLATION,
        "lexicon_entries_schema_version_check",
    );

    let invalid_revision_entry = insert_entry(&pool, admin_id, 3, "word", None, None)
        .await
        .unwrap();
    let invalid_first_write = sqlx::query(
        "INSERT INTO lexicon.v3_entry_state (entry_id, origin, first_v3_write_revision) VALUES ($1, 'native', 0)",
    )
    .bind(invalid_revision_entry)
    .execute(&pool)
    .await;
    assert_db_error(
        invalid_first_write,
        CHECK_VIOLATION,
        "lexicon_v3_entry_state_first_write_revision_check",
    );
}

#[sqlx::test]
async fn v2_form_slot_constraints_are_unchanged(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_entry(&pool, admin_id, 2, "word", Some("unified"), None)
        .await
        .unwrap();
    let noun_id = catalog_pos_id(&pool, "noun").await;
    let mut tx = pool.begin().await.unwrap();
    let pos_id = Uuid::now_v7();
    insert_node(&mut tx, pos_id, entry_id, "pos", None, "forms.pos", false).await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_pos (
            id, entry_id, part_of_speech_id, spelling_mode, phonetic_mode, sort_order
        ) VALUES ($1, $2, $3, 'unified', 'unified', 0)
        "#,
    )
    .bind(pos_id)
    .bind(entry_id)
    .bind(noun_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let group_id = Uuid::now_v7();
    insert_node(
        &mut tx,
        group_id,
        entry_id,
        "form_group",
        Some(pos_id),
        "forms.form_group",
        false,
    )
    .await;
    sqlx::query(
        "INSERT INTO lexicon.form_groups (id, entry_id, entry_pos_id, is_regular, sort_order) VALUES ($1, $2, $3, TRUE, 0)",
    )
    .bind(group_id)
    .bind(entry_id)
    .bind(pos_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let base_id = Uuid::now_v7();
    insert_node(
        &mut tx,
        base_id,
        entry_id,
        "form_slot",
        Some(pos_id),
        "forms.base_form",
        false,
    )
    .await;
    sqlx::query(
        "INSERT INTO lexicon.form_slots (id, entry_id, entry_pos_id, form_group_id, form_type, sort_order) VALUES ($1, $2, $3, NULL, 'base', 0)",
    )
    .bind(base_id)
    .bind(entry_id)
    .bind(pos_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut duplicate_tx = pool.begin().await.unwrap();
    let second_base_id = Uuid::now_v7();
    insert_node(
        &mut duplicate_tx,
        second_base_id,
        entry_id,
        "form_slot",
        Some(pos_id),
        "forms.base_form:duplicate-test",
        false,
    )
    .await;
    let duplicate = sqlx::query(
        "INSERT INTO lexicon.form_slots (id, entry_id, entry_pos_id, form_group_id, form_type, sort_order) VALUES ($1, $2, $3, NULL, 'base', 1)",
    )
    .bind(second_base_id)
    .bind(entry_id)
    .bind(pos_id)
    .execute(&mut *duplicate_tx)
    .await;
    assert_db_error(
        duplicate,
        UNIQUE_VIOLATION,
        "lexicon_form_slots_one_base_idx",
    );

    let mut shape_tx = pool.begin().await.unwrap();
    let invalid_slot_id = Uuid::now_v7();
    insert_node(
        &mut shape_tx,
        invalid_slot_id,
        entry_id,
        "form_slot",
        Some(pos_id),
        "forms.form_slot:plural",
        false,
    )
    .await;
    let invalid_shape = sqlx::query(
        "INSERT INTO lexicon.form_slots (id, entry_id, entry_pos_id, form_group_id, form_type, sort_order) VALUES ($1, $2, $3, NULL, 'plural', 1)",
    )
    .bind(invalid_slot_id)
    .bind(entry_id)
    .bind(pos_id)
    .execute(&mut *shape_tx)
    .await;
    assert_db_error(
        invalid_shape,
        CHECK_VIOLATION,
        "lexicon_form_slots_group_shape_check",
    );
}

#[sqlx::test]
async fn v3_allows_duplicate_form_types_multiple_bases_and_cross_group_membership(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_v3_entry(&pool, admin_id).await;
    let noun_id = catalog_pos_id(&pool, "noun").await;
    let mut tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut tx, entry_id, noun_id, 0).await;
    let first_group = insert_v3_group(&mut tx, entry_id, pos_id, 0).await;
    let second_group = insert_v3_group(&mut tx, entry_id, pos_id, 1).await;
    let (first_base, _, _) =
        insert_valid_common_form(&mut tx, entry_id, pos_id, first_group, "base", 0, 0).await;
    insert_v3_membership(&mut tx, entry_id, pos_id, second_group, first_base, 0).await;
    insert_valid_common_form(&mut tx, entry_id, pos_id, first_group, "base", 1, 1).await;
    insert_valid_common_form(&mut tx, entry_id, pos_id, first_group, "plural", 2, 2).await;
    insert_valid_common_form(&mut tx, entry_id, pos_id, first_group, "plural", 3, 3).await;
    tx.commit().await.unwrap();

    let base_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id = $1 AND form_type = 'base'",
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let first_base_memberships: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_group_memberships WHERE form_id = $1")
            .bind(first_base)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(base_count, 2);
    assert_eq!(first_base_memberships, 2);
}

#[sqlx::test]
async fn v3_membership_rejects_same_group_duplicate_and_cross_pos_reference(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_v3_entry(&pool, admin_id).await;
    let noun_id = catalog_pos_id(&pool, "noun").await;
    let verb_id = catalog_pos_id(&pool, "verb").await;
    let mut tx = pool.begin().await.unwrap();
    let noun_pos = insert_v3_pos(&mut tx, entry_id, noun_id, 0).await;
    let verb_pos = insert_v3_pos(&mut tx, entry_id, verb_id, 1).await;
    let noun_group = insert_v3_group(&mut tx, entry_id, noun_pos, 0).await;
    let verb_group = insert_v3_group(&mut tx, entry_id, verb_pos, 0).await;
    let (noun_form, _, _) =
        insert_valid_common_form(&mut tx, entry_id, noun_pos, noun_group, "base", 0, 0).await;
    let (verb_form, _, _) =
        insert_valid_common_form(&mut tx, entry_id, verb_pos, verb_group, "base", 0, 0).await;
    tx.commit().await.unwrap();

    let mut duplicate_tx = pool.begin().await.unwrap();
    let duplicate_id = Uuid::now_v7();
    insert_node(
        &mut duplicate_tx,
        duplicate_id,
        entry_id,
        "group_membership",
        Some(noun_group),
        "forms.group_membership",
        false,
    )
    .await;
    let duplicate = sqlx::query(
        "INSERT INTO lexicon.v3_group_memberships (id, entry_id, entry_pos_id, form_group_id, form_id, ordinal) VALUES ($1, $2, $3, $4, $5, 1)",
    )
    .bind(duplicate_id)
    .bind(entry_id)
    .bind(noun_pos)
    .bind(noun_group)
    .bind(noun_form)
    .execute(&mut *duplicate_tx)
    .await;
    assert_db_error(
        duplicate,
        UNIQUE_VIOLATION,
        "lexicon_v3_group_memberships_group_form_key",
    );

    let mut crossed_tx = pool.begin().await.unwrap();
    let crossed_id = Uuid::now_v7();
    insert_node(
        &mut crossed_tx,
        crossed_id,
        entry_id,
        "group_membership",
        Some(noun_group),
        "forms.group_membership",
        false,
    )
    .await;
    let crossed = sqlx::query(
        "INSERT INTO lexicon.v3_group_memberships (id, entry_id, entry_pos_id, form_group_id, form_id, ordinal) VALUES ($1, $2, $3, $4, $5, 1)",
    )
    .bind(crossed_id)
    .bind(entry_id)
    .bind(noun_pos)
    .bind(noun_group)
    .bind(verb_form)
    .execute(&mut *crossed_tx)
    .await;
    assert_db_error(
        crossed,
        FOREIGN_KEY_VIOLATION,
        "lexicon_v3_group_memberships_form_owner_fkey",
    );
}

#[sqlx::test]
async fn v3_orphan_constraint_is_deferred_and_allows_atomic_form_deletion(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_v3_entry(&pool, admin_id).await;
    let noun_id = catalog_pos_id(&pool, "noun").await;

    let mut orphan_tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut orphan_tx, entry_id, noun_id, 0).await;
    let form_id = insert_v3_form(&mut orphan_tx, entry_id, pos_id, "base", 0).await;
    insert_v3_variant(&mut orphan_tx, entry_id, form_id, "common", "orphan").await;
    assert_db_error(
        orphan_tx.commit().await,
        CHECK_VIOLATION,
        "lexicon_v3_concrete_forms_membership_required_check",
    );

    let mut valid_tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut valid_tx, entry_id, noun_id, 0).await;
    let group_id = insert_v3_group(&mut valid_tx, entry_id, pos_id, 0).await;
    let (form_id, membership_id, _) =
        insert_valid_common_form(&mut valid_tx, entry_id, pos_id, group_id, "base", 0, 0).await;
    valid_tx.commit().await.unwrap();

    let mut last_membership_tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM lexicon.v3_group_memberships WHERE id = $1")
        .bind(membership_id)
        .execute(&mut *last_membership_tx)
        .await
        .unwrap();
    assert_db_error(
        last_membership_tx.commit().await,
        CHECK_VIOLATION,
        "lexicon_v3_concrete_forms_membership_required_check",
    );

    let mut atomic_delete_tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM lexicon.v3_group_memberships WHERE id = $1")
        .bind(membership_id)
        .execute(&mut *atomic_delete_tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM lexicon.v3_concrete_forms WHERE id = $1")
        .bind(form_id)
        .execute(&mut *atomic_delete_tx)
        .await
        .unwrap();
    atomic_delete_tx.commit().await.unwrap();
}

#[sqlx::test]
async fn v3_regional_shape_is_deferred_and_can_switch_atomically(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_v3_entry(&pool, admin_id).await;
    let noun_id = catalog_pos_id(&pool, "noun").await;

    let mut mixed_tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut mixed_tx, entry_id, noun_id, 0).await;
    let group_id = insert_v3_group(&mut mixed_tx, entry_id, pos_id, 0).await;
    let form_id = insert_v3_form(&mut mixed_tx, entry_id, pos_id, "base", 0).await;
    insert_v3_membership(&mut mixed_tx, entry_id, pos_id, group_id, form_id, 0).await;
    insert_v3_variant(&mut mixed_tx, entry_id, form_id, "common", "colour").await;
    insert_v3_variant(&mut mixed_tx, entry_id, form_id, "uk", "colour").await;
    assert_db_error(
        mixed_tx.commit().await,
        CHECK_VIOLATION,
        "lexicon_v3_form_variants_regional_shape_check",
    );

    let mut incomplete_tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut incomplete_tx, entry_id, noun_id, 0).await;
    let group_id = insert_v3_group(&mut incomplete_tx, entry_id, pos_id, 0).await;
    let form_id = insert_v3_form(&mut incomplete_tx, entry_id, pos_id, "base", 0).await;
    insert_v3_membership(&mut incomplete_tx, entry_id, pos_id, group_id, form_id, 0).await;
    insert_v3_variant(&mut incomplete_tx, entry_id, form_id, "uk", "colour").await;
    assert_db_error(
        incomplete_tx.commit().await,
        CHECK_VIOLATION,
        "lexicon_v3_form_variants_regional_shape_check",
    );

    let mut common_tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut common_tx, entry_id, noun_id, 0).await;
    let group_id = insert_v3_group(&mut common_tx, entry_id, pos_id, 0).await;
    let (form_id, _, common_variant_id) =
        insert_valid_common_form(&mut common_tx, entry_id, pos_id, group_id, "base", 0, 0).await;
    common_tx.commit().await.unwrap();

    let mut switch_tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM lexicon.v3_form_variants WHERE id = $1")
        .bind(common_variant_id)
        .execute(&mut *switch_tx)
        .await
        .unwrap();
    insert_v3_variant(&mut switch_tx, entry_id, form_id, "uk", "colour").await;
    insert_v3_variant(&mut switch_tx, entry_id, form_id, "us", "color").await;
    switch_tx.commit().await.unwrap();

    let dialects: Vec<String> = sqlx::query_scalar(
        "SELECT dialect FROM lexicon.v3_form_variants WHERE form_id = $1 ORDER BY dialect",
    )
    .bind(form_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(dialects, ["uk", "us"]);
}

#[sqlx::test]
async fn v3_pronunciation_allows_draft_null_style_and_rejects_duplicate_complete_triple(
    pool: PgPool,
) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_v3_entry(&pool, admin_id).await;
    let noun_id = catalog_pos_id(&pool, "noun").await;
    let mut tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut tx, entry_id, noun_id, 0).await;
    let group_id = insert_v3_group(&mut tx, entry_id, pos_id, 0).await;
    let (_, _, variant_id) =
        insert_valid_common_form(&mut tx, entry_id, pos_id, group_id, "base", 0, 0).await;
    let draft_id = Uuid::now_v7();
    insert_node(
        &mut tx,
        draft_id,
        entry_id,
        "pronunciation",
        Some(variant_id),
        "forms.pronunciation",
        false,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_pronunciations (
            id, entry_id, form_variant_id, dict_phonetic, actual_pron,
            normalized_dict_phonetic, normalized_actual_pron,
            style, normalization_version, ordinal
        ) VALUES ($1, $2, $3, '', '', '', '', NULL, 1, 0)
        "#,
    )
    .bind(draft_id)
    .bind(entry_id)
    .bind(variant_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let second_draft_id = Uuid::now_v7();
    insert_node(
        &mut tx,
        second_draft_id,
        entry_id,
        "pronunciation",
        Some(variant_id),
        "forms.pronunciation",
        false,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_pronunciations (
            id, entry_id, form_variant_id, dict_phonetic, actual_pron,
            normalized_dict_phonetic, normalized_actual_pron,
            style, normalization_version, ordinal
        ) VALUES ($1, $2, $3, '', '', '', '', NULL, 1, 1)
        "#,
    )
    .bind(second_draft_id)
    .bind(entry_id)
    .bind(variant_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    let complete_id = Uuid::now_v7();
    insert_node(
        &mut tx,
        complete_id,
        entry_id,
        "pronunciation",
        Some(variant_id),
        "forms.pronunciation",
        false,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_pronunciations (
            id, entry_id, form_variant_id, dict_phonetic, actual_pron,
            normalized_dict_phonetic, normalized_actual_pron,
            style, normalization_version, ordinal
        ) VALUES ($1, $2, $3, 'd', 'a', 'd', 'a', 'normal', 1, 2)
        "#,
    )
    .bind(complete_id)
    .bind(entry_id)
    .bind(variant_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut duplicate_tx = pool.begin().await.unwrap();
    let duplicate_id = Uuid::now_v7();
    insert_node(
        &mut duplicate_tx,
        duplicate_id,
        entry_id,
        "pronunciation",
        Some(variant_id),
        "forms.pronunciation",
        false,
    )
    .await;
    let duplicate = sqlx::query(
        r#"
        INSERT INTO lexicon.v3_pronunciations (
            id, entry_id, form_variant_id, dict_phonetic, actual_pron,
            normalized_dict_phonetic, normalized_actual_pron,
            style, normalization_version, ordinal
        ) VALUES ($1, $2, $3, ' D ', 'A', 'd', 'a', 'normal', 1, 3)
        "#,
    )
    .bind(duplicate_id)
    .bind(entry_id)
    .bind(variant_id)
    .execute(&mut *duplicate_tx)
    .await;
    assert_db_error(
        duplicate,
        UNIQUE_VIOLATION,
        "lexicon_v3_pronunciations_complete_triple_key",
    );
}

#[sqlx::test]
async fn v3_sibling_ordinals_are_unique(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let entry_id = insert_v3_entry(&pool, admin_id).await;
    let noun_id = catalog_pos_id(&pool, "noun").await;
    let verb_id = catalog_pos_id(&pool, "verb").await;
    let mut tx = pool.begin().await.unwrap();
    insert_v3_pos(&mut tx, entry_id, noun_id, 0).await;
    tx.commit().await.unwrap();

    let mut pos_tx = pool.begin().await.unwrap();
    let duplicate_pos_id = Uuid::now_v7();
    insert_node(
        &mut pos_tx,
        duplicate_pos_id,
        entry_id,
        "pos",
        None,
        "forms.pos",
        false,
    )
    .await;
    let duplicate_pos = sqlx::query(
        r#"
        INSERT INTO lexicon.entry_pos (
            id, entry_id, part_of_speech_id, content_schema_version,
            spelling_mode, phonetic_mode, sort_order
        ) VALUES ($1, $2, $3, 3, 'unified', 'unified', 0)
        "#,
    )
    .bind(duplicate_pos_id)
    .bind(entry_id)
    .bind(verb_id)
    .execute(&mut *pos_tx)
    .await;
    assert_db_error(
        duplicate_pos,
        UNIQUE_VIOLATION,
        "lexicon_entry_pos_v3_ordinal_key",
    );

    let second_entry = insert_v3_entry(&pool, admin_id).await;
    let mut group_tx = pool.begin().await.unwrap();
    let pos_id = insert_v3_pos(&mut group_tx, second_entry, noun_id, 0).await;
    let group_id = insert_v3_group(&mut group_tx, second_entry, pos_id, 0).await;
    let (_, _, variant_id) =
        insert_valid_common_form(&mut group_tx, second_entry, pos_id, group_id, "base", 0, 0).await;
    let pronunciation_id = Uuid::now_v7();
    insert_node(
        &mut group_tx,
        pronunciation_id,
        second_entry,
        "pronunciation",
        Some(variant_id),
        "forms.pronunciation",
        false,
    )
    .await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.v3_pronunciations (
            id, entry_id, form_variant_id, dict_phonetic, actual_pron,
            normalized_dict_phonetic, normalized_actual_pron,
            style, normalization_version, ordinal
        ) VALUES ($1, $2, $3, 'd', 'a', 'd', 'a', 'normal', 1, 0)
        "#,
    )
    .bind(pronunciation_id)
    .bind(second_entry)
    .bind(variant_id)
    .execute(&mut *group_tx)
    .await
    .unwrap();
    group_tx.commit().await.unwrap();

    let mut duplicate_group_tx = pool.begin().await.unwrap();
    let duplicate_group_id = Uuid::now_v7();
    insert_node(
        &mut duplicate_group_tx,
        duplicate_group_id,
        second_entry,
        "form_group",
        Some(pos_id),
        "forms.form_group",
        false,
    )
    .await;
    let duplicate_group = sqlx::query(
        "INSERT INTO lexicon.v3_form_groups (id, entry_id, entry_pos_id, is_regular, ordinal) VALUES ($1, $2, $3, TRUE, 0)",
    )
    .bind(duplicate_group_id)
    .bind(second_entry)
    .bind(pos_id)
    .execute(&mut *duplicate_group_tx)
    .await;
    assert_db_error(
        duplicate_group,
        UNIQUE_VIOLATION,
        "lexicon_v3_form_groups_ordinal_key",
    );

    let mut duplicate_form_tx = pool.begin().await.unwrap();
    let duplicate_form_id = Uuid::now_v7();
    insert_node(
        &mut duplicate_form_tx,
        duplicate_form_id,
        second_entry,
        "concrete_form",
        Some(pos_id),
        "forms.concrete_form",
        false,
    )
    .await;
    let duplicate_form = sqlx::query(
        "INSERT INTO lexicon.v3_concrete_forms (id, entry_id, entry_pos_id, form_type, ordinal) VALUES ($1, $2, $3, 'plural', 0)",
    )
    .bind(duplicate_form_id)
    .bind(second_entry)
    .bind(pos_id)
    .execute(&mut *duplicate_form_tx)
    .await;
    assert_db_error(
        duplicate_form,
        UNIQUE_VIOLATION,
        "lexicon_v3_concrete_forms_ordinal_key",
    );

    let mut duplicate_membership_tx = pool.begin().await.unwrap();
    let second_form_id = insert_v3_form(
        &mut duplicate_membership_tx,
        second_entry,
        pos_id,
        "plural",
        1,
    )
    .await;
    insert_v3_variant(
        &mut duplicate_membership_tx,
        second_entry,
        second_form_id,
        "common",
        "words",
    )
    .await;
    let duplicate_membership_id = Uuid::now_v7();
    insert_node(
        &mut duplicate_membership_tx,
        duplicate_membership_id,
        second_entry,
        "group_membership",
        Some(group_id),
        "forms.group_membership",
        false,
    )
    .await;
    let duplicate_membership = sqlx::query(
        "INSERT INTO lexicon.v3_group_memberships (id, entry_id, entry_pos_id, form_group_id, form_id, ordinal) VALUES ($1, $2, $3, $4, $5, 0)",
    )
    .bind(duplicate_membership_id)
    .bind(second_entry)
    .bind(pos_id)
    .bind(group_id)
    .bind(second_form_id)
    .execute(&mut *duplicate_membership_tx)
    .await;
    assert_db_error(
        duplicate_membership,
        UNIQUE_VIOLATION,
        "lexicon_v3_group_memberships_ordinal_key",
    );

    let mut duplicate_pronunciation_tx = pool.begin().await.unwrap();
    let duplicate_pronunciation_id = Uuid::now_v7();
    insert_node(
        &mut duplicate_pronunciation_tx,
        duplicate_pronunciation_id,
        second_entry,
        "pronunciation",
        Some(variant_id),
        "forms.pronunciation:ordinal-test",
        false,
    )
    .await;
    let duplicate_pronunciation = sqlx::query(
        r#"
        INSERT INTO lexicon.v3_pronunciations (
            id, entry_id, form_variant_id, dict_phonetic, actual_pron,
            normalized_dict_phonetic, normalized_actual_pron,
            style, normalization_version, ordinal
        ) VALUES ($1, $2, $3, 'x', 'x', 'x', 'x', 'strong', 1, 0)
        "#,
    )
    .bind(duplicate_pronunciation_id)
    .bind(second_entry)
    .bind(variant_id)
    .execute(&mut *duplicate_pronunciation_tx)
    .await;
    assert_db_error(
        duplicate_pronunciation,
        UNIQUE_VIOLATION,
        "lexicon_v3_pronunciations_ordinal_key",
    );
}
