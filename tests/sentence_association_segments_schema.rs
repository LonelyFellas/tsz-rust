use sqlx::PgPool;
use uuid::Uuid;

async fn insert_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'sentence identity test')",
    )
    .bind(id)
    .bind(format!("sentence-identity-{}", id.simple()))
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

async fn insert_node(pool: &PgPool, entry_id: Uuid, node_type: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO lexicon.nodes (id, entry_id, node_type) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(entry_id)
        .bind(node_type)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn insert_publication(pool: &PgPool, admin_id: Uuid, entry_id: Uuid, number: i32) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision,
            content_schema_version, snapshot, snapshot_hash, published_by_admin_id
        ) VALUES ($1, $2, $3, $3, 2, '{}', $4, $5)
        "#,
    )
    .bind(id)
    .bind(entry_id)
    .bind(number)
    .bind(id.as_bytes().to_vec())
    .bind(admin_id)
    .execute(pool)
    .await
    .unwrap();
    id
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

#[derive(Clone, Copy)]
struct ExactAssociationTarget {
    entry_id: Uuid,
    publication_id: Uuid,
    sense_id: Uuid,
    form_id: Uuid,
    variant_id: Uuid,
}

async fn insert_exact_association(
    pool: &PgPool,
    source_entry_id: Uuid,
    sentence_id: Uuid,
    target: ExactAssociationTarget,
    range_start: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO lexicon.sentence_associations (
            id, entry_id, sentence_id, source_dialect,
            association_schema_version, segment_count,
            range_start, range_end, surface, state,
            target_entry_id, target_sense_id, target_form_slot_id,
            target_publication_id, target_form_variant_id,
            target_component_usages_snapshot, origin,
            target_headword_snapshot, target_gloss_snapshot,
            resolved_pos, resolved_form_type
        ) VALUES (
            $1, $2, $3, 'common', 3, 1,
            $4, $4 + 1, 'x', 'linked',
            $5, $6, $7, $8, $9, '[]', 'manual',
            'target', '目标', 'noun', 'base'
        )
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(source_entry_id)
    .bind(sentence_id)
    .bind(range_start)
    .bind(target.entry_id)
    .bind(target.sense_id)
    .bind(target.form_id)
    .bind(target.publication_id)
    .bind(target.variant_id)
    .execute(pool)
    .await
    .map(|_| ())
}

fn assert_constraint(result: Result<(), sqlx::Error>, expected: &str) {
    let error = match result {
        Err(sqlx::Error::Database(error)) => error,
        other => panic!("expected database constraint error, got {other:?}"),
    };
    assert_eq!(error.code().as_deref(), Some("23503"));
    assert_eq!(error.constraint(), Some(expected));
}

#[sqlx::test]
async fn segmented_association_expand_and_generation_guards_are_installed(pool: PgPool) {
    let constraints: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT conname
        FROM pg_constraint
        WHERE connamespace = 'lexicon'::regnamespace
          AND conname = ANY($1::text[])
        ORDER BY conname
        "#,
    )
    .bind([
        "lexicon_sentence_association_segments_ordinal_check",
        "lexicon_sentence_association_segments_parent_fkey",
        "lexicon_sentence_associations_segment_count_check",
        "lexicon_sentence_associations_segment_parent_key",
        "lexicon_sentence_associations_v2_segment_count_check",
        "lexicon_sentence_associations_variant_identity_shape_check",
        "lexicon_sentence_associations_target_publication_fkey",
        "lexicon_sentence_associations_target_variant_fkey",
        "lexicon_sentence_associations_target_publication_variant_fkey",
        "lexicon_sentence_associations_target_publication_sense_fkey",
        "lexicon_sentence_associations_target_publication_form_fkey",
    ])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        constraints.len(),
        11,
        "segmented contract constraints drifted"
    );

    let identity_columns: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'lexicon'
          AND table_name = 'sentence_associations'
          AND column_name = ANY($1::text[])
        ORDER BY column_name
        "#,
    )
    .bind([
        "target_component_usages_snapshot",
        "target_form_variant_id",
        "target_publication_id",
    ])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        identity_columns.len(),
        3,
        "variant identity columns drifted"
    );

    let segment_columns: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'lexicon'
          AND table_name = 'sentence_association_segments'
        ORDER BY ordinal_position
        "#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        segment_columns,
        [
            "association_id",
            "ordinal",
            "sentence_id",
            "source_dialect",
            "range_start",
            "range_end",
            "surface",
        ]
    );

    let triggers: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT tgname
        FROM pg_trigger
        WHERE NOT tgisinternal
          AND tgname = ANY($1::text[])
        ORDER BY tgname
        "#,
    )
    .bind([
        "lexicon_sentence_associations_legacy_segment_trigger",
        "lexicon_surface_sources_discovery_generation_trigger",
    ])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(triggers.len(), 2, "dual-write/generation guard drifted");

    let generation: i64 = sqlx::query_scalar(
        "SELECT generation FROM lexicon.sentence_discovery_generation WHERE singleton = TRUE",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(generation > 0);
}

#[sqlx::test]
async fn exact_publication_identity_rejects_sense_and_form_from_another_publication(pool: PgPool) {
    let admin_id = insert_admin(&pool).await;
    let source_entry_id = insert_entry(&pool, admin_id).await;
    let target_entry_id = insert_entry(&pool, admin_id).await;
    let sentence_id = insert_node(&pool, source_entry_id, "sentence").await;
    let sense_one = insert_node(&pool, target_entry_id, "sense").await;
    let sense_two = insert_node(&pool, target_entry_id, "sense").await;
    let form_one = insert_node(&pool, target_entry_id, "form_slot").await;
    let form_two = insert_node(&pool, target_entry_id, "form_slot").await;
    let variant_one = insert_node(&pool, target_entry_id, "form_variant").await;
    let variant_two = insert_node(&pool, target_entry_id, "form_variant").await;
    let publication_one = insert_publication(&pool, admin_id, target_entry_id, 1).await;
    let publication_two = insert_publication(&pool, admin_id, target_entry_id, 2).await;
    for (publication, sense, form, variant) in [
        (publication_one, sense_one, form_one, variant_one),
        (publication_two, sense_two, form_two, variant_two),
    ] {
        insert_publication_node(&pool, publication, target_entry_id, sense, "sense").await;
        insert_publication_node(&pool, publication, target_entry_id, form, "form_slot").await;
        insert_publication_node(&pool, publication, target_entry_id, variant, "form_variant").await;
    }

    let valid_target = ExactAssociationTarget {
        entry_id: target_entry_id,
        publication_id: publication_one,
        sense_id: sense_one,
        form_id: form_one,
        variant_id: variant_one,
    };
    insert_exact_association(&pool, source_entry_id, sentence_id, valid_target, 0)
        .await
        .unwrap();
    assert_constraint(
        insert_exact_association(
            &pool,
            source_entry_id,
            sentence_id,
            ExactAssociationTarget {
                sense_id: sense_two,
                ..valid_target
            },
            2,
        )
        .await,
        "lexicon_sentence_associations_target_publication_sense_fkey",
    );
    assert_constraint(
        insert_exact_association(
            &pool,
            source_entry_id,
            sentence_id,
            ExactAssociationTarget {
                form_id: form_two,
                ..valid_target
            },
            4,
        )
        .await,
        "lexicon_sentence_associations_target_publication_form_fkey",
    );
    assert_constraint(
        insert_exact_association(
            &pool,
            source_entry_id,
            sentence_id,
            ExactAssociationTarget {
                variant_id: variant_two,
                ..valid_target
            },
            6,
        )
        .await,
        "lexicon_sentence_associations_target_publication_variant_fkey",
    );
}
