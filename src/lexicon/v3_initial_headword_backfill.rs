use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::{
    dto::{
        BuiltinDictionaryEvidenceV3, DetectLexiconSurfaceResponseV3, SourceDialect,
        SuggestedRegionalVariantsV3, WordFormTypeV3, WordHeadwordsV2,
    },
    normalization::{NormalizedHeadword, normalize_headword, sha256_json},
};

#[derive(Debug, Clone, Serialize)]
pub struct InitialHeadwordBackfillBlocker {
    pub entry_id: Uuid,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InitialHeadwordBackfillReport {
    pub mode: &'static str,
    pub scanned: usize,
    pub ready: usize,
    pub applied: usize,
    pub manifest_digest: String,
    pub blockers: Vec<InitialHeadwordBackfillBlocker>,
}

#[derive(Debug)]
struct Candidate {
    entry_id: Uuid,
    entry_kind: String,
    active_hidden: bool,
    headwords: WordHeadwordsV2,
    keys: Vec<String>,
}

fn initial_keys(headwords: &WordHeadwordsV2) -> anyhow::Result<Vec<String>> {
    match headwords {
        WordHeadwordsV2::Unified { common } => {
            let key = normalize_headword(common)?.key;
            Ok(vec![format!("uk:{key}"), format!("us:{key}")])
        }
        WordHeadwordsV2::Distinguish { uk, us, .. } => Ok(vec![
            format!("uk:{}", normalize_headword(uk)?.key),
            format!("us:{}", normalize_headword(us)?.key),
        ]),
    }
}

fn derive_headwords(detection: &DetectLexiconSurfaceResponseV3) -> anyhow::Result<WordHeadwordsV2> {
    let mut candidates = BTreeSet::new();
    if let BuiltinDictionaryEvidenceV3::Matched {
        suggested_forms, ..
    } = &detection.builtin_dictionary
    {
        for form in suggested_forms
            .iter()
            .filter(|form| form.form_type == WordFormTypeV3::Base)
        {
            match &form.regional_variants {
                SuggestedRegionalVariantsV3::Common { common }
                    if !common.spelling.trim().is_empty() =>
                {
                    candidates.insert(("common", common.spelling.clone(), String::new()));
                }
                SuggestedRegionalVariantsV3::UkUs { uk, us }
                    if !uk.spelling.trim().is_empty() && !us.spelling.trim().is_empty() =>
                {
                    candidates.insert(("uk_us", uk.spelling.clone(), us.spelling.clone()));
                }
                _ => {}
            }
        }
    }
    match candidates.len() {
        0 => Ok(WordHeadwordsV2::Unified {
            common: NormalizedHeadword::parse(&detection.request.surface)?.display,
        }),
        1 => {
            let (mode, first, second) = candidates.into_iter().next().expect("one candidate");
            if mode == "common" {
                return Ok(WordHeadwordsV2::Unified { common: first });
            }
            let normalized_uk = normalize_headword(&first)?.key;
            let normalized_us = normalize_headword(&second)?.key;
            let source_dialect = if detection.normalized_surface == normalized_us
                && detection.normalized_surface != normalized_uk
            {
                SourceDialect::Us
            } else {
                SourceDialect::Uk
            };
            Ok(WordHeadwordsV2::Distinguish {
                uk: first,
                us: second,
                source_dialect,
            })
        }
        count => anyhow::bail!("ambiguous_base_headwords:{count}"),
    }
}

async fn candidates(
    tx: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<(Vec<Candidate>, Vec<InitialHeadwordBackfillBlocker>)> {
    let rows = sqlx::query_as::<_, (Uuid, Value, String, bool)>(
        r#"
        SELECT
            state.entry_id,
            entry.detection_snapshot,
            entry.kind,
            entry.archived_at IS NULL AND NOT EXISTS (
                SELECT 1
                FROM lexicon.surface_sources source
                WHERE source.entry_id = state.entry_id
                  AND source.is_deleted = FALSE
            ) AS active_hidden
        FROM lexicon.v3_entry_state state
        JOIN lexicon.entries entry ON entry.id = state.entry_id
        WHERE state.origin = 'native'
          AND state.initial_headwords IS NULL
          AND state.initial_headword_keys IS NULL
        ORDER BY state.entry_id
        "#,
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut ready = Vec::new();
    let mut blockers = Vec::new();
    for (entry_id, snapshot, entry_kind, active_hidden) in rows {
        let derived = (|| -> anyhow::Result<Candidate> {
            let detection: DetectLexiconSurfaceResponseV3 =
                serde_json::from_value(snapshot).context("invalid_detection_snapshot")?;
            let headwords = derive_headwords(&detection)?;
            let keys = initial_keys(&headwords)?;
            Ok(Candidate {
                entry_id,
                entry_kind,
                active_hidden,
                headwords,
                keys,
            })
        })();
        match derived {
            Ok(candidate) => ready.push(candidate),
            Err(error) => blockers.push(InitialHeadwordBackfillBlocker {
                entry_id,
                reason: format!("{error:#}"),
            }),
        }
    }
    let existing = sqlx::query_as::<_, (Uuid, String, Vec<String>)>(
        r#"
        SELECT state.entry_id, entry.kind, state.initial_headword_keys
        FROM lexicon.v3_entry_state state
        JOIN lexicon.entries entry ON entry.id = state.entry_id
        WHERE state.origin = 'native'
          AND state.initial_headword_keys IS NOT NULL
          AND entry.archived_at IS NULL
          AND NOT EXISTS (
              SELECT 1
              FROM lexicon.surface_sources source
              WHERE source.entry_id = state.entry_id
                AND source.is_deleted = FALSE
          )
        ORDER BY state.entry_id
        "#,
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut owners = BTreeMap::<(String, String), BTreeSet<Uuid>>::new();
    for (entry_id, entry_kind, keys) in existing {
        for key in keys {
            owners
                .entry((entry_kind.clone(), key))
                .or_default()
                .insert(entry_id);
        }
    }
    for candidate in ready.iter().filter(|candidate| candidate.active_hidden) {
        for key in &candidate.keys {
            owners
                .entry((candidate.entry_kind.clone(), key.clone()))
                .or_default()
                .insert(candidate.entry_id);
        }
    }
    let conflicted = owners
        .values()
        .filter(|entry_ids| entry_ids.len() > 1)
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>();
    if !conflicted.is_empty() {
        ready.retain(|candidate| !conflicted.contains(&candidate.entry_id));
        let mut blocked_entry_ids = blockers
            .iter()
            .map(|blocker| blocker.entry_id)
            .collect::<BTreeSet<_>>();
        for entry_id in conflicted {
            if !blocked_entry_ids.insert(entry_id) {
                continue;
            }
            blockers.push(InitialHeadwordBackfillBlocker {
                entry_id,
                reason: "duplicate_active_empty_skeleton".to_owned(),
            });
        }
        blockers.sort_by_key(|blocker| blocker.entry_id);
    }
    Ok((ready, blockers))
}

fn manifest_digest(ready: &[Candidate]) -> anyhow::Result<String> {
    let manifest = ready
        .iter()
        .map(|candidate| {
            (
                &candidate.entry_id,
                &candidate.entry_kind,
                &candidate.headwords,
                &candidate.keys,
            )
        })
        .collect::<Vec<_>>();
    Ok(sha256_json(&manifest)?
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub async fn dry_run(pool: &PgPool) -> anyhow::Result<InitialHeadwordBackfillReport> {
    run(pool, None).await
}

pub async fn apply(
    pool: &PgPool,
    expected_manifest_digest: &str,
) -> anyhow::Result<InitialHeadwordBackfillReport> {
    run(pool, Some(expected_manifest_digest)).await
}

async fn run(
    pool: &PgPool,
    expected_manifest_digest: Option<&str>,
) -> anyhow::Result<InitialHeadwordBackfillReport> {
    let apply = expected_manifest_digest.is_some();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('lexicon.surface-policy-writer', 0))",
    )
    .execute(&mut *tx)
    .await?;
    let (ready, blockers) = candidates(&mut tx).await?;
    let manifest_digest = manifest_digest(&ready)?;
    if let Some(expected) = expected_manifest_digest {
        anyhow::ensure!(
            manifest_digest == expected,
            "v3_initial_headword_backfill_manifest_mismatch:expected={expected}:actual={manifest_digest}"
        );
    }
    if apply && !blockers.is_empty() {
        anyhow::bail!(
            "v3_initial_headword_backfill_blocked:{}",
            serde_json::to_string(&blockers)?
        );
    }
    let mut applied = 0;
    if apply {
        for candidate in &ready {
            let result = sqlx::query(
                r#"
                UPDATE lexicon.v3_entry_state
                SET initial_headwords = $2,
                    initial_headword_keys = $3
                WHERE entry_id = $1
                  AND origin = 'native'
                  AND initial_headwords IS NULL
                  AND initial_headword_keys IS NULL
                "#,
            )
            .bind(candidate.entry_id)
            .bind(serde_json::to_value(&candidate.headwords)?)
            .bind(&candidate.keys)
            .execute(&mut *tx)
            .await?;
            anyhow::ensure!(
                result.rows_affected() == 1,
                "v3_initial_headword_backfill_manifest_candidate_changed:{}",
                candidate.entry_id
            );
            applied += 1;
        }
        anyhow::ensure!(
            applied == ready.len(),
            "v3_initial_headword_backfill_applied_mismatch:expected={}:actual={applied}",
            ready.len()
        );
        let remaining = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM lexicon.v3_entry_state
            WHERE origin = 'native'
              AND (initial_headwords IS NULL OR initial_headword_keys IS NULL)
            "#,
        )
        .fetch_one(&mut *tx)
        .await?;
        anyhow::ensure!(
            remaining == 0,
            "v3_initial_headword_backfill_remaining_null:{remaining}"
        );
    }
    tx.commit().await?;
    Ok(InitialHeadwordBackfillReport {
        mode: if apply { "apply" } else { "dry_run" },
        scanned: ready.len() + blockers.len(),
        ready: ready.len(),
        applied,
        manifest_digest,
        blockers,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn ambiguous_historical_base_suggestions_are_blocked_instead_of_guessed() {
        let detection: DetectLexiconSurfaceResponseV3 = serde_json::from_value(json!({
            "schema_version": 3,
            "detection_id": "00000000-0000-4000-8000-000000000001",
            "expires_at": "2026-08-30T00:05:00Z",
            "request": {"language": "en", "kind": "word", "surface": "centre"},
            "normalized_surface": "centre",
            "builtin_dictionary": {
                "status": "matched",
                "provider": {"name": "fixture", "version": "1"},
                "suggested_pos": ["noun", "verb"],
                "suggested_forms": [{
                    "pos": "noun",
                    "form_type": "base",
                    "regional_variants": {
                        "mode": "common",
                        "common": {"dialect": "common", "spelling": "centre", "pronunciations": []}
                    }
                }, {
                    "pos": "verb",
                    "form_type": "base",
                    "regional_variants": {
                        "mode": "common",
                        "common": {"dialect": "common", "spelling": "center", "pronunciations": []}
                    }
                }],
                "coverage": {
                    "forms": "partial",
                    "pronunciations": "missing",
                    "meanings": "missing",
                    "examples": "missing",
                    "frequency": "missing"
                },
                "provenance": {}
            },
            "suggested_pos": ["noun", "verb"],
            "matches": [],
            "requires_acknowledgement": false
        }))
        .unwrap();

        let error = derive_headwords(&detection).unwrap_err();
        assert!(error.to_string().contains("ambiguous_base_headwords:2"));
    }
}
