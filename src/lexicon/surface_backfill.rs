use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::lexicon::{
    dto::{AdminWordStatus, AdminWordV2},
    repository::{
        LexiconRepository, SurfaceContentScope, SurfaceProjectionSource, surface_projection_sources,
    },
    service::entry_from_record,
    surface_policy::{SurfaceCreationPolicy, SurfacePolicyStore},
};

pub const SURFACE_WRITER_VERSION: &str = "surface-writer-v1";

pub fn surface_cutover_artifact_sha256() -> String {
    checksum_bytes(
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/ops/lexicon-surface-cutover/20260816_drop_cross_entry_headword_unique.sql"
        ))
        .as_bytes(),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct SurfaceBackfillReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub writer_version: &'static str,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub start_watermark: i64,
    pub high_watermark: i64,
    pub scanned_entries: usize,
    pub changed_entries: usize,
    pub parity: SurfaceParityReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct SurfaceParityReport {
    pub expected_active_rows: usize,
    pub actual_active_rows: usize,
    pub missing_rows: Vec<SurfaceRowIdentity>,
    pub orphan_rows: Vec<SurfaceRowIdentity>,
    pub mismatched_rows: Vec<SurfaceRowIdentity>,
    pub counts: Vec<SurfaceParityCount>,
    pub expected_checksum: String,
    pub actual_checksum: String,
    pub outbox_lag: i64,
    pub ready: bool,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SurfaceCutoverReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub writer_version: &'static str,
    pub artifact_sha256: String,
    pub parity: SurfaceParityReport,
    pub legacy_unique_present_before: bool,
    pub legacy_unique_present_after: bool,
    pub non_unique_lookup_present: bool,
    pub creation_policy: SurfaceCreationPolicy,
    pub publication_policy: SurfaceCreationPolicy,
    pub executed: bool,
    pub ready: bool,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SurfaceRowIdentity {
    pub source_id: String,
    pub content_scope: String,
    pub dialect_scope: String,
    pub normalization_version: i16,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SurfaceParityCount {
    pub lifecycle: String,
    pub content_scope: String,
    pub expected: usize,
    pub actual: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct CanonicalSurfaceRow {
    identity: SurfaceRowIdentity,
    entry_id: Uuid,
    source_kind: String,
    source_node_id: Option<Uuid>,
    language: String,
    entry_kind: String,
    dialect: String,
    surface: String,
    normalized_surface: String,
    source_revision: i64,
    publication_id: Option<Uuid>,
    pos_id: Option<Uuid>,
    pos: Option<String>,
    form_type: Option<String>,
    lifecycle: String,
}

#[derive(Debug, FromRow)]
struct PublicationSnapshotRow {
    publication_id: Uuid,
    source_revision: i64,
    snapshot: serde_json::Value,
}

#[derive(Debug, FromRow)]
struct StoredSurfaceRow {
    entry_id: Uuid,
    source_id: String,
    source_kind: String,
    source_node_id: Option<Uuid>,
    language: String,
    entry_kind: String,
    dialect: String,
    dialect_scope: String,
    surface: String,
    normalized_surface: String,
    normalization_version: i16,
    source_revision: i64,
    content_scope: String,
    publication_id: Option<Uuid>,
    pos_id: Option<Uuid>,
    pos: Option<String>,
    form_type: Option<String>,
    lifecycle: Option<String>,
}

pub async fn run_surface_backfill(pool: &PgPool) -> anyhow::Result<SurfaceBackfillReport> {
    let started_at = Utc::now();
    let start_watermark = projection_watermark(pool).await?;
    let entry_ids = sqlx::query_scalar::<_, Uuid>("SELECT id FROM lexicon.entries ORDER BY id")
        .fetch_all(pool)
        .await?;
    let mut changed_entries = 0;

    for entry_id in &entry_ids {
        let mut transaction = pool.begin().await?;
        LexiconRepository::lock_surface_policy_writer(&mut transaction).await?;
        let Some(record) =
            LexiconRepository::entry_by_id_for_update(&mut transaction, *entry_id).await?
        else {
            transaction.rollback().await?;
            continue;
        };
        let word = entry_from_record(record)?;
        let draft_sources = surface_projection_sources(&word)?;
        let publication = current_publication(&mut transaction, *entry_id).await?;
        let lifecycle = lifecycle(&word).to_owned();

        let expected_draft =
            canonical_sources(&draft_sources, "draft", None, word.revision, &lifecycle);
        let actual_draft = active_entry_scope(&mut transaction, *entry_id, "draft").await?;
        let draft_changed = expected_draft != actual_draft;
        if draft_changed {
            LexiconRepository::replace_surface_projection(
                &mut transaction,
                *entry_id,
                word.revision,
                SurfaceContentScope::Draft,
                None,
                &[],
                &draft_sources,
            )
            .await?;
        }

        let publication_changed = if let Some(publication) = publication {
            let published_word: AdminWordV2 = serde_json::from_value(publication.snapshot)?;
            let publication_sources = surface_projection_sources(&published_word)?;
            let expected = canonical_sources(
                &publication_sources,
                "current_publication",
                Some(publication.publication_id),
                publication.source_revision,
                &lifecycle,
            );
            let actual =
                active_entry_scope(&mut transaction, *entry_id, "current_publication").await?;
            if expected != actual {
                LexiconRepository::replace_surface_projection(
                    &mut transaction,
                    *entry_id,
                    publication.source_revision,
                    SurfaceContentScope::CurrentPublication(publication.publication_id),
                    Some(publication.publication_id),
                    &[],
                    &publication_sources,
                )
                .await?;
                true
            } else {
                false
            }
        } else {
            let unexpected =
                active_entry_scope(&mut transaction, *entry_id, "current_publication").await?;
            anyhow::ensure!(
                unexpected.is_empty(),
                "entry {entry_id} has active current_publication surfaces without a current publication"
            );
            false
        };

        if draft_changed || publication_changed {
            changed_entries += 1;
        }
        transaction.commit().await?;
    }

    let parity = run_surface_parity(pool).await?;
    Ok(SurfaceBackfillReport {
        schema_version: 1,
        mode: "backfill",
        writer_version: SURFACE_WRITER_VERSION,
        started_at,
        finished_at: Utc::now(),
        start_watermark,
        high_watermark: projection_watermark(pool).await?,
        scanned_entries: entry_ids.len(),
        changed_entries,
        parity,
    })
}

pub async fn run_surface_parity(pool: &PgPool) -> anyhow::Result<SurfaceParityReport> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('lexicon.surface-policy-writer', 0))",
    )
    .execute(&mut *transaction)
    .await?;

    let report = surface_parity_in_transaction(&mut transaction).await?;
    transaction.commit().await?;
    Ok(report)
}

pub async fn run_surface_cutover_preflight(
    pool: &PgPool,
    policies: &SurfacePolicyStore,
    expected_writer_version: &str,
) -> anyhow::Result<SurfaceCutoverReport> {
    run_surface_cutover(pool, policies, expected_writer_version, None).await
}

pub async fn execute_surface_cutover(
    pool: &PgPool,
    policies: &SurfacePolicyStore,
    expected_writer_version: &str,
    confirmed_artifact_sha256: &str,
) -> anyhow::Result<SurfaceCutoverReport> {
    let artifact = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/ops/lexicon-surface-cutover/20260816_drop_cross_entry_headword_unique.sql"
    ));
    let artifact_sha256 = surface_cutover_artifact_sha256();
    anyhow::ensure!(
        artifact_sha256 == confirmed_artifact_sha256,
        "cutover artifact hash mismatch: expected {confirmed_artifact_sha256}, actual {artifact_sha256}"
    );
    run_surface_cutover(pool, policies, expected_writer_version, Some(artifact)).await
}

async fn run_surface_cutover(
    pool: &PgPool,
    policies: &SurfacePolicyStore,
    expected_writer_version: &str,
    artifact: Option<&'static str>,
) -> anyhow::Result<SurfaceCutoverReport> {
    let artifact_text = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/ops/lexicon-surface-cutover/20260816_drop_cross_entry_headword_unique.sql"
    ));
    let artifact_sha256 = checksum_bytes(artifact_text.as_bytes());
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('lexicon.surface-policy-writer', 0))",
    )
    .execute(&mut *transaction)
    .await?;
    let creation_policy = policies.exact_headword_creation().await?;
    let publication_policy = policies
        .multiple_active_exact_headword_publications()
        .await?;
    let parity = surface_parity_in_transaction(&mut transaction).await?;
    let legacy_unique_present_before = index_exists(
        &mut transaction,
        "lexicon.lexicon_entry_headword_keys_unique_idx",
    )
    .await?;
    let non_unique_lookup_present = index_exists(
        &mut transaction,
        "lexicon.lexicon_entry_headword_keys_lookup_idx",
    )
    .await?;
    let mut blocking_reasons = parity.blocking_reasons.clone();
    if expected_writer_version != SURFACE_WRITER_VERSION {
        blocking_reasons.push("writer_version_mismatch".to_owned());
    }
    if !non_unique_lookup_present {
        blocking_reasons.push("non_unique_lookup_index_missing".to_owned());
    }
    if !legacy_unique_present_before {
        blocking_reasons.push("legacy_unique_index_missing".to_owned());
    }
    if creation_policy.enabled {
        blocking_reasons.push("exact_headword_creation_policy_enabled".to_owned());
    }
    if publication_policy.enabled {
        blocking_reasons
            .push("multiple_active_exact_headword_publications_policy_enabled".to_owned());
    }

    let ready = blocking_reasons.is_empty();
    let executed = artifact.is_some() && ready;
    if let Some(artifact) = artifact {
        anyhow::ensure!(
            ready,
            "surface cutover blocked: {}",
            blocking_reasons.join(",")
        );
        sqlx::raw_sql(artifact).execute(&mut *transaction).await?;
    }
    let legacy_unique_present_after = index_exists(
        &mut transaction,
        "lexicon.lexicon_entry_headword_keys_unique_idx",
    )
    .await?;
    if executed {
        anyhow::ensure!(
            !legacy_unique_present_after,
            "cutover artifact did not remove the legacy unique index"
        );
    }
    transaction.commit().await?;

    Ok(SurfaceCutoverReport {
        schema_version: 1,
        mode: if executed { "cutover" } else { "preflight" },
        writer_version: SURFACE_WRITER_VERSION,
        artifact_sha256,
        parity,
        legacy_unique_present_before,
        legacy_unique_present_after,
        non_unique_lookup_present,
        creation_policy,
        publication_policy,
        executed,
        ready,
        blocking_reasons,
    })
}

async fn surface_parity_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<SurfaceParityReport> {
    let entry_ids = sqlx::query_scalar::<_, Uuid>("SELECT id FROM lexicon.entries ORDER BY id")
        .fetch_all(&mut **transaction)
        .await?;
    let mut expected = Vec::new();
    for entry_id in entry_ids {
        let Some(record) =
            LexiconRepository::entry_by_id_for_update(&mut *transaction, entry_id).await?
        else {
            continue;
        };
        let word = entry_from_record(record)?;
        let lifecycle = lifecycle(&word);
        expected.extend(canonical_sources(
            &surface_projection_sources(&word)?,
            "draft",
            None,
            word.revision,
            lifecycle,
        ));
        if let Some(publication) = current_publication(&mut *transaction, entry_id).await? {
            let published_word: AdminWordV2 = serde_json::from_value(publication.snapshot)?;
            expected.extend(canonical_sources(
                &surface_projection_sources(&published_word)?,
                "current_publication",
                Some(publication.publication_id),
                publication.source_revision,
                lifecycle,
            ));
        }
    }
    expected.sort();
    let mut actual = all_active_surfaces(&mut *transaction).await?;
    actual.sort();
    let outbox_lag = surface_outbox_lag(&mut *transaction).await?;
    Ok(compare_surface_rows(expected, actual, outbox_lag))
}

async fn index_exists(
    transaction: &mut Transaction<'_, Postgres>,
    qualified_name: &str,
) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
            .bind(qualified_name)
            .fetch_one(&mut **transaction)
            .await?,
    )
}

async fn current_publication(
    transaction: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> anyhow::Result<Option<PublicationSnapshotRow>> {
    Ok(sqlx::query_as::<_, PublicationSnapshotRow>(
        r#"
        SELECT publication.id AS publication_id,
               publication.source_revision,
               publication.snapshot
        FROM lexicon.entries entry
        JOIN lexicon.entry_publications publication
          ON publication.id = entry.current_publication_id
         AND publication.entry_id = entry.id
        WHERE entry.id = $1
        "#,
    )
    .bind(entry_id)
    .fetch_optional(&mut **transaction)
    .await?)
}

async fn active_entry_scope(
    transaction: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    content_scope: &str,
) -> anyhow::Result<Vec<CanonicalSurfaceRow>> {
    let stored = sqlx::query_as::<_, StoredSurfaceRow>(
        r#"
        SELECT source.entry_id, source.source_id, source.source_kind,
               source.source_node_id, source.language, source.entry_kind,
               source.dialect, source.dialect_scope, source.surface,
               source.normalized_surface, source.normalization_version,
               source.source_revision, source.content_scope,
               source.publication_id, source.pos_id, source.pos,
               source.form_type,
               CASE
                 WHEN entry.archived_at IS NOT NULL THEN 'archived'
                 WHEN entry.current_publication_id IS NOT NULL THEN 'published'
                 WHEN entry.id IS NOT NULL THEN 'draft'
                 ELSE NULL
               END AS lifecycle
        FROM lexicon.surface_sources source
        LEFT JOIN lexicon.entries entry ON entry.id = source.entry_id
        WHERE source.is_deleted = FALSE
          AND source.entry_id = $1
          AND source.content_scope = $2
        ORDER BY source.source_id, source.content_scope,
                 source.dialect_scope, source.normalization_version
        "#,
    )
    .bind(entry_id)
    .bind(content_scope)
    .fetch_all(&mut **transaction)
    .await?;
    Ok(stored.into_iter().map(canonical_stored).collect())
}

async fn all_active_surfaces(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<Vec<CanonicalSurfaceRow>> {
    Ok(sqlx::query_as::<_, StoredSurfaceRow>(
        r#"
        SELECT source.entry_id, source.source_id, source.source_kind,
               source.source_node_id, source.language, source.entry_kind,
               source.dialect, source.dialect_scope, source.surface,
               source.normalized_surface, source.normalization_version,
               source.source_revision, source.content_scope,
               source.publication_id, source.pos_id, source.pos,
               source.form_type,
               CASE
                 WHEN entry.archived_at IS NOT NULL THEN 'archived'
                 WHEN entry.current_publication_id IS NOT NULL THEN 'published'
                 WHEN entry.id IS NOT NULL THEN 'draft'
                 ELSE NULL
               END AS lifecycle
        FROM lexicon.surface_sources source
        LEFT JOIN lexicon.entries entry ON entry.id = source.entry_id
        WHERE source.is_deleted = FALSE
        ORDER BY source.source_id, source.content_scope,
                 source.dialect_scope, source.normalization_version
        "#,
    )
    .fetch_all(&mut **transaction)
    .await?
    .into_iter()
    .map(canonical_stored)
    .collect())
}

fn canonical_sources(
    sources: &[SurfaceProjectionSource],
    content_scope: &str,
    publication_id: Option<Uuid>,
    source_revision: i64,
    lifecycle: &str,
) -> Vec<CanonicalSurfaceRow> {
    let mut rows = sources
        .iter()
        .map(|source| CanonicalSurfaceRow {
            identity: SurfaceRowIdentity {
                source_id: source.source_id.clone(),
                content_scope: content_scope.to_owned(),
                dialect_scope: source.dialect_scope.to_owned(),
                normalization_version: source.normalization_version,
            },
            entry_id: source.entry_id,
            source_kind: source.source_kind.to_owned(),
            source_node_id: source.source_node_id,
            language: source.language.clone(),
            entry_kind: source.entry_kind.to_owned(),
            dialect: source.dialect.to_owned(),
            surface: source.surface.clone(),
            normalized_surface: source.normalized_surface.clone(),
            source_revision,
            publication_id,
            pos_id: source.pos_id,
            pos: source.pos.clone(),
            form_type: source.form_type.clone(),
            lifecycle: lifecycle.to_owned(),
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn canonical_stored(row: StoredSurfaceRow) -> CanonicalSurfaceRow {
    CanonicalSurfaceRow {
        identity: SurfaceRowIdentity {
            source_id: row.source_id,
            content_scope: row.content_scope,
            dialect_scope: row.dialect_scope,
            normalization_version: row.normalization_version,
        },
        entry_id: row.entry_id,
        source_kind: row.source_kind,
        source_node_id: row.source_node_id,
        language: row.language,
        entry_kind: row.entry_kind,
        dialect: row.dialect,
        surface: row.surface,
        normalized_surface: row.normalized_surface,
        source_revision: row.source_revision,
        publication_id: row.publication_id,
        pos_id: row.pos_id,
        pos: row.pos,
        form_type: row.form_type,
        lifecycle: row.lifecycle.unwrap_or_else(|| "orphan".to_owned()),
    }
}

fn compare_surface_rows(
    expected: Vec<CanonicalSurfaceRow>,
    actual: Vec<CanonicalSurfaceRow>,
    outbox_lag: i64,
) -> SurfaceParityReport {
    let expected_map = expected
        .iter()
        .cloned()
        .map(|row| (row.identity.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let actual_map = actual
        .iter()
        .cloned()
        .map(|row| (row.identity.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let missing_rows = expected_map
        .keys()
        .filter(|key| !actual_map.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let orphan_rows = actual_map
        .keys()
        .filter(|key| !expected_map.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let mismatched_rows = expected_map
        .iter()
        .filter_map(|(key, expected)| {
            actual_map
                .get(key)
                .filter(|actual| *actual != expected)
                .map(|_| key.clone())
        })
        .collect::<Vec<_>>();

    let groups = expected
        .iter()
        .chain(&actual)
        .map(|row| (row.lifecycle.clone(), row.identity.content_scope.clone()))
        .collect::<BTreeSet<_>>();
    let counts = groups
        .into_iter()
        .map(|(lifecycle, content_scope)| SurfaceParityCount {
            expected: expected
                .iter()
                .filter(|row| {
                    row.lifecycle == lifecycle && row.identity.content_scope == content_scope
                })
                .count(),
            actual: actual
                .iter()
                .filter(|row| {
                    row.lifecycle == lifecycle && row.identity.content_scope == content_scope
                })
                .count(),
            lifecycle,
            content_scope,
        })
        .collect::<Vec<_>>();

    let mut blocking_reasons = Vec::new();
    if !missing_rows.is_empty() {
        blocking_reasons.push("missing_surface_rows".to_owned());
    }
    if !orphan_rows.is_empty() {
        blocking_reasons.push("orphan_surface_rows".to_owned());
    }
    if !mismatched_rows.is_empty() {
        blocking_reasons.push("mismatched_surface_rows".to_owned());
    }
    if outbox_lag != 0 {
        blocking_reasons.push("surface_outbox_lag".to_owned());
    }

    SurfaceParityReport {
        expected_active_rows: expected.len(),
        actual_active_rows: actual.len(),
        missing_rows,
        orphan_rows,
        mismatched_rows,
        counts,
        expected_checksum: checksum(&expected),
        actual_checksum: checksum(&actual),
        outbox_lag,
        ready: blocking_reasons.is_empty(),
        blocking_reasons,
    }
}

async fn surface_outbox_lag(transaction: &mut Transaction<'_, Postgres>) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>(
        r#"
        WITH latest_event AS (
            SELECT aggregate_id AS entry_id,
                   payload->>'content_scope' AS content_scope,
                   max((payload->>'event_offset')::bigint) AS event_offset
            FROM platform.outbox_events
            WHERE aggregate_type = 'lexicon.surface_projection'
              AND event_type = 'lexicon.surface_projection_replaced'
            GROUP BY aggregate_id, payload->>'content_scope'
        ), latest_projection AS (
            SELECT entry_id, content_scope, max(event_offset) AS event_offset
            FROM lexicon.surface_sources
            GROUP BY entry_id, content_scope
        )
        SELECT count(*)
        FROM latest_event event
        FULL OUTER JOIN latest_projection projection
          ON projection.entry_id = event.entry_id
         AND projection.content_scope = event.content_scope
        WHERE event.event_offset IS DISTINCT FROM projection.event_offset
        "#,
    )
    .fetch_one(&mut **transaction)
    .await?)
}

async fn projection_watermark(pool: &PgPool) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT last_value FROM lexicon.surface_projection_event_offset_seq",
    )
    .fetch_one(pool)
    .await?)
}

fn lifecycle(word: &AdminWordV2) -> &'static str {
    match word.status {
        AdminWordStatus::Draft => "draft",
        AdminWordStatus::Published => "published",
        AdminWordStatus::Archived => "archived",
    }
}

fn checksum(rows: &[CanonicalSurfaceRow]) -> String {
    let payload = serde_json::to_vec(rows).expect("canonical surface rows must serialize");
    checksum_bytes(&payload)
}

fn checksum_bytes(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source_id: &str, lifecycle: &str) -> CanonicalSurfaceRow {
        CanonicalSurfaceRow {
            identity: SurfaceRowIdentity {
                source_id: source_id.to_owned(),
                content_scope: "draft".to_owned(),
                dialect_scope: "uk".to_owned(),
                normalization_version: 1,
            },
            entry_id: Uuid::nil(),
            source_kind: "headword".to_owned(),
            source_node_id: None,
            language: "en".to_owned(),
            entry_kind: "word".to_owned(),
            dialect: "common".to_owned(),
            surface: "workspace".to_owned(),
            normalized_surface: "workspace".to_owned(),
            source_revision: 1,
            publication_id: None,
            pos_id: None,
            pos: None,
            form_type: None,
            lifecycle: lifecycle.to_owned(),
        }
    }

    #[test]
    fn parity_is_deterministic_and_fail_closed_for_every_difference() {
        let expected = vec![row("expected", "draft"), row("changed", "published")];
        let mut changed = row("changed", "published");
        changed.surface = "changed".to_owned();
        let actual = vec![changed, row("orphan", "orphan")];

        let report = compare_surface_rows(expected.clone(), actual, 1);
        assert!(!report.ready);
        assert_eq!(report.missing_rows[0].source_id, "expected");
        assert_eq!(report.orphan_rows[0].source_id, "orphan");
        assert_eq!(report.mismatched_rows[0].source_id, "changed");
        assert_eq!(
            report.blocking_reasons,
            [
                "missing_surface_rows",
                "orphan_surface_rows",
                "mismatched_surface_rows",
                "surface_outbox_lag"
            ]
        );
        assert_ne!(report.expected_checksum, report.actual_checksum);

        let clean = compare_surface_rows(expected.clone(), expected, 0);
        assert!(clean.ready);
        assert!(clean.blocking_reasons.is_empty());
        assert_eq!(clean.expected_checksum, clean.actual_checksum);
    }
}
