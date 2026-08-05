//! dictionary schema constraints for the read-only built-in reference dictionary.

use sqlx::PgPool;

async fn insert_dataset(pool: &PgPool, version: &str, status: &str) -> i64 {
    sqlx::query_scalar(
        r#"
        INSERT INTO dictionary.datasets (
            version, source_name, source_version, rules_version,
            terms_sha256, regions_sha256, status
        )
        VALUES ($1, 'Kaikki', 'source-v1', 'rules-v1', 'terms-hash', 'regions-hash', $2)
        RETURNING id
        "#,
    )
    .bind(version)
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("dataset should insert")
}

#[sqlx::test]
async fn permits_only_one_active_dataset(pool: PgPool) {
    insert_dataset(&pool, "v1", "active").await;
    let second = sqlx::query(
        r#"
        INSERT INTO dictionary.datasets (
            version, source_name, source_version, rules_version,
            terms_sha256, regions_sha256, status
        )
        VALUES ('v2', 'Kaikki', 'source-v2', 'rules-v1', 'a', 'b', 'active')
        "#,
    )
    .execute(&pool)
    .await;
    assert!(second.is_err(), "a second active dataset must be rejected");
}

#[sqlx::test]
async fn rejects_unknown_term_region_family(pool: PgPool) {
    let dataset_id = insert_dataset(&pool, "v1", "importing").await;
    let result = sqlx::query(
        r#"
        INSERT INTO dictionary.terms (
            dataset_id, normalized_term, term, kind, pos, status,
            sense_count, filtered_cold_sense_count, region_family
        )
        VALUES ($1, 'colour', 'colour', 'word', ARRAY['noun'], 'accepted', 1, 0, 'mars')
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await;
    assert!(result.is_err(), "unknown region family must fail its CHECK");
}

#[sqlx::test]
async fn preserves_original_region_evidence_and_cascades_dataset(pool: PgPool) {
    let dataset_id = insert_dataset(&pool, "v1", "active").await;
    sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        )
        VALUES (
            $1, 'colour', 'colour', 'british_influenced',
            ARRAY['british_influenced'], ARRAY['Commonwealth', 'Ireland'],
            ARRAY['spelling'], ARRAY['noun'], ARRAY['color'], true
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .expect("region surface should insert");

    sqlx::query(
        r#"
        INSERT INTO dictionary.region_evidence (
            dataset_id, normalized_term, evidence_type,
            original_region_tags, raw_tags, pos, targets
        )
        VALUES (
            $1, 'colour', 'spelling', ARRAY['Commonwealth', 'Ireland'],
            ARRAY['Commonwealth', 'Ireland', 'alternative'], 'noun', ARRAY['color']
        )
        "#,
    )
    .bind(dataset_id)
    .execute(&pool)
    .await
    .expect("original evidence should insert");

    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT original_region_tags FROM dictionary.region_evidence WHERE dataset_id = $1",
    )
    .bind(dataset_id)
    .fetch_one(&pool)
    .await
    .expect("evidence should read back");
    assert_eq!(tags, ["Commonwealth", "Ireland"]);

    sqlx::query("DELETE FROM dictionary.datasets WHERE id = $1")
        .bind(dataset_id)
        .execute(&pool)
        .await
        .expect("dataset delete should cascade");
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM dictionary.region_evidence")
        .fetch_one(&pool)
        .await
        .expect("count should succeed");
    assert_eq!(remaining, 0);
}
