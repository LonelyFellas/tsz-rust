use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use tsz_rust::lexicon::{
    dto::{
        AdminWordStatus, AdminWordV2, DetectionRequestEcho, Dialect, DialectRulesV2,
        DraftFormsStepContent, DraftMeaningsStepContent, EntryKind, PersistedWordStep,
        PronunciationStyle, TextOrigin, WordBaseFormSlotV2, WordCreationStep,
        WordDerivedFormSlotV2, WordDetectionSnapshotSmartDictionaryV2, WordDetectionSnapshotV2,
        WordFormGroupV2, WordFormVariantV2, WordHeadwordsV2, WordPosFormsV2, WordPronunciationV2,
    },
    v3_migration::{
        MAX_MIGRATION_BATCH_ENTRIES, ROLLBACK_BLOCKED_V3_WRITE, apply, approve, dry_run,
        enable_publication_canary, rollback, verify,
    },
};
use uuid::Uuid;

struct Fixture {
    admin_id: Uuid,
    entry_id: Uuid,
    publication_id: Uuid,
    base_form_id: Uuid,
    group_ids: Vec<Uuid>,
    source_forms: Value,
    snapshot: Value,
    snapshot_hash: Vec<u8>,
}

async fn wait_for_advisory_lock_waiters(pool: &PgPool, expected: i64) {
    for _ in 0..100 {
        let waiting: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND wait_event_type = 'Lock'
              AND wait_event = 'advisory'
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("expected {expected} transactions waiting for advisory locks");
}

async fn wait_for_database_lock_waiters(pool: &PgPool, expected: i64) {
    for _ in 0..100 {
        let waiting: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND wait_event_type = 'Lock'
            "#,
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("expected {expected} transactions waiting for database locks");
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

async fn seed_published_v2(pool: &PgPool, group_count: usize) -> Fixture {
    seed_published_v2_with_surface(pool, group_count, None).await
}

async fn seed_published_v2_with_surface(
    pool: &PgPool,
    group_count: usize,
    custom_surface: Option<&str>,
) -> Fixture {
    let admin_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'V3 migration test')",
    )
    .bind(admin_id)
    .bind(format!("v3-migration-{}", admin_id.simple()))
    .execute(pool)
    .await
    .unwrap();
    let entry_id = Uuid::now_v7();
    let publication_id = Uuid::now_v7();
    let pos_id = Uuid::now_v7();
    let base_form_id = Uuid::now_v7();
    let base_variant_id = Uuid::now_v7();
    let base_pronunciation_id = Uuid::now_v7();
    let surface = custom_surface
        .map(str::to_owned)
        .unwrap_or_else(|| format!("migration-{}", entry_id.simple()));
    let now = Utc::now();
    let mut groups = Vec::new();
    let mut derived = Vec::new();
    for index in 0..group_count {
        let group_id = Uuid::now_v7();
        let slot_id = Uuid::now_v7();
        let variant_id = Uuid::now_v7();
        groups.push(WordFormGroupV2 {
            id: group_id,
            is_regular: index % 2 == 0,
            slots: vec![WordDerivedFormSlotV2 {
                id: slot_id,
                form_type: "plural".to_owned(),
                variants: vec![WordFormVariantV2 {
                    id: variant_id,
                    dialect: Dialect::Common,
                    spelling: format!("{surface}-{index}"),
                    origin: TextOrigin::Manual,
                    pronunciations: Vec::new(),
                }],
            }],
        });
        derived.push((group_id, slot_id, variant_id));
    }
    let forms = DraftFormsStepContent {
        pos: vec![WordPosFormsV2 {
            pos_id,
            pos: "noun".to_owned(),
            dialect_rules: DialectRulesV2 {
                spelling_mode: "unified".to_owned(),
                phonetic_mode: "unified".to_owned(),
            },
            base_form: WordBaseFormSlotV2 {
                id: base_form_id,
                form_type: "base".to_owned(),
                variants: vec![WordFormVariantV2 {
                    id: base_variant_id,
                    dialect: Dialect::Common,
                    spelling: surface.clone(),
                    origin: TextOrigin::Manual,
                    pronunciations: vec![WordPronunciationV2 {
                        id: base_pronunciation_id,
                        dict_phonetic: "test".to_owned(),
                        actual_pron: "test".to_owned(),
                        style: PronunciationStyle::Normal,
                    }],
                }],
            },
            form_groups: groups,
        }],
    };
    let meanings = DraftMeaningsStepContent::default();
    let detection = WordDetectionSnapshotV2 {
        detection_id: Uuid::now_v7(),
        request: DetectionRequestEcho {
            language: "en".to_owned(),
            headword: surface.clone(),
        },
        normalized_headword: surface.clone(),
        entry_kind: EntryKind::Word,
        matched_dialect: Dialect::Common,
        builtin_dictionary_status: "not_found".to_owned(),
        smart_dictionary: WordDetectionSnapshotSmartDictionaryV2::Clear {
            surface_warning: None,
        },
        headwords: WordHeadwordsV2::Unified {
            common: surface.clone(),
        },
        suggested_pos: vec!["noun".to_owned()],
        dictionary_provider: None,
        dictionary_coverage: None,
        dictionary_provenance: None,
        detected_at: now,
    };
    let word = AdminWordV2 {
        schema_version: 2,
        id: entry_id,
        language: "en".to_owned(),
        kind: EntryKind::Word,
        status: AdminWordStatus::Published,
        revision: 7,
        lifecycle_revision: 1,
        published_revision: Some(7),
        has_unpublished_changes: false,
        headwords: WordHeadwordsV2::Unified {
            common: surface.clone(),
        },
        frequency: None,
        detection_snapshot: detection.clone(),
        forms: forms.clone(),
        meanings: meanings.clone(),
        completed_steps: vec![
            PersistedWordStep::Basics,
            PersistedWordStep::Forms,
            PersistedWordStep::Meanings,
        ],
        max_reachable_step: WordCreationStep::Preview,
        created_by: admin_id,
        created_at: now,
        updated_at: now,
        archived_at: None,
        archived_by: None,
        published_at: Some(now),
    };
    let snapshot = serde_json::to_value(&word).unwrap();
    let snapshot_hash = Sha256::digest(serde_json::to_vec(&snapshot).unwrap()).to_vec();

    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, content_schema_version, language, kind, revision,
            headword_mode, source_dialect, detection_snapshot,
            created_by_admin_id, updated_by_admin_id, created_at, updated_at
        ) VALUES ($1, 2, 'en', 'word', 7, 'unified', NULL, $2, $3, $3, $4, $4)
        "#,
    )
    .bind(entry_id)
    .bind(serde_json::to_value(&detection).unwrap())
    .bind(admin_id)
    .bind(now)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_headwords (
            id, entry_id, dialect, headword, normalized_headword,
            normalization_version, origin
        ) VALUES ($1, $2, 'common', $3, $3, 1, 'manual')
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(entry_id)
    .bind(&surface)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_headword_keys (
            entry_id, language, kind, dialect_scope,
            normalized_headword, normalization_version
        ) VALUES
            ($1, 'en', 'word', 'uk', $2, 1),
            ($1, 'en', 'word', 'us', $2, 1)
        "#,
    )
    .bind(entry_id)
    .bind(&surface)
    .execute(&mut *tx)
    .await
    .unwrap();
    let headword_surface_id = format!("headword:{entry_id}");
    sqlx::query(
        r#"
        INSERT INTO lexicon.surface_sources (
            entry_id, source_id, source_kind, source_node_id,
            language, entry_kind, dialect, dialect_scope,
            surface, normalized_surface, normalization_version,
            source_revision, is_deleted, content_scope, publication_id,
            pos_id, pos, form_type, content_schema_version
        ) VALUES
            ($1, $3, 'headword', NULL,
             'en', 'word', 'common', 'uk', $2, $2, 1,
             7, FALSE, 'draft', NULL, NULL, NULL, NULL, 2),
            ($1, $3, 'headword', NULL,
             'en', 'word', 'common', 'us', $2, $2, 1,
             7, FALSE, 'draft', NULL, NULL, NULL, NULL, 2)
        "#,
    )
    .bind(entry_id)
    .bind(&surface)
    .bind(headword_surface_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_editor_projection (
            entry_id, forms, meanings, rebuilt_revision
        ) VALUES ($1, $2, $3, 7)
        "#,
    )
    .bind(entry_id)
    .bind(serde_json::to_value(&forms).unwrap())
    .bind(serde_json::to_value(&meanings).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    let catalog_pos_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = 'noun'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    insert_node(&mut tx, pos_id, entry_id, "pos", None, "forms.pos", false).await;
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_pos (
            id, entry_id, part_of_speech_id, spelling_mode,
            phonetic_mode, sort_order, content_schema_version
        ) VALUES ($1, $2, $3, 'unified', 'unified', 0, 2)
        "#,
    )
    .bind(pos_id)
    .bind(entry_id)
    .bind(catalog_pos_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    for (index, (group_id, _, _)) in derived.iter().enumerate() {
        insert_node(
            &mut tx,
            *group_id,
            entry_id,
            "form_group",
            Some(pos_id),
            "forms.form_group",
            false,
        )
        .await;
        sqlx::query(
            "INSERT INTO lexicon.form_groups (id, entry_id, entry_pos_id, is_regular, sort_order) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(group_id)
        .bind(entry_id)
        .bind(pos_id)
        .bind(index % 2 == 0)
        .bind(index as i32)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    insert_node(
        &mut tx,
        base_form_id,
        entry_id,
        "form_slot",
        Some(pos_id),
        "forms.base_form",
        true,
    )
    .await;
    sqlx::query(
        "INSERT INTO lexicon.form_slots (id, entry_id, entry_pos_id, form_group_id, form_type, sort_order) VALUES ($1, $2, $3, NULL, 'base', 0)",
    )
    .bind(base_form_id)
    .bind(entry_id)
    .bind(pos_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    insert_node(
        &mut tx,
        base_variant_id,
        entry_id,
        "form_variant",
        Some(base_form_id),
        "forms.form_variant:common",
        true,
    )
    .await;
    sqlx::query(
        "INSERT INTO lexicon.form_variants (id, entry_id, form_slot_id, dialect, spelling, origin, sort_order) VALUES ($1, $2, $3, 'common', $4, 'manual', 0)",
    )
    .bind(base_variant_id)
    .bind(entry_id)
    .bind(base_form_id)
    .bind(&surface)
    .execute(&mut *tx)
    .await
    .unwrap();
    insert_node(
        &mut tx,
        base_pronunciation_id,
        entry_id,
        "pronunciation",
        Some(base_variant_id),
        "forms.pronunciation",
        false,
    )
    .await;
    sqlx::query(
        "INSERT INTO lexicon.pronunciations (id, entry_id, form_variant_id, dict_phonetic, actual_pron, style, sort_order) VALUES ($1, $2, $3, 'test', 'test', 'normal', 0)",
    )
    .bind(base_pronunciation_id)
    .bind(entry_id)
    .bind(base_variant_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    for (group_id, slot_id, variant_id) in &derived {
        insert_node(
            &mut tx,
            *slot_id,
            entry_id,
            "form_slot",
            Some(*group_id),
            "forms.form_slot:plural",
            true,
        )
        .await;
        sqlx::query(
            "INSERT INTO lexicon.form_slots (id, entry_id, entry_pos_id, form_group_id, form_type, sort_order) VALUES ($1, $2, $3, $4, 'plural', 0)",
        )
        .bind(slot_id)
        .bind(entry_id)
        .bind(pos_id)
        .bind(group_id)
        .execute(&mut *tx)
        .await
        .unwrap();
        insert_node(
            &mut tx,
            *variant_id,
            entry_id,
            "form_variant",
            Some(*slot_id),
            "forms.form_variant:common",
            true,
        )
        .await;
        let spelling = format!(
            "{surface}-{}",
            derived.iter().position(|row| row.1 == *slot_id).unwrap()
        );
        sqlx::query(
            "INSERT INTO lexicon.form_variants (id, entry_id, form_slot_id, dialect, spelling, origin, sort_order) VALUES ($1, $2, $3, 'common', $4, 'manual', 0)",
        )
        .bind(variant_id)
        .bind(entry_id)
        .bind(slot_id)
        .bind(spelling)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"
        INSERT INTO lexicon.entry_publications (
            id, entry_id, publication_number, source_revision,
            content_schema_version, snapshot, snapshot_hash,
            published_by_admin_id, published_at
        ) VALUES ($1, $2, 1, 7, 2, $3, $4, $5, $6)
        "#,
    )
    .bind(publication_id)
    .bind(entry_id)
    .bind(&snapshot)
    .bind(&snapshot_hash)
    .bind(admin_id)
    .bind(now)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE lexicon.entries SET current_publication_id = $2 WHERE id = $1")
        .bind(entry_id)
        .bind(publication_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    Fixture {
        admin_id,
        entry_id,
        publication_id,
        base_form_id,
        group_ids: derived.iter().map(|row| row.0).collect(),
        source_forms: serde_json::to_value(forms).unwrap(),
        snapshot,
        snapshot_hash,
    }
}

async fn assert_publication_unchanged(pool: &PgPool, fixture: &Fixture) {
    let (snapshot, hash, current): (Value, Vec<u8>, Option<Uuid>) = sqlx::query_as(
        r#"
        SELECT publication.snapshot, publication.snapshot_hash,
               entry.current_publication_id
        FROM lexicon.entry_publications publication
        JOIN lexicon.entries entry ON entry.id = publication.entry_id
        WHERE publication.id = $1
        "#,
    )
    .bind(fixture.publication_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(snapshot, fixture.snapshot);
    assert_eq!(hash, fixture.snapshot_hash);
    assert_eq!(current, Some(fixture.publication_id));
    let historical: AdminWordV2 = serde_json::from_value(snapshot).unwrap();
    assert_eq!(historical.schema_version, 2);
}

async fn approved_manifest(pool: &PgPool, actor_id: Uuid, entry_ids: &[Uuid]) -> (Uuid, String) {
    let batch_id = Uuid::now_v7();
    let report = dry_run(pool, batch_id, actor_id, Uuid::now_v7(), entry_ids)
        .await
        .unwrap();
    let approval = approve(
        pool,
        batch_id,
        actor_id,
        Uuid::now_v7(),
        &report.manifest_digest,
    )
    .await
    .unwrap();
    assert!(!approval.replayed);
    (batch_id, report.manifest_digest)
}

#[sqlx::test]
async fn dry_run_is_read_only_and_maps_zero_single_and_multiple_groups(pool: PgPool) {
    let zero = seed_published_v2(&pool, 0).await;
    let single = seed_published_v2(&pool, 1).await;
    let multiple = seed_published_v2(&pool, 2).await;
    let entry_ids = [zero.entry_id, single.entry_id, multiple.entry_id];
    let batch_id = Uuid::now_v7();
    let request_id = Uuid::now_v7();
    let first = dry_run(&pool, batch_id, zero.admin_id, request_id, &entry_ids)
        .await
        .unwrap();
    let second = dry_run(&pool, batch_id, zero.admin_id, request_id, &entry_ids)
        .await
        .unwrap();

    assert_eq!(first.eligible_entries, 3);
    assert_eq!(first.blocked_entries, 0);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.expected_digest.clone())
            .collect::<Vec<_>>(),
        second
            .entries
            .iter()
            .map(|entry| entry.expected_digest.clone())
            .collect::<Vec<_>>()
    );
    let counts = first
        .entries
        .iter()
        .map(|entry| (entry.entry_id, entry.counts.unwrap()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(counts[&zero.entry_id].form_groups, 1);
    assert_eq!(counts[&zero.entry_id].synthetic_groups, 1);
    assert_eq!(counts[&zero.entry_id].memberships, 1);
    assert_eq!(counts[&single.entry_id].memberships, 2);
    assert_eq!(counts[&multiple.entry_id].memberships, 4);
    let control_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_migration_batches")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        control_rows, 1,
        "dry-run persists only its control manifest"
    );
    assert_eq!(first.manifest_digest, second.manifest_digest);
    let migrated_entries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.entries WHERE id = ANY($1) AND content_schema_version = 3",
    )
    .bind(entry_ids)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(migrated_entries, 0, "dry-run must not write business rows");
}

#[sqlx::test]
async fn dry_run_blocks_a_v2_entry_that_exceeds_the_v3_limit_after_conversion(pool: PgPool) {
    // V2 has 4 + 3*400 = 1204 nodes. V3 preserves those nodes and adds two
    // memberships per group, so the converted target has 2004 nodes.
    let fixture = seed_published_v2(&pool, 400).await;
    let report = dry_run(
        &pool,
        Uuid::now_v7(),
        fixture.admin_id,
        Uuid::now_v7(),
        &[fixture.entry_id],
    )
    .await
    .unwrap();

    assert_eq!(report.eligible_entries, 0, "{report:?}");
    assert_eq!(report.blocked_entries, 1, "{report:?}");
    assert_eq!(
        report.entries[0].block_code.as_deref(),
        Some("v3_target_contract_invalid")
    );
    assert!(
        report.entries[0]
            .block_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("content_limit_exceeded")),
        "{report:?}"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i16>(
            "SELECT content_schema_version FROM lexicon.entries WHERE id = $1",
        )
        .bind(fixture.entry_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        2,
        "dry-run must not mutate the source entry"
    );
}

#[sqlx::test]
async fn dry_run_requires_an_explicit_bounded_selection(pool: PgPool) {
    let actor_id = Uuid::now_v7();
    let empty_batch_id = Uuid::now_v7();
    let empty_error = dry_run(&pool, empty_batch_id, actor_id, Uuid::now_v7(), &[])
        .await
        .unwrap_err();
    assert!(
        empty_error
            .to_string()
            .contains("migration_entry_ids_required")
    );

    let oversized_batch_id = Uuid::now_v7();
    let oversized_entry_ids = (0..=MAX_MIGRATION_BATCH_ENTRIES)
        .map(|_| Uuid::now_v7())
        .collect::<Vec<_>>();
    let oversized_error = dry_run(
        &pool,
        oversized_batch_id,
        actor_id,
        Uuid::now_v7(),
        &oversized_entry_ids,
    )
    .await
    .unwrap_err();
    assert!(
        oversized_error
            .to_string()
            .contains("migration_batch_too_large")
    );

    let persisted_batches: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_migration_batches WHERE id = ANY($1)")
            .bind(vec![empty_batch_id, oversized_batch_id])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        persisted_batches, 0,
        "rejected selections must not persist control rows"
    );
}

#[sqlx::test]
async fn apply_requires_a_persisted_and_explicitly_approved_manifest(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 1).await;
    let missing_batch = apply(
        &pool,
        Uuid::now_v7(),
        fixture.admin_id,
        Uuid::now_v7(),
        "missing",
    )
    .await
    .unwrap_err();
    assert!(
        missing_batch
            .to_string()
            .contains("migration_dry_run_required")
    );

    let batch_id = Uuid::now_v7();
    let manifest = dry_run(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &[fixture.entry_id],
    )
    .await
    .unwrap();
    let wrong_approval = approve(&pool, batch_id, fixture.admin_id, Uuid::now_v7(), "wrong")
        .await
        .unwrap_err();
    assert!(
        wrong_approval
            .to_string()
            .contains("migration_manifest_digest_mismatch")
    );
    let unapproved = apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest.manifest_digest,
    )
    .await
    .unwrap_err();
    assert!(
        unapproved
            .to_string()
            .contains("migration_approval_required")
    );
    let (schema_version, state_count): (i16, i64) = sqlx::query_as(
        r#"
        SELECT entry.content_schema_version,
               (SELECT count(*) FROM lexicon.v3_entry_state WHERE entry_id = entry.id)
        FROM lexicon.entries entry WHERE entry.id = $1
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((schema_version, state_count), (2, 0));
}

#[sqlx::test]
async fn approved_manifest_rejects_source_drift_before_any_business_write(pool: PgPool) {
    let forms_drift = seed_published_v2(&pool, 1).await;
    let revision_drift = seed_published_v2(&pool, 1).await;
    let publication_drift = seed_published_v2(&pool, 1).await;
    let (forms_batch, forms_manifest) =
        approved_manifest(&pool, forms_drift.admin_id, &[forms_drift.entry_id]).await;
    let (revision_batch, revision_manifest) =
        approved_manifest(&pool, revision_drift.admin_id, &[revision_drift.entry_id]).await;
    let (publication_batch, publication_manifest) = approved_manifest(
        &pool,
        publication_drift.admin_id,
        &[publication_drift.entry_id],
    )
    .await;

    let mut changed_forms = forms_drift.source_forms.clone();
    changed_forms["pos"][0]["base_form"]["variants"][0]["spelling"] =
        serde_json::json!("changed-after-approval");
    sqlx::query("UPDATE lexicon.entry_editor_projection SET forms = $2 WHERE entry_id = $1")
        .bind(forms_drift.entry_id)
        .bind(changed_forms)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.entries SET revision = revision + 1 WHERE id = $1")
        .bind(revision_drift.entry_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lexicon.entry_publications SET snapshot_hash = $2 WHERE id = $1")
        .bind(publication_drift.publication_id)
        .bind(vec![0x5a_u8; 32])
        .execute(&pool)
        .await
        .unwrap();

    for (fixture, batch_id, manifest_digest) in [
        (&forms_drift, forms_batch, forms_manifest),
        (&revision_drift, revision_batch, revision_manifest),
        (&publication_drift, publication_batch, publication_manifest),
    ] {
        let error = apply(
            &pool,
            batch_id,
            fixture.admin_id,
            Uuid::now_v7(),
            &manifest_digest,
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("migration_manifest_source_changed")
        );
        let (schema_version, v3_state_count, v3_form_count): (i16, i64, i64) = sqlx::query_as(
            r#"
                SELECT entry.content_schema_version,
                       (SELECT count(*) FROM lexicon.v3_entry_state WHERE entry_id = entry.id),
                       (SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id = entry.id)
                FROM lexicon.entries entry WHERE entry.id = $1
                "#,
        )
        .bind(fixture.entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((schema_version, v3_state_count, v3_form_count), (2, 0, 0));
    }
}

#[sqlx::test]
async fn preflight_failure_aborts_all_planned_entries_and_frees_live_ownership(pool: PgPool) {
    let drifted = seed_published_v2(&pool, 1).await;
    let untouched = seed_published_v2(&pool, 1).await;
    let entry_ids = [drifted.entry_id, untouched.entry_id];
    let (batch_id, manifest_digest) = approved_manifest(&pool, drifted.admin_id, &entry_ids).await;
    sqlx::query("UPDATE lexicon.entries SET revision = 8 WHERE id = $1")
        .bind(drifted.entry_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE lexicon.entry_editor_projection SET rebuilt_revision = 8 WHERE entry_id = $1",
    )
    .bind(drifted.entry_id)
    .execute(&pool)
    .await
    .unwrap();

    let error = apply(
        &pool,
        batch_id,
        drifted.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("migration_manifest_source_changed"),
        "{error:#}"
    );

    let batch: (String, i32, i32) = sqlx::query_as(
        "SELECT status, applied_count, failed_count FROM lexicon.v3_migration_batches WHERE id = $1",
    )
    .bind(batch_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(batch, ("failed".to_owned(), 0, 2));
    let entries = sqlx::query_as::<_, (Uuid, String, Option<String>, i16)>(
        r#"
        SELECT migration.entry_id, migration.status, migration.failure_code,
               entry.content_schema_version
        FROM lexicon.v3_migration_entries migration
        JOIN lexicon.entries entry ON entry.id = migration.entry_id
        WHERE migration.batch_id = $1
        ORDER BY migration.entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|(_, status, failure_code, schema)| status == "failed"
                && failure_code.is_some()
                && *schema == 2)
    );
    assert!(entries.iter().any(|(_, _, failure_code, _)| {
        failure_code.as_deref() == Some("manifest_source_changed")
    }));
    assert!(entries.iter().any(|(_, _, failure_code, _)| {
        failure_code.as_deref() == Some("manifest_preflight_aborted")
    }));

    let replacement = dry_run(
        &pool,
        Uuid::now_v7(),
        drifted.admin_id,
        Uuid::now_v7(),
        &entry_ids,
    )
    .await
    .expect("terminal preflight failure must release live entry ownership");
    assert_eq!(replacement.scanned_entries, 2);
    assert_eq!(replacement.eligible_entries, 2);
}

#[sqlx::test]
async fn apply_replay_recovers_failed_checkpoint_then_rollback_restores_partial_batch(
    pool: PgPool,
) {
    let checkpoint_failed = seed_published_v2(&pool, 1).await;
    let pending = seed_published_v2(&pool, 1).await;
    let entry_ids = [checkpoint_failed.entry_id, pending.entry_id];
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, checkpoint_failed.admin_id, &entry_ids).await;
    sqlx::query("UPDATE lexicon.v3_migration_batches SET status = 'applying' WHERE id = $1")
        .bind(batch_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_entries
        SET status = 'failed', failure_code = 'apply_failed'
        WHERE batch_id = $1 AND entry_id = $2
        "#,
    )
    .bind(batch_id)
    .bind(checkpoint_failed.entry_id)
    .execute(&pool)
    .await
    .unwrap();

    let report = apply(
        &pool,
        batch_id,
        checkpoint_failed.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .expect("apply replay must converge an applying batch with a failed checkpoint");
    assert_eq!(report.applied_entries, 1);
    assert_eq!(report.failed_entries, 1);
    assert!(
        report.entries.iter().any(|entry| {
            entry.entry_id == checkpoint_failed.entry_id && entry.status == "failed"
        })
    );
    assert!(
        report
            .entries
            .iter()
            .any(|entry| entry.entry_id == pending.entry_id && entry.status == "applied")
    );
    let before_rollback = sqlx::query_as::<_, (String, i16, i16)>(
        r#"
        SELECT batch.status, failed_entry.content_schema_version,
               applied_entry.content_schema_version
        FROM lexicon.v3_migration_batches batch
        JOIN lexicon.entries failed_entry ON failed_entry.id = $2
        JOIN lexicon.entries applied_entry ON applied_entry.id = $3
        WHERE batch.id = $1
        "#,
    )
    .bind(batch_id)
    .bind(checkpoint_failed.entry_id)
    .bind(pending.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(before_rollback, ("failed".to_owned(), 2, 3));

    let rollback_report = rollback(&pool, batch_id, checkpoint_failed.admin_id, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(rollback_report.rolled_back_entries, 1);
    let final_state = sqlx::query_as::<_, (String, String, String, i16, i16)>(
        r#"
        SELECT batch.status, failed_migration.status, applied_migration.status,
               failed_entry.content_schema_version, applied_entry.content_schema_version
        FROM lexicon.v3_migration_batches batch
        JOIN lexicon.v3_migration_entries failed_migration
          ON failed_migration.batch_id = batch.id AND failed_migration.entry_id = $2
        JOIN lexicon.v3_migration_entries applied_migration
          ON applied_migration.batch_id = batch.id AND applied_migration.entry_id = $3
        JOIN lexicon.entries failed_entry ON failed_entry.id = failed_migration.entry_id
        JOIN lexicon.entries applied_entry ON applied_entry.id = applied_migration.entry_id
        WHERE batch.id = $1
        "#,
    )
    .bind(batch_id)
    .bind(checkpoint_failed.entry_id)
    .bind(pending.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        final_state,
        (
            "rolled_back".to_owned(),
            "failed".to_owned(),
            "rolled_back".to_owned(),
            2,
            2,
        )
    );
}

#[sqlx::test]
async fn rollback_cleans_legacy_failed_batch_without_business_writes(pool: PgPool) {
    let failed = seed_published_v2(&pool, 1).await;
    let stranded = seed_published_v2(&pool, 1).await;
    let entry_ids = [failed.entry_id, stranded.entry_id];
    let (batch_id, _) = approved_manifest(&pool, failed.admin_id, &entry_ids).await;
    sqlx::query(
        "UPDATE lexicon.v3_migration_batches SET status = 'failed', failed_count = 1 WHERE id = $1",
    )
    .bind(batch_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        UPDATE lexicon.v3_migration_entries
        SET status = 'failed', failure_code = 'manifest_source_changed'
        WHERE batch_id = $1 AND entry_id = $2
        "#,
    )
    .bind(batch_id)
    .bind(failed.entry_id)
    .execute(&pool)
    .await
    .unwrap();

    let rollback_request_id = Uuid::now_v7();
    let report = rollback(&pool, batch_id, failed.admin_id, rollback_request_id)
        .await
        .expect("failed no-write batch must have a legal terminal cleanup path");
    assert_eq!(report.rolled_back_entries, 0);
    let replay = rollback(&pool, batch_id, failed.admin_id, rollback_request_id)
        .await
        .expect("same rollback command must replay after response loss");
    assert_eq!(replay.rolled_back_entries, 0);
    assert!(replay.entries.is_empty());
    let conflict = rollback(&pool, batch_id, failed.admin_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        conflict
            .to_string()
            .contains("migration_rollback_idempotency_conflict")
    );
    let states = sqlx::query_as::<_, (String, String, Option<String>, i16)>(
        r#"
        SELECT batch.status, migration.status, migration.failure_code,
               entry.content_schema_version
        FROM lexicon.v3_migration_batches batch
        JOIN lexicon.v3_migration_entries migration ON migration.batch_id = batch.id
        JOIN lexicon.entries entry ON entry.id = migration.entry_id
        WHERE batch.id = $1
        ORDER BY migration.entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(states.len(), 2);
    assert!(
        states
            .iter()
            .all(|(batch_status, entry_status, _, schema)| {
                batch_status == "rolled_back" && entry_status == "failed" && *schema == 2
            })
    );
    assert!(states.iter().any(|(_, _, failure_code, _)| {
        failure_code.as_deref() == Some("rollback_aborted_unapplied")
    }));
    let (audit_actor, audit_request, audit_count, metadata): (Uuid, Uuid, i64, Value) =
        sqlx::query_as(
            r#"
            SELECT audit.actor_admin_id, audit.request_id,
                   (SELECT count(*) FROM audit.admin_actions counted
                    WHERE counted.action = 'lexicon.migration_batch.rollback'
                      AND counted.resource_type = 'lexicon.migration_batch'
                      AND counted.resource_id = $1),
                   audit.metadata
            FROM audit.admin_actions audit
            WHERE audit.action = 'lexicon.migration_batch.rollback'
              AND audit.resource_type = 'lexicon.migration_batch'
              AND audit.resource_id = $1
            "#,
        )
        .bind(batch_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audit_actor, failed.admin_id);
    assert_eq!(audit_request, rollback_request_id);
    assert_eq!(audit_count, 1);
    assert_eq!(metadata["rolled_back_entries"], 0);
}

#[sqlx::test]
async fn apply_and_rollback_serialize_on_every_batch_entry(pool: PgPool) {
    let first = seed_published_v2(&pool, 1).await;
    let second = seed_published_v2(&pool, 1).await;
    let mut entry_ids = vec![first.entry_id, second.entry_id];
    entry_ids.sort_unstable();
    let (batch_id, manifest_digest) = approved_manifest(&pool, first.admin_id, &entry_ids).await;

    let mut last_entry_gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "lexicon.v3-migration.entry:{}",
            entry_ids[entry_ids.len() - 1]
        ))
        .execute(&mut *last_entry_gate)
        .await
        .unwrap();
    let apply_pool = pool.clone();
    let apply_digest = manifest_digest.clone();
    let actor_id = first.admin_id;
    let apply_task = tokio::spawn(async move {
        apply(
            &apply_pool,
            batch_id,
            actor_id,
            Uuid::now_v7(),
            &apply_digest,
        )
        .await
    });
    wait_for_advisory_lock_waiters(&pool, 1).await;

    let rollback_pool = pool.clone();
    let rollback_task =
        tokio::spawn(
            async move { rollback(&rollback_pool, batch_id, actor_id, Uuid::now_v7()).await },
        );
    wait_for_advisory_lock_waiters(&pool, 2).await;
    let before_release = sqlx::query_scalar::<_, i16>(
        "SELECT content_schema_version FROM lexicon.entries WHERE id = ANY($1) ORDER BY id",
    )
    .bind(&entry_ids)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(before_release, vec![2, 2]);

    last_entry_gate.commit().await.unwrap();
    let apply_report = tokio::time::timeout(std::time::Duration::from_secs(10), apply_task)
        .await
        .expect("apply must finish without deadlock")
        .unwrap()
        .unwrap();
    assert_eq!(apply_report.applied_entries, 2);
    let rollback_report = tokio::time::timeout(std::time::Duration::from_secs(10), rollback_task)
        .await
        .expect("rollback must run after the complete apply command")
        .unwrap()
        .unwrap();
    assert_eq!(rollback_report.rolled_back_entries, 2);

    let final_rows = sqlx::query_as::<_, (String, String, i16)>(
        r#"
        SELECT batch.status, migration.status, entry.content_schema_version
        FROM lexicon.v3_migration_batches batch
        JOIN lexicon.v3_migration_entries migration ON migration.batch_id = batch.id
        JOIN lexicon.entries entry ON entry.id = migration.entry_id
        WHERE batch.id = $1
        ORDER BY migration.entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(final_rows.len(), 2);
    assert!(
        final_rows
            .iter()
            .all(|(batch_status, entry_status, schema)| {
                batch_status == "rolled_back" && entry_status == "rolled_back" && *schema == 2
            })
    );
}

#[sqlx::test]
async fn cancelled_apply_replays_committed_entry_without_duplicate_surface_event(pool: PgPool) {
    let first_fixture = seed_published_v2(&pool, 1).await;
    let second_fixture = seed_published_v2(&pool, 1).await;
    let mut ordered = [
        (first_fixture.entry_id, first_fixture.admin_id),
        (second_fixture.entry_id, second_fixture.admin_id),
    ];
    ordered.sort_unstable_by_key(|(entry_id, _)| *entry_id);
    let entry_ids = [ordered[0].0, ordered[1].0];
    let actor_id = ordered[0].1;
    let (batch_id, manifest_digest) = approved_manifest(&pool, actor_id, &entry_ids).await;

    let mut second_entry_gate = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE")
        .bind(entry_ids[1])
        .execute(&mut *second_entry_gate)
        .await
        .unwrap();
    let apply_pool = pool.clone();
    let apply_digest = manifest_digest.clone();
    let apply_task = tokio::spawn(async move {
        apply(
            &apply_pool,
            batch_id,
            actor_id,
            Uuid::now_v7(),
            &apply_digest,
        )
        .await
    });
    wait_for_database_lock_waiters(&pool, 1).await;
    let checkpoint: (String, i16, i16) = sqlx::query_as(
        r#"
        SELECT migration.status, first_entry.content_schema_version,
               second_entry.content_schema_version
        FROM lexicon.v3_migration_entries migration
        JOIN lexicon.entries first_entry ON first_entry.id = migration.entry_id
        JOIN lexicon.entries second_entry ON second_entry.id = $3
        WHERE migration.batch_id = $1 AND migration.entry_id = $2
        "#,
    )
    .bind(batch_id)
    .bind(entry_ids[0])
    .bind(entry_ids[1])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(checkpoint, ("applied".to_owned(), 3, 2));

    apply_task.abort();
    assert!(apply_task.await.unwrap_err().is_cancelled());
    second_entry_gate.commit().await.unwrap();
    let interrupted_batch_status: String =
        sqlx::query_scalar("SELECT status FROM lexicon.v3_migration_batches WHERE id = $1")
            .bind(batch_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(interrupted_batch_status, "approved");

    let replay = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        apply(&pool, batch_id, actor_id, Uuid::now_v7(), &manifest_digest),
    )
    .await
    .expect("cancelled apply must be replayable")
    .unwrap();
    assert_eq!(replay.applied_entries, 2);
    assert_eq!(replay.replayed_entries, 1);
    let first_surface_events: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM platform.outbox_events
        WHERE aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
          AND payload ->> 'migration_batch_id' = $2
          AND payload ->> 'transition' = 'v2_to_v3'
        "#,
    )
    .bind(entry_ids[0])
    .bind(batch_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(first_surface_events, 1);
}

#[sqlx::test]
async fn cancelled_apply_can_rollback_its_committed_checkpoint_without_finishing_the_batch(
    pool: PgPool,
) {
    let first_fixture = seed_published_v2(&pool, 1).await;
    let second_fixture = seed_published_v2(&pool, 1).await;
    let mut ordered = [
        (first_fixture.entry_id, first_fixture.admin_id),
        (second_fixture.entry_id, second_fixture.admin_id),
    ];
    ordered.sort_unstable_by_key(|(entry_id, _)| *entry_id);
    let entry_ids = [ordered[0].0, ordered[1].0];
    let actor_id = ordered[0].1;
    let (batch_id, manifest_digest) = approved_manifest(&pool, actor_id, &entry_ids).await;

    let mut second_entry_gate = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.entries WHERE id = $1 FOR UPDATE")
        .bind(entry_ids[1])
        .execute(&mut *second_entry_gate)
        .await
        .unwrap();
    let apply_pool = pool.clone();
    let apply_task = tokio::spawn(async move {
        apply(
            &apply_pool,
            batch_id,
            actor_id,
            Uuid::now_v7(),
            &manifest_digest,
        )
        .await
    });
    wait_for_database_lock_waiters(&pool, 1).await;
    let checkpoint: (String, i16, i16) = sqlx::query_as(
        r#"
        SELECT migration.status, first_entry.content_schema_version,
               second_entry.content_schema_version
        FROM lexicon.v3_migration_entries migration
        JOIN lexicon.entries first_entry ON first_entry.id = migration.entry_id
        JOIN lexicon.entries second_entry ON second_entry.id = $3
        WHERE migration.batch_id = $1 AND migration.entry_id = $2
        "#,
    )
    .bind(batch_id)
    .bind(entry_ids[0])
    .bind(entry_ids[1])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(checkpoint, ("applied".to_owned(), 3, 2));
    apply_task.abort();
    assert!(apply_task.await.unwrap_err().is_cancelled());
    second_entry_gate.commit().await.unwrap();

    let rollback_request_id = Uuid::now_v7();
    let report = rollback(&pool, batch_id, actor_id, rollback_request_id)
        .await
        .expect("an interrupted batch must be rollbackable without migration continuation");
    assert_eq!(report.rolled_back_entries, 1);
    assert_eq!(report.entries[0].entry_id, entry_ids[0]);

    let replay = rollback(&pool, batch_id, actor_id, rollback_request_id)
        .await
        .expect("a lost rollback response must replay the stored result");
    assert_eq!(replay.rolled_back_entries, report.rolled_back_entries);
    assert_eq!(replay.entries[0].entry_id, report.entries[0].entry_id);
    assert_eq!(replay.entries[0].digest, report.entries[0].digest);
    let conflict = rollback(&pool, batch_id, actor_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(
        conflict
            .to_string()
            .contains("migration_rollback_idempotency_conflict")
    );

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, i16)>(
        r#"
        SELECT migration.entry_id, migration.status, migration.failure_code,
               entry.content_schema_version
        FROM lexicon.v3_migration_entries migration
        JOIN lexicon.entries entry ON entry.id = migration.entry_id
        WHERE migration.batch_id = $1
        ORDER BY migration.entry_id
        "#,
    )
    .bind(batch_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], (entry_ids[0], "rolled_back".to_owned(), None, 2));
    assert_eq!(rows[1].0, entry_ids[1]);
    assert_eq!(rows[1].1, "failed");
    assert_eq!(rows[1].2.as_deref(), Some("rollback_aborted_unapplied"));
    assert_eq!(rows[1].3, 2);
    let (batch_status, audit_count, audit_request): (String, i64, Uuid) = sqlx::query_as(
        r#"
        SELECT batch.status,
               (SELECT count(*) FROM audit.admin_actions counted
                WHERE counted.action = 'lexicon.migration_batch.rollback'
                  AND counted.resource_type = 'lexicon.migration_batch'
                  AND counted.resource_id = batch.id),
               audit.request_id
        FROM lexicon.v3_migration_batches batch
        JOIN audit.admin_actions audit
          ON audit.action = 'lexicon.migration_batch.rollback'
         AND audit.resource_type = 'lexicon.migration_batch'
         AND audit.resource_id = batch.id
        WHERE batch.id = $1
        "#,
    )
    .bind(batch_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(batch_status, "rolled_back");
    assert_eq!(audit_count, 1);
    assert_eq!(audit_request, rollback_request_id);
}

#[sqlx::test]
async fn apply_is_concurrently_idempotent_and_preserves_v2_publication(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 2).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    let request_id = Uuid::now_v7();
    let first_pool = pool.clone();
    let second_pool = pool.clone();
    let first = apply(
        &first_pool,
        batch_id,
        fixture.admin_id,
        request_id,
        &manifest_digest,
    );
    let second = apply(
        &second_pool,
        batch_id,
        fixture.admin_id,
        request_id,
        &manifest_digest,
    );
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.failed_entries + second.failed_entries, 0);
    assert_eq!(first.applied_entries, 1);
    assert_eq!(second.applied_entries, 1);
    assert_eq!(first.replayed_entries + second.replayed_entries, 1);

    let (schema_version, current_publication_id): (i16, Option<Uuid>) = sqlx::query_as(
        "SELECT content_schema_version, current_publication_id FROM lexicon.entries WHERE id = $1",
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(schema_version, 3);
    assert_eq!(current_publication_id, Some(fixture.publication_id));
    let base_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lexicon.v3_concrete_forms WHERE entry_id = $1 AND id = $2 AND form_type = 'base'",
    )
    .bind(fixture.entry_id)
    .bind(fixture.base_form_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let base_memberships: Vec<Uuid> = sqlx::query_scalar(
        "SELECT form_group_id FROM lexicon.v3_group_memberships WHERE entry_id = $1 AND form_id = $2 ORDER BY form_group_id",
    )
    .bind(fixture.entry_id)
    .bind(fixture.base_form_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        base_count, 1,
        "multi-group migration must not copy the base form"
    );
    assert_eq!(
        base_memberships
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        fixture.group_ids.iter().copied().collect()
    );
    let unstable_variants: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM lexicon.v3_form_variants variant
        JOIN lexicon.nodes node ON node.id = variant.id AND node.entry_id = variant.entry_id
        WHERE variant.entry_id = $1 AND node.stable_slot = FALSE
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        unstable_variants, 0,
        "V3 regional variants stay stable nodes"
    );
    let (active_v2_surfaces, active_v3_surfaces): (i64, i64) = sqlx::query_as(
        r#"
        SELECT count(*) FILTER (
                   WHERE content_schema_version = 2 AND is_deleted = FALSE
               ),
               count(*) FILTER (
                   WHERE content_schema_version = 3 AND is_deleted = FALSE
               )
        FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_v2_surfaces, 0);
    assert!(active_v3_surfaces > 0);
    let (surface_event_revision, surface_event): (i64, Value) = sqlx::query_as(
        r#"
        SELECT aggregate_revision, payload
        FROM platform.outbox_events
        WHERE aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
          AND payload ->> 'migration_batch_id' = $2
          AND payload ->> 'transition' = 'v2_to_v3'
        "#,
    )
    .bind(fixture.entry_id)
    .bind(batch_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(surface_event["content_schema_version"], 3);
    assert_eq!(surface_event["source_count"], active_v3_surfaces);
    assert_eq!(surface_event["event_offset"], surface_event_revision);
    let distinct_transition_offsets: i64 = sqlx::query_scalar(
        r#"
        SELECT count(DISTINCT event_offset)
        FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        distinct_transition_offsets, 1,
        "one migration replacement must use one event offset"
    );
    let (resource_type, metadata): (String, Value) = sqlx::query_as(
        r#"
        SELECT resource_type, metadata
        FROM audit.admin_actions
        WHERE resource_id = $1 AND action = 'lexicon.entry.migrate.v2_to_v3'
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(resource_type, "lexicon.entry");
    assert!(!metadata["changed_node_ids"].as_array().unwrap().is_empty());
    assert!(
        !metadata["generated_node_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(metadata["retired_node_ids"].as_array().unwrap().is_empty());
    assert_publication_unchanged(&pool, &fixture).await;
    let unverified_canary = enable_publication_canary(
        &pool,
        batch_id,
        fixture.entry_id,
        fixture.admin_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(
        unverified_canary
            .to_string()
            .contains("migration_canary_not_eligible")
    );
    let verify_request_id = Uuid::now_v7();
    let verification = verify(&pool, batch_id, fixture.admin_id, verify_request_id)
        .await
        .unwrap();
    assert!(verification.ready);
    assert_eq!(verification.verified_entries, 1);
    let verify_replay = verify(&pool, batch_id, fixture.admin_id, verify_request_id)
        .await
        .expect("same terminal verify command must replay after response loss");
    assert_eq!(verify_replay.checked_entries, verification.checked_entries);
    assert_eq!(
        verify_replay.verified_entries,
        verification.verified_entries
    );
    assert_eq!(verify_replay.ready, verification.ready);
    assert_eq!(
        verify_replay.entries[0].digest,
        verification.entries[0].digest
    );
    let (verify_actor, verify_request, verify_metadata, verify_audit_count): (
        Uuid,
        Uuid,
        Value,
        i64,
    ) = sqlx::query_as(
        r#"
        SELECT audit.actor_admin_id, audit.request_id, audit.metadata,
               (SELECT count(*) FROM audit.admin_actions counted
                WHERE counted.action = 'lexicon.migration_batch.verify'
                  AND counted.resource_type = 'lexicon.migration_batch'
                  AND counted.resource_id = $1)
        FROM audit.admin_actions audit
        WHERE audit.action = 'lexicon.migration_batch.verify'
          AND audit.resource_type = 'lexicon.migration_batch'
          AND audit.resource_id = $1
        "#,
    )
    .bind(batch_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(verify_actor, fixture.admin_id);
    assert_eq!(verify_request, verify_request_id);
    assert_eq!(verify_metadata["ready"], true);
    assert_eq!(verify_metadata["verified_entries"], 1);
    assert_eq!(verify_audit_count, 1);
    let canary_request_id = Uuid::now_v7();
    let canary = enable_publication_canary(
        &pool,
        batch_id,
        fixture.entry_id,
        fixture.admin_id,
        canary_request_id,
    )
    .await
    .unwrap();
    assert!(!canary.replayed);
    let replay = enable_publication_canary(
        &pool,
        batch_id,
        fixture.entry_id,
        fixture.admin_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(replay.replayed);
    let (canary_enabled, audit_actor, audit_request, audit_count): (bool, Uuid, Uuid, i64) =
        sqlx::query_as(
            r#"
        SELECT state.publication_canary_enabled, audit.actor_admin_id,
               audit.request_id,
               (SELECT count(*) FROM audit.admin_actions counted
                WHERE counted.resource_id = state.entry_id
                  AND counted.action = 'lexicon.entry.enable_v3_publication_canary')
        FROM lexicon.v3_entry_state state
        JOIN audit.admin_actions audit
          ON audit.resource_id = state.entry_id
         AND audit.action = 'lexicon.entry.enable_v3_publication_canary'
        WHERE state.entry_id = $1 AND audit.request_id = $2
        "#,
        )
        .bind(fixture.entry_id)
        .bind(canary_request_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(canary_enabled);
    assert_eq!(audit_actor, fixture.admin_id);
    assert_eq!(audit_request, canary_request_id);
    assert_eq!(audit_count, 1);
}

#[sqlx::test]
async fn verify_rejects_a_missing_migration_surface_event(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 1).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();
    sqlx::query(
        r#"
        DELETE FROM platform.outbox_events
        WHERE aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
          AND payload ->> 'migration_batch_id' = $2
          AND payload ->> 'transition' = 'v2_to_v3'
        "#,
    )
    .bind(fixture.entry_id)
    .bind(batch_id.to_string())
    .execute(&pool)
    .await
    .unwrap();

    let report = verify(&pool, batch_id, fixture.admin_id, Uuid::now_v7())
        .await
        .unwrap();
    assert!(!report.ready);
    assert_eq!(report.verified_entries, 0);
    assert!(
        report.entries[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("migration_surface_outbox_mismatch")),
        "{report:?}"
    );
    let canary = enable_publication_canary(
        &pool,
        batch_id,
        fixture.entry_id,
        fixture.admin_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(canary.to_string().contains("migration_canary_not_eligible"));
}

#[sqlx::test]
async fn rollback_restores_v2_before_first_write_and_preserves_history(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 0).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();
    let report = rollback(&pool, batch_id, fixture.admin_id, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(report.rolled_back_entries, 1);
    let (schema_version, forms): (i16, Value) = sqlx::query_as(
        r#"
        SELECT entry.content_schema_version, projection.forms
        FROM lexicon.entries entry
        JOIN lexicon.entry_editor_projection projection ON projection.entry_id = entry.id
        WHERE entry.id = $1
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(schema_version, 2);
    assert_eq!(forms, fixture.source_forms);
    assert_publication_unchanged(&pool, &fixture).await;
    let v3_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lexicon.v3_entry_state WHERE entry_id = $1")
            .bind(fixture.entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(v3_rows, 0);
    let (active_v2_surfaces, v3_surfaces): (i64, i64) = sqlx::query_as(
        r#"
        SELECT count(*) FILTER (
                   WHERE content_schema_version = 2 AND is_deleted = FALSE
               ),
               count(*) FILTER (WHERE content_schema_version = 3)
        FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_v2_surfaces, 2);
    assert_eq!(v3_surfaces, 0);
    let (surface_event_revision, surface_event): (i64, Value) = sqlx::query_as(
        r#"
        SELECT aggregate_revision, payload
        FROM platform.outbox_events
        WHERE aggregate_id = $1
          AND event_type = 'lexicon.surface_projection_replaced'
          AND payload ->> 'migration_batch_id' = $2
          AND payload ->> 'transition' = 'v3_to_v2'
        "#,
    )
    .bind(fixture.entry_id)
    .bind(batch_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(surface_event["content_schema_version"], 2);
    assert_eq!(surface_event["source_count"], active_v2_surfaces);
    assert_eq!(surface_event["event_offset"], surface_event_revision);
    let active_v2_offset_count: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM lexicon.surface_sources
        WHERE entry_id = $1 AND content_scope = 'draft'
          AND content_schema_version = 2 AND is_deleted = FALSE
          AND event_offset = $2
        "#,
    )
    .bind(fixture.entry_id)
    .bind(surface_event_revision)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_v2_offset_count, active_v2_surfaces);
    let metadata: Value = sqlx::query_scalar(
        r#"
        SELECT metadata FROM audit.admin_actions
        WHERE resource_id = $1 AND action = 'lexicon.entry.rollback.v3_migration'
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!metadata["retired_node_ids"].as_array().unwrap().is_empty());
    let rolled_back_canary = enable_publication_canary(
        &pool,
        batch_id,
        fixture.entry_id,
        fixture.admin_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(
        rolled_back_canary
            .to_string()
            .contains("migration_canary_not_eligible")
    );
}

#[sqlx::test]
async fn rollback_locks_entries_before_the_batch_row(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 1).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();

    let mut entry_gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.entry:{}", fixture.entry_id))
        .execute(&mut *entry_gate)
        .await
        .unwrap();
    let rollback_pool = pool.clone();
    let rollback_task = tokio::spawn(async move {
        rollback(&rollback_pool, batch_id, fixture.admin_id, Uuid::now_v7()).await
    });
    wait_for_advisory_lock_waiters(&pool, 1).await;

    sqlx::query("SET LOCAL lock_timeout = '1s'")
        .execute(&mut *entry_gate)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM lexicon.v3_migration_batches WHERE id = $1 FOR UPDATE")
        .bind(batch_id)
        .execute(&mut *entry_gate)
        .await
        .expect("entry-first canary/publish lock order must not wait on rollback's batch row");
    entry_gate.commit().await.unwrap();

    let report = tokio::time::timeout(std::time::Duration::from_secs(5), rollback_task)
        .await
        .expect("rollback must finish without a deadlock")
        .unwrap()
        .unwrap();
    assert_eq!(report.rolled_back_entries, 1);
}

#[sqlx::test]
async fn verify_and_rollback_are_serialized_without_resurrecting_the_batch(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 1).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();

    let mut entry_gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("lexicon.v3-migration.entry:{}", fixture.entry_id))
        .execute(&mut *entry_gate)
        .await
        .unwrap();
    let admin_id = fixture.admin_id;
    let verify_pool = pool.clone();
    let verify_task =
        tokio::spawn(async move { verify(&verify_pool, batch_id, admin_id, Uuid::now_v7()).await });
    let rollback_pool = pool.clone();
    let rollback_task =
        tokio::spawn(
            async move { rollback(&rollback_pool, batch_id, admin_id, Uuid::now_v7()).await },
        );
    wait_for_advisory_lock_waiters(&pool, 2).await;
    entry_gate.commit().await.unwrap();

    let verify_result = tokio::time::timeout(std::time::Duration::from_secs(5), verify_task)
        .await
        .expect("verify must not deadlock")
        .unwrap();
    let rollback_report = tokio::time::timeout(std::time::Duration::from_secs(5), rollback_task)
        .await
        .expect("rollback must not deadlock")
        .unwrap()
        .unwrap();
    assert_eq!(rollback_report.rolled_back_entries, 1);
    match verify_result {
        Ok(report) => assert!(report.ready, "{report:?}"),
        Err(error) => assert!(
            error.to_string().contains("migration_batch_not_verifiable"),
            "{error:#}"
        ),
    }
    let (batch_status, entry_status, content_schema_version): (String, String, i16) =
        sqlx::query_as(
            r#"
            SELECT batch.status, migration.status, entry.content_schema_version
            FROM lexicon.v3_migration_batches batch
            JOIN lexicon.v3_migration_entries migration ON migration.batch_id = batch.id
            JOIN lexicon.entries entry ON entry.id = migration.entry_id
            WHERE batch.id = $1 AND migration.entry_id = $2
            "#,
        )
        .bind(batch_id)
        .bind(fixture.entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(batch_status, "rolled_back");
    assert_eq!(entry_status, "rolled_back");
    assert_eq!(content_schema_version, 2);
}

#[sqlx::test]
async fn apply_replay_cannot_resurrect_a_rolled_back_batch(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 1).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();

    let mut batch_gate = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM lexicon.v3_migration_batches WHERE id = $1 FOR UPDATE")
        .bind(batch_id)
        .execute(&mut *batch_gate)
        .await
        .unwrap();
    let replay_pool = pool.clone();
    let replay_digest = manifest_digest.clone();
    let admin_id = fixture.admin_id;
    let replay_task = tokio::spawn(async move {
        apply(
            &replay_pool,
            batch_id,
            admin_id,
            Uuid::now_v7(),
            &replay_digest,
        )
        .await
    });
    wait_for_database_lock_waiters(&pool, 1).await;
    let rollback_pool = pool.clone();
    let rollback_task =
        tokio::spawn(
            async move { rollback(&rollback_pool, batch_id, admin_id, Uuid::now_v7()).await },
        );
    wait_for_database_lock_waiters(&pool, 2).await;
    batch_gate.commit().await.unwrap();

    let replay_report = tokio::time::timeout(std::time::Duration::from_secs(5), replay_task)
        .await
        .expect("apply replay must not deadlock")
        .unwrap()
        .unwrap();
    let rollback_report = tokio::time::timeout(std::time::Duration::from_secs(5), rollback_task)
        .await
        .expect("rollback must not deadlock")
        .unwrap()
        .unwrap();
    assert_eq!(rollback_report.rolled_back_entries, 1);
    assert_eq!(replay_report.replayed_entries, 1);
    let (batch_status, entry_status, content_schema_version): (String, String, i16) =
        sqlx::query_as(
            r#"
            SELECT batch.status, migration.status, entry.content_schema_version
            FROM lexicon.v3_migration_batches batch
            JOIN lexicon.v3_migration_entries migration ON migration.batch_id = batch.id
            JOIN lexicon.entries entry ON entry.id = migration.entry_id
            WHERE batch.id = $1 AND migration.entry_id = $2
            "#,
        )
        .bind(batch_id)
        .bind(fixture.entry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(batch_status, "rolled_back");
    assert_eq!(entry_status, "rolled_back");
    assert_eq!(content_schema_version, 2);
}

#[sqlx::test]
async fn rollback_fails_closed_after_first_v3_write(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 1).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE lexicon.v3_entry_state SET first_v3_write_revision = 8 WHERE entry_id = $1",
    )
    .bind(fixture.entry_id)
    .execute(&pool)
    .await
    .unwrap();
    let error = rollback(&pool, batch_id, fixture.admin_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(error.to_string().contains(ROLLBACK_BLOCKED_V3_WRITE));
    let schema_version: i16 =
        sqlx::query_scalar("SELECT content_schema_version FROM lexicon.entries WHERE id = $1")
            .bind(fixture.entry_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        schema_version, 3,
        "failed rollback must have zero side effects"
    );
    assert_publication_unchanged(&pool, &fixture).await;
}

#[sqlx::test]
async fn unpublished_v2_draft_migrates_without_inventing_a_publication(pool: PgPool) {
    let fixture = seed_published_v2(&pool, 1).await;
    sqlx::query("UPDATE lexicon.entries SET current_publication_id = NULL WHERE id = $1")
        .bind(fixture.entry_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM lexicon.entry_publications WHERE entry_id = $1")
        .bind(fixture.entry_id)
        .execute(&pool)
        .await
        .unwrap();
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    let report = apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();
    assert_eq!(report.applied_entries, 1);
    assert_eq!(report.blocked_entries, 0);
    let (current_publication_id, source_publication_id, canary): (
        Option<Uuid>,
        Option<Uuid>,
        bool,
    ) = sqlx::query_as(
        r#"
        SELECT entry.current_publication_id, state.source_publication_id,
               state.publication_canary_enabled
        FROM lexicon.entries entry
        JOIN lexicon.v3_entry_state state ON state.entry_id = entry.id
        WHERE entry.id = $1
        "#,
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(current_publication_id.is_none());
    assert!(source_publication_id.is_none());
    assert!(!canary);
    assert!(
        verify(&pool, batch_id, fixture.admin_id, Uuid::now_v7(),)
            .await
            .unwrap()
            .ready
    );
    let canary_error = enable_publication_canary(
        &pool,
        batch_id,
        fixture.entry_id,
        fixture.admin_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(
        canary_error
            .to_string()
            .contains("migration_canary_not_eligible")
    );
    rollback(&pool, batch_id, fixture.admin_id, Uuid::now_v7())
        .await
        .unwrap();
    let restored: (i16, Option<Uuid>) = sqlx::query_as(
        "SELECT content_schema_version, current_publication_id FROM lexicon.entries WHERE id = $1",
    )
    .bind(fixture.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(restored, (2, None));
}

#[sqlx::test]
async fn verify_never_hides_blocked_entries_in_a_mixed_batch(pool: PgPool) {
    let good = seed_published_v2(&pool, 1).await;
    let blocked = seed_published_v2(&pool, 1).await;
    let mut invalid_forms = blocked.source_forms.clone();
    invalid_forms["pos"][0]["base_form"]["variants"] = serde_json::json!([]);
    sqlx::query("UPDATE lexicon.entry_editor_projection SET forms = $2 WHERE entry_id = $1")
        .bind(blocked.entry_id)
        .bind(invalid_forms)
        .execute(&pool)
        .await
        .unwrap();
    let entry_ids = [good.entry_id, blocked.entry_id];
    let (batch_id, manifest_digest) = approved_manifest(&pool, good.admin_id, &entry_ids).await;
    let report = apply(
        &pool,
        batch_id,
        good.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();
    assert_eq!(report.applied_entries, 1);
    assert_eq!(report.blocked_entries, 1);

    let verification = verify(&pool, batch_id, good.admin_id, Uuid::now_v7())
        .await
        .unwrap();
    assert!(!verification.ready);
    assert_eq!(verification.checked_entries, 2);
    assert!(
        verification
            .entries
            .iter()
            .any(|entry| entry.entry_id == blocked.entry_id && entry.status == "blocked")
    );
    let partial_canary = enable_publication_canary(
        &pool,
        batch_id,
        good.entry_id,
        good.admin_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(
        partial_canary
            .to_string()
            .contains("migration_canary_not_eligible")
    );
    let canary_enabled: bool = sqlx::query_scalar(
        "SELECT publication_canary_enabled FROM lexicon.v3_entry_state WHERE entry_id = $1",
    )
    .bind(good.entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!canary_enabled);
    let (status, blocked_count, failed_count): (String, i32, i32) = sqlx::query_as(
        "SELECT status, blocked_count, failed_count FROM lexicon.v3_migration_batches WHERE id = $1",
    )
    .bind(batch_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(blocked_count, 1);
    assert_eq!(failed_count, 0, "verify must not overwrite recorded counts");
}

#[sqlx::test]
async fn migration_form_storage_uses_the_authoritative_surface_normalization(pool: PgPool) {
    let fixture = seed_published_v2_with_surface(&pool, 0, Some("It’s Well—Known")).await;
    let (batch_id, manifest_digest) =
        approved_manifest(&pool, fixture.admin_id, &[fixture.entry_id]).await;
    let report = apply(
        &pool,
        batch_id,
        fixture.admin_id,
        Uuid::now_v7(),
        &manifest_digest,
    )
    .await
    .unwrap();
    assert_eq!(report.applied_entries, 1);
    assert_eq!(report.failed_entries, 0);

    let canonical_and_surface: (String, String, i16, String, String, i16) = sqlx::query_as(
        r#"
        SELECT variant.spelling, variant.normalized_spelling,
               variant.normalization_version, source.surface,
               source.normalized_surface, source.normalization_version
        FROM lexicon.v3_form_variants variant
        JOIN lexicon.surface_sources source
          ON source.entry_id = variant.entry_id
         AND source.source_node_id = variant.id
         AND source.content_schema_version = 3
         AND source.content_scope = 'draft'
         AND source.is_deleted = FALSE
        WHERE variant.entry_id = $1 AND variant.form_id = $2
        ORDER BY source.dialect_scope
        LIMIT 1
        "#,
    )
    .bind(fixture.entry_id)
    .bind(fixture.base_form_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        canonical_and_surface,
        (
            "It’s Well—Known".to_owned(),
            "it's well-known".to_owned(),
            1,
            "It’s Well—Known".to_owned(),
            "it's well-known".to_owned(),
            1,
        ),
        "V2→V3 conversion and V3 surface projection must share normalization v1"
    );
    assert_publication_unchanged(&pool, &fixture).await;
}

#[sqlx::test]
async fn publication_canary_whitelist_rejects_native_v3_entries(pool: PgPool) {
    let admin_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', 'native V3')",
    )
    .bind(admin_id)
    .bind(format!("native-v3-{}", admin_id.simple()))
    .execute(&pool)
    .await
    .unwrap();
    let entry_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO lexicon.entries (
            id, content_schema_version, language, kind, revision,
            headword_mode, source_dialect, detection_snapshot,
            created_by_admin_id, updated_by_admin_id
        ) VALUES ($1, 3, 'en', 'word', 1, NULL, NULL, '{}', $2, $2)
        "#,
    )
    .bind(entry_id)
    .bind(admin_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO lexicon.v3_entry_state (entry_id, origin) VALUES ($1, 'native')")
        .bind(entry_id)
        .execute(&pool)
        .await
        .unwrap();
    let result = sqlx::query(
        "UPDATE lexicon.v3_entry_state SET publication_canary_enabled = TRUE WHERE entry_id = $1",
    )
    .bind(entry_id)
    .execute(&pool)
    .await;
    let error = result.unwrap_err();
    let error = error.as_database_error().unwrap().constraint();
    assert_eq!(error, Some("lexicon_v3_entry_state_origin_shape_check"));
}
